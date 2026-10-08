//! `icm run|logs|shot|stop ios-sim` end to end against fake tools
//! (`tests/fixtures/fake-ios/`): a fake `xcrun` (actool and simctl), `cargo`
//! (real `metadata`, a synthetic simulator Mach-O for `build`), `rustc`,
//! `rustup`, `xcodebuild`, `codesign` and `xattr`. The real `plutil` lints
//! the generated plists, so these run on macOS only.
#![cfg(target_os = "macos")]

use icm::platform::ios_sim::{image, macho};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[path = "support/secret.rs"]
mod secret;

const BIN: &str = env!("CARGO_BIN_EXE_icm");

struct Fake {
    root: tempfile::TempDir,
    project: PathBuf,
    state: PathBuf,
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

fn write_json(path: &Path, value: &Value) {
    std::fs::write(path, value.to_string()).unwrap();
}

impl Fake {
    fn new() -> Fake {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("app");
        copy_dir(&fixtures().join("app"), &project);
        let state = root.path().join("state");
        std::fs::create_dir_all(&state).unwrap();

        let triple = if cfg!(target_arch = "x86_64") {
            "x86_64-apple-ios"
        } else {
            "aarch64-apple-ios-sim"
        };
        std::fs::create_dir_all(state.join("sysroot/lib/rustlib").join(triple).join("lib"))
            .unwrap();
        std::fs::create_dir_all(state.join("Xcode.app/Contents/Developer/usr/bin")).unwrap();
        std::fs::write(
            state.join("exe"),
            macho::synthetic(macho::PLATFORM_IOSSIMULATOR, (16, 0, 0), (27, 0, 0)),
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
            state.join("adb"),
            "#!/bin/sh\n[ \"$1\" = devices ] && printf 'List of devices attached\\n\\n'\nexit 0\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(state.join("adb"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }

        let iphone = |name: &str| {
            json!({"name": name, "productFamily": "iPhone",
                   "identifier": format!("com.apple.CoreSimulator.SimDeviceType.{}", name.replace(' ', "-"))})
        };
        write_json(
            &state.join("runtimes.json"),
            &json!({"runtimes": [{
                "isAvailable": true, "version": "27.0", "buildversion": "24A434", "platform": "iOS",
                "identifier": "com.apple.CoreSimulator.SimRuntime.iOS-27-0", "name": "iOS 27.0",
                "supportedDeviceTypes": [iphone("iPhone 18 Pro"), iphone("iPhone 17"), iphone("iPhone Air")]
            }]}),
        );
        write_json(
            &state.join("devicetypes.json"),
            &json!({"devicetypes": [iphone("iPhone 18 Pro"), iphone("iPhone 17"), iphone("iPhone Air")]}),
        );
        write_json(
            &state.join("devices.json"),
            &json!({"devices": {"com.apple.CoreSimulator.SimRuntime.iOS-27-0": [
                {"udid": "OWNER-UDID", "name": "iPhone 17", "state": "Shutdown", "isAvailable": true,
                 "deviceTypeIdentifier": "com.apple.CoreSimulator.SimDeviceType.iPhone-17"}
            ]}}),
        );
        write_json(
            &state.join("devices-created.json"),
            &json!({"devices": {"com.apple.CoreSimulator.SimRuntime.iOS-27-0": [
                {"udid": "OWNER-UDID", "name": "iPhone 17", "state": "Shutdown", "isAvailable": true,
                 "deviceTypeIdentifier": "com.apple.CoreSimulator.SimDeviceType.iPhone-17"},
                {"udid": "FAKE-UDID", "name": "icm-iphone-17-ios-27.0", "state": "Shutdown", "isAvailable": true,
                 "deviceTypeIdentifier": "com.apple.CoreSimulator.SimDeviceType.iPhone-17",
                 "dataPath": state.join("device-data").display().to_string()}
            ]}}),
        );

        Fake {
            root,
            project,
            state,
        }
    }

    fn run(&self, scenario: &str, args: &[&str]) -> Output {
        self.run_with(scenario, args, &[])
    }

    /// [`Fake::run`] with more variables in icm's environment.
    fn run_with(&self, scenario: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
        let fakes = fixtures().join("fake-ios");
        let home = std::env::var("HOME").unwrap_or_default();
        let mut command = Command::new(BIN);
        let _ = command
            .args(args)
            .current_dir(&self.project)
            .stdin(Stdio::null())
            .env("ICM_CACHE_DIR", self.root.path().join("cache"))
            .env("ICM_HOST_CONFIG", self.root.path().join("no-host.toml"))
            .env("CARGO_TARGET_DIR", self.project.join("target"))
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
            .env("ICM_FAKE_STATE", &self.state)
            .env("ICM_FAKE_SCENARIO", scenario)
            .env(
                "DEVELOPER_DIR",
                self.state.join("Xcode.app/Contents/Developer"),
            )
            .env("ICM_TOOL_XCRUN", fakes.join("xcrun"))
            .env("ICM_TOOL_CARGO", fakes.join("cargo"))
            .env("ICM_TOOL_RUSTC", fakes.join("rustc"))
            .env("ICM_TOOL_RUSTUP", fakes.join("rustup"))
            .env("ICM_TOOL_XCODEBUILD", fakes.join("xcodebuild"))
            .env("ICM_TOOL_CODESIGN", fakes.join("ok"))
            .env("ICM_TOOL_XATTR", fakes.join("ok"))
            // `stop --all` also asks Android: an adb with no device online,
            // so no test reaches the host's real adb.
            .env("ICM_TOOL_ADB", self.state.join("adb"))
            .env("ICM_TOOL_EMULATOR", fakes.join("ok"))
            .envs(env.iter().copied());
        for var in [
            "ICM_JSON",
            "ICM_CONFIG",
            "ICM_TIMEOUT",
            "ICM_RUN_ID",
            "ICM_RUN_DIR",
            "ICM_RUN_ROOT",
            "ICM_DETACHED",
        ] {
            let _ = command.env_remove(var);
        }
        command.output().unwrap()
    }

    fn result(&self, scenario: &str, args: &[&str]) -> Value {
        self.result_with(scenario, args, &[])
    }

    /// [`Fake::result`] with more variables in icm's environment.
    fn result_with(&self, scenario: &str, args: &[&str], env: &[(&str, &str)]) -> Value {
        let output = self.run_with(scenario, args, env);
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
            output.status.code(),
            result["exit"].as_i64().map(|code| code as i32),
            "{result}"
        );
        result
    }

    fn path(&self, value: &Value) -> PathBuf {
        let path = PathBuf::from(value.as_str().expect("a path"));
        if path.is_absolute() {
            path
        } else {
            self.project.join(path)
        }
    }

    fn xcrun_log(&self) -> String {
        std::fs::read_to_string(self.state.join("xcrun.log")).unwrap_or_default()
    }

    /// Every NDJSON line of a `--json` run (its result last).
    fn events(&self, scenario: &str, args: &[&str]) -> Vec<Value> {
        let output = self.run(scenario, args);
        let events: Vec<Value> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            events.last().map(|e| e["type"].clone()),
            Some(json!("result")),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        events
    }

    /// Writes the host.toml icm reads.
    fn host(&self, text: &str) {
        std::fs::write(self.root.path().join("no-host.toml"), text).unwrap();
    }

    /// The owner variable icm set in a simulator's launchd environment.
    fn owner(&self, udid: &str) -> Option<String> {
        std::fs::read_to_string(self.state.join(format!("owner-{udid}")))
            .ok()
            .map(|text| text.trim().to_string())
    }

    fn set_owner(&self, udid: &str, tag: &str) {
        std::fs::write(self.state.join(format!("owner-{udid}")), format!("{tag}\n")).unwrap();
    }

    /// Two booted simulators and no session: icm's managed one
    /// (`MANAGED-UDID`, the one a run picks without a pin) and a test run's
    /// `icm-test-pinned` (`TEST-UDID`).
    fn booted_pair(&self) {
        let device = |udid: &str, name: &str| {
            json!({"udid": udid, "name": name, "state": "Booted", "isAvailable": true,
                   "deviceTypeIdentifier": "com.apple.CoreSimulator.SimDeviceType.iPhone-17"})
        };
        write_json(
            &self.state.join("devices.json"),
            &json!({"devices": {"com.apple.CoreSimulator.SimRuntime.iOS-27-0": [
                device("MANAGED-UDID", "icm-iphone-17-ios-27.0"),
                device("TEST-UDID", "icm-test-pinned")
            ]}}),
        );
    }

    fn kill_app(&self) {
        if let Ok(pid) = std::fs::read_to_string(self.state.join("app.pid")) {
            let _ = Command::new("kill").arg(pid.trim()).status();
        }
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.kill_app();
        // The collector (`log stream`) the fake runs as `sleep`.
        if let Ok(text) =
            std::fs::read_to_string(self.project.join("target/icm/sessions/ios-sim.json"))
            && let Ok(session) = serde_json::from_str::<Value>(&text)
            && let Some(pid) = session["collector_pid"].as_i64()
        {
            let _ = Command::new("kill").arg(format!("-{pid}")).status();
        }
    }
}

#[test]
fn run_logs_shot_and_stop() {
    let fake = Fake::new();
    let run = fake.result("ok", &["run", "ios-sim", "--json", "-q"]);
    assert_eq!(run["exit"], 0, "{run}");
    assert_eq!(run["process"]["ready"]["source"], "icm_event");
    assert_eq!(run["process"]["alive"], true);
    assert_eq!(run["device"]["name"], "icm-iphone-17-ios-27.0");
    assert_eq!(run["device"]["udid"], "FAKE-UDID");
    assert_eq!(run["device"]["managed"], true);
    assert_eq!(run["screen"]["px"], json!([1206, 2622]));
    assert_eq!(run["screen"]["preview"], json!([471, 1024]));
    assert_eq!(run["screen"]["scale"], 3.0);
    assert_eq!(run["checks"]["failed"], json!([]));
    assert_eq!(run["profile"], "debug");
    let preview = fake.path(&run["artifacts"]["preview"]);
    assert_eq!(image::png_size(&preview), Some((471, 1024)));
    assert!(fake.path(&run["artifacts"]["app_log"]).is_file());

    // The bundle.
    let bundle = fake.path(&run["artifacts"]["bundle"]);
    assert!(bundle.ends_with("Fixture.app"), "{}", bundle.display());
    assert!(bundle.join("fixture-app").is_file());
    assert!(bundle.join("PrivacyInfo.xcprivacy").is_file());
    assert!(bundle.join("Assets.car").is_file());
    let plist = std::fs::read_to_string(bundle.join("Info.plist")).unwrap();
    assert!(plist.contains("<key>UIApplicationSceneManifest</key>"));
    assert!(plist.contains("<string>com.acme.fixture</string>"));
    assert!(plist.contains("<string>iPhoneSimulator</string>"));

    // The managed simulator was created; the owner's was left alone; the
    // launch opted in to events.
    let log = fake.xcrun_log();
    assert!(log.contains(
        "simctl create icm-iphone-17-ios-27.0 com.apple.CoreSimulator.SimDeviceType.iPhone-17 com.apple.CoreSimulator.SimRuntime.iOS-27-0"
    ), "{log}");
    assert!(!log.contains("OWNER-UDID"), "{log}");
    assert!(log.contains("  env SIMCTL_CHILD_ICM_EVENTS=1"), "{log}");
    assert!(log.contains("  env SIMCTL_CHILD_RUST_BACKTRACE=1"), "{log}");
    assert!(log.contains("simctl spawn FAKE-UDID log stream --level debug --style ndjson"));
    let cargo = std::fs::read_to_string(fake.state.join("cargo.log")).unwrap();
    assert!(
        cargo.contains("--target aarch64-apple-ios-sim")
            || cargo.contains("--target x86_64-apple-ios")
    );

    // Logs re-read the live files.
    let logs = fake.result("ok", &["logs", "ios-sim", "--json", "-q"]);
    assert_eq!(logs["exit"], 0, "{logs}");
    let records = logs["records"].as_array().unwrap();
    assert!(
        records.iter().any(|r| r["tag"] == "ICM_EVENT"
            && r["msg"].as_str().unwrap().contains("\"kind\":\"ready\""))
    );
    assert!(records.iter().any(|r| r["msg"] == "hello from stdout"));
    let errors_only = fake.result(
        "ok",
        &["logs", "ios-sim", "--level", "error", "--json", "-q"],
    );
    assert_eq!(errors_only["count"], 0, "{errors_only}");

    // A new screenshot.
    let shot = fake.result("ok", &["shot", "ios-sim", "--json", "-q"]);
    assert_eq!(shot["exit"], 0, "{shot}");
    assert!(fake.path(&shot["artifacts"]["screenshot"]).is_file());
    assert_eq!(shot["screen"]["pt"], json!([402.0, 874.0]));

    // Device state through simctl; touches need AXe (not in phase 1).
    let dark = fake.result(
        "ok",
        &["input", "ios-sim", "appearance", "dark", "--json", "-q"],
    );
    assert_eq!(dark["exit"], 0, "{dark}");
    assert!(
        fake.xcrun_log()
            .contains("simctl ui FAKE-UDID appearance dark")
    );
    let tap = fake.result(
        "ok",
        &["input", "ios-sim", "tap", "100", "200", "--json", "-q"],
    );
    assert_eq!(tap["exit"], 2, "{tap}");
    assert_eq!(tap["errors"][0]["id"], "input.unsupported");

    // Stop ends the app and the collector; the session stays readable.
    let pid = run["process"]["pid"].as_i64().unwrap();
    let stop = fake.result("ok", &["stop", "ios-sim", "--json", "-q"]);
    assert_eq!(stop["exit"], 0, "{stop}");
    assert!(
        stop["summary"]
            .as_str()
            .unwrap()
            .contains("terminated com.acme.fixture")
    );
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        !Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    let session: Value = serde_json::from_str(
        &std::fs::read_to_string(fake.project.join("target/icm/sessions/ios-sim.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(session["state"], "stopped");
    assert!(session["collector_pid"].is_null());
}

/// A live process that is not the app, and not icm's child: what a pid
/// recorded for an app that has exited can name later. Killed and reaped
/// when dropped.
struct Unrelated(std::process::Child);

impl Unrelated {
    fn start() -> Unrelated {
        Unrelated(
            Command::new("sleep")
                .arg("120")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    fn running(&mut self) -> bool {
        self.0.try_wait().unwrap().is_none()
    }
}

impl Drop for Unrelated {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Fake {
    /// The ios-sim session an earlier `run` left on `udid`, its app's pid
    /// and `fields` (the pid's identity) as given.
    fn write_session(&self, udid: &str, pid: u32, fields: Value) {
        let mut session = json!({
            "schema": "icm.session/1", "platform": "ios-sim", "run": "r1", "run_dir": null,
            "state": "running",
            "device": {"udid": udid, "name": "icm-iphone-17-ios-27.0", "os": "27.0",
                       "type": "iPhone 17", "managed": true, "fresh": false, "data_path": null},
            "app_id": "com.acme.fixture", "exe": "fixture-app", "bundle": "/b/Fixture.app",
            "pid": pid, "launch_unix_ms": 1,
            "logs": {"stdout": "/o", "stderr": "/e", "oslog": "/l"},
            "collector_pid": null
        });
        session
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let sessions = self.project.join("target/icm/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("ios-sim.json"), session.to_string()).unwrap();
    }
}

/// The pid in a session says nothing about the app once the app has
/// exited: another process can take the number. `app_alive` took any
/// process under it for the app, so `stop ios-sim` ran `simctl terminate`
/// for an app that was not running (and failed when that found nothing,
/// since the pid stayed "alive"), and the next `run` on another simulator
/// ended the app there the same way.
#[test]
fn a_pid_another_process_took_is_not_the_app() {
    let mut unrelated = Unrelated::start();
    // A session an older icm wrote (no identity), and one whose identity is
    // another process's.
    for (case, fields) in [
        ("no identity", json!({})),
        (
            "another identity",
            json!({"pid_identity": {"start": "1791334000.000001", "exe": "/x/fixture-app"}}),
        ),
    ] {
        let fake = Fake::new();
        fake.write_session("OTHER-UDID", unrelated.0.id(), fields.clone());

        let stop = fake.result("ok", &["stop", "ios-sim", "--json", "-q"]);
        assert_eq!(stop["exit"], 0, "{case}: {stop}");
        assert!(
            stop["summary"]
                .as_str()
                .unwrap()
                .contains("com.acme.fixture was not running"),
            "{case}: {stop}"
        );
        assert!(
            !fake.xcrun_log().contains("simctl terminate"),
            "{case}: {}",
            fake.xcrun_log()
        );
        assert_eq!(stop["process"]["alive"], false, "{case}: {stop}");
        assert!(unrelated.running());

        // The next run on another simulator ends what the previous session
        // left only when that is the app.
        fake.write_session("OTHER-UDID", unrelated.0.id(), fields);
        let run = fake.result("ok", &["run", "ios-sim", "--json", "-q"]);
        assert_eq!(run["exit"], 0, "{case}: {run}");
        assert!(
            !fake.xcrun_log().contains("simctl terminate OTHER-UDID"),
            "{case}: {}",
            fake.xcrun_log()
        );
        assert!(unrelated.running());
    }
}

/// `icm ps` judges an ios-sim session's app as `stop ios-sim` does: by the
/// identity `run` recorded (`pid_identity`), not by whether the pid exists
/// and started before the file was last written, which a rewrite of the file
/// (`shot`, `run --attach`) after the pid was reused satisfies.
#[test]
fn ps_judges_the_ios_sim_app_by_its_identity() {
    let unrelated = Unrelated::start();
    let pid = unrelated.0.id();
    let own = serde_json::to_value(icm::procid::of(pid as i32).unwrap()).unwrap();
    for (case, fields, running) in [
        ("no identity", json!({}), false),
        (
            "another identity",
            json!({"pid_identity": {"start": "1791334000.000001", "exe": "/x/fixture-app"}}),
            false,
        ),
        ("the app's identity", json!({"pid_identity": own}), true),
    ] {
        let fake = Fake::new();
        // Written now, after the process started: the old test passes it.
        fake.write_session("OTHER-UDID", pid, fields);

        let ps = fake.result("ok", &["ps", "--json", "-q"]);
        assert_eq!(ps["exit"], 0, "{case}: {ps}");
        let session = &ps["sessions"][0];
        assert_eq!(session["platform"], "ios-sim", "{case}: {ps}");
        assert_eq!(session["running"], running, "{case}: {ps}");
        assert_eq!(
            session["alive"],
            if running { json!([pid]) } else { json!([]) },
            "{case}: {ps}"
        );
        assert_eq!(
            ps["summary"],
            format!("1 session(s), {} running", u8::from(running)),
            "{case}: {ps}"
        );
    }
}

/// A session whose app pid still has the process icm read at launch is the
/// app: `stop` terminates it, and the session records the identity `run`
/// read (`run_logs_shot_and_stop` ends the app that way).
#[test]
fn a_run_records_the_identity_stop_checks() {
    let fake = Fake::new();
    let run = fake.result("ok", &["run", "ios-sim", "--json", "-q"]);
    assert_eq!(run["exit"], 0, "{run}");
    let session: Value = serde_json::from_str(
        &std::fs::read_to_string(fake.project.join("target/icm/sessions/ios-sim.json")).unwrap(),
    )
    .unwrap();
    let pid = session["pid"].as_i64().unwrap();
    let recorded: icm::procid::Identity =
        serde_json::from_value(session["pid_identity"].clone()).unwrap();
    assert_eq!(icm::procid::of(pid as i32).unwrap().start, recorded.start);
    let collector = session["collector_pid"].as_i64().unwrap();
    let recorded: icm::procid::Identity =
        serde_json::from_value(session["collector_identity"].clone()).unwrap();
    assert_eq!(
        icm::procid::of(collector as i32).unwrap().start,
        recorded.start
    );

    let stop = fake.result("ok", &["stop", "ios-sim", "--json", "-q"]);
    assert_eq!(stop["exit"], 0, "{stop}");
    assert!(
        stop["summary"]
            .as_str()
            .unwrap()
            .contains("terminated com.acme.fixture"),
        "{stop}"
    );
    assert!(fake.xcrun_log().contains("simctl terminate FAKE-UDID"));
}

#[test]
fn a_panic_exits_ten_with_the_location() {
    let fake = Fake::new();
    let run = fake.result("panic", &["run", "ios-sim", "--json", "-q"]);
    assert_eq!(run["exit"], 10, "{run}");
    assert_eq!(run["errors"][0]["id"], "run.app_panicked");
    assert_eq!(
        run["errors"][0]["detail"],
        "panicked at src/lib.rs:7:5: boom"
    );
    let evidence = &run["errors"][0]["evidence"][0];
    assert!(evidence["path"].as_str().unwrap().ends_with("app.stderr"));
    assert_eq!(evidence["line"], 3);
    assert_eq!(run["process"]["alive"], false);
    assert!(fake.path(&evidence["path"]).is_file());
}

/// What a command keeps in its run directory holds no secret the app
/// logged: the value of a secret-named variable in icm's environment,
/// logged on stdout and stderr (plain, as JSON, in an `ICM_EVENT`), in the
/// unified log (as `log` escapes it, `/` as `\/`), in a panic and in a crash
/// report, is `<redacted>` in the copies of stdout and stderr, `app.log`,
/// `logs.ndjson`, `system.ndjson`, the crash report's copy, events and
/// results. The live files in `target/icm/sessions` are the app's and
/// `log`'s own output and keep it.
#[test]
fn run_directories_keep_no_secret() {
    let fake = Fake::new();
    let env = [(secret::NAME, secret::TOKEN)];
    let icm = fake.project.join("target/icm");

    let run = fake.result_with("leak", &["run", "ios-sim", "--json", "-q"], &env);
    assert_eq!(run["exit"], 0, "{run}");
    let app_log = std::fs::read_to_string(fake.path(&run["artifacts"]["app_log"])).unwrap();
    for line in [
        "signed in with <redacted>",
        "{\"token\":\"<redacted>\"}",
        "token <redacted>",
    ] {
        assert!(app_log.contains(line), "{line}: {app_log}");
    }
    let live = icm
        .join("sessions/ios-sim")
        .join(run["run"].as_str().unwrap());
    for file in ["app.stdout", "app.stderr", "oslog.ndjson"] {
        assert!(secret::holds(&live.join(file)), "{file}");
    }
    // Nor do the session records, though the app sent it in its `ready`
    // event's fields of its own.
    secret::assert_sessions_keep_none(&icm);

    let logs = fake.result_with("leak", &["logs", "ios-sim", "--json", "-q"], &env);
    assert_eq!(logs["exit"], 0, "{logs}");
    let oslog = logs["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["source"] == "oslog")
        .unwrap_or_else(|| panic!("{logs}"));
    assert_eq!(oslog["msg"], "signed in with <redacted>");
    // Raw `log` lines: the app's JSON line escaped twice in each, and once
    // more in the result.
    let raw = fake.result_with("leak", &["logs", "ios-sim", "--raw", "--json", "-q"], &env);
    assert_eq!(raw["exit"], 0, "{raw}");
    let lines: Vec<Value> = raw["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|record| record["source"] == "oslog")
        .map(|record| serde_json::from_str(record["msg"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(lines.len(), 3, "{raw}");
    assert_eq!(lines[1]["eventMessage"], "{\"token\":\"<redacted>\"}");
    // A URL with lowercase hex.
    assert_eq!(
        lines[2]["eventMessage"],
        "GET https://api.example.com/v1?lower=<redacted>"
    );
    assert!(!raw.to_string().contains(secret::TAIL), "{raw}");
    assert_eq!(
        fake.result_with("leak", &["stop", "ios-sim", "--json", "-q"], &env)["exit"],
        0
    );

    let died = fake.result_with("leak-panic", &["run", "ios-sim", "--json", "-q"], &env);
    assert_eq!(died["exit"], 10, "{died}");
    assert_eq!(
        died["errors"][0]["detail"],
        "panicked at src/lib.rs:7:5: rejected token <redacted>"
    );
    let crash = std::fs::read_to_string(fake.path(&died["artifacts"]["crash"])).unwrap();
    assert!(crash.contains("rejected token <redacted>"), "{crash}");
    let system = std::fs::read_to_string(fake.path(&died["artifacts"]["system_log"])).unwrap();
    assert!(system.contains("the system saw <redacted>"), "{system}");

    secret::assert_kept_nowhere(&icm.join("runs"));
    let last = std::fs::read_to_string(icm.join("last.json")).unwrap();
    assert!(
        secret::forms()
            .iter()
            .all(|form| !last.contains(form.as_str()))
    );
}

/// A later command whose environment lacks the secret (`icm logs ios-sim`
/// from another shell after `icm run ios-sim --env API_TOKEN=…`) still
/// redacts what the app logged: the session keeps the secret values of
/// the app's `--env` in a 0600 file next to its live files, and every
/// command of the project reads them. A secret-named variable of the
/// user's shell that icm only inherits reaches no file under `target/`.
#[test]
fn later_commands_without_the_secret_keep_none() {
    use std::os::unix::fs::PermissionsExt;
    let fake = Fake::new();
    let target = fake.project.join("target");
    let icm = target.join("icm");
    let pair = format!("{}={}", secret::NAME, secret::TOKEN);
    let shell = [(secret::INHERITED_NAME, secret::INHERITED)];

    let run = fake.result_with(
        "leak",
        &["run", "ios-sim", "--env", &pair, "--json", "-q"],
        &shell,
    );
    assert_eq!(run["exit"], 0, "{run}");
    let live = icm
        .join("sessions/ios-sim")
        .join(run["run"].as_str().unwrap());
    for file in ["app.stdout", "app.stderr", "oslog.ndjson"] {
        assert!(secret::holds(&live.join(file)), "{file}");
    }
    let mode = std::fs::metadata(live.join("secrets.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    for args in [
        &["logs", "ios-sim", "--json", "-q"][..],
        &["logs", "ios-sim", "--source", "all", "--json", "-q"],
        &["logs", "ios-sim", "--raw", "--json", "-q"],
        &["shot", "ios-sim", "--json", "-q"],
        &["stop", "ios-sim", "--json", "-q"],
    ] {
        let result = fake.result_with("leak", args, &shell);
        assert_eq!(result["exit"], 0, "{args:?}: {result}");
        // `shot` records its screen in the session.
        if args[0] != "stop" {
            secret::assert_sessions_keep_none(&icm);
        }
    }
    let system = icm.join("runs");
    secret::assert_kept_nowhere(&system);
    let last = std::fs::read_to_string(icm.join("last.json")).unwrap();
    assert!(
        secret::forms()
            .iter()
            .all(|form| !last.contains(form.as_str()))
    );
    secret::assert_inherited_nowhere(&target);
}

#[test]
fn without_events_the_probe_decides() {
    let fake = Fake::new();
    let run = fake.result("silent", &["run", "ios-sim", "--json", "-q"]);
    assert_eq!(run["exit"], 0, "{run}");
    assert_eq!(run["process"]["ready"]["source"], "probe");
    assert!(
        fake.xcrun_log()
            .contains("simctl spawn FAKE-UDID launchctl list")
    );
}

#[test]
fn usage_environment_and_dry_runs() {
    let fake = Fake::new();

    let missing = fake.result(
        "ok",
        &["run", "ios-sim", "--runtime", "19.0", "--json", "-q"],
    );
    assert_eq!(missing["exit"], 4, "{missing}");
    assert_eq!(missing["errors"][0]["id"], "env.ios_runtime_missing");
    assert!(
        missing["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("installed: iOS 27.0")
    );

    let unknown = fake.result("ok", &["run", "ios-sim", "--sim", "nope", "--json", "-q"]);
    assert_eq!(unknown["exit"], 7, "{unknown}");
    assert_eq!(unknown["errors"][0]["id"], "ios.sim.not_found");

    let none = fake.result("ok", &["logs", "ios-sim", "--json", "-q"]);
    assert_eq!(none["exit"], 7, "{none}");
    assert_eq!(none["errors"][0]["id"], "run.no_session");
    let stop = fake.result("ok", &["stop", "ios-sim", "--json", "-q"]);
    assert_eq!(stop["exit"], 0, "{stop}");

    let plan = fake.result("ok", &["run", "ios-sim", "--dry-run", "--json", "-q"]);
    assert_eq!(plan["exit"], 0, "{plan}");
    let names: Vec<&str> = plan["plan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"cargo.build") && names.contains(&"simctl.launch"),
        "{names:?}"
    );
    assert!(!fake.xcrun_log().contains("launch"));
}

#[test]
fn store_screenshots_need_a_store_size_simulator() {
    let fake = Fake::new();
    let iphone = |name: &str| {
        json!({"name": name, "productFamily": "iPhone",
               "identifier": format!("com.apple.CoreSimulator.SimDeviceType.{}", name.replace(' ', "-"))})
    };
    let types = [
        iphone("iPhone 17"),
        iphone("iPhone 17 Pro Max"),
        iphone("iPhone 16 Pro Max"),
    ];
    write_json(
        &fake.state.join("runtimes.json"),
        &json!({"runtimes": [{
            "isAvailable": true, "version": "27.0", "buildversion": "24A434", "platform": "iOS",
            "identifier": "com.apple.CoreSimulator.SimRuntime.iOS-27-0", "name": "iOS 27.0",
            "supportedDeviceTypes": types
        }]}),
    );
    write_json(
        &fake.state.join("devicetypes.json"),
        &json!({"devicetypes": types}),
    );
    write_json(
        &fake.state.join("devices-created.json"),
        &json!({"devices": {"com.apple.CoreSimulator.SimRuntime.iOS-27-0": [
            {"udid": "FAKE-UDID", "name": "icm-iphone-17-pro-max-ios-27.0", "state": "Shutdown", "isAvailable": true,
             "deviceTypeIdentifier": "com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro-Max",
             "dataPath": fake.state.join("device-data").display().to_string()}
        ]}}),
    );
    let mut screen = image::Rgba::filled(1320, 2868, [255, 255, 255, 255]);
    for y in 300..420 {
        for x in 100..1200 {
            screen.set(x, y, [80, 90, 240, 255]);
        }
    }
    image::write_png(&fake.state.join("screen.png"), &screen, true).unwrap();

    let run = fake.result("ok", &["run", "ios-sim", "--store", "--json", "-q"]);
    assert_eq!(run["exit"], 0, "{run}");
    assert_eq!(run["device"]["name"], "icm-iphone-17-pro-max-ios-27.0");
    assert!(fake.xcrun_log().contains(
        "simctl create icm-iphone-17-pro-max-ios-27.0 com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro-Max"
    ));

    let shot = fake.result(
        "ok",
        &[
            "shot", "ios-sim", "--store", "--name", "home", "--json", "-q",
        ],
    );
    assert_eq!(shot["exit"], 0, "{shot}");
    assert_eq!(shot["store"]["class"], "6.9-inch");
    let kept = fake.path(&shot["artifacts"]["store_screenshot"]);
    assert!(
        kept.ends_with("target/icm/store/ios/home-1320x2868.png"),
        "{}",
        kept.display()
    );
    // An RGB PNG: IHDR's colour type (byte 25) is 2, no alpha channel.
    assert_eq!(std::fs::read(&kept).unwrap()[25], 2);

    // A screen App Store Connect does not take.
    let small = image::Rgba::filled(1206, 2622, [255, 255, 255, 255]);
    image::write_png(&fake.state.join("screen.png"), &small, false).unwrap();
    let refused = fake.result("ok", &["shot", "ios-sim", "--store", "--json", "-q"]);
    assert_eq!(refused["exit"], 7, "{refused}");
    assert_eq!(refused["errors"][0]["id"], "ios.shot.store_size");
    assert!(
        refused["errors"][0]["fix"]["commands"][0]
            .as_str()
            .unwrap()
            .starts_with("icm run ios-sim --store")
    );
    let _ = fake.result("ok", &["stop", "ios-sim", "--json", "-q"]);
}

fn checks<'a>(events: &'a [Value], id: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|event| event["type"] == "check" && event["id"] == id)
        .collect()
}

/// `stop ios-sim --shutdown` without a session shuts nothing down, not even
/// icm's managed simulator booted and untagged: the acceptance scripts that
/// never run on a simulator (phase3.sh) stop it this way.
#[test]
fn stop_ios_sim_without_a_session_shuts_nothing_down() {
    let fake = Fake::new();
    fake.booted_pair();
    let stop = fake.result("ok", &["stop", "ios-sim", "--shutdown", "--json", "-q"]);
    assert_eq!(stop["exit"], 0, "{stop}");
    assert_eq!(stop["summary"], "no ios-sim session to stop", "{stop}");
    assert!(!fake.xcrun_log().contains("simctl"), "{}", fake.xcrun_log());
}

/// With host.toml pinning a test run's simulator and no ios-sim session,
/// `stop --all --shutdown` shuts down neither the pinned simulator (not
/// icm's) nor icm's managed one (not the one this project uses: another
/// project may be running on it).
#[test]
fn shutdown_without_a_session_honours_the_pinned_simulator() {
    let fake = Fake::new();
    fake.booted_pair();
    fake.host("[ios]\nsimulator_udid = \"TEST-UDID\"\n");

    let events = fake.events("ok", &["stop", "--all", "--shutdown", "--json"]);
    let stop = events.last().unwrap();
    assert_eq!(stop["exit"], 0, "{stop}");
    assert_eq!(stop["shutdown"], json!([]), "{stop}");
    let log = fake.xcrun_log();
    assert!(!log.contains("simctl shutdown"), "{log}");
    let left = checks(&events, "run.no_session");
    assert!(
        left.iter().any(|check| check["detail"]
            .as_str()
            .unwrap()
            .contains("left icm-test-pinned (TEST-UDID) running")),
        "{events:?}"
    );
}

/// A pinned `icm-test-*` simulator stays booted after `stop ios-sim
/// --shutdown` even when the run booted it: icm shuts down only the
/// simulators it created (Appendix D item 19, where the emulator `icm run
/// android` booted is the one exception), so whoever made a test simulator
/// shuts it down and deletes it (AGENTS.md "Devices").
#[test]
fn a_test_simulator_the_run_booted_stays_booted() {
    let fake = Fake::new();
    write_json(
        &fake.state.join("devices.json"),
        &json!({"devices": {"com.apple.CoreSimulator.SimRuntime.iOS-27-0": [
            {"udid": "TEST-UDID", "name": "icm-test-pinned", "state": "Shutdown", "isAvailable": true,
             "deviceTypeIdentifier": "com.apple.CoreSimulator.SimDeviceType.iPhone-17"}
        ]}}),
    );
    fake.host("[ios]\nsimulator_udid = \"TEST-UDID\"\n");
    let run = fake.result("ok", &["run", "ios-sim", "--json", "-q"]);
    assert_eq!(run["exit"], 0, "{run}");
    assert_eq!(run["device"]["udid"], "TEST-UDID", "{run}");
    assert!(
        fake.xcrun_log().contains("simctl boot TEST-UDID"),
        "{}",
        fake.xcrun_log()
    );

    let events = fake.events("ok", &["stop", "ios-sim", "--shutdown", "--json"]);
    let stop = events.last().unwrap();
    assert_eq!(stop["exit"], 0, "{stop}");
    let log = fake.xcrun_log();
    assert!(log.contains("simctl terminate TEST-UDID"), "{log}");
    assert!(!log.contains("simctl shutdown"), "{log}");
    assert!(
        checks(&events, "run.no_session")
            .iter()
            .any(|check| check["detail"]
                .as_str()
                .unwrap()
                .contains("left icm-test-pinned running")),
        "{events:?}"
    );
}

/// With an ios-sim session, `stop --all --shutdown` considers only the
/// session's simulator: a run on a test simulator (`--sim`) leaves icm's
/// managed one alone, booted and untagged as it is, since another process
/// may be running on it.
#[test]
fn shutdown_with_a_session_leaves_the_default_simulator() {
    let fake = Fake::new();
    fake.booted_pair();
    let run = fake.result(
        "ok",
        &["run", "ios-sim", "--sim", "TEST-UDID", "--json", "-q"],
    );
    assert_eq!(run["exit"], 0, "{run}");
    assert_eq!(run["device"]["udid"], "TEST-UDID");

    // The plan says so.
    let plan = fake.result(
        "ok",
        &["stop", "--all", "--shutdown", "--dry-run", "--json", "-q"],
    );
    let step = plan["plan"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["name"] == "ios-sim.shutdown")
        .unwrap_or_else(|| panic!("{plan}"));
    assert!(
        step["display"]
            .as_str()
            .unwrap()
            .contains("the session's simulator"),
        "{step}"
    );

    let events = fake.events("ok", &["stop", "--all", "--shutdown", "--json"]);
    let stop = events.last().unwrap();
    assert_eq!(stop["exit"], 0, "{stop}");
    assert_eq!(stop["shutdown"], json!([]), "{stop}");
    let log = fake.xcrun_log();
    assert!(log.contains("simctl terminate TEST-UDID"), "{log}");
    assert!(!log.contains("simctl shutdown"), "{log}");
    assert!(
        !log.contains("simctl getenv MANAGED-UDID"),
        "the default simulator was considered: {log}"
    );
    assert!(
        checks(&events, "run.no_session")
            .iter()
            .any(|check| check["detail"]
                .as_str()
                .unwrap()
                .contains("left icm-test-pinned running")),
        "{events:?}"
    );

    // The record stays after a stop, so a second stop still goes by it.
    let again = fake.result("ok", &["stop", "--all", "--shutdown", "--json", "-q"]);
    assert_eq!(again["exit"], 0, "{again}");
    assert_eq!(again["shutdown"], json!([]), "{again}");
    assert!(!fake.xcrun_log().contains("simctl shutdown"));
}

/// Without a session, the managed simulator is shut down unless icm booted
/// it for another project; then the advice names that simulator only.
#[test]
fn shutdown_without_a_session_leaves_another_projects_simulator() {
    let fake = Fake::new();
    fake.booted_pair();
    fake.set_owner("MANAGED-UDID", "0123456789abcdef");

    let events = fake.events("ok", &["stop", "--all", "--shutdown", "--json"]);
    let stop = events.last().unwrap();
    assert_eq!(stop["exit"], 0, "{stop}");
    assert_eq!(stop["shutdown"], json!([]), "{stop}");
    assert!(!fake.xcrun_log().contains("simctl shutdown"));
    assert!(
        fake.xcrun_log()
            .contains("simctl getenv MANAGED-UDID ICM_BOOTED_BY")
    );
    let shared = checks(&events, "ios.sim.shared");
    assert_eq!(shared.len(), 1, "{events:?}");
    assert_eq!(shared[0]["status"], "info");
    let detail = shared[0]["detail"].as_str().unwrap();
    assert!(
        detail.contains("icm-iphone-17-ios-27.0 (MANAGED-UDID)")
            && detail.contains("0123456789abcdef"),
        "{detail}"
    );
    assert_eq!(
        shared[0]["fix"]["commands"],
        json!(["xcrun simctl shutdown MANAGED-UDID"]),
        "{events:?}"
    );

    // Nobody's (booted outside icm, or by an icm before owners): shut down,
    // as before; the test run's simulator stays.
    std::fs::remove_file(fake.state.join("owner-MANAGED-UDID")).unwrap();
    let stop = fake.result("ok", &["stop", "--all", "--shutdown", "--json", "-q"]);
    assert_eq!(stop["exit"], 0, "{stop}");
    assert_eq!(
        stop["shutdown"],
        json!(["icm-iphone-17-ios-27.0 (MANAGED-UDID)"])
    );
    let log = fake.xcrun_log();
    assert!(log.contains("simctl shutdown MANAGED-UDID"), "{log}");
    assert!(!log.contains("simctl shutdown TEST-UDID"), "{log}");
}

/// A run that boots icm's simulator marks it as this project's; the
/// session's `stop --shutdown` shuts down only a simulator no other project
/// claims.
#[test]
fn a_run_claims_the_simulator_it_boots() {
    let fake = Fake::new();
    let run = fake.result("ok", &["run", "ios-sim", "--json", "-q"]);
    assert_eq!(run["exit"], 0, "{run}");
    let ours = fake.owner("FAKE-UDID").expect("the run set no owner");
    assert_eq!(ours.len(), 16, "{ours}");
    assert!(
        fake.xcrun_log().contains(&format!(
            "simctl spawn FAKE-UDID launchctl setenv ICM_BOOTED_BY {ours}"
        )),
        "{}",
        fake.xcrun_log()
    );

    // Another project booted it since: left running.
    fake.set_owner("FAKE-UDID", "0123456789abcdef");
    let events = fake.events("ok", &["stop", "ios-sim", "--shutdown", "--json"]);
    let stop = events.last().unwrap();
    assert_eq!(stop["exit"], 0, "{stop}");
    assert!(!fake.xcrun_log().contains("simctl shutdown"), "{stop}");
    assert_eq!(checks(&events, "ios.sim.shared").len(), 1, "{events:?}");

    // Ours: shut down.
    fake.set_owner("FAKE-UDID", &ours);
    let stop = fake.result("ok", &["stop", "ios-sim", "--shutdown", "--json", "-q"]);
    assert_eq!(stop["exit"], 0, "{stop}");
    assert!(
        stop["summary"]
            .as_str()
            .unwrap()
            .contains("shut down icm-iphone-17-ios-27.0"),
        "{stop}"
    );
    assert!(fake.xcrun_log().contains("simctl shutdown FAKE-UDID"));
}

/// A simulator whose owner icm cannot read stays up: a failed `simctl
/// getenv` is not an owner-less simulator, so `stop --shutdown` leaves it
/// running with a warning that names it.
#[test]
fn an_unreadable_owner_keeps_the_simulator_running() {
    let fake = Fake::new();
    fake.booted_pair();
    std::fs::write(fake.state.join("getenv-fails"), "").unwrap();

    let events = fake.events("ok", &["stop", "--all", "--shutdown", "--json"]);
    let stop = events.last().unwrap();
    assert_eq!(stop["exit"], 0, "{stop}");
    assert_eq!(stop["shutdown"], json!([]), "{stop}");
    let log = fake.xcrun_log();
    assert!(
        log.contains("simctl getenv MANAGED-UDID ICM_BOOTED_BY"),
        "{log}"
    );
    assert!(!log.contains("simctl shutdown"), "{log}");
    let unknown = checks(&events, "ios.sim.owner_unknown");
    assert_eq!(unknown.len(), 1, "{events:?}");
    assert_eq!(unknown[0]["status"], "warn");
    let detail = unknown[0]["detail"].as_str().unwrap();
    assert!(
        detail.contains("icm-iphone-17-ios-27.0 (MANAGED-UDID) left running")
            && detail.contains("exit 149"),
        "{detail}"
    );
    assert_eq!(
        unknown[0]["fix"]["commands"],
        json!(["xcrun simctl shutdown MANAGED-UDID"]),
        "{events:?}"
    );
    assert!(checks(&events, "ios.sim.shared").is_empty(), "{events:?}");
}

/// A run that cannot mark the simulator it booted says so, and the
/// session's `stop --shutdown` leaves the booted simulator running while
/// its owner cannot be read; one that is no longer booted needs no warning.
#[test]
fn failed_ownership_claims_and_reads_are_reported() {
    let fake = Fake::new();
    std::fs::write(fake.state.join("setenv-fails"), "").unwrap();
    let events = fake.events("ok", &["run", "ios-sim", "--json"]);
    let run = events.last().unwrap();
    assert_eq!(run["exit"], 0, "{run}");
    assert_eq!(fake.owner("FAKE-UDID"), None);
    let unclaimed = checks(&events, "ios.sim.owner_unknown");
    assert_eq!(unclaimed.len(), 1, "{events:?}");
    assert_eq!(unclaimed[0]["status"], "warn");
    let detail = unclaimed[0]["detail"].as_str().unwrap();
    assert!(
        detail.contains("could not mark icm-iphone-17-ios-27.0 (FAKE-UDID)")
            && detail.contains("exit 149"),
        "{detail}"
    );
    std::fs::remove_file(fake.state.join("setenv-fails")).unwrap();

    // The listing the fake prints, with the session's simulator's state.
    let listing = fake.state.join("devices-created.json");
    let set_state = |state: &str| {
        let mut devices: Value =
            serde_json::from_str(&std::fs::read_to_string(&listing).unwrap()).unwrap();
        for device in devices["devices"]["com.apple.CoreSimulator.SimRuntime.iOS-27-0"]
            .as_array_mut()
            .unwrap()
        {
            if device["udid"] == "FAKE-UDID" {
                device["state"] = json!(state);
            }
        }
        write_json(&listing, &devices);
    };

    // Booted, and its owner unreadable: left running.
    set_state("Booted");
    std::fs::write(fake.state.join("getenv-fails"), "").unwrap();
    let events = fake.events("ok", &["stop", "ios-sim", "--shutdown", "--json"]);
    let stop = events.last().unwrap();
    assert_eq!(stop["exit"], 0, "{stop}");
    assert!(!fake.xcrun_log().contains("simctl shutdown"), "{stop}");
    assert_eq!(
        checks(&events, "ios.sim.owner_unknown").len(),
        1,
        "{events:?}"
    );
    assert!(
        stop["summary"]
            .as_str()
            .unwrap()
            .contains("left icm-iphone-17-ios-27.0 running"),
        "{stop}"
    );

    // No longer booted: nothing is left running, so no warning.
    set_state("Shutdown");
    let events = fake.events("ok", &["stop", "ios-sim", "--shutdown", "--json"]);
    assert_eq!(events.last().unwrap()["exit"], 0, "{events:?}");
    assert!(
        checks(&events, "ios.sim.owner_unknown").is_empty(),
        "{events:?}"
    );
}
