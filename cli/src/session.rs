//! Sessions (design §3 "Sessions", §4.6): what `icm run` leaves running,
//! recorded in `target/icm/sessions/<platform>.json` so that `icm ps`
//! lists it and `icm stop` ends it without knowing the platform.
//!
//! The file (schema `icm.session/1`; unknown keys are kept and ignored):
//!
//! ```json
//! {"v":1, "platform":"ios-sim", "run":"<run id>", "started":"2026-10-06T21:03:11Z",
//!  "pid":14879,                                   // the app or session host icm started
//!  "pids":[{"pid":14880,"what":"emulator"}],      // more processes icm started
//!  "app":{"id":"com.example.app"},
//!  "device":{"kind":"simulator","id":"6F1…","name":"icm-iphone-17-ios-27.0","managed":true},
//!  "stop":[["xcrun","simctl","terminate","6F1…","com.example.app"]],
//!  "shutdown":[["xcrun","simctl","shutdown","6F1…"]],
//!  "url":"http://127.0.0.1:8787/"}
//! ```
//!
//! `icm stop <platform>` runs the `stop` commands, then sends SIGTERM (and
//! after a grace period SIGKILL) to each recorded process, to its whole
//! group when it leads one. A pid is signalled only while it is alive and
//! the process started before the session file was last written, so a pid
//! reused after a reboot is never touched. `--shutdown` also runs the
//! `shutdown` commands when the device is icm-managed (`icm-` names only).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// A session file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Session {
    /// The schema version (1).
    #[serde(default = "one")]
    pub v: u32,
    /// The dev platform.
    #[serde(default)]
    pub platform: String,
    /// The run that started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// When it started (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<String>,
    /// The app's process, or the session host's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
    /// More processes icm started for the session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pids: Vec<SessionProcess>,
    /// The app (`id`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<Value>,
    /// The device it runs on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<SessionDevice>,
    /// Commands that stop the app (argv each).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<Vec<String>>,
    /// Commands that shut an icm-managed device down (argv each).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shutdown: Vec<Vec<String>>,
    /// The web URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Whatever else the platform records (ports, log paths, launch mark).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn one() -> u32 {
    1
}

/// A process a session started.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionProcess {
    /// Its pid.
    pub pid: i32,
    /// What it is (`server`, `chrome`, `emulator`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub what: String,
}

/// The device a session runs on.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionDevice {
    /// `simulator`, `emulator`, `device`, `browser`, `host`.
    #[serde(default)]
    pub kind: String,
    /// The UDID or serial.
    #[serde(default, alias = "udid", alias = "serial")]
    pub id: String,
    /// Its name (`icm-iphone-17-ios-27.0`, `icm-api36`).
    #[serde(default)]
    pub name: String,
    /// Whether icm created it (and may shut it down).
    #[serde(default)]
    pub managed: bool,
}

/// `<sessions>/<platform>.json`.
/// What a platform knows about its app, beyond the session's host pids
/// (`icm ps`): an Android app runs on a device, a web app in a page of the
/// session's browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppState {
    /// The app runs.
    Running,
    /// It does not, and why ("not running on emulator-5580").
    Gone(String),
    /// The platform cannot tell (no device to ask, no answer).
    Unknown,
}

/// `<sessions>/<platform>.json`.
pub fn path(sessions_dir: &Path, platform: &str) -> PathBuf {
    sessions_dir.join(format!("{platform}.json"))
}

/// Writes a session file atomically.
pub fn write(sessions_dir: &Path, session: &Session) -> std::io::Result<PathBuf> {
    let path = path(sessions_dir, &session.platform);
    let mut text = serde_json::to_string_pretty(session).map_err(std::io::Error::other)?;
    text.push('\n');
    crate::output::rundir::write_atomic(&path, text.as_bytes())?;
    Ok(path)
}

/// Reads a session file.
pub fn read(path: &Path) -> Result<Session, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", crate::paths::display(path)))?;
    let mut session: Session = serde_json::from_str(&text).map_err(|error| {
        format!(
            "{} is not a session file: {error}",
            crate::paths::display(path)
        )
    })?;
    if session.platform.is_empty()
        && let Some(stem) = path.file_stem()
    {
        session.platform = stem.to_string_lossy().into_owned();
    }
    // Android's app runs on the device: its pid (`app_pid`, `pid` in files
    // written before that) is no host process to probe or signal.
    if session.extra.get("schema").and_then(Value::as_str) == Some(crate::android::session::SCHEMA)
        && let Some(pid) = session.pid.take()
    {
        let _ = session.extra.insert("app_pid".to_string(), pid.into());
    }
    Ok(session)
}

/// Every session file, sorted by platform.
pub fn list(sessions_dir: &Path) -> Vec<(PathBuf, Result<Session, String>)> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(sessions_dir)
        .map(|read| {
            read.flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|e| e == "json"))
                .collect()
        })
        .unwrap_or_default();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let session = read(&path);
            (path, session)
        })
        .collect()
}

impl Session {
    /// Every recorded pid, the main one first.
    pub fn all_pids(&self) -> Vec<i32> {
        let mut pids: Vec<i32> = self.pid.into_iter().collect();
        for process in &self.pids {
            if !pids.contains(&process.pid) {
                pids.push(process.pid);
            }
        }
        pids.retain(|pid| *pid > 1 && *pid != std::process::id() as i32);
        pids
    }
}

/// Whether a recorded pid is still the process the session started: alive,
/// and started no later than the session file was last written.
pub fn is_ours(pid: i32, written: Option<SystemTime>) -> bool {
    if pid <= 1 || !crate::signals::alive(pid) {
        return false;
    }
    let Some(written) = written else {
        return false;
    };
    match process_age(pid) {
        Some(age) => match SystemTime::now().checked_sub(age) {
            Some(started) => started <= written + Duration::from_secs(5),
            None => false,
        },
        None => false,
    }
}

/// How long a process has run (`ps -o etime=`).
pub fn process_age(pid: i32) -> Option<Duration> {
    let outcome = crate::process::run(
        &crate::process::Cmd::new("/bin/ps")
            .args(["-o", "etime=", "-p"])
            .arg(pid.to_string())
            .timeout(Duration::from_secs(10)),
        None,
        None,
    )
    .ok()?;
    if !outcome.success() {
        return None;
    }
    parse_etime(outcome.stdout_text().trim())
}

/// Parses `ps`'s elapsed time: `[[dd-]hh:]mm:ss`.
pub fn parse_etime(text: &str) -> Option<Duration> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let (days, rest) = match text.split_once('-') {
        Some((days, rest)) => (days.parse::<u64>().ok()?, rest),
        None => (0, text),
    };
    let parts: Vec<u64> = rest
        .split(':')
        .map(|part| part.parse::<u64>())
        .collect::<Result<_, _>>()
        .ok()?;
    let seconds = match parts.as_slice() {
        [m, s] => m * 60 + s,
        [h, m, s] => h * 3600 + m * 60 + s,
        _ => return None,
    };
    Some(Duration::from_secs(days * 86_400 + seconds))
}

/// Stops a process (its group when it leads one): SIGTERM, then SIGKILL
/// after `grace`. Returns whether it is gone.
pub fn terminate(pid: i32, grace: Duration) -> bool {
    // SAFETY: getpgid only reads the process table.
    let leads_group = unsafe { libc::getpgid(pid) } == pid;
    let send = |signal: i32| {
        if leads_group {
            crate::signals::kill_group(pid, signal);
        } else {
            // SAFETY: kill with a checked pid.
            unsafe {
                let _ = libc::kill(pid, signal);
            }
        }
    };
    send(libc::SIGTERM);
    let until = std::time::Instant::now() + grace;
    while std::time::Instant::now() < until {
        if !crate::signals::alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    send(libc::SIGKILL);
    std::thread::sleep(Duration::from_millis(100));
    !crate::signals::alive(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etimes_parse() {
        assert_eq!(parse_etime("00:05"), Some(Duration::from_secs(5)));
        assert_eq!(parse_etime("   01:02"), Some(Duration::from_secs(62)));
        assert_eq!(parse_etime("1:02:03"), Some(Duration::from_secs(3723)));
        assert_eq!(
            parse_etime("2-01:00:00"),
            Some(Duration::from_secs(2 * 86_400 + 3600))
        );
        assert_eq!(parse_etime(""), None);
        assert_eq!(parse_etime("x"), None);
    }

    #[test]
    fn sessions_round_trip_and_keep_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let text = r#"{"v":1,"platform":"web","pid":123,"url":"http://127.0.0.1:8787/","ports":{"http":8787},
            "device":{"kind":"simulator","udid":"ABC","name":"icm-iphone-17-ios-27.0","managed":true}}"#;
        std::fs::write(dir.path().join("web.json"), text).unwrap();
        let sessions = list(dir.path());
        assert_eq!(sessions.len(), 1);
        let session = sessions[0].1.as_ref().unwrap();
        assert_eq!(session.platform, "web");
        assert_eq!(session.all_pids(), vec![123]);
        assert_eq!(session.device.as_ref().unwrap().id, "ABC");
        assert_eq!(session.extra["ports"]["http"], 8787);

        let written = write(dir.path(), session).unwrap();
        let again = read(&written).unwrap();
        assert_eq!(again.extra["ports"]["http"], 8787);
        assert_eq!(again.url.as_deref(), Some("http://127.0.0.1:8787/"));
    }

    #[test]
    fn our_own_processes_are_ours_and_later_ones_are_not() {
        let me = std::process::id() as i32;
        assert!(is_ours(me, Some(SystemTime::now())));
        // A file written before this process started cannot name it.
        assert!(!is_ours(
            me,
            Some(SystemTime::now() - Duration::from_secs(10 * 86_400))
        ));
        assert!(!is_ours(me, None));
        assert!(!is_ours(1, Some(SystemTime::now())));
    }
}
