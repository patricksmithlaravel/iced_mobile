//! The desktop dev platform (design §10.1, §13.1, §13.3): `icm build|run|
//! shot|logs|stop desktop`.
//!
//! `run` builds the binary with icm's profiles (`--config`, Appendix C item
//! 4) and `MACOSX_DEPLOYMENT_TARGET` from `[desktop.macos] min_os`, links it
//! into `target/icm/build/desktop/<profile>/`, stops this project's previous
//! desktop app (Appendix C item 13), and starts the new one in its own
//! session and process group with `ICM_EVENTS=1` (Appendix C item 27), its
//! stdout and stderr going to `app.stdout` and `app.stderr` in
//! `target/icm/sessions/desktop/<run>/` (outside the run directories, so
//! pruning never removes files the app still writes; the run directory
//! gets a copy). It is ready on `ICM_EVENT ready`; an app that sends no events
//! is ready when it is alive after 3 s and owns a window (`source:
//! "probe"`). A panic, an exit or no first frame within `--wait-ready`
//! fails the run (exit 10) and stops the app.
//!
//! The screenshot on macOS is `screencapture -l <window id>` after the
//! Screen Recording preflight ([`macos`]); without that permission, or
//! without a capturable window (Wayland, a missing X11 tool), it is a
//! headless render of the view at the window's size ([`headless`]), with
//! WARN `desktop.shot.permission` for the permission. Either way the run
//! writes `screen.png`, `screen.preview.png`, checks for a blank screen,
//! and assembles `app.log` and `logs.ndjson` ([`logs`]).
//!
//! The session file `target/icm/sessions/desktop.json` (also copied into
//! the run directory as `session.json`) records the pid, the log files, the
//! launch time and the window; `shot`, `logs` and `stop` read it.

pub mod headless;
pub mod linux;
pub mod logs;
#[cfg(target_os = "macos")]
pub mod macos;

use crate::cargo::{self, Invocation, Select};
use crate::catalogue::CheckId;
use crate::cli::{BuildArgs, InputArgs, LogSource, LogsArgs, RunArgs, ShotArgs, StopArgs};
use crate::context::{Ctx, Project};
use crate::error::{Check, Evidence, IcmError, Result, Status};
use crate::exit::Exit;
use crate::image::{self, Blank};
use crate::output::rundir;
use crate::paths;
use crate::plan::{Plan, Step};
use crate::process::{self, Cmd};
use crate::screen::Screen;
use crate::signals;
use crate::time::format_duration;
use logs::{Filter, Record};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// The platform's name.
pub const PLATFORM: &str = "desktop";

/// How long an app that sends no `ICM_EVENT` lines must be alive (and own a
/// window) to count as ready.
const PROBE_AFTER: Duration = Duration::from_secs(3);

/// How long `stop` waits after SIGTERM before SIGKILL.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// How long icm waits for a panic's message, or the exit, after the panic
/// event before it reports the panic.
const PANIC_GRACE: Duration = Duration::from_millis(500);

const POLL: Duration = Duration::from_millis(50);

/// The window size used when nothing better is known (iced's default).
const DEFAULT_SIZE: (f64, f64) = (1024.0, 768.0);

/// The height of a standard macOS title bar, in points.
#[cfg(target_os = "macos")]
const TITLE_BAR: f64 = 32.0;

// ---- sessions ----------------------------------------------------------------------

/// The window the app reported, or the probe found.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowHint {
    /// The content size in logical pixels (points).
    pub size: (f64, f64),
    /// The content size in physical pixels, when the app reported it.
    pub physical: Option<(u32, u32)>,
    /// Physical pixels per logical pixel.
    pub scale: f64,
}

impl WindowHint {
    /// From a `ready` event's `window` object.
    pub fn from_ready(event: &Value) -> Option<WindowHint> {
        let window = event.get("window")?;
        let pair = |key: &str| -> Option<(f64, f64)> {
            let values = window.get(key)?.as_array()?;
            Some((values.first()?.as_f64()?, values.get(1)?.as_f64()?))
        };
        let size = pair("size")?;
        let physical = pair("physical").map(|(w, h)| (w.round() as u32, h.round() as u32));
        let scale = window.get("scale").and_then(Value::as_f64).unwrap_or(1.0);
        Some(WindowHint {
            size,
            physical,
            scale: if scale > 0.0 { scale } else { 1.0 },
        })
    }

    fn to_json(self) -> Value {
        json!({
            "size": [self.size.0, self.size.1],
            "physical": self.physical.map(|(w, h)| [w, h]),
            "scale": self.scale,
        })
    }
}

/// `target/icm/sessions/desktop.json`: the running app.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// Always `desktop`.
    pub platform: String,
    /// The app's pid.
    pub pid: i32,
    /// Its process group (the app leads its own session).
    pub pgid: i32,
    /// The run that launched it.
    pub run: String,
    /// That run's directory.
    pub run_dir: PathBuf,
    /// The executable.
    pub exe: PathBuf,
    /// Its working directory.
    pub cwd: PathBuf,
    /// Its stdout file.
    pub stdout: PathBuf,
    /// Its stderr file.
    pub stderr: PathBuf,
    /// The launch mark, `2026-10-06T12:34:56.789Z`.
    pub launched: String,
    /// `debug` or `release`.
    pub profile: String,
    /// The window, once ready.
    #[serde(default)]
    pub window: Option<WindowHint>,
    /// The `ready` event, if the app sent one.
    #[serde(default)]
    pub ready: Option<Value>,
}

/// The session file.
pub fn session_path(project: &Project) -> PathBuf {
    project.sessions_dir().join(format!("{PLATFORM}.json"))
}

/// This project's desktop session, if one is recorded.
pub fn read_session(project: &Project) -> Option<Session> {
    read_session_file(&session_path(project))
}

/// Where a run's app writes its stdout and stderr while it runs:
/// `target/icm/sessions/desktop/<run>/`.
fn files_dir(project: &Project, run: &str) -> PathBuf {
    project.sessions_dir().join(PLATFORM).join(run)
}

/// Removes the live-file directories of runs other than `keep` (their apps
/// have been stopped; the run directories keep copies).
fn prune_files(project: &Project, keep: &str) {
    let Ok(read) = std::fs::read_dir(project.sessions_dir().join(PLATFORM)) else {
        return;
    };
    for entry in read.flatten() {
        if entry.file_name().to_string_lossy() != keep
            && entry.file_type().is_ok_and(|t| t.is_dir())
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Copies the app's live stdout and stderr into its run directory.
fn snapshot(session: &Session) {
    for (from, name) in [
        (&session.stdout, "app.stdout"),
        (&session.stderr, "app.stderr"),
    ] {
        let to = session.run_dir.join(name);
        if *from != to {
            let _ = std::fs::copy(from, to);
        }
    }
}

/// A finished session whose live files are gone (a later run removed them)
/// reads the copies in its run directory.
fn with_copies(mut session: Session) -> Session {
    for (path, name) in [
        (&mut session.stdout, "app.stdout"),
        (&mut session.stderr, "app.stderr"),
    ] {
        let copy = session.run_dir.join(name);
        if !path.exists() && copy.exists() {
            *path = copy;
        }
    }
    session
}

fn read_session_file(path: &Path) -> Option<Session> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// The newest run that launched a desktop app (its `session.json`), for
/// logs after the app is gone. A run that failed before its launch has none.
fn last_session(project: &Project) -> Option<Session> {
    // The last launch's live files stay until the next launch, even when
    // its run directory has been pruned.
    let live = std::fs::read_dir(project.sessions_dir().join(PLATFORM))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| read_session_file(&entry.path().join("session.json")))
        .max_by(|a, b| a.run.cmp(&b.run));
    if live.is_some() {
        return live;
    }
    if let Some(session) = read_session_file(&project.latest_dir(PLATFORM).join("session.json")) {
        return Some(session);
    }
    let mut runs: Vec<PathBuf> = std::fs::read_dir(project.runs_dir())
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|dir| {
            dir.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| rundir::is_run_id(name) && name.contains("-run-desktop-"))
        })
        .collect();
    runs.sort();
    runs.iter()
        .rev()
        .find_map(|dir| read_session_file(&dir.join("session.json")))
}

fn write_session(project: &Project, session: &Session) {
    let mut text = serde_json::to_string_pretty(session).unwrap_or_default();
    text.push('\n');
    let _ = rundir::write_atomic(&session_path(project), text.as_bytes());
    let _ = rundir::write_atomic(&session.run_dir.join("session.json"), text.as_bytes());
    if let Some(files) = session.stderr.parent()
        && files != session.run_dir
    {
        let _ = rundir::write_atomic(&files.join("session.json"), text.as_bytes());
    }
}

/// Removes the session file if it still records `pid` (a newer run may have
/// replaced it).
fn remove_session(project: &Project, pid: i32) {
    let path = session_path(project);
    if read_session_file(&path).is_some_and(|session| session.pid == pid) {
        let _ = std::fs::remove_file(path);
    }
}

// ---- processes -------------------------------------------------------------------

/// How a launched app ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ended {
    Exited(i32),
    Signaled(i32),
    /// It is gone; it was not icm's child, so how is unknown.
    Gone,
}

impl Ended {
    fn describe(self) -> String {
        match self {
            Ended::Exited(code) => format!("exited with code {code}"),
            Ended::Signaled(signal) => format!("was killed by {}", signals::name(signal)),
            Ended::Gone => "is gone".to_string(),
        }
    }

    fn to_json(self) -> Value {
        match self {
            Ended::Exited(code) => json!({"code": code}),
            Ended::Signaled(signal) => json!({"signal": signals::name(signal)}),
            Ended::Gone => json!({}),
        }
    }
}

/// Whether `pid` has ended: reaps it when it is this process's child,
/// otherwise checks that it still exists.
fn reap(pid: i32) -> Option<Ended> {
    if pid <= 1 {
        return Some(Ended::Gone);
    }
    let mut status: libc::c_int = 0;
    // SAFETY: waitpid with WNOHANG only reads the child's status into a
    // local integer.
    let reaped = unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) };
    if reaped == pid {
        if libc::WIFEXITED(status) {
            Some(Ended::Exited(libc::WEXITSTATUS(status)))
        } else if libc::WIFSIGNALED(status) {
            Some(Ended::Signaled(libc::WTERMSIG(status)))
        } else {
            None
        }
    } else if reaped == 0 {
        None
    } else {
        (!signals::alive(pid)).then_some(Ended::Gone)
    }
}

/// Whether the session's app still runs: the pid exists and still runs the
/// session's executable (a pid can be reused).
fn running(session: &Session) -> bool {
    if reap(session.pid).is_some() {
        return false;
    }
    let ps = Cmd::tool("ps")
        .args(["-ww", "-o", "command=", "-p"])
        .arg(session.pid.to_string())
        .timeout(Duration::from_secs(10));
    match process::run(&ps, None, None) {
        Ok(outcome) if outcome.success() => outcome
            .stdout_text()
            .trim()
            .starts_with(&session.exe.display().to_string()),
        Ok(_) => false,
        // ps itself failed: trust the pid.
        Err(_) => signals::alive(session.pid),
    }
}

/// Waits until `pid` is gone, at most `limit`.
fn wait_gone(pid: i32, limit: Duration) -> bool {
    let until = Instant::now() + limit;
    loop {
        if reap(pid).is_some() {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// SIGTERM to the app's process group, then SIGKILL after [`STOP_GRACE`].
fn terminate(pid: i32, pgid: i32) -> &'static str {
    if reap(pid).is_some() {
        return "already exited";
    }
    signals::kill_group(pgid, libc::SIGTERM);
    if wait_gone(pid, STOP_GRACE) {
        return "stopped with SIGTERM";
    }
    signals::kill_group(pgid, libc::SIGKILL);
    let _ = wait_gone(pid, Duration::from_secs(2));
    "killed with SIGKILL"
}

/// Stops this project's desktop app, if one is recorded, and removes the
/// session. Returns what was stopped (`{platform, pid, run, how}`), for
/// `stop` and for `stop --all`.
pub fn stop_session(ctx: &Ctx, project: &Project) -> Result<Option<Value>> {
    let Some(session) = read_session(project) else {
        return Ok(None);
    };
    let how = if running(&session) {
        terminate(session.pid, session.pgid)
    } else {
        "already exited"
    };
    remove_session(project, session.pid);
    ctx.rep.progress(format!(
        "desktop app pid {} (run {}): {how}",
        session.pid, session.run
    ));
    Ok(Some(json!({
        "platform": PLATFORM,
        "pid": session.pid,
        "run": session.run,
        "how": how,
    })))
}

// ---- helpers -----------------------------------------------------------------------

fn profile(release: bool) -> &'static str {
    if release { "release" } else { "dev" }
}

fn internal(what: &str, error: impl std::fmt::Display) -> IcmError {
    IcmError::new(CheckId::InternalBug, format!("{what}: {error}"))
}

/// Output of a short command, trimmed, when it succeeds.
fn quick(program: &str, args: &[&str]) -> Option<String> {
    let cmd = Cmd::tool(program)
        .args(args)
        .timeout(Duration::from_secs(10));
    let outcome = process::run(&cmd, None, None).ok()?;
    let text = outcome.stdout_text().trim().to_string();
    (outcome.success() && !text.is_empty()).then_some(text)
}

/// The result's `device`.
/// This machine, as the desktop's `device` (`icm devices desktop` too).
pub fn device_json() -> Value {
    let os = if cfg!(target_os = "macos") {
        format!(
            "macOS {}",
            quick("sw_vers", &["-productVersion"]).unwrap_or_default()
        )
    } else if cfg!(target_os = "linux") {
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|line| line.strip_prefix("PRETTY_NAME="))
                    .map(|name| name.trim_matches('"').to_string())
            })
            .unwrap_or_else(|| "Linux".to_string())
    } else {
        std::env::consts::OS.to_string()
    };
    json!({
        "kind": "desktop",
        "name": "this machine",
        "os": os.trim(),
        "arch": std::env::consts::ARCH,
    })
}

/// Whether the system appearance is dark (the headless fallback's theme).
fn dark_mode() -> bool {
    cfg!(target_os = "macos")
        && quick("defaults", &["read", "-g", "AppleInterfaceStyle"])
            .is_some_and(|style| style.eq_ignore_ascii_case("dark"))
}

/// `--env K=V` pairs.
fn parse_env(pairs: &[String]) -> Result<Vec<(String, String)>> {
    pairs
        .iter()
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) if !key.is_empty() => Ok((key.to_string(), value.to_string())),
            _ => Err(IcmError::new(
                CheckId::UsageBadArgs,
                format!("--env {pair:?} is not KEY=VALUE"),
            )),
        })
        .collect()
}

/// This run's directory (attached when the project resolved).
fn run_dir(ctx: &Ctx, project: &Project) -> Result<PathBuf> {
    let dir = match ctx.rep.run_dir() {
        Some(dir) => dir,
        None => ctx
            .rep
            .attach(&project.icm_dir)
            .map_err(|error| internal("cannot create the run directory", error))?,
    };
    std::fs::create_dir_all(&dir)
        .map_err(|error| internal(&format!("cannot create {}", dir.display()), error))?;
    Ok(dir)
}

fn no_session(detail: impl Into<String>) -> IcmError {
    IcmError::new(CheckId::RunNoSession, detail).fix(
        "Start the app with `icm run desktop`.",
        &["icm run desktop --json -q"],
    )
}

// ---- build -------------------------------------------------------------------------

/// The cargo invocation and binary name for a profile.
fn invocation(project: &Project, profile: &str) -> Result<(Invocation, String)> {
    let package = project.package_for(PLATFORM)?;
    let bin = project.bin_for(PLATFORM)?;
    let mut invocation = Invocation::new("build", &package.manifest_path, &package.name);
    invocation.select = Select::Bin(bin.clone());
    invocation.profile = profile.to_string();
    invocation.config = cargo::profile_config(profile);
    Ok((invocation, bin))
}

fn min_os(project: &Project) -> &str {
    &project.config.config.desktop.macos.min_os
}

/// Builds the app and links the executable into the build directory.
fn compile(ctx: &Ctx, project: &Project, profile: &str) -> Result<PathBuf> {
    let (invocation, bin) = invocation(project, profile)?;
    let mut env = Vec::new();
    let stamp = match ctx.deployment_target(
        project,
        &invocation.package,
        None,
        profile,
        min_os(project),
    )? {
        Some((pair, stamp)) => {
            env.push(pair);
            Some(stamp)
        }
        None => None,
    };

    let output = ctx.cargo("cargo.build", &invocation, &env)?;
    if let Some(stamp) = stamp {
        stamp
            .write()
            .map_err(|error| internal("cannot write the deployment-target stamp", error))?;
    }

    let built = output
        .executable(&bin)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cargo::artifacts_dir(&project.target_dir, None, profile).join(&bin));
    if !built.is_file() {
        return Err(IcmError::new(
            CheckId::BuildCargoFailed,
            format!(
                "cargo built no executable for binary `{bin}` (looked for {})",
                paths::display(&built)
            ),
        ));
    }

    let exe = project.build_dir(PLATFORM, profile).join(&bin);
    install(&built, &exe)?;
    Ok(exe)
}

/// Links (or copies) the built executable to `to`.
fn install(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| internal(&format!("cannot create {}", parent.display()), error))?;
    }
    let _ = std::fs::remove_file(to);
    if std::fs::hard_link(from, to).is_err() {
        std::fs::copy(from, to).map_err(|error| {
            internal(
                &format!("cannot copy {} to {}", from.display(), to.display()),
                error,
            )
        })?;
    }
    Ok(())
}

/// The executable of the last build, for `--no-build`.
fn previous_build(project: &Project, profile: &str) -> Result<PathBuf> {
    let bin = project.bin_for(PLATFORM)?;
    let exe = project.build_dir(PLATFORM, profile).join(bin);
    if exe.is_file() {
        Ok(exe)
    } else {
        Err(IcmError::new(
            CheckId::UsageBadArgs,
            format!(
                "--no-build: nothing was built yet at {}",
                paths::display(&exe)
            ),
        )
        .fix(
            "Build first: run without --no-build, or `icm build desktop`.",
            &["icm run desktop --json -q"],
        ))
    }
}

/// `icm build desktop`.
pub fn build(ctx: &mut Ctx, args: &BuildArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    let profile = profile(args.release);
    ctx.rep.set("profile", json!(cargo::profile_dir(profile)));
    ctx.rep.set("device", device_json());

    if ctx.dry_run() {
        plan(ctx, &project, profile, None)?.report(ctx);
        return Ok(());
    }

    let _lock = ctx.lock_platform(PLATFORM)?;
    let exe = compile(ctx, &project, profile)?;
    ctx.rep.artifact("bundle", &exe);
    ctx.rep.summary(format!("built {}", paths::display(&exe)));
    ctx.rep.next(
        if args.release {
            "icm run desktop --release --no-build --json -q"
        } else {
            "icm run desktop --no-build --json -q"
        },
        "launch this build",
    );
    Ok(())
}

// ---- the plan (--dry-run) ----------------------------------------------------------

fn app_cmd(project: &Project, exe: &Path, run: &str, extra: &[(String, String)]) -> Cmd {
    let mut cmd = Cmd::new(exe)
        .cwd(project.dir())
        .keep_locale()
        .env("ICM_EVENTS", "1")
        .env("ICM_RUN_ID", run)
        .env("RUST_BACKTRACE", "1");
    // icm's own run plumbing is not the app's business.
    for var in ["ICM_DETACHED", "ICM_RUN_DIR", "ICM_RUN_ROOT", "ICM_JSON"] {
        cmd = cmd.env_remove(var);
    }
    for (key, value) in extra {
        cmd = cmd.env(key, value);
    }
    cmd
}

fn plan(ctx: &Ctx, project: &Project, profile: &str, run: Option<&RunArgs>) -> Result<Plan> {
    let mut plan = Plan::new();
    let (invocation, bin) = invocation(project, profile)?;
    let exe = project.build_dir(PLATFORM, profile).join(&bin);

    if run.is_none_or(|args| !args.no_build) {
        let mut cmd = invocation.cmd();
        if let Some(var) = cargo::deployment_var(crate::toolchain::host_triple()) {
            cmd = cmd.env(var, min_os(project));
        }
        plan.push(Step::exec("cargo.build", cmd).on_fail(CheckId::BuildCompileError));
        plan.push(Step::internal(
            "desktop.install",
            &format!("link the executable to {}", paths::display(&exe)),
        ));
    }

    let Some(args) = run else {
        return Ok(plan);
    };
    let extra = parse_env(&args.env)?;
    plan.push(Step::internal(
        "desktop.stop_previous",
        &format!(
            "stop this project's previous desktop app ({}), if any",
            paths::display(&session_path(project))
        ),
    ));
    plan.push(
        Step::exec(
            "desktop.launch",
            app_cmd(project, &exe, &ctx.rep.run_id(), &extra),
        )
        .on_fail(CheckId::RunAppDied),
    );
    plan.push(
        Step::internal(
            "desktop.ready",
            &format!(
                "wait up to {} for `ICM_EVENT ready` in app.stderr (without events: alive and owning a window after {})",
                format_duration(args.wait_ready),
                format_duration(PROBE_AFTER)
            ),
        )
        .gate(CheckId::RunReady)
        .on_fail(CheckId::RunNotReady),
    );
    if !args.no_shot {
        if cfg!(target_os = "macos") {
            plan.push(
                Step::exec(
                    "desktop.screencapture",
                    Cmd::tool("screencapture").args([
                        "-x",
                        "-o",
                        "-l",
                        "<window-id>",
                        "<run-dir>/screen.png",
                    ]),
                )
                .gate(CheckId::DesktopShotPermission),
            );
        }
        plan.push(
            Step::internal(
                "desktop.preview",
                "write screen.preview.png (long edge at most 1024 px) and look for a blank screen",
            )
            .gate(CheckId::RunScreenBlank),
        );
    }
    Ok(plan)
}

// ---- launch and readiness -------------------------------------------------------------

struct Launched {
    pid: i32,
    started: Instant,
    launched: String,
    /// The live files the app writes.
    stdout: PathBuf,
    stderr: PathBuf,
    /// The copy of stderr in the run directory, which evidence names (the
    /// live file goes when a later run starts).
    stderr_copy: PathBuf,
}

fn launch(
    ctx: &Ctx,
    project: &Project,
    exe: &Path,
    run_dir: &Path,
    extra: &[(String, String)],
) -> Result<Launched> {
    let run = ctx.rep.run_id();
    let files = files_dir(project, &run);
    std::fs::create_dir_all(&files)
        .map_err(|error| internal(&format!("cannot create {}", files.display()), error))?;
    prune_files(project, &run);
    let stdout = files.join("app.stdout");
    let stderr = files.join("app.stderr");
    let cmd = app_cmd(project, exe, &run, extra);

    ctx.rep.step_begin(
        "desktop.launch",
        &cmd.display_argv(),
        &cmd.display_env(),
        cmd.cwd.as_deref(),
    );
    let started = Instant::now();
    let launched = logs::timestamp(SystemTime::now());
    let pid = process::spawn_detached(&cmd, &stdout, &stderr).map_err(|error| {
        ctx.rep.step_end_internal("desktop.launch", false, 0);
        IcmError::new(
            CheckId::ToolFailed,
            format!("cannot start {}: {error}", paths::display(exe)),
        )
    })?;
    ctx.rep
        .step_end_internal("desktop.launch", true, started.elapsed().as_millis() as u64);

    Ok(Launched {
        pid: pid as i32,
        started,
        launched,
        stdout,
        stderr,
        stderr_copy: run_dir.join("app.stderr"),
    })
}

/// A panic the app reported.
#[derive(Clone, Debug, Default)]
struct Panic {
    message: String,
    location: Option<String>,
    thread: Option<String>,
    /// The stderr line that shows it, and that line's text.
    line: Option<u32>,
    excerpt: Option<String>,
    from_text: bool,
}

/// Follows the app's stderr for `ICM_EVENT` lines and panics, and its pid.
struct Watch {
    pid: i32,
    stderr: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    lines: u32,
    start: Option<Value>,
    ready: Option<Value>,
    panic: Option<Panic>,
    panic_seen: Option<Instant>,
    awaiting_message: bool,
    warnings: Vec<Value>,
    ended: Option<Ended>,
}

impl Watch {
    fn new(pid: i32, stderr: &Path) -> Watch {
        Watch {
            pid,
            stderr: stderr.to_path_buf(),
            offset: 0,
            partial: Vec::new(),
            lines: 0,
            start: None,
            ready: None,
            panic: None,
            panic_seen: None,
            awaiting_message: false,
            warnings: Vec::new(),
            ended: None,
        }
    }

    /// Checks the pid, then reads what stderr gained (so everything written
    /// before an exit is seen).
    fn poll(&mut self) {
        if self.ended.is_none() {
            self.ended = reap(self.pid);
        }
        self.read();
    }

    fn read(&mut self) {
        let Ok(mut file) = File::open(&self.stderr) else {
            return;
        };
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            return;
        }
        self.offset += bytes.len() as u64;
        self.partial.extend(bytes);
        while let Some(newline) = self.partial.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=newline).collect();
            let text = String::from_utf8_lossy(&line[..line.len() - 1])
                .trim_end_matches('\r')
                .to_string();
            self.lines += 1;
            self.line(&text);
        }
    }

    fn line(&mut self, text: &str) {
        if let Some(json) = text.strip_prefix("ICM_EVENT ") {
            let Ok(event) = serde_json::from_str::<Value>(json) else {
                return;
            };
            let field = |key: &str| event.get(key).and_then(Value::as_str).map(str::to_string);
            match event.get("kind").and_then(Value::as_str) {
                Some("start") if self.start.is_none() => self.start = Some(event),
                Some("ready") if self.ready.is_none() => self.ready = Some(event),
                Some("panic") if self.panic.is_none() => {
                    self.panic = Some(Panic {
                        message: field("message").unwrap_or_default(),
                        location: field("location"),
                        thread: field("thread"),
                        line: Some(self.lines),
                        excerpt: Some(text.to_string()),
                        from_text: false,
                    });
                    self.panic_seen = Some(Instant::now());
                }
                Some("warning") => self.warnings.push(event),
                _ => {}
            }
            return;
        }

        if self.awaiting_message {
            self.awaiting_message = false;
            if let Some(panic) = &mut self.panic
                && panic.message.is_empty()
            {
                panic.message = text.to_string();
            }
        }

        if let Some((thread, location)) = panicked_at(text) {
            match &mut self.panic {
                None => {
                    self.panic = Some(Panic {
                        message: String::new(),
                        location,
                        thread,
                        line: Some(self.lines),
                        excerpt: Some(text.to_string()),
                        from_text: true,
                    });
                    self.awaiting_message = true;
                    self.panic_seen = Some(Instant::now());
                }
                // The event came first; std's own line reads better as
                // evidence.
                Some(panic) if !panic.from_text => {
                    panic.line = Some(self.lines);
                    panic.excerpt = Some(text.to_string());
                    panic.from_text = true;
                }
                Some(_) => {}
            }
        }
    }

    /// The failure to report now, if any: a panic (once its message had a
    /// moment to arrive) or an exit.
    fn failure(
        &self,
        project: &Project,
        launched: &Launched,
        after_ready: bool,
    ) -> Option<IcmError> {
        if let Some(panic) = &self.panic {
            let settled = self.ended.is_some()
                || self
                    .panic_seen
                    .is_some_and(|seen| seen.elapsed() >= PANIC_GRACE);
            return settled.then(|| panic_error(panic, project, launched));
        }
        self.ended
            .map(|ended| died_error(ended, launched, after_ready))
    }
}

/// `thread 'main' panicked at src/lib.rs:41:9:` → (thread, location).
fn panicked_at(line: &str) -> Option<(Option<String>, Option<String>)> {
    let (head, rest) = line.split_once(" panicked at ")?;
    // `thread 'main'`, or `thread 'main' (14513581)` since Rust 1.92.
    let thread = head
        .trim()
        .strip_prefix("thread '")
        .and_then(|name| name.rsplit_once('\''))
        .map(|(name, _)| name.to_string());
    let location = rest.trim().trim_end_matches(':').trim();
    Some((thread, (!location.is_empty()).then(|| location.to_string())))
}

/// `src/lib.rs:41:9` → (`src/lib.rs`, 41).
fn split_location(location: &str) -> Option<(&str, u32)> {
    let mut parts = location.rsplitn(3, ':');
    let _column = parts.next()?;
    let line = parts.next()?.parse().ok()?;
    let file = parts.next()?;
    Some((file, line))
}

fn last_lines(path: &Path, count: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with("ICM_EVENT "))
        .collect();
    lines[lines.len().saturating_sub(count)..].join("\n")
}

fn panic_error(panic: &Panic, project: &Project, launched: &Launched) -> IcmError {
    let location = panic.location.as_deref().unwrap_or("an unknown location");
    let mut detail = format!("panicked at {location}");
    if !panic.message.is_empty() {
        detail.push_str(&format!(": {}", panic.message));
    }
    if let Some(thread) = &panic.thread {
        detail.push_str(&format!(" (thread '{thread}')"));
    }

    let mut error = IcmError::new(CheckId::RunAppPanicked, detail);
    error = error.evidence(match (panic.line, &panic.excerpt) {
        (Some(line), Some(excerpt)) => Evidence::line(&launched.stderr_copy, line, excerpt.clone()),
        _ => Evidence::file(&launched.stderr_copy),
    });

    match panic.location.as_deref().and_then(split_location) {
        Some((file, line)) => {
            // rustc names the file relative to the workspace root (or
            // absolute, for dependencies).
            let source = [project.dir(), &project.metadata.workspace_root]
                .iter()
                .map(|base| base.join(file))
                .find(|path| path.is_file());
            let file = match &source {
                Some(path) => {
                    error = error.evidence(Evidence::line(path, line, panic.message.clone()));
                    paths::display(path)
                }
                None => file.to_string(),
            };
            error.cause(format!("a bug at {file}:{line}")).fix(
                format!("Fix the panic at {file}:{line}, then rerun."),
                &["icm run desktop --json -q"],
            )
        }
        None => error.fix(
            "Read the panic in app.stderr, fix it, then rerun.",
            &[
                "icm logs desktop --level warn --json",
                "icm run desktop --json -q",
            ],
        ),
    }
}

fn died_error(ended: Ended, launched: &Launched, after_ready: bool) -> IcmError {
    let mut detail = format!(
        "the app {} {} after launch, {} its first frame",
        ended.describe(),
        format_duration(launched.started.elapsed()),
        if after_ready { "after" } else { "before" }
    );
    let tail = last_lines(&launched.stderr, 6);
    if !tail.is_empty() {
        detail.push_str(&format!(":\n{tail}"));
    }
    let excerpt = tail.lines().last().unwrap_or("").to_string();
    let evidence = if excerpt.is_empty() {
        Evidence::file(&launched.stderr_copy)
    } else {
        Evidence::file(&launched.stderr_copy).with_excerpt(excerpt)
    };
    IcmError::new(CheckId::RunAppDied, detail)
        .evidence(evidence)
        .fix(
            "Read the app's output, fix the app, then rerun.",
            &[
                "icm logs desktop --level warn --json",
                "icm run desktop --json -q",
            ],
        )
}

/// How the app became ready.
struct Ready {
    source: &'static str,
    ms: u64,
    hint: Option<WindowHint>,
    event: Option<Value>,
}

/// The probe: an app without `ICM_EVENT` lines is ready when it owns a
/// window (macOS) or is alive (elsewhere) after [`PROBE_AFTER`].
#[cfg(target_os = "macos")]
fn probe(pid: i32) -> Option<Option<WindowHint>> {
    let window = macos::main_window(pid)?;
    let scale = macos::main_display_scale().unwrap_or(1.0);
    Some(Some(WindowHint {
        size: (window.bounds.2, (window.bounds.3 - TITLE_BAR).max(1.0)),
        physical: None,
        scale,
    }))
}

#[cfg(not(target_os = "macos"))]
fn probe(pid: i32) -> Option<Option<WindowHint>> {
    signals::alive(pid).then_some(None)
}

fn wait_ready(
    ctx: &Ctx,
    project: &Project,
    watch: &mut Watch,
    launched: &Launched,
    limit: Duration,
) -> Result<Ready> {
    let own = launched.started + limit;
    let (deadline, overall) = match ctx.deadline() {
        Some(overall) if overall < own => (overall, true),
        _ => (own, false),
    };

    loop {
        watch.poll();
        if let Some(signal) = signals::pending() {
            return Err(crate::output::interrupted(signal));
        }
        if let Some(error) = watch.failure(project, launched, false) {
            return Err(error);
        }
        if watch.panic.is_none()
            && let Some(event) = &watch.ready
        {
            return Ok(Ready {
                source: "icm_event",
                ms: launched.started.elapsed().as_millis() as u64,
                hint: WindowHint::from_ready(event),
                event: Some(event.clone()),
            });
        }
        if watch.panic.is_none()
            && watch.start.is_none()
            && launched.started.elapsed() >= PROBE_AFTER
            && let Some(hint) = probe(watch.pid)
        {
            return Ok(Ready {
                source: "probe",
                ms: launched.started.elapsed().as_millis() as u64,
                hint,
                event: None,
            });
        }

        if Instant::now() >= deadline {
            let waited = format_duration(launched.started.elapsed());
            if overall {
                return Err(IcmError::new(
                    CheckId::StepTimeout,
                    format!(
                        "the overall --timeout ran out after {waited} while waiting for the app's first frame"
                    ),
                ));
            }
            let mut detail = format!("the app is alive but drew no first frame within {waited}");
            if watch.start.is_none() {
                detail.push_str(" and sent no ICM_EVENT lines (and owns no window)");
            }
            return Err(IcmError::new(CheckId::RunNotReady, detail)
                .evidence(Evidence::file(&launched.stderr_copy))
                .fix(
                    "Read the app's output; raise --wait-ready if it is legitimately slow.",
                    &[
                        "icm logs desktop --level warn --json",
                        "icm run desktop --wait-ready 90s --json -q",
                    ],
                ));
        }
        std::thread::sleep(POLL);
    }
}

/// Waits `settle` while watching for a panic or an exit.
fn settle(
    project: &Project,
    watch: &mut Watch,
    launched: &Launched,
    settle: Duration,
) -> Result<()> {
    let until = Instant::now() + settle;
    loop {
        watch.poll();
        if let Some(signal) = signals::pending() {
            return Err(crate::output::interrupted(signal));
        }
        if let Some(error) = watch.failure(project, launched, true) {
            return Err(error);
        }
        if Instant::now() >= until && watch.panic.is_none() {
            return Ok(());
        }
        std::thread::sleep(POLL);
    }
}

fn describe_ready(ready: &Ready) -> String {
    let size = ready
        .hint
        .map(|hint| {
            format!(
                "{}x{}@{}",
                hint.size.0.round(),
                hint.size.1.round(),
                trim_float(hint.scale)
            )
        })
        .unwrap_or_else(|| "a window".to_string());
    let after = format_duration(Duration::from_millis(ready.ms));
    match &ready.event {
        Some(event) => {
            let field = |key| event.get(key).and_then(Value::as_str).unwrap_or("?");
            format!(
                "first frame {size} after {after} (source: icm_event; {} {} on {})",
                field("backend"),
                field("api"),
                field("adapter")
            )
        }
        None => {
            format!("{size} is up after {after} (source: probe; the app sent no ICM_EVENT lines)")
        }
    }
}

fn trim_float(value: f64) -> String {
    let text = format!("{value:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn process_json(pid: i32, alive: bool, ready: Option<&Ready>, exit: Option<Value>) -> Value {
    let mut value = json!({
        "pid": pid,
        "alive": alive,
        "ready": {
            "source": ready.map_or("none", |ready| ready.source),
            "ms": ready.map(|ready| ready.ms),
        },
        "exit": exit,
    });
    if let Some(event) = ready.and_then(|ready| ready.event.as_ref()) {
        for key in ["backend", "adapter", "api"] {
            if let Some(text) = event.get(key) {
                value[key] = text.clone();
            }
        }
    }
    value
}

// ---- screenshots -------------------------------------------------------------------

/// Why no window could be captured.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Fallback {
    /// macOS Screen Recording is not allowed.
    Permission,
    /// The app owns no window icm can find.
    NoWindow,
    /// The capture tool failed or is missing.
    Failed(String),
    /// No display server to capture from.
    NoDisplay(&'static str),
}

impl Fallback {
    fn describe(&self) -> String {
        match self {
            Fallback::Permission => "Screen Recording is not allowed".to_string(),
            Fallback::NoWindow => "the app owns no window icm can find".to_string(),
            Fallback::Failed(why) => format!("the window capture failed: {why}"),
            Fallback::NoDisplay(why) => (*why).to_string(),
        }
    }
}

/// Runs a capture step; a missing or failing tool is a [`Fallback`], an
/// interrupt or timeout is an error.
fn capture_step(ctx: &Ctx, name: &str, cmd: &Cmd) -> Result<std::result::Result<String, Fallback>> {
    match ctx.step(name, cmd) {
        Ok(outcome) if outcome.success() => Ok(Ok(outcome.stdout_text())),
        Ok(outcome) => Ok(Err(Fallback::Failed(format!(
            "{} {}: {}",
            cmd.program_name(),
            outcome.describe(),
            outcome.stderr_tail(2)
        )))),
        Err(error) if matches!(error.exit, Exit::Interrupted | Exit::Timeout) => Err(error),
        Err(error) => Ok(Err(Fallback::Failed(error.detail))),
    }
}

/// macOS: `screencapture -l <window id>` after the permission preflight.
/// Returns the scale of the captured pixels.
#[cfg(target_os = "macos")]
fn capture_window(
    ctx: &Ctx,
    pid: i32,
    hint: Option<WindowHint>,
    out: &Path,
) -> Result<std::result::Result<f64, Fallback>> {
    if !macos::screen_capture_allowed() {
        return Ok(Err(Fallback::Permission));
    }
    let Some(window) = macos::main_window(pid) else {
        return Ok(Err(Fallback::NoWindow));
    };

    let cmd = Cmd::tool("screencapture")
        .args(["-x", "-o", "-l"])
        .arg(window.id.to_string())
        .arg(out)
        .timeout(Duration::from_secs(30));
    match capture_step(ctx, "desktop.screencapture", &cmd)? {
        // What screencapture says when the permission is missing after all.
        Err(Fallback::Failed(why)) if why.contains("could not create image") => {
            return Ok(Err(Fallback::Permission));
        }
        Err(fallback) => return Ok(Err(fallback)),
        Ok(_) => {}
    }
    if !out.is_file() {
        return Ok(Err(Fallback::Failed(
            "screencapture wrote no file".to_string(),
        )));
    }

    let image = image::Image::read_png(out)
        .map_err(|error| internal("cannot read the screenshot", error))?;
    let scale = hint.map(|hint| hint.scale).unwrap_or_else(|| {
        if window.bounds.2 > 0.0 {
            f64::from(image.width) / window.bounds.2
        } else {
            macos::main_display_scale().unwrap_or(1.0)
        }
    });
    if let Some(content) = content_of(&image, hint, scale) {
        content
            .write_png(out)
            .map_err(|error| internal("cannot write the screenshot", error))?;
    }
    Ok(Ok(scale))
}

/// A window capture includes the title bar; this is the part the app
/// draws (its reported physical size, at the bottom), as on every other
/// platform. `None` when the capture is already that, or does not match.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn content_of(image: &image::Image, hint: Option<WindowHint>, scale: f64) -> Option<image::Image> {
    let (width, height) = hint?.physical?;
    (image.width == width
        && image.height > height
        && f64::from(image.height - height) <= 64.0 * scale)
        .then(|| image.crop(0, image.height - height, width, height))
}

/// Linux: X11 through `xdotool` and ImageMagick's `import`; Wayland and
/// headless hosts fall back. Compiled everywhere so it stays checked.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn capture_x11(
    ctx: &Ctx,
    pid: i32,
    hint: Option<WindowHint>,
    out: &Path,
) -> Result<std::result::Result<f64, Fallback>> {
    match linux::display(&ctx.env) {
        linux::Display::X11(_) => {}
        linux::Display::Wayland => {
            return Ok(Err(Fallback::NoDisplay(
                "Wayland offers no window capture without a portal prompt",
            )));
        }
        linux::Display::None => {
            return Ok(Err(Fallback::NoDisplay(
                "no display server (DISPLAY is unset)",
            )));
        }
    }

    let stdout = match capture_step(ctx, "desktop.xdotool", &linux::search_cmd(pid))? {
        Ok(stdout) => stdout,
        Err(fallback) => return Ok(Err(fallback)),
    };
    let Some(window) = linux::parse_search(&stdout) else {
        return Ok(Err(Fallback::NoWindow));
    };
    if let Err(fallback) = capture_step(ctx, "desktop.import", &linux::capture_cmd(&window, out))? {
        return Ok(Err(fallback));
    }
    if !out.is_file() {
        return Ok(Err(Fallback::Failed("import wrote no file".to_string())));
    }
    Ok(Ok(hint.map_or(1.0, |hint| hint.scale)))
}

#[cfg(not(target_os = "macos"))]
fn capture_window(
    ctx: &Ctx,
    pid: i32,
    hint: Option<WindowHint>,
    out: &Path,
) -> Result<std::result::Result<f64, Fallback>> {
    capture_x11(ctx, pid, hint, out)
}

/// The main display's scale (1 where icm cannot ask).
#[cfg(target_os = "macos")]
fn default_scale() -> f64 {
    macos::main_display_scale().unwrap_or(1.0)
}

/// The main display's scale (1 where icm cannot ask).
#[cfg(not(target_os = "macos"))]
fn default_scale() -> f64 {
    1.0
}

/// `screen.png` → `screen.preview.png`.
fn preview_path(screen: &Path) -> PathBuf {
    let stem = screen
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "screen".to_string());
    screen.with_file_name(format!("{stem}.preview.png"))
}

/// A screenshot that was written.
struct Shot {
    source: &'static str,
    blank: Blank,
}

/// Captures the app's window (or renders the view headlessly), writes the
/// preview, checks for a blank screen and reports the artifacts and
/// `screen`. `None` when no screenshot could be made (reported as WARN).
fn screenshot(
    ctx: &Ctx,
    project: &Project,
    pid: i32,
    hint: Option<WindowHint>,
    out: &Path,
    expect_content: bool,
) -> Result<Option<Shot>> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| internal(&format!("cannot create {}", parent.display()), error))?;
    }
    let _ = std::fs::remove_file(out);

    let started = Instant::now();
    let (source, scale, note) = match capture_window(ctx, pid, hint, out)? {
        Ok(scale) => ("window", scale, None),
        Err(fallback) => {
            let (size, scale) = hint.map_or((DEFAULT_SIZE, default_scale()), |hint| {
                (hint.size, hint.scale)
            });
            if fallback == Fallback::Permission {
                ctx.rep.check(
                    Check::warn(
                        CheckId::DesktopShotPermission,
                        format!(
                            "macOS does not allow the process that started icm (its terminal or agent app) to record the screen, so {} is a headless render of the view at {}x{}@{} instead of the window",
                            paths::display(out),
                            size.0.round(),
                            size.1.round(),
                            trim_float(scale)
                        ),
                    )
                    .fix(
                        "Optional, for the owner: allow Screen Recording for the app that runs icm in System Settings > Privacy & Security > Screen & System Audio Recording, then restart that app. The headless render shows the view's initial state.",
                        &["icm shot desktop --json -q"],
                    ),
                );
            } else {
                ctx.rep.progress(format!(
                    "no window capture ({}); rendering the view headlessly",
                    fallback.describe()
                ));
            }

            if let Err(error) = headless::render(ctx, project, size, scale, dark_mode(), out) {
                if matches!(error.exit, Exit::Interrupted | Exit::Timeout) {
                    return Err(error);
                }
                let mut error = error;
                error.detail = format!("no screenshot: {}", error.detail);
                ctx.rep.check(Check::from_error(error, Status::Warn));
                return Ok(None);
            }
            ("headless", scale, Some(fallback.describe()))
        }
    };

    let preview = preview_path(out);
    let image::Preview { px, blank, .. } = image::write_preview(out, &preview)
        .map_err(|error| internal("cannot write the preview", error))?;
    ctx.rep.step_end_internal(
        "desktop.screenshot",
        true,
        started.elapsed().as_millis() as u64,
    );

    if blank.is_blank() {
        let check = Check::new(
            CheckId::RunScreenBlank,
            if expect_content {
                Status::Fail
            } else {
                Status::Warn
            },
            blank.describe(),
        )
        .evidence(Evidence::file(out))
        .fix(
            "Compare with the headless render; check fonts and theme; read the logs.",
            &[
                "icm logs desktop --level warn --json",
                "icm shot --headless --json -q",
            ],
        );
        ctx.rep.check(check);
    }

    let bytes = |path: &Path| std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut extra = Map::new();
    let _ = extra.insert("bytes".into(), json!(bytes(out)));
    let _ = extra.insert("blank".into(), json!(blank.is_blank()));
    let _ = extra.insert("source".into(), json!(source));
    ctx.rep.artifact_with("screenshot", out, extra);
    let mut extra = Map::new();
    let _ = extra.insert("bytes".into(), json!(bytes(&preview)));
    ctx.rep.artifact_with("preview", &preview, extra);

    let mut screen = Screen::new(px, scale).to_json();
    screen["source"] = json!(source);
    if let Some(note) = note {
        screen["note"] = json!(note);
    }
    ctx.rep.set("screen", screen);

    Ok(Some(Shot { source, blank }))
}

// ---- logs --------------------------------------------------------------------------

/// Reads what a file gained since the last read, as records.
struct Tail {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    parser: logs::Parser,
}

impl Tail {
    fn new(path: &Path, stderr: bool, launched: &str) -> Tail {
        Tail {
            path: path.to_path_buf(),
            offset: 0,
            partial: Vec::new(),
            parser: logs::Parser::new(stderr, launched),
        }
    }

    fn read(&mut self) -> Vec<Record> {
        let Ok(mut file) = File::open(&self.path) else {
            return Vec::new();
        };
        let mut bytes = Vec::new();
        if file.seek(SeekFrom::Start(self.offset)).is_err() || file.read_to_end(&mut bytes).is_err()
        {
            return Vec::new();
        }
        self.offset += bytes.len() as u64;
        self.partial.extend(bytes);
        let Some(last) = self.partial.iter().rposition(|b| *b == b'\n') else {
            return Vec::new();
        };
        let complete: Vec<u8> = self.partial.drain(..=last).collect();
        self.parser.parse(&String::from_utf8_lossy(&complete))
    }
}

/// Every record so far, both files merged.
fn read_records(session: &Session) -> (Vec<Record>, [Tail; 2]) {
    let mut stderr = Tail::new(&session.stderr, true, &session.launched);
    let mut stdout = Tail::new(&session.stdout, false, &session.launched);
    let records = logs::merge(stderr.read(), stdout.read());
    (records, [stderr, stdout])
}

/// Copies the app's output into the run directory and writes `app.log`
/// and `logs.ndjson` there.
fn write_logs(ctx: &Ctx, session: &Session) {
    snapshot(session);
    let (records, _) = read_records(session);
    let app_log = session.run_dir.join("app.log");
    let ndjson = session.run_dir.join("logs.ndjson");
    let mut text = String::new();
    let mut lines = String::new();
    for record in &records {
        text.push_str(&record.to_line());
        text.push('\n');
        lines.push_str(&record.to_json(Some(session.pid)).to_string());
        lines.push('\n');
    }
    if std::fs::write(&app_log, text).is_ok() {
        ctx.rep.artifact("app_log", &app_log);
    }
    if std::fs::write(&ndjson, lines).is_ok() {
        ctx.rep.artifact("logs", &ndjson);
    }
}

fn emit_record(ctx: &Ctx, record: &Record, pid: i32) {
    let mut event = record.to_json(Some(pid));
    event["type"] = json!("log");
    ctx.rep.emit(event);
}

/// How following ended.
enum FollowEnd {
    Exited(Ended),
    Signal,
    Deadline,
}

/// Streams new records as `log` events until the app ends, a signal
/// arrives or the overall deadline passes.
fn follow(ctx: &Ctx, tails: &mut [Tail], pid: i32, filter: &Filter) -> FollowEnd {
    loop {
        let ended = reap(pid);
        for tail in tails.iter_mut() {
            for record in tail.read() {
                if filter.keeps(&record) {
                    emit_record(ctx, &record, pid);
                }
            }
        }
        if signals::pending().is_some() {
            return FollowEnd::Signal;
        }
        if let Some(ended) = ended {
            return FollowEnd::Exited(ended);
        }
        if ctx
            .deadline()
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return FollowEnd::Deadline;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// `icm logs desktop`: re-reads the app's stdout and stderr from the launch.
pub fn logs(ctx: &mut Ctx, args: &LogsArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    if ctx.dry_run() {
        let mut plan = Plan::new();
        plan.push(Step::internal(
            "desktop.logs",
            &format!(
                "read app.stderr and app.stdout of the session ({}), or of the last run, the last {} records{}",
                paths::display(&session_path(&project)),
                args.tail,
                if args.follow { ", then follow" } else { "" }
            ),
        ));
        plan.report(ctx);
        return Ok(());
    }
    let filter = Filter::new(args.level, &args.since, args.grep.as_deref())
        .map_err(|error| IcmError::new(CheckId::UsageBadArgs, format!("--since: {error}")))?;

    // The running app's session, else the last run's copy.
    let (session, live) = match read_session(&project) {
        Some(session) => {
            let live = running(&session);
            (with_copies(session), live)
        }
        None => match last_session(&project) {
            Some(session) => (with_copies(session), false),
            None => {
                return Err(no_session(
                    "no desktop app has run in this project yet, so there are no logs",
                ));
            }
        },
    };

    ctx.rep.set(
        "process",
        json!({"pid": session.pid, "alive": live, "run": session.run}),
    );
    ctx.rep.artifact("stderr", &session.stderr);
    ctx.rep.artifact("stdout", &session.stdout);

    if args.raw {
        let mut raw = Map::new();
        for (name, path) in [("stderr", &session.stderr), ("stdout", &session.stdout)] {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            let kept = lines[lines.len().saturating_sub(args.tail)..].join("\n");
            if !kept.is_empty() {
                ctx.rep.content(&kept);
            }
            let _ = raw.insert(name.to_string(), json!(kept));
        }
        ctx.rep.set("raw", Value::Object(raw));
    }

    let (records, mut tails) = read_records(&session);
    let wanted = matches!(
        args.source.unwrap_or(LogSource::All),
        LogSource::App | LogSource::All
    );
    let kept: Vec<&Record> = if wanted {
        records
            .iter()
            .filter(|record| filter.keeps(record))
            .collect()
    } else {
        Vec::new()
    };
    if wanted {
        crate::grep::warn_unmatched(ctx, args.grep.as_deref(), kept.len(), records.len());
    }
    let shown = &kept[kept.len().saturating_sub(args.tail)..];
    if !args.raw {
        for record in shown {
            emit_record(ctx, record, session.pid);
        }
    }
    ctx.rep.set(
        "records",
        json!(
            shown
                .iter()
                .map(|record| record.to_json(Some(session.pid)))
                .collect::<Vec<_>>()
        ),
    );
    ctx.rep.set(
        "counts",
        json!({"total": records.len(), "matched": kept.len(), "shown": shown.len()}),
    );

    let state = if live {
        format!("the app (pid {}) is running", session.pid)
    } else {
        format!("the app of run {} is no longer running", session.run)
    };
    let mut summary = format!(
        "{} of {} records from the desktop app; {state}",
        shown.len(),
        records.len()
    );
    if !wanted {
        summary.push_str("; the desktop has no system or crash log source (use --source app)");
    }
    ctx.rep.summary(summary);

    if args.follow && live {
        match follow(ctx, &mut tails, session.pid, &filter) {
            FollowEnd::Exited(ended) => {
                remove_session(&project, session.pid);
                ctx.rep.summary(format!(
                    "followed the desktop app until it {}",
                    ended.describe()
                ));
            }
            FollowEnd::Signal | FollowEnd::Deadline => {
                ctx.rep.summary("stopped following; the app keeps running");
            }
        }
    }
    Ok(())
}

// ---- commands ----------------------------------------------------------------------

/// `icm run desktop`.
pub fn run(ctx: &mut Ctx, args: &RunArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    let profile = profile(args.release);
    ctx.rep.set("profile", json!(cargo::profile_dir(profile)));
    ctx.rep.set("device", device_json());
    let extra = parse_env(&args.env)?;

    if ctx.dry_run() {
        plan(ctx, &project, profile, Some(args))?.report(ctx);
        return Ok(());
    }

    let _lock = ctx.lock_platform(PLATFORM)?;
    ctx.rep.latest(PLATFORM);
    let exe = if args.no_build {
        previous_build(&project, profile)?
    } else {
        compile(ctx, &project, profile)?
    };
    ctx.rep.artifact("bundle", &exe);

    // `run` replaces this project's running desktop app (Appendix C item 13).
    if let Some(replaced) = stop_session(ctx, &project)? {
        ctx.rep.set("replaced", replaced);
    }

    let run_dir = run_dir(ctx, &project)?;
    let launched = launch(ctx, &project, &exe, &run_dir, &extra)?;
    let mut session = Session {
        platform: PLATFORM.to_string(),
        pid: launched.pid,
        pgid: launched.pid,
        run: ctx.rep.run_id(),
        run_dir: run_dir.clone(),
        exe: exe.clone(),
        cwd: project.dir().to_path_buf(),
        stdout: launched.stdout.clone(),
        stderr: launched.stderr.clone(),
        launched: launched.launched.clone(),
        profile: cargo::profile_dir(profile).to_string(),
        window: None,
        ready: None,
    };
    write_session(&project, &session);
    ctx.rep
        .set("session", json!(paths::display(&session_path(&project))));
    ctx.rep.artifact("stderr", &launched.stderr_copy);
    ctx.rep.artifact("stdout", &run_dir.join("app.stdout"));

    // Until the run succeeds, a signal to icm stops the app too.
    signals::register_group(launched.pid);
    let mut watch = Watch::new(launched.pid, &launched.stderr);
    let outcome = observe(ctx, &project, args, &launched, &mut watch, &mut session);

    match outcome {
        Ok(_) => {
            signals::unregister_group(launched.pid);
            // The project's `[checks] desktop` scripts (design §13.6).
            crate::hooks::run_for(
                ctx,
                &project,
                &crate::hooks::HookContext {
                    platform: PLATFORM.to_string(),
                    pid: u32::try_from(launched.pid).ok(),
                    bin: Some(exe.clone()),
                    app_stderr: Some(launched.stderr.clone()),
                    logs: Some(session.run_dir.join("logs.ndjson")),
                    log_mark: Some(launched.launched.clone()),
                    ..crate::hooks::HookContext::default()
                },
            )?;
            if args.attach {
                attach(ctx, &project, &session, &launched)
            } else {
                Ok(())
            }
        }
        Err((error, ready)) => {
            // A failed run leaves nothing running. The group stays
            // registered until the app is gone, and the watchdog waits for
            // the cleanup, so an interrupted run never leaves the app or
            // its session behind.
            let _cleanup = signals::cleanup();
            let exit = match watch.ended {
                Some(ended) => ended.to_json(),
                None => json!({"stopped_by_icm": terminate(launched.pid, launched.pid)}),
            };
            signals::unregister_group(launched.pid);
            remove_session(&project, launched.pid);
            write_logs(ctx, &session);
            ctx.rep.set(
                "process",
                process_json(launched.pid, false, ready.as_ref(), Some(exit)),
            );
            Err(error)
        }
    }
}

/// Everything after the launch: ready, settle, screenshot, logs. On error,
/// also returns how far readiness got.
fn observe(
    ctx: &Ctx,
    project: &Project,
    args: &RunArgs,
    launched: &Launched,
    watch: &mut Watch,
    session: &mut Session,
) -> std::result::Result<Ready, (IcmError, Option<Ready>)> {
    let ready = wait_ready(ctx, project, watch, launched, args.wait_ready)
        .map_err(|error| (error, None))?;

    ctx.rep
        .check(Check::pass(CheckId::RunReady, describe_ready(&ready)));
    let mut event = json!({
        "session": paths::display(&session_path(project)),
        "source": ready.source,
        "ms_since_launch": ready.ms,
    });
    if let Some(hint) = ready.hint {
        event["window"] = hint.to_json();
    }
    ctx.rep.ready(event);
    session.window = ready.hint;
    session.ready = ready.event.clone();
    write_session(project, session);

    let fail = |error: IcmError, ready: Ready| (error, Some(ready));
    if let Err(error) = settle(project, watch, launched, args.settle) {
        return Err(fail(error, ready));
    }
    ctx.rep.check(Check::pass(
        CheckId::RunAlive,
        format!(
            "pid {} is running {} after launch",
            launched.pid,
            format_duration(launched.started.elapsed())
        ),
    ));
    for warning in &watch.warnings {
        let code = warning.get("code").and_then(Value::as_str).unwrap_or("");
        let message = warning.get("message").and_then(Value::as_str).unwrap_or("");
        if code == "font.default_missing" {
            ctx.rep
                .check(Check::warn(CheckId::RunFontMissing, message.to_string()));
        } else {
            ctx.rep
                .progress(format!("the app warned {code}: {message}"));
        }
    }

    let mut summary = format!(
        "the app is running (pid {}); {}",
        launched.pid,
        describe_ready(&ready)
    );
    if !args.no_shot {
        let out = session.run_dir.join("screen.png");
        match screenshot(
            ctx,
            project,
            launched.pid,
            ready.hint,
            &out,
            args.expect_content,
        ) {
            Ok(Some(shot)) => {
                summary.push_str(&format!(
                    "; screenshot from the {}{}",
                    if shot.source == "window" {
                        "window"
                    } else {
                        "headless renderer"
                    },
                    if shot.blank.is_blank() {
                        " (blank)"
                    } else {
                        ""
                    }
                ));
            }
            Ok(None) => summary.push_str("; no screenshot"),
            Err(error) => return Err(fail(error, ready)),
        }
        // The app may have died while it was captured.
        watch.poll();
        if let Some(error) = watch.failure(project, launched, true) {
            return Err(fail(error, ready));
        }
    }

    write_logs(ctx, session);
    ctx.rep.set(
        "process",
        process_json(launched.pid, true, Some(&ready), None),
    );
    ctx.rep.summary(summary);
    if !args.attach {
        ctx.rep.next(
            "icm logs desktop --level warn --json",
            "read the app's output",
        );
        ctx.rep
            .next("icm shot desktop --json -q", "screenshot it again");
        ctx.rep.next("icm stop desktop", "terminate the app");
    }
    Ok(ready)
}

/// `--attach`: streams the app's logs (from the launch) until it exits (its
/// exit decides the result) or icm gets a signal (which stops the app).
fn attach(ctx: &Ctx, project: &Project, session: &Session, launched: &Launched) -> Result<()> {
    let (records, mut tails) = read_records(session);
    for record in &records {
        emit_record(ctx, record, session.pid);
    }
    signals::register_group(session.pid);
    let end = follow(ctx, &mut tails, session.pid, &Filter::default());
    // As in `run`: registered until the app is gone, and the watchdog waits.
    let _cleanup = signals::cleanup();
    let how = match end {
        FollowEnd::Signal | FollowEnd::Deadline => Some(terminate(session.pid, session.pgid)),
        FollowEnd::Exited(_) => None,
    };
    signals::unregister_group(session.pid);
    remove_session(project, session.pid);
    match end {
        FollowEnd::Exited(ended) => {
            write_logs(ctx, session);
            // "The app is running" no longer holds; an error's summary
            // comes from the error, a clean exit sets its own below.
            ctx.rep.clear_summary();
            ctx.rep.set(
                "process",
                json!({"pid": session.pid, "alive": false, "exit": ended.to_json()}),
            );
            let mut watch = Watch::new(session.pid, &session.stderr);
            watch.read();
            watch.ended = Some(ended);
            match ended {
                _ if watch.panic.is_some() => Err(watch
                    .failure(project, launched, true)
                    .unwrap_or_else(|| died_error(ended, launched, true))),
                // A normal exit, or `icm stop desktop` from elsewhere.
                Ended::Exited(0) | Ended::Gone => {
                    ctx.rep.summary("the app exited normally");
                    Ok(())
                }
                Ended::Signaled(signal)
                    if [libc::SIGTERM, libc::SIGINT, libc::SIGHUP].contains(&signal) =>
                {
                    ctx.rep
                        .summary(format!("the app was stopped by {}", signals::name(signal)));
                    Ok(())
                }
                other => Err(died_error(other, launched, true)),
            }
        }
        FollowEnd::Signal | FollowEnd::Deadline => {
            let how = how.unwrap_or("already exited");
            write_logs(ctx, session);
            ctx.rep.set(
                "process",
                json!({"pid": session.pid, "alive": false, "stopped": how}),
            );
            ctx.rep
                .summary(format!("stopped following; the app: {how}"));
            Ok(())
        }
    }
}

/// `icm shot desktop`: captures the running app again.
pub fn shot(ctx: &mut Ctx, args: &ShotArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    let run_dir = run_dir(ctx, &project)?;
    let name = match &args.name {
        Some(name) => {
            let clean: String = name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '-'
                    }
                })
                .collect();
            format!("screen-{clean}.png")
        }
        None => "screen.png".to_string(),
    };
    let out = match &args.out {
        Some(path) => std::path::absolute(path)
            .map_err(|error| internal(&format!("bad --out {}", path.display()), error))?,
        None => run_dir.join(name),
    };

    // Planned before the session is read: a dry run needs no running app,
    // and leaves a stale session record where it is.
    if ctx.dry_run() {
        let app = match read_session(&project) {
            Some(session) => format!("pid {}", session.pid),
            None => "none runs now".to_string(),
        };
        let mut plan = Plan::new();
        plan.push(Step::internal(
            "desktop.screenshot",
            &format!(
                "capture the window of the app in {} ({app}) to {} (headless render without Screen Recording)",
                paths::display(&session_path(&project)),
                paths::display(&out)
            ),
        ));
        plan.report(ctx);
        ctx.rep
            .summary("the plan of icm shot desktop (--dry-run: nothing ran)");
        return Ok(());
    }

    let Some(session) = read_session(&project) else {
        return Err(no_session("no desktop app is running for this project"));
    };
    if !running(&session) {
        remove_session(&project, session.pid);
        return Err(no_session(format!(
            "the desktop app (pid {}) of run {} is no longer running",
            session.pid, session.run
        )));
    }
    ctx.rep
        .set("session", json!(paths::display(&session_path(&project))));
    ctx.rep.set(
        "process",
        json!({"pid": session.pid, "alive": true, "run": session.run}),
    );

    match screenshot(ctx, &project, session.pid, session.window, &out, false)? {
        Some(shot) => ctx.rep.summary(format!(
            "captured the desktop app (pid {}) from the {}",
            session.pid,
            if shot.source == "window" {
                "window"
            } else {
                "headless renderer"
            }
        )),
        None => ctx.rep.summary("no screenshot could be made"),
    }
    Ok(())
}

/// `icm stop desktop`.
pub fn stop(ctx: &mut Ctx, _args: &StopArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    if ctx.dry_run() {
        let mut plan = Plan::new();
        plan.push(Step::internal(
            "desktop.stop",
            &format!(
                "SIGTERM the process group in {}, SIGKILL after {}",
                paths::display(&session_path(&project)),
                format_duration(STOP_GRACE)
            ),
        ));
        plan.report(ctx);
        return Ok(());
    }
    let stopped = stop_session(ctx, &project)?;
    ctx.rep.summary(match &stopped {
        Some(stopped) => format!(
            "desktop app pid {}: {}",
            stopped["pid"],
            stopped["how"].as_str().unwrap_or("stopped")
        ),
        None => "no desktop app was running for this project".to_string(),
    });
    ctx.rep
        .set("stopped", json!(stopped.into_iter().collect::<Vec<_>>()));
    Ok(())
}

/// `icm input desktop …`: not in phase 1 (design §6).
pub fn input(_ctx: &mut Ctx, _args: &InputArgs) -> Result<()> {
    Err(IcmError::new(
        CheckId::InputUnsupported,
        "input to a desktop app comes with the agent bridge (phase 6); drive the UI headlessly with `.ice` flows instead",
    )
    .fix(
        "Use the headless harness: read the widget tree, then write and run a `.ice` flow.",
        &["icm ui --headless tree --json", "icm test --json -q"],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_lines_parse() {
        assert_eq!(
            panicked_at("thread 'main' panicked at src/lib.rs:41:9:"),
            Some((Some("main".into()), Some("src/lib.rs:41:9".into())))
        );
        assert_eq!(
            panicked_at("thread '<unnamed>' panicked at /x/y.rs:1:2:"),
            Some((Some("<unnamed>".into()), Some("/x/y.rs:1:2".into())))
        );
        assert_eq!(
            panicked_at("thread 'main' (14513581) panicked at examples/app/src/lib.rs:108:13:"),
            Some((
                Some("main".into()),
                Some("examples/app/src/lib.rs:108:13".into())
            ))
        );
        assert_eq!(panicked_at("all good"), None);
        assert_eq!(split_location("src/lib.rs:41:9"), Some(("src/lib.rs", 41)));
        assert_eq!(split_location("C:/x.rs:3:4"), Some(("C:/x.rs", 3)));
        assert_eq!(split_location("nowhere"), None);
    }

    #[test]
    fn the_watch_reads_events_and_panics() {
        let dir = tempfile::tempdir().unwrap();
        let stderr = dir.path().join("app.stderr");
        std::fs::write(
            &stderr,
            "ICM_EVENT {\"v\":1,\"kind\":\"start\",\"protocol\":1}\nICM_EVENT {\"v\":1,\"kind\":\"ready\",\"ms\":5,\"window\":{\"size\":[1024,768],\"physical\":[2048,1536],\"scale\":2}}\n",
        )
        .unwrap();
        // A pid that is not this process's child and does not exist.
        let mut watch = Watch::new(i32::MAX, &stderr);
        watch.read();
        assert!(watch.start.is_some());
        let hint = WindowHint::from_ready(watch.ready.as_ref().unwrap()).unwrap();
        assert_eq!(hint.size, (1024.0, 768.0));
        assert_eq!(hint.physical, Some((2048, 1536)));
        assert_eq!(hint.scale, 2.0);

        let mut text = std::fs::read_to_string(&stderr).unwrap();
        text.push_str("ICM_EVENT {\"v\":1,\"kind\":\"panic\",\"message\":\"boom\",\"location\":\"src/lib.rs:4:1\",\"thread\":\"main\"}\n");
        text.push_str("\nthread 'main' panicked at src/lib.rs:4:1:\nboom\n");
        std::fs::write(&stderr, text).unwrap();
        watch.read();
        let panic = watch.panic.clone().unwrap();
        assert_eq!(panic.message, "boom");
        assert_eq!(panic.location.as_deref(), Some("src/lib.rs:4:1"));
        assert_eq!(panic.line, Some(5));
        assert!(panic.from_text);

        // A panic seen only as text takes its message from the next line.
        let other = dir.path().join("other.stderr");
        std::fs::write(
            &other,
            "thread 'main' panicked at src/main.rs:2:5:\nexplicit panic\n",
        )
        .unwrap();
        let mut watch = Watch::new(i32::MAX, &other);
        watch.read();
        assert_eq!(watch.panic.unwrap().message, "explicit panic");
    }

    #[test]
    fn processes_end_and_are_reaped() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let pid = process::spawn_detached(&Cmd::new("/bin/sh").args(["-c", "exit 3"]), &out, &out)
            .unwrap() as i32;
        let until = Instant::now() + Duration::from_secs(5);
        let ended = loop {
            if let Some(ended) = reap(pid) {
                break ended;
            }
            assert!(Instant::now() < until, "pid {pid} did not end");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(ended, Ended::Exited(3));
        assert_eq!(reap(pid), Some(Ended::Gone));

        let pid =
            process::spawn_detached(&Cmd::new("/bin/sleep").arg("30"), &out, &out).unwrap() as i32;
        assert_eq!(reap(pid), None);
        assert_eq!(terminate(pid, pid), "stopped with SIGTERM");
        assert!(!signals::alive(pid));
    }

    #[test]
    fn sessions_round_trip_and_check_their_pid() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let pid =
            process::spawn_detached(&Cmd::new("/bin/sleep").arg("30"), &out, &out).unwrap() as i32;
        let mut session = Session {
            platform: PLATFORM.into(),
            pid,
            pgid: pid,
            run: "r".into(),
            run_dir: dir.path().into(),
            exe: PathBuf::from("/bin/sleep"),
            cwd: dir.path().into(),
            stdout: out.clone(),
            stderr: out.clone(),
            launched: "2026-10-06T00:00:00.000Z".into(),
            profile: "debug".into(),
            window: Some(WindowHint {
                size: (1024.0, 768.0),
                physical: Some((2048, 1536)),
                scale: 2.0,
            }),
            ready: None,
        };
        let text = serde_json::to_string(&session).unwrap();
        assert_eq!(serde_json::from_str::<Session>(&text).unwrap(), session);

        assert!(running(&session));
        // The same pid running something else is not the session's app.
        session.exe = PathBuf::from("/usr/bin/not-sleep");
        assert!(!running(&session));
        session.exe = PathBuf::from("/bin/sleep");
        assert_eq!(terminate(pid, pid), "stopped with SIGTERM");
        assert!(!running(&session));
        assert_eq!(terminate(pid, pid), "already exited");
    }

    #[test]
    fn env_pairs_and_names() {
        assert_eq!(
            parse_env(&["RUST_LOG=debug".into(), "A=b=c".into()]).unwrap(),
            vec![
                ("RUST_LOG".to_string(), "debug".to_string()),
                ("A".to_string(), "b=c".to_string())
            ]
        );
        assert_eq!(parse_env(&["=x".into()]).unwrap_err().id, "usage.bad_args");
        assert_eq!(parse_env(&["x".into()]).unwrap_err().id, "usage.bad_args");
        assert_eq!(
            preview_path(Path::new("/r/screen.png")),
            Path::new("/r/screen.preview.png")
        );
        assert_eq!(trim_float(2.0), "2");
        assert_eq!(trim_float(2.625), "2.625");
    }

    #[test]
    fn window_captures_lose_their_title_bar() {
        let hint = WindowHint {
            size: (1024.0, 768.0),
            physical: Some((2048, 1536)),
            scale: 2.0,
        };
        // A 32 pt title bar at @2 above the content.
        let mut capture = image::Image::filled(2048, 1600, [200, 200, 200, 255]);
        capture.rgba[..2048 * 64 * 4].fill(0);
        let content = content_of(&capture, Some(hint), 2.0).unwrap();
        assert_eq!((content.width, content.height), (2048, 1536));
        assert!(content.rgba.iter().all(|byte| *byte >= 200));

        // Already the content, another width, or far too tall: kept as is.
        let exact = image::Image::filled(2048, 1536, [0, 0, 0, 255]);
        assert!(content_of(&exact, Some(hint), 2.0).is_none());
        let resized = image::Image::filled(1000, 1600, [0, 0, 0, 255]);
        assert!(content_of(&resized, Some(hint), 2.0).is_none());
        let tall = image::Image::filled(2048, 1800, [0, 0, 0, 255]);
        assert!(content_of(&tall, Some(hint), 2.0).is_none());
        assert!(content_of(&capture, None, 2.0).is_none());
    }
}
