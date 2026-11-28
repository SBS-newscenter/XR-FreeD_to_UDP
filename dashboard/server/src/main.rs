//! The service's entry point: read the arguments, bind the two sockets, start
//! the threads, and on SIGTERM or SIGINT come down in order.
//!
//! It runs from the repository's root, where `setup/run.sh` starts it:
//! `server.toml`, `data/`, `log/` and the built page are found from there.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use xrfd_dashboard::bridge::{Bridge, CMD_TIMEOUT, bind_diag, udp_ask};
use xrfd_dashboard::clock::{local_time, wall_now};
use xrfd_dashboard::history::{History, Kind};
use xrfd_dashboard::http::{self, App};
use xrfd_dashboard::service::{append_lifecycle, port_from_manifest, write_pidfile};
use xrfd_dashboard::state::{Hub, State};
use xrfd_dashboard::{CTRL_PORT, DIAG_PORT, STATUS_KEEP_DAYS};

/// Threads taking requests. An event stream has a thread of its own, so four
/// cover the page's files and its few JSON requests.
const WORKERS: usize = 4;
const USAGE: &str = "xrfd-dashboard [--root <repository root>] [--port <port>]\n\
                     \x20 --root  where server.toml, data/, log/ and the built page are (default: .)\n\
                     \x20 --port  the page's port (default: the port of server.toml)";

struct Args {
    root: PathBuf,
    port: Option<u16>,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xrfd-dashboard: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let Some(args) = parse_args(std::env::args().skip(1))? else {
        println!("{USAGE}");
        return Ok(());
    };
    let port = match args.port {
        Some(port) => port,
        None => manifest_port(&args.root)?,
    };

    let shutdown = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(signal, Arc::clone(&shutdown))
            .map_err(|error| format!("registering signal {signal} failed: {error}"))?;
    }

    // Both sockets before anything is written: a port already taken ends the
    // start here, with nothing to clean up.
    let diag =
        bind_diag(DIAG_PORT).map_err(|error| format!("binding UDP {DIAG_PORT} failed: {error}"))?;
    let server = tiny_http::Server::http(("0.0.0.0", port))
        .map_err(|error| format!("binding TCP {port} failed: {error}"))?;

    let started = wall_now();
    let history = Arc::new(History::new(
        args.root.join("data/history"),
        STATUS_KEEP_DAYS,
    ));
    let mut state = State::new(Some(Arc::clone(&history)), started);
    state.add_log(Kind::Info, "dashboard started", started);
    let hub = Arc::new(Hub::new(state));
    let ask = udp_ask(CTRL_PORT, CMD_TIMEOUT);
    let app = App {
        hub: Arc::clone(&hub),
        ask: Arc::clone(&ask),
        dist: args.root.join("dashboard/frontend/dist"),
        started,
        shutdown: Arc::clone(&shutdown),
    };
    let mut bridge = Bridge::new(hub, Some(history), ask);

    let pid = std::process::id();
    let pidfile = args.root.join("data/xrfd.pid");
    let log_dir = args.root.join("log");
    write_pidfile(&pidfile, pid, local_time(started))
        .map_err(|error| format!("writing {} failed: {error}", pidfile.display()))?;
    let version = env!("CARGO_PKG_VERSION");
    // The manager's log view is a convenience: the service runs without its line.
    if let Err(error) = append_lifecycle(
        &log_dir,
        "STARTED",
        &format!("version={version} pid={pid}"),
        local_time(started),
    ) {
        eprintln!(
            "xrfd-dashboard: writing the start line under {} failed: {error}",
            log_dir.display()
        );
    }
    println!(
        "XRFD dashboard {version} listening on http://0.0.0.0:{port}  (http://localhost:{port})"
    );

    // Scoped threads borrow `app`, `server` and `shutdown`, and the scope
    // returns only when every one of them has ended.
    std::thread::scope(|scope| {
        scope.spawn(|| bridge.run(diag, DIAG_PORT, &shutdown));
        for _ in 0..WORKERS {
            scope.spawn(|| http::serve(&app, &server));
        }
    });

    if let Err(error) = std::fs::remove_file(&pidfile) {
        eprintln!(
            "xrfd-dashboard: removing {} failed: {error}",
            pidfile.display()
        );
    }
    if let Err(error) = append_lifecycle(
        &log_dir,
        "STOPPED",
        &format!("pid={pid}"),
        local_time(wall_now()),
    ) {
        eprintln!(
            "xrfd-dashboard: writing the stop line under {} failed: {error}",
            log_dir.display()
        );
    }
    Ok(())
}

/// The arguments, or `None` when help was asked for.
fn parse_args(mut raw: impl Iterator<Item = String>) -> Result<Option<Args>, String> {
    let mut args = Args {
        root: PathBuf::from("."),
        port: None,
    };
    while let Some(flag) = raw.next() {
        match flag.as_str() {
            "--root" => args.root = PathBuf::from(raw.next().ok_or("--root needs a directory")?),
            "--port" => {
                let value = raw.next().ok_or("--port needs a number")?;
                args.port = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--port is not a port: {value}"))?,
                );
            }
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    Ok(Some(args))
}

fn manifest_port(root: &std::path::Path) -> Result<u16, String> {
    let path = root.join("server.toml");
    let manifest = std::fs::read_to_string(&path)
        .map_err(|error| format!("reading {} failed: {error}", path.display()))?;
    port_from_manifest(&manifest).ok_or_else(|| format!("{} names no port", path.display()))
}
