//! Disk history: what the dashboard heard, kept after the process is gone.
//!
//! Two append-only text files per local day, under one directory:
//!
//! ```text
//! events-YYYY-MM-DD.txt   <time> | <kind> | <message>      the event log
//! status-YYYY-MM-DD.txt   <time> | <device ip> | <line>    every status line
//! ```
//!
//! Events are small and kept. Status files older than `keep_days` are deleted.
//!
//! Failure: a write the disk refuses is reported once and dropped. The bridge
//! keeps forwarding to the page, which is worth more than the record.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Duration, FixedOffset, NaiveDate};
use serde::Serialize;

const TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S %z";
const DAY_FORMAT: &str = "%Y-%m-%d";
const SEPARATOR: &str = " | ";

/// What an event log line is about; the page colours by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Info,
    Warn,
    Cmd,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Info => "info",
            Kind::Warn => "warn",
            Kind::Cmd => "cmd",
        }
    }

    fn parse(text: &str) -> Option<Kind> {
        match text {
            "info" => Some(Kind::Info),
            "warn" => Some(Kind::Warn),
            "cmd" => Some(Kind::Cmd),
            _ => None,
        }
    }
}

/// One line of the event log, in the shape the page reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogEntry {
    /// Local time without its zone, `2026-11-27 12:00:05`.
    pub t: String,
    pub k: Kind,
    pub m: String,
}

/// Where a complaint about the disk goes: stderr in the service, a list in a test.
pub type Reporter = Box<dyn Fn(&str) + Send + Sync>;

pub struct History {
    directory: PathBuf,
    keep_days: i64,
    report: Reporter,
    /// Held across each append: events come from the bridge and from routes.
    guarded: Mutex<Guarded>,
}

struct Guarded {
    pruned_on: Option<NaiveDate>,
    last_complaint: String,
}

impl History {
    pub fn new(directory: PathBuf, keep_days: i64) -> History {
        History::with_reporter(
            directory,
            keep_days,
            Box::new(|complaint| eprintln!("{complaint}")),
        )
    }

    pub fn with_reporter(directory: PathBuf, keep_days: i64, report: Reporter) -> History {
        let guarded = Mutex::new(Guarded {
            pruned_on: None,
            last_complaint: String::new(),
        });
        History {
            directory,
            keep_days,
            report,
            guarded,
        }
    }

    pub fn event(&self, kind: Kind, message: &str, when: DateTime<FixedOffset>) {
        let mut guarded = self.lock();
        self.append(&mut guarded, "events", when, kind.as_str(), message);
    }

    pub fn status(&self, device_ip: &str, line: &str, when: DateTime<FixedOffset>) {
        let mut guarded = self.lock();
        self.append(&mut guarded, "status", when, device_ip, line);
        let today = when.date_naive();
        if guarded.pruned_on != Some(today) {
            // The first line of each day.
            guarded.pruned_on = Some(today);
            self.prune(&mut guarded, today);
        }
    }

    /// The newest `limit` events, newest first.
    pub fn recent_events(&self, limit: usize) -> Vec<LogEntry> {
        let mut guarded = self.lock();
        let mut paths = self.files(&mut guarded, "events-");
        paths.sort_by(|a, b| b.cmp(a));
        let mut events = Vec::new();
        for (_, path) in paths {
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) => {
                    self.complain(&mut guarded, &format!("reading {}", path.display()), &error);
                    continue;
                }
            };
            for line in text.lines().rev() {
                if events.len() == limit {
                    return events;
                }
                // A torn or foreign line parses to nothing and is passed over.
                events.extend(event_line(line));
            }
        }
        events
    }

    fn append(
        &self,
        guarded: &mut Guarded,
        stem: &str,
        when: DateTime<FixedOffset>,
        tag: &str,
        text: &str,
    ) {
        // One record is one line: a `targets` reply spans four.
        let one_line = text.replace('\r', "").replace('\n', " / ");
        let path = self
            .directory
            .join(format!("{stem}-{}.txt", when.format(DAY_FORMAT)));
        let time = when.format(TIME_FORMAT);
        let record = format!("{time}{SEPARATOR}{tag}{SEPARATOR}{one_line}\n");
        let written = fs::create_dir_all(&self.directory)
            .and_then(|()| OpenOptions::new().create(true).append(true).open(&path))
            .and_then(|mut file| file.write_all(record.as_bytes()));
        if let Err(error) = written {
            self.complain(guarded, &format!("appending to {}", path.display()), &error);
        }
    }

    fn prune(&self, guarded: &mut Guarded, today: NaiveDate) {
        let cutoff = today - Duration::days(self.keep_days);
        for (day, path) in self.files(guarded, "status-") {
            if day < cutoff
                && let Err(error) = fs::remove_file(&path)
            {
                self.complain(guarded, &format!("deleting {}", path.display()), &error);
            }
        }
    }

    /// The files this module named with `prefix`, each with its day.
    fn files(&self, guarded: &mut Guarded, prefix: &str) -> Vec<(NaiveDate, PathBuf)> {
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            // No directory yet is a first start, not a fault.
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                let doing = format!("listing {}", self.directory.display());
                self.complain(guarded, &doing, &error);
                return Vec::new();
            }
        };
        let mut files = Vec::new();
        for path in entries.flatten().map(|entry| entry.path()) {
            let day = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix(prefix)?.strip_suffix(".txt"))
                .and_then(|day| NaiveDate::parse_from_str(day, DAY_FORMAT).ok());
            if let Some(day) = day {
                files.push((day, path));
            }
        }
        files
    }

    fn complain(&self, guarded: &mut Guarded, doing: &str, error: &io::Error) {
        // A full disk fails every five seconds: say it once, not on every line.
        let complaint = format!("history: {doing} failed: {error}");
        if complaint != guarded.last_complaint {
            (self.report)(&complaint);
            guarded.last_complaint = complaint;
        }
    }

    fn lock(&self) -> MutexGuard<'_, Guarded> {
        // A thread that panicked mid-append leaves nothing half-updated here.
        self.guarded.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn event_line(line: &str) -> Option<LogEntry> {
    let mut fields = line.splitn(3, SEPARATOR);
    let (time, kind, message) = (fields.next()?, fields.next()?, fields.next()?);
    // The page shows local time without the zone: the first 19 characters.
    Some(LogEntry {
        t: time.get(..19)?.to_string(),
        k: Kind::parse(kind)?,
        m: message.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::TimeZone;

    use super::*;
    use crate::testing::TempDir;

    fn noon() -> DateTime<FixedOffset> {
        let kst = FixedOffset::east_opt(9 * 3600).unwrap();
        kst.with_ymd_and_hms(2026, 11, 27, 12, 0, 5).unwrap()
    }

    fn names(dir: &TempDir) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn event_is_one_record_in_that_days_file() {
        // Red when append stops writing, or names the file by another day.
        let dir = TempDir::new();
        History::new(dir.path(), 30).event(Kind::Warn, "NO SIGNAL: 10.10.204.87", noon());
        let text = fs::read_to_string(dir.path().join("events-2026-11-27.txt")).unwrap();
        assert_eq!(
            text,
            "2026-11-27 12:00:05 +0900 | warn | NO SIGNAL: 10.10.204.87\n"
        );
    }

    #[test]
    fn multi_line_message_stays_one_record() {
        // Red when the newline replacement goes: a `targets` reply would then
        // spill into lines recent_events cannot read back.
        let dir = TempDir::new();
        History::new(dir.path(), 30).event(Kind::Cmd, "targets  ->  t0 on\nt1 off", noon());
        let text = fs::read_to_string(dir.path().join("events-2026-11-27.txt")).unwrap();
        assert_eq!(
            text,
            "2026-11-27 12:00:05 +0900 | cmd | targets  ->  t0 on / t1 off\n"
        );
    }

    #[test]
    fn recent_events_are_newest_first_across_days_and_limited() {
        // Red when the files or their lines are read oldest first, or the limit is ignored.
        let dir = TempDir::new();
        let history = History::new(dir.path(), 30);
        history.event(Kind::Info, "first", noon() - Duration::days(1));
        history.event(Kind::Info, "second", noon());
        history.event(Kind::Warn, "third", noon() + Duration::seconds(1));
        assert_eq!(
            history.recent_events(2),
            vec![
                LogEntry {
                    t: "2026-11-27 12:00:06".into(),
                    k: Kind::Warn,
                    m: "third".into()
                },
                LogEntry {
                    t: "2026-11-27 12:00:05".into(),
                    k: Kind::Info,
                    m: "second".into()
                },
            ]
        );
    }

    #[test]
    fn recent_events_of_an_empty_directory_is_empty() {
        // Red when a first start, with no history directory yet, complains or panics.
        let dir = TempDir::new();
        assert_eq!(
            History::new(dir.path().join("absent"), 30).recent_events(80),
            vec![]
        );
    }

    #[test]
    fn status_line_is_recorded_with_its_device() {
        // Red when status() stops appending.
        let dir = TempDir::new();
        let line = "XRFD up=10 ms=10000 ip=10.10.204.87 rx=5 dhcp=0/0 rtr=Y t0=A,5,0,0";
        History::new(dir.path(), 30).status("10.10.204.87", line, noon());
        let text = fs::read_to_string(dir.path().join("status-2026-11-27.txt")).unwrap();
        assert_eq!(
            text,
            format!("2026-11-27 12:00:05 +0900 | 10.10.204.87 | {line}\n")
        );
    }

    #[test]
    fn old_status_files_are_deleted_and_nothing_else() {
        // Red when prune is not called, deletes by the wrong cutoff, or reaches
        // past its own status files.
        let dir = TempDir::new();
        let history = History::new(dir.path(), 30);
        let (old, kept) = (noon() - Duration::days(31), noon() - Duration::days(30));
        history.status("10.10.204.87", "XRFD old", old);
        history.status("10.10.204.87", "XRFD kept", kept);
        history.event(Kind::Info, "an old event", old);
        fs::write(dir.path().join("status-notes.txt"), "not ours").unwrap();
        history.status("10.10.204.87", "XRFD today", noon());
        assert_eq!(
            names(&dir),
            [
                "events-2026-10-27.txt",
                "status-2026-10-28.txt",
                "status-2026-11-27.txt",
                "status-notes.txt"
            ]
        );
    }

    #[test]
    fn refused_write_is_reported_once_and_does_not_panic() {
        // Red when the error escapes into the bridge, or every line repeats the complaint.
        let dir = TempDir::new();
        let blocker = dir.path().join("history");
        fs::write(&blocker, "a file where the directory should be").unwrap();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&heard);
        let report: Reporter =
            Box::new(move |complaint| sink.lock().unwrap().push(complaint.to_string()));
        let history = History::with_reporter(blocker, 30, report);
        history.event(Kind::Info, "one", noon());
        history.event(Kind::Info, "two", noon());
        let heard = heard.lock().unwrap();
        assert_eq!(heard.len(), 1);
        assert!(heard[0].contains("appending to"), "{}", heard[0]);
    }
}
