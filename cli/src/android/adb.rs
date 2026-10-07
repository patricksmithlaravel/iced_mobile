//! adb: the device list, shell commands and their parsing.
//!
//! `adb shell` joins its arguments with spaces and hands the line to the
//! device's shell, so every dynamic value is quoted here ([`quote`]).
//! icm never runs `adb kill-server`: other tools (and other agents) share
//! the server.

use super::Toolset;
use crate::error::IcmError;
use crate::process::{self, Cmd, Outcome};
use std::collections::BTreeMap;
use std::time::Duration;

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
        let text = self.shell_text("date +%s.%N", Duration::from_secs(15))?;
        let mark = text.lines().next()?.trim().to_string();
        // toybox prints a literal %N on very old images.
        let mark = mark.replace(".%N", ".0");
        mark.parse::<f64>().ok().map(|_| mark)
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
    fn text_input_is_quoted() {
        assert_eq!(input_text_arg("hello"), "hello");
        assert_eq!(input_text_arg("hello world"), "hello%sworld");
        assert_eq!(input_text_arg("it's"), "'it'\\''s'");
        assert_eq!(input_text_arg("a&b"), "'a&b'");
    }
}
