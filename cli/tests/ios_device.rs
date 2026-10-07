//! `icm build|run|logs|shot|stop|devices|input ios-device` against fake
//! tools (`tests/fixtures/fake-ios/`): a fake `xcrun devicectl` that lists
//! one physical device, installs, launches (the "app" prints `ICM_EVENT`
//! lines and sleeps) and captures a screenshot; fake `cargo`, `codesign`,
//! `security`, `xcodebuild`; a fake development profile. No device,
//! certificate or keychain is touched. macOS only (the real `plutil`).
#![cfg(target_os = "macos")]

use icm::ios::{macho, profile, sha1};
use icm::platform::ios_device::devicectl;
use icm::platform::ios_sim::image;
use icm::platform::ios_sim::macho::PLATFORM_IOS;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/secret.rs"]
mod secret;

const BIN: &str = env!("CARGO_BIN_EXE_icm");
const TEAM: &str = "ABCDE12345";
const UDID: &str = "00008150-001A2B3C4D5E6F70";
const CERT: &[u8] = b"the DER certificate of Jo's Apple Development identity";
const IDENTITY: &str = "Apple Development: Jo Doe (ABCDE12345)";

const SCRUB: &[&str] = &[
    "ICM_JSON",
    "ICM_CONFIG",
    "ICM_TIMEOUT",
    "ICM_RUN_ID",
    "ICM_RUN_DIR",
    "ICM_RUN_ROOT",
    "ICM_DETACHED",
    "ICM_KEYCHAIN",
    "ICM_PROVISIONING_PROFILES",
    "ICM_CODESIGN_TIMEOUT",
];

struct Device {
    root: tempfile::TempDir,
    env: Vec<(String, String)>,
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

fn write_plist(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, icm::platform::ios_sim::plist::to_xml(value)).unwrap();
}

impl Device {
    fn new() -> Device {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("app");
        copy_dir(&fixtures().join("release"), &app);
        let config = app.join("icm.toml");
        let mut text = std::fs::read_to_string(&config).unwrap();
        text.push_str(&format!("\n[ios]\nteam_id = \"{TEAM}\"\n"));
        std::fs::write(&config, text).unwrap();

        let state = root.path().join("state");
        std::fs::create_dir_all(state.join("sysroot/lib/rustlib/aarch64-apple-ios/lib")).unwrap();
        let developer = state.join("Xcode.app/Contents/Developer");
        std::fs::create_dir_all(developer.join("usr/bin")).unwrap();
        write_plist(
            &state.join("Xcode.app/Contents/Info.plist"),
            &json!({"DTXcode": "2700"}),
        );
        write_plist(
            &developer.join("Platforms/iPhoneOS.platform/Info.plist"),
            &json!({"DefaultProperties": {"DEFAULT_COMPILER": "com.apple.compilers.llvm.clang.1_0"}}),
        );
        std::fs::write(
            state.join("exe"),
            macho::synthetic(
                PLATFORM_IOS,
                (16, 0, 0),
                (27, 0, 0),
                [3; 16],
                &["_stat"],
                b"",
            ),
        )
        .unwrap();
        let mut screen = image::Rgba::filled(1206, 2622, [255, 255, 255, 255]);
        for y in 300..420 {
            for x in 100..1100 {
                screen.set(x, y, [80, 90, 240, 255]);
            }
        }
        image::write_png(&state.join("screen.png"), &screen, false).unwrap();
        std::fs::write(
            state.join("identities.txt"),
            format!("  1) {} \"{IDENTITY}\"\n", sha1::hex_upper(CERT)),
        )
        .unwrap();
        std::fs::create_dir_all(state.join("profiles")).unwrap();

        let device = Device {
            root,
            env: vec![("ICM_TODAY".into(), "2026-10-07".into())],
        };
        device.devices(&[devicectl::fixture_device("Jo's iPhone", UDID, true, true)]);
        device.profile(&[UDID]);
        device
    }

    fn state(&self) -> PathBuf {
        self.root.path().join("state")
    }

    fn dir(&self) -> PathBuf {
        self.root.path().join("app")
    }

    fn devices(&self, devices: &[Value]) {
        std::fs::write(
            self.state().join("devicectl-devices.json"),
            json!({"info": {"outcome": "success"}, "result": {"devices": devices}}).to_string(),
        )
        .unwrap();
    }

    fn profile(&self, devices: &[&str]) {
        let fixture = profile::Fixture {
            name: "Fixture Development".into(),
            uuid: "D11C296F-D9CC-48F3-8624-50ECE5C568E2".into(),
            team: TEAM.into(),
            app_id: "*".into(),
            certificates: vec![CERT.to_vec()],
            devices: devices.iter().map(ToString::to_string).collect(),
            get_task_allow: true,
            expires: "2027-06-30T12:00:00Z".into(),
        };
        std::fs::write(
            self.state().join("profiles/dev.mobileprovision"),
            fixture.bytes(),
        )
        .unwrap();
    }

    fn set(&mut self, key: &str, value: &str) {
        self.env.push((key.into(), value.into()));
    }

    fn json(&self, args: &[&str]) -> Value {
        let fakes = fixtures().join("fake-ios");
        let home = std::env::var("HOME").unwrap_or_default();
        let state = self.state();
        let mut command = Command::new(BIN);
        for var in SCRUB {
            let _ = command.env_remove(var);
        }
        let _ = command
            .args(args)
            .args(["--json", "-q"])
            .current_dir(self.dir())
            .stdin(Stdio::null())
            .env("ICM_CACHE_DIR", self.root.path().join("cache"))
            .env("ICM_HOST_CONFIG", self.root.path().join("no-host.toml"))
            .env("CARGO_TARGET_DIR", self.dir().join("target"))
            .env("HOME", self.root.path().join("home"))
            .env(
                "CARGO_HOME",
                std::env::var("CARGO_HOME").unwrap_or(format!("{home}/.cargo")),
            )
            .env(
                "RUSTUP_HOME",
                std::env::var("RUSTUP_HOME").unwrap_or(format!("{home}/.rustup")),
            )
            .env(
                "ICM_REAL_CARGO",
                std::env::var("CARGO").unwrap_or("cargo".into()),
            )
            .env("ICM_FAKE_STATE", &state)
            .env("ICM_FAKE_BIN", "release-app")
            .env("ICM_FAKE_IDENTITY_NAME", IDENTITY)
            .env("ICM_FAKE_TEAM", TEAM)
            .env("DEVELOPER_DIR", state.join("Xcode.app/Contents/Developer"))
            .env("ICM_PROVISIONING_PROFILES", state.join("profiles"))
            .env("ICM_TOOL_XCRUN", fakes.join("xcrun"))
            .env("ICM_TOOL_CARGO", fakes.join("cargo"))
            .env("ICM_TOOL_RUSTC", fakes.join("rustc"))
            .env("ICM_TOOL_RUSTUP", fakes.join("rustup"))
            .env("ICM_TOOL_XCODEBUILD", fakes.join("xcodebuild"))
            .env("ICM_TOOL_CODESIGN", fakes.join("codesign"))
            .env("ICM_TOOL_SECURITY", fakes.join("security"))
            .env("ICM_TOOL_SW_VERS", fakes.join("sw_vers"))
            .env("ICM_TOOL_XATTR", fakes.join("ok"));
        for (key, value) in &self.env {
            let _ = command.env(key, value);
        }
        let output = command.output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        let last = text.lines().last().unwrap_or_else(|| {
            panic!(
                "no output; stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        let result: Value = serde_json::from_str(last).unwrap();
        assert_eq!(result["type"], "result");
        assert_eq!(
            output.status.code().map(i64::from),
            result["exit"].as_i64(),
            "{result}"
        );
        result
    }

    fn abs(&self, value: &Value) -> PathBuf {
        let path = PathBuf::from(
            value
                .as_str()
                .unwrap_or_else(|| panic!("not a path: {value}")),
        );
        if path.is_absolute() {
            path
        } else {
            self.dir().join(path)
        }
    }

    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.state().join(name)).unwrap_or_default()
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        if let Ok(pid) = std::fs::read_to_string(self.state().join("device-app.pid")) {
            let _ = Command::new("kill").arg(pid.trim()).status();
        }
        if let Ok(text) =
            std::fs::read_to_string(self.dir().join("target/icm/sessions/ios-device.json"))
            && let Ok(session) = serde_json::from_str::<Value>(&text)
            && let Some(pid) = session["pid"].as_i64()
        {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
    }
}

#[test]
fn run_logs_shot_and_stop_on_a_device() {
    let device = Device::new();
    let run = device.json(&["run", "ios-device", "--settle", "0s"]);
    assert_eq!(run["exit"], 0, "{run}");
    assert_eq!(run["device"]["udid"], UDID);
    assert_eq!(run["device"]["name"], "Jo's iPhone");
    assert_eq!(run["process"]["ready"]["source"], "icm_event");
    assert_eq!(run["process"]["pid"], 4242);
    assert_eq!(run["screen"]["px"], json!([1206, 2622]));
    assert!(device.abs(&run["artifacts"]["preview"]).is_file());
    assert_eq!(run["checks"]["failed"], json!([]), "{run}");

    // Signed with the development identity and get-task-allow; the profile
    // is embedded.
    let bundle = device.abs(&run["artifacts"]["bundle"]);
    assert!(bundle.ends_with("target/icm/build/ios-device/debug/Fixture.app"));
    assert!(bundle.join("embedded.mobileprovision").is_file());
    let plist = std::fs::read_to_string(bundle.join("Info.plist")).unwrap();
    assert!(plist.contains("<string>iPhoneOS</string>"));
    assert!(plist.contains("<key>DTXcodeBuild</key>"));
    let codesign = device.log("codesign.log");
    assert!(
        codesign.contains(&format!(
            "--force --sign {} --entitlements",
            sha1::hex_upper(CERT)
        )),
        "{codesign}"
    );
    let signed = device.log("signed-entitlements.plist");
    assert!(
        signed.contains("<key>get-task-allow</key>\n\t<true/>"),
        "{signed}"
    );

    // Installed, then launched with the events opted in.
    let xcrun = device.log("xcrun.log");
    assert!(
        xcrun.contains(&format!("devicectl device install app --device {UDID} ")),
        "{xcrun}"
    );
    assert!(xcrun.contains(&format!(
        "devicectl device process launch --device {UDID} --terminate-existing --console com.acme.fixture"
    )));
    assert!(
        xcrun.contains("  env DEVICECTL_CHILD_ICM_EVENTS=1"),
        "{xcrun}"
    );
    assert!(
        xcrun.contains("  env DEVICECTL_CHILD_RUST_BACKTRACE=1"),
        "{xcrun}"
    );

    // The session record names the stop command.
    let session: Value = serde_json::from_str(
        &std::fs::read_to_string(device.dir().join("target/icm/sessions/ios-device.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(session["platform"], "ios-device");
    assert_eq!(
        session["stop"][0],
        json!([
            "xcrun",
            "devicectl",
            "device",
            "process",
            "terminate",
            "--device",
            UDID,
            "--pid",
            "4242"
        ])
    );

    let logs = device.json(&["logs", "ios-device"]);
    assert_eq!(logs["exit"], 0, "{logs}");
    assert!(logs["counts"]["total"].as_u64().unwrap() >= 2, "{logs}");
    assert!(
        logs["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["tag"] == "ICM_EVENT" && r["platform"] == "ios-device"),
        "{logs}"
    );

    let shot = device.json(&["shot", "ios-device", "--name", "again"]);
    assert_eq!(shot["exit"], 0, "{shot}");
    assert!(
        device
            .abs(&shot["artifacts"]["screenshot"])
            .ends_with("again.png")
    );

    let ps = device.json(&["ps"]);
    assert_eq!(ps["exit"], 0, "{ps}");

    let stop = device.json(&["stop", "ios-device"]);
    assert_eq!(stop["exit"], 0, "{stop}");
    assert!(device.log("xcrun.log").contains(&format!(
        "devicectl device process terminate --device {UDID} --pid 4242"
    )));
    assert!(
        !device
            .dir()
            .join("target/icm/sessions/ios-device.json")
            .exists()
    );
}

#[test]
fn a_panic_on_the_device_is_reported() {
    let mut device = Device::new();
    device.set("ICM_FAKE_SCENARIO", "panic");
    let run = device.json(&["run", "ios-device"]);
    assert_eq!(run["exit"], 10, "{run}");
    assert_eq!(run["errors"][0]["id"], "run.app_panicked");
    assert!(
        run["errors"][0]["evidence"][0]["excerpt"]
            .as_str()
            .unwrap()
            .contains("panicked at src/lib.rs:7:5")
    );
}

/// What a command keeps in its run directory holds no secret the app
/// logged on the device's console: the value of a secret-named variable in
/// icm's environment, logged plain, as JSON, in an `ICM_EVENT` and in a
/// panic, is `<redacted>` in the console's copy (which the evidence names),
/// `app.log`, `logs.ndjson`, events and results. The session's console in
/// `target/icm/sessions` is the app's own output and keeps it.
#[test]
fn run_directories_keep_no_secret() {
    let mut device = Device::new();
    device.set(secret::NAME, secret::TOKEN);
    device.set("ICM_FAKE_SCENARIO", "leak");
    let icm = device.dir().join("target/icm");

    let run = device.json(&["run", "ios-device", "--settle", "0s"]);
    assert_eq!(run["exit"], 0, "{run}");
    let app_log = std::fs::read_to_string(device.abs(&run["artifacts"]["app_log"])).unwrap();
    for line in [
        "signed in with <redacted>",
        "{\"token\":\"<redacted>\"}",
        "token <redacted>",
    ] {
        assert!(app_log.contains(line), "{line}: {app_log}");
    }
    let live = icm
        .join("sessions/ios-device")
        .join(run["run"].as_str().unwrap());
    assert!(secret::holds(&live.join("console.log")));

    let logs = device.json(&["logs", "ios-device"]);
    assert_eq!(logs["exit"], 0, "{logs}");
    assert!(
        logs["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["msg"] == "signed in with <redacted>"),
        "{logs}"
    );
    assert_eq!(device.json(&["stop", "ios-device"])["exit"], 0);

    device.set("ICM_FAKE_SCENARIO", "leak-panic");
    let died = device.json(&["run", "ios-device"]);
    assert_eq!(died["exit"], 10, "{died}");
    assert_eq!(died["errors"][0]["id"], "run.app_panicked");
    let evidence = device.abs(&died["errors"][0]["evidence"][0]["path"]);
    assert!(
        evidence.starts_with(device.abs(&died["run_dir"])),
        "{evidence:?}"
    );
    let console = std::fs::read_to_string(&evidence).unwrap();
    assert!(console.contains("rejected token <redacted>"), "{console}");

    secret::assert_kept_nowhere(&icm.join("runs"));
    let last = std::fs::read_to_string(icm.join("last.json")).unwrap();
    assert!(
        secret::forms()
            .iter()
            .all(|form| !last.contains(form.as_str()))
    );
}

#[test]
fn devices_signing_and_input_failures_name_who_acts() {
    let device = Device::new();
    let listed = device.json(&["devices", "ios-device"]);
    assert_eq!(listed["exit"], 0, "{listed}");
    assert_eq!(
        listed["platforms"]["ios-device"]["devices"][0]["udid"],
        UDID
    );

    // No connected device; two; Developer Mode off.
    device.devices(&[devicectl::fixture_device("Jo's iPhone", UDID, false, true)]);
    let none = device.json(&["run", "ios-device"]);
    assert_eq!(none["exit"], 7, "{none}");
    assert_eq!(none["errors"][0]["id"], "ios.device.not_found");
    device.devices(&[
        devicectl::fixture_device("Jo's iPhone", UDID, true, true),
        devicectl::fixture_device("Test iPad", "00008020-0011223344556677", true, true),
    ]);
    let two = device.json(&["run", "ios-device"]);
    assert_eq!(two["errors"][0]["id"], "ios.device.ambiguous", "{two}");
    device.devices(&[devicectl::fixture_device("Jo's iPhone", UDID, true, false)]);
    let off = device.json(&["run", "ios-device"]);
    assert_eq!(off["exit"], 9, "{off}");
    assert_eq!(off["errors"][0]["id"], "ios.device.developer_mode_off");

    // A profile without the device: the owner's, before any build.
    device.devices(&[devicectl::fixture_device("Jo's iPhone", UDID, true, true)]);
    device.profile(&["00008020-0011223344556677"]);
    let mismatch = device.json(&["run", "ios-device"]);
    assert_eq!(mismatch["exit"], 9, "{mismatch}");
    assert_eq!(mismatch["errors"][0]["id"], "ios.sign.profile_mismatch");
    assert!(
        !device
            .log("cargo.log")
            .lines()
            .any(|l| l.starts_with("build"))
    );

    let input = device.json(&["input", "ios-device", "tap", "10", "10"]);
    assert_eq!(input["exit"], 2, "{input}");
    assert_eq!(input["errors"][0]["id"], "input.unsupported");
}

#[test]
fn dry_runs_touch_no_device() {
    let device = Device::new();
    let run = device.json(&["run", "ios-device", "--dry-run"]);
    assert_eq!(run["exit"], 0, "{run}");
    let steps: Vec<&str> = run["plan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    for step in [
        "devicectl.list",
        "cargo.build",
        "ios.codesign",
        "devicectl.install",
        "devicectl.launch",
        "devicectl.screenshot",
    ] {
        assert!(steps.contains(&step), "{step} not in {steps:?}");
    }
    for args in [
        &["build", "ios-device", "--dry-run"][..],
        &["shot", "ios-device", "--dry-run"],
        &["logs", "ios-device", "--dry-run"],
        &["stop", "ios-device", "--dry-run"],
    ] {
        let result = device.json(args);
        assert_eq!(result["exit"], 0, "{args:?}: {result}");
    }
    assert_eq!(device.log("xcrun.log"), "");
    assert_eq!(device.log("codesign.log"), "");
    assert!(!device.dir().join("target/icm/build").exists());
}
