//! End-to-end tests of the icm binary: the output contract, exit codes,
//! the runner's timeouts and signals, --detach and wait, config errors and
//! the lockfile checks. Each test gets its own cache dir and no host.toml.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_icm");

struct Sandbox {
    cache: tempfile::TempDir,
    cwd: PathBuf,
    _project: Option<tempfile::TempDir>,
}

impl Sandbox {
    fn new() -> Sandbox {
        let cache = tempfile::tempdir().unwrap();
        let cwd = cache.path().to_path_buf();
        Sandbox {
            cache,
            cwd,
            _project: None,
        }
    }

    /// A sandbox whose working directory is a copy of a fixture project.
    fn with_fixture(name: &str) -> Sandbox {
        let project = tempfile::tempdir().unwrap();
        copy_dir(&fixtures().join(name), project.path());
        let mut sandbox = Sandbox::new();
        sandbox.cwd = project.path().to_path_buf();
        sandbox._project = Some(project);
        sandbox
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        let _ = command
            .args(args)
            .current_dir(&self.cwd)
            .env("ICM_CACHE_DIR", self.cache.path())
            .env("ICM_HOST_CONFIG", self.cache.path().join("no-host.toml"))
            .env("CARGO_TARGET_DIR", self.cwd.join("target"))
            .stdin(Stdio::null());
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
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn runs(&self) -> PathBuf {
        self.cache.path().join("runs")
    }

    /// A path from a result, which is relative to icm's working directory
    /// when it lies inside it.
    fn path(&self, value: &Value) -> PathBuf {
        let path = PathBuf::from(value.as_str().expect("a path"));
        if path.is_absolute() {
            path
        } else {
            self.cwd.join(path)
        }
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

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Every stdout line parses, has the common fields, and the last is the
/// result; returns all of them.
fn ndjson(output: &Output) -> Vec<Value> {
    let text = stdout(output);
    let events: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("bad line {line:?}: {e}")))
        .collect();
    assert!(
        !events.is_empty(),
        "no output; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for event in &events {
        assert_eq!(event["v"], 1, "{event}");
        assert!(
            event["type"].is_string() && event["run"].is_string() && event["t"].is_u64(),
            "{event}"
        );
    }
    let result = events.last().unwrap();
    assert_eq!(
        result["type"], "result",
        "last line is not the result: {text}"
    );
    assert_eq!(result["ok"], result["exit"] == 0);
    assert_eq!(output.status.code().map(i64::from), result["exit"].as_i64());
    events
}

fn result(output: &Output) -> Value {
    ndjson(output).pop().unwrap()
}

#[test]
fn version_line() {
    let output = Sandbox::new().run(&["--version"]);
    assert!(output.status.success());
    let text = stdout(&output);
    assert!(text.starts_with("icm 0.14.1-mobile."), "{text}");
    assert!(text.trim_end().contains(' '), "{text}");
    let rest = text.trim_start_matches("icm 0.14.1-mobile.");
    assert!(rest.chars().next().unwrap().is_ascii_digit(), "{text}");
}

#[test]
fn usage_errors_exit_two_with_a_result() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["run", "nowhere", "--json", "-q"]);
    let events = ndjson(&output);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["exit"], 2);
    assert_eq!(events[0]["errors"][0]["id"], "usage.bad_args");
    assert!(
        events[0]["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("nowhere")
    );

    // Human mode: clap's message on stderr, protocol lines on stdout.
    let output = sandbox.run(&["run", "nowhere"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid value 'nowhere'"));
    let text = stdout(&output);
    assert!(
        text.lines()
            .any(|l| l.starts_with("CHECK FAIL usage.bad_args")),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.starts_with("RESULT fail run exit=2")),
        "{text}"
    );

    // ICM_JSON=1 switches to NDJSON too.
    let output = sandbox
        .command(&["run", "nowhere"])
        .env("ICM_JSON", "1")
        .output()
        .unwrap();
    assert_eq!(result(&output)["exit"], 2);

    // No subcommand.
    let output = sandbox.run(&["--json"]);
    assert_eq!(result(&output)["exit"], 2);

    // Usage errors leave no run directory behind.
    assert!(!sandbox.runs().exists());
}

#[test]
fn unimplemented_commands_say_so() {
    let sandbox = Sandbox::new();
    for args in [vec!["run", "ios-device"], vec!["version", "show"]] {
        let mut full = args.clone();
        full.extend(["--json", "-q"]);
        let result = result(&sandbox.run(&full));
        assert_eq!(result["exit"], 2, "{args:?}");
        assert_eq!(
            result["errors"][0]["id"], "usage.not_implemented",
            "{args:?}"
        );
        assert!(
            result["errors"][0]["detail"]
                .as_str()
                .unwrap()
                .contains("not implemented")
        );
    }
    let unknown = result(&sandbox.run(&["frobnicate", "--json"]));
    assert_eq!(unknown["errors"][0]["id"], "usage.bad_args");
}

#[test]
fn devices_lists_the_desktop_and_refuses_ios_devices() {
    let sandbox = Sandbox::new();
    let desktop = result(&sandbox.run(&["devices", "desktop", "--json", "-q"]));
    assert_eq!(desktop["exit"], 0, "{desktop}");
    assert_eq!(
        desktop["platforms"]["desktop"]["devices"][0]["kind"],
        "desktop"
    );
    let device = result(&sandbox.run(&["devices", "ios-device", "--json", "-q"]));
    assert_eq!(device["exit"], 2);
    assert_eq!(device["errors"][0]["id"], "usage.not_implemented");
    assert!(
        device["errors"][0]["fix"]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|command| command == "icm devices ios-sim --json -q")
    );
}

#[test]
fn explain() {
    let sandbox = Sandbox::new();

    let output = sandbox.run(&["explain", "deps.single_iced"]);
    assert!(output.status.success());
    let text = stdout(&output);
    assert!(text.starts_with("# deps.single_iced\n"), "{text}");
    assert!(
        text.contains("Exit code when it fails: 3 (CONFIG)"),
        "{text}"
    );
    assert!(text.contains("cargo update -p iced"), "{text}");
    assert!(
        !text.contains("RESULT"),
        "content commands keep stdout clean: {text}"
    );

    let json = result(&sandbox.run(&["explain", "env.jdk_missing", "--json", "-q"]));
    assert_eq!(json["entry"]["exit"], 4);
    assert_eq!(json["entry"]["by"], "doctor-yes");
    assert!(json["doc"].as_str().unwrap().contains("JAVA_HOME"));

    let list = result(&sandbox.run(&["explain", "--list", "--json", "-q"]));
    let catalogue = list["catalogue"].as_array().unwrap();
    assert!(catalogue.len() > 150);
    assert!(
        catalogue
            .iter()
            .filter(|e| e["hand_written"] == true)
            .count()
            >= 20
    );

    let codes = result(&sandbox.run(&["explain", "exit-codes", "--json", "-q"]));
    assert_eq!(codes["exit_codes"].as_array().unwrap().len(), 13);

    let unknown = sandbox.run(&["explain", "config.unknown_keys", "--json", "-q"]);
    let unknown = result(&unknown);
    assert_eq!(unknown["exit"], 2);
    assert!(
        unknown["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("config.unknown_key")
    );

    // Failures of content commands go to stderr in human mode.
    let output = sandbox.run(&["explain", "nope.nope"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stdout(&output).is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("CHECK FAIL usage.bad_args"));
}

#[test]
fn panics_exit_seventy_with_a_result() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["__test", "panic", "--json"]);
    let result = result(&output);
    assert_eq!(result["exit"], 70);
    assert_eq!(result["errors"][0]["id"], "internal.bug");
    assert!(
        result["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("deliberate panic")
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("deliberate panic"));

    // The run directory has the same result, last in events.ndjson too.
    let run_dir = sandbox.path(&result["run_dir"]);
    let events = std::fs::read_to_string(run_dir.join("events.ndjson")).unwrap();
    let last: Value = serde_json::from_str(events.lines().last().unwrap()).unwrap();
    assert_eq!(last["type"], "result");
    assert_eq!(last["exit"], 70);
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("result.json")).unwrap())
            .unwrap();
    assert_eq!(saved["exit"], 70);
    assert!(sandbox.cache.path().join("last.json").exists());
}

#[test]
fn failures_take_their_catalogue_exit_code() {
    let sandbox = Sandbox::new();
    for (id, exit) in [
        ("config.invalid", 3),
        ("env.jdk_missing", 4),
        ("run.app_panicked", 10),
        ("env.xcode_missing", 9),
    ] {
        let result = result(&sandbox.run(&["__test", "fail", id, "--json", "-q"]));
        assert_eq!(result["exit"], exit, "{id}");
        assert_eq!(result["errors"][0]["id"], id);
        assert_eq!(result["errors"][0]["docs"], format!("icm explain {id}"));
    }
}

#[test]
fn warnings_and_non_blocking_failures() {
    let sandbox = Sandbox::new();
    let ok = result(&sandbox.run(&["__test", "checks", "--json", "-q"]));
    assert_eq!(ok["exit"], 0);
    assert_eq!(ok["checks"]["warn"], 1);
    assert_eq!(ok["warnings"][0]["id"], "run.screen_blank");

    let strict = result(&sandbox.run(&["__test", "checks", "--strict", "--json", "-q"]));
    assert_eq!(strict["exit"], 1);

    let failed = result(&sandbox.run(&["__test", "checks", "--fail", "--json", "-q"]));
    assert_eq!(failed["exit"], 1);
    assert_eq!(failed["errors"][0]["id"], "web.size_budget");

    // Human mode: protocol lines only; -q keeps only FAIL/WARN and RESULT.
    let text = stdout(&sandbox.run(&["__test", "checks"]));
    for line in text.lines() {
        let keyword = line.split_whitespace().next().unwrap_or("");
        assert!(
            line.starts_with("  ")
                || [
                    "STEP", "CHECK", "ARTIFACT", "READY", "PLAN", "NEXT", "RESULT"
                ]
                .contains(&keyword),
            "not a protocol line: {line}"
        );
    }
    assert!(text.contains("CHECK PASS deps.single_iced"));
    let quiet = stdout(&sandbox.run(&["__test", "checks", "-q"]));
    assert!(!quiet.contains("CHECK PASS"));
    assert!(quiet.contains("CHECK WARN run.screen_blank"));
    assert!(
        quiet
            .lines()
            .last()
            .unwrap()
            .starts_with("RESULT ok selftest checks exit=0")
    );
}

fn pid_in(dir: &Path, name: &str) -> Option<i32> {
    std::fs::read_to_string(dir.join(name))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn wait_dead(pid: i32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let stat = String::from_utf8_lossy(&state.stdout).trim().to_string();
        if stat.is_empty() || stat.starts_with('Z') {
            return;
        }
        assert!(Instant::now() < deadline, "process {pid} survived ({stat})");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn timeouts_kill_the_process_group() {
    let sandbox = Sandbox::new();
    let started = Instant::now();
    let output = sandbox.run(&["__test", "sleep", "30s", "--timeout", "1s", "--json", "-q"]);
    assert!(started.elapsed() < Duration::from_secs(15));
    let result = result(&output);
    assert_eq!(result["exit"], 8);
    assert_eq!(result["errors"][0]["id"], "step.timeout");
    assert!(
        result["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("--timeout")
    );

    let run_dir = sandbox.path(&result["run_dir"]);
    let log = run_dir.join("steps").join("01-selftest.sleep.log");
    assert!(std::fs::read_to_string(&log).unwrap().contains("timed out"));
    wait_dead(pid_in(&run_dir, "child.pid").unwrap());
    wait_dead(pid_in(&run_dir, "grandchild.pid").unwrap());
}

fn signal_stops_children_and_writes_a_result(signal: i32, name: &str) {
    let sandbox = Sandbox::new();
    let child = sandbox
        .command(&["__test", "sleep", "60s", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Wait for the step's grandchild to exist.
    let deadline = Instant::now() + Duration::from_secs(20);
    let run_dir = loop {
        let found = std::fs::read_dir(sandbox.runs())
            .ok()
            .and_then(|mut entries| entries.next())
            .and_then(Result::ok)
            .map(|entry| entry.path())
            .filter(|dir| dir.join("grandchild.pid").exists());
        if let Some(dir) = found {
            break dir;
        }
        assert!(Instant::now() < deadline, "the scenario never started");
        std::thread::sleep(Duration::from_millis(50));
    };
    std::thread::sleep(Duration::from_millis(100));

    // SAFETY: sending a signal to the child we spawned.
    unsafe {
        let _ = libc::kill(child.id() as i32, signal);
    }
    let output = child.wait_with_output().unwrap();
    let result = result(&output);
    assert_eq!(result["exit"], 130, "{name}");
    assert_eq!(result["errors"][0]["id"], "run.interrupted");
    assert!(
        result["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains(name)
    );
    assert!(run_dir.join("result.json").exists());
    wait_dead(pid_in(&run_dir, "child.pid").unwrap());
    wait_dead(pid_in(&run_dir, "grandchild.pid").unwrap());
}

#[test]
fn sigterm_stops_children_and_writes_a_result() {
    signal_stops_children_and_writes_a_result(libc::SIGTERM, "SIGTERM");
}

#[test]
fn sighup_stops_children_and_writes_a_result() {
    signal_stops_children_and_writes_a_result(libc::SIGHUP, "SIGHUP");
}

#[test]
fn the_watchdog_writes_the_result_when_the_main_thread_is_busy() {
    let sandbox = Sandbox::new();
    let child = sandbox
        .command(&["__test", "busy", "60s", "--json", "-q"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !sandbox.runs().exists() {
        assert!(Instant::now() < deadline, "the scenario never started");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(200));

    let started = Instant::now();
    // SAFETY: sending a signal to the child we spawned.
    unsafe {
        let _ = libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let output = child.wait_with_output().unwrap();
    assert!(started.elapsed() < Duration::from_secs(15));
    let result = result(&output);
    assert_eq!(result["exit"], 130);
    assert_eq!(result["errors"][0]["id"], "run.interrupted");
    assert!(
        sandbox
            .path(&result["run_dir"])
            .join("result.json")
            .exists()
    );
}

#[test]
fn detach_and_wait() {
    let sandbox = Sandbox::new();
    let started = Instant::now();
    let detached = result(&sandbox.run(&["__test", "sleep", "2s", "--detach", "--json", "-q"]));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "--detach must return at once"
    );
    assert_eq!(detached["exit"], 0);
    assert_eq!(detached["status"], "running");
    let run = detached["run"].as_str().unwrap().to_string();
    assert!(
        detached["next"][0]["cmd"]
            .as_str()
            .unwrap()
            .starts_with(&format!("icm wait {run}"))
    );

    // A short wait while it runs: exit 8, still running, call again.
    let early = result(&sandbox.run(&["wait", &run, "--timeout", "200ms", "--json", "-q"]));
    assert_eq!(early["exit"], 8);
    assert_eq!(early["errors"][0]["id"], "run.still_running");
    assert_eq!(early["status"], "running");
    assert_eq!(early["run"], run.as_str());

    // Waiting long enough replays the run: its events, then its result.
    let output = sandbox.run(&["wait", &run, "--timeout", "30s", "--json"]);
    let events = ndjson(&output);
    assert_eq!(events[0]["type"], "start");
    assert!(events.iter().all(|e| e["run"] == run.as_str()));
    let done = events.last().unwrap();
    assert_eq!(done["exit"], 0);
    assert_eq!(done["command"], "selftest");
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "step" && e["name"] == "selftest.sleep")
    );

    // Human replay prints the same protocol lines.
    let text = stdout(&sandbox.run(&["wait", &run]));
    assert!(text.contains("STEP selftest.sleep ok"), "{text}");
    assert!(
        text.lines()
            .last()
            .unwrap()
            .starts_with("RESULT ok selftest sleep exit=0"),
        "{text}"
    );

    let missing = result(&sandbox.run(&["wait", "20990101T000000Z-x-0000", "--json", "-q"]));
    assert_eq!(missing["exit"], 2);
    assert_eq!(missing["errors"][0]["id"], "run.not_found");
}

#[test]
fn a_detached_failure_keeps_its_exit_code() {
    let sandbox = Sandbox::new();
    let detached = result(&sandbox.run(&[
        "__test",
        "fail",
        "env.jdk_missing",
        "--detach",
        "--json",
        "-q",
    ]));
    let run = detached["run"].as_str().unwrap().to_string();
    let output = sandbox.run(&["wait", &run, "--json", "-q"]);
    let done = result(&output);
    assert_eq!(done["exit"], 4);
    assert_eq!(done["errors"][0]["id"], "env.jdk_missing");
}

/// `--dry-run` on the web, Android and session commands prints a plan and
/// touches nothing: no adb, no browser, no build, no session, no ledger.
#[test]
fn dry_runs_touch_nothing() {
    let sandbox = Sandbox::with_fixture("app");
    let config = sandbox.cwd.join("icm.toml");
    let text = std::fs::read_to_string(&config).unwrap().replace(
        "build = 7\n",
        "build = 7\nplatforms = [\"desktop\", \"web\", \"android\"]\n",
    );
    std::fs::write(&config, text).unwrap();
    let log = sandbox.cwd.join("tools.log");
    let fake = sandbox.cwd.join("fake-tool");
    std::fs::write(
        &fake,
        format!("#!/bin/sh\necho \"$0 $*\" >> '{}'\nexit 1\n", log.display()),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let commands: &[&[&str]] = &[
        &["build", "android"],
        &["run", "android"],
        &["stop", "android", "--shutdown"],
        &["shot", "android"],
        &["logs", "android"],
        &["input", "android", "tap", "1", "2"],
        &["devices", "android"],
        &["build", "web"],
        &["run", "web"],
        &["shot", "web"],
        &["logs", "web"],
        &["input", "web", "tap", "1", "2"],
        &["stop", "web"],
        &["stop", "--all", "--shutdown"],
        &["stop", "ios-sim", "--shutdown"],
        &["input", "ios-sim", "appearance", "dark"],
        &["shot", "ios-sim"],
        &["logs", "ios-sim"],
        &["logs", "desktop"],
        &["build", "--all"],
        &["ledger", "mark-uploaded", "android", "--build", "3"],
    ];
    for args in commands {
        let mut full: Vec<&str> = args.to_vec();
        full.extend(["--dry-run", "--json", "-q"]);
        let mut command = sandbox.command(&full);
        for tool in [
            "ADB",
            "EMULATOR",
            "XCRUN",
            "WASM_BINDGEN",
            "AAPT2",
            "APKSIGNER",
        ] {
            let _ = command.env(format!("ICM_TOOL_{tool}"), &fake);
        }
        let result = result(&command.output().unwrap());
        assert_eq!(result["exit"], 0, "{args:?}: {result}");
        assert_eq!(result["dry_run"], true, "{args:?}: {result}");
        assert!(
            !result["plan"].as_array().unwrap().is_empty(),
            "{args:?}: {result}"
        );
    }
    // `print plan` is the same as --dry-run.
    let printed = result(&sandbox.run(&["print", "plan", "run", "android", "--json", "-q"]));
    assert!(
        printed["plan"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step["name"] == "cargo.rustc"),
        "{printed}"
    );

    assert!(
        !log.exists(),
        "a tool ran: {}",
        std::fs::read_to_string(&log).unwrap()
    );
    let icm = sandbox.cwd.join("target/icm");
    for dir in ["build", "sessions", "locks", "gen"] {
        assert!(!icm.join(dir).exists(), "target/icm/{dir} was created");
    }
    assert!(!sandbox.cwd.join(".icm/ledger.toml").exists());
}

#[test]
fn plans_dry_run_and_execute() {
    let sandbox = Sandbox::new();
    let dry = sandbox.run(&["__test", "plan", "--dry-run", "--json"]);
    let events = ndjson(&dry);
    let plan = events.iter().find(|e| e["type"] == "plan").unwrap();
    let steps = plan["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 3);
    assert_eq!(steps[0]["argv"][0], "/bin/echo");
    assert_eq!(steps[1]["kind"], "internal");
    assert_eq!(steps[2]["env"]["ICM_SELFTEST_TOKEN"], "<redacted>");
    assert_eq!(steps[2]["gates"][0], "run.ready");
    assert!(!events.iter().any(|e| e["type"] == "step"));
    assert_eq!(events.last().unwrap()["dry_run"], true);

    let human = stdout(&sandbox.run(&["__test", "plan", "--dry-run"]));
    assert!(
        human.contains("PLAN 01 selftest.echo: /bin/echo one"),
        "{human}"
    );

    let run = sandbox.run(&["__test", "plan", "--json"]);
    let events = ndjson(&run);
    let ends: Vec<&Value> = events
        .iter()
        .filter(|e| e["type"] == "step" && e["phase"] == "end")
        .collect();
    assert_eq!(ends.len(), 3);
    assert!(ends.iter().all(|e| e["ok"] == true));
    let begin = events
        .iter()
        .find(|e| e["type"] == "step" && e["phase"] == "begin" && e["name"] == "selftest.env")
        .unwrap();
    assert_eq!(begin["env"]["ICM_SELFTEST_TOKEN"], "<redacted>");

    let run_dir = sandbox.path(&events.last().unwrap()["run_dir"]);
    let log = std::fs::read_to_string(run_dir.join("steps").join("03-selftest.env.log")).unwrap();
    assert!(
        log.contains("# env: ICM_SELFTEST_TOKEN=<redacted>"),
        "{log}"
    );
    // The tool echoed the secret; the log keeps only the placeholder.
    assert!(!log.contains("do-not-print-me"), "{log}");
    assert!(
        !std::fs::read_to_string(run_dir.join("events.ndjson"))
            .unwrap()
            .contains("do-not-print-me")
    );
}

#[test]
fn projects_resolve_and_their_runs_live_under_target() {
    let sandbox = Sandbox::with_fixture("app");
    let output = sandbox.run(&["__test", "project", "--json", "-q"]);
    let result = result(&output);
    assert_eq!(result["exit"], 0, "{result}");
    assert_eq!(result["project"]["package"], "fixture-app");
    assert_eq!(result["project"]["bin"], "fixture-app");
    assert_eq!(result["project"]["lib"], "fixture_app");
    assert_eq!(result["app"]["id"], "com.acme.fixture");
    assert_eq!(result["app"]["version"], "0.3.0");
    assert_eq!(result["app"]["build"], 7);
    assert_eq!(result["checks"]["fail"], 0);
    assert!(result["inputs"]["icm_toml_sha256"].as_str().unwrap().len() == 64);
    let run_dir = result["run_dir"].as_str().unwrap();
    assert!(run_dir.starts_with("target/icm/runs/"), "{run_dir}");
    assert!(sandbox.cwd.join(run_dir).join("result.json").exists());
    assert!(sandbox.cwd.join("target/icm/last.json").exists());

    // From a subdirectory, icm.toml is found by walking up.
    let nested = Command::new(BIN)
        .args(["print", "config", "--json", "-q"])
        .current_dir(sandbox.cwd.join("src"))
        .env("ICM_CACHE_DIR", sandbox.cache.path())
        .env("ICM_HOST_CONFIG", sandbox.cache.path().join("none.toml"))
        .env_remove("ICM_CONFIG")
        .output()
        .unwrap();
    let config = self::result(&nested);
    assert_eq!(config["config"]["app"]["name"], "Fixture");
    assert_eq!(config["config"]["android"]["target_sdk"], 36);

    let paths = self::result(&sandbox.run(&["print", "paths", "--json", "-q"]));
    assert_eq!(paths["paths"]["runs"], "target/icm/runs");
}

#[test]
fn two_copies_of_iced_exit_three() {
    let fixture = fixtures().join("twocopies").join("icm.toml");
    let project = tempfile::tempdir().unwrap();
    copy_dir(fixture.parent().unwrap(), project.path());
    let sandbox = Sandbox::new();
    let config = project.path().join("icm.toml");
    let output = sandbox.run(&[
        "__test",
        "project",
        "--config",
        config.to_str().unwrap(),
        "--json",
        "-q",
    ]);
    let result = result(&output);
    assert_eq!(result["exit"], 3);
    assert_eq!(result["errors"][0]["id"], "deps.single_iced");
    let evidence = result["errors"][0]["evidence"].as_array().unwrap();
    assert!(
        evidence
            .iter()
            .any(|e| e["excerpt"].as_str().unwrap_or("").contains("0.13.2"))
    );
    // iced_core from crates.io is also not the fork.
    assert!(
        result["checks"]["failed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id == "deps.iced_not_fork")
    );
}

#[test]
fn config_errors_point_at_the_line() {
    let sandbox = Sandbox::with_fixture("badconfig");
    let result = result(&sandbox.run(&["print", "config", "--json", "-q"]));
    assert_eq!(result["exit"], 3);
    let error = &result["errors"][0];
    assert_eq!(error["id"], "config.unknown_key");
    assert_eq!(error["evidence"][0]["path"], "icm.toml");
    assert_eq!(error["evidence"][0]["line"], 6);
    assert_eq!(error["evidence"][0]["excerpt"], "colour = \"#FFFFFF\"");
    assert!(
        error["detail"]
            .as_str()
            .unwrap()
            .starts_with("icm.toml:6:1: unknown field `colour`")
    );

    // Outside any project.
    let nowhere = Sandbox::new();
    let missing = self::result(&nowhere.run(&["print", "config", "--json", "-q"]));
    assert_eq!(missing["exit"], 3);
    assert_eq!(missing["errors"][0]["id"], "config.not_found");
}

#[test]
fn platform_locks_are_exclusive() {
    let sandbox = Sandbox::with_fixture("app");
    let holder = sandbox
        .command(&["__test", "lock", "web", "4s", "--json", "-q"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let lock = sandbox.cwd.join("target/icm/locks/web.lock");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !std::fs::read_to_string(&lock)
        .unwrap_or_default()
        .contains("pid")
    {
        assert!(Instant::now() < deadline, "the holder never took the lock");
        std::thread::sleep(Duration::from_millis(50));
    }

    let busy = result(&sandbox.run(&["__test", "lock", "web", "1ms", "--json", "-q"]));
    assert_eq!(busy["exit"], 7);
    assert_eq!(busy["errors"][0]["id"], "run.lock_busy");

    let other = result(&sandbox.run(&["__test", "lock", "android", "1ms", "--json", "-q"]));
    assert_eq!(other["exit"], 0);

    let waited = result(&sandbox.run(&[
        "__test",
        "lock",
        "web",
        "1ms",
        "--wait-lock",
        "20s",
        "--json",
        "-q",
    ]));
    assert_eq!(waited["exit"], 0);
    let _ = holder.wait_with_output().unwrap();
}

#[test]
fn deployment_target_changes_relink_the_app() {
    let sandbox = Sandbox::with_fixture("app");
    let triple = if cfg!(target_arch = "x86_64") {
        "x86_64-apple-ios"
    } else {
        "aarch64-apple-ios-sim"
    };
    let cleaned = |result: &Value| -> bool {
        result["checks"]["failed"].as_array().unwrap().is_empty()
            && std::fs::read_dir(sandbox.path(&result["run_dir"]).join("steps"))
                .unwrap()
                .any(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .contains("cargo.clean.deployment_target")
                })
    };

    // Nothing built yet: no clean.
    let fresh = result(&sandbox.run(&["__test", "deployment", "16.0", "--json", "-q"]));
    assert_eq!(fresh["exit"], 0, "{fresh}");
    assert_eq!(fresh["env"]["IPHONEOS_DEPLOYMENT_TARGET"], "16.0");
    assert!(!cleaned(&fresh));

    // Same value again: no clean.
    std::fs::create_dir_all(sandbox.cwd.join("target").join(triple).join("debug")).unwrap();
    let same = result(&sandbox.run(&["__test", "deployment", "16.0", "--json", "-q"]));
    assert!(!cleaned(&same));

    // A new minimum OS: the app package is cleaned so it relinks.
    let changed = result(&sandbox.run(&["__test", "deployment", "17.0", "--json", "-q"]));
    assert_eq!(changed["exit"], 0, "{changed}");
    assert!(cleaned(&changed));
    let stamp = std::fs::read_to_string(
        sandbox
            .cwd
            .join(format!("target/icm/stamps/deployment-{triple}-debug.txt")),
    )
    .unwrap();
    assert_eq!(stamp.trim(), "IPHONEOS_DEPLOYMENT_TARGET=17.0");
}

#[test]
fn print_env_web_works_without_a_project() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["print", "env", "web"]);
    assert!(output.status.success());
    assert_eq!(stdout(&output), "export RUSTUP_AUTO_INSTALL=0\n");
}

#[test]
fn print_policy_shows_the_dated_table() {
    let sandbox = Sandbox::new();
    let mut command = sandbox.command(&["print", "policy", "--json", "-q"]);
    let _ = command.env("ICM_TODAY", "2026-10-07");
    let result = result(&command.output().unwrap());
    assert_eq!(result["exit"], 0, "{result}");
    assert_eq!(result["policy"]["reviewed"], "2026-10-06");
    assert_eq!(result["policy"]["stale"], false);
    let rules = result["policy"]["rules"].as_array().unwrap();
    let play = rules
        .iter()
        .find(|rule| rule["id"] == "play.target_sdk")
        .unwrap();
    assert_eq!(play["in_force"], 36);

    // A year later the table is stale: a WARN, still exit 0.
    let mut command = sandbox.command(&["print", "policy", "--json", "-q"]);
    let _ = command.env("ICM_TODAY", "2027-10-07");
    let stale = self::result(&command.output().unwrap());
    assert_eq!(stale["exit"], 0, "{stale}");
    assert_eq!(stale["warnings"][0]["id"], "env.policy_stale");

    // Human mode prints the table on stdout.
    let output = sandbox.run(&["print", "policy"]);
    assert!(stdout(&output).contains("play.target_sdk"));
}

#[test]
fn print_commands_lists_the_surface() {
    let result = result(&Sandbox::new().run(&["print", "commands", "--json", "-q"]));
    let names: Vec<&str> = result["commands"]["subcommands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    for expected in [
        "new",
        "doctor",
        "check",
        "run",
        "logs",
        "shot",
        "input",
        "ui",
        "test",
        "stop",
        "explain",
        "wait",
        "print",
        "release",
        "verify",
        "upload-commands",
        "ledger",
        "diagnose",
    ] {
        assert!(
            names.contains(&expected),
            "{expected} missing from {names:?}"
        );
    }
    assert!(!names.contains(&"__test"));
}
