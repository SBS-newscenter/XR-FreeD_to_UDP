//! The XRFD dashboard's backend, as a library the service's binary is built
//! on. So far: the device's two messages (`protocol`), the disk history
//! (`history`), what the dashboard knows and sends the page (`state`), the
//! thread that hears the device and asks it (`bridge`), and the clock they
//! are stamped with (`clock`).

pub mod bridge;
pub mod clock;
pub mod history;
pub mod protocol;
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
