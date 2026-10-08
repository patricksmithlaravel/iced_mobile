//! `icm run|logs|shot|stop|input desktop` end to end, with the
//! `fixtures/desktop` stand-in app (no window, no dependencies). Each test
//! gets its own copy of the fixture, cache dir and target dir, and stops
//! every app it started.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

#[path = "support/secret.rs"]
mod secret;

const BIN: &str = env!("CARGO_BIN_EXE_icm");

struct Sandbox {
    cache: tempfile::TempDir,
    project: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Sandbox {
        let project = tempfile::tempdir().unwrap();
        copy_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/desktop"),
            project.path(),
        );
        Sandbox {
            cache: tempfile::tempdir().unwrap(),
            project,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with(args, &[])
    }

    /// [`Sandbox::run`] with more variables in icm's environment.
    fn run_with(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(BIN);
        let _ = command
            .args(args)
            .current_dir(self.project.path())
            .env("ICM_CACHE_DIR", self.cache.path())
            .env("ICM_HOST_CONFIG", self.cache.path().join("no-host.toml"))
            .env("CARGO_TARGET_DIR", self.project.path().join("target"))
            .envs(env.iter().copied())
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
        command.output().unwrap()
    }

    /// The result line of a `--json -q` run.
    fn result(&self, args: &[&str]) -> Value {
        self.result_with(args, &[])
    }

    /// [`Sandbox::result`] with more variables in icm's environment.
    fn result_with(&self, args: &[&str], env: &[(&str, &str)]) -> Value {
        let mut full = args.to_vec();
        full.extend(["--json", "-q"]);
        let output = self.run_with(&full, env);
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let last = text.lines().last().unwrap_or_else(|| {
            panic!(
                "no output from {args:?}; stderr: {}",
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

    fn path(&self, value: &Value) -> PathBuf {
        let path = PathBuf::from(value.as_str().expect("a path"));
        if path.is_absolute() {
            path
        } else {
            self.project.path().join(path)
        }
    }

    fn session(&self) -> PathBuf {
        self.project.path().join("target/icm/sessions/desktop.json")
    }
}

/// Kills the apps a test started, whatever happens.
struct Apps(Vec<i32>);

impl Drop for Apps {
    fn drop(&mut self) {
        for pid in &self.0 {
            // SAFETY: kill(2) on a pid this test started.
            unsafe {
                let _ = libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
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

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn wait_dead(pid: i32) {
    let until = Instant::now() + Duration::from_secs(10);
    while alive(pid) {
        assert!(Instant::now() < until, "pid {pid} is still alive");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn ids(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn run_logs_shot_and_stop() {
    let sandbox = Sandbox::new();
    let mut apps = Apps(Vec::new());

    let first = sandbox.result(&["run", "desktop", "--settle", "200ms"]);
    assert_eq!(first["exit"], 0, "{first}");
    let pid = first["process"]["pid"].as_i64().unwrap() as i32;
    apps.0.push(pid);
    assert!(alive(pid));
    assert_eq!(first["process"]["alive"], true);
    assert_eq!(first["process"]["ready"]["source"], "icm_event");
    assert_eq!(first["process"]["backend"], "tiny-skia");
    assert_eq!(first["profile"], "debug");
    assert_eq!(first["device"]["kind"], "desktop");
    assert!(first["checks"]["pass"].as_u64().unwrap() >= 2, "{first}");

    // No window and no harness: the run is still ok, without a screenshot.
    assert!(
        ids(&first["warnings"]).contains(&"harness.missing".to_string()),
        "{first}"
    );
    assert!(first["artifacts"]["screenshot"].is_null());

    // The executable was linked into the build dir and runs from there.
    let bundle = sandbox.path(&first["artifacts"]["bundle"]);
    assert!(bundle.ends_with("target/icm/build/desktop/debug/fixture-desktop"));
    let session: Value =
        serde_json::from_str(&std::fs::read_to_string(sandbox.session()).unwrap()).unwrap();
    assert_eq!(session["pid"], pid);
    assert_eq!(session["window"]["physical"][0], 800);

    // The app got ICM_EVENTS=1 and its output is in the run directory.
    let app_log = std::fs::read_to_string(sandbox.path(&first["artifacts"]["app_log"])).unwrap();
    assert!(app_log.contains("hello from stdout"), "{app_log}");
    assert!(
        app_log.contains("WARN  stderr: warning: a fixture warning"),
        "{app_log}"
    );

    // logs re-read the live files, filtered.
    let warn = sandbox.result(&["logs", "desktop", "--level", "warn"]);
    assert_eq!(warn["exit"], 0, "{warn}");
    let records = warn["records"].as_array().unwrap();
    assert_eq!(records.len(), 1, "{warn}");
    assert_eq!(records[0]["msg"], "warning: a fixture warning");
    assert_eq!(warn["process"]["alive"], true);
    let hello = sandbox.result(&["logs", "desktop", "--grep", "hello|nothing"]);
    assert_eq!(hello["records"][0]["source"], "stdout");
    assert_eq!(hello["counts"]["shown"], 1);

    // Human logs print LOG lines.
    let human = sandbox.run(&["logs", "desktop", "--source", "app"]);
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(
        text.contains("LOG warn stderr: warning: a fixture warning"),
        "{text}"
    );

    // A second run replaces the first app.
    let second = sandbox.result(&["run", "desktop", "--settle", "200ms", "--no-shot"]);
    assert_eq!(second["exit"], 0, "{second}");
    let second_pid = second["process"]["pid"].as_i64().unwrap() as i32;
    apps.0.push(second_pid);
    assert_eq!(second["replaced"]["pid"], pid);
    wait_dead(pid);
    assert!(alive(second_pid));

    // shot works on the running app (a WARN without window or harness).
    let shot = sandbox.result(&["shot", "desktop"]);
    assert_eq!(shot["exit"], 0, "{shot}");
    assert_eq!(shot["process"]["pid"], second_pid);

    // input is not a desktop feature in phase 1.
    let input = sandbox.result(&["input", "desktop", "tap", "10", "10"]);
    assert_eq!(input["exit"], 2);
    assert_eq!(input["errors"][0]["id"], "input.unsupported");

    // stop ends it and is idempotent.
    let stop = sandbox.result(&["stop", "desktop"]);
    assert_eq!(stop["exit"], 0, "{stop}");
    assert_eq!(stop["stopped"][0]["pid"], second_pid);
    wait_dead(second_pid);
    assert!(!sandbox.session().exists());
    let again = sandbox.result(&["stop", "desktop"]);
    assert_eq!(again["exit"], 0);
    assert_eq!(again["stopped"], serde_json::json!([]));

    // Without an app, shot fails with run.no_session; logs still read the
    // last run.
    let none = sandbox.result(&["shot", "desktop"]);
    assert_eq!(none["exit"], 7);
    assert_eq!(none["errors"][0]["id"], "run.no_session");
    let last = sandbox.result(&["logs", "desktop"]);
    assert_eq!(last["exit"], 0, "{last}");
    assert_eq!(last["process"]["alive"], false);
    assert!(last["counts"]["total"].as_u64().unwrap() >= 2);

    // --no-build launches the last build.
    let rerun = sandbox.result(&[
        "run",
        "desktop",
        "--no-build",
        "--no-shot",
        "--settle",
        "100ms",
    ]);
    assert_eq!(rerun["exit"], 0, "{rerun}");
    let rerun_pid = rerun["process"]["pid"].as_i64().unwrap() as i32;
    apps.0.push(rerun_pid);
    assert_eq!(sandbox.result(&["stop", "desktop"])["exit"], 0);
    wait_dead(rerun_pid);
}

#[test]
fn panics_exits_and_hangs_fail_with_exit_ten() {
    let sandbox = Sandbox::new();
    let _apps = Apps(Vec::new());

    let panicked = sandbox.result(&["run", "desktop", "--env", "ICM_FIXTURE=panic"]);
    assert_eq!(panicked["exit"], 10, "{panicked}");
    let error = &panicked["errors"][0];
    assert_eq!(error["id"], "run.app_panicked");
    assert!(
        error["detail"]
            .as_str()
            .unwrap()
            .starts_with("panicked at src/main.rs:"),
        "{error}"
    );
    assert!(
        error["detail"]
            .as_str()
            .unwrap()
            .contains("index out of bounds")
    );
    let evidence = error["evidence"].as_array().unwrap();
    assert!(
        evidence[0]["path"]
            .as_str()
            .unwrap()
            .ends_with("app.stderr")
    );
    assert!(evidence[0]["line"].as_u64().is_some());
    assert!(
        evidence
            .iter()
            .any(|e| e["path"].as_str().unwrap().ends_with("src/main.rs"))
    );
    assert!(
        error["likely_causes"][0]
            .as_str()
            .unwrap()
            .starts_with("a bug at src/main.rs:")
    );
    assert_eq!(panicked["process"]["alive"], false);
    assert!(!sandbox.session().exists());

    // The dead app's logs stay readable; the panic is one error record.
    let logs = sandbox.result(&["logs", "desktop", "--level", "error"]);
    assert_eq!(logs["exit"], 0, "{logs}");
    assert_eq!(logs["process"]["alive"], false);
    let panic = logs["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["tag"] == "panic")
        .unwrap_or_else(|| panic!("{logs}"));
    assert!(
        panic["msg"]
            .as_str()
            .unwrap()
            .contains("index out of bounds")
    );

    let exited = sandbox.result(&["run", "desktop", "--env", "ICM_FIXTURE=exit"]);
    assert_eq!(exited["exit"], 10, "{exited}");
    assert_eq!(exited["errors"][0]["id"], "run.app_died");
    assert_eq!(exited["process"]["exit"]["code"], 3);
    assert!(
        exited["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("error: the fixture gives up")
    );

    // No events and no window: never ready (on Linux the probe only needs
    // the pid alive, so this case is macOS's).
    if cfg!(target_os = "macos") {
        let hung = sandbox.result(&[
            "run",
            "desktop",
            "--env",
            "ICM_FIXTURE=hang",
            "--wait-ready",
            "4s",
        ]);
        assert_eq!(hung["exit"], 10, "{hung}");
        assert_eq!(hung["errors"][0]["id"], "run.not_ready");
        let pid = hung["process"]["pid"].as_i64().unwrap() as i32;
        wait_dead(pid);
        assert!(hung["process"]["exit"]["stopped_by_icm"].is_string());
    }

    // A bad --env is a usage error.
    let bad = sandbox.result(&["run", "desktop", "--env", "NOEQUALS", "--no-build"]);
    assert_eq!(bad["exit"], 2);
}

/// What a command keeps in its run directory holds no secret the app
/// logged: the value of a secret-named variable in icm's environment, which
/// the app inherits and logs plain, in a JSON line, in an `ICM_EVENT` and in
/// a panic, is `<redacted>` in the copies of its stdout and stderr, in
/// `app.log` and `logs.ndjson`, the step logs, events and results, raw or
/// escaped. The live files in `target/icm/sessions` are the app's own
/// output and keep it, and they are the only files under `target/` that
/// do: icm writes no inherited value anywhere, `secrets.json` included.
#[test]
fn run_directories_keep_no_secret() {
    let sandbox = Sandbox::new();
    let mut apps = Apps(Vec::new());
    let env = [(secret::NAME, secret::TOKEN)];
    let icm = sandbox.project.path().join("target/icm");

    let run = sandbox.result_with(&["run", "desktop", "--settle", "200ms"], &env);
    assert_eq!(run["exit"], 0, "{run}");
    let pid = run["process"]["pid"].as_i64().unwrap() as i32;
    apps.0.push(pid);
    let app_log = std::fs::read_to_string(sandbox.path(&run["artifacts"]["app_log"])).unwrap();
    assert!(app_log.contains("signed in with <redacted>"), "{app_log}");
    assert!(app_log.contains("{\"token\":\"<redacted>\"}"), "{app_log}");
    let stderr = std::fs::read_to_string(sandbox.path(&run["artifacts"]["stderr"])).unwrap();
    assert!(stderr.contains("token <redacted>"), "{stderr}");
    let live = icm
        .join("sessions/desktop")
        .join(run["run"].as_str().unwrap());
    assert!(secret::holds(&live.join("app.stdout")));
    assert!(secret::holds(&live.join("app.stderr")));

    let logs = sandbox.result_with(&["logs", "desktop"], &env);
    assert_eq!(logs["exit"], 0, "{logs}");
    assert!(
        logs.to_string().contains("signed in with <redacted>"),
        "{logs}"
    );
    assert_eq!(sandbox.result_with(&["stop", "desktop"], &env)["exit"], 0);
    wait_dead(pid);

    let leaked = sandbox.result_with(&["run", "desktop", "--env", "ICM_FIXTURE=leak"], &env);
    assert_eq!(leaked["exit"], 10, "{leaked}");
    assert_eq!(leaked["errors"][0]["id"], "run.app_panicked");
    assert!(
        leaked["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("rejected token <redacted>"),
        "{leaked}"
    );
    let stderr =
        std::fs::read_to_string(sandbox.path(&leaked["errors"][0]["evidence"][0]["path"])).unwrap();
    assert!(stderr.contains("rejected token <redacted>"), "{stderr}");

    secret::assert_kept_nowhere(&icm.join("runs"));
    secret::assert_kept_nowhere(&icm.join("latest"));
    let last = std::fs::read_to_string(icm.join("last.json")).unwrap();
    assert!(
        secret::forms()
            .iter()
            .all(|form| !last.contains(form.as_str()))
    );
    let live = icm
        .join("sessions/desktop")
        .join(leaked["run"].as_str().unwrap());
    let holders: Vec<PathBuf> = secret::leaks(&sandbox.project.path().join("target"))
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    assert!(
        holders
            .iter()
            .all(|path| *path == live.join("app.stdout") || *path == live.join("app.stderr")),
        "{holders:?}"
    );
}

/// A later command whose environment lacks the secret (`icm logs` from
/// another shell after `icm run desktop --env ICM_TEST_API_TOKEN=…`) still
/// redacts what the app logged: the session keeps the values of the app's
/// secret-named `--env` in a 0600 file next to its live files, and every
/// command of the project reads them. A secret-named variable of the
/// user's shell that icm and the app only inherit (another tool's token)
/// reaches no file under `target/`, whichever command had it.
#[test]
fn later_commands_without_the_secret_keep_none() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new();
    let mut apps = Apps(Vec::new());
    let target = sandbox.project.path().join("target");
    let icm = target.join("icm");
    let token = format!("{}={}", secret::NAME, secret::TOKEN);
    let pair = "ICM_TEST_DB_PASSWORD=given-with-env-99";
    let shell = [(secret::INHERITED_NAME, secret::INHERITED)];

    let run = sandbox.result_with(
        &[
            "run", "desktop", "--settle", "200ms", "--env", &token, "--env", pair,
        ],
        &shell,
    );
    assert_eq!(run["exit"], 0, "{run}");
    let pid = run["process"]["pid"].as_i64().unwrap() as i32;
    apps.0.push(pid);
    let live = icm
        .join("sessions/desktop")
        .join(run["run"].as_str().unwrap());
    assert!(secret::holds(&live.join("app.stdout")));
    let kept = live.join("secrets.json");
    let mode = std::fs::metadata(&kept).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let text = std::fs::read_to_string(&kept).unwrap();
    assert!(text.contains("given-with-env-99"), "{text}");
    assert!(text.contains(secret::TAIL), "{text}");
    secret::assert_inherited_nowhere(&target);

    for args in [
        &["logs", "desktop"][..],
        &["logs", "desktop", "--raw"],
        &["shot", "desktop"],
        &["stop", "desktop"],
        &["logs", "desktop"],
    ] {
        let result = sandbox.result_with(args, &shell);
        assert_eq!(result["exit"], 0, "{args:?}: {result}");
    }
    wait_dead(pid);

    secret::assert_kept_nowhere(&icm.join("runs"));
    secret::assert_kept_nowhere(&icm.join("latest"));
    let last = std::fs::read_to_string(icm.join("last.json")).unwrap();
    assert!(last.contains("signed in with <redacted>"), "{last}");
    assert!(
        secret::forms()
            .iter()
            .all(|form| !last.contains(form.as_str()))
    );

    // The next run, without the secret, removes the old live files with
    // what they kept.
    let next = sandbox.result_with(
        &["run", "desktop", "--settle", "200ms", "--no-build"],
        &shell,
    );
    assert_eq!(next["exit"], 0, "{next}");
    apps.0.push(next["process"]["pid"].as_i64().unwrap() as i32);
    assert!(!kept.exists());
    let again = icm
        .join("sessions/desktop")
        .join(next["run"].as_str().unwrap())
        .join("secrets.json");
    assert!(!again.exists());
    for args in [
        &["logs", "desktop"][..],
        &["shot", "desktop"],
        &["stop", "desktop"],
    ] {
        let result = sandbox.result_with(args, &shell);
        assert_eq!(result["exit"], 0, "{args:?}: {result}");
    }
    secret::assert_inherited_nowhere(&target);
}

#[test]
fn dry_runs_print_the_plan() {
    let sandbox = Sandbox::new();
    let plan = sandbox.result(&["run", "desktop", "--dry-run"]);
    assert_eq!(plan["exit"], 0, "{plan}");
    let names: Vec<&str> = plan["plan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["name"].as_str().unwrap())
        .collect();
    assert_eq!(names[0], "cargo.build");
    assert!(names.contains(&"desktop.launch") && names.contains(&"desktop.ready"));
    let build = &plan["plan"][0];
    let display = build["display"].as_str().unwrap();
    assert!(
        display.contains("--config 'profile.dev.package.\"*\".opt-level=2'"),
        "{display}"
    );
    if cfg!(target_os = "macos") {
        assert!(
            build["env"]["MACOSX_DEPLOYMENT_TARGET"].is_string(),
            "{build}"
        );
    }
    let launch = plan["plan"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["name"] == "desktop.launch")
        .unwrap();
    assert_eq!(launch["env"]["ICM_EVENTS"], "1");
    // `print plan <command>` is the same, with --json and -q after the
    // command applying to the output.
    let printed = sandbox.result(&["print", "plan", "run", "desktop"]);
    assert_eq!(printed["exit"], 0, "{printed}");
    assert_eq!(printed["plan"][0]["name"], "cargo.build", "{printed}");

    // Nothing ran, and `latest/desktop` still means the last real run.
    assert!(!sandbox.project.path().join("target/icm/build").exists());
    assert!(
        !sandbox
            .project
            .path()
            .join("target/icm/latest/desktop")
            .exists()
    );
}

#[test]
fn run_runs_the_projects_desktop_hooks() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new();
    let mut apps = Apps(Vec::new());
    let toml = sandbox.project.path().join("icm.toml");
    let mut text = std::fs::read_to_string(&toml).unwrap();
    text.push_str("\n[checks]\ndesktop = [\"checks.sh\"]\n");
    std::fs::write(&toml, text).unwrap();
    let script = sandbox.project.path().join("checks.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nif kill -0 \"$ICM_PID\"; then echo \"CHECK PASS app_alive: pid $ICM_PID on $ICM_PLATFORM\"; else echo 'CHECK FAIL app_alive: gone'; fi\n[ -f \"$ICM_APP_STDERR\" ] || echo 'CHECK FAIL stderr: no ICM_APP_STDERR'\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let run = sandbox.result(&["run", "desktop", "--settle", "100ms", "--no-shot"]);
    let pid = run["process"]["pid"].as_i64().unwrap() as i32;
    apps.0.push(pid);
    assert_eq!(run["exit"], 0, "{run}");
    assert_eq!(run["hooks"][0]["script"], "checks.sh", "{run}");
    assert_eq!(run["hooks"][0]["ok"], true, "{run}");
    assert_eq!(
        run["hooks"][0]["checks"][0]["id"], "hook.app_alive",
        "{run}"
    );

    // A failing hook is a non-blocking FAIL: the app keeps running, exit 1.
    std::fs::write(
        &script,
        "#!/bin/sh\necho 'CHECK FAIL smoke: the login screen is missing'\n",
    )
    .unwrap();
    let failed = sandbox.result(&["run", "desktop", "--settle", "100ms", "--no-shot"]);
    let second = failed["process"]["pid"].as_i64().unwrap() as i32;
    apps.0.push(second);
    assert_eq!(failed["exit"], 1, "{failed}");
    assert!(
        failed["checks"]["failed"]
            .as_array()
            .unwrap()
            .contains(&Value::from("hook.smoke")),
        "{failed}"
    );
    assert!(alive(second));
    assert_eq!(sandbox.result(&["stop", "desktop"])["exit"], 0);
    wait_dead(second);
}

#[test]
fn build_all_builds_every_app_platform() {
    let sandbox = Sandbox::new();
    // The fixture's [app] platforms is just the desktop.
    let all = sandbox.result(&["build", "--all"]);
    assert_eq!(all["exit"], 0, "{all}");
    assert_eq!(all["built"], serde_json::json!(["desktop"]), "{all}");
    assert!(
        sandbox
            .path(&all["artifacts"]["bundle"])
            .ends_with("target/icm/build/desktop/debug/fixture-desktop"),
        "{all}"
    );
    // No platform means the same.
    let bare = sandbox.result(&["build"]);
    assert_eq!(bare["built"], serde_json::json!(["desktop"]), "{bare}");
}
