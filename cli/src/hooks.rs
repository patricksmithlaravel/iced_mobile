//! Project hooks (design §13.6): extra checks a project keeps as scripts,
//! the way Tawara keeps its own.
//!
//! ```toml
//! [checks]
//! ios-sim = ["platform/ios/checks.sh"]
//! android = ["platform/android/checks.sh", "scripts/smoke.py"]
//! ```
//!
//! `run` calls [`run`] after a successful launch (and `test --on`, in a
//! later phase, after each step). Each script:
//!
//! - runs from the project directory (where icm.toml is), with stdin closed,
//!   in its own process group, for at most [`TIMEOUT`];
//! - is executed directly when it is executable, else with `/bin/sh`;
//! - gets the environment of [`HookContext`]: `ICM_PLATFORM`, `ICM_RUN_DIR`,
//!   `ICM_PID`, `ICM_DEVICE`, `ICM_ADB`, `ICM_APP_ID`, `ICM_BIN`,
//!   `ICM_APP_STDERR`, `ICM_LOGS`, `ICM_LOG_MARK`, `ICM_SIM_DATA` and
//!   `ICM_PROJECT_DIR` (unset when unknown), plus the platform's tool
//!   environment (Android hooks get `JAVA_HOME` and `PATH`, Appendix C
//!   item 2);
//! - reports checks by printing `CHECK PASS <name>: <detail>` or
//!   `CHECK FAIL <name>: <detail>` (also `WARN`, `SKIP`, `INFO`) on stdout,
//!   which become `hook.<name>` checks, secret values redacted;
//! - fails as `hook.<script>` when it exits non-zero, times out or is
//!   missing. A script that exits 0 and prints no CHECK line passes as
//!   `hook.<script>`.
//!
//! Hook failures are non-blocking FAILs: the command finishes and exits 1.

use crate::catalogue::CheckId;
use crate::context::{Ctx, Project};
use crate::error::{Check, Evidence, IcmError, Result, Status};
use crate::process::Cmd;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long one hook may run.
pub const TIMEOUT: Duration = Duration::from_secs(300);

/// What a hook is told about the app it checks.
#[derive(Clone, Debug, Default)]
pub struct HookContext {
    /// The dev platform (`ICM_PLATFORM`), e.g. `ios-sim`; it picks the
    /// `[checks]` entry.
    pub platform: String,
    /// The app's process id (`ICM_PID`).
    pub pid: Option<u32>,
    /// The simulator UDID or device serial (`ICM_DEVICE`).
    pub device: Option<String>,
    /// The adb command line for the device, e.g. `/sdk/platform-tools/adb
    /// -s emulator-5580` (`ICM_ADB`).
    pub adb: Option<String>,
    /// The built executable or bundle (`ICM_BIN`).
    pub bin: Option<PathBuf>,
    /// The app's stderr file (`ICM_APP_STDERR`).
    pub app_stderr: Option<PathBuf>,
    /// The normalized logs (`ICM_LOGS`, `logs.ndjson`).
    pub logs: Option<PathBuf>,
    /// The launch mark logs are read from (`ICM_LOG_MARK`).
    pub log_mark: Option<String>,
    /// The app's data container on the simulator (`ICM_SIM_DATA`).
    pub sim_data: Option<PathBuf>,
    /// More environment for the scripts (the platform's tool environment).
    pub env: Vec<(String, String)>,
    /// How long each script may run (default [`TIMEOUT`]).
    pub timeout: Option<Duration>,
}

/// One script's outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookReport {
    /// The script as written in icm.toml.
    pub script: String,
    /// Its own check id, `hook.<script stem>`.
    pub id: String,
    /// Its exit code, if it exited.
    pub exit: Option<i32>,
    /// Whether it passed: exit 0 and no FAIL line.
    pub ok: bool,
    /// How long it ran.
    pub ms: u64,
    /// Its log.
    pub log: Option<String>,
    /// The `hook.<name>` checks it reported, with their status.
    pub checks: Vec<(String, Status)>,
}

impl HookReport {
    /// The entry of the result's `hooks` array.
    pub fn to_json(&self) -> Value {
        json!({
            "script": self.script,
            "id": self.id,
            "exit": self.exit,
            "ok": self.ok,
            "ms": self.ms,
            "log": self.log,
            "checks": self.checks.iter().map(|(id, status)| json!({"id": id, "status": status})).collect::<Vec<_>>(),
        })
    }
}

/// Runs the project's `[checks]` scripts for `context.platform`, reports
/// their checks, and sets the result's `hooks` field. Failing hooks are
/// non-blocking FAILs; the error returned is only for an interrupt or the
/// overall `--timeout`.
pub fn run(ctx: &mut Ctx, context: &HookContext) -> Result<Vec<HookReport>> {
    let project = ctx.project()?.clone();
    run_for(ctx, &project, context)
}

/// Whether the project has `[checks]` scripts for a platform (so a
/// platform can skip gathering what only hooks need).
pub fn configured(project: &Project, platform: &str) -> bool {
    project
        .config
        .config
        .checks
        .get(platform)
        .is_some_and(|scripts| !scripts.is_empty())
}

/// [`run`] for a known project, from a shared context (the platforms'
/// `run` call it after a successful launch).
pub fn run_for(ctx: &Ctx, project: &Project, context: &HookContext) -> Result<Vec<HookReport>> {
    let scripts = project
        .config
        .config
        .checks
        .get(&context.platform)
        .cloned()
        .unwrap_or_default();
    let dir = project.dir().to_path_buf();
    let app_id = project.app().id.clone();
    if scripts.is_empty() {
        return Ok(Vec::new());
    }

    let reports = run_scripts(ctx, &dir, &app_id, &scripts, context)?;
    ctx.rep.set(
        "hooks",
        Value::Array(reports.iter().map(HookReport::to_json).collect()),
    );
    Ok(reports)
}

/// Runs the given scripts (relative to `dir`) as hooks.
pub fn run_scripts(
    ctx: &Ctx,
    dir: &Path,
    app_id: &str,
    scripts: &[String],
    context: &HookContext,
) -> Result<Vec<HookReport>> {
    let mut reports = Vec::new();
    for script in scripts {
        reports.push(run_one(ctx, dir, app_id, script, context)?);
    }
    Ok(reports)
}

/// The id part for a name: lower case letters, digits and `_`, starting
/// with a letter (`Login-Screen` → `login_screen`).
pub fn sanitize(name: &str) -> String {
    let name = name.trim();
    let name = name.strip_prefix("hook.").unwrap_or(name);
    let mapped: String = name
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = mapped.trim_matches('_');
    if trimmed.is_empty() {
        "hook".to_string()
    } else if trimmed.starts_with(|c: char| c.is_ascii_lowercase()) {
        trimmed.to_string()
    } else {
        format!("h_{trimmed}")
    }
}

/// `hook.<stem>` for a script path.
pub fn script_id(script: &str) -> String {
    let stem = Path::new(script)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| script.to_string());
    format!("hook.{}", sanitize(&stem))
}

/// A `CHECK <STATUS> <name>[: <detail>]` line from a hook.
pub fn parse_check_line(line: &str) -> Option<(Status, String, String)> {
    let rest = line.trim_end().strip_prefix("CHECK ")?;
    let (word, rest) = rest.split_once(' ')?;
    let status = match word {
        "PASS" => Status::Pass,
        "FAIL" => Status::Fail,
        "WARN" => Status::Warn,
        "SKIP" => Status::Skip,
        "INFO" => Status::Info,
        _ => return None,
    };
    let (name, detail) = match rest.split_once(':') {
        Some((name, detail)) => (name.trim(), detail.trim()),
        None => (rest.trim(), ""),
    };
    if name.is_empty() || name.contains(char::is_whitespace) {
        return None;
    }
    Some((status, sanitize(name), detail.to_string()))
}

fn hook_check(id_part: &str, status: Status, detail: String, evidence: Option<Evidence>) -> Check {
    let mut error = IcmError::hook(id_part, detail);
    if let Some(evidence) = evidence {
        error = error.evidence(evidence);
    }
    Check::from_error(error, status)
}

fn run_one(
    ctx: &Ctx,
    dir: &Path,
    app_id: &str,
    script: &str,
    context: &HookContext,
) -> Result<HookReport> {
    let id = script_id(script);
    let id_part = id.trim_start_matches("hook.").to_string();
    let path = if Path::new(script).is_absolute() {
        PathBuf::from(script)
    } else {
        dir.join(script)
    };
    let mut report = HookReport {
        script: script.to_string(),
        id: id.clone(),
        exit: None,
        ok: false,
        ms: 0,
        log: None,
        checks: Vec::new(),
    };

    if !path.is_file() {
        ctx.rep.check(
            hook_check(
                &id_part,
                Status::Fail,
                format!(
                    "the hook script `{script}` ([checks] {}) does not exist at {}",
                    context.platform,
                    crate::paths::display(&path)
                ),
                None,
            )
            .fix(
                "Create the script, or remove it from [checks] in icm.toml.",
                &[],
            ),
        );
        return Ok(report);
    }

    let cmd = command(&path, dir, app_id, context, ctx);
    let step = format!("hook.{id_part}");

    let rep = ctx.rep.clone();
    let mut reported: Vec<(String, Status)> = Vec::new();
    let mut on_line = |line: &str| {
        // Redacted before parsing: a secret in the name would otherwise
        // reach the check id sanitized, where the reporter cannot see it.
        let line = crate::process::redact_values(line);
        if let Some((status, name, detail)) = parse_check_line(&line) {
            let detail = if detail.is_empty() {
                format!("reported by {script}")
            } else {
                detail
            };
            rep.check(hook_check(&name, status, detail, None));
            reported.push((format!("hook.{name}"), status));
        }
    };

    let outcome = match ctx.step_with(&step, &cmd, Some(&mut on_line)) {
        Ok(outcome) => outcome,
        Err(error) if error.check_id() == Some(CheckId::StepTimeout) && !overall_spent(ctx) => {
            let limit = context.timeout.unwrap_or(TIMEOUT);
            report.checks = reported;
            report.ms = limit.as_millis() as u64;
            let mut check = hook_check(
                &id_part,
                Status::Fail,
                format!(
                    "{script} did not finish within {}; its process group was killed",
                    crate::time::format_duration(limit)
                ),
                None,
            );
            check.error.evidence = error.evidence;
            ctx.rep.check(check);
            return Ok(report);
        }
        Err(error) => return Err(error),
    };

    report.exit = outcome.code();
    report.ms = outcome.duration.as_millis() as u64;
    report.log = outcome.log.as_deref().map(crate::paths::display);
    let any_failed = reported.iter().any(|(_, status)| *status == Status::Fail);
    report.checks = reported;

    if outcome.success() {
        report.ok = !any_failed;
        if report.checks.is_empty() {
            ctx.rep.check(hook_check(
                &id_part,
                Status::Pass,
                format!("{script} exited 0"),
                None,
            ));
        }
    } else {
        let tail = outcome.stderr_tail(6);
        let mut detail = format!("{script} failed ({})", outcome.describe());
        if !tail.is_empty() {
            detail.push_str(&format!(":\n{tail}"));
        }
        let evidence = outcome.log.as_ref().map(|log| {
            Evidence::file(log).with_excerpt(tail.lines().last().unwrap_or("").to_string())
        });
        ctx.rep
            .check(hook_check(&id_part, Status::Fail, detail, evidence));
    }

    Ok(report)
}

/// Whether the overall `--timeout` has run out.
fn overall_spent(ctx: &Ctx) -> bool {
    ctx.remaining().is_some_and(|left| left.is_zero())
}

fn command(path: &Path, dir: &Path, app_id: &str, context: &HookContext, ctx: &Ctx) -> Cmd {
    let mut cmd = if crate::paths::is_executable(path) {
        Cmd::new(path)
    } else {
        Cmd::new("/bin/sh").arg(path)
    };
    cmd = cmd
        .cwd(dir)
        .timeout(context.timeout.unwrap_or(TIMEOUT))
        .keep_locale();

    let text = |value: Option<&Path>| value.map(|p| p.display().to_string());
    let vars: [(&str, Option<String>); 12] = [
        ("ICM_PLATFORM", Some(context.platform.clone())),
        (
            "ICM_RUN_DIR",
            ctx.rep.run_dir().map(|d| d.display().to_string()),
        ),
        ("ICM_PID", context.pid.map(|pid| pid.to_string())),
        ("ICM_DEVICE", context.device.clone()),
        ("ICM_ADB", context.adb.clone()),
        ("ICM_APP_ID", Some(app_id.to_string())),
        ("ICM_BIN", text(context.bin.as_deref())),
        ("ICM_APP_STDERR", text(context.app_stderr.as_deref())),
        ("ICM_LOGS", text(context.logs.as_deref())),
        ("ICM_LOG_MARK", context.log_mark.clone()),
        ("ICM_SIM_DATA", text(context.sim_data.as_deref())),
        ("ICM_PROJECT_DIR", Some(dir.display().to_string())),
    ];
    for (key, value) in vars {
        cmd = match value {
            Some(value) => cmd.env(key, value),
            None => cmd.env_remove(key),
        };
    }
    for (key, value) in &context.env {
        cmd = cmd.env(key, value);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::GlobalArgs;
    use crate::output::{Mode, Reporter, RunInfo};
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn context(global: GlobalArgs) -> (Ctx, Sink) {
        let sink = Sink::default();
        let rep = Reporter::with_writers(
            Mode {
                json: true,
                ..Mode::default()
            },
            RunInfo {
                run: "20261006T000000Z-run-desktop-0000".into(),
                command: "run".into(),
                target: Some("desktop".into()),
                argv: vec![],
                save: false,
            },
            Box::new(sink.clone()),
            Box::new(Sink::default()),
        );
        (Ctx::new(global, rep, vec![]), sink)
    }

    fn events(sink: &Sink) -> Vec<Value> {
        String::from_utf8(sink.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn script(dir: &Path, name: &str, body: &str, executable: bool) {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn check_lines_parse() {
        assert_eq!(
            parse_check_line("CHECK PASS wallet_smoke: balance shown"),
            Some((Status::Pass, "wallet_smoke".into(), "balance shown".into()))
        );
        assert_eq!(
            parse_check_line("CHECK FAIL Login-Screen"),
            Some((Status::Fail, "login_screen".into(), String::new()))
        );
        assert_eq!(
            parse_check_line("CHECK FAIL hook.x: y"),
            Some((Status::Fail, "x".into(), "y".into()))
        );
        assert_eq!(parse_check_line("CHECK MAYBE x: y"), None);
        assert_eq!(parse_check_line("CHECK PASS two words: y"), None);
        assert_eq!(parse_check_line("checking things"), None);
    }

    #[test]
    fn ids_are_well_formed() {
        assert_eq!(script_id("platform/ios/checks.sh"), "hook.checks");
        assert_eq!(script_id("scripts/Smoke Test.py"), "hook.smoke_test");
        assert_eq!(sanitize("9lives"), "h_9lives");
        assert_eq!(sanitize("---"), "hook");
        assert_eq!(sanitize("a.b-c"), "a_b_c");
    }

    #[test]
    fn hooks_report_checks_and_failures() {
        let dir = tempfile::tempdir().unwrap();
        script(
            dir.path(),
            "platform/ok.sh",
            "#!/bin/sh\necho \"CHECK PASS env: $ICM_PLATFORM $ICM_APP_ID $ICM_PID $ICM_DEVICE\"\necho \"CHECK FAIL wallet: no balance\"\n[ -z \"$ICM_SIM_DATA\" ] && echo 'CHECK PASS unset: sim data unset'\n[ \"$(pwd -P)\" = \"$(cd \"$ICM_PROJECT_DIR\" && pwd -P)\" ] && echo 'CHECK PASS cwd: project dir'\nexit 0\n",
            true,
        );
        script(
            dir.path(),
            "platform/plain.sh",
            "echo 'not a check line'\n[ -t 0 ] && echo 'CHECK FAIL stdin: open'\nexit 0\n",
            false,
        );
        script(
            dir.path(),
            "platform/bad.sh",
            "#!/bin/sh\necho 'going down' >&2\nexit 3\n",
            true,
        );
        let (ctx, sink) = context(GlobalArgs::default());
        let hook = HookContext {
            platform: "ios-sim".into(),
            pid: Some(4242),
            device: Some("UDID-1".into()),
            ..HookContext::default()
        };
        let scripts = [
            "platform/ok.sh".to_string(),
            "platform/plain.sh".to_string(),
            "platform/bad.sh".to_string(),
            "platform/missing.sh".to_string(),
        ];
        let reports = run_scripts(&ctx, dir.path(), "com.acme.x", &scripts, &hook).unwrap();
        assert_eq!(reports.len(), 4);
        assert!(!reports[0].ok, "a FAIL line fails the script");
        assert_eq!(reports[0].exit, Some(0));
        assert_eq!(reports[0].checks.len(), 4, "{:?}", reports[0].checks);
        assert!(reports[1].ok);
        assert_eq!(reports[2].exit, Some(3));
        assert!(!reports[2].ok);
        assert_eq!(reports[3].exit, None);

        let checks: Vec<(String, String, String)> = events(&sink)
            .into_iter()
            .filter(|event| event["type"] == "check")
            .map(|event| {
                (
                    event["id"].as_str().unwrap().to_string(),
                    event["status"].as_str().unwrap().to_string(),
                    event["detail"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let find = |id: &str| {
            checks
                .iter()
                .find(|(i, _, _)| i == id)
                .unwrap_or_else(|| panic!("{id} missing from {checks:?}"))
                .clone()
        };
        assert_eq!(find("hook.env").2, "ios-sim com.acme.x 4242 UDID-1");
        assert_eq!(find("hook.wallet").1, "fail");
        assert_eq!(find("hook.unset").1, "pass");
        assert_eq!(find("hook.cwd").1, "pass");
        assert_eq!(find("hook.plain").1, "pass");
        assert!(!checks.iter().any(|(id, _, _)| id == "hook.stdin"));
        let bad = find("hook.bad");
        assert_eq!(bad.1, "fail");
        assert!(
            bad.2.contains("exit 3") && bad.2.contains("going down"),
            "{bad:?}"
        );
        let missing = find("hook.missing");
        assert_eq!(missing.1, "fail");
        assert!(missing.2.contains("does not exist"));
    }

    #[test]
    fn a_hanging_hook_is_killed_and_fails() {
        let dir = tempfile::tempdir().unwrap();
        script(dir.path(), "hang.sh", "#!/bin/sh\nsleep 30\n", true);
        let hook = HookContext {
            platform: "desktop".into(),
            timeout: Some(Duration::from_millis(300)),
            ..HookContext::default()
        };
        let started = std::time::Instant::now();
        let (ctx, sink) = context(GlobalArgs::default());
        let reports = run_scripts(
            &ctx,
            dir.path(),
            "com.acme.x",
            &["hang.sh".to_string()],
            &hook,
        )
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!reports[0].ok);
        let failed: Vec<Value> = events(&sink)
            .into_iter()
            .filter(|event| event["type"] == "check" && event["status"] == "fail")
            .collect();
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert_eq!(failed[0]["id"], "hook.hang");
        assert!(
            failed[0]["detail"]
                .as_str()
                .unwrap()
                .contains("did not finish")
        );

        // When the overall --timeout runs out, that blocks the command.
        let global = GlobalArgs {
            timeout: Some(Duration::from_millis(300)),
            ..GlobalArgs::default()
        };
        let (short, _sink) = context(global);
        let hook = HookContext {
            platform: "desktop".into(),
            ..HookContext::default()
        };
        let error = run_scripts(
            &short,
            dir.path(),
            "com.acme.x",
            &["hang.sh".to_string()],
            &hook,
        )
        .unwrap_err();
        assert_eq!(error.id, "step.timeout");
    }
}
