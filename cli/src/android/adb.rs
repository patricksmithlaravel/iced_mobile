//! adb: the device list, shell commands and their parsing.
//!
//! `adb shell` joins its arguments with spaces and hands the line to the
//! device's shell, so every dynamic value is quoted here ([`quote`]).
//! icm never runs `adb kill-server`: other tools (and other agents) share
//! the server.

use super::Toolset;
use crate::catalogue::CheckId;
use crate::error::IcmError;
use crate::process::{self, Cmd, Outcome};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// How long a device that adb lists but does not answer gets before icm
/// calls it gone. adb lists an emulator offline for a moment after a heavy
/// step (the CI runner's CPU-only emulator did so right after `am kill`), and
/// such a device answers again within this wait. A run's `--timeout` clips it.
pub const ANSWER_WAIT: Duration = Duration::from_secs(30);

/// The pause between two asks of a device that does not answer yet.
pub const ANSWER_POLL: Duration = Duration::from_secs(1);

/// The longest one read of adb takes in the wait: an ask of the device's
/// clock and the closing listing. Each gets less when less of the wait (or
/// of the run's `--timeout`) is left.
const ASK_TIMEOUT: Duration = Duration::from_secs(15);

/// One line of `adb devices -l`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// The serial, e.g. `emulator-5580`.
    pub serial: String,
    /// `device`, `offline`, `unauthorized`, ...
    pub state: String,
    /// `product:`, `model:`, `device:`, `transport_id:` values.
    pub props: BTreeMap<String, String>,
}

impl Listed {
    /// Whether adb can use it.
    pub fn online(&self) -> bool {
        self.state == "device"
    }

    /// Whether it is an emulator.
    pub fn is_emulator(&self) -> bool {
        self.serial.starts_with("emulator-")
    }

    /// The console port of an emulator serial.
    pub fn port(&self) -> Option<u16> {
        self.serial.strip_prefix("emulator-")?.parse().ok()
    }
}

/// Parses `adb devices -l`.
pub fn parse_devices(text: &str) -> Vec<Listed> {
    text.lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty() && !line.starts_with("List of devices") && !line.starts_with('*')
        })
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let serial = parts.next()?.to_string();
            let state = parts.next()?.to_string();
            let props = parts
                .filter_map(|part| part.split_once(':'))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            Some(Listed {
                serial,
                state,
                props,
            })
        })
        .collect()
}

/// Quotes a value for the device's shell.
pub fn quote(value: &str) -> String {
    crate::process::shell_quote(value)
}

/// Runs a quick command, returning its outcome even when it failed; `None`
/// when it could not start. Polls use this: no step event, no log.
pub fn quick(cmd: Cmd, timeout: Duration) -> Option<Outcome> {
    process::run(&cmd.timeout(timeout), None, None).ok()
}

/// When a wait of `wait` from `now` ends, and whether the run's `--timeout`
/// (`deadline`) ends it first (then `true`).
fn wait_end(now: Instant, wait: Duration, deadline: Option<Instant>) -> (Instant, bool) {
    let until = now + wait;
    match deadline {
        Some(deadline) if deadline < until => (deadline, true),
        _ => (until, false),
    }
}

/// The instant the answer waits measure from.
#[cfg(not(test))]
fn now() -> Instant {
    Instant::now()
}

/// Pauses between two asks of a device that does not answer.
#[cfg(not(test))]
fn pause(duration: Duration) {
    std::thread::sleep(duration);
}

// Under test the waits run on a clock that only [`pause`] moves. Each
// thread's clock starts at the real instant it is first read, so a wait's
// asks and its end are exact and take no real time.
#[cfg(test)]
thread_local! {
    static CLOCK: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn now() -> Instant {
    CLOCK.with(|clock| match clock.get() {
        Some(now) => now,
        None => {
            let now = Instant::now();
            clock.set(Some(now));
            now
        }
    })
}

#[cfg(test)]
fn pause(duration: Duration) {
    let next = now() + duration;
    CLOCK.with(|clock| clock.set(Some(next)));
}

// Under test, the timeouts the wait gives its asks ("ask") and its closing
// listing ("listing"), in the order they run. A fake adb answers at once, so
// these are what the tests check the bound by.
#[cfg(test)]
thread_local! {
    static TIMEOUTS: std::cell::RefCell<Vec<(&'static str, Duration)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Notes the timeout one of the wait's reads was given (under test only).
#[cfg(test)]
fn note_timeout(read: &'static str, timeout: Duration) {
    TIMEOUTS.with(|timeouts| timeouts.borrow_mut().push((read, timeout)));
}

#[cfg(not(test))]
fn note_timeout(_read: &'static str, _timeout: Duration) {}

/// adb bound to one device.
#[derive(Clone, Debug)]
pub struct Adb {
    base: Cmd,
    /// The device's serial.
    pub serial: String,
}

impl Adb {
    /// adb for `serial`.
    pub fn new(tools: &Toolset, serial: &str) -> Result<Adb, IcmError> {
        Ok(Adb {
            base: tools.adb()?.args(["-s", serial]),
            serial: serial.to_string(),
        })
    }

    /// adb run from `program` (a test's fake adb), bound to `serial`.
    #[cfg(test)]
    pub fn from_program(program: &std::path::Path, serial: &str) -> Adb {
        Adb {
            base: Cmd::new(program).args(["-s", serial]),
            serial: serial.to_string(),
        }
    }

    /// `adb -s <serial> <args…>`.
    pub fn cmd<I, S>(&self, args: I) -> Cmd
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        self.base.clone().args(args)
    }

    /// `adb -s <serial> shell <line>`; the line is run by the device's sh.
    pub fn shell(&self, line: &str) -> Cmd {
        self.cmd(["shell", line])
    }

    /// Runs a shell line quickly and returns its trimmed stdout when it
    /// exits 0.
    pub fn shell_text(&self, line: &str, timeout: Duration) -> Option<String> {
        let outcome = quick(self.shell(line), timeout)?;
        outcome
            .success()
            .then(|| outcome.stdout_text().trim().to_string())
    }

    /// `getprop <name>`.
    pub fn getprop(&self, name: &str) -> Option<String> {
        self.shell_text(&format!("getprop {}", quote(name)), Duration::from_secs(15))
    }

    /// The app's pids (`pidof <id>`), empty when it is not running.
    pub fn pids(&self, app_id: &str) -> Vec<u32> {
        self.shell_text(&format!("pidof {}", quote(app_id)), Duration::from_secs(15))
            .map(|text| {
                text.split_whitespace()
                    .filter_map(|pid| pid.parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The device clock as `seconds.nanoseconds` (the log mark).
    pub fn epoch(&self) -> Option<String> {
        self.epoch_within(ASK_TIMEOUT)
    }

    /// [`Adb::epoch`], giving the ask at most `timeout`.
    fn epoch_within(&self, timeout: Duration) -> Option<String> {
        let text = self.shell_text("date +%s.%N", timeout)?;
        let mark = text.lines().next()?.trim().to_string();
        // toybox prints a literal %N on very old images.
        let mark = mark.replace(".%N", ".0");
        mark.parse::<f64>().ok().map(|_| mark)
    }

    /// The device clock once the device answers `date`, for the lifecycle
    /// suite's step probe and its restore. adb lists a device offline for a
    /// moment, and a single ask then would end the run as if the device were
    /// gone. An unanswered ask is repeated every [`ANSWER_POLL`] for up to
    /// [`ANSWER_WAIT`], or until the run's `--timeout` (`deadline`) when that
    /// comes first. An ask is given no more time than the wait has left (at
    /// most [`ASK_TIMEOUT`]), and no ask is made once none is left. A device
    /// still silent then is `android.device.none`.
    pub fn answering(&self, deadline: Option<Instant>) -> Result<String, IcmError> {
        let (until, clipped) = wait_end(now(), ANSWER_WAIT, deadline);
        self.answer_until(until, clipped, deadline)
    }

    /// [`Adb::answering`] with its end given. `clipped` says that the run's
    /// `--timeout` (`deadline`) ends the wait, which the error says.
    fn answer_until(
        &self,
        until: Instant,
        clipped: bool,
        deadline: Option<Instant>,
    ) -> Result<String, IcmError> {
        loop {
            let left = until.saturating_duration_since(now());
            if left.is_zero() {
                return Err(self.not_answering(clipped, deadline));
            }
            let timeout = left.min(ASK_TIMEOUT);
            note_timeout("ask", timeout);
            if let Some(mark) = self.epoch_within(timeout) {
                return Ok(mark);
            }
            if let Some(signal) = crate::signals::pending() {
                return Err(crate::output::interrupted(signal));
            }
            pause(ANSWER_POLL.min(until.saturating_duration_since(now())));
        }
    }

    /// Whether adb lists this device now, read for at most `timeout`:
    /// `Some(false)` when it lists other devices only, `None` when adb's
    /// listing cannot be read in time.
    fn listed(&self, timeout: Duration) -> Option<bool> {
        // The tools' environment (JAVA_HOME, ANDROID_HOME, PATH, ...) stays
        // with the base; its `-s <serial>` goes, since the listing is of all
        // the devices.
        let base = Cmd {
            args: Vec::new(),
            ..self.base.clone()
        };
        note_timeout("listing", timeout);
        let outcome = quick(base.args(["devices", "-l"]), timeout)?;
        outcome.success().then(|| {
            parse_devices(&outcome.stdout_text())
                .iter()
                .any(|device| device.serial == self.serial)
        })
    }

    /// The error for a device that does not answer `date` once the wait is
    /// over. The detail says what adb lists now, and the fix fits a device
    /// that is there but silent: the catalogue's default (create the managed
    /// emulator, or connect a device) does not. The listing gets no more time
    /// than the run's `--timeout` has left, and none when that is gone: the
    /// detail then says only that the device does not answer `date`.
    fn not_answering(&self, clipped: bool, deadline: Option<Instant>) -> IcmError {
        let serial = &self.serial;
        let when = if clipped {
            "before the --timeout ran out".to_string()
        } else {
            format!("within {} s", ANSWER_WAIT.as_secs())
        };
        let left = deadline.map(|deadline| deadline.saturating_duration_since(now()));
        let listing = match left {
            Some(left) if left.is_zero() => None,
            Some(left) => self.listed(left.min(ASK_TIMEOUT)),
            None => self.listed(ASK_TIMEOUT),
        };
        let detail = match listing {
            Some(true) => format!("{serial} is listed by adb but does not answer `date` {when}"),
            Some(false) => {
                format!("{serial} is not listed by adb and does not answer `date` {when}")
            }
            None => format!("{serial} does not answer `date` {when}"),
        };
        let check = format!("adb -s {serial} shell date");
        if serial.starts_with("emulator-") {
            // `--shutdown` stops only an emulator icm booted for the project
            // or a managed `icm-*` AVD; any other one is quit by hand.
            let kill = format!("adb -s {serial} emu kill");
            IcmError::new(CheckId::AndroidDeviceNone, detail).fix(
                format!(
                    "Check that the device answers `{check}`. If it does not, restart it: `icm stop android --shutdown` stops only an emulator icm booted for this project or a managed icm-* AVD, so quit any other emulator or run `{kill}`, then run `icm run android`."
                ),
                &[
                    check.as_str(),
                    kill.as_str(),
                    "icm stop android --shutdown",
                    "icm run android",
                ],
            )
        } else {
            IcmError::new(CheckId::AndroidDeviceNone, detail).fix(
                format!(
                    "Check that the device answers `{check}`. If it does not, restart it: reconnect its USB cable and allow USB debugging, then run `icm run android`."
                ),
                &[check.as_str(), "icm run android"],
            )
        }
    }

    /// The emulator's AVD name (`adb emu avd name`), for emulators.
    pub fn avd_name(&self) -> Option<String> {
        if !self.serial.starts_with("emulator-") {
            return None;
        }
        if let Some(outcome) = quick(self.cmd(["emu", "avd", "name"]), Duration::from_secs(10))
            && outcome.success()
        {
            let text = outcome.stdout_text();
            if let Some(name) = text
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && *line != "OK")
            {
                return Some(name.to_string());
            }
        }
        self.getprop("ro.boot.qemu.avd_name")
            .filter(|name| !name.is_empty())
    }
}

/// `adb devices -l`.
pub fn devices(tools: &Toolset) -> Result<Vec<Listed>, IcmError> {
    let cmd = tools.adb()?.args(["devices", "-l"]);
    match quick(cmd, Duration::from_secs(30)) {
        Some(outcome) if outcome.success() => Ok(parse_devices(&outcome.stdout_text())),
        Some(outcome) => Err(IcmError::new(
            crate::catalogue::CheckId::ToolFailed,
            format!("adb devices failed: {}", outcome.stderr_tail(4)),
        )),
        None => Err(IcmError::new(
            crate::catalogue::CheckId::EnvToolMissing,
            "adb could not be started",
        )
        .fix_commands(["icm doctor android --fix --yes"])),
    }
}

/// `am start -W` output: the status and total time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Started {
    /// `Status:` (`ok`, `timeout`, ...).
    pub status: Option<String>,
    /// `TotalTime:` in ms.
    pub total_ms: Option<u64>,
    /// An `Error:` line.
    pub error: Option<String>,
}

/// Parses `am start -W` output.
pub fn parse_am_start(text: &str) -> Started {
    let mut started = Started::default();
    for line in text.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("Status:") {
            started.status = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("TotalTime:") {
            started.total_ms = value.trim().parse().ok();
        } else if line.starts_with("Error") && started.error.is_none() {
            started.error = Some(line.to_string());
        }
    }
    started
}

/// The `INSTALL_FAILED_*` (or other `Failure [...]`) reason in adb install
/// output.
pub fn install_failure(text: &str) -> Option<String> {
    if let Some(at) = text.find("INSTALL_") {
        let reason: String = text[at..]
            .chars()
            .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
            .collect();
        return Some(reason);
    }
    text.lines()
        .find(|line| line.contains("Failure") || line.contains("failed to install"))
        .map(|line| line.trim().to_string())
}

/// The current display size in pixels (rotation applied) from
/// `dumpsys window displays` (`cur=1080x2400`), or `wm size`.
pub fn parse_display_size(dumpsys: &str, wm_size: &str) -> Option<(u32, u32)> {
    let parse = |value: &str| -> Option<(u32, u32)> {
        let (w, h) = value.split_once('x')?;
        let h: String = h.chars().take_while(char::is_ascii_digit).collect();
        Some((w.trim().parse().ok()?, h.parse().ok()?))
    };
    if let Some(cur) = dumpsys
        .split_whitespace()
        .find_map(|word| word.strip_prefix("cur="))
        && let Some(size) = parse(cur)
    {
        return Some(size);
    }
    // `wm size`: "Physical size: 1080x2400" and maybe "Override size: …".
    let pick = |prefix: &str| {
        wm_size
            .lines()
            .find_map(|line| line.trim().strip_prefix(prefix))
            .and_then(|value| parse(value.trim()))
    };
    pick("Override size:").or_else(|| pick("Physical size:"))
}

/// The density from `wm density` (override first), as a scale (dpi / 160).
pub fn parse_density(text: &str) -> Option<f64> {
    let pick = |prefix: &str| {
        text.lines()
            .find_map(|line| line.trim().strip_prefix(prefix))
            .and_then(|value| value.trim().parse::<f64>().ok())
    };
    pick("Override density:")
        .or_else(|| pick("Physical density:"))
        .map(|dpi| dpi / 160.0)
}

/// Whether `dumpsys window` says the focused window of `app_id` has
/// `FLAG_SECURE`.
pub fn focused_window_is_secure(dumpsys: &str, app_id: &str) -> bool {
    // Window blocks start with "Window #N Window{... <package>/...}:" and
    // list "fl=... SECURE ...".
    let mut in_app_window = false;
    for line in dumpsys.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Window #") {
            in_app_window = trimmed.contains(app_id);
            continue;
        }
        if in_app_window
            && (trimmed.contains("fl=") || trimmed.contains("flags="))
            && trimmed.contains("SECURE")
        {
            return true;
        }
    }
    false
}

/// What `dumpsys activity activities` says about an app's activities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Activities {
    /// The app has an activity, in any state (resumed, paused, stopped).
    /// Android removes it once it is destroyed, while the process may live
    /// on, cached.
    pub any: bool,
    /// One of them is the top resumed activity: the app is in front.
    pub top: bool,
}

/// Reads `dumpsys activity activities` for `app_id`: its activities are
/// `ActivityRecord{<hash> u0 <app_id>/<class> t<task>}` entries of a task's
/// history (`* Hist #0:`, `Run #0:` before API 29, `* ActivityRecord{` in
/// the window hierarchy), and the one in front is on the
/// `topResumedActivity=` (`ResumedActivity:` before API 29) line. Other
/// lines can name an activity Android has destroyed: the display's
/// `RotationHistory` keeps the activity that asked for each rotation as its
/// `source=`, so only those lines count. `None` when the text lists no
/// activity at all: a device whose launcher runs always lists one, so such
/// an answer (a device still booting, a `dumpsys` that failed) says nothing
/// about the app.
pub fn parse_activities(dumpsys: &str, app_id: &str) -> Option<Activities> {
    if !dumpsys.contains("ActivityRecord{") {
        return None;
    }
    let component = format!(" {app_id}/");
    let mut activities = Activities {
        any: false,
        top: false,
    };
    for line in dumpsys.lines() {
        if !line.contains("ActivityRecord{") || !line.contains(&component) {
            continue;
        }
        let line = line.trim_start();
        let top = line.starts_with("topResumedActivity=") || line.starts_with("ResumedActivity:");
        let listed = line.starts_with("* Hist ")
            || line.starts_with("* ActivityRecord{")
            || line.starts_with("Run #");
        activities.any |= top || listed;
        activities.top |= top;
    }
    Some(activities)
}

/// The text form `adb shell input text` accepts: spaces as `%s`, quoted
/// for the device shell. (`input text` cannot type a literal `%s`, and
/// only ASCII.)
pub fn input_text_arg(text: &str) -> String {
    quote(&text.replace(' ', "%s"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_lists() {
        let text = "List of devices attached\n\
            emulator-5580          device product:sdk_gphone64_arm64 model:sdk_gphone64_arm64 device:emu64a transport_id:3\n\
            R58M123                unauthorized usb:1-1 transport_id:4\n\
            emulator-5582 offline\n\n";
        let devices = parse_devices(text);
        assert_eq!(devices.len(), 3);
        assert!(devices[0].online() && devices[0].is_emulator());
        assert_eq!(devices[0].port(), Some(5580));
        assert_eq!(devices[0].props["model"], "sdk_gphone64_arm64");
        assert_eq!(devices[1].state, "unauthorized");
        assert!(!devices[1].is_emulator());
        assert!(!devices[2].online());
        assert!(
            parse_devices("* daemon started successfully\nList of devices attached\n").is_empty()
        );
    }

    #[test]
    fn am_start_output() {
        let ok = "Starting: Intent { cmp=com.example.app/android.app.NativeActivity }\nStatus: ok\nLaunchState: COLD\nActivity: com.example.app/android.app.NativeActivity\nTotalTime: 812\nWaitTime: 815\nComplete\n";
        let started = parse_am_start(ok);
        assert_eq!(started.status.as_deref(), Some("ok"));
        assert_eq!(started.total_ms, Some(812));
        assert!(started.error.is_none());

        let error = parse_am_start(
            "Starting: Intent { cmp=x/y }\nError type 3\nError: Activity class {x/y} does not exist.\n",
        );
        assert_eq!(error.error.as_deref(), Some("Error type 3"));
    }

    #[test]
    fn install_failures() {
        assert_eq!(
            install_failure("Performing Streamed Install\nadb: failed to install app.apk: Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE: Package com.example.app signatures do not match]").as_deref(),
            Some("INSTALL_FAILED_UPDATE_INCOMPATIBLE")
        );
        assert_eq!(install_failure("Success\n"), None);
    }

    #[test]
    fn display_metrics() {
        let dumpsys = "Display: mDisplayId=0\n  init=1080x2400 420dpi base=1080x2400 cur=2400x1080 app=2400x1017 rng=1080x1017-2400x2337\n";
        assert_eq!(parse_display_size(dumpsys, ""), Some((2400, 1080)));
        assert_eq!(
            parse_display_size("", "Physical size: 1080x2424\n"),
            Some((1080, 2424))
        );
        assert_eq!(
            parse_display_size("", "Physical size: 1080x2424\nOverride size: 720x1600\n"),
            Some((720, 1600))
        );
        assert_eq!(parse_density("Physical density: 420\n"), Some(2.625));
        assert_eq!(
            parse_density("Physical density: 420\nOverride density: 320\n"),
            Some(2.0)
        );
    }

    #[test]
    fn secure_windows() {
        let dumpsys = "  Window #3 Window{abc u0 com.example.app/android.app.NativeActivity}:\n    mAttrs={(0,0)(fillxfill) ty=BASE_APPLICATION fl=LAYOUT_IN_SCREEN SECURE HARDWARE_ACCELERATED}\n  Window #4 Window{def u0 StatusBar}:\n    mAttrs={fl=NOT_FOCUSABLE}\n";
        assert!(focused_window_is_secure(dumpsys, "com.example.app"));
        assert!(!focused_window_is_secure(dumpsys, "com.other"));
    }

    #[test]
    fn activities_of_an_app() {
        // API 36: the app in front, then after Home, then after Back (the
        // activity destroyed, the process cached).
        let front = "  * Task{89fff12 #8 type=standard A=10213:com.example.fixer U=0 visible=true}\n    topResumedActivity=ActivityRecord{253882921 u0 com.example.fixer/android.app.NativeActivity t8}\n    * Hist  #0: ActivityRecord{253882921 u0 com.example.fixer/android.app.NativeActivity t8}\n      mLastPausedActivity: ActivityRecord{266162751 u0 com.google.android.apps.nexuslauncher/.NexusLauncherActivity t6}\n  ResumedActivity: ActivityRecord{253882921 u0 com.example.fixer/android.app.NativeActivity t8}\n";
        let home = "      topResumedActivity=ActivityRecord{266162751 u0 com.google.android.apps.nexuslauncher/.NexusLauncherActivity t6}\n  * Task{89fff12 #8 type=standard A=10213:com.example.fixer U=0 visible=false}\n    mLastPausedActivity: ActivityRecord{253882921 u0 com.example.fixer/android.app.NativeActivity t8}\n    * Hist  #0: ActivityRecord{253882921 u0 com.example.fixer/android.app.NativeActivity t8}\n  ResumedActivity: ActivityRecord{266162751 u0 com.google.android.apps.nexuslauncher/.NexusLauncherActivity t6}\n";
        let back = "      topResumedActivity=ActivityRecord{266162751 u0 com.google.android.apps.nexuslauncher/.NexusLauncherActivity t6}\n  ResumedActivity: ActivityRecord{266162751 u0 com.google.android.apps.nexuslauncher/.NexusLauncherActivity t6}\n";
        let app = "com.example.fixer";
        assert_eq!(
            parse_activities(front, app),
            Some(Activities {
                any: true,
                top: true
            })
        );
        assert_eq!(
            parse_activities(home, app),
            Some(Activities {
                any: true,
                top: false
            })
        );
        assert_eq!(
            parse_activities(back, app),
            Some(Activities {
                any: false,
                top: false
            })
        );
        // API 36, after Back from an app that turned the screen: the
        // display's rotation history still names the destroyed activity.
        let rotated = format!(
            "{back}    RotationHistory\n      10-07 13:09:29.760 ROTATION_90 to ROTATION_270\n        source=ActivityRecord{{36924985 u0 com.example.fixer/android.app.NativeActivity t12}} SCREEN_ORIENTATION_UNSPECIFIED\n        mode=USER_ROTATION_LOCKED user=ROTATION_270 sensor=ROTATION_0\n"
        );
        assert_eq!(
            parse_activities(&rotated, app),
            Some(Activities {
                any: false,
                top: false
            })
        );
        // Before API 29: `Run #` and `mResumedActivity`.
        let old = "  Running activities (most recent first):\n    TaskRecord{8f1b2c4 #12 A=com.example.fixer U=0 StackId=3 sz=1}\n      Run #0: ActivityRecord{2d1e5a1 u0 com.example.fixer/android.app.NativeActivity t12}\n    mResumedActivity: ActivityRecord{2d1e5a1 u0 com.example.fixer/android.app.NativeActivity t12}\n  ResumedActivity: ActivityRecord{2d1e5a1 u0 com.example.fixer/android.app.NativeActivity t12}\n";
        assert_eq!(
            parse_activities(old, app),
            Some(Activities {
                any: true,
                top: true
            })
        );
        // An id that another one starts with is not that one.
        assert!(!parse_activities(front, "com.example.fix").unwrap().any);
        assert!(!parse_activities(front, "example.fixer").unwrap().any);
        // An answer that lists no activity at all says nothing.
        assert_eq!(parse_activities("", app), None);
        assert_eq!(
            parse_activities("Can't find service: activity\n", app),
            None
        );
    }

    #[test]
    fn text_input_is_quoted() {
        assert_eq!(input_text_arg("hello"), "hello");
        assert_eq!(input_text_arg("hello world"), "hello%sworld");
        assert_eq!(input_text_arg("it's"), "'it'\\''s'");
        assert_eq!(input_text_arg("a&b"), "'a&b'");
    }

    /// The listing the fake adb gives its emulator: adb lists it offline.
    const LISTED: Option<&str> = Some("emulator-5580 offline");

    /// A device adb lists offline for a moment answers the ask after the
    /// moment, and the wait takes the two seconds of its two unanswered asks.
    #[test]
    fn a_device_listed_offline_for_a_moment_answers_again() {
        let dir = tempfile::tempdir().unwrap();
        let adb = fake::device(dir.path(), 2, LISTED, true);
        let start = now();
        assert_eq!(adb.answering(None).unwrap(), "1791334000.123456789");
        assert_eq!(fake::asks_of(dir.path()), 3);
        assert_eq!(now() - start, Duration::from_secs(2));
    }

    /// The timeouts the wait gave its reads since the last call.
    fn take_timeouts() -> Vec<(&'static str, Duration)> {
        TIMEOUTS.with(|timeouts| std::mem::take(&mut *timeouts.borrow_mut()))
    }

    /// The wait is 30 s through the call `answering` makes: a device that
    /// stays offline is asked once a second, the last time within the wait's
    /// last second, then is gone with the restart fix. A wait of any other
    /// length asks another number of times, so this fails when the default
    /// stops being used.
    #[test]
    fn the_default_wait_is_thirty_seconds_and_ends_with_the_fix() {
        let dir = tempfile::tempdir().unwrap();
        let adb = fake::device(dir.path(), u32::MAX, LISTED, true);
        take_timeouts();
        let start = now();
        let error = adb.answering(None).unwrap_err();
        assert_eq!(now() - start, ANSWER_WAIT);
        assert_eq!(fake::asks_of(dir.path()), 30);
        let timeouts = take_timeouts();
        assert_eq!(timeouts.first(), Some(&("ask", ASK_TIMEOUT)));
        assert_eq!(timeouts.last(), Some(&("listing", ASK_TIMEOUT)));
        assert_eq!(
            timeouts[timeouts.len() - 2],
            ("ask", Duration::from_secs(1)),
            "the last ask is given the wait's last second"
        );
        assert_eq!(error.id, "android.device.none");
        assert_eq!(
            error.detail,
            "emulator-5580 is listed by adb but does not answer `date` within 30 s"
        );
        assert!(
            error.fix.summary.contains("restart it"),
            "{}",
            error.fix.summary
        );
        for command in [
            "adb -s emulator-5580 shell date",
            "adb -s emulator-5580 emu kill",
            "icm stop android --shutdown",
            "icm run android",
        ] {
            assert!(
                error.fix.commands.iter().any(|c| c == command),
                "{command} is missing: {:?}",
                error.fix.commands
            );
        }
        assert!(
            !error.fix.commands.iter().any(|c| c.contains("doctor")),
            "the catalogue's default fix creates the managed emulator: {:?}",
            error.fix.commands
        );
    }

    /// The run's `--timeout` ends the wait when it comes first: five asks a
    /// second apart, and none at the deadline, which has no time left for an
    /// ask. The error says the `--timeout` ended the wait, and does not say
    /// what adb lists, since the listing has no time left either.
    #[test]
    fn the_run_deadline_ends_the_wait_when_it_comes_first() {
        let dir = tempfile::tempdir().unwrap();
        let adb = fake::device(dir.path(), u32::MAX, LISTED, true);
        let start = now();
        let error = adb
            .answering(Some(start + Duration::from_secs(5)))
            .unwrap_err();
        assert_eq!(now() - start, Duration::from_secs(5));
        assert_eq!(fake::asks_of(dir.path()), 5);
        assert_eq!(
            error.detail,
            "emulator-5580 does not answer `date` before the --timeout ran out"
        );
    }

    /// An ask gets no more time than the budget has left: with three seconds
    /// left the asks are given three, two and one, never the 15 s of an ask
    /// with the whole wait ahead, and the wait ends at the deadline.
    #[test]
    fn an_ask_gets_no_more_than_the_budget_left() {
        let dir = tempfile::tempdir().unwrap();
        let adb = fake::device(dir.path(), u32::MAX, LISTED, true);
        take_timeouts();
        let start = now();
        let error = adb
            .answering(Some(start + Duration::from_secs(3)))
            .unwrap_err();
        assert_eq!(now() - start, Duration::from_secs(3));
        assert_eq!(fake::asks_of(dir.path()), 3);
        assert_eq!(
            take_timeouts(),
            [
                ("ask", Duration::from_secs(3)),
                ("ask", Duration::from_secs(2)),
                ("ask", Duration::from_secs(1)),
            ]
        );
        assert_eq!(
            error.detail,
            "emulator-5580 does not answer `date` before the --timeout ran out"
        );
    }

    /// A run whose `--timeout` is already used up makes no ask, as the restore
    /// does after a run that spent its budget: an ask can take its whole
    /// timeout, so the error comes at once, and the listing is not asked
    /// either. A deadline that is this very instant is used up too.
    #[test]
    fn an_expired_budget_makes_no_ask() {
        let dir = tempfile::tempdir().unwrap();
        let adb = fake::device(dir.path(), u32::MAX, LISTED, true);
        take_timeouts();
        let start = now();
        let deadline = start + Duration::from_secs(5);
        pause(Duration::from_secs(5));
        let error = adb.answering(Some(deadline)).unwrap_err();
        assert_eq!(error.id, "android.device.none");
        assert_eq!(
            error.detail,
            "emulator-5580 does not answer `date` before the --timeout ran out"
        );
        assert_eq!(now() - start, Duration::from_secs(5));
        assert_eq!(fake::asks_of(dir.path()), 0);
        assert!(take_timeouts().is_empty(), "a read ran with no budget left");

        let error = adb.answering(Some(now())).unwrap_err();
        assert_eq!(error.id, "android.device.none");
        assert_eq!(fake::asks_of(dir.path()), 0);
        assert!(take_timeouts().is_empty(), "a read ran with no budget left");
    }

    /// The closing listing gets the budget that is left, at most its own
    /// timeout: with one second left after the wait's 30 s, it is given one.
    #[test]
    fn the_closing_listing_gets_only_the_budget_left() {
        let dir = tempfile::tempdir().unwrap();
        let adb = fake::device(dir.path(), u32::MAX, LISTED, true);
        take_timeouts();
        let start = now();
        let error = adb
            .answering(Some(start + Duration::from_secs(31)))
            .unwrap_err();
        assert_eq!(now() - start, ANSWER_WAIT);
        assert_eq!(
            take_timeouts().last(),
            Some(&("listing", Duration::from_secs(1)))
        );
        assert_eq!(
            error.detail,
            "emulator-5580 is listed by adb but does not answer `date` within 30 s"
        );
    }

    /// A deadline after the default's end does not shorten the default wait.
    #[test]
    fn a_later_deadline_leaves_the_default_wait() {
        let dir = tempfile::tempdir().unwrap();
        let adb = fake::device(dir.path(), u32::MAX, LISTED, true);
        let start = now();
        let error = adb
            .answering(Some(start + Duration::from_secs(60)))
            .unwrap_err();
        assert_eq!(now() - start, ANSWER_WAIT);
        assert_eq!(fake::asks_of(dir.path()), 30);
        assert!(error.detail.ends_with("within 30 s"), "{}", error.detail);
    }

    /// The detail says what adb lists when the wait ends on its own: a device
    /// that is no longer listed is not said to be, and an unreadable listing
    /// says nothing.
    #[test]
    fn the_detail_says_what_adb_lists_when_the_wait_ends() {
        let start = now();
        let later = Some(start + Duration::from_secs(60));

        let gone_dir = tempfile::tempdir().unwrap();
        let gone = fake::device(gone_dir.path(), u32::MAX, Some(""), true);
        let error = gone.answering(later).unwrap_err();
        assert_eq!(
            error.detail,
            "emulator-5580 is not listed by adb and does not answer `date` within 30 s"
        );

        let unreadable_dir = tempfile::tempdir().unwrap();
        let unreadable = fake::device(unreadable_dir.path(), u32::MAX, None, true);
        let error = unreadable.answering(later).unwrap_err();
        assert_eq!(
            error.detail,
            "emulator-5580 does not answer `date` within 30 s"
        );
    }

    /// The listing runs with the tools' environment, as every other adb
    /// command does: a listing without it finds no SDK tools. The fake adb
    /// lists its device only when the variable the base carries is set.
    #[test]
    fn the_listing_keeps_the_tools_environment() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let tools = dir.path().join("platform-tools");
        std::fs::create_dir_all(&tools).unwrap();
        let path = tools.join("adb");
        let script = "#!/bin/sh\n\
            if [ \"$*\" = 'devices -l' ] && [ \"$ICM_TEST_TOOLS\" = set ]; then\n\
              printf 'List of devices attached\\nemulator-5580 device\\n'; exit 0\n\
            fi\n\
            exit 1\n";
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut adb = Adb::from_program(&path, "emulator-5580");
        assert_eq!(adb.listed(ASK_TIMEOUT), None);
        adb.base = adb.base.env("ICM_TEST_TOOLS", "set");
        assert_eq!(adb.listed(ASK_TIMEOUT), Some(true));
    }

    /// A phone that does not answer is fixed at its cable, and gets none of
    /// the emulator's commands.
    #[test]
    fn a_silent_phone_is_fixed_at_its_cable() {
        let dir = tempfile::tempdir().unwrap();
        fake::device(dir.path(), u32::MAX, Some("R58M123 device"), true);
        let phone = Adb::from_program(&dir.path().join("platform-tools/adb"), "R58M123");
        let error = phone
            .answering(Some(now() + Duration::from_secs(60)))
            .unwrap_err();
        assert_eq!(
            error.detail,
            "R58M123 is listed by adb but does not answer `date` within 30 s"
        );
        assert!(
            error.fix.summary.contains("USB cable"),
            "{}",
            error.fix.summary
        );
        assert!(
            !error
                .fix
                .commands
                .iter()
                .any(|c| c.contains("emu kill") || c.contains("--shutdown")),
            "{:?}",
            error.fix.commands
        );
    }

    /// The run's `--timeout` clips the wait only when it comes before the
    /// default's end, and then the clip is what ends it.
    #[test]
    fn the_deadline_clips_the_wait_only_when_it_comes_first() {
        let now = Instant::now();
        let five = Duration::from_secs(5);
        assert_eq!(wait_end(now, ANSWER_WAIT, None), (now + ANSWER_WAIT, false));
        assert_eq!(
            wait_end(now, ANSWER_WAIT, Some(now + Duration::from_secs(60))),
            (now + ANSWER_WAIT, false)
        );
        assert_eq!(
            wait_end(now, ANSWER_WAIT, Some(now + five)),
            (now + five, true)
        );
    }
}

/// Fake adbs for the tests of the android module.
#[cfg(test)]
pub(in crate::android) mod fake {
    use super::Adb;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    /// A fake adb at `dir/platform-tools/adb`, bound to `emulator-5580`. Every
    /// invocation but `devices -l` is an ask, and the first `offline` asks fail
    /// as adb does for a device it lists offline. The asks after them run: `date`
    /// answers the device clock, and any other line is logged in `dir/shells`
    /// and exits 0 (`shell_ok`) or 1. `devices -l` prints `listing` (`None`:
    /// it fails). The asks are counted in `dir/asks`.
    pub(in crate::android) fn device(
        dir: &Path,
        offline: u32,
        listing: Option<&str>,
        shell_ok: bool,
    ) -> Adb {
        let tools = dir.join("platform-tools");
        std::fs::create_dir_all(&tools).unwrap();
        let path = tools.join("adb");
        let devices = match listing {
            Some(text) => format!("printf 'List of devices attached\\n%s\\n' '{text}'; exit 0"),
            None => "exit 1".to_string(),
        };
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$*\" = 'devices -l' ]; then {devices}; fi\n\
             [ \"$1\" = -s ] && shift 2\n\
             n=$(cat '{asks}' 2>/dev/null || echo 0)\n\
             n=$((n + 1))\n\
             echo \"$n\" > '{asks}'\n\
             if [ \"$n\" -le {offline} ]; then echo 'error: device offline' >&2; exit 1; fi\n\
             case \"$*\" in\n\
             'shell date +%s.%N') echo 1791334000.123456789 ;;\n\
             *) echo \"$*\" >> '{shells}'; exit {status} ;;\n\
             esac\n",
            asks = dir.join("asks").display(),
            shells = dir.join("shells").display(),
            status = if shell_ok { 0 } else { 1 },
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Adb::from_program(&path, "emulator-5580")
    }

    /// The asks `device` has counted in `dir`.
    pub(in crate::android) fn asks_of(dir: &Path) -> u32 {
        std::fs::read_to_string(dir.join("asks"))
            .unwrap_or_default()
            .trim()
            .parse()
            .unwrap_or(0)
    }

    /// The shell lines (other than `date`) `device` has run in `dir`.
    pub(in crate::android) fn shells_of(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("shells"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}
