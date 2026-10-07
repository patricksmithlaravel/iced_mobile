//! Session records (design §3 "Sessions", §4.6):
//! `target/icm/sessions/<platform>.json`, one per platform per project.
//!
//! A record is a JSON object with at least `schema` (`icm.session/1`),
//! `platform`, `pid` (the process to stop), `pgid` (its process group, when
//! icm started it in its own), `run` (the run that started it), `started`
//! and `marker`: a string the process's command line must contain for icm
//! to treat the pid as still the same process (pids are reused). Platforms
//! add their own fields (the web session adds `url`, `port`, `control`,
//! `chrome`, `logs`, ...). Records may hold a control token, so they are
//! written with mode 0600.
//!
//! `icm ps` lists records; `icm stop` ends them; `icm run <platform>`
//! replaces this project's record for that platform (Appendix C item 13).

use crate::process::{self, Cmd};
use serde_json::Value;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The record schema.
pub const SCHEMA: &str = "icm.session/1";

/// The record for a platform.
pub fn path(dir: &Path, platform: &str) -> PathBuf {
    dir.join(format!("{platform}.json"))
}

/// Reads a platform's record, if there is one.
pub fn read(dir: &Path, platform: &str) -> Option<Value> {
    let text = std::fs::read_to_string(path(dir, platform)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Every record in `dir`, sorted by platform.
pub fn list(dir: &Path) -> Vec<(String, Value)> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut records: Vec<(String, Value)> = read_dir
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let platform = name.strip_suffix(".json")?.to_string();
            let text = std::fs::read_to_string(entry.path()).ok()?;
            let value: Value = serde_json::from_str(&text).ok()?;
            value.get("schema")?;
            Some((platform, value))
        })
        .collect();
    records.sort_by(|a, b| a.0.cmp(&b.0));
    records
}

/// Writes a record atomically, readable only by the user.
pub fn write(dir: &Path, platform: &str, record: &Value) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::create_dir_all(dir)?;
    let target = path(dir, platform);
    let tmp = dir.join(format!(".{platform}.json.tmp-{}", std::process::id()));
    {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        let mut text = serde_json::to_string_pretty(record).unwrap_or_default();
        text.push('\n');
        file.write_all(text.as_bytes())?;
    }
    std::fs::rename(&tmp, &target)
}

/// Removes a platform's record, but only while it still names `pid` (a
/// newer session may have replaced it).
pub fn remove(dir: &Path, platform: &str, pid: i32) {
    if read(dir, platform).and_then(|record| record_pid(&record)) == Some(pid) {
        let _ = std::fs::remove_file(path(dir, platform));
    }
}

/// A record's `pid`.
pub fn record_pid(record: &Value) -> Option<i32> {
    record
        .get("pid")
        .and_then(Value::as_i64)
        .map(|pid| pid as i32)
}

/// A record's process group: `pgid`, else `pid`.
pub fn record_pgid(record: &Value) -> Option<i32> {
    record
        .get("pgid")
        .and_then(Value::as_i64)
        .map(|pgid| pgid as i32)
        .or_else(|| record_pid(record))
}

/// The command line of a running process (`ps -o command= -p <pid>`).
pub fn command_line(pid: i32) -> Option<String> {
    let outcome = process::run(
        &Cmd::tool("ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .timeout(Duration::from_secs(5)),
        None,
        None,
    )
    .ok()?;
    let text = outcome.stdout_text().trim().to_string();
    (outcome.success() && !text.is_empty()).then_some(text)
}

/// Whether the record's process is still the one it describes: the pid
/// exists and, when the record has a `marker`, its command line contains
/// it.
pub fn alive(record: &Value) -> bool {
    let Some(pid) = record_pid(record) else {
        return false;
    };
    if !crate::signals::alive(pid) {
        return false;
    }
    match record.get("marker").and_then(Value::as_str) {
        Some(marker) if !marker.is_empty() => {
            command_line(pid).is_some_and(|line| line.contains(marker))
        }
        _ => true,
    }
}

/// How a stop went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stopped {
    /// The process was not running.
    NotRunning,
    /// It exited after SIGTERM.
    Terminated,
    /// It needed SIGKILL.
    Killed,
}

/// Stops a record's process: SIGTERM to its group, up to `grace` for it to
/// exit, then SIGKILL. Checks [`alive`] first, so a reused pid is never
/// signalled.
pub fn terminate(record: &Value, grace: Duration) -> Stopped {
    if !alive(record) {
        return Stopped::NotRunning;
    }
    let (Some(pid), Some(pgid)) = (record_pid(record), record_pgid(record)) else {
        return Stopped::NotRunning;
    };

    let signal = |signal: i32| {
        if pgid > 1 {
            crate::signals::kill_group(pgid, signal);
        }
        // SAFETY: kill(2) has no memory-safety preconditions.
        unsafe {
            let _ = libc::kill(pid, signal);
        }
    };

    signal(libc::SIGTERM);
    if wait_gone(pid, grace) {
        return Stopped::Terminated;
    }
    signal(libc::SIGKILL);
    let _ = wait_gone(pid, Duration::from_secs(2));
    Stopped::Killed
}

/// Waits up to `limit` for a process to disappear (or become a zombie this
/// process may reap).
pub fn wait_gone(pid: i32, limit: Duration) -> bool {
    let until = Instant::now() + limit;
    loop {
        reap(pid);
        if !crate::signals::alive(pid) {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Reaps `pid` if it is an exited child of this process; returns whether
/// it was.
pub fn reap(pid: i32) -> bool {
    let mut status = 0;
    // SAFETY: waitpid with WNOHANG on a specific pid; it only fails (ECHILD)
    // when the pid is not our child.
    let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    reaped == pid
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::process::CommandExt;

    #[test]
    fn records_round_trip_privately() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let record = json!({"schema": SCHEMA, "platform": "web", "pid": 42, "token": "t"});
        write(dir.path(), "web", &record).unwrap();
        let mode = std::fs::metadata(path(dir.path(), "web"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(read(dir.path(), "web").unwrap(), record);
        assert_eq!(list(dir.path()).len(), 1);

        // Only the session that wrote it removes it.
        remove(dir.path(), "web", 7);
        assert!(read(dir.path(), "web").is_some());
        remove(dir.path(), "web", 42);
        assert!(read(dir.path(), "web").is_none());
    }

    #[test]
    fn stale_or_reused_pids_are_not_alive() {
        let me = std::process::id() as i32;
        assert!(alive(&json!({"pid": me})));
        assert!(!alive(&json!({"pid": me, "marker": "no-such-marker-xyz"})));
        assert!(!alive(&json!({"pid": 0})));
        assert_eq!(
            terminate(
                &json!({"pid": me, "marker": "no-such-marker-xyz"}),
                Duration::ZERO
            ),
            Stopped::NotRunning
        );
    }

    #[test]
    fn terminate_stops_a_process_group() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let record = json!({"pid": pid, "pgid": pid, "marker": "sleep"});
        assert!(alive(&record));
        assert_eq!(
            terminate(&record, Duration::from_secs(5)),
            Stopped::Terminated
        );
        assert!(!alive(&record));
        let _ = child.wait();
    }
}
