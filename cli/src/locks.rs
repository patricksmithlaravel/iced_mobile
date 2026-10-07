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
                return Err(busy(dir, &path, name, wait));
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

/// The holder a lock file records: `{"pid": N, "run": "<id>"}`.
fn holder(path: &Path) -> (Option<u64>, Option<String>) {
    let value: serde_json::Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(text.trim()).ok())
        .unwrap_or_default();
    (
        value.get("pid").and_then(serde_json::Value::as_u64),
        value
            .get("run")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    )
}

/// `run.lock_busy`, naming the holder. When the holder is a detached run
/// (`<root>/runs/<id>/detached.json`, the locks dir being `<root>/locks`),
/// the fix is to `icm wait` for it.
fn busy(dir: &Path, path: &Path, name: &str, wait: Option<Duration>) -> IcmError {
    let (pid, run) = holder(path);
    let mut detail = format!("the {name} lock is held");
    match (&run, pid) {
        (Some(run), Some(pid)) => detail.push_str(&format!(" by run {run} (pid {pid})")),
        (Some(run), None) => detail.push_str(&format!(" by run {run}")),
        (None, Some(pid)) => detail.push_str(&format!(" by pid {pid}")),
        (None, None) => {}
    }
    if let Some(wait) = wait {
        detail.push_str(&format!(" (waited {})", crate::time::format_duration(wait)));
    }
    let detached = run.as_deref().filter(|run| {
        crate::output::rundir::is_run_id(run)
            && dir.parent().is_some_and(|root| {
                crate::output::rundir::runs_dir(root)
                    .join(run)
                    .join("detached.json")
                    .is_file()
            })
    });
    let error = IcmError::new(CheckId::RunLockBusy, detail).evidence(Evidence::file(path));
    match detached {
        Some(run) => error.fix(
            format!(
                "A detached icm (run {run}) holds the lock: wait for its result with `icm wait`, then rerun; or rerun with --wait-lock 9m."
            ),
            &[&format!("icm wait {run} --timeout 9m --json -q")],
        ),
        None => error.fix(
            "Wait for the other icm to finish, rerun with --wait-lock 9m, or stop it.",
            &[],
        ),
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
        assert!(
            busy.detail.contains("by run run-a (pid "),
            "{}",
            busy.detail
        );
        assert!(busy.fix.commands.is_empty());

        drop(held);
        let again = acquire(dir.path(), "web", None, "run-c").unwrap();
        drop(again);

        // Different platforms do not contend.
        let _a = acquire(dir.path(), "android", None, "x").unwrap();
        let _b = acquire(dir.path(), "ios-sim", None, "y").unwrap();
    }

    #[test]
    fn a_detached_holder_is_waited_for() {
        let root = tempfile::tempdir().unwrap();
        let locks = root.path().join("locks");
        let run = "20261007T043524Z-run-android-f660";
        let run_dir = crate::output::rundir::runs_dir(root.path()).join(run);
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("detached.json"), "{\"pid\":1}").unwrap();
        let _held = acquire(&locks, "android", None, run).unwrap();
        let busy = acquire(&locks, "android", None, "other").unwrap_err();
        assert!(
            busy.detail.contains(&format!("by run {run} (pid ")),
            "{}",
            busy.detail
        );
        assert_eq!(
            busy.fix.commands,
            [format!("icm wait {run} --timeout 9m --json -q")]
        );
    }
}
