//! Sessions (design §3 "Sessions", §4.6): what `icm run` leaves running,
//! recorded in `target/icm/sessions/<platform>.json` so that `icm ps`
//! lists it and `icm stop` ends it without knowing the platform.
//!
//! The file (schema `icm.session/1`; unknown keys are kept and ignored):
//!
//! ```json
//! {"v":1, "platform":"ios-sim", "run":"<run id>", "started":"2026-10-06T21:03:11Z",
//!  "pid":14879,                                   // the app or session host icm started
//!  "identity":{"start":"1791334000.123456"},      // what that process was (procid)
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
//! group when it leads one. A pid is signalled only while it still is the
//! process icm started: it has the `identity` recorded for it (the
//! process's start time, [`crate::procid`]; `identity` beside `pid`, and
//! on each entry of `pids`), or, in a record written before identities, it
//! is alive and started before the session file was last written, so a pid
//! that another process took is never touched. An `identity` that holds
//! `unavailable` (icm started the process and could not read it, and says
//! why) is neither: it matches no process, and the write-time test is not
//! used for it, so that pid is never signalled; `stop` and `ps` say so
//! (`run.identity_unavailable`). `--shutdown` also runs the `shutdown`
//! commands when the device is icm-managed (`icm-` names only).

use crate::catalogue::CheckId;
use crate::error::Check;
use crate::procid::{self, Identity, Verdict};
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
    /// What `pid` was when icm started it ([`crate::procid`]); a record
    /// from before identities has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Identity>,
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
    /// What it was when icm started it ([`crate::procid`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Identity>,
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

    /// What the record says `pid` was when icm started it, if it says.
    pub fn identity_of(&self, pid: i32) -> Option<&Identity> {
        if self.pid == Some(pid) {
            return self.identity.as_ref();
        }
        self.pids
            .iter()
            .find(|process| process.pid == pid)
            .and_then(|process| process.identity.as_ref())
    }

    /// Whether `pid`, one of the record's, is still the process icm
    /// started ([`is_ours`]); `written` is when the file was last written.
    pub fn is_ours(&self, pid: i32, written: Option<SystemTime>) -> bool {
        is_ours(pid, self.identity_of(pid), written)
    }

    /// The record's pids that a process runs under and nothing tells from
    /// another process, each with why: icm could not read the identity when
    /// it started the process ([`Identity::unavailable`]), or the OS will
    /// not describe it now. They are not ours ([`Session::is_ours`]), and
    /// are no more gone than running.
    pub fn unverified_pids(&self) -> Vec<(i32, String)> {
        self.all_pids()
            .into_iter()
            .filter_map(|pid| match procid::check(pid, self.identity_of(pid)?) {
                Verdict::Unknown(why) => Some((pid, why)),
                _ => None,
            })
            .collect()
    }
}

/// Whether a recorded pid is still the process the session started. With
/// the `identity` recorded for it, the pid must still have it; an identity
/// icm could not read when it started the process ([`Identity::unavailable`])
/// matches no process, so such a pid is never ours. Without one (a record
/// from before identities) it must be alive and have started no later than
/// the session file was last written, which a pid that another process
/// took, or one reused after a reboot, does not satisfy.
pub fn is_ours(pid: i32, identity: Option<&Identity>, written: Option<SystemTime>) -> bool {
    if pid <= 1 {
        return false;
    }
    if let Some(identity) = identity {
        return procid::check(pid, identity) == Verdict::Same;
    }
    if !crate::signals::alive(pid) {
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

/// The WARN for a process icm has just started whose identity it could not
/// read ([`Identity::unavailable`], from [`procid::capture`]) while a
/// process still runs under its pid: later commands cannot tell it from
/// another process, so they will not signal it. `what` names the process
/// (`the desktop app`). `None` for an identity that was read, a record
/// without one, and a process that has already ended.
pub fn unavailable_check(what: &str, pid: i32, identity: Option<&Identity>) -> Option<Check> {
    let identity = identity?;
    let reason = identity.unavailable.as_deref()?;
    if procid::check(pid, identity) == Verdict::Gone {
        return None;
    }
    Some(
        Check::warn(
            CheckId::RunIdentityUnavailable,
            format!(
                "icm could not read the identity of {what} (pid {pid}) when it started it ({reason}), so later commands cannot tell whether that pid is still {what}, and they will not signal it"
            ),
        )
        .fix(
            format!("When you are done with {what}, check what runs under the pid, and stop it yourself if it is {what}."),
            &[&format!("ps -p {pid} -o pid,lstart,command")],
        ),
    )
}

/// The WARN for a process found under a recorded pid that nothing tells
/// from another process (`stop`, a run that replaces a session, `logs`):
/// `why` is [`Verdict::Unknown`]'s, either that the record's identity is
/// unavailable or that the OS will not describe the process now. It is
/// treated as not running and never signalled.
pub fn unverified_check(what: &str, pid: i32, why: &str) -> Check {
    Check::warn(
        CheckId::RunIdentityUnavailable,
        format!(
            "a process runs under pid {pid} and icm cannot tell whether it is {what} ({why}), so it counts as not running and is never signalled"
        ),
    )
    .fix(
        format!("Check what runs under the pid, and stop it yourself if it is {what}."),
        &[&format!("ps -p {pid} -o pid,lstart,command")],
    )
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
        // A record from before identities: alive and started before the
        // file was written.
        assert!(is_ours(me, None, Some(SystemTime::now())));
        // A file written before this process started cannot name it.
        assert!(!is_ours(
            me,
            None,
            Some(SystemTime::now() - Duration::from_secs(10 * 86_400))
        ));
        assert!(!is_ours(me, None, None));
        assert!(!is_ours(1, None, Some(SystemTime::now())));
    }

    /// With an identity the pid must still have it, whenever the file was
    /// written: another process's pid is not ours, and ours is, however
    /// old the file is.
    #[test]
    fn a_recorded_identity_decides_which_process_a_pid_is() {
        let me = std::process::id() as i32;
        let mine = procid::of(me).unwrap();
        let other = Identity {
            start: "1791334000.000001".to_string(),
            exe: String::new(),
            unavailable: None,
        };
        let old = Some(SystemTime::now() - Duration::from_secs(10 * 86_400));
        assert!(is_ours(me, Some(&mine), old));
        assert!(is_ours(me, Some(&mine), None));
        assert!(!is_ours(me, Some(&other), Some(SystemTime::now())));
        assert!(!is_ours(1, Some(&mine), Some(SystemTime::now())));

        // The record's own pid and the pids it lists carry their own.
        let session = Session {
            pid: Some(me),
            identity: Some(other.clone()),
            pids: vec![SessionProcess {
                pid: me + 1,
                what: "chrome".to_string(),
                identity: Some(mine.clone()),
            }],
            ..Session::default()
        };
        assert_eq!(session.identity_of(me), Some(&other));
        assert_eq!(session.identity_of(me + 1), Some(&mine));
        assert_eq!(session.identity_of(me + 2), None);
        assert!(!session.is_ours(me, Some(SystemTime::now())));
        let text = serde_json::to_string(&session).unwrap();
        let back: Session = serde_json::from_str(&text).unwrap();
        assert_eq!(back.identity_of(me), Some(&other));
        assert_eq!(back.pids[0].identity.as_ref(), Some(&mine));
    }

    /// A pid whose identity icm could not read when it started the process
    /// is not judged as a record from before identities is: that one is
    /// ours while it is alive and the file is no older than the process,
    /// which can fool a rewrite after a pid was reused; this one is never
    /// ours, however recent the file, and the record says why.
    #[test]
    fn an_identity_icm_could_not_read_is_never_ours() {
        let me = std::process::id() as i32;
        let unread = Identity::unavailable("proc_pidinfo: Operation not permitted");
        let now = Some(SystemTime::now());
        assert!(is_ours(me, None, now));
        assert!(!is_ours(me, Some(&unread), now));
        assert!(!is_ours(me, Some(&unread), None));

        let spawn = || {
            std::process::Command::new("sleep")
                .arg("60")
                .spawn()
                .unwrap()
        };
        let (mut first, mut second) = (spawn(), spawn());
        let (one, two) = (first.id() as i32, second.id() as i32);
        let session = Session {
            pid: Some(one),
            identity: Some(unread.clone()),
            pids: vec![SessionProcess {
                pid: two,
                what: "server".to_string(),
                // Nothing recorded at all: an older icm's record.
                identity: None,
            }],
            ..Session::default()
        };
        assert!(!session.is_ours(one, now));
        // A process runs under the pid nothing can tell from another: it
        // is listed as unverified, neither ours nor gone. The older record
        // has none to be unverified by.
        assert_eq!(
            session
                .unverified_pids()
                .into_iter()
                .map(|(pid, why)| (pid, why.contains("Operation not permitted")))
                .collect::<Vec<_>>(),
            [(one, true)]
        );
        assert_eq!(session.identity_of(two), None);
        let text = serde_json::to_string(&session).unwrap();
        let back: Session = serde_json::from_str(&text).unwrap();
        assert_eq!(back.identity, Some(unread));
        assert!(
            text.contains(r#""unavailable":"proc_pidinfo: Operation not permitted""#),
            "{text}"
        );

        // Once nothing runs under the pid, it is not unverified either.
        first.kill().unwrap();
        let _ = first.wait().unwrap();
        assert!(session.unverified_pids().is_empty());
        second.kill().unwrap();
        let _ = second.wait().unwrap();
    }

    /// The WARN names the pid and why; it is for a process that still runs
    /// under the pid, not for one that has ended, an identity that was
    /// read, or a record without any.
    #[test]
    fn the_warning_is_for_a_running_process_icm_could_not_read() {
        let me = std::process::id() as i32;
        let unread = Identity::unavailable("proc_pidinfo: Operation not permitted");
        let check = unavailable_check("the desktop app", me, Some(&unread)).unwrap();
        assert_eq!(check.id(), "run.identity_unavailable");
        let text = serde_json::to_string(&check.to_event()).unwrap();
        assert!(text.contains(&format!("pid {me}")), "{text}");
        assert!(text.contains("Operation not permitted"), "{text}");
        assert!(text.contains("will not signal"), "{text}");
        assert!(unavailable_check("the app", me, procid::of(me).as_ref()).is_none());
        assert!(unavailable_check("the app", me, None).is_none());
        // Nothing runs under the pid: nothing to warn about.
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        assert!(unavailable_check("the app", pid, Some(&unread)).is_some());
        child.kill().unwrap();
        let _ = child.wait().unwrap();
        assert!(unavailable_check("the app", pid, Some(&unread)).is_none());

        let check = unverified_check("the desktop app", me, "the OS would not say");
        let text = serde_json::to_string(&check.to_event()).unwrap();
        assert!(text.contains("the OS would not say"), "{text}");
        assert!(text.contains("never signalled"), "{text}");
    }
}
