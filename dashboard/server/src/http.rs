//! The page's server: the built page as files, the snapshot, the event
//! stream, and the two requests that change something (a command to the
//! device, and which device is shown).
//!
//! `route` decides what a request gets and touches no socket, so a test calls
//! it as the server would. `serve` is the loop a worker thread runs around it.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::bridge::{Ask, refresh_targets, send_command};
use crate::clock::wall_now;
use crate::history::Kind;
use crate::state::Hub;

/// How often a waiting thread looks at the shutdown flag.
const POLL: Duration = Duration::from_millis(500);
/// A comment line is sent on an idle stream this often, so a proxy keeps it open.
const KEEPALIVE: Duration = Duration::from_secs(15);
/// The two JSON requests are a line each; a larger body is cut and fails to parse.
const BODY_MAX: u64 = 4096;
const JSON: &str = "application/json";
const TEXT: &str = "text/plain; charset=utf-8";
const EVENT_STREAM_HEAD: &str = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                                 Cache-Control: no-cache\r\nX-Accel-Buffering: no\r\n\
                                 Connection: close\r\n\r\n";

/// What the routes work with.
pub struct App {
    pub hub: Arc<Hub>,
    pub ask: Ask,
    /// The built page, `dashboard/frontend/dist`.
    pub dist: PathBuf,
    /// When the service started, for `/health`.
    pub started: f64,
    pub shutdown: Arc<AtomicBool>,
}

#[derive(Debug, PartialEq)]
pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

pub enum Outcome {
    Reply(Reply),
    /// An event stream: the snapshot to open it with, then what `Hub::publish` sends.
    Events(String, Receiver<Arc<str>>),
}

#[derive(Deserialize)]
struct CommandBody {
    cmd: Option<String>,
}

#[derive(Deserialize)]
struct DeviceBody {
    ip: Option<String>,
}

/// Decide what a request gets. `from_loopback` opens the fields of `/health`
/// that only the service manager on this host needs.
pub fn route(
    app: &App,
    method: &str,
    url: &str,
    body: &[u8],
    from_loopback: bool,
    now: f64,
) -> Outcome {
    let path = url.split('?').next().unwrap_or(url);
    let reply = match (method, path) {
        ("GET", "/api/status") => text(200, JSON, app.hub.snapshot_json(now)),
        ("GET", "/health") => health(app, from_loopback, now),
        ("POST", "/api/cmd") => command(app, body, now),
        ("POST", "/api/device") => select_device(app, body, now),
        ("GET", "/events") => match app.hub.subscribe() {
            Some(receiver) => return Outcome::Events(app.hub.snapshot_json(now), receiver),
            None => text(503, TEXT, "too many open event streams"),
        },
        ("GET", "/") => page_file(&app.dist, "index.html").unwrap_or_else(|| {
            text(
                503,
                TEXT,
                "frontend not built — run: cd dashboard/frontend && npm run build",
            )
        }),
        ("GET", _) => page_file(&app.dist, &path[1..]).unwrap_or_else(not_found),
        _ => not_found(),
    };
    Outcome::Reply(reply)
}

fn health(app: &App, from_loopback: bool, now: f64) -> Reply {
    let mut fields = json!({"status": "ok"});
    if from_loopback {
        fields["pid"] = json!(std::process::id());
        fields["uptime_seconds"] = json!(((now - app.started) * 10.0).round() / 10.0);
        fields["device"] = json!(app.hub.state().device_ip());
    }
    text(200, JSON, fields.to_string())
}

/// Relay a command to the selected device and log it with its reply.
fn command(app: &App, body: &[u8], now: f64) -> Reply {
    let Some(cmd) = serde_json::from_slice::<CommandBody>(body)
        .ok()
        .and_then(|body| body.cmd)
    else {
        return text(400, JSON, json!({"error": "missing 'cmd'"}).to_string());
    };
    let reply = send_command(&app.hub, &app.ask, &cmd);
    let shown = reply.as_deref().unwrap_or("(no reply)");
    app.hub
        .state()
        .add_log(Kind::Cmd, format!("{cmd}  ->  {shown}"), now);
    if cmd.starts_with("target ") {
        // The command changed a target: show what the device now holds.
        refresh_targets(&app.hub, &app.ask, now);
    }
    text(
        200,
        JSON,
        json!({"ok": reply.is_some(), "reply": reply}).to_string(),
    )
}

fn select_device(app: &App, body: &[u8], now: f64) -> Reply {
    let Some(ip) = serde_json::from_slice::<DeviceBody>(body)
        .ok()
        .and_then(|body| body.ip)
    else {
        return text(400, JSON, json!({"error": "missing 'ip'"}).to_string());
    };
    if !app.hub.state().select_device(&ip, now) {
        return text(
            404,
            JSON,
            json!({"error": format!("unknown device: {ip}")}).to_string(),
        );
    }
    app.hub.publish(now);
    text(200, JSON, json!({"ok": true, "selected": ip}).to_string())
}

/// A file of the built page, or `None` when the name is not one of its files.
/// The name comes from the request: a name that steps outside `dist` is
/// refused before the disk is asked.
fn page_file(dist: &Path, name: &str) -> Option<Reply> {
    let inside = !name.is_empty()
        && name.split('/').all(|part| !matches!(part, "" | "." | ".."))
        && !name.contains('\\');
    if !inside {
        return None;
    }
    let path = dist.join(name);
    if !path.is_file() {
        return None;
    }
    let body = std::fs::read(&path).ok()?;
    Some(Reply {
        status: 200,
        content_type: content_type(&path),
        body,
    })
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => JSON,
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn text(status: u16, content_type: &'static str, body: impl Into<String>) -> Reply {
    Reply {
        status,
        content_type,
        body: body.into().into_bytes(),
    }
}

fn not_found() -> Reply {
    text(404, TEXT, "not found")
}

/// One line per request on stdout, where the service manager reads it:
/// `<ip> "<method> <path>" <status>`.
pub fn access_line(ip: Option<&str>, method: &str, path: &str, status: u16) -> String {
    format!("{} \"{method} {path}\" {status}", ip.unwrap_or("-"))
}

/// Write an event stream until the page closes it or `shutdown` is set: the
/// opening snapshot, then each published one, and a comment line when idle.
pub fn stream_events(
    mut writer: impl Write,
    first: &str,
    receiver: &Receiver<Arc<str>>,
    shutdown: &AtomicBool,
) {
    let mut send = |text: String| {
        writer
            .write_all(text.as_bytes())
            .and_then(|()| writer.flush())
            .is_ok()
    };
    if !send(format!("{EVENT_STREAM_HEAD}data: {first}\n\n")) {
        return;
    }
    let mut idle = Duration::ZERO;
    while !shutdown.load(Ordering::Relaxed) {
        let sent = match receiver.recv_timeout(POLL) {
            Ok(snapshot) => {
                idle = Duration::ZERO;
                send(format!("data: {snapshot}\n\n"))
            }
            Err(RecvTimeoutError::Timeout) => {
                idle += POLL;
                if idle < KEEPALIVE {
                    continue;
                }
                idle = Duration::ZERO;
                send(": keepalive\n\n".to_string())
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if !sent {
            // The page closed the stream; dropping the receiver unsubscribes it.
            return;
        }
    }
}

/// A worker thread's loop: take requests until `shutdown` is set.
pub fn serve(app: &App, server: &tiny_http::Server) {
    while !app.shutdown.load(Ordering::Relaxed) {
        match server.recv_timeout(POLL) {
            Ok(Some(request)) => handle(app, request),
            Ok(None) => {}
            Err(error) => {
                eprintln!("http: taking a request failed: {error}; this worker stops");
                return;
            }
        }
    }
}

fn handle(app: &App, mut request: tiny_http::Request) {
    let method = request.method().to_string();
    let url = request.url().to_string();
    let remote = request.remote_addr().map(|address| address.ip());
    let mut body = Vec::new();
    // A body that cannot be read is an empty one, which the route refuses.
    let _ = request.as_reader().take(BODY_MAX).read_to_end(&mut body);
    let from_loopback = remote.is_some_and(|ip| ip.is_loopback());
    let outcome = route(app, &method, &url, &body, from_loopback, wall_now());

    let status = match &outcome {
        Outcome::Reply(reply) => reply.status,
        Outcome::Events(..) => 200,
    };
    let path = url.split('?').next().unwrap_or(&url);
    println!(
        "{}",
        access_line(
            remote.map(|ip| ip.to_string()).as_deref(),
            &method,
            path,
            status
        )
    );

    match outcome {
        Outcome::Reply(reply) => respond(request, reply),
        Outcome::Events(first, receiver) => {
            // A stream lives as long as its page: on a thread of its own, so
            // the workers stay free for requests.
            let writer = request.into_writer();
            let shutdown = Arc::clone(&app.shutdown);
            std::thread::spawn(move || stream_events(writer, &first, &receiver, &shutdown));
        }
    }
}

fn respond(request: tiny_http::Request, reply: Reply) {
    let mut response = tiny_http::Response::from_data(reply.body).with_status_code(reply.status);
    // The name and the value are ASCII literals of this file, which is all the header type refuses.
    if let Ok(header) = tiny_http::Header::from_bytes("Content-Type", reply.content_type) {
        response.add_header(header);
    }
    // A page that left before its answer is not this server's failure.
    let _ = request.respond(response);
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::Value;

    use super::*;
    use crate::protocol::parse_diag;
    use crate::state::State;
    use crate::testing::TempDir;

    const T0: f64 = 1_800_000_000.0;

    /// An app whose device answers every command with `reply`, and the commands it was asked.
    fn app(reply: Option<&str>, dist: PathBuf) -> (App, Arc<Mutex<Vec<String>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&sent);
        let reply = reply.map(str::to_string);
        let ask: Ask = Arc::new(move |_, command| {
            record.lock().unwrap().push(command.to_string());
            reply.clone()
        });
        let hub = Arc::new(Hub::new(State::new(None, T0)));
        let shutdown = Arc::new(AtomicBool::new(false));
        (
            App {
                hub,
                ask,
                dist,
                started: T0,
                shutdown,
            },
            sent,
        )
    }

    fn heard(app: &App, ip: &str) {
        let line = format!("XRFD up=1 ms=1000 ip={ip} rx=0 dhcp=0/0 rtr=Y");
        app.hub
            .state()
            .update_from_diag(&parse_diag(&line).unwrap(), ip, T0);
    }

    fn get(app: &App, path: &str) -> Reply {
        request(app, "GET", path, b"")
    }

    fn request(app: &App, method: &str, path: &str, body: &[u8]) -> Reply {
        match route(app, method, path, body, true, T0) {
            Outcome::Reply(reply) => reply,
            Outcome::Events(..) => panic!("{path} opened an event stream"),
        }
    }

    fn json_of(reply: &Reply) -> Value {
        serde_json::from_slice(&reply.body).unwrap()
    }

    #[test]
    fn status_returns_snapshot() {
        // Red when /api/status stops answering with the snapshot.
        let (app, _) = app(None, PathBuf::new());
        let reply = get(&app, "/api/status");
        let body = json_of(&reply);
        assert_eq!((reply.status, reply.content_type), (200, JSON));
        assert_eq!(
            (body["deviceIp"].as_str(), body["live"].as_bool()),
            (Some(""), Some(false))
        );
    }

    #[test]
    fn health_contract_fields() {
        // Red when a field the service manager reads leaves /health.
        let (app, _) = app(None, PathBuf::new());
        let body = json_of(&get(&app, "/health"));
        assert_eq!(body["status"], "ok");
        assert!(
            body["pid"].is_u64()
                && body["uptime_seconds"].is_number()
                && body["device"].is_string()
        );
    }

    #[test]
    fn health_tells_another_host_only_that_it_is_up() {
        // Red when the process id and the device's address are given to the LAN.
        let (app, _) = app(None, PathBuf::new());
        let Outcome::Reply(reply) = route(&app, "GET", "/health", b"", false, T0) else {
            panic!()
        };
        assert_eq!(json_of(&reply), json!({"status": "ok"}));
    }

    #[test]
    fn cmd_calls_bridge_and_returns_reply() {
        // Red when the command is not relayed as given, or its reply not returned and logged.
        let (app, sent) = app(Some("ok:status"), PathBuf::new());
        heard(&app, "10.10.204.123");
        let reply = request(&app, "POST", "/api/cmd", br#"{"cmd": "status"}"#);
        assert_eq!(reply.status, 200);
        assert_eq!(json_of(&reply), json!({"ok": true, "reply": "ok:status"}));
        assert_eq!(*sent.lock().unwrap(), ["status"]);
        assert_eq!(
            app.hub.state().snapshot(T0).log[0].m,
            "status  ->  ok:status"
        );
    }

    #[test]
    fn cmd_no_reply_reports_error() {
        // Red when a command the device did not answer is reported as done.
        let (app, _) = app(None, PathBuf::new());
        heard(&app, "10.10.204.123");
        let body = json_of(&request(&app, "POST", "/api/cmd", br#"{"cmd": "status"}"#));
        assert_eq!(body, json!({"ok": false, "reply": null}));
        assert_eq!(
            app.hub.state().snapshot(T0).log[0].m,
            "status  ->  (no reply)"
        );
    }

    #[test]
    fn cmd_missing_field_is_400() {
        // Red when a body with no command reaches the device.
        let (app, sent) = app(Some("ok"), PathBuf::new());
        assert_eq!(request(&app, "POST", "/api/cmd", b"{}").status, 400);
        assert_eq!(request(&app, "POST", "/api/cmd", b"not json").status, 400);
        assert!(sent.lock().unwrap().is_empty());
    }

    #[test]
    fn a_target_command_shows_what_the_device_now_holds() {
        // Red when the targets are not asked for again after a target command:
        // the card would show the old state until the next timed refresh.
        let (app, sent) = app(Some("t0 off 10.10.204.184:50001"), PathBuf::new());
        heard(&app, "10.10.204.123");
        request(&app, "POST", "/api/cmd", br#"{"cmd": "target 0 off"}"#);
        assert_eq!(*sent.lock().unwrap(), ["target 0 off", "targets"]);
        assert!(!app.hub.state().snapshot(T0).targets[0].on);
    }

    #[test]
    fn events_streams_initial_snapshot() {
        // Red when a new page has to wait for the next status line to paint.
        let (app, _) = app(None, PathBuf::new());
        let Outcome::Events(first, receiver) = route(&app, "GET", "/events", b"", true, T0) else {
            panic!("/events did not open a stream")
        };
        let stop = AtomicBool::new(true);
        let mut written = Vec::new();
        stream_events(&mut written, &first, &receiver, &stop);
        let written = String::from_utf8(written).unwrap();
        let (head, events) = written.split_once("\r\n\r\n").unwrap();
        assert!(head.contains("Content-Type: text/event-stream"), "{head}");
        assert!(
            events.starts_with("data: ") && events.contains("\"deviceIp\""),
            "{events}"
        );
        assert!(events.ends_with("\n\n"));
    }

    #[test]
    fn index_served_when_dist_missing_returns_helpful_503() {
        // Red when a server with no built page answers with a blank 404.
        let dir = TempDir::new();
        let (app, _) = app(None, dir.path().join("nonexistent"));
        let reply = get(&app, "/");
        assert_eq!(reply.status, 503);
        assert!(
            String::from_utf8(reply.body)
                .unwrap()
                .contains("npm run build")
        );
    }

    #[test]
    fn page_files_are_served_and_nothing_outside_them() {
        // Red when the built page is not served, or a request can name a file
        // outside it.
        let dir = TempDir::new();
        let dist = dir.path().join("dist");
        std::fs::create_dir_all(dist.join("assets")).unwrap();
        std::fs::write(dist.join("index.html"), "<html>").unwrap();
        std::fs::write(dist.join("assets/app.js"), "js").unwrap();
        std::fs::write(dir.path().join("secret.txt"), "outside").unwrap();
        let (app, _) = app(None, dist);
        assert_eq!(
            (get(&app, "/").status, get(&app, "/").content_type),
            (200, "text/html; charset=utf-8")
        );
        assert_eq!(get(&app, "/assets/app.js?v=1").body, b"js");
        assert_eq!(get(&app, "/../secret.txt").status, 404);
        assert_eq!(get(&app, "/assets/../../secret.txt").status, 404);
        assert_eq!(get(&app, "/missing.js").status, 404);
    }

    #[test]
    fn select_device_switches_selection() {
        // Red when the page cannot switch to another device it lists.
        let (app, _) = app(None, PathBuf::new());
        heard(&app, "10.10.204.123");
        heard(&app, "10.10.204.124");
        let reply = request(&app, "POST", "/api/device", br#"{"ip": "10.10.204.124"}"#);
        assert_eq!(reply.status, 200);
        assert_eq!(json_of(&reply)["selected"], "10.10.204.124");
        assert_eq!(
            json_of(&get(&app, "/api/status"))["deviceIp"],
            "10.10.204.124"
        );
    }

    #[test]
    fn select_device_rejects_unknown() {
        // Red when an address never heard can be selected.
        let (app, _) = app(None, PathBuf::new());
        heard(&app, "10.10.204.123");
        let reply = request(&app, "POST", "/api/device", br#"{"ip": "10.10.204.199"}"#);
        assert_eq!(reply.status, 404);
        assert_eq!(
            json_of(&get(&app, "/api/status"))["deviceIp"],
            "10.10.204.123"
        );
    }

    #[test]
    fn select_device_requires_ip() {
        // Red when a body with no address is taken as a selection.
        let (app, _) = app(None, PathBuf::new());
        assert_eq!(request(&app, "POST", "/api/device", b"{}").status, 400);
    }

    #[test]
    fn status_lists_discovered_devices() {
        // Red when a device that was heard is missing from the page's list.
        let (app, _) = app(None, PathBuf::new());
        heard(&app, "10.10.204.123");
        let devices = json_of(&get(&app, "/api/status"))["devices"].clone();
        assert_eq!(
            devices,
            json!([{"ip": "10.10.204.123", "ageSec": 0, "live": true}])
        );
    }

    #[test]
    fn access_line_has_the_address_the_request_and_the_status() {
        // Red when the line's shape changes: the service manager filters its
        // own health polls by the address at its start.
        assert_eq!(
            access_line(Some("10.10.204.211"), "GET", "/api/status", 200),
            "10.10.204.211 \"GET /api/status\" 200"
        );
        assert_eq!(
            access_line(Some("10.10.204.211"), "GET", "/missing", 404),
            "10.10.204.211 \"GET /missing\" 404"
        );
        assert_eq!(access_line(None, "GET", "/", 200), "- \"GET /\" 200");
    }

    #[test]
    fn an_idle_stream_ends_when_the_page_goes_away() {
        // Red when a stream keeps its thread after the page closed it.
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (app, _) = app(None, PathBuf::new());
        let receiver = app.hub.subscribe().unwrap();
        stream_events(Closed, "{}", &receiver, &AtomicBool::new(false));
    }
}
