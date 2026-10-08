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

    /// What the fake device lists for `devicectl device info processes`,
    /// whatever its fake "app" does.
    fn list_processes(&self, processes: &[Value]) {
        std::fs::write(
            self.state().join("processes.json"),
            json!({"info": {"outcome": "success"}, "result": {"runningProcesses": processes}})
                .to_string(),
        )
        .unwrap();
    }

    /// The fake device's listing is back to following its fake "app".
    fn list_processes_as_the_app_runs(&self) {
        let _ = std::fs::remove_file(self.state().join("processes.json"));
    }

    /// Whether a command of `xcrun devicectl device process terminate` ran.
    fn terminated(&self) -> bool {
        self.log("xcrun.log")
            .lines()
            .any(|line| line.starts_with("devicectl device process terminate "))
    }

    /// Names the app `name` (its bundle is `<name>.app`).
    fn rename_app(&self, name: &str) {
        let config = self.dir().join("icm.toml");
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(text.contains("name = \"Fixture\""), "{text}");
        std::fs::write(
            &config,
            text.replace("name = \"Fixture\"", &format!("name = \"{name}\"")),
        )
        .unwrap();
    }

    fn session_path(&self) -> PathBuf {
        self.dir().join("target/icm/sessions/ios-device.json")
    }

    fn session(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.session_path()).unwrap()).unwrap()
    }
}

/// The app's executable on the fake device: the binary the fake cargo built
/// (`ICM_FAKE_BIN`, which `Device::json` sets), in the bundle `Fixture.app`.
const APP_EXECUTABLE: &str =
    "file:///private/var/containers/Bundle/Application/X/Fixture.app/release-app";

fn process(pid: i64, executable: &str) -> Value {
    json!({"executable": executable, "processIdentifier": pid})
}

/// Ends what the fake devicectl left running under `pid`, a number a file
/// of the test holds: its console, a `sleep 30` once the script has exec'd
/// it. Whatever has ended since may have left the number to another
/// process, so one shell command looks at the process and signals it only
/// while it still is that `sleep 30`.
fn end_fake_console(pid: &str) {
    let _ = Command::new("/bin/sh")
        .args([
            "-c",
            r#"[ "$(/bin/ps -p "$1" -o command= 2>/dev/null)" = "sleep 30" ] && kill "$1""#,
            "sh",
            pid.trim(),
        ])
        .stderr(Stdio::null())
        .status();
}

impl Drop for Device {
    fn drop(&mut self) {
        if let Ok(pid) = std::fs::read_to_string(self.state().join("device-app.pid")) {
            end_fake_console(&pid);
        }
        if let Ok(text) =
            std::fs::read_to_string(self.dir().join("target/icm/sessions/ios-device.json"))
            && let Ok(session) = serde_json::from_str::<Value>(&text)
            && let Some(pid) = session["pid"].as_i64()
        {
            end_fake_console(&pid.to_string());
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

    let before = device.log("xcrun.log").len();
    let stop = device.json(&["stop", "ios-device"]);
    assert_eq!(stop["exit"], 0, "{stop}");
    // The device was asked what runs under the recorded pid first, and only
    // because it lists the app there is the recorded command run.
    let calls: Vec<String> = device.log("xcrun.log")[before..]
        .lines()
        .filter(|line| line.starts_with("devicectl "))
        .map(str::to_string)
        .collect();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(
        calls[0].starts_with(&format!("devicectl device info processes --device {UDID} ")),
        "{calls:?}"
    );
    assert_eq!(
        calls[1],
        format!("devicectl device process terminate --device {UDID} --pid 4242")
    );
    assert_eq!(stop["stopped"][0]["app"], "terminated", "{stop}");
    assert_eq!(stop["unverified"], json!([]), "{stop}");
    assert_eq!(stop["warnings"], json!([]), "{stop}");
    assert!(!device.session_path().exists());
}

/// `run` records the identity of the console process it starts, and `stop`
/// signals that process only while its pid still has it. The record's pid
/// was the only test before and, with the file's write time, a pid that
/// another process took after the file was last written still passed.
#[test]
fn stop_signals_only_the_console_process_icm_started() {
    let device = Device::new();
    let run = device.json(&["run", "ios-device", "--settle", "0s"]);
    assert_eq!(run["exit"], 0, "{run}");
    let path = device.dir().join("target/icm/sessions/ios-device.json");
    let mut session: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let pid = session["pid"].as_i64().unwrap();
    let recorded: icm::procid::Identity =
        serde_json::from_value(session["identity"].clone()).unwrap();
    assert_eq!(icm::procid::of(pid as i32).unwrap().start, recorded.start);
    let alive = || icm::procid::of(pid as i32).is_some();

    // Another process has the pid now: the record keeps the identity of
    // the one that had it. The record's stop command stays, and the device
    // lists no app (the fake one is that console process, which this test
    // keeps out of the stop command's reach), so only the pid's test is
    // left to decide.
    assert!(!session["stop"].as_array().unwrap().is_empty());
    device.list_processes(&[]);
    session["identity"] = json!({"start": "1791334000.000001", "exe": "/usr/bin/xcrun"});
    std::fs::write(&path, session.to_string()).unwrap();
    let stop = device.json(&["stop", "ios-device"]);
    assert_eq!(stop["exit"], 0, "{stop}");
    assert_eq!(stop["stopped"][0]["already_gone"], json!([pid]), "{stop}");
    assert_eq!(stop["stopped"][0]["processes"], json!([]), "{stop}");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(alive(), "a process that only has the pid was signalled");
    assert!(!path.exists());
    assert!(!device.terminated(), "{}", device.log("xcrun.log"));

    // With the identity run recorded, the process is ended.
    session["identity"] = serde_json::to_value(&recorded).unwrap();
    std::fs::write(&path, session.to_string()).unwrap();
    let stop = device.json(&["stop", "ios-device"]);
    assert_eq!(stop["stopped"][0]["processes"], json!([pid]), "{stop}");
    assert!(!alive());
    assert!(!device.terminated(), "{}", device.log("xcrun.log"));
}

/// `stop` used to run the record's `devicectl device process terminate
/// --device <udid> --pid <pid>` as it stood. The pid is the app's on the
/// phone, where pids are reused like anywhere: an app that had exited, or
/// been restarted by hand, left the pid to another process, which the
/// command then terminated. The device is asked first, and the stored
/// command (kept in the record here) is run only when the device lists that
/// pid as the app: not when another process has it, not when no process
/// does, and the app running under another pid is not the one icm launched.
#[test]
fn stop_leaves_a_process_that_took_the_apps_pid_alone() {
    for (case, listing, why) in [
        (
            "another process has the pid",
            vec![
                process(50, "file:///usr/libexec/backboardd"),
                process(
                    4242,
                    "file:///private/var/containers/Bundle/Application/Y/Other.app/other",
                ),
            ],
            "pid 4242 is /private/var/containers/Bundle/Application/Y/Other.app/other on the device now",
        ),
        (
            "the app runs under another pid",
            vec![process(5000, APP_EXECUTABLE)],
            "the app runs as pid 5000",
        ),
        (
            "nothing has the pid",
            vec![process(50, "file:///usr/libexec/backboardd")],
            "no process has pid 4242",
        ),
    ] {
        let device = Device::new();
        let run = device.json(&["run", "ios-device", "--settle", "0s"]);
        assert_eq!(run["exit"], 0, "{case}: {run}");
        assert_eq!(run["process"]["pid"], 4242, "{case}: {run}");
        let stored = device.session()["stop"].clone();
        assert_eq!(stored.as_array().unwrap().len(), 1, "{case}");
        device.list_processes(&listing);

        let stop = device.json(&["stop", "ios-device"]);
        assert_eq!(stop["exit"], 0, "{case}: {stop}");
        let xcrun = device.log("xcrun.log");
        assert!(
            xcrun.contains(&format!("devicectl device info processes --device {UDID} ")),
            "{case}: the device was not asked: {xcrun}"
        );
        assert!(
            !device.terminated(),
            "{case}: the recorded command ran for a pid the device does not list as the app: {xcrun}"
        );
        assert_eq!(stop["stopped"][0]["app"], "not_running", "{case}: {stop}");
        let note = stop["stopped"][0]["app_note"].as_str().unwrap();
        assert!(note.contains(why), "{case}: {note}");
        assert_eq!(stop["stopped"][0]["commands"], json!([]), "{case}: {stop}");
        assert_eq!(stop["unverified"], json!([]), "{case}: {stop}");
        assert_eq!(stop["warnings"], json!([]), "{case}: {stop}");
        assert_eq!(stop["checks"]["failed"], json!([]), "{case}: {stop}");
        assert!(!device.session_path().exists(), "{case}");
    }
}

/// devicectl lists a process's executable as a file URL, and Foundation
/// percent-encodes a URL's path: an app named `My App` runs in
/// `My%20App.app`, and `Café` in `Caf%C3%A9.app`. The record names the
/// bundle as its directory is called, so the two were compared unlike and
/// the app was taken for another process: `stop` terminated nothing, said
/// the app was not running, and removed the record, with the app still on
/// the phone and no stop to retry. Not seen on a phone: this host has none,
/// so the listing's spelling is Foundation's documented one, and the fake
/// device lists it that way.
#[test]
fn an_app_whose_name_the_listing_escapes_is_found_and_terminated() {
    for (name, listed) in [("My App", "My%20App"), ("Café", "Caf%C3%A9")] {
        let device = Device::new();
        device.rename_app(name);
        let run = device.json(&["run", "ios-device", "--settle", "0s"]);
        assert_eq!(run["exit"], 0, "{name}: {run}");
        let session = device.session();
        assert_eq!(session["app"]["bundle"], format!("{name}.app"), "{name}");
        assert_eq!(session["stop"].as_array().unwrap().len(), 1, "{name}");
        let in_bundle = |dir: &str| {
            format!("file:///private/var/containers/Bundle/Application/X/{dir}/release-app")
        };

        // Another bundle's executable under the pid is not the app, however
        // alike: nothing is terminated.
        device.list_processes(&[process(4242, &in_bundle(&format!("{listed}%20Beta.app")))]);
        let stop = device.json(&["stop", "ios-device"]);
        assert_eq!(stop["exit"], 0, "{name}: {stop}");
        assert_eq!(stop["stopped"][0]["app"], "not_running", "{name}: {stop}");
        assert!(!device.terminated(), "{name}: {}", device.log("xcrun.log"));

        // The app itself, listed with its path escaped, is terminated with
        // the stored command (the record was removed by that stop, so the
        // app is started again).
        let run = device.json(&["run", "ios-device", "--settle", "0s"]);
        assert_eq!(run["exit"], 0, "{name}: {run}");
        device.list_processes(&[process(4242, &in_bundle(&format!("{listed}.app")))]);
        let stop = device.json(&["stop", "ios-device"]);
        assert_eq!(stop["exit"], 0, "{name}: {stop}");
        assert_eq!(stop["stopped"][0]["app"], "terminated", "{name}: {stop}");
        assert_eq!(stop["stopped"][0]["app_note"], Value::Null, "{name}");
        assert!(device.terminated(), "{name}: {}", device.log("xcrun.log"));
        assert_eq!(stop["unverified"], json!([]), "{name}: {stop}");
        assert_eq!(stop["warnings"], json!([]), "{name}: {stop}");
        assert!(!device.session_path().exists(), "{name}");
    }
}

/// When the device cannot be asked (not connected, locked, devicectl
/// failing), whose process the recorded pid is cannot be told either, so
/// nothing is terminated: a WARN says so, the result lists the device as
/// `unverified` instead of reporting the app stopped, and the record stays
/// so that the next stop can ask again. The console process icm started is
/// ended all the same, since its own identity is checked locally.
#[test]
fn stop_terminates_nothing_it_could_not_confirm_and_keeps_the_record() {
    let device = Device::new();
    let run = device.json(&["run", "ios-device", "--settle", "0s"]);
    assert_eq!(run["exit"], 0, "{run}");
    let stored = device.session()["stop"].clone();
    let console_pid = device.session()["pid"].as_i64().unwrap() as i32;
    std::fs::write(device.state().join("processes-fail"), "").unwrap();

    let stop = device.json(&["stop", "ios-device"]);
    assert_eq!(stop["exit"], 0, "{stop}");
    assert!(!device.terminated(), "{}", device.log("xcrun.log"));
    assert_eq!(stop["stopped"], json!([]), "{stop}");
    assert_eq!(stop["unverified"], json!([UDID]), "{stop}");
    assert_eq!(
        stop["summary"],
        "the app on Jo's iPhone may still be running (devicectl could not confirm it)",
        "{stop}"
    );
    let warned: Vec<&Value> = stop["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["id"] == "ios.device.stop_unconfirmed")
        .collect();
    assert_eq!(warned.len(), 1, "{stop}");
    let detail = warned[0]["detail"].as_str().unwrap();
    assert!(
        detail.starts_with("ios-device: did not terminate the app on Jo's iPhone: ")
            && detail.contains("not connected"),
        "{detail}"
    );
    assert_eq!(
        warned[0]["fix"]["commands"],
        json!([
            format!("xcrun devicectl device info processes --device {UDID}"),
            "icm stop ios-device --json -q"
        ])
    );
    // The record stays, with its stop command; icm's own console is gone.
    assert_eq!(device.session()["stop"], stored);
    assert!(icm::procid::of(console_pid).is_none(), "the console runs");

    // The device answers again, and lists the app under the recorded pid:
    // the retry terminates it with the stored command and removes the record.
    std::fs::remove_file(device.state().join("processes-fail")).unwrap();
    device.list_processes(&[process(4242, APP_EXECUTABLE)]);
    let stop = device.json(&["stop", "ios-device"]);
    assert_eq!(stop["exit"], 0, "{stop}");
    assert!(device.terminated(), "{}", device.log("xcrun.log"));
    assert_eq!(stop["stopped"][0]["app"], "terminated", "{stop}");
    assert_eq!(stop["unverified"], json!([]), "{stop}");
    assert_eq!(stop["warnings"], json!([]), "{stop}");
    assert!(!device.session_path().exists());
}

/// A device that answers with text that is no process list is not an empty
/// device: devicectl writes its failures as JSON too.
#[test]
fn an_answer_that_is_no_process_list_confirms_nothing() {
    for (case, text) in [
        ("an empty answer", ""),
        (
            "a failure written as JSON",
            r#"{"info":{"outcome":"failed"},"error":{"localizedDescription":"locked"}}"#,
        ),
        ("a listing without the processes", r#"{"result":{}}"#),
    ] {
        let device = Device::new();
        let run = device.json(&["run", "ios-device", "--settle", "0s"]);
        assert_eq!(run["exit"], 0, "{case}: {run}");
        std::fs::write(device.state().join("processes.json"), text).unwrap();

        let stop = device.json(&["stop", "ios-device"]);
        assert_eq!(stop["exit"], 0, "{case}: {stop}");
        assert!(!device.terminated(), "{case}");
        assert_eq!(stop["unverified"], json!([UDID]), "{case}: {stop}");
        assert!(device.session_path().exists(), "{case}");
    }
}

/// The stored command must be the one for the pid and device the record
/// names: the device is asked about `app_pid`, so a command for another
/// pid would end a process nothing checked.
#[test]
fn a_stored_command_for_another_pid_is_not_run() {
    let device = Device::new();
    let run = device.json(&["run", "ios-device", "--settle", "0s"]);
    assert_eq!(run["exit"], 0, "{run}");
    let mut session = device.session();
    session["stop"][0][8] = json!("99");
    std::fs::write(device.session_path(), session.to_string()).unwrap();
    device.list_processes(&[
        process(4242, APP_EXECUTABLE),
        process(99, "file:///sbin/launchd"),
    ]);

    let stop = device.json(&["stop", "ios-device"]);
    assert_eq!(stop["exit"], 0, "{stop}");
    assert!(!device.terminated(), "{}", device.log("xcrun.log"));
    assert_eq!(stop["unverified"], json!([UDID]), "{stop}");
    let detail = stop["warnings"][0]["detail"].as_str().unwrap();
    assert!(detail.contains("is not the one for pid 4242"), "{detail}");
    assert!(device.session_path().exists());

    // The same record with its own command is run.
    session["stop"][0][8] = json!("4242");
    std::fs::write(device.session_path(), session.to_string()).unwrap();
    let stop = device.json(&["stop", "ios-device"]);
    assert_eq!(stop["stopped"][0]["app"], "terminated", "{stop}");
    assert!(device.terminated());
    device.list_processes_as_the_app_runs();
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
    // Nor does the session record, though the app sent it in its `ready`
    // event's fields of its own.
    secret::assert_sessions_keep_none(&icm);

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

/// A later command whose environment lacks the secret (`icm logs
/// ios-device` from another shell after `icm run ios-device --env
/// API_TOKEN=…`) still redacts what the app logged on the console: the
/// session keeps the secret values of the app's `--env` next to its
/// console, and every command of the project reads them. A secret-named
/// variable of the user's shell that icm only inherits reaches no file
/// under `target/`.
#[test]
fn later_commands_without_the_secret_keep_none() {
    let mut device = Device::new();
    device.set("ICM_FAKE_SCENARIO", "leak");
    device.set(secret::INHERITED_NAME, secret::INHERITED);
    let target = device.dir().join("target");
    let icm = target.join("icm");
    let pair = format!("{}={}", secret::NAME, secret::TOKEN);

    let run = device.json(&["run", "ios-device", "--settle", "0s", "--env", &pair]);
    assert_eq!(run["exit"], 0, "{run}");
    let live = icm
        .join("sessions/ios-device")
        .join(run["run"].as_str().unwrap());
    assert!(secret::holds(&live.join("console.log")));
    assert!(live.join("secrets.json").is_file());
    secret::assert_sessions_keep_none(&icm);

    for args in [
        &["logs", "ios-device"][..],
        &["logs", "ios-device", "--raw"],
        &["stop", "ios-device"],
    ] {
        let result = device.json(args);
        assert_eq!(result["exit"], 0, "{args:?}: {result}");
    }
    secret::assert_kept_nowhere(&icm.join("runs"));
    let last = std::fs::read_to_string(icm.join("last.json")).unwrap();
    assert!(
        secret::forms()
            .iter()
            .all(|form| !last.contains(form.as_str()))
    );
    secret::assert_inherited_nowhere(&target);
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

/// A console process whose identity icm could not read when it started it
/// (`ICM_FAKE_IDENTITY_UNREADABLE` makes the read fail, as it does when the
/// OS will not describe the process) is recorded as such, and the run says
/// so. That is not a record from before identities (judged by the file's
/// write time): `stop` does not signal the process, and does not report it
/// as already gone; it says it left it running, with its pid, and `ps`
/// lists it as unverified, neither running nor stale.
#[test]
fn stop_leaves_a_console_process_whose_identity_could_not_be_read() {
    let mut device = Device::new();
    let why = "proc_pidinfo: Operation not permitted";
    device.set("ICM_FAKE_IDENTITY_UNREADABLE", why);
    let run = device.json(&["run", "ios-device", "--settle", "0s"]);
    device
        .env
        .retain(|(key, _)| key != "ICM_FAKE_IDENTITY_UNREADABLE");
    assert_eq!(run["exit"], 0, "{run}");
    let warning = run["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|warning| warning["id"] == "run.identity_unavailable")
        .unwrap_or_else(|| panic!("no run.identity_unavailable in {run}"));
    let detail = warning["detail"].as_str().unwrap();
    assert!(detail.contains("console process"), "{detail}");
    assert!(detail.contains(why), "{detail}");
    let path = device.dir().join("target/icm/sessions/ios-device.json");
    let mut session: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let pid = session["pid"].as_i64().unwrap();
    assert_eq!(
        session["identity"],
        json!({"start": "", "unavailable": why}),
        "{session}"
    );
    let alive = || icm::procid::of(pid as i32).is_some();
    assert!(alive());

    // `stop` runs no command here, only the pid's test.
    session["stop"] = json!([]);
    std::fs::write(&path, session.to_string()).unwrap();

    let ps = device.json(&["ps"]);
    assert_eq!(ps["sessions"][0]["unverified"], json!([pid]), "{ps}");
    assert_eq!(ps["sessions"][0]["running"], false, "{ps}");

    let stop = device.json(&["stop", "ios-device"]);
    assert_eq!(stop["exit"], 0, "{stop}");
    assert_eq!(stop["stopped"][0]["left_running"], json!([pid]), "{stop}");
    assert_eq!(stop["stopped"][0]["already_gone"], json!([]), "{stop}");
    assert_eq!(stop["stopped"][0]["processes"], json!([]), "{stop}");
    let warning = stop["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|warning| warning["id"] == "run.identity_unavailable")
        .unwrap_or_else(|| panic!("no run.identity_unavailable in {stop}"));
    let detail = warning["detail"].as_str().unwrap();
    assert!(detail.contains(&format!("pid {pid}")), "{detail}");
    assert!(detail.contains("Operation not permitted"), "{detail}");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        alive(),
        "a process whose identity was unavailable was signalled"
    );

    // SAFETY: kill(2) on the fake console process this test started.
    unsafe {
        let _ = libc::kill(pid as i32, libc::SIGKILL);
    }
}
