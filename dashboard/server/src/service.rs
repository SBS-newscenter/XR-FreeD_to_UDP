//! What the service manager on the host reads from this service: the port in
//! `server.toml`, the pidfile, and the start and stop lines of its day's log.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use chrono::{DateTime, FixedOffset};

/// The unit's name, as `server.toml` and the log's lines carry it.
pub const SERVICE: &str = "xr-freed-to-udp";

/// The `port` of `server.toml`. This is not a TOML parser: it reads the one
/// key this service needs, written as the manifest writes it, a `port = N`
/// line of its own.
pub fn port_from_manifest(manifest: &str) -> Option<u16> {
    manifest.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        if key.trim() != "port" {
            return None;
        }
        let value = value.split('#').next()?.trim();
        value.parse().ok()
    })
}

/// The pidfile's text: the JSON the service manager reads.
pub fn pidfile_text(pid: u32, started: DateTime<FixedOffset>) -> String {
    let started_at = started.format("%Y-%m-%dT%H:%M:%S");
    format!(r#"{{"pid": {pid}, "command": "xrfd-dashboard", "started_at": "{started_at}"}}"#)
}

pub fn write_pidfile(path: &Path, pid: u32, started: DateTime<FixedOffset>) -> io::Result<()> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)?;
    }
    fs::write(path, pidfile_text(pid, started))
}

/// Append `<time> | <service> | <EVENT> | <detail>` to the day's file under
/// `log_dir`, the format the service manager's log view shares.
pub fn append_lifecycle(
    log_dir: &Path,
    event: &str,
    detail: &str,
    when: DateTime<FixedOffset>,
) -> io::Result<()> {
    fs::create_dir_all(log_dir)?;
    let path = log_dir.join(format!("{}.txt", when.format("%Y-%m-%d")));
    let time = when.format("%Y-%m-%d %H:%M:%S %z");
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{time} | {SERVICE} | {event} | {detail}")
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::testing::TempDir;

    fn noon() -> DateTime<FixedOffset> {
        let kst = FixedOffset::east_opt(9 * 3600).unwrap();
        kst.with_ymd_and_hms(2026, 11, 27, 12, 0, 5).unwrap()
    }

    #[test]
    fn port_is_read_from_the_manifest() {
        // Red when the port is taken from anywhere but server.toml, or a
        // trailing comment on its line breaks the read.
        let manifest = "[[service]]\nname = \"xr-freed-to-udp\"\nhost = \"0.0.0.0\"  # LAN\nport = 10000  # the page\nhealth_path = \"/health\"\n";
        assert_eq!(port_from_manifest(manifest), Some(10000));
        assert_eq!(port_from_manifest("name = \"x\"\n"), None);
        assert_eq!(port_from_manifest("port = \"ten\"\n"), None);
    }

    #[test]
    fn pidfile_is_the_json_the_manager_reads() {
        // Red when the pidfile stops being JSON with the process id in it.
        let text = pidfile_text(4242, noon());
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["pid"], 4242);
        assert_eq!(json["started_at"], "2026-11-27T12:00:05");
    }

    #[test]
    fn lifecycle_lines_are_appended_to_the_days_file() {
        // Red when a start or stop is not recorded, or overwrites the file.
        let dir = TempDir::new();
        append_lifecycle(&dir.path(), "STARTED", "version=0.2.0 pid=1", noon()).unwrap();
        append_lifecycle(&dir.path(), "STOPPED", "pid=1", noon()).unwrap();
        let text = fs::read_to_string(dir.path().join("2026-11-27.txt")).unwrap();
        assert_eq!(
            text,
            "2026-11-27 12:00:05 +0900 | xr-freed-to-udp | STARTED | version=0.2.0 pid=1\n\
             2026-11-27 12:00:05 +0900 | xr-freed-to-udp | STOPPED | pid=1\n"
        );
    }
}
