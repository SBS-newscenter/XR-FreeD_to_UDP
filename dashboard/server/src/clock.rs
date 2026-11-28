//! The wall clock, and a moment as people read it on the page and in the
//! history files. Everything that needs the time takes it as a parameter:
//! only the threads' outer loops read the clock, so a test passes a number.

use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, FixedOffset, Local, TimeZone, Utc};

const CLOCK_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

/// Seconds since the Unix epoch.
pub fn wall_now() -> f64 {
    // A clock set before 1970 reads as the epoch itself.
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0)
}

/// The moment in the server's own time zone, with its offset.
pub fn local_time(epoch: f64) -> DateTime<FixedOffset> {
    let seconds = epoch.floor() as i64;
    match Local.timestamp_opt(seconds, 0).single() {
        Some(local) => local.fixed_offset(),
        // An hour that a zone's clock change skips or repeats: UTC is unambiguous.
        None => DateTime::<Utc>::from_timestamp(seconds, 0)
            .unwrap_or_default()
            .fixed_offset(),
    }
}

/// `2026-11-27 12:00:05`, local time, as the event log prints it.
pub fn clock_text(epoch: f64) -> String {
    local_time(epoch).format(CLOCK_FORMAT).to_string()
}
