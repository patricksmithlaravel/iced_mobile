//! The Android commands against a fake SDK: a shell-script `adb` that
//! answers like an emulator and records its argv. They cover device
//! choice, the coordinate space of `input`, `devices`, `stop --shutdown`
//! and the preflight that fails before a build; the real pipeline is
//! verified on an emulator (see `docs/icm/DESIGN.md` Appendix D).

use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_icm");

/// The fake adb: one emulator, `emulator-5580`, running the AVD
/// `icm-api36` (or `$FAKE_AVD`) with a 1080x2400 display at 420 dpi. Every call is appended
/// to `$FAKE_ADB_LOG`; `emu kill` removes the emulator. `logcat` prints
/// `logcat.txt` (`events.txt` for `-b events`) next to the log, and `pidof`
/// the file `pidof` there when it exists (else 4321). Reading
/// `debug.icm.booted_by` prints the file `booted-by` there (else nothing),
/// and fails with the file `getprop-fails` there. `emu kill` also kills the
/// host process whose pid the file `emulator-pid` there holds, as a real
/// emulator exits when it is told to; with the file `ignores-emu-kill` there
/// it does nothing, as an emulator that hangs does, and stays listed.
const FAKE_ADB: &str = r#"#!/bin/sh
echo "$*" >> "$FAKE_ADB_LOG"
state_dir=$(dirname "$FAKE_ADB_LOG")
if [ "$1" = "devices" ]; then
  echo "List of devices attached"
  [ -f "$state_dir/killed" ] || echo "emulator-5580          device product:sdk_gphone64_arm64 model:fake device:emu64a transport_id:1"
  exit 0
fi
[ "$1" = "-s" ] && shift 2
case "$1" in
  emu)
    if [ "$2" = "kill" ]; then
      [ -f "$state_dir/ignores-emu-kill" ] && { echo OK; exit 0; }
      touch "$state_dir/killed"
      [ -f "$state_dir/emulator-pid" ] && kill "$(cat "$state_dir/emulator-pid")" 2>/dev/null
      echo OK; exit 0
    fi
    echo "${FAKE_AVD:-icm-api36}"; echo "OK"; exit 0 ;;
  shell)
    case "$2" in
      "getprop ro.product.cpu.abi") echo "arm64-v8a" ;;
      "getprop ro.build.version.sdk") echo "36" ;;
      "getprop debug.icm.booted_by")
        if [ -f "$state_dir/getprop-fails" ]; then echo "error: closed" >&2; exit 1; fi
        if [ -f "$state_dir/booted-by" ]; then cat "$state_dir/booted-by"; else echo ""; fi ;;
      getprop*) echo "" ;;
      "wm density") echo "Physical density: 420" ;;
      "wm size") echo "Physical size: 1080x2400" ;;
      "dumpsys window displays") echo "  init=1080x2400 420dpi cur=1080x2400 app=1080x2337" ;;
      pidof*) if [ -f "$state_dir/pidof" ]; then cat "$state_dir/pidof"; else echo "4321"; fi ;;
      "date +%s.%N") echo "1791334000.123456789" ;;
      *) ;;
    esac
    exit 0 ;;
  logcat)
    case "$*" in
      *"-b events"*) [ -f "$state_dir/events.txt" ] && cat "$state_dir/events.txt" ;;
      *) [ -f "$state_dir/logcat.txt" ] && cat "$state_dir/logcat.txt" ;;
    esac
    exit 0 ;;
esac
exit 0
"#;

struct Sandbox {
    root: tempfile::TempDir,
    project: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        let root = tempfile::tempdir().unwrap();
        let sdk = root.path().join("sdk");
        for dir in ["platform-tools", "platforms", "avd", "cache"] {
            std::fs::create_dir_all(sdk.join(dir)).unwrap();
        }
        let adb = sdk.join("platform-tools/adb");
        std::fs::write(&adb, FAKE_ADB).unwrap();
        std::fs::set_permissions(&adb, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(
            root.path().join("host.toml"),
            format!("android_sdk = \"{}\"\n", sdk.display()),
        )
        .unwrap();

        let project = root.path().join("app");
        copy_dir(&fixtures().join("app"), &project);
        Sandbox { root, project }
    }

    fn log(&self) -> PathBuf {
        self.root.path().join("adb.log")
    }

    fn adb_calls(&self) -> String {
        std::fs::read_to_string(self.log()).unwrap_or_default()
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(BIN);
        let _ = command
            .args(args)
            .current_dir(&self.project)
            .env("ICM_CACHE_DIR", self.root.path().join("cache"))
            .env("ICM_HOST_CONFIG", self.root.path().join("host.toml"))
            .env("CARGO_TARGET_DIR", self.project.join("target"))
            .env("ANDROID_AVD_HOME", self.root.path().join("sdk/avd"))
            .env("FAKE_ADB_LOG", self.log())
            .env_remove("ANDROID_HOME")
            .env_remove("ANDROID_SDK_ROOT")
            .env_remove("ANDROID_NDK_HOME")
            .env_remove("ANDROID_NDK_ROOT")
            .stdin(Stdio::null());
        for var in [
            "ICM_JSON",
            "ICM_CONFIG",
            "ICM_TIMEOUT",
            "ICM_RUN_ID",
            "ICM_DETACHED",
            "ANDROID_SERIAL",
        ] {
            let _ = command.env_remove(var);
        }
        let _ = command.envs(env.iter().copied());
        command.output().unwrap()
    }

    /// The result object (the last line of `--json -q`).
    fn result(&self, args: &[&str]) -> Value {
        self.result_with(args, &[])
    }

    /// [`Sandbox::result`] with extra environment.
    fn result_with(&self, args: &[&str], env: &[(&str, &str)]) -> Value {
        let mut args = args.to_vec();
        args.extend(["--json", "-q"]);
        let output = self.run(&args, env);
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let last = text.lines().last().unwrap_or_else(|| {
            panic!(
                "no output; stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        let result: Value = serde_json::from_str(last).unwrap();
        assert_eq!(result["type"], "result");
        assert_eq!(output.status.code().map(i64::from), result["exit"].as_i64());
        result
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            let _ = std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

#[test]
fn taps_are_in_preview_pixels_by_default() {
    let sandbox = Sandbox::new();
    // 1080x2400 previews at 461x1024, so a preview pixel is 1080/461 device
    // pixels across and 2400/1024 down; preview (230.5, 512) is the centre.
    let result = sandbox.result(&["input", "android", "tap", "230.5", "512"]);
    assert_eq!(result["exit"], 0, "{result}");
    assert_eq!(result["input"]["px"], serde_json::json!([540, 1200]));
    assert_eq!(result["screen"]["preview"], serde_json::json!([461, 1024]));
    assert_eq!(result["screen"]["scale"], 2.625);
    assert!(
        sandbox
            .adb_calls()
            .contains("-s emulator-5580 shell input tap 540 1200"),
        "{}",
        sandbox.adb_calls()
    );

    let result = sandbox.result(&["input", "android", "tap", "100", "200", "--space", "pt"]);
    assert_eq!(result["input"]["px"], serde_json::json!([263, 525]));

    let result = sandbox.result(&["input", "android", "tap", "2000", "10"]);
    assert_eq!(result["exit"], 2);
    assert_eq!(result["errors"][0]["id"], "usage.bad_args");
}

#[test]
fn text_and_keys_go_through_adb_input() {
    let sandbox = Sandbox::new();
    let result = sandbox.result(&["input", "android", "text", "hello world"]);
    assert_eq!(result["exit"], 0, "{result}");
    let result = sandbox.result(&["input", "android", "key", "back"]);
    assert_eq!(result["exit"], 0, "{result}");
    let calls = sandbox.adb_calls();
    assert!(calls.contains("shell input text hello%sworld"), "{calls}");
    assert!(
        calls.contains("shell input keyevent KEYCODE_BACK"),
        "{calls}"
    );

    let result = sandbox.result(&["input", "android", "text", "naïve"]);
    assert_eq!(result["exit"], 2);
}

#[test]
fn devices_lists_emulators_with_their_avds() {
    let sandbox = Sandbox::new();
    let result = sandbox.result(&["devices", "android"]);
    assert_eq!(result["exit"], 0, "{result}");
    let device = &result["devices"][0];
    assert_eq!(device["serial"], "emulator-5580");
    assert_eq!(device["avd"], "icm-api36");
    assert_eq!(device["abi"], "arm64-v8a");
    assert_eq!(result["default_avd"], "icm-api36");
}

#[test]
fn stop_shutdown_stops_only_icms_emulator() {
    let sandbox = Sandbox::new();
    let result = sandbox.result(&["stop", "android"]);
    assert_eq!(result["exit"], 0, "{result}");
    assert!(!sandbox.adb_calls().contains("emu kill"));

    let result = sandbox.result(&["stop", "android", "--shutdown"]);
    assert_eq!(result["exit"], 0, "{result}");
    assert_eq!(result["stopped"][0]["emulator"], "emulator-5580");
    assert!(sandbox.adb_calls().contains("-s emulator-5580 emu kill"));
}

/// An emulator whose owner icm cannot read is not taken for one nobody
/// claimed: `--shutdown` leaves it running and warns, naming it.
#[test]
fn an_unreadable_owner_keeps_the_emulator_running() {
    let sandbox = Sandbox::new();
    std::fs::write(sandbox.root.path().join("getprop-fails"), "").unwrap();
    let output = sandbox.run(&["stop", "android", "--shutdown", "--json"], &[]);
    let events: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let result = events.last().unwrap();
    assert_eq!(result["exit"], 0, "{result}");
    let calls = sandbox.adb_calls();
    assert!(
        calls.contains("-s emulator-5580 shell getprop debug.icm.booted_by"),
        "{calls}"
    );
    assert!(!calls.contains("emu kill"), "{calls}");
    assert_eq!(result["stopped"], serde_json::json!([]), "{result}");
    let unknown: Vec<&Value> = events
        .iter()
        .filter(|event| event["type"] == "check" && event["id"] == "android.emulator.owner_unknown")
        .collect();
    assert_eq!(unknown.len(), 1, "{events:?}");
    assert_eq!(unknown[0]["status"], "warn");
    let detail = unknown[0]["detail"].as_str().unwrap();
    assert!(
        detail.contains("emulator-5580 left running") && detail.contains("exit 1"),
        "{detail}"
    );
    assert_eq!(
        unknown[0]["fix"]["commands"],
        serde_json::json!(["adb -s emulator-5580 emu kill"]),
        "{events:?}"
    );

    // Readable and unset again: nobody's, so shut down as before.
    std::fs::remove_file(sandbox.root.path().join("getprop-fails")).unwrap();
    let result = sandbox.result(&["stop", "android", "--shutdown"]);
    assert_eq!(
        result["stopped"][0]["emulator"], "emulator-5580",
        "{result}"
    );
    assert!(sandbox.adb_calls().contains("-s emulator-5580 emu kill"));
}

/// A session whose emulator icm booted says nothing about the emulator on
/// its serial once that emulator's process has exited: another project may
/// have booted the managed AVD on the same port since and claimed it.
/// `--shutdown` reads the owner then, and leaves that emulator, and the app
/// on it, alone.
#[test]
fn a_stale_session_never_shuts_down_another_projects_emulator() {
    let sandbox = Sandbox::new();
    std::fs::write(sandbox.root.path().join("booted-by"), "fedcba9876543210\n").unwrap();
    let mut exited = Command::new("true").spawn().unwrap();
    let dead = exited.id();
    let _ = exited.wait().unwrap();
    let sessions = sandbox.project.join("target/icm/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("android.json"),
        serde_json::json!({
            "schema": "icm.session.android/1", "run": "r1", "serial": "emulator-5580",
            "kind": "emulator", "avd": "icm-api36", "booted_by_icm": true,
            "emulator_pid": dead, "abi": "arm64-v8a", "app_id": "com.example.app",
            "started": "2026-10-06T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let output = sandbox.run(&["stop", "android", "--shutdown", "--json"], &[]);
    let events: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let result = events.last().unwrap();
    assert_eq!(result["exit"], 0, "{result}");
    let calls = sandbox.adb_calls();
    assert!(
        calls.contains("-s emulator-5580 shell getprop debug.icm.booted_by"),
        "{calls}"
    );
    assert!(!calls.contains("emu kill"), "{calls}");
    assert!(!calls.contains("force-stop"), "{calls}");
    assert_eq!(result["stopped"], serde_json::json!([]), "{result}");
    let shared: Vec<&Value> = events
        .iter()
        .filter(|event| event["type"] == "check" && event["id"] == "android.emulator.shared")
        .collect();
    assert_eq!(shared.len(), 1, "{events:?}");
    assert!(
        shared[0]["detail"]
            .as_str()
            .unwrap()
            .contains("fedcba9876543210"),
        "{events:?}"
    );
    assert!(
        !sessions.join("android.json").exists(),
        "the stale session was kept"
    );

    // Nobody's, but no icm AVD: the owner's own AVD on the reused port is
    // never taken for the session's. Neither the app nor the emulator is
    // touched, and each INFO says what icm knows: the recorded process has
    // ended or was replaced, and the property is unset.
    std::fs::remove_file(sandbox.root.path().join("booted-by")).unwrap();
    std::fs::write(sandbox.log(), "").unwrap();
    std::fs::write(
        sessions.join("android.json"),
        serde_json::json!({
            "schema": "icm.session.android/1", "run": "r2", "serial": "emulator-5580",
            "kind": "emulator", "avd": "icm-api36", "booted_by_icm": true,
            "emulator_pid": dead, "abi": "arm64-v8a", "app_id": "com.example.app",
            "started": "2026-10-06T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();
    let output = sandbox.run(
        &["stop", "android", "--shutdown", "--json"],
        &[("FAKE_AVD", "owners_pixel")],
    );
    let events: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let result = events.last().unwrap();
    assert_eq!(result["exit"], 0, "{result}");
    let calls = sandbox.adb_calls();
    assert!(!calls.contains("emu kill"), "{calls}");
    assert!(!calls.contains("force-stop"), "{calls}");
    let left: Vec<&str> = checks(&events, "run.no_session")
        .iter()
        .map(|event| event["detail"].as_str().unwrap())
        .collect();
    assert_eq!(left.len(), 2, "{events:?}");
    assert!(
        left[0].starts_with("com.acme.fixture was not force-stopped on emulator-5580: the recorded emulator process has ended or was replaced;"),
        "{left:?}"
    );
    assert!(
        left[1].starts_with("emulator-5580 (owners_pixel) left running: the recorded emulator process has ended or was replaced;"),
        "{left:?}"
    );
    for detail in &left {
        assert!(
            detail.contains("debug.icm.booted_by is unset") && !detail.contains("did not boot"),
            "{detail}"
        );
    }
}

impl Sandbox {
    /// `args` with `--json`: every event, the result last.
    fn events(&self, args: &[&str], env: &[(&str, &str)]) -> Vec<Value> {
        let mut args = args.to_vec();
        args.push("--json");
        String::from_utf8_lossy(&self.run(&args, env).stdout)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// `target/icm/sessions`.
    fn sessions(&self) -> PathBuf {
        let sessions = self.project.join("target/icm/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        sessions
    }

    /// The Android session a run that booted `emulator-5580` left, with
    /// `fields` added (the emulator's pid, its identity).
    fn write_session(&self, fields: Value) {
        let mut session = serde_json::json!({
            "schema": "icm.session.android/1", "run": "r1", "serial": "emulator-5580",
            "kind": "emulator", "avd": "icm-api36", "booted_by_icm": true,
            "abi": "arm64-v8a", "app_id": "com.example.app",
            "started": "2026-10-06T00:00:00Z"
        });
        session
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        std::fs::write(self.sessions().join("android.json"), session.to_string()).unwrap();
    }
}

/// The checks of one id.
fn checks<'a>(events: &'a [Value], id: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|event| event["type"] == "check" && event["id"] == id)
        .collect()
}

/// The stand-in for an emulator's host process: a `sleep` whose parent has
/// exited, as the emulator an earlier `icm run` booted is, so that it is
/// reaped as soon as it ends (`adb emu kill` ends it, as it does the real
/// one). Killed when dropped.
struct Emulator(u32);

impl Emulator {
    fn start() -> Emulator {
        let output = Command::new("sh")
            .args(["-c", "sleep 120 >/dev/null 2>&1 & echo $!"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        Emulator(
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse()
                .unwrap(),
        )
    }

    /// Whether the process still runs.
    fn running(&self) -> bool {
        icm::procid::of(self.0 as i32).is_some()
    }

    /// What icm records when it starts the process.
    fn identity(&self) -> Value {
        serde_json::to_value(icm::procid::of(self.0 as i32).unwrap()).unwrap()
    }

    /// Ends the process and waits until it is gone.
    fn end(&self) {
        // SAFETY: kill(2) on a process this test started.
        unsafe {
            let _ = libc::kill(self.0 as i32, libc::SIGKILL);
        }
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while self.running() && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!self.running());
    }
}

/// The pid a fake `emulator` script wrote to `file`. icm starts the script
/// detached and does not wait for it, so the script's own first lines run
/// concurrently with whatever icm and the test do next: the file may not
/// exist yet when icm returns. Polls until a pid is there, for at most
/// `within`; `None` when the script wrote none by then. The script writes it
/// atomically (a temporary file renamed over `file`), so a pid that is read
/// is a whole one.
fn written_pid(file: &Path, within: std::time::Duration) -> Option<u32> {
    let until = std::time::Instant::now() + within;
    loop {
        let pid = std::fs::read_to_string(file)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
            .filter(|pid| *pid > 1);
        if pid.is_some() || std::time::Instant::now() >= until {
            return pid;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The emulator a fake `emulator` script started, whose pid it wrote to the
/// file: killed when dropped, whatever the test did. The script writes the
/// file a moment after it starts, so the guard waits for it (a bounded wait,
/// which only a test that never started the emulator pays) rather than
/// finding it missing when the test failed early. The process is not judged
/// by its program: that is the shell until the script execs `sleep` (and
/// the shell's own name varies, `sh` is `bash` here), and while it execs
/// it is neither. The script lives 120 s, far longer than a test, so the
/// number is still the script's when the guard runs.
struct Started(PathBuf);

impl Drop for Started {
    fn drop(&mut self) {
        if let Some(pid) = written_pid(&self.0, std::time::Duration::from_secs(5)) {
            // SAFETY: kill(2) on the process a script of this test started.
            unsafe {
                let _ = libc::kill(pid as i32, libc::SIGKILL);
            }
        }
    }
}

/// Even console ports in the range host.toml accepts whose adb port, the
/// next one, is free too, `count` of them, from a start that differs from
/// one process to the next. A fake emulator holds no port, so another run
/// of this test (a second checkout's `cargo test`) can pick the same one,
/// or be probing it in `bind` while icm looks: icm takes the first of
/// several that is free when it looks, and the test reads the serial it
/// took from the record.
fn free_console_ports(count: usize) -> Vec<u16> {
    let candidates: Vec<u16> = (5554..=5682).step_by(2).collect();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos() as usize);
    let start = (std::process::id() as usize ^ nanos) % candidates.len();
    let free: Vec<u16> = (0..candidates.len())
        .map(|offset| candidates[(start + offset) % candidates.len()])
        .filter(|port| {
            std::net::TcpListener::bind(("127.0.0.1", *port)).is_ok()
                && std::net::TcpListener::bind(("127.0.0.1", *port + 1)).is_ok()
        })
        .take(count)
        .collect();
    assert_eq!(free.len(), count, "too many console ports are busy");
    free
}

impl Drop for Emulator {
    fn drop(&mut self) {
        // SAFETY: kill(2) on a process this test started.
        unsafe {
            let _ = libc::kill(self.0 as i32, libc::SIGKILL);
        }
    }
}

/// An identity that no process has now: what another process that had the
/// pid before recorded.
fn another_identity() -> Value {
    serde_json::json!({"start": "1791334000.000001", "exe": "/sdk/emulator/emulator"})
}

impl Sandbox {
    /// What an emulator booted by this project is tagged with
    /// (`debug.icm.booted_by`).
    fn own_tag(&self) -> String {
        icm::hash::sha256_hex(self.sessions().to_string_lossy().as_bytes())[..16].to_string()
    }

    /// The fake adb answers `debug.icm.booted_by` with `tag`.
    fn owned_by(&self, tag: &str) {
        std::fs::write(self.root.path().join("booted-by"), format!("{tag}\n")).unwrap();
    }

    /// The record `icm run` keeps per emulator it booted, with `fields`
    /// added (the emulator's pid, its identity).
    fn write_booted(&self, fields: Value) {
        let mut record = serde_json::json!({"serial": "emulator-5580", "avd": "icm-api36"});
        record
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let dir = self.sessions().join("android-booted");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("emulator-5580.json"), record.to_string()).unwrap();
    }

    /// `stop android --shutdown`'s events.
    fn shutdown(&self) -> Vec<Value> {
        self.events(&["stop", "android", "--shutdown"], &[])
    }
}

/// What `stop --shutdown` does for an emulator it leaves alone: it read the
/// owner, sent the emulator nothing (no `am force-stop`, no `emu kill`, no
/// signal) and stopped nothing.
fn assert_left_alone(sandbox: &Sandbox, events: &[Value], emulator: &Emulator, case: &str) {
    let result = events.last().unwrap();
    assert_eq!(result["exit"], 0, "{case}: {result}");
    let calls = sandbox.adb_calls();
    assert!(
        calls.contains("-s emulator-5580 shell getprop debug.icm.booted_by"),
        "{case}: {calls}"
    );
    assert!(!calls.contains("emu kill"), "{case}: {calls}");
    assert!(!calls.contains("force-stop"), "{case}: {calls}");
    assert_eq!(result["stopped"], serde_json::json!([]), "{case}: {result}");
    assert!(emulator.running(), "{case}: the process was signalled");
}

/// A recorded emulator pid that another process has now says nothing about
/// the emulator on the record's serial. A session (or a per-serial record)
/// of an emulator icm booted named the pid of an unrelated live process, and
/// `stop --shutdown` took the emulator on emulator-5580 for the one the
/// run booted: it sent `am force-stop` and `emu kill` to another project's
/// emulator without reading `debug.icm.booted_by`, and then SIGTERMed the
/// unrelated process. A record counts for the emulator only while its pid
/// still has the process icm recorded (its start time), and an older
/// record, which holds no such identity, proves nothing.
#[test]
fn a_reused_host_pid_never_makes_a_stale_record_authoritative() {
    let session = |fields: Value| move |sandbox: &Sandbox| sandbox.write_session(fields.clone());
    let booted = |fields: Value| move |sandbox: &Sandbox| sandbox.write_booted(fields.clone());
    type Write = Box<dyn Fn(&Sandbox)>;
    let live = |pid: u32, identity: Option<Value>| {
        let mut fields = serde_json::json!({"emulator_pid": pid});
        if let Some(identity) = identity {
            fields["emulator_identity"] = identity;
        }
        fields
    };
    for case in [
        "a session an older icm wrote",
        "a session with another process's identity",
        "a record of the emulator an older icm wrote",
        "a record with another process's identity",
    ] {
        let sandbox = Sandbox::new();
        sandbox.owned_by("fedcba9876543210");
        let process = Emulator::start();
        let write: Write = match case {
            "a session an older icm wrote" => Box::new(session(live(process.0, None))),
            "a session with another process's identity" => {
                Box::new(session(live(process.0, Some(another_identity()))))
            }
            "a record of the emulator an older icm wrote" => {
                Box::new(booted(live(process.0, None)))
            }
            _ => Box::new(booted(live(process.0, Some(another_identity())))),
        };
        write(&sandbox);

        let events = sandbox.shutdown();
        assert_left_alone(&sandbox, &events, &process, case);
        let shared = checks(&events, "android.emulator.shared");
        assert_eq!(shared.len(), 1, "{case}: {events:?}");
        assert!(
            shared[0]["detail"]
                .as_str()
                .unwrap()
                .contains("fedcba9876543210"),
            "{case}: {events:?}"
        );
        assert!(!sandbox.sessions().join("android.json").exists(), "{case}");
    }
}

/// The owner is read before anything is done to an emulator a live record
/// names, and a failed read leaves it running with a WARN, as it does for
/// an emulator with no record.
#[test]
fn an_unreadable_owner_keeps_the_recorded_emulator_running_too() {
    for (case, identity) in [
        ("a verified process", Some("verified")),
        ("a pid of an older record", None),
        ("another process's pid", Some("another")),
    ] {
        let sandbox = Sandbox::new();
        std::fs::write(sandbox.root.path().join("getprop-fails"), "").unwrap();
        let process = Emulator::start();
        let identity = match identity {
            Some("verified") => Some(process.identity()),
            Some(_) => Some(another_identity()),
            None => None,
        };
        let mut fields = serde_json::json!({"emulator_pid": process.0});
        if let Some(identity) = identity {
            fields["emulator_identity"] = identity;
        }
        sandbox.write_session(fields.clone());
        sandbox.write_booted(fields);

        let events = sandbox.shutdown();
        assert_left_alone(&sandbox, &events, &process, case);
        let unknown = checks(&events, "android.emulator.owner_unknown");
        // A verified process or an old pid may be the session's emulator:
        // its app was left running and the emulator too, said once.
        assert_eq!(unknown.len(), 1, "{case}: {events:?}");
        assert_eq!(unknown[0]["status"], "warn", "{case}");
        assert_eq!(
            unknown[0]["fix"]["commands"],
            serde_json::json!(["adb -s emulator-5580 emu kill"]),
            "{case}"
        );
    }
}

/// A record whose process still has the identity icm recorded is the
/// emulator icm booted: `stop --shutdown` force-stops the app and shuts it
/// down, after reading its owner, and the process ends with it.
#[test]
fn a_verified_emulator_process_is_shut_down_as_before() {
    for (case, owner) in [
        ("no mark on it", None),
        ("marked as this project's", Some(())),
    ] {
        let sandbox = Sandbox::new();
        if owner.is_some() {
            sandbox.owned_by(&sandbox.own_tag());
        }
        let process = Emulator::start();
        std::fs::write(
            sandbox.root.path().join("emulator-pid"),
            process.0.to_string(),
        )
        .unwrap();
        let fields = serde_json::json!({
            "emulator_pid": process.0, "emulator_identity": process.identity()
        });
        sandbox.write_session(fields.clone());
        sandbox.write_booted(fields);

        let events = sandbox.shutdown();
        let result = events.last().unwrap();
        assert_eq!(result["exit"], 0, "{case}: {result}");
        let calls = sandbox.adb_calls();
        assert!(
            calls.contains("-s emulator-5580 shell getprop debug.icm.booted_by"),
            "{case}: {calls}"
        );
        assert!(calls.contains("shell am force-stop"), "{case}: {calls}");
        assert!(
            calls.contains("-s emulator-5580 emu kill"),
            "{case}: {calls}"
        );
        assert_eq!(
            result["stopped"],
            serde_json::json!([
                {"platform": "android", "app": "com.acme.fixture", "serial": "emulator-5580"},
                {"platform": "android", "emulator": "emulator-5580"}
            ]),
            "{case}: {result}"
        );
        // `emu kill` ended it.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while process.running() && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!process.running(), "{case}");
        assert!(
            !sandbox
                .sessions()
                .join("android-booted/emulator-5580.json")
                .exists()
        );
    }
}

/// A record that proves the emulator is this project's does not outrank the
/// device: when its owner property names another project, nothing is done
/// to it, the process included.
#[test]
fn a_verified_process_does_not_outrank_the_devices_owner() {
    let sandbox = Sandbox::new();
    sandbox.owned_by("fedcba9876543210");
    let process = Emulator::start();
    sandbox.write_session(serde_json::json!({
        "emulator_pid": process.0, "emulator_identity": process.identity()
    }));

    let events = sandbox.shutdown();
    assert_left_alone(&sandbox, &events, &process, "verified, another project's");
    let shared = checks(&events, "android.emulator.shared");
    assert_eq!(shared.len(), 1, "{events:?}");
}

/// An older record has no identity, so its pid is never signalled; but an
/// emulator the device itself marks as this project's is still shut down,
/// since the mark says icm booted it for the project.
#[test]
fn an_older_record_is_acted_on_only_as_far_as_the_devices_mark_goes() {
    let sandbox = Sandbox::new();
    sandbox.owned_by(&sandbox.own_tag());
    // A live process whose pid the record names, which `emu kill` does not
    // end (the fake adb is not told it is the emulator's).
    let process = Emulator::start();
    sandbox.write_session(serde_json::json!({"emulator_pid": process.0}));

    let events = sandbox.shutdown();
    let result = events.last().unwrap();
    assert_eq!(result["exit"], 0, "{result}");
    let calls = sandbox.adb_calls();
    assert!(calls.contains("shell am force-stop"), "{calls}");
    assert!(calls.contains("-s emulator-5580 emu kill"), "{calls}");
    assert_eq!(
        result["stopped"][1]["emulator"], "emulator-5580",
        "{result}"
    );
    // No identity, so no SIGTERM, however long the wait took.
    assert!(process.running(), "an unverified pid was signalled");
}

/// A record whose emulator process is gone is no proof, whether it holds
/// an identity or not: the serial may hold another emulator, so the owner is
/// read and decides, whatever the record says. Another project's emulator is
/// left alone with the app on it; one the device marks as this project's is
/// stopped, since the mark says icm booted it for the project.
#[test]
fn a_record_of_an_exited_emulator_proves_nothing() {
    let sandbox = Sandbox::new();
    sandbox.owned_by("fedcba9876543210");
    let process = Emulator::start();
    let identity = process.identity();
    process.end();
    let record = serde_json::json!({"emulator_pid": process.0, "emulator_identity": identity});
    sandbox.write_session(record.clone());
    let events = sandbox.shutdown();
    let result = events.last().unwrap();
    assert_eq!(result["exit"], 0, "{result}");
    let calls = sandbox.adb_calls();
    assert!(
        calls.contains("-s emulator-5580 shell getprop debug.icm.booted_by"),
        "{calls}"
    );
    assert!(!calls.contains("emu kill"), "{calls}");
    assert!(!calls.contains("force-stop"), "{calls}");
    assert_eq!(
        checks(&events, "android.emulator.shared").len(),
        1,
        "{events:?}"
    );

    // The same record on an emulator this project booted and marked.
    sandbox.owned_by(&sandbox.own_tag());
    std::fs::write(sandbox.log(), "").unwrap();
    sandbox.write_session(record);
    let events = sandbox.shutdown();
    let result = events.last().unwrap();
    assert_eq!(result["exit"], 0, "{result}");
    let calls = sandbox.adb_calls();
    assert!(calls.contains("shell am force-stop"), "{calls}");
    assert!(calls.contains("-s emulator-5580 emu kill"), "{calls}");
    assert_eq!(
        result["stopped"],
        serde_json::json!([
            {"platform": "android", "app": "com.acme.fixture", "serial": "emulator-5580"},
            {"platform": "android", "emulator": "emulator-5580"}
        ]),
        "{result}"
    );
}

/// A record whose emulator process no longer matches (another process has
/// its pid, as after the pid was reused) says only that icm cannot confirm
/// the process. While its serial is online, `stop --shutdown` decides as it
/// does for a record from before identities, by the device's
/// `debug.icm.booted_by`, on an `icm-test-*` AVD that nothing else vouches
/// for:
///
/// - this project's tag: the app is force-stopped and the emulator shut down
///   with `emu kill`, and the recorded pid is never signalled;
/// - another project's: both are left running, INFO `android.emulator.shared`;
/// - unset: both are left running (fail-closed), and INFO `run.no_session`
///   says what icm knows, that the recorded emulator process has ended or
///   was replaced and the property is unset, never that icm did not boot it;
/// - unreadable: both are left running, WARN `android.emulator.owner_unknown`.
///
/// Before, a mismatched session left the app and the emulator running without
/// reading the property and said icm did not boot an emulator its record says
/// it booted, and a mismatched per-serial record was not looked at at all.
#[test]
fn a_record_whose_process_no_longer_matches_goes_by_the_devices_owner() {
    for records in ["a session and its record", "only the per-serial record"] {
        let with_session = records.starts_with("a session");
        for owner in [
            "this project's tag",
            "another project's tag",
            "unset",
            "unreadable",
        ] {
            let case = format!("{records}, owner {owner}");
            let sandbox = Sandbox::new();
            match owner {
                "this project's tag" => sandbox.owned_by(&sandbox.own_tag()),
                "another project's tag" => sandbox.owned_by("fedcba9876543210"),
                "unset" => {}
                _ => std::fs::write(sandbox.root.path().join("getprop-fails"), "").unwrap(),
            }
            // A live process that has the recorded pid but not the recorded
            // identity; `emu kill` is not told it is the emulator's.
            let process = Emulator::start();
            let fields = serde_json::json!({
                "avd": "icm-test-api36",
                "emulator_pid": process.0, "emulator_identity": another_identity()
            });
            if with_session {
                sandbox.write_session(fields.clone());
            }
            sandbox.write_booted(fields);

            let events = sandbox.events(
                &["stop", "android", "--shutdown"],
                &[("FAKE_AVD", "icm-test-api36")],
            );
            let record = sandbox.sessions().join("android-booted/emulator-5580.json");
            let (shared, unknown, unproven) = (
                checks(&events, "android.emulator.shared"),
                checks(&events, "android.emulator.owner_unknown"),
                checks(&events, "run.no_session"),
            );
            // What each says about the app and about the emulator.
            let where_it_was_left = if with_session {
                "emulator-5580 kept running with com.acme.fixture on it"
            } else {
                "emulator-5580 left running"
            };
            if owner == "this project's tag" {
                let result = events.last().unwrap();
                assert_eq!(result["exit"], 0, "{case}: {result}");
                let calls = sandbox.adb_calls();
                assert!(
                    calls.contains("-s emulator-5580 shell getprop debug.icm.booted_by"),
                    "{case}: {calls}"
                );
                assert_eq!(
                    calls.contains("shell am force-stop com.acme.fixture"),
                    with_session,
                    "{case}: {calls}"
                );
                assert!(
                    calls.contains("-s emulator-5580 emu kill"),
                    "{case}: {calls}"
                );
                let mut stopped = vec![];
                if with_session {
                    stopped.push(serde_json::json!(
                        {"platform": "android", "app": "com.acme.fixture", "serial": "emulator-5580"}
                    ));
                }
                stopped
                    .push(serde_json::json!({"platform": "android", "emulator": "emulator-5580"}));
                assert_eq!(result["stopped"], Value::Array(stopped), "{case}: {result}");
                assert!(
                    shared.is_empty() && unknown.is_empty() && unproven.is_empty(),
                    "{case}: {events:?}"
                );
                assert!(process.running(), "{case}: the recorded pid was signalled");
                assert!(!record.exists(), "{case}: the record outlived the emulator");
                assert!(!sandbox.sessions().join("android.json").exists(), "{case}");
                continue;
            }
            assert_left_alone(&sandbox, &events, &process, &case);
            assert!(record.exists(), "{case}: the record of a running emulator");
            assert!(!sandbox.sessions().join("android.json").exists(), "{case}");
            match owner {
                "another project's tag" => {
                    assert_eq!(shared.len(), 1, "{case}: {events:?}");
                    let detail = shared[0]["detail"].as_str().unwrap();
                    assert!(
                        detail.starts_with(where_it_was_left)
                            && detail.contains("fedcba9876543210"),
                        "{case}: {detail}"
                    );
                    assert!(unknown.is_empty() && unproven.is_empty(), "{case}");
                }
                "unreadable" => {
                    assert_eq!(unknown.len(), 1, "{case}: {events:?}");
                    assert_eq!(unknown[0]["status"], "warn", "{case}");
                    assert!(
                        unknown[0]["detail"]
                            .as_str()
                            .unwrap()
                            .starts_with(where_it_was_left),
                        "{case}: {events:?}"
                    );
                    assert_eq!(
                        unknown[0]["fix"]["commands"],
                        serde_json::json!(["adb -s emulator-5580 emu kill"]),
                        "{case}"
                    );
                    assert!(shared.is_empty() && unproven.is_empty(), "{case}");
                }
                _ => {
                    // The app, when a session names it, then the emulator.
                    let details: Vec<&str> = unproven
                        .iter()
                        .map(|check| check["detail"].as_str().unwrap())
                        .collect();
                    assert_eq!(
                        details.len(),
                        1 + usize::from(with_session),
                        "{case}: {events:?}"
                    );
                    for detail in &details {
                        assert!(
                            detail.contains(
                                "the recorded emulator process has ended or was replaced;"
                            ) && detail.contains("debug.icm.booted_by is unset"),
                            "{case}: {detail}"
                        );
                        assert!(!detail.contains("did not boot"), "{case}: {detail}");
                    }
                    if with_session {
                        assert!(
                            details[0].starts_with(
                                "com.acme.fixture was not force-stopped on emulator-5580:"
                            ),
                            "{case}: {details:?}"
                        );
                    }
                    assert!(
                        details
                            .last()
                            .unwrap()
                            .starts_with("emulator-5580 (icm-test-api36) left running:"),
                        "{case}: {details:?}"
                    );
                    for check in &unproven {
                        assert_eq!(check["status"], "info", "{case}");
                    }
                    assert!(shared.is_empty() && unknown.is_empty(), "{case}");
                }
            }
        }
    }
}

/// The recorded pid of a process that does not match is never signalled,
/// even for an emulator that is this project's by its mark and ignores
/// `emu kill`: there is nothing icm may end, so it is reported as still
/// running, with its record kept.
#[test]
fn a_mismatched_emulator_process_is_not_signalled_when_emu_kill_is_ignored() {
    let sandbox = Sandbox::new();
    sandbox.owned_by(&sandbox.own_tag());
    std::fs::write(sandbox.root.path().join("ignores-emu-kill"), "").unwrap();
    let process = Emulator::start();
    let fields = serde_json::json!({
        "avd": "icm-test-api36",
        "emulator_pid": process.0, "emulator_identity": another_identity()
    });
    sandbox.write_session(fields.clone());
    sandbox.write_booted(fields);

    let events = sandbox.events(
        &["stop", "android", "--shutdown", "--timeout", "2s"],
        &[("FAKE_AVD", "icm-test-api36")],
    );
    let result = events.last().unwrap();
    assert_eq!(result["exit"], 0, "{result}");
    let calls = sandbox.adb_calls();
    assert!(calls.contains("shell am force-stop"), "{calls}");
    assert!(calls.contains("-s emulator-5580 emu kill"), "{calls}");
    assert_eq!(
        result["stopped"],
        serde_json::json!([
            {"platform": "android", "app": "com.acme.fixture", "serial": "emulator-5580"}
        ]),
        "{result}"
    );
    assert_eq!(
        result["still_running"],
        serde_json::json!(["emulator-5580"]),
        "{result}"
    );
    let failed = checks(&events, "android.emulator.shutdown_failed");
    assert_eq!(failed.len(), 1, "{events:?}");
    assert!(
        failed[0]["detail"]
            .as_str()
            .unwrap()
            .contains("no verified process"),
        "{events:?}"
    );
    assert!(
        sandbox
            .sessions()
            .join("android-booted/emulator-5580.json")
            .exists()
    );
    assert!(process.running(), "the recorded pid was signalled");
}

/// A device icm did not boot (the one the run was told to use, a phone or
/// an emulator of the owner's) is not checked against any emulator process:
/// `stop` force-stops the app there, as it always did, without reading an
/// owner or touching the emulator.
#[test]
fn the_app_is_force_stopped_on_a_device_icm_did_not_boot() {
    let sandbox = Sandbox::new();
    sandbox.owned_by("fedcba9876543210");
    sandbox.write_session(serde_json::json!({"booted_by_icm": false, "kind": "device"}));
    let result = sandbox.result(&["stop", "android"]);
    assert_eq!(result["exit"], 0, "{result}");
    let calls = sandbox.adb_calls();
    assert!(
        calls.contains("-s emulator-5580 shell am force-stop"),
        "{calls}"
    );
    assert!(!calls.contains("getprop debug.icm.booted_by"), "{calls}");
    assert!(!calls.contains("emu kill"), "{calls}");
    assert_eq!(
        result["stopped"],
        serde_json::json!([
            {"platform": "android", "app": "com.acme.fixture", "serial": "emulator-5580"}
        ]),
        "{result}"
    );
}

/// A session an older icm wrote (no process identity) on an emulator nothing
/// marks proves nothing by itself: the app is force-stopped there only when
/// the emulator runs one of icm's own AVDs, which `--shutdown` would shut
/// down too. On any other AVD the app is left running, and the INFO says
/// why.
#[test]
fn an_older_session_on_an_unmarked_emulator_needs_a_managed_avd() {
    let sandbox = Sandbox::new();
    let process = Emulator::start();
    sandbox.write_session(serde_json::json!({"emulator_pid": process.0}));

    // icm-api36, nobody's: icm's own.
    let result = sandbox.result(&["stop", "android"]);
    assert_eq!(result["exit"], 0, "{result}");
    assert!(sandbox.adb_calls().contains("shell am force-stop"));
    assert!(!sandbox.adb_calls().contains("emu kill"));
    assert!(process.running());

    // The owner's own AVD on that serial: not icm's to touch.
    std::fs::write(sandbox.log(), "").unwrap();
    sandbox.write_session(serde_json::json!({"emulator_pid": process.0}));
    let events = sandbox.events(&["stop", "android"], &[("FAKE_AVD", "owners_pixel")]);
    let result = events.last().unwrap();
    assert_eq!(result["exit"], 0, "{result}");
    let calls = sandbox.adb_calls();
    assert!(!calls.contains("force-stop"), "{calls}");
    assert!(!calls.contains("emu kill"), "{calls}");
    assert_eq!(result["stopped"], serde_json::json!([]), "{result}");
    let info = checks(&events, "run.no_session");
    assert_eq!(info.len(), 1, "{events:?}");
    assert!(
        info[0]["detail"]
            .as_str()
            .unwrap()
            .starts_with("com.acme.fixture was not force-stopped on emulator-5580"),
        "{events:?}"
    );
    assert!(process.running());
}

/// What `--dry-run` says `stop` does is what it does: the app is force-stopped
/// on an emulator icm booted only after its owner property is read, and not
/// when the property names another project or cannot be read, whatever the
/// record's process is now; `--shutdown` also says what happens to an
/// emulator that ignores `emu kill`.
#[test]
fn the_stop_plan_states_the_rules_stop_follows() {
    let sandbox = Sandbox::new();
    let plan = |args: &[&str]| -> Vec<String> {
        let result = sandbox.result(args);
        assert_eq!(result["exit"], 0, "{result}");
        result["plan"]
            .as_array()
            .unwrap()
            .iter()
            .map(|step| step["display"].as_str().unwrap().to_string())
            .collect()
    };
    let steps = plan(&["stop", "android", "--shutdown", "--dry-run"]);
    assert_eq!(steps.len(), 2, "{steps:?}");
    for phrase in [
        "am force-stop com.acme.fixture",
        "on a device icm did not boot, always",
        "after reading its debug.icm.booted_by",
        "ended or was replaced",
        "names another project or cannot be read",
        "names this project",
        "verified to still run",
        "an icm-* AVD",
    ] {
        assert!(steps[0].contains(phrase), "{phrase}: {}", steps[0]);
    }
    // An ended process is no reason to leave the app alone.
    assert!(!steps[0].contains("has exited"), "{}", steps[0]);
    for phrase in [
        "adb emu kill",
        "same rules as for the app",
        "SIGTERM",
        "only while that is the process icm started",
        "android.emulator.shutdown_failed",
        "not reported as stopped",
    ] {
        assert!(steps[1].contains(phrase), "{phrase}: {}", steps[1]);
    }
    // Nothing touched a device.
    assert_eq!(sandbox.adb_calls(), "");
}

/// `emu kill` can be ignored. With no process icm can verify (no record at
/// all, one an older icm wrote, or one whose identity another process has
/// now) the emulator is still listed when the wait ends and there is nothing
/// to signal: it is not reported as stopped, its record stays, and a WARN
/// names it, with the command that tries again. The pid such a record names
/// is never signalled.
#[test]
fn an_emulator_that_ignores_emu_kill_is_not_reported_stopped() {
    for case in [
        "no record",
        "a record an older icm wrote",
        "a record with another process's identity",
    ] {
        let sandbox = Sandbox::new();
        std::fs::write(sandbox.root.path().join("ignores-emu-kill"), "").unwrap();
        let process = Emulator::start();
        match case {
            "no record" => {}
            "a record an older icm wrote" => {
                sandbox.write_booted(serde_json::json!({"emulator_pid": process.0}));
            }
            _ => sandbox.write_booted(serde_json::json!({
                "emulator_pid": process.0, "emulator_identity": another_identity()
            })),
        }

        // `--timeout` bounds the wait for the emulator to go.
        let events = sandbox.events(&["stop", "android", "--shutdown", "--timeout", "2s"], &[]);
        let result = events.last().unwrap();
        assert_eq!(result["exit"], 0, "{case}: {result}");
        assert!(
            sandbox.adb_calls().contains("-s emulator-5580 emu kill"),
            "{case}"
        );
        assert_eq!(result["stopped"], serde_json::json!([]), "{case}: {result}");
        assert_eq!(
            result["still_running"],
            serde_json::json!(["emulator-5580"]),
            "{case}: {result}"
        );
        assert_eq!(
            result["summary"], "emulator-5580 did not shut down (still running)",
            "{case}: {result}"
        );
        let failed = checks(&events, "android.emulator.shutdown_failed");
        assert_eq!(failed.len(), 1, "{case}: {events:?}");
        assert_eq!(failed[0]["status"], "warn", "{case}");
        let detail = failed[0]["detail"].as_str().unwrap();
        assert!(
            detail.starts_with("emulator-5580 is still running: it ignored `adb emu kill`")
                && detail.contains("no verified process"),
            "{case}: {detail}"
        );
        assert_eq!(
            failed[0]["fix"]["commands"],
            serde_json::json!(["adb -s emulator-5580 emu kill"]),
            "{case}"
        );
        assert_eq!(
            sandbox
                .sessions()
                .join("android-booted/emulator-5580.json")
                .exists(),
            case != "no record",
            "{case}: the record of an emulator that is still up was deleted"
        );
        assert!(process.running(), "{case}: an unverified pid was signalled");
    }
}

/// A verified emulator process that ignores `emu kill` gets SIGTERM when the
/// wait ends, and is reported stopped once that ended it.
#[test]
fn a_lingering_verified_emulator_is_ended_by_sigterm() {
    let sandbox = Sandbox::new();
    std::fs::write(sandbox.root.path().join("ignores-emu-kill"), "").unwrap();
    let process = Emulator::start();
    sandbox.write_booted(serde_json::json!({
        "emulator_pid": process.0, "emulator_identity": process.identity()
    }));

    let events = sandbox.events(&["stop", "android", "--shutdown", "--timeout", "2s"], &[]);
    let result = events.last().unwrap();
    assert_eq!(result["exit"], 0, "{result}");
    assert_eq!(
        result["stopped"],
        serde_json::json!([{"platform": "android", "emulator": "emulator-5580"}]),
        "{result}"
    );
    assert_eq!(result["still_running"], serde_json::json!([]), "{result}");
    assert!(checks(&events, "android.emulator.shutdown_failed").is_empty());
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while process.running() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!process.running(), "SIGTERM did not reach the process");
    assert!(
        !sandbox
            .sessions()
            .join("android-booted/emulator-5580.json")
            .exists()
    );
}

/// Plain `stop` says why it left the app running on another project's
/// emulator, as `--shutdown` says why it left the emulator: it read the
/// device's owner, force-stopped nothing, and the INFO names the owner and
/// the command that stops the app by hand, once, whichever of `stop` and
/// `stop --all` ran it.
#[test]
fn plain_stop_says_why_it_leaves_another_projects_emulator() {
    for args in [&["stop", "android"][..], &["stop", "--all"][..]] {
        let sandbox = Sandbox::new();
        sandbox.owned_by("fedcba9876543210");
        let process = Emulator::start();
        sandbox.write_session(serde_json::json!({
            "emulator_pid": process.0, "emulator_identity": process.identity()
        }));

        let events = sandbox.events(args, &[]);
        let result = events.last().unwrap();
        assert_eq!(result["exit"], 0, "{args:?}: {result}");
        let calls = sandbox.adb_calls();
        assert!(
            calls.contains("-s emulator-5580 shell getprop debug.icm.booted_by"),
            "{args:?}: {calls}"
        );
        assert!(!calls.contains("force-stop"), "{args:?}: {calls}");
        assert!(!calls.contains("emu kill"), "{args:?}: {calls}");
        assert_eq!(
            result["stopped"],
            serde_json::json!([]),
            "{args:?}: {result}"
        );
        let shared = checks(&events, "android.emulator.shared");
        assert_eq!(shared.len(), 1, "{args:?}: {events:?}");
        assert_eq!(shared[0]["status"], "info", "{args:?}");
        let detail = shared[0]["detail"].as_str().unwrap();
        assert!(
            detail.starts_with("emulator-5580 kept running with com.acme.fixture on it")
                && detail.contains("fedcba9876543210"),
            "{args:?}: {detail}"
        );
        assert_eq!(
            shared[0]["fix"]["commands"],
            serde_json::json!(["adb -s emulator-5580 shell am force-stop com.acme.fixture"]),
            "{args:?}"
        );
        assert!(process.running());
    }
}

/// `run` on an emulator that is already up carries an earlier booted record
/// forward, and so claims the emulator with `debug.icm.booted_by`, only when
/// the record's process verifies: a pid an older icm recorded without an
/// identity, one another process has taken and one that has exited are not
/// the emulator icm booted, and a rerun must not claim another project's
/// (or somebody's) emulator on their account. The run stops at the NDK check
/// after that decision, so the per-serial record is what shows it.
#[test]
fn a_rerun_carries_an_earlier_booted_record_forward_only_when_it_verifies() {
    for case in [
        "a verified process",
        "a session an older icm wrote",
        "another process's identity",
        "an exited process",
    ] {
        let sandbox = Sandbox::new();
        let process = Emulator::start();
        let identity = process.identity();
        let fields = match case {
            "a verified process" => {
                serde_json::json!({"emulator_pid": process.0, "emulator_identity": identity})
            }
            "a session an older icm wrote" => serde_json::json!({"emulator_pid": process.0}),
            "another process's identity" => {
                serde_json::json!({"emulator_pid": process.0, "emulator_identity": another_identity()})
            }
            _ => {
                process.end();
                serde_json::json!({"emulator_pid": process.0, "emulator_identity": identity})
            }
        };
        sandbox.write_session(fields);

        let result = sandbox.result(&["run", "android"]);
        // The fake SDK has no NDK: an environment failure, after the device
        // was chosen and the earlier emulator carried forward (or not).
        assert_eq!(result["exit"], 4, "{case}: {result}");
        assert!(
            result["errors"][0]["id"]
                .as_str()
                .unwrap()
                .starts_with("env."),
            "{case}: {result}"
        );
        let record = sandbox.sessions().join("android-booted/emulator-5580.json");
        if case == "a verified process" {
            let written: Value =
                serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
            assert_eq!(written["serial"], "emulator-5580", "{case}");
            assert_eq!(written["avd"], "icm-api36", "{case}");
            assert_eq!(written["emulator_pid"], process.0, "{case}");
            // The start time is the identity; the program may differ, as
            // the stand-in's shell becomes `sleep` after it is read.
            assert_eq!(
                written["emulator_identity"]["start"], identity["start"],
                "{case}: the identity was dropped"
            );
        } else {
            assert!(!record.exists(), "{case}: an emulator was claimed");
        }
    }
}

/// An emulator icm boots is recorded, in the session and in the per-serial
/// record, with the identity of its process: everything later compares the
/// pid with it, and a record without one proves nothing (a rerun would not
/// carry it forward, `stop` could never signal it). The run stops at the NDK
/// check, after the boot has been recorded.
#[test]
fn a_boot_records_the_emulators_identity() {
    let sandbox = Sandbox::new();
    // No device is online, so the named AVD is booted.
    std::fs::write(sandbox.root.path().join("killed"), "").unwrap();
    let avd_home = sandbox.root.path().join("sdk/avd");
    std::fs::create_dir_all(avd_home.join("icm-test-api36.avd")).unwrap();
    std::fs::write(
        avd_home.join("icm-test-api36.ini"),
        format!(
            "path={}\ntarget=android-36\n",
            avd_home.join("icm-test-api36.avd").display()
        ),
    )
    .unwrap();
    std::fs::write(
        avd_home.join("icm-test-api36.avd/config.ini"),
        "abi.type=arm64-v8a\n",
    )
    .unwrap();
    // An emulator that is a long `sleep` (its launcher execs it, as the real
    // one does qemu), on a console port nothing uses. icm starts it
    // detached and goes on, so the script writes its pid on its own time:
    // it takes a while to (the test reads the file by polling for it, and
    // the delay makes sure it has to) and writes it atomically.
    let pidfile = sandbox.root.path().join("emulator-started");
    let emulator = sandbox.root.path().join("sdk/emulator/emulator");
    std::fs::create_dir_all(emulator.parent().unwrap()).unwrap();
    std::fs::write(
        &emulator,
        format!(
            "#!/bin/sh\nsleep 0.5\necho $$ > '{file}.tmp'\nmv '{file}.tmp' '{file}'\nexec sleep 120\n",
            file = pidfile.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&emulator, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _guard = Started(pidfile.clone());
    let ports = free_console_ports(4);
    std::fs::write(
        sandbox.root.path().join("host.toml"),
        format!(
            "android_sdk = \"{}\"\n[android]\nemulator_ports = [{}]\n",
            sandbox.root.path().join("sdk").display(),
            ports
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
    .unwrap();

    let result = sandbox.result(&["run", "android", "--avd", "icm-test-api36"]);
    assert_eq!(result["exit"], 4, "{result}");
    assert!(
        result["errors"][0]["id"]
            .as_str()
            .unwrap()
            .starts_with("env."),
        "{result}"
    );
    let pid = written_pid(&pidfile, std::time::Duration::from_secs(20))
        .expect("the fake emulator never wrote its pid");
    let identity = serde_json::to_value(icm::procid::of(pid as i32).unwrap()).unwrap();

    let session: Value = serde_json::from_str(
        &std::fs::read_to_string(sandbox.sessions().join("android.json")).unwrap(),
    )
    .unwrap();
    // The first of the ports that was free when icm looked.
    let serial = session["serial"].as_str().unwrap().to_string();
    assert!(
        ports
            .iter()
            .any(|port| serial == format!("emulator-{port}")),
        "{serial} is none of {ports:?}"
    );
    let booted: Value = serde_json::from_str(
        &std::fs::read_to_string(
            sandbox
                .sessions()
                .join(format!("android-booted/{serial}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    for (what, record) in [("the session", &session), ("the booted record", &booted)] {
        assert_eq!(record["serial"], serial.as_str(), "{what}");
        assert_eq!(record["emulator_pid"], pid, "{what}");
        // The start time is the identity; the program may differ (the
        // launcher's shell becomes `sleep` after it is read).
        assert_eq!(
            record["emulator_identity"]["start"], identity["start"],
            "{what} has no identity of the emulator's process"
        );
    }
    assert_eq!(session["booted_by_icm"], true);
}

#[test]
fn another_avd_is_never_shut_down() {
    let sandbox = Sandbox::new();
    std::fs::write(
        sandbox.root.path().join("host.toml"),
        format!(
            "android_sdk = \"{}\"\n[android]\navd = \"someone_elses\"\n",
            sandbox.root.path().join("sdk").display()
        ),
    )
    .unwrap();
    let result = sandbox.result(&["stop", "android", "--shutdown"]);
    assert_eq!(result["exit"], 0, "{result}");
    assert!(!sandbox.adb_calls().contains("emu kill"));
}

#[test]
fn a_test_runs_avd_is_never_shut_down() {
    // An `icm-test-` AVD belongs to the test run that made it: even as the
    // configured AVD, `--shutdown` leaves it running unless icm booted it.
    let sandbox = Sandbox::new();
    std::fs::write(
        sandbox.root.path().join("host.toml"),
        format!(
            "android_sdk = \"{}\"\n[android]\navd = \"icm-test-api36\"\n",
            sandbox.root.path().join("sdk").display()
        ),
    )
    .unwrap();
    let result = sandbox.result_with(
        &["stop", "android", "--shutdown"],
        &[("FAKE_AVD", "icm-test-api36")],
    );
    assert_eq!(result["exit"], 0, "{result}");
    assert!(!sandbox.adb_calls().contains("emu kill"));
}

#[test]
fn logs_need_a_launch_mark_or_a_duration() {
    let sandbox = Sandbox::new();
    let result = sandbox.result(&["logs", "android"]);
    assert_eq!(result["exit"], 7, "{result}");
    assert_eq!(result["errors"][0]["id"], "run.no_session");

    let result = sandbox.result(&["logs", "android", "--since", "soon"]);
    assert_eq!(result["exit"], 2, "{result}");
}

impl Sandbox {
    /// The text of an artifact a result names.
    fn artifact(&self, result: &Value, kind: &str) -> String {
        let path = PathBuf::from(result["artifacts"][kind].as_str().unwrap());
        let path = if path.is_absolute() {
            path
        } else {
            self.project.join(path)
        };
        std::fs::read_to_string(path).unwrap()
    }
}

/// The log files a command keeps are redacted as its stdout is: a value of
/// a secret-named variable in icm's environment that the app logged
/// (`--json` printed `<redacted>`) stays out of `logcat.txt`, `logs.ndjson`
/// and `app.log`, a JSON-escaped one included.
#[test]
fn log_files_keep_no_secret() {
    let sandbox = Sandbox::new();
    std::fs::write(
        sandbox.root.path().join("logcat.txt"),
        "--------- beginning of main
1791333501.000  4321  4350 I iced: signed in with tok-sekrit-123456
1791333501.100  4321  4350 I iced: {\"token\":\"tok-sekrit-123456\"}
",
    )
    .unwrap();

    let output = sandbox.run(
        &["logs", "android", "--since", "15m", "--json"],
        &[("ICM_TEST_API_TOKEN", "tok-sekrit-123456")],
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let result: Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
    assert_eq!(result["exit"], 0, "{result}");
    assert_eq!(result["records"][0]["msg"], "signed in with <redacted>");
    assert!(!stdout.contains("tok-sekrit-123456"));
    for kind in ["logcat", "logs", "app_log"] {
        let text = sandbox.artifact(&result, kind);
        assert!(!text.contains("tok-sekrit-123456"), "{kind}: {text}");
        assert!(text.contains("signed in with <redacted>"), "{kind}: {text}");
    }
    let logs = sandbox.artifact(&result, "logs");
    let second: Value = serde_json::from_str(logs.lines().nth(1).unwrap()).unwrap();
    assert_eq!(second["msg"], "{\"token\":\"<redacted>\"}");
}

/// The app's records are those its processes wrote while they were its
/// own, whatever their tags: another iced_mobile app writes `ICM_EVENT` and
/// `iced` lines too (the events opt-in is a system property), and a pid the
/// app had before can belong to another process now.
#[test]
fn logs_are_the_apps_processes() {
    let sandbox = Sandbox::new();
    let state = sandbox.root.path();
    // The app is not running now: no pid to start from.
    std::fs::write(state.join("pidof"), "").unwrap();
    // pid 4321 was the app's from 3500 to 3600, then another app's; pid
    // 7777 is another iced_mobile app.
    std::fs::write(
        state.join("events.txt"),
        "1791333500.000   600   610 I am_proc_start: [0,4321,10123,com.acme.fixture,next-top-activity,{com.acme.fixture/android.app.NativeActivity}]
1791333600.000   600   610 I am_proc_died: [0,4321,com.acme.fixture,900,19]
1791333700.000   600   610 I am_proc_start: [0,4321,10200,com.other.app,activity,{com.other.app/.Main}]
1791333710.000   600   610 I am_proc_start: [0,7777,10201,com.other.iced,activity,{com.other.iced/android.app.NativeActivity}]
",
    )
    .unwrap();
    std::fs::write(
        state.join("logcat.txt"),
        "--------- beginning of main
1791333500.500  4321  4321 I ICM_EVENT: {\"v\":1,\"kind\":\"start\",\"protocol\":1,\"pid\":4321,\"platform\":\"android\"}
1791333501.000  4321  4350 I iced: the app's own line
1791333501.200  4321  4350 I ICM_EVENT: {\"v\":1,\"kind\":\"ready\",\"ms\":700}
1791333701.000  4321  4321 I iced: another app's process with the old pid
1791333711.000  7777  7777 I ICM_EVENT: {\"v\":1,\"kind\":\"ready\",\"ms\":90}
1791333711.100  7777  7790 W iced: another iced_mobile app
1791333711.200  7777  7790 E RustStdoutStderr: thread 'main' panicked at other/src/lib.rs:1:1:
",
    )
    .unwrap();

    let result = sandbox.result(&["logs", "android", "--since", "15m"]);
    assert_eq!(result["exit"], 0, "{result}");
    let records = result["records"].as_array().unwrap();
    let messages: Vec<&str> = records
        .iter()
        .map(|record| record["msg"].as_str().unwrap())
        .collect();
    assert_eq!(records.len(), 3, "{messages:?}");
    assert!(
        records.iter().all(|record| record["pid"] == 4321),
        "{messages:?}"
    );
    assert!(messages.contains(&"the app's own line"), "{messages:?}");
    let app_log = sandbox.artifact(&result, "app_log");
    assert!(!app_log.contains("another"), "{app_log}");
    assert_eq!(sandbox.artifact(&result, "logs").lines().count(), 3);
}

#[test]
fn run_fails_before_building_when_the_sdk_lacks_pieces() {
    let sandbox = Sandbox::new();
    let result = sandbox.result(&["run", "android"]);
    // The fake SDK has no NDK, build-tools or platform; whichever the
    // preflight meets first, it is an environment failure (exit 4) that
    // names its fix, before cargo runs.
    assert_eq!(result["exit"], 4, "{result}");
    let id = result["errors"][0]["id"].as_str().unwrap();
    assert!(id.starts_with("env."), "{result}");
    assert!(
        !result["errors"][0]["fix"]["commands"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(result["device"], Value::Null);

    // --from-aab installs the newest release's bundle: without one it says
    // how to make it, before choosing a device.
    let result = sandbox.result(&["run", "android", "--from-aab"]);
    assert_eq!(result["exit"], 2, "{result}");
    assert_eq!(result["errors"][0]["id"], "release.not_found");
    assert!(
        result["errors"][0]["fix"]["commands"][0]
            .as_str()
            .unwrap()
            .starts_with("icm release android --sign none")
    );
    assert!(!sandbox.adb_calls().contains("install"));
}

#[test]
fn the_lifecycle_suite_and_from_aab_plan_without_a_device() {
    let sandbox = Sandbox::new();
    for args in [
        &["test", "android", "--lifecycle", "--dry-run"][..],
        &["test", "--on", "android", "--lifecycle", "--dry-run"][..],
    ] {
        let result = sandbox.result(args);
        assert_eq!(result["exit"], 0, "{result}");
        assert_eq!(result["dry_run"], true);
        let names: Vec<&str> = result["plan"]
            .as_array()
            .unwrap()
            .iter()
            .map(|step| step["name"].as_str().unwrap())
            .collect();
        for name in [
            "android.launch",
            "lifecycle.dark-mode",
            "lifecycle.landscape",
            "lifecycle.font-scale",
            "lifecycle.home",
            "lifecycle.home-relaunch",
            "lifecycle.back",
            "lifecycle.kill",
            "lifecycle.kill-relaunch",
            "lifecycle.restore",
        ] {
            assert!(names.contains(&name), "{name} not in {names:?}");
        }
    }
    // `--on` and the positional platform are one choice.
    let both = sandbox.result(&["test", "android", "--on", "android", "--lifecycle"]);
    assert_eq!(both["exit"], 2, "{both}");

    let result = sandbox.result(&["run", "android", "--from-aab", "--dry-run"]);
    assert_eq!(result["exit"], 0, "{result}");
    let names: Vec<&str> = result["plan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"bundletool.install"), "{names:?}");
    assert!(!names.contains(&"adb.install"), "{names:?}");
    assert!(
        !names.iter().any(|name| name.starts_with("cargo")),
        "{names:?}"
    );

    // Nothing touched a device.
    assert_eq!(sandbox.adb_calls(), "");
}

#[test]
fn several_devices_are_ambiguous() {
    let sandbox = Sandbox::new();
    // A second, non-icm emulator: the default AVD's emulator still wins.
    let adb = sandbox.root.path().join("sdk/platform-tools/adb");
    let script = FAKE_ADB.replace(
        "  exit 0\nfi",
        "  echo \"R58M123 device usb:1-1 model:phone transport_id:2\"\n  exit 0\nfi",
    );
    std::fs::write(&adb, script).unwrap();
    let result = sandbox.result(&["input", "android", "key", "home"]);
    assert_eq!(result["exit"], 0, "{result}");
    assert_eq!(result["device"]["serial"], "emulator-5580");

    // Without icm's AVD among them, two online devices are ambiguous.
    std::fs::write(
        sandbox.root.path().join("host.toml"),
        format!(
            "android_sdk = \"{}\"\n[android]\navd = \"icm-other\"\n",
            sandbox.root.path().join("sdk").display()
        ),
    )
    .unwrap();
    let result = sandbox.result(&["input", "android", "key", "home"]);
    assert_eq!(result["exit"], 7, "{result}");
    assert_eq!(result["errors"][0]["id"], "android.device.ambiguous");

    // adb's own variable picks one.
    let result = sandbox.result_with(
        &["input", "android", "key", "home"],
        &[("ANDROID_SERIAL", "R58M123")],
    );
    assert_eq!(result["exit"], 0, "{result}");
    assert_eq!(result["device"]["serial"], "R58M123");
}
