//! The device's two text messages: the status line it broadcasts every five
//! seconds, and its reply to the `targets` command. Parsing only, no I/O.
//!
//! A line that does not match yields `None` or is skipped: the port is open
//! to the LAN, and anything may arrive on it.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use serde::Serialize;

/// What the device reports for one target's sender, as it writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TargetState {
    #[serde(rename = "A")]
    Alive,
    #[serde(rename = "B")]
    Backoff,
    #[serde(rename = "C")]
    NoSocket,
    #[serde(rename = "off")]
    Off,
}

/// One target's counters from a status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetDiag {
    pub state: TargetState,
    pub ok: u64,
    pub fail: u64,
    pub skip: u64,
}

/// One status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diag {
    pub up: u64,
    /// The device's own clock in milliseconds; absent on firmware before 1.5.
    pub ms: Option<u64>,
    pub ip: String,
    pub rx: u64,
    pub dhcp_ok: u64,
    pub dhcp_fail: u64,
    pub rtr: String,
    pub conflict: bool,
    pub targets: BTreeMap<u8, TargetDiag>,
}

/// One line of the `targets` reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub n: u8,
    pub on: bool,
    pub ip: String,
    pub port: u16,
}

/// Parse a status line such as
/// `XRFD up=123 ms=123456 ip=10.10.204.87 rx=7380 dhcp=0/0 rtr=Y t0=A,7380,0,0 t1=off`.
pub fn parse_diag(line: &str) -> Option<Diag> {
    let mut words = line.split_whitespace().skip_while(|word| *word != "XRFD");
    words.next()?;
    let up = number(words.next()?.strip_prefix("up=")?)?;
    let mut word = words.next()?;
    let ms = match word.strip_prefix("ms=") {
        Some(text) => {
            word = words.next()?;
            Some(number(text)?)
        }
        None => None,
    };
    let ip = word.strip_prefix("ip=").filter(|ip| !ip.is_empty())?;
    let rx = number(words.next()?.strip_prefix("rx=")?)?;
    let (dhcp_ok, dhcp_fail) = words.next()?.strip_prefix("dhcp=")?.split_once('/')?;
    let rtr = words.next()?.strip_prefix("rtr=")?.chars().next()?;
    Some(Diag {
        up,
        ms,
        ip: ip.to_string(),
        rx,
        dhcp_ok: number(dhcp_ok)?,
        dhcp_fail: number(dhcp_fail)?,
        rtr: rtr.to_string(),
        conflict: line.contains(" CONFLICT"),
        targets: words.filter_map(target_word).collect(),
    })
}

/// Parse the `targets` reply, one `t0 on 10.10.204.184:50001` per line.
pub fn parse_targets(reply: &str) -> Vec<Target> {
    reply.lines().filter_map(target_line).collect()
}

/// `t0=A,7380,0,0` or `t1=off`; counters the device left out read as zero.
fn target_word(word: &str) -> Option<(u8, TargetDiag)> {
    let (name, value) = word.split_once('=')?;
    let n = target_number(name)?;
    let mut fields = value.split(',');
    let state = match fields.next()? {
        "A" => TargetState::Alive,
        "B" => TargetState::Backoff,
        "C" => TargetState::NoSocket,
        "off" => TargetState::Off,
        _ => return None,
    };
    let mut count = || fields.next().and_then(number).unwrap_or(0);
    let (ok, fail, skip) = (count(), count(), count());
    Some((
        n,
        TargetDiag {
            state,
            ok,
            fail,
            skip,
        },
    ))
}

fn target_line(line: &str) -> Option<Target> {
    let mut words = line.split(' ');
    let n = target_number(words.next()?)?;
    let on = match words.next()? {
        "on" => true,
        "off" => false,
        _ => return None,
    };
    let (ip, port) = words.next()?.split_once(':')?;
    ip.parse::<Ipv4Addr>().ok()?;
    Some(Target {
        n,
        on,
        ip: ip.to_string(),
        port: port.parse().ok()?,
    })
}

/// `t0` to `t9`: the letter and one digit.
fn target_number(name: &str) -> Option<u8> {
    let digit = name.strip_prefix('t')?;
    if digit.len() != 1 {
        return None;
    }
    digit.parse().ok()
}

/// Digits only: `parse` alone would also take a leading `+`.
fn number(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = "XRFD up=336055 ms=336055050 ip=10.10.204.123 rx=20157893 dhcp=0/0 rtr=Y \
                        t0=A,20157893,0,0 t1=A,20157893,0,0 t2=off t3=off";

    #[test]
    fn parse_diag_basic_fields() {
        // Red when a field is read from the wrong word.
        let d = parse_diag(REAL).unwrap();
        assert_eq!(d.up, 336055);
        assert_eq!(d.ms, Some(336055050));
        assert_eq!(d.ip, "10.10.204.123");
        assert_eq!(d.rx, 20157893);
        assert_eq!((d.dhcp_ok, d.dhcp_fail), (0, 0));
        assert_eq!(d.rtr, "Y");
        assert!(!d.conflict);
    }

    #[test]
    fn parse_diag_targets() {
        // Red when the counters are dropped or an `off` target is skipped.
        let d = parse_diag(REAL).unwrap();
        let alive = TargetDiag {
            state: TargetState::Alive,
            ok: 20157893,
            fail: 0,
            skip: 0,
        };
        let off = TargetDiag {
            state: TargetState::Off,
            ok: 0,
            fail: 0,
            skip: 0,
        };
        assert_eq!(d.targets[&0], alive);
        assert_eq!(d.targets[&2], off);
    }

    #[test]
    fn parse_diag_legacy_no_ms() {
        // Red when `ms=` becomes required: firmware before 1.5 does not send it.
        let d = parse_diag("XRFD up=10 ip=1.2.3.4 rx=5 dhcp=1/2 rtr=N").unwrap();
        assert_eq!(d.ms, None);
        assert_eq!(d.rtr, "N");
        assert_eq!(d.dhcp_fail, 2);
    }

    #[test]
    fn parse_diag_conflict() {
        // Red when the trailing CONFLICT word is not looked for.
        assert!(parse_diag(&format!("{REAL} CONFLICT")).unwrap().conflict);
    }

    #[test]
    fn parse_diag_garbage_returns_none() {
        // Red when a line that is not a status line is accepted.
        assert_eq!(parse_diag("hello world"), None);
        assert_eq!(parse_diag("XRFD up="), None);
    }

    #[test]
    fn parse_targets_reads_each_line_and_skips_the_rest() {
        // Red when a foreign line stops the parse or lands in the list.
        let reply = "t0 on 10.10.204.184:50001\nt1 off 10.10.204.175:50001\ngarbage line\n";
        assert_eq!(
            parse_targets(reply),
            vec![
                Target {
                    n: 0,
                    on: true,
                    ip: "10.10.204.184".into(),
                    port: 50001
                },
                Target {
                    n: 1,
                    on: false,
                    ip: "10.10.204.175".into(),
                    port: 50001
                },
            ]
        );
    }

    #[test]
    fn target_state_is_written_as_the_device_writes_it() {
        // Red when a serde rename goes: the page reads "A", "B", "C" and "off".
        let written: Vec<String> = [
            TargetState::Alive,
            TargetState::Backoff,
            TargetState::NoSocket,
            TargetState::Off,
        ]
        .iter()
        .map(|state| serde_json::to_string(state).unwrap())
        .collect();
        assert_eq!(written, ["\"A\"", "\"B\"", "\"C\"", "\"off\""]);
    }
}
