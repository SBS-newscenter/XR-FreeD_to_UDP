//! The bridge: the one thread that hears the device's status broadcast on
//! UDP, and the one way a command reaches a device.
//!
//! Hearing: every status line is recorded, applied to the state and published
//! to the open pages. Between lines the loop wakes twice a second to notice a
//! device that has stopped, and to ask the selected device for its targets.
//!
//! Asking: a command goes out from a socket of its own and waits for one
//! reply. `Ask` is a function so a test answers in the device's place.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};

use crate::clock::{local_time, wall_now};
use crate::history::History;
use crate::protocol::{parse_diag, parse_targets};
use crate::state::{Hub, Signal};

/// How long a command waits for the device's reply.
pub const CMD_TIMEOUT: Duration = Duration::from_secs(3);
/// The selected device is asked for its targets this often.
const TARGETS_REFRESH_S: f64 = 30.0;
/// While the selected device is silent, the pages are sent a snapshot this
/// often, so their "silent for" keeps counting with no status line to move it.
pub const SILENT_REPUBLISH_S: f64 = 5.0;
/// How long the loop waits for a status line before its other duties.
const POLL: Duration = Duration::from_millis(500);
/// A status line is under 300 bytes; a larger datagram is cut and fails to parse.
const DATAGRAM_MAX: usize = 2048;

/// Send `command` to the device at `ip` and return its reply, or `None` when
/// none came.
pub type Ask = Arc<dyn Fn(&str, &str) -> Option<String> + Send + Sync>;

/// Bind the status port so that the operator's tools, and a second backend,
/// can bind it at the same time: a broadcast is delivered to every socket
/// bound with these two options.
pub fn bind_diag(port: u16) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_reuse_port(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())?;
    socket.set_read_timeout(Some(POLL))?;
    Ok(socket.into())
}

/// The `Ask` of the service: over UDP to the device's control port.
pub fn udp_ask(ctrl_port: u16, timeout: Duration) -> Ask {
    // No reply is an outcome the page is told about, whatever kept it away:
    // the device off, its cable out, or the send itself refused.
    Arc::new(move |ip, command| ask_over_udp(ip, ctrl_port, command, timeout).ok())
}

fn ask_over_udp(ip: &str, port: u16, command: &str, timeout: Duration) -> io::Result<String> {
    let device: Ipv4Addr = ip
        .parse()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_read_timeout(Some(timeout))?;
    socket.send_to(command.as_bytes(), (device, port))?;
    let mut buffer = [0u8; DATAGRAM_MAX];
    let (length, _) = socket.recv_from(&mut buffer)?;
    Ok(String::from_utf8_lossy(&buffer[..length]).into_owned())
}

/// Send a command to the selected device; `None` when no device is selected
/// or none replied.
pub fn send_command(hub: &Hub, ask: &Ask, command: &str) -> Option<String> {
    let ip = hub.state().device_ip().to_string();
    if ip.is_empty() {
        return None;
    }
    ask(&ip, command)
}

/// Ask the selected device for its targets and show them. The lock is not
/// held while the device is asked, so the selection can move meanwhile: a
/// reply for a device no longer selected is dropped.
pub fn refresh_targets(hub: &Hub, ask: &Ask, now: f64) {
    let ip = hub.state().device_ip().to_string();
    let Some(reply) = send_command(hub, ask, "targets") else {
        return;
    };
    let targets = parse_targets(&reply);
    if targets.is_empty() {
        return;
    }
    {
        let mut state = hub.state();
        if state.device_ip() != ip {
            return;
        }
        state.set_targets(targets);
    }
    hub.publish(now);
}

pub struct Bridge {
    hub: Arc<Hub>,
    history: Option<Arc<History>>,
    ask: Ask,
    last_publish: Option<f64>,
    last_targets_refresh: Option<f64>,
    /// The device the last targets were asked of; a switch asks again at once.
    targets_for_ip: String,
}

impl Bridge {
    pub fn new(hub: Arc<Hub>, history: Option<Arc<History>>, ask: Ask) -> Bridge {
        Bridge {
            hub,
            history,
            ask,
            last_publish: None,
            last_targets_refresh: None,
            targets_for_ip: String::new(),
        }
    }

    /// Hear status lines on `socket` until `shutdown` is set. `port` is the
    /// port it was bound to, for binding again after a socket error.
    pub fn run(&mut self, mut socket: UdpSocket, port: u16, shutdown: &AtomicBool) {
        let mut buffer = [0u8; DATAGRAM_MAX];
        while !shutdown.load(Ordering::Relaxed) {
            self.watch_silence(wall_now());
            match socket.recv_from(&mut buffer) {
                Ok((length, source)) => {
                    self.on_datagram(&buffer[..length], &source.ip().to_string(), wall_now());
                }
                Err(error) if is_timeout(&error) => self.maybe_refresh_targets(wall_now()),
                Err(error) => {
                    eprintln!("bridge: receiving on UDP {port} failed: {error}; binding again");
                    std::thread::sleep(Duration::from_secs(1));
                    match bind_diag(port) {
                        Ok(bound) => socket = bound,
                        Err(error) => eprintln!("bridge: binding UDP {port} failed: {error}"),
                    }
                }
            }
        }
    }

    /// Take one datagram from `src`. Anything that is not a status line is
    /// dropped: the port is open to the LAN.
    pub fn on_datagram(&mut self, data: &[u8], src: &str, now: f64) {
        let line = String::from_utf8_lossy(data);
        let Some(diag) = parse_diag(&line) else {
            return;
        };
        if let Some(history) = &self.history {
            history.status(src, &line, local_time(now));
        }
        self.hub.state().update_from_diag(&diag, src, now);
        self.publish(now);
    }

    /// Log a device that stopped, and keep the open pages told while it is
    /// stopped. A page repaints only on a published snapshot, and a status
    /// line is what publishes one: without this a silent device leaves every
    /// open page showing its last numbers as live.
    pub fn watch_silence(&mut self, now: f64) {
        self.hub.state().mark_silent(now);
        let republish_due = self
            .last_publish
            .is_none_or(|last| now - last >= SILENT_REPUBLISH_S);
        if republish_due && self.hub.state().signal(now) == Signal::Lost {
            self.publish(now);
        }
    }

    /// Ask the selected device for its targets when the last answer is old,
    /// or was another device's.
    pub fn maybe_refresh_targets(&mut self, now: f64) {
        let ip = self.hub.state().device_ip().to_string();
        if ip.is_empty() {
            return;
        }
        if ip != self.targets_for_ip {
            self.targets_for_ip = ip;
            self.last_targets_refresh = None;
        }
        if self
            .last_targets_refresh
            .is_some_and(|last| now - last < TARGETS_REFRESH_S)
        {
            return;
        }
        self.last_targets_refresh = Some(now);
        refresh_targets(&self.hub, &self.ask, now);
    }

    fn publish(&mut self, now: f64) {
        self.last_publish = Some(now);
        self.hub.publish(now);
    }
}

/// A read that waited its whole timeout: the two kinds the platforms report.
fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::thread;

    use serde_json::Value;

    use super::*;
    use crate::state::State;
    use crate::testing::TempDir;

    const DEV_LINE: &str = "XRFD up=1 ms=1000 ip=127.0.0.1 rx=0 dhcp=0/0 rtr=Y";
    /// A fixed wall time: the silence tests read no clock.
    const T0: f64 = 1_800_000_000.0;

    fn no_reply() -> Ask {
        Arc::new(|_, _| None)
    }

    fn hub() -> Arc<Hub> {
        Arc::new(Hub::new(State::new(None, T0)))
    }

    fn heard(hub: &Hub, src: &str, now: f64) {
        hub.state()
            .update_from_diag(&parse_diag(DEV_LINE).unwrap(), src, now);
    }

    /// A bridge running on a port of the system's choosing, stopped on drop.
    struct Running {
        hub: Arc<Hub>,
        port: u16,
        shutdown: Arc<AtomicBool>,
    }

    impl Running {
        fn start() -> Running {
            let hub = hub();
            let socket = bind_diag(0).unwrap();
            let port = socket.local_addr().unwrap().port();
            let shutdown = Arc::new(AtomicBool::new(false));
            let mut bridge = Bridge::new(Arc::clone(&hub), None, no_reply());
            let flag = Arc::clone(&shutdown);
            thread::spawn(move || bridge.run(socket, port, &flag));
            Running {
                hub,
                port,
                shutdown,
            }
        }

        fn send(&self, datagram: &[u8]) {
            let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            socket
                .send_to(datagram, (Ipv4Addr::LOCALHOST, self.port))
                .unwrap();
        }

        fn selected_within_two_seconds(&self) -> String {
            for _ in 0..20 {
                self.send(DEV_LINE.as_bytes());
                thread::sleep(Duration::from_millis(100));
                let selected = self.hub.state().device_ip().to_string();
                if !selected.is_empty() {
                    return selected;
                }
            }
            String::new()
        }
    }

    impl Drop for Running {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Relaxed);
        }
    }

    #[test]
    fn bridge_receives_and_updates_state() {
        // Red when the loop stops applying what it hears.
        assert_eq!(Running::start().selected_within_two_seconds(), "127.0.0.1");
    }

    #[test]
    fn bridge_survives_garbage_packet() {
        // Red when a datagram that is not a status line ends the loop.
        let running = Running::start();
        running.send(b"\xff\xff not xrfd");
        assert_eq!(running.selected_within_two_seconds(), "127.0.0.1");
    }

    #[test]
    fn send_command_roundtrip() {
        // Red when the command does not reach the control port, or the reply is not read.
        let device = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = device.local_addr().unwrap().port();
        thread::spawn(move || {
            let mut buffer = [0u8; 512];
            let (length, asker) = device.recv_from(&mut buffer).unwrap();
            let reply = [b"ok:", &buffer[..length]].concat();
            device.send_to(&reply, asker).unwrap();
        });
        let hub = hub();
        heard(&hub, "127.0.0.1", T0);
        let reply = send_command(&hub, &udp_ask(port, Duration::from_secs(2)), "status");
        assert_eq!(reply.as_deref(), Some("ok:status"));
    }

    #[test]
    fn send_command_no_device_returns_none() {
        // Red when a command is sent with no device selected.
        let asked = Arc::new(Mutex::new(0));
        let count = Arc::clone(&asked);
        let ask: Ask = Arc::new(move |_, _| {
            *count.lock().unwrap() += 1;
            Some("reply".to_string())
        });
        assert_eq!(send_command(&hub(), &ask, "status"), None);
        assert_eq!(*asked.lock().unwrap(), 0);
    }

    #[test]
    fn status_port_can_be_bound_twice() {
        // Red when either reuse option goes: the operator's tools, and the
        // second backend of the side-by-side day, bind the same port.
        let first = bind_diag(0).unwrap();
        let port = first.local_addr().unwrap().port();
        assert!(bind_diag(port).is_ok());
    }

    /// The (command, device) pairs an `Ask` was called with.
    type Calls = Arc<Mutex<Vec<(String, String)>>>;

    /// An `Ask` that records what it was asked and always answers one target.
    fn recording_ask() -> (Ask, Calls) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&calls);
        let ask: Ask = Arc::new(move |ip, command| {
            record
                .lock()
                .unwrap()
                .push((command.to_string(), ip.to_string()));
            Some("t0 on 10.10.204.184:50001".to_string())
        });
        (ask, calls)
    }

    #[test]
    fn targets_refetched_immediately_after_device_switch() {
        // Red when a switch waits out the refresh interval: the page would
        // show the previous device's targets for up to thirty seconds.
        let hub = hub();
        heard(&hub, "10.10.204.123", T0);
        heard(&hub, "10.10.204.124", T0);
        let (ask, calls) = recording_ask();
        let mut bridge = Bridge::new(Arc::clone(&hub), None, ask);

        bridge.maybe_refresh_targets(T0);
        assert_eq!(
            *calls.lock().unwrap(),
            [("targets".to_string(), "10.10.204.123".to_string())]
        );
        bridge.maybe_refresh_targets(T0 + 1.0);
        assert_eq!(calls.lock().unwrap().len(), 1, "within the interval");

        hub.state().select_device("10.10.204.124", T0 + 2.0);
        bridge.maybe_refresh_targets(T0 + 2.0);
        assert_eq!(
            calls.lock().unwrap()[1],
            ("targets".to_string(), "10.10.204.124".to_string())
        );
    }

    #[test]
    fn targets_are_asked_again_when_the_interval_has_passed() {
        // Red when the first answer is kept for good: a target changed from
        // another page or tool would never show.
        let hub = hub();
        heard(&hub, "10.10.204.123", T0);
        let (ask, calls) = recording_ask();
        let mut bridge = Bridge::new(Arc::clone(&hub), None, ask);
        bridge.maybe_refresh_targets(T0);
        bridge.maybe_refresh_targets(T0 + TARGETS_REFRESH_S);
        assert_eq!(calls.lock().unwrap().len(), 2);
    }

    #[test]
    fn stale_targets_reply_discarded_after_switch_during_send() {
        // Red when a reply is stored without checking the selection again: it
        // belongs to the device selected when it was asked for.
        let hub = hub();
        heard(&hub, "10.10.204.123", T0);
        heard(&hub, "10.10.204.124", T0);
        let switching = Arc::clone(&hub);
        let ask: Ask = Arc::new(move |_, _| {
            switching.state().select_device("10.10.204.124", T0);
            Some("t0 on 10.10.204.184:50001".to_string())
        });
        refresh_targets(&hub, &ask, T0);
        assert!(hub.state().snapshot(T0).targets.is_empty());
    }

    fn published(receiver: &std::sync::mpsc::Receiver<Arc<str>>) -> Option<Value> {
        receiver
            .try_recv()
            .ok()
            .map(|json| serde_json::from_str(&json).unwrap())
    }

    #[test]
    fn open_pages_are_told_when_the_device_falls_silent() {
        // Red when watch_silence stops publishing: an open page then keeps the
        // last snapshot it was sent, the one that said "ok".
        let hub = hub();
        heard(&hub, "10.10.204.87", T0);
        let mut bridge = Bridge::new(Arc::clone(&hub), None, no_reply());
        let receiver = hub.subscribe().unwrap();
        bridge.watch_silence(T0 + 12.0);
        assert_eq!(published(&receiver), None);
        bridge.watch_silence(T0 + 13.0);
        assert_eq!(published(&receiver).unwrap()["signal"], "lost");
    }

    #[test]
    fn silent_device_is_republished_on_the_interval_only() {
        // Red when the republish goes (the page's "silent for" freezes), or
        // loses its interval (two snapshots a second to every page).
        let hub = hub();
        heard(&hub, "10.10.204.87", T0);
        let mut bridge = Bridge::new(Arc::clone(&hub), None, no_reply());
        bridge.watch_silence(T0 + 13.0);
        let receiver = hub.subscribe().unwrap();
        bridge.watch_silence(T0 + 13.0 + SILENT_REPUBLISH_S - 1.0);
        assert_eq!(published(&receiver), None);
        bridge.watch_silence(T0 + 13.0 + SILENT_REPUBLISH_S);
        assert_eq!(published(&receiver).unwrap()["silentSec"], 18);
    }

    #[test]
    fn heard_device_is_not_republished() {
        // Red when the republish stops asking whether the device is silent:
        // every extra snapshot adds a point to the page's rate chart.
        let hub = hub();
        heard(&hub, "10.10.204.87", T0);
        let mut bridge = Bridge::new(Arc::clone(&hub), None, no_reply());
        let receiver = hub.subscribe().unwrap();
        bridge.watch_silence(T0 + SILENT_REPUBLISH_S + 1.0);
        assert_eq!(published(&receiver), None);
    }

    #[test]
    fn bridge_records_each_status_line_and_nothing_else() {
        // Red when a parsed line is not handed to the history, or a datagram
        // that is not a status line is.
        let dir = TempDir::new();
        let history = Arc::new(History::new(dir.path(), 30));
        let mut bridge = Bridge::new(hub(), Some(history), no_reply());
        bridge.on_datagram(b"not a status line", "10.10.204.9", T0);
        bridge.on_datagram(DEV_LINE.as_bytes(), "10.10.204.87", T0);
        let day = local_time(T0).format("%Y-%m-%d");
        let text = std::fs::read_to_string(dir.path().join(format!("status-{day}.txt"))).unwrap();
        let records: Vec<&str> = text.lines().collect();
        assert_eq!(records.len(), 1);
        assert!(
            records[0].ends_with(&format!(" | 10.10.204.87 | {DEV_LINE}")),
            "{}",
            records[0]
        );
    }
}
