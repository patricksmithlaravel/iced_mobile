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
//! process.
//!
//! What a record holds for a process is one of three things, and they are
//! not the same:
//!
//! - an identity with a start time, which [`check`] compares;
//! - an [`Identity::unavailable`]: icm started the process and could not
//!   read it ([`capture`]: the process had already exited, or the OS would
//!   not describe it), and the record says so, with the reason. It
//!   verifies nothing: [`check`] never answers `Same` for it, and callers
//!   treat the pid as not ours, never signal it and report that they
//!   cannot tell it from another process;
//! - none, in a record an older icm wrote before identities existed. It
//!   verifies nothing either, and the few callers that still judge such a
//!   record by the pid (a command line, an executable, when the file was
//!   written) do so for this case only. A record that holds an
//!   unavailable identity is never judged that way: icm wrote it knowing
//!   about identities, and failing to read one is no reason to trust a
//!   pid it knows nothing about.
//!
//! The start time is the identity; the program is not compared, because a
//! launcher may exec another program after icm read it (the Android
//! emulator's launcher execs qemu, `xcrun` execs `simctl`), and the start
//! time survives an exec. So `Same` says that the process is the one icm
//! started, not that it still runs the program icm started or has the
//! environment it started with.

use serde::{Deserialize, Serialize};

/// What tells a process from every other that has had its pid, or why icm
/// could not read it ([`Identity::unavailable`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// When the process started, as an opaque token of the OS's own
    /// counter: equal for every read of one process, different for any
    /// process that has the pid later. Empty for an
    /// [`Identity::unavailable`], which matches nothing.
    #[serde(default)]
    pub start: String,
    /// The program it ran when icm read it (a path, or empty when the OS
    /// would not say). For messages; not compared.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub exe: String,
    /// Why icm could not read the identity when it started the process,
    /// when it could not: the record then holds this in place of a start
    /// time, which is not the same as holding no identity at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

impl Identity {
    /// What a record holds for a process icm started and could not read
    /// ([`capture`]), with the reason.
    pub fn unavailable(reason: impl Into<String>) -> Identity {
        Identity {
            start: String::new(),
            exe: String::new(),
            unavailable: Some(reason.into()),
        }
    }

    /// Whether this can tell the process from another: false for an
    /// [`Identity::unavailable`], and for one that holds no start time.
    pub fn is_verifiable(&self) -> bool {
        self.unavailable.is_none() && !self.start.is_empty()
    }

    /// Why this verifies nothing, when it does not
    /// ([`Identity::is_verifiable`]).
    pub fn why_unverifiable(&self) -> Option<String> {
        match (&self.unavailable, self.start.is_empty()) {
            (Some(reason), _) => Some(format!(
                "icm could not read the process's identity when it started it ({reason})"
            )),
            (None, true) => Some("the record holds no start time for the process".to_string()),
            (None, false) => None,
        }
    }
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
    /// Neither the same nor another: the OS would not say (the process
    /// belongs to another user, or this OS has no reader), or the record
    /// has no identity that could tell (why).
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
/// describes it.
pub fn of(pid: i32) -> Option<Identity> {
    match probe(pid) {
        Probe::Found(identity) => Some(identity),
        Probe::Missing | Probe::Unreadable(_) => None,
    }
}

/// What to record beside the pid of a process icm has just started: its
/// identity or, when that cannot be read (the process has already exited,
/// or the OS would not describe it), an [`Identity::unavailable`] with the
/// reason, so the record says that icm tried.
pub fn capture(pid: i32) -> Identity {
    // `ICM_FAKE_IDENTITY_UNREADABLE=<why>`: icm's own tests make this read
    // fail, as it does when the OS will not describe a process, to see
    // what each command that starts a process records and says. Only here:
    // [`check`] and [`of`] read as ever, so a later command from a shell
    // without it sees the record as a real failure left it.
    if let Some(why) = std::env::var("ICM_FAKE_IDENTITY_UNREADABLE")
        .ok()
        .filter(|why| !why.is_empty())
    {
        return Identity::unavailable(why);
    }
    match probe(pid) {
        Probe::Found(identity) => identity,
        Probe::Missing => Identity::unavailable(if pid <= 1 {
            format!("{pid} is not the pid of a process icm starts")
        } else {
            "no process had the pid when icm read it; it had already exited".to_string()
        }),
        Probe::Unreadable(why) => Identity::unavailable(why),
    }
}

/// Whether `pid` is still the process `recorded` describes. A `recorded`
/// that is not verifiable ([`Identity::is_verifiable`]) is never `Same`:
/// the pid is `Gone` when no process has it, and otherwise `Unknown`, with
/// why nothing tells.
pub fn check(pid: i32, recorded: &Identity) -> Verdict {
    if let Some(why) = recorded.why_unverifiable() {
        return match probe(pid) {
            Probe::Missing => Verdict::Gone,
            Probe::Found(_) | Probe::Unreadable(_) => Verdict::Unknown(why),
        };
    }
    match probe(pid) {
        Probe::Missing => Verdict::Gone,
        Probe::Found(now) if now.start == recorded.start => Verdict::Same,
        Probe::Found(now) => Verdict::Other(now),
        Probe::Unreadable(why) => Verdict::Unknown(why),
    }
}

/// Whether `pid` is the process `recorded` describes: false for a record
/// without an identity, and for one whose identity is unavailable, since
/// nothing then says which process it was.
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
        unavailable: None,
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
        unavailable: None,
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
            unavailable: None,
        };
        let text = serde_json::to_string(&identity).unwrap();
        assert_eq!(serde_json::from_str::<Identity>(&text).unwrap(), identity);
        let bare = serde_json::to_string(&Identity {
            start: "7".to_string(),
            ..Identity::default()
        })
        .unwrap();
        assert_eq!(bare, r#"{"start":"7"}"#);
        assert_eq!(serde_json::from_str::<Identity>(&bare).unwrap().exe, "");
    }

    /// A process icm could not read is recorded as such, which is not the
    /// absence of an identity: the record round-trips with the reason, and
    /// it verifies nothing.
    #[test]
    fn an_identity_that_could_not_be_read_is_recorded_with_its_reason() {
        let unavailable = Identity::unavailable("proc_pidinfo: Operation not permitted");
        assert!(!unavailable.is_verifiable());
        let text = serde_json::to_string(&unavailable).unwrap();
        assert_eq!(
            text,
            r#"{"start":"","unavailable":"proc_pidinfo: Operation not permitted"}"#
        );
        let back: Identity = serde_json::from_str(&text).unwrap();
        assert_eq!(back, unavailable);
        assert!(
            back.why_unverifiable()
                .unwrap()
                .contains("Operation not permitted")
        );
        // Written without the empty start, as a person might.
        let by_hand: Identity = serde_json::from_str(r#"{"unavailable":"x"}"#).unwrap();
        assert!(!by_hand.is_verifiable());
        // An identity with a start time is verifiable, an empty one is not.
        assert!(
            Identity {
                start: "7".into(),
                ..Identity::default()
            }
            .is_verifiable()
        );
        assert!(!Identity::default().is_verifiable());
        assert!(Identity::default().why_unverifiable().is_some());
    }

    /// Whatever process has the pid, an identity that could not be read is
    /// never the same one: a pid that nothing runs under is gone, and any
    /// other cannot be told, with why. A caller that treats only `Same` as
    /// its process never signals it, as it does not for a record with no
    /// identity.
    #[test]
    fn an_unavailable_identity_is_never_the_same_process() {
        let mut child = sleeper();
        let pid = child.id() as i32;
        let unavailable = Identity::unavailable("the OS would not say");
        match check(pid, &unavailable) {
            Verdict::Unknown(why) => assert!(why.contains("the OS would not say"), "{why}"),
            other => panic!("a running process read as {other:?}"),
        }
        assert!(!same(pid, Some(&unavailable)));
        // Nor is an identity that holds no start time.
        assert!(!same(pid, Some(&Identity::default())));
        assert!(same(pid, of(pid).as_ref()));

        child.kill().unwrap();
        let _ = child.wait().unwrap();
        assert_eq!(check(pid, &unavailable), Verdict::Gone);
        assert!(!same(pid, Some(&unavailable)));
    }

    /// `capture` is what a launcher records: the identity when the process
    /// can be read, the reason when it cannot.
    #[test]
    fn capture_records_an_identity_or_why_there_is_none() {
        let mut child = sleeper();
        let pid = child.id() as i32;
        let captured = capture(pid);
        assert!(captured.is_verifiable(), "{captured:?}");
        assert_eq!(Some(captured.clone()), of(pid));
        assert_eq!(check(pid, &captured), Verdict::Same);

        child.kill().unwrap();
        let _ = child.wait().unwrap();
        let gone = capture(pid);
        assert!(!gone.is_verifiable());
        assert!(
            gone.unavailable
                .as_deref()
                .is_some_and(|why| why.contains("exited")),
            "{gone:?}"
        );
        // Neither init nor a process group is a process icm starts.
        for pid in [0, 1, -1] {
            let identity = capture(pid);
            assert!(!identity.is_verifiable(), "{pid}: {identity:?}");
            assert!(identity.unavailable.is_some());
        }
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
