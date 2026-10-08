//! Process identity: whether a pid still names the process icm recorded.
//!
//! A pid names a process only while that process runs. Once it has exited
//! the number goes to whatever starts next, so a record that says "icm
//! started pid N" proves nothing later: `kill(N, 0)` succeeding means that
//! some process has the number, not that it is the one icm started.
//! Anything icm does to a recorded process (a signal, or a decision about a
//! device because "its process still runs") therefore needs more than the
//! pid.
//!
//! That is an [`Identity`], taken when icm starts the process and kept in
//! the record beside the pid: the kernel's start time of the process, which
//! no later process with the same pid can share (microseconds since the
//! epoch on macOS; clock ticks since boot, with the boot's id, on Linux),
//! and the program it ran then, for the messages. [`check`] reads the pid
//! again and says whether it is [`Verdict::Same`], gone, another process,
//! or cannot be read. Only `Same` lets a caller treat the pid as its
//! process; a record that holds no identity (an older icm wrote it)
//! cannot be verified, which callers treat as not ours.
//!
//! The start time is the identity; the program is not compared, because a
//! launcher may exec another program after icm read it (the Android
//! emulator's launcher execs qemu, `xcrun` execs `simctl`), and the start
//! time survives an exec.

use serde::{Deserialize, Serialize};

/// What tells a process from every other that has had its pid.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// When the process started, as an opaque token of the OS's own
    /// counter: equal for every read of one process, different for any
    /// process that has the pid later.
    pub start: String,
    /// The program it ran when icm read it (a path, or empty when the OS
    /// would not say). For messages; not compared.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub exe: String,
}

/// What a pid is now, against a recorded [`Identity`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The process that was recorded: it started when the identity says,
    /// and it has not exited.
    Same,
    /// No process has the pid (or the one that had it has exited and waits
    /// to be reaped).
    Gone,
    /// Another process has the pid now.
    Other(Identity),
    /// The OS would not say (the process belongs to another user, or this
    /// OS has no reader): neither the same nor another.
    Unknown(String),
}

/// What reading a pid found.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Probe {
    /// No process, or an exited one nobody has reaped.
    Missing,
    /// A running process.
    Found(Identity),
    /// A process the OS would not describe (why).
    Unreadable(String),
}

/// The identity of the process that has `pid` now, when it runs and the OS
/// describes it. Taken right after icm starts a process, to record beside
/// its pid.
pub fn of(pid: i32) -> Option<Identity> {
    match probe(pid) {
        Probe::Found(identity) => Some(identity),
        Probe::Missing | Probe::Unreadable(_) => None,
    }
}

/// Whether `pid` is still the process `recorded` describes.
pub fn check(pid: i32, recorded: &Identity) -> Verdict {
    match probe(pid) {
        Probe::Missing => Verdict::Gone,
        Probe::Found(now) if now.start == recorded.start => Verdict::Same,
        Probe::Found(now) => Verdict::Other(now),
        Probe::Unreadable(why) => Verdict::Unknown(why),
    }
}

/// Whether `pid` is the process `recorded` describes: false for a record
/// without an identity, since nothing then says which process it was.
pub fn same(pid: i32, recorded: Option<&Identity>) -> bool {
    recorded.is_some_and(|recorded| check(pid, recorded) == Verdict::Same)
}

fn probe(pid: i32) -> Probe {
    if pid <= 1 {
        // Neither init nor a process group's pid 0 is ever a process icm
        // started.
        return Probe::Missing;
    }
    probe_os(pid)
}

#[cfg(target_os = "macos")]
fn probe_os(pid: i32) -> Probe {
    // SAFETY: proc_bsdinfo is plain old data that proc_pidinfo fills in.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: the buffer is `size` bytes of a local the call may write.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast::<libc::c_void>(),
            size,
        )
    };
    if read != size {
        let error = std::io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::ESRCH) => Probe::Missing,
            _ if !crate::signals::alive(pid) => Probe::Missing,
            _ => Probe::Unreadable(format!("proc_pidinfo: {error}")),
        };
    }
    // SZOMB: it has exited and its parent has not reaped it.
    if info.pbi_status == 5 {
        return Probe::Missing;
    }
    let mut path = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is as long as the size passed.
    let length = unsafe {
        libc::proc_pidpath(
            pid,
            path.as_mut_ptr().cast::<libc::c_void>(),
            path.len() as u32,
        )
    };
    let exe = usize::try_from(length)
        .ok()
        .filter(|length| *length > 0)
        .map(|length| String::from_utf8_lossy(&path[..length.min(path.len())]).into_owned())
        .unwrap_or_default();
    Probe::Found(Identity {
        start: format!("{}.{:06}", info.pbi_start_tvsec, info.pbi_start_tvusec),
        exe,
    })
}

#[cfg(target_os = "linux")]
fn probe_os(pid: i32) -> Probe {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(_) if !crate::signals::alive(pid) => return Probe::Missing,
        Err(error) => return Probe::Unreadable(format!("/proc/{pid}/stat: {error}")),
    };
    let Some((state, ticks)) = parse_proc_stat(&stat) else {
        return Probe::Unreadable(format!("/proc/{pid}/stat is not in the format of proc(5)"));
    };
    if state == 'Z' || state == 'X' {
        return Probe::Missing;
    }
    // Ticks count from boot, so the boot's id keeps two boots apart.
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|id| id.trim().to_string())
        .unwrap_or_default();
    let exe = std::fs::read_link(format!("/proc/{pid}/exe"))
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    Probe::Found(Identity {
        start: format!("{boot}:{ticks}"),
        exe,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn probe_os(pid: i32) -> Probe {
    if crate::signals::alive(pid) {
        Probe::Unreadable("process start times are not read on this OS".to_string())
    } else {
        Probe::Missing
    }
}

/// The state letter and start time (clock ticks since boot) in a
/// `/proc/<pid>/stat` line: `pid (comm) S ppid ... starttime ...`. The
/// command name may hold spaces and parentheses, so fields count from its
/// last closing parenthesis.
#[cfg(any(target_os = "linux", test))]
fn parse_proc_stat(stat: &str) -> Option<(char, u64)> {
    let after = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = after.split_whitespace().collect();
    // Field 3 is the state; starttime is field 22.
    let state = fields.first()?.chars().next()?;
    let ticks = fields.get(19)?.parse().ok()?;
    Some((state, ticks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    fn sleeper() -> std::process::Child {
        Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[test]
    fn a_process_is_the_same_until_it_exits() {
        let mut child = sleeper();
        let pid = child.id() as i32;
        let identity = of(pid).expect("a running process has an identity");
        assert!(!identity.start.is_empty());
        assert_eq!(check(pid, &identity), Verdict::Same);
        assert!(same(pid, Some(&identity)));
        // Reading again gives the same token.
        assert_eq!(of(pid).unwrap().start, identity.start);

        child.kill().unwrap();
        // Exited, not yet reaped: a zombie is no running process.
        let until = Instant::now() + Duration::from_secs(5);
        while check(pid, &identity) == Verdict::Same {
            assert!(Instant::now() < until, "the process did not end");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(check(pid, &identity), Verdict::Gone);
        let _ = child.wait().unwrap();
        assert_eq!(check(pid, &identity), Verdict::Gone);
        assert!(!same(pid, Some(&identity)));
    }

    #[test]
    fn another_process_with_the_pid_is_not_the_recorded_one() {
        let mut child = sleeper();
        let pid = child.id() as i32;
        let mut recorded = of(pid).unwrap();
        // What an earlier process that had this pid recorded.
        recorded.start = "1.000001".to_string();
        recorded.exe = "/usr/bin/earlier".to_string();
        match check(pid, &recorded) {
            Verdict::Other(now) => assert_ne!(now.start, recorded.start),
            other => panic!("another process read as {other:?}"),
        }
        assert!(!same(pid, Some(&recorded)));
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Without an identity nothing says which process the pid was.
    #[test]
    fn a_record_without_an_identity_verifies_nothing() {
        let me = std::process::id() as i32;
        assert!(of(me).is_some());
        assert!(!same(me, None));
        assert_eq!(of(0), None);
        assert_eq!(of(1), None);
        assert!(!same(1, of(me).as_ref()));
    }

    #[test]
    fn identities_round_trip_and_older_records_have_none() {
        let identity = Identity {
            start: "1791334000.123456".to_string(),
            exe: "/sdk/emulator/emulator".to_string(),
        };
        let text = serde_json::to_string(&identity).unwrap();
        assert_eq!(serde_json::from_str::<Identity>(&text).unwrap(), identity);
        let bare = serde_json::to_string(&Identity {
            start: "7".to_string(),
            exe: String::new(),
        })
        .unwrap();
        assert_eq!(bare, r#"{"start":"7"}"#);
        assert_eq!(serde_json::from_str::<Identity>(&bare).unwrap().exe, "");
    }

    #[test]
    fn proc_stat_counts_fields_from_the_last_parenthesis() {
        // comm = "a (b) c": spaces and parentheses inside it.
        let stat = "4242 (a (b) c) S 1 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 987654 \
                    1000000 100 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";
        assert_eq!(parse_proc_stat(stat), Some(('S', 987_654)));
        assert_eq!(parse_proc_stat("4242 (x) Z 1"), None);
        assert_eq!(parse_proc_stat("garbage"), None);
    }
}
