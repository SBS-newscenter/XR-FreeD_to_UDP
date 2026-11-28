//! What the dashboard knows: every device it has heard, the detail of the one
//! selected, and the event log. `State` is plain data changed by one caller at
//! a time; `Hub` puts it behind a lock and hands snapshots to the open pages.
//!
//! Every method that reads the time takes it as `now`, seconds since the Unix
//! epoch, so a test states the moment instead of waiting for it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::Serialize;

use crate::clock::{clock_text, local_time};
use crate::history::{History, Kind, LogEntry};
use crate::protocol::{Diag, Target, TargetDiag, TargetState};

/// A device is heard while its last status line is no older than this.
pub const DEVICE_LIVE_S: f64 = 12.0;
/// Event log lines kept in memory.
pub const LOG_MAXLEN: usize = 80;
/// Event log lines sent to the page.
const PAGE_LOG_LEN: usize = 40;
/// `ageSec` of a device never heard. A real age is not capped, or a device
/// silent for twelve days would read the same as one never seen.
const NEVER_HEARD_AGE: i64 = 999_999;
/// Open event streams served at once; each holds a thread.
pub const MAX_EVENT_STREAMS: usize = 32;
/// Snapshots queued for one page before the newest is dropped for it.
const SUBSCRIBER_BACKLOG: usize = 10;

/// Whether the selected device is being heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Signal {
    Ok,
    Lost,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    pub ip: String,
    pub age_sec: i64,
    pub live: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TargetView {
    pub n: u8,
    pub on: bool,
    pub ip: String,
    pub port: u16,
    pub state: TargetState,
    pub ok: u64,
    pub fail: u64,
    pub skip: u64,
}

/// What the page is sent. The field names are the page's contract.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub device_ip: String,
    pub devices: Vec<DeviceView>,
    pub age_sec: i64,
    pub live: bool,
    pub signal: Signal,
    pub last_seen: String,
    pub silent_sec: i64,
    pub listening_since: String,
    pub up: u64,
    pub rx: u64,
    pub fps: f64,
    pub dhcp_ok: u64,
    pub dhcp_fail: u64,
    pub rtr: String,
    pub conflict: bool,
    pub targets: Vec<TargetView>,
    pub log: Vec<LogEntry>,
}

/// The counters of the selected device, cleared when another is selected so
/// one device's numbers are never read as another's.
#[derive(Debug, Clone, PartialEq)]
struct Detail {
    /// When the selected device's detail was last filled; `None` since a switch.
    last_seen: Option<f64>,
    up: u64,
    rx: u64,
    fps: f64,
    dhcp_ok: u64,
    dhcp_fail: u64,
    rtr: String,
    conflict: bool,
    diag_targets: BTreeMap<u8, TargetDiag>,
    targets: Vec<Target>,
    /// The line before this one, for the rate: frames, device clock, wall clock.
    previous: Option<(u64, Option<u64>, f64)>,
}

impl Detail {
    fn empty() -> Detail {
        Detail {
            last_seen: None,
            up: 0,
            rx: 0,
            fps: 0.0,
            dhcp_ok: 0,
            dhcp_fail: 0,
            rtr: "?".to_string(),
            conflict: false,
            diag_targets: BTreeMap::new(),
            targets: Vec::new(),
            previous: None,
        }
    }
}

pub struct State {
    history: Option<Arc<History>>,
    started: f64,
    /// The selected device; empty until the first one is heard.
    device_ip: String,
    /// Every device heard, with the time of its last status line.
    devices: BTreeMap<String, f64>,
    /// Devices whose silence has been logged, until they return.
    silent: BTreeSet<String>,
    detail: Detail,
    /// Newest first.
    log: VecDeque<LogEntry>,
}

impl State {
    /// A state that has heard nothing, with the event log read back from `history`.
    pub fn new(history: Option<Arc<History>>, now: f64) -> State {
        let log = match &history {
            Some(history) => history.recent_events(LOG_MAXLEN).into(),
            None => VecDeque::new(),
        };
        State {
            history,
            started: now,
            device_ip: String::new(),
            devices: BTreeMap::new(),
            silent: BTreeSet::new(),
            detail: Detail::empty(),
            log,
        }
    }

    pub fn device_ip(&self) -> &str {
        &self.device_ip
    }

    pub fn add_log(&mut self, kind: Kind, message: impl Into<String>, now: f64) {
        let message = message.into();
        if let Some(history) = &self.history {
            history.event(kind, &message, local_time(now));
        }
        self.log.push_front(LogEntry {
            t: clock_text(now),
            k: kind,
            m: message,
        });
        self.log.truncate(LOG_MAXLEN);
    }

    /// Take one status line from the device at `src_ip`.
    pub fn update_from_diag(&mut self, diag: &Diag, src_ip: &str, now: f64) {
        // Every device that broadcasts stays in the registry so the page can
        // offer it; only the selected one fills the detail.
        let heard_before = self.devices.insert(src_ip.to_string(), now);
        let was_silent = self.silent.remove(src_ip);
        match heard_before {
            None => self.add_log(Kind::Info, format!("device found: {src_ip}"), now),
            Some(last_heard) if was_silent => {
                let since = clock_text(last_heard);
                let message = format!("signal restored: {src_ip} (silent since {since})");
                self.add_log(Kind::Info, message, now);
            }
            Some(_) => {}
        }
        if self.device_ip.is_empty() {
            self.device_ip = src_ip.to_string();
        }
        if src_ip == self.device_ip {
            self.take_detail(diag, now);
        }
    }

    fn take_detail(&mut self, diag: &Diag, now: f64) {
        // A real reboot resets uptime and frames together; the device's
        // millisecond clock wrapping resets uptime alone.
        if diag.up < self.detail.up && diag.rx < self.detail.rx {
            let (before, after) = (self.detail.up, diag.up);
            let message = format!("DEVICE REBOOTED (uptime reset: {before}s -> {after}s)");
            self.add_log(Kind::Warn, message, now);
            self.detail.previous = None;
        }
        if diag.conflict && !self.detail.conflict {
            self.add_log(Kind::Warn, "IP CONFLICT detected on device LAN!", now);
        }
        if let Some(fps) = rate(self.detail.previous, diag, now) {
            self.detail.fps = fps;
        }
        let detail = &mut self.detail;
        detail.last_seen = Some(now);
        detail.up = diag.up;
        detail.rx = diag.rx;
        detail.dhcp_ok = diag.dhcp_ok;
        detail.dhcp_fail = diag.dhcp_fail;
        detail.rtr = diag.rtr.clone();
        detail.conflict = diag.conflict;
        detail.diag_targets = diag.targets.clone();
        detail.previous = Some((diag.rx, diag.ms, now));
    }

    /// Log each device whose status lines have stopped, once per silence.
    pub fn mark_silent(&mut self, now: f64) {
        let newly_silent: Vec<(String, f64)> = self
            .devices
            .iter()
            .filter(|(ip, heard)| now - **heard > DEVICE_LIVE_S && !self.silent.contains(*ip))
            .map(|(ip, heard)| (ip.clone(), *heard))
            .collect();
        for (ip, heard) in newly_silent {
            let message = format!("NO SIGNAL: {ip} (last status {})", clock_text(heard));
            self.add_log(Kind::Warn, message, now);
            self.silent.insert(ip);
        }
    }

    /// Read from the registry, not from the detail, which a switch of device
    /// clears until that device's next line: a live device just switched to
    /// is `Ok`, never `Lost`.
    pub fn signal(&self, now: f64) -> Signal {
        match self.devices.get(&self.device_ip) {
            None => Signal::None,
            Some(heard) if now - heard <= DEVICE_LIVE_S => Signal::Ok,
            Some(_) => Signal::Lost,
        }
    }

    /// Point the dashboard at another device it has heard. False when it has
    /// never heard `ip`.
    pub fn select_device(&mut self, ip: &str, now: f64) -> bool {
        if !self.devices.contains_key(ip) {
            return false;
        }
        if ip != self.device_ip {
            self.device_ip = ip.to_string();
            self.detail = Detail::empty();
            self.add_log(Kind::Info, format!("switched to {ip}"), now);
        }
        true
    }

    /// Store the selected device's `targets` reply.
    pub fn set_targets(&mut self, targets: Vec<Target>) {
        self.detail.targets = targets;
    }

    pub fn snapshot(&self, now: f64) -> Snapshot {
        let detail = &self.detail;
        let age_sec = detail
            .last_seen
            .map_or(NEVER_HEARD_AGE, |seen| (now - seen) as i64);
        let selected_heard = self.devices.get(&self.device_ip).copied();
        Snapshot {
            device_ip: self.device_ip.clone(),
            devices: self
                .devices
                .iter()
                .map(|(ip, heard)| device_view(ip, *heard, now))
                .collect(),
            age_sec,
            live: !self.device_ip.is_empty() && age_sec <= DEVICE_LIVE_S as i64,
            signal: self.signal(now),
            last_seen: selected_heard.map(clock_text).unwrap_or_default(),
            silent_sec: selected_heard.map_or(0, |heard| (now - heard) as i64),
            listening_since: clock_text(self.started),
            up: detail.up,
            rx: detail.rx,
            fps: detail.fps,
            dhcp_ok: detail.dhcp_ok,
            dhcp_fail: detail.dhcp_fail,
            rtr: detail.rtr.clone(),
            conflict: detail.conflict,
            targets: detail
                .targets
                .iter()
                .map(|target| self.target_view(target))
                .collect(),
            log: self.log.iter().take(PAGE_LOG_LEN).cloned().collect(),
        }
    }

    /// A target's address from the `targets` reply, with its counters from the
    /// status line; a target the status line does not name reads as off.
    fn target_view(&self, target: &Target) -> TargetView {
        let off = TargetDiag {
            state: TargetState::Off,
            ok: 0,
            fail: 0,
            skip: 0,
        };
        let diag = self
            .detail
            .diag_targets
            .get(&target.n)
            .copied()
            .unwrap_or(off);
        TargetView {
            n: target.n,
            on: target.on,
            ip: target.ip.clone(),
            port: target.port,
            state: diag.state,
            ok: diag.ok,
            fail: diag.fail,
            skip: diag.skip,
        }
    }
}

fn device_view(ip: &str, heard: f64, now: f64) -> DeviceView {
    DeviceView {
        ip: ip.to_string(),
        age_sec: (now - heard) as i64,
        live: now - heard <= DEVICE_LIVE_S,
    }
}

/// Frames per second between the previous line and this one, to two places.
/// By the device's own clock when it sends one, so neither network delay nor
/// this server's load moves the figure; by the wall clock otherwise. `None`
/// when the two lines cannot give a rate, and the last figure stands.
fn rate(previous: Option<(u64, Option<u64>, f64)>, diag: &Diag, now: f64) -> Option<f64> {
    let (previous_rx, previous_ms, previous_time) = previous?;
    let frames = diag.rx.checked_sub(previous_rx)? as f64;
    let seconds = match (diag.ms, previous_ms) {
        (Some(ms), Some(previous_ms)) if ms >= previous_ms + 1000 => {
            (ms - previous_ms) as f64 / 1000.0
        }
        (None, _) if frames > 0.0 && now - previous_time > 0.5 => now - previous_time,
        _ => return None,
    };
    Some((frames / seconds * 100.0).round() / 100.0)
}

/// The state behind its lock, and the open pages waiting for snapshots.
pub struct Hub {
    state: Mutex<State>,
    subscribers: Mutex<Vec<SyncSender<Arc<str>>>>,
}

impl Hub {
    pub fn new(state: State) -> Hub {
        Hub {
            state: Mutex::new(state),
            subscribers: Mutex::new(Vec::new()),
        }
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        // A thread that panicked while holding the lock must not take the
        // service with it; the state it left is still the best there is.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn snapshot_json(&self, now: f64) -> String {
        let snapshot = self.state().snapshot(now);
        // A struct of strings and numbers always serialises.
        serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".to_string())
    }

    /// A queue of the snapshots published from now on, or `None` when
    /// `MAX_EVENT_STREAMS` pages already hold one.
    pub fn subscribe(&self) -> Option<Receiver<Arc<str>>> {
        let mut subscribers = self.subscribers();
        if subscribers.len() >= MAX_EVENT_STREAMS {
            return None;
        }
        let (sender, receiver) = sync_channel(SUBSCRIBER_BACKLOG);
        subscribers.push(sender);
        Some(receiver)
    }

    /// Send the present snapshot to every open page.
    pub fn publish(&self, now: f64) {
        let json: Arc<str> = self.snapshot_json(now).into();
        self.subscribers()
            .retain(|subscriber| match subscriber.try_send(Arc::clone(&json)) {
                // A page that has fallen behind misses this one; the bridge never waits.
                Ok(()) | Err(TrySendError::Full(_)) => true,
                // The page closed its stream.
                Err(TrySendError::Disconnected(_)) => false,
            });
    }

    fn subscribers(&self) -> MutexGuard<'_, Vec<SyncSender<Arc<str>>>> {
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDateTime;
    use serde_json::Value;

    use super::*;
    use crate::protocol::parse_diag;
    use crate::testing::TempDir;

    const DEV: &str = "10.10.204.123";
    const DEV2: &str = "10.10.204.124";
    /// A fixed wall time: these tests read no clock.
    const T0: f64 = 1_800_000_000.0;

    fn line(up: u64, ms: u64, rx: u64, tail: &str) -> String {
        format!("XRFD up={up} ms={ms} ip={DEV} rx={rx} {tail}")
    }

    fn feed(state: &mut State, text: &str, src: &str, now: f64) {
        state.update_from_diag(&parse_diag(text).unwrap(), src, now);
    }

    fn heard(src: &str) -> State {
        let mut state = State::new(None, T0);
        feed(&mut state, &line(10, 10000, 5, "dhcp=0/0 rtr=Y"), src, T0);
        state
    }

    fn messages(state: &State, prefix: &str) -> Vec<String> {
        state
            .log
            .iter()
            .map(|e| e.m.clone())
            .filter(|m| m.starts_with(prefix))
            .collect()
    }

    fn is_clock(text: &str) -> bool {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").is_ok()
    }

    #[test]
    fn first_packet_sets_device_and_logs() {
        // Red when the first device heard is not selected, or not logged.
        let state = heard(DEV);
        let snap = state.snapshot(T0);
        assert_eq!(snap.device_ip, DEV);
        assert!(snap.live);
        assert_eq!((snap.up, snap.rx, snap.rtr.as_str()), (10, 5, "Y"));
        assert_eq!(messages(&state, "device found").len(), 1);
    }

    #[test]
    fn no_device_snapshot_is_safe() {
        // Red when a state that has heard nothing cannot be drawn.
        let snap = State::new(None, T0).snapshot(T0);
        assert_eq!(snap.device_ip, "");
        assert!(!snap.live);
        assert_eq!(snap.age_sec, NEVER_HEARD_AGE);
    }

    #[test]
    fn fps_from_device_ms() {
        // Red when the rate is taken from the wall clock while the device sends its own.
        let mut state = State::new(None, T0);
        feed(&mut state, &line(1, 1000, 0, "dhcp=0/0 rtr=Y"), DEV, T0);
        feed(
            &mut state,
            &line(2, 2000, 60, "dhcp=0/0 rtr=Y"),
            DEV,
            T0 + 7.0,
        );
        assert_eq!(state.snapshot(T0 + 7.0).fps, 60.0);
    }

    #[test]
    fn fps_from_the_wall_clock_without_device_ms() {
        // Red when firmware that sends no `ms=` gets no rate at all.
        let mut state = State::new(None, T0);
        feed(
            &mut state,
            "XRFD up=1 ip=1.2.3.4 rx=0 dhcp=0/0 rtr=Y",
            DEV,
            T0,
        );
        feed(
            &mut state,
            "XRFD up=3 ip=1.2.3.4 rx=120 dhcp=0/0 rtr=Y",
            DEV,
            T0 + 2.0,
        );
        assert_eq!(state.snapshot(T0 + 2.0).fps, 60.0);
    }

    #[test]
    fn reboot_detection_resets_fps_baseline() {
        // Red when uptime and frames both falling is not read as a reboot.
        let mut state = State::new(None, T0);
        feed(
            &mut state,
            &line(100, 100000, 6000, "dhcp=0/0 rtr=Y"),
            DEV,
            T0,
        );
        feed(
            &mut state,
            &line(2, 2000, 10, "dhcp=0/0 rtr=Y"),
            DEV,
            T0 + 5.0,
        );
        assert_eq!(messages(&state, "DEVICE REBOOTED").len(), 1);
        assert_eq!(state.detail.previous, Some((10, Some(2000), T0 + 5.0)));
    }

    #[test]
    fn log_capped_at_maxlen() {
        // Red when the event log grows without bound.
        let mut state = State::new(None, T0);
        for i in 0..200 {
            state.add_log(Kind::Info, format!("msg {i}"), T0);
        }
        assert_eq!(state.log.len(), LOG_MAXLEN);
        assert_eq!(state.log[0].m, "msg 199");
    }

    #[test]
    fn subscribe_receives_published_snapshot() {
        // Red when publish does not reach a subscriber, or a dropped one is kept.
        let hub = Hub::new(State::new(None, T0));
        let receiver = hub.subscribe().unwrap();
        hub.publish(T0);
        let snapshot: Value = serde_json::from_str(&receiver.try_recv().unwrap()).unwrap();
        assert_eq!(snapshot["deviceIp"], "");
        drop(receiver);
        hub.publish(T0);
        assert_eq!(hub.subscribers().len(), 0);
    }

    #[test]
    fn update_from_diag_publishes_to_subscribers() {
        // Red when a snapshot is taken before the status line is applied.
        let hub = Hub::new(State::new(None, T0));
        let receiver = hub.subscribe().unwrap();
        feed(
            &mut hub.state(),
            &line(1, 1000, 0, "dhcp=0/0 rtr=Y"),
            DEV,
            T0,
        );
        hub.publish(T0);
        let snapshot: Value = serde_json::from_str(&receiver.try_recv().unwrap()).unwrap();
        assert_eq!(snapshot["deviceIp"], DEV);
    }

    #[test]
    fn event_streams_are_capped() {
        // Red when the cap goes: each open stream holds a thread.
        let hub = Hub::new(State::new(None, T0));
        let held: Vec<_> = (0..MAX_EVENT_STREAMS)
            .map(|_| hub.subscribe().unwrap())
            .collect();
        assert!(hub.subscribe().is_none());
        drop(held);
    }

    #[test]
    fn log_timestamp_includes_date() {
        // Red when the stamp loses its date: the log spans days.
        let mut state = State::new(None, T0);
        state.add_log(Kind::Info, "hello", T0);
        assert!(is_clock(&state.snapshot(T0).log[0].t));
    }

    #[test]
    fn second_device_is_registered_not_discarded() {
        // Red when a second device is dropped, or takes the selection.
        let mut state = heard(DEV);
        feed(&mut state, &line(3, 3000, 1, "dhcp=0/0 rtr=Y"), DEV2, T0);
        let snap = state.snapshot(T0);
        assert_eq!(snap.device_ip, DEV);
        let ips: Vec<&str> = snap.devices.iter().map(|d| d.ip.as_str()).collect();
        assert_eq!(ips, [DEV, DEV2]);
        assert_eq!((snap.up, snap.rx), (10, 5));
    }

    #[test]
    fn unselected_device_does_not_overwrite_detail() {
        // Red when another device's line fills the selected device's detail,
        // or trips the reboot rule on it.
        let mut state = State::new(None, T0);
        feed(
            &mut state,
            &line(100, 100000, 6000, "dhcp=0/0 rtr=Y"),
            DEV,
            T0,
        );
        feed(&mut state, &line(1, 1000, 0, "dhcp=0/0 rtr=N"), DEV2, T0);
        let snap = state.snapshot(T0);
        assert_eq!((snap.up, snap.rx, snap.rtr.as_str()), (100, 6000, "Y"));
        assert!(messages(&state, "DEVICE REBOOTED").is_empty());
    }

    #[test]
    fn select_device_switches_and_resets_detail() {
        // Red when the previous device's counters linger after a switch.
        let mut state = State::new(None, T0);
        feed(
            &mut state,
            &line(100, 100000, 6000, "dhcp=3/1 rtr=Y"),
            DEV,
            T0,
        );
        feed(&mut state, &line(7, 7000, 42, "dhcp=0/0 rtr=N"), DEV2, T0);
        assert!(state.select_device(DEV2, T0));
        let snap = state.snapshot(T0);
        assert_eq!(snap.device_ip, DEV2);
        assert_eq!((snap.up, snap.rx, snap.fps), (0, 0, 0.0));
        assert_eq!(
            (snap.dhcp_ok, snap.dhcp_fail, snap.rtr.as_str()),
            (0, 0, "?")
        );
        assert!(snap.targets.is_empty());
        assert!(!snap.live, "no detail from it since the switch");
    }

    #[test]
    fn select_device_then_next_diag_populates() {
        // Red when the newly selected device's next line does not fill the detail.
        let mut state = State::new(None, T0);
        feed(
            &mut state,
            &line(100, 100000, 6000, "dhcp=0/0 rtr=Y"),
            DEV,
            T0,
        );
        feed(&mut state, &line(7, 7000, 42, "dhcp=0/0 rtr=N"), DEV2, T0);
        state.select_device(DEV2, T0);
        feed(
            &mut state,
            &line(8, 8000, 100, "dhcp=0/0 rtr=N"),
            DEV2,
            T0 + 1.0,
        );
        let snap = state.snapshot(T0 + 1.0);
        assert_eq!((snap.up, snap.rx, snap.live), (8, 100, true));
    }

    #[test]
    fn select_unknown_device_rejected() {
        // Red when the page can select an address that was never heard.
        let mut state = heard(DEV);
        assert!(!state.select_device("10.10.204.199", T0));
        assert_eq!(state.snapshot(T0).device_ip, DEV);
    }

    #[test]
    fn device_found_logged_once_per_ip() {
        // Red when every status line logs the device as found.
        let mut state = State::new(None, T0);
        for _ in 0..3 {
            feed(&mut state, &line(10, 10000, 5, "dhcp=0/0 rtr=Y"), DEV2, T0);
        }
        assert_eq!(messages(&state, "device found").len(), 1);
    }

    #[test]
    fn signal_is_none_then_ok_then_lost() {
        // Red when signal stops reading the registry, or the threshold moves.
        assert_eq!(State::new(None, T0).snapshot(T0).signal, Signal::None);
        let state = heard(DEV);
        assert_eq!(state.snapshot(T0 + 12.0).signal, Signal::Ok);
        assert_eq!(state.snapshot(T0 + 13.0).signal, Signal::Lost);
    }

    #[test]
    fn silence_is_logged_once() {
        // Red when mark_silent forgets which devices it has already logged: the
        // bridge calls it twice a second and the log would fill with one line.
        let mut state = heard(DEV);
        state.mark_silent(T0 + 12.0);
        assert!(messages(&state, "NO SIGNAL").is_empty());
        state.mark_silent(T0 + 13.0);
        state.mark_silent(T0 + 60.0);
        let logged = messages(&state, "NO SIGNAL");
        assert_eq!(logged.len(), 1);
        assert!(logged[0].contains(DEV));
    }

    #[test]
    fn return_after_silence_is_logged_and_rearms_the_alarm() {
        // Red when a returning device is not taken out of the silent set: its
        // second silence would then pass unlogged.
        let mut state = heard(DEV);
        state.mark_silent(T0 + 13.0);
        feed(
            &mut state,
            &line(110, 110000, 6000, "dhcp=0/0 rtr=Y"),
            DEV,
            T0 + 100.0,
        );
        assert_eq!(messages(&state, "signal restored").len(), 1);
        assert_eq!(state.snapshot(T0 + 100.0).signal, Signal::Ok);
        state.mark_silent(T0 + 113.0);
        assert_eq!(messages(&state, "NO SIGNAL").len(), 2);
    }

    #[test]
    fn snapshot_says_when_the_selected_device_was_last_heard() {
        // Red when lastSeen or silentSec leave the snapshot: the page's alarm
        // would have nothing to print.
        let snap = heard(DEV).snapshot(T0 + 300.0);
        assert_eq!(snap.silent_sec, 300);
        assert!(is_clock(&snap.last_seen), "{}", snap.last_seen);
        assert!(is_clock(&snap.listening_since), "{}", snap.listening_since);
    }

    #[test]
    fn a_long_silence_keeps_its_real_age() {
        // Red when the age is capped again: at 999999 s a device silent for
        // twenty days read the same as one never heard.
        let twenty_days = 20 * 86_400;
        let snap = heard(DEV).snapshot(T0 + twenty_days as f64);
        assert_eq!(snap.age_sec, twenty_days);
        assert_eq!(snap.devices[0].age_sec, twenty_days);
    }

    #[test]
    fn switching_to_a_live_device_raises_no_alarm() {
        // Red when signal reads the detail, which a switch clears until the
        // next line: the page would flash NO SIGNAL on every switch.
        let mut state = heard(DEV);
        feed(&mut state, &line(10, 10000, 5, "dhcp=0/0 rtr=Y"), DEV2, T0);
        state.select_device(DEV2, T0);
        assert_eq!(state.snapshot(T0 + 1.0).signal, Signal::Ok);
    }

    #[test]
    fn log_is_written_to_history_and_read_back_on_start() {
        // Red when add_log stops handing events to the history, or a new State
        // stops loading them: a restart would open with an empty event log.
        let dir = TempDir::new();
        let history = || Some(Arc::new(History::new(dir.path(), 30)));
        State::new(history(), T0).add_log(Kind::Warn, "before the restart", T0);
        let restarted = State::new(history(), T0 + 60.0);
        let logged: Vec<&str> = restarted.log.iter().map(|e| e.m.as_str()).collect();
        assert_eq!(logged, ["before the restart"]);
    }

    #[test]
    fn target_view_joins_the_address_with_the_status_lines_counters() {
        // Red when a target's counters are not matched to it by its number,
        // or a target the status line leaves out is not shown as off.
        let mut state = State::new(None, T0);
        feed(
            &mut state,
            &line(1, 1000, 9, "dhcp=0/0 rtr=Y t0=A,9,2,1"),
            DEV,
            T0,
        );
        state.set_targets(vec![
            Target {
                n: 0,
                on: true,
                ip: "10.10.204.184".into(),
                port: 50001,
            },
            Target {
                n: 1,
                on: false,
                ip: "10.10.204.166".into(),
                port: 50001,
            },
        ]);
        let targets = state.snapshot(T0).targets;
        assert_eq!(
            (
                targets[0].state,
                targets[0].ok,
                targets[0].fail,
                targets[0].skip
            ),
            (TargetState::Alive, 9, 2, 1)
        );
        assert_eq!((targets[1].state, targets[1].ok), (TargetState::Off, 0));
    }

    #[test]
    fn snapshot_is_written_with_the_names_the_page_reads() {
        // Red when a field is renamed here and not in the page: nothing else
        // ties the two together.
        let mut state = heard(DEV);
        state.set_targets(vec![Target {
            n: 0,
            on: true,
            ip: "10.10.204.184".into(),
            port: 50001,
        }]);
        let json: Value = serde_json::to_value(state.snapshot(T0)).unwrap();
        let keys =
            |value: &Value| -> Vec<String> { value.as_object().unwrap().keys().cloned().collect() };
        assert_eq!(
            keys(&json),
            [
                "ageSec",
                "conflict",
                "deviceIp",
                "devices",
                "dhcpFail",
                "dhcpOk",
                "fps",
                "lastSeen",
                "listeningSince",
                "live",
                "log",
                "rtr",
                "rx",
                "signal",
                "silentSec",
                "targets",
                "up"
            ]
        );
        assert_eq!(keys(&json["devices"][0]), ["ageSec", "ip", "live"]);
        assert_eq!(
            keys(&json["targets"][0]),
            ["fail", "ip", "n", "ok", "on", "port", "skip", "state"]
        );
        assert_eq!(keys(&json["log"][0]), ["k", "m", "t"]);
        assert_eq!(
            (json["signal"].as_str(), json["log"][0]["k"].as_str()),
            (Some("ok"), Some("info"))
        );
    }
}
