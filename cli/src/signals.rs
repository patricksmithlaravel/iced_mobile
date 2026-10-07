//! SIGINT, SIGTERM and SIGHUP: stop every child process group and still
//! write a result (Appendix C item 24).
//!
//! The handler only records the signal. The process runner notices it
//! between polls, kills its child's group and returns; the command then
//! fails with `run.interrupted` and the reporter writes the result. A
//! watchdog thread (started by `lib::main`) is the backstop when the main
//! thread is busy elsewhere: it kills the registered groups and, after a
//! grace period, writes the result itself and exits 130.

use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};

static PENDING: AtomicI32 = AtomicI32::new(0);
static GROUPS: Mutex<Vec<i32>> = Mutex::new(Vec::new());

extern "C" fn on_signal(signal: libc::c_int) {
    PENDING.store(signal, Ordering::SeqCst);
}

/// Installs the handlers for SIGINT, SIGTERM and SIGHUP.
pub fn install() {
    let handler = on_signal as extern "C" fn(libc::c_int);
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: the handler only performs an atomic store, which is
        // async-signal-safe.
        unsafe {
            let _ = libc::signal(signal, handler as libc::sighandler_t);
        }
    }
}

/// The signal received, if any.
pub fn pending() -> Option<i32> {
    match PENDING.load(Ordering::SeqCst) {
        0 => None,
        signal => Some(signal),
    }
}

/// The signal's name, e.g. `SIGTERM`.
pub fn name(signal: i32) -> String {
    match signal {
        libc::SIGINT => "SIGINT".to_string(),
        libc::SIGTERM => "SIGTERM".to_string(),
        libc::SIGHUP => "SIGHUP".to_string(),
        libc::SIGKILL => "SIGKILL".to_string(),
        libc::SIGQUIT => "SIGQUIT".to_string(),
        libc::SIGABRT => "SIGABRT".to_string(),
        libc::SIGSEGV => "SIGSEGV".to_string(),
        libc::SIGBUS => "SIGBUS".to_string(),
        libc::SIGPIPE => "SIGPIPE".to_string(),
        other => format!("signal {other}"),
    }
}

/// Registers a child's process group, so the watchdog can stop it.
pub fn register_group(pgid: i32) {
    if let Ok(mut groups) = GROUPS.lock() {
        groups.push(pgid);
    }
}

/// Forgets a child's process group.
pub fn unregister_group(pgid: i32) {
    if let Ok(mut groups) = GROUPS.lock() {
        groups.retain(|group| *group != pgid);
    }
}

/// Sends `signal` to every registered process group.
pub fn kill_registered(signal: i32) {
    let groups = GROUPS
        .lock()
        .map(|groups| groups.clone())
        .unwrap_or_default();
    for pgid in groups {
        kill_group(pgid, signal);
    }
}

/// Sends `signal` to a process group. A group that is gone is not an error.
pub fn kill_group(pgid: i32, signal: i32) {
    if pgid > 1 {
        // SAFETY: kill(2) with a negative pid targets a process group; it
        // has no memory-safety preconditions.
        unsafe {
            let _ = libc::kill(-pgid, signal);
        }
    }
}

/// Whether a process exists (it may be a zombie).
pub fn alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 only checks for existence and permission.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}
