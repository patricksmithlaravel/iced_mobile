//! Per-project, per-platform locks: `target/icm/locks/<platform>.lock`,
//! held with `std::fs::File::try_lock` (no libc flock; Appendix C item 30).
//! A busy lock exits 7 `run.lock_busy` unless `--wait-lock <dur>` is given.

use crate::catalogue::CheckId;
use crate::error::{Evidence, IcmError};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A held lock; released on drop.
#[derive(Debug)]
pub struct Lock {
    file: File,
    /// The lock file.
    pub path: PathBuf,
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Takes `<dir>/<name>.lock`, waiting up to `wait` for another holder.
pub fn acquire(
    dir: &Path,
    name: &str,
    wait: Option<Duration>,
    run_id: &str,
) -> Result<Lock, IcmError> {
    std::fs::create_dir_all(dir).map_err(|error| {
        IcmError::new(
            CheckId::InternalBug,
            format!("cannot create {}: {error}", dir.display()),
        )
    })?;
    let path = dir.join(format!("{name}.lock"));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| {
            IcmError::new(
                CheckId::InternalBug,
                format!("cannot open {}: {error}", path.display()),
            )
        })?;

    let deadline = wait.map(|wait| Instant::now() + wait);
    loop {
        match file.try_lock() {
            Ok(()) => {
                let _ = file.set_len(0);
                let _ = file.seek(SeekFrom::Start(0));
                let _ = writeln!(
                    file,
                    "{{\"pid\":{},\"run\":{}}}",
                    std::process::id(),
                    serde_json::Value::String(run_id.to_string())
                );
                return Ok(Lock { file, path });
            }
            Err(TryLockError::WouldBlock) => {
                if deadline.is_some_and(|deadline| Instant::now() < deadline) {
                    if crate::signals::pending().is_some() {
                        return Err(crate::output::interrupted(
                            crate::signals::pending().unwrap_or(libc::SIGINT),
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                let holder = std::fs::read_to_string(&path).unwrap_or_default();
                let mut detail = format!("the {name} lock is held");
                if !holder.trim().is_empty() {
                    detail.push_str(&format!(" by {}", holder.trim()));
                }
                if let Some(wait) = wait {
                    detail.push_str(&format!(" (waited {})", crate::time::format_duration(wait)));
                }
                return Err(IcmError::new(CheckId::RunLockBusy, detail)
                    .evidence(Evidence::file(&path))
                    .fix(
                        "Wait for the other icm to finish, pass --wait-lock <dur>, or stop it.",
                        &[],
                    ));
            }
            Err(TryLockError::Error(error)) => {
                return Err(IcmError::new(
                    CheckId::InternalBug,
                    format!("cannot lock {}: {error}", path.display()),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_is_busy_until_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let held = acquire(dir.path(), "web", None, "run-a").unwrap();
        let text = std::fs::read_to_string(&held.path).unwrap();
        assert!(text.contains("\"run\":\"run-a\""), "{text}");

        // A second handle in this process sees the lock as held.
        let busy =
            acquire(dir.path(), "web", Some(Duration::from_millis(250)), "run-b").unwrap_err();
        assert_eq!(busy.id, "run.lock_busy");
        assert_eq!(busy.exit, crate::exit::Exit::Device);
        assert!(busy.detail.contains("run-a"), "{}", busy.detail);

        drop(held);
        let again = acquire(dir.path(), "web", None, "run-c").unwrap();
        drop(again);

        // Different platforms do not contend.
        let _a = acquire(dir.path(), "android", None, "x").unwrap();
        let _b = acquire(dir.path(), "ios-sim", None, "y").unwrap();
    }
}
