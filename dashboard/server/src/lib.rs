//! The XRFD dashboard's backend.
//!
//! One thread hears the device's status broadcast on UDP (`bridge`), keeps
//! what it said (`state`), appends it to disk (`history`), and the HTTP
//! threads (`http`) serve the page, its snapshot and its event stream.

pub mod bridge;
pub mod clock;
pub mod history;
pub mod http;
pub mod protocol;
pub mod service;
pub mod state;

/// UDP port the device answers control commands on.
pub const CTRL_PORT: u16 = 50998;
/// UDP port the device broadcasts its status line to.
pub const DIAG_PORT: u16 = 50999;
/// Status files kept on disk; about 3 MB a day per device.
pub const STATUS_KEEP_DAYS: i64 = 30;

#[cfg(test)]
pub(crate) mod testing {
    //! A directory of a test's own, removed when the test ends.

    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT: AtomicU32 = AtomicU32::new(0);

    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new() -> TempDir {
            let name = format!(
                "xrfd-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let path = std::env::temp_dir().join(name);
            std::fs::create_dir_all(&path).expect("a test's directory under the system's temp");
            TempDir(path)
        }

        pub fn path(&self) -> PathBuf {
            self.0.clone()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            // Left behind on failure, the system's temp cleaning takes it.
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
