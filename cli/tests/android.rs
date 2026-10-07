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
/// to `$FAKE_ADB_LOG`; `emu kill` removes the emulator.
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
    if [ "$2" = "kill" ]; then touch "$state_dir/killed"; echo OK; exit 0; fi
    echo "${FAKE_AVD:-icm-api36}"; echo "OK"; exit 0 ;;
  shell)
    case "$2" in
      "getprop ro.product.cpu.abi") echo "arm64-v8a" ;;
      "getprop ro.build.version.sdk") echo "36" ;;
      getprop*) echo "" ;;
      "wm density") echo "Physical density: 420" ;;
      "wm size") echo "Physical size: 1080x2400" ;;
      "dumpsys window displays") echo "  init=1080x2400 420dpi cur=1080x2400 app=1080x2337" ;;
      pidof*) echo "4321" ;;
      "date +%s.%N") echo "1791334000.123456789" ;;
      *) ;;
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
