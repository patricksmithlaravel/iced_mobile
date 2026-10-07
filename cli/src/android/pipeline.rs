//! The Android commands (design §10.4, §13):
//!
//! - `build`: the signed dev APK for one ABI;
//! - `run`: device (booting the managed emulator while the build runs),
//!   build, install, `setprop debug.icm.events 1`, launch, readiness
//!   (`ICM_EVENT ready`, else a probe), screenshot, logs, session;
//! - `shot`, `logs`, `input`, `stop`, `devices` on the session's device;

use super::Toolset;
use super::adb::{self, Adb};
use super::apk;
use super::avd;
use super::device::{self, Chosen};
use super::image;
use super::logcat::{self, Record};
use super::manifest::ACTIVITY;
use super::session::{self, Geometry, Session};
use crate::catalogue::CheckId;
use crate::cli::{
    BuildArgs, InputAction, InputArgs, Key, LogSource, LogsArgs, Rotation, RunArgs, ShotArgs,
    StopArgs, Theme,
};
use crate::config::Abi;
use crate::context::{Ctx, Project};
use crate::error::{Check, Evidence, IcmError, Result, Status};
use crate::host::HostConfig;
use crate::screen::{Screen, Space, preview_size};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn setup(ctx: &mut Ctx) -> Result<(Project, HostConfig, Toolset)> {
    let project = ctx.project()?.clone();
    let host = ctx.host()?.clone();
    let tools = Toolset::discover(&host, &ctx.env)?;
    Ok((project, host, tools))
}

fn run_dir(ctx: &Ctx, project: &Project) -> PathBuf {
    ctx.rep
        .run_dir()
        .unwrap_or_else(|| project.runs_dir().join(ctx.rep.run_id()))
}

fn internal(what: &str, error: impl std::fmt::Display) -> IcmError {
    IcmError::new(CheckId::InternalBug, format!("{what}: {error}"))
}

// ---- build ---------------------------------------------------------------------

/// `icm build android`: the signed dev APK for `--abi`, the `--device`'s
/// ABI, or the host's emulator ABI.
pub fn build(ctx: &mut Ctx, args: &BuildArgs) -> Result<()> {
    let (project, _host, tools) = setup(ctx)?;
    let _lock = ctx.lock_platform("android")?;
    let ctx: &Ctx = ctx;

    let abi = match (&args.abi, &args.device) {
        (Some(name), _) => Abi::from_name(name).ok_or_else(|| {
            IcmError::new(
                CheckId::UsageBadArgs,
                format!("unknown ABI `{name}`; use arm64-v8a, x86_64, armeabi-v7a or x86"),
            )
        })?,
        (None, Some(serial)) => device::device_abi(&Adb::new(&tools, serial)?)?,
        (None, None) => avd::host_abi(),
    };
    ctx.rep.set(
        "profile",
        json!(crate::cargo::profile_dir(if args.release {
            "release"
        } else {
            "dev"
        })),
    );
    let built = apk::build(ctx, &project, &tools, abi, args.release)?;
    ctx.rep.artifact("apk", &built.apk);
    ctx.rep.summary(format!(
        "built {} ({})",
        crate::paths::display(&built.apk),
        abi.as_str()
    ));
    ctx.rep.next(
        "icm run android --no-build --json -q",
        "install and launch this build",
    );
    Ok(())
}

// ---- run -----------------------------------------------------------------------

/// `(name, value)` system properties.
type Props = Vec<(String, String)>;

/// System properties standing in for `--env` (Android apps get no
/// environment), and the variables that have no stand-in.
fn props_from_env(env: &[String]) -> Result<(Props, Vec<String>)> {
    let mut props = Vec::new();
    let mut ignored = Vec::new();
    for pair in env {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(IcmError::new(
                CheckId::UsageBadArgs,
                format!("--env {pair}: expected K=V"),
            ));
        };
        match key {
            "ICED_BACKEND" => props.push(("debug.iced.backend".to_string(), value.to_string())),
            "ICM_EVENTS" => {}
            _ => ignored.push(key.to_string()),
        }
    }
    Ok((props, ignored))
}

/// `icm run android`.
pub fn run(ctx: &mut Ctx, args: &RunArgs) -> Result<()> {
    if args.from_aab {
        return Err(IcmError::new(
            CheckId::UsageNotImplemented,
            "`--from-aab` (install through bundletool) comes with Android releases",
        )
        .fix("Run without --from-aab.", &["icm run android"]));
    }
    let (props, ignored_env) = props_from_env(&args.env)?;
    let (project, host, tools) = setup(ctx)?;
    ctx.rep.latest("android");
    let _lock = ctx.lock_platform("android")?;
    let ctx: &Ctx = ctx;

    let config = &project.config.config;
    let app_id = config.app.id.clone();
    let profile = if args.release { "release" } else { "dev" };
    ctx.rep
        .set("profile", json!(crate::cargo::profile_dir(profile)));
    for key in &ignored_env {
        ctx.rep.check(Check::warn(
            CheckId::UsageBadArgs,
            format!(
                "--env {key}: Android apps get no environment variables; only ICED_BACKEND has a stand-in (the sysprop debug.iced.backend)"
            ),
        ));
    }
    let dir = run_dir(ctx, &project);

    // 1. The device; an emulator boots while the app builds.
    let request = device::Request {
        serial: args.device.clone(),
        avd: args.avd.clone(),
        show: args.show,
        wipe: args.fresh,
        target_sdk: config.android.target_sdk,
    };
    let chosen = device::choose(
        ctx,
        &tools,
        &host,
        &ctx.env,
        &request,
        true,
        &dir.join("emulator.log"),
    )?;
    ctx.rep
        .progress(format!("device: {} ({})", chosen.serial, chosen.reason));
    // An emulator an earlier run booted is still icm's to shut down: a
    // rerun on it must not forget that.
    let booted_earlier = match &chosen.booting {
        Some(booting) => {
            session::write_booted(
                &project,
                &session::Booted {
                    serial: booting.serial.clone(),
                    avd: booting.avd.clone(),
                    emulator_pid: Some(booting.pid),
                    emulator_log: Some(booting.log.clone()),
                },
            );
            None
        }
        None => {
            // The record, else (files from before it) the last session's.
            let previous = session::read(&project)
                .filter(|previous| previous.booted_by_icm)
                .map(|previous| session::Booted {
                    serial: previous.serial,
                    avd: previous.avd.unwrap_or_default(),
                    emulator_pid: previous.emulator_pid,
                    emulator_log: previous.emulator_log,
                })
                .filter(session::Booted::alive);
            session::booted(&project)
                .into_iter()
                .chain(previous)
                .find(|booted| {
                    booted.serial == chosen.serial
                        && chosen.avd.as_deref().is_none_or(|avd| avd == booted.avd)
                })
                .inspect(|booted| session::write_booted(&project, booted))
        }
    };
    let mut session = Session {
        schema: session::SCHEMA.to_string(),
        run: ctx.rep.run_id(),
        run_dir: Some(dir.clone()),
        serial: chosen.serial.clone(),
        kind: chosen.kind().to_string(),
        avd: chosen.avd.clone(),
        booted_by_icm: chosen.booting.is_some() || booted_earlier.is_some(),
        emulator_pid: chosen
            .booting
            .as_ref()
            .map(|b| b.pid)
            .or_else(|| booted_earlier.as_ref().and_then(|p| p.emulator_pid)),
        emulator_log: chosen
            .booting
            .as_ref()
            .map(|b| b.log.clone())
            .or_else(|| booted_earlier.as_ref().and_then(|p| p.emulator_log.clone())),
        abi: chosen.abi.as_str().to_string(),
        app_id: app_id.clone(),
        started: crate::time::Utc::now().rfc3339(),
        ..Session::default()
    };
    if let Some(booting) = &chosen.booting {
        // Recorded now, so `icm stop android --shutdown` finds the emulator
        // even when the build fails.
        let _ = session::write(&project, &session);
        ctx.rep.artifact("emulator_log", &booting.log);
    }

    // 2. The APK.
    let apk_path = if args.no_build {
        let package = project.package_for("android")?;
        let path = apk::apk_path(&project, &package.name, profile);
        if !path.is_file() {
            return Err(IcmError::new(
                CheckId::UsageBadArgs,
                format!(
                    "--no-build: there is no APK at {} from an earlier build",
                    crate::paths::display(&path)
                ),
            )
            .fix("Build it first.", &["icm build android", "icm run android"]));
        }
        let names = super::zip::names(&path).unwrap_or_default();
        if !names
            .iter()
            .any(|name| name.starts_with(&format!("lib/{}/", chosen.abi.as_str())))
        {
            return Err(IcmError::new(
                CheckId::AndroidSoAbis,
                format!(
                    "--no-build: {} has no library for {}, the device's ABI",
                    crate::paths::display(&path),
                    chosen.abi.as_str()
                ),
            )
            .fix_commands([format!("icm build android --abi {}", chosen.abi.as_str())]));
        }
        path
    } else {
        apk::build(ctx, &project, &tools, chosen.abi, args.release)?.apk
    };
    ctx.rep.artifact("apk", &apk_path);
    session.apk = Some(apk_path.clone());

    // 3. Boot, install, launch.
    if let Some(booting) = &chosen.booting {
        ctx.rep
            .progress(format!("waiting for {} to boot", booting.serial));
        let took = avd::wait_booted(ctx, &tools, booting)?;
        ctx.rep.progress(format!(
            "{} booted in {}",
            booting.serial,
            crate::time::format_duration(took)
        ));
    }
    let adb = Adb::new(&tools, &chosen.serial)?;
    if chosen.managed() {
        avd::prepare(&adb);
    }
    let device_json = device::to_json(&chosen, &adb);
    ctx.rep.set("device", device_json.clone());
    // "emulator-5580 (icm-api36, API 36)" for the summary.
    let device_name = {
        let mut about: Vec<String> = Vec::new();
        if let Some(name) = chosen
            .avd
            .clone()
            .or_else(|| device_json["model"].as_str().map(str::to_string))
        {
            about.push(name);
        }
        if let Some(api) = device_json["api"].as_u64() {
            about.push(format!("API {api}"));
        }
        if about.is_empty() {
            chosen.serial.clone()
        } else {
            format!("{} ({})", chosen.serial, about.join(", "))
        }
    };

    install(
        ctx,
        &adb,
        &apk_path,
        &app_id,
        args.reinstall,
        args.wipe_data,
    )?;

    let mark = adb.epoch().ok_or_else(|| {
        IcmError::new(
            CheckId::AndroidDeviceNone,
            format!("{} does not answer `date`", adb.serial),
        )
    })?;
    session.log_mark = Some(mark.clone());
    set_props(ctx, &adb, &props)?;

    let launched = Instant::now();
    launch(ctx, &adb, &app_id)?;

    // 4. Ready, screenshot, logs.
    let outcome = wait_ready(ctx, &adb, &app_id, &mark, launched, args.wait_ready);
    let pids: BTreeSet<u32> = match &outcome {
        Ok(ready) => ready.pids.clone(),
        Err(_) => adb.pids(&app_id).into_iter().collect(),
    };
    session.pid = pids.iter().next().copied();

    let mut result = outcome.and_then(|ready| {
        let readiness = report_ready(ctx, &project, &ready, launched);
        ctx.rep.summary(format!(
            "{} ({app_id}) is running on {device_name}; {readiness}",
            config.app.name
        ));
        if !args.no_shot {
            std::thread::sleep(args.settle);
            let shot = capture(ctx, &adb, &dir, "screen", &app_id, args.expect_content)?;
            session.screen = Some(Geometry::from(&shot));
            // Dying between the first frame and the screenshot is dying.
            if adb.pids(&app_id).is_empty() {
                return Err(IcmError::new(
                    CheckId::RunAppDied,
                    format!("{app_id} exited after its first frame"),
                ));
            }
        }
        ctx.rep.set(
            "process",
            json!({
                "pid": ready.pids.iter().next(),
                "alive": true,
                "ready": {"source": ready.source, "ms": ready.ms},
            }),
        );
        Ok(())
    });

    let logs = collect_logs(ctx, &adb, &dir, &app_id, &mark, &pids);
    let recreated = recreation(ctx, &adb, &dir, &project, &mark);
    if let Err(error) = &mut result {
        attach_evidence(error, &logs, &project);
        if let Some(recreated) = &recreated {
            // The relaunch is why the app never drew, or stopped answering.
            error.likely_causes.insert(0, recreated.cause.clone());
            if [CheckId::RunNotReady.id(), CheckId::RunAnr.id()].contains(&&*error.id) {
                error.fix.summary = recreated.fix.clone();
                error.fix.commands = recreated.commands.clone();
                if let Some(evidence) = &recreated.evidence {
                    error.evidence.insert(0, evidence.clone());
                }
            }
        }
        ctx.rep.set(
            "process",
            json!({
                "pid": session.pid,
                "alive": !adb.pids(&app_id).is_empty(),
                "ready": {"source": "none", "ms": null},
            }),
        );
    }

    if let Ok(path) = session::write(&project, &session) {
        ctx.rep.set("session", json!(crate::paths::display(&path)));
    }
    result?;

    // The project's `[checks] android` scripts (design §13.6), with the
    // JDK and SDK environment (Appendix C item 2).
    crate::hooks::run_for(
        ctx,
        &project,
        &crate::hooks::HookContext {
            platform: "android".to_string(),
            pid: session.pid,
            device: Some(adb.serial.clone()),
            adb: Some(format!(
                "{} -s {}",
                crate::process::shell_quote(&tools.sdk.adb(&ctx.env).display().to_string()),
                adb.serial
            )),
            bin: Some(apk_path.clone()),
            logs: Some(dir.join("logs.ndjson")),
            log_mark: Some(mark.clone()),
            env: tools.child_env().to_vec(),
            ..crate::hooks::HookContext::default()
        },
    )?;

    ctx.rep.next(
        "icm logs android --level warn --json",
        "read the app's warnings and errors",
    );
    ctx.rep.next(
        "icm input android tap <x> <y> --json -q",
        "tap in screen.preview.png pixels",
    );
    ctx.rep.next("icm stop android --json -q", "stop the app");

    if args.attach {
        match follow(ctx, &adb, &app_id, &mark, pids.clone(), true, &|_, _| true)? {
            Followed::Gone { stopped: true } => {
                ctx.rep
                    .summary(format!("{app_id} was stopped (am force-stop)"));
            }
            Followed::Gone { stopped: false } => {
                // The app's own exit decides the result, as on the other
                // platforms: a panic or crash is the run's failure.
                let mut error = IcmError::new(
                    CheckId::RunAppDied,
                    format!("{app_id} exited during --attach, after its first frame"),
                );
                let logs = collect_logs(ctx, &adb, &dir, &app_id, &mark, &pids);
                attach_evidence(&mut error, &logs, &project);
                ctx.rep
                    .set("process", json!({"pid": session.pid, "alive": false}));
                ctx.rep.clear_summary();
                return Err(error);
            }
            Followed::Ended => {}
        }
    }
    Ok(())
}

fn set_props(ctx: &Ctx, adb: &Adb, props: &[(String, String)]) -> Result<()> {
    // Events are opt-in for every build (Appendix C item 27).
    let mut lines = vec!["setprop debug.icm.events 1".to_string()];
    let backend = props
        .iter()
        .find(|(key, _)| key == "debug.iced.backend")
        .map(|(_, value)| value.clone());
    match backend {
        Some(value) => lines.push(format!("setprop debug.iced.backend {}", adb::quote(&value))),
        // A backend an earlier run chose must not leak into this one.
        None => lines.push("setprop debug.iced.backend ''".to_string()),
    }
    let cmd = adb
        .shell(&lines.join(" && "))
        .timeout(Duration::from_secs(30));
    let outcome = ctx.step("adb.setprop", &cmd)?;
    if outcome.success() {
        Ok(())
    } else {
        Err(ctx.step_failure("adb.setprop", CheckId::ToolFailed, &outcome))
    }
}

fn install(
    ctx: &Ctx,
    adb: &Adb,
    apk: &Path,
    app_id: &str,
    reinstall: bool,
    wipe: bool,
) -> Result<()> {
    if reinstall {
        let line = if wipe {
            format!("pm uninstall {}", adb::quote(app_id))
        } else {
            format!("pm uninstall -k {}", adb::quote(app_id))
        };
        // Not installed is fine.
        let _ = ctx.step(
            "adb.uninstall",
            &adb.shell(&line).timeout(Duration::from_secs(120)),
        )?;
    }

    let cmd = adb
        .cmd(["install", "-r", "-d"])
        .arg(apk)
        .timeout(Duration::from_secs(300));
    let outcome = ctx.step("adb.install", &cmd)?;
    let text = format!("{}\n{}", outcome.stdout_text(), outcome.stderr_text());
    if outcome.success() && text.contains("Success") {
        return Ok(());
    }

    let reason = adb::install_failure(&text).unwrap_or_else(|| outcome.describe());
    let mut error = match reason.as_str() {
        "INSTALL_FAILED_UPDATE_INCOMPATIBLE" => IcmError::new(
            CheckId::AndroidInstallSignatureMismatch,
            format!(
                "{app_id} is installed on {} with a different signing key; reinstalling wipes its data",
                adb.serial
            ),
        )
        .fix_commands(["icm run android --reinstall --wipe-data"]),
        "INSTALL_FAILED_NO_MATCHING_ABIS" => IcmError::new(
            CheckId::AndroidSoAbis,
            format!("{} has no library for {}'s ABI", apk.display(), adb.serial),
        ),
        _ => IcmError::new(
            CheckId::AndroidInstallFailed,
            format!("adb install failed on {}: {reason}", adb.serial),
        ),
    };
    if let Some(log) = &outcome.log {
        error = error.evidence(Evidence::file(log).with_excerpt(reason));
    }
    Err(error)
}

fn launch(ctx: &Ctx, adb: &Adb, app_id: &str) -> Result<adb::Started> {
    let component = format!("{app_id}/{ACTIVITY}");
    let cmd = adb
        .shell(&format!("am start -W -S -n {}", adb::quote(&component)))
        .timeout(Duration::from_secs(90));
    let outcome = ctx.step("adb.launch", &cmd)?;
    let text = format!("{}\n{}", outcome.stdout_text(), outcome.stderr_text());
    let started = adb::parse_am_start(&text);
    if !outcome.success() || started.error.is_some() {
        let mut error = IcmError::new(
            CheckId::AndroidLaunchFailed,
            format!(
                "am start {component} failed: {}",
                started
                    .error
                    .clone()
                    .unwrap_or_else(|| outcome.stderr_tail(3))
            ),
        );
        if let Some(log) = &outcome.log {
            error = error.evidence(Evidence::file(log));
        }
        return Err(error);
    }
    Ok(started)
}

/// How the app became ready.
#[derive(Clone, Debug)]
struct Ready {
    source: &'static str,
    ms: Option<u64>,
    window: Option<Value>,
    pids: BTreeSet<u32>,
}

/// How long a relaunched activity gets to draw before the run gives up on
/// it. The relaunch ends the application, and the new activity starts it
/// again in the same process, which takes about as long as a warm start and
/// sends its own `ICM_EVENT start` and `ready`. An app that stays silent
/// this long after a relaunch will not draw (a framework from before the
/// Android lifecycle fix freezes there: winit 0.30.13 does not end its
/// event loop when the activity is destroyed).
const RELAUNCH_GRACE: Duration = Duration::from_secs(10);

/// Waits for `ICM_EVENT ready` in logcat (the framework emits it after
/// the first presented frame). An app that never speaks the protocol (no
/// `start` event) is ready by probe: alive and the top resumed activity on
/// three polls in a row. A death or panic fails at once; so does an
/// activity Android relaunched that does not draw within
/// [`RELAUNCH_GRACE`], which the probe cannot tell from a live one.
fn wait_ready(
    ctx: &Ctx,
    adb: &Adb,
    app_id: &str,
    mark: &str,
    launched: Instant,
    wait: Duration,
) -> Result<Ready> {
    let own = launched + wait;
    let (deadline, overall) = match ctx.deadline() {
        Some(overall) if overall < own => (overall, true),
        _ => (own, false),
    };
    let mut pids: BTreeSet<u32> = BTreeSet::new();
    let mut start_seen = false;
    let mut probes = 0;
    let mut gone_polls = 0;
    let mut relaunched: Option<Instant> = None;

    loop {
        if let Some(signal) = crate::signals::pending() {
            return Err(crate::output::interrupted(signal));
        }
        let records = events_since(adb, mark);
        for record in &records {
            let Some(event) = logcat::event(record) else {
                continue;
            };
            let _ = pids.insert(record.pid);
            match logcat::kind(&event) {
                "start" => start_seen = true,
                "ready" => {
                    return Ok(Ready {
                        source: "icm_event",
                        ms: event.get("ms").and_then(Value::as_u64),
                        window: event.get("window").cloned(),
                        pids,
                    });
                }
                "panic" => {
                    let message = event
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let location = event
                        .get("location")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown location");
                    return Err(IcmError::new(
                        CheckId::RunAppPanicked,
                        format!("panicked at {location}: {message}"),
                    ));
                }
                _ => {}
            }
        }

        let alive = adb.pids(app_id);
        if alive.is_empty() {
            gone_polls += 1;
            // Give a fresh launch a moment to appear; a vanished process
            // (or one that never came) is dead.
            if !pids.is_empty() || gone_polls >= 6 {
                return Err(IcmError::new(
                    CheckId::RunAppDied,
                    format!(
                        "{app_id} exited {} after launch, before its first frame",
                        crate::time::format_duration(launched.elapsed())
                    ),
                ));
            }
        } else {
            gone_polls = 0;
            pids.extend(alive);
        }

        if relaunched.is_none() && !relaunches_since(adb, mark, app_id).is_empty() {
            relaunched = Some(Instant::now());
        }
        if let Some(seen) = relaunched
            && seen.elapsed() >= RELAUNCH_GRACE
        {
            return Err(IcmError::new(
                CheckId::RunNotReady,
                format!(
                    "{app_id} is alive but sent no ICM_EVENT ready in the {} after Android relaunched its activity",
                    crate::time::format_duration(seen.elapsed())
                ),
            ));
        }

        if !start_seen
            && relaunched.is_none()
            && !pids.is_empty()
            && launched.elapsed() >= Duration::from_secs(6)
        {
            if top_resumed(adb, app_id) {
                probes += 1;
                if probes >= 3 {
                    return Ok(Ready {
                        source: "probe",
                        ms: None,
                        window: None,
                        pids,
                    });
                }
            } else {
                probes = 0;
            }
        }

        if Instant::now() >= deadline {
            let waited = crate::time::format_duration(launched.elapsed());
            if overall {
                // The app may be fine: icm ran out of time, not the app.
                return Err(IcmError::new(
                    CheckId::StepTimeout,
                    format!(
                        "the overall --timeout ran out after {waited} while waiting for {app_id}'s first frame"
                    ),
                ));
            }
            let detail = if start_seen {
                format!("{app_id} is alive but sent no ICM_EVENT ready within {waited}")
            } else {
                format!("{app_id} is alive but never became the resumed activity within {waited}")
            };
            return Err(IcmError::new(CheckId::RunNotReady, detail)
                .fix_commands(["icm logs android --level warn --json".to_string()]));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn events_since(adb: &Adb, mark: &str) -> Vec<Record> {
    let cmd = adb.cmd([
        "logcat",
        "-d",
        "-v",
        "threadtime,epoch",
        "-T",
        mark,
        "-s",
        "ICM_EVENT:I",
    ]);
    adb::quick(cmd, Duration::from_secs(15))
        .map(|outcome| logcat::parse(&outcome.stdout_text()))
        .unwrap_or_default()
}

/// The events buffer since `mark`, as logcat prints it (`tags` empty:
/// every tag).
fn events_buffer(adb: &Adb, mark: &str, tags: &[&str]) -> Option<String> {
    let mut args: Vec<String> = [
        "logcat",
        "-d",
        "-b",
        "events",
        "-v",
        "threadtime,epoch",
        "-T",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    args.push(mark.to_string());
    if !tags.is_empty() {
        args.push("-s".to_string());
        args.extend(tags.iter().map(|tag| format!("{tag}:I")));
    }
    let outcome = adb::quick(adb.cmd(&args), Duration::from_secs(15))?;
    outcome.success().then(|| outcome.stdout_text())
}

/// The relaunches of the app's activity since `mark`.
fn relaunches_since(adb: &Adb, mark: &str, app_id: &str) -> Vec<logcat::Relaunch> {
    events_buffer(adb, mark, logcat::RELAUNCH_TAGS)
        .map(|text| logcat::relaunches(&logcat::parse(&text), app_id))
        .unwrap_or_default()
}

fn top_resumed(adb: &Adb, app_id: &str) -> bool {
    adb.shell_text(
        "dumpsys activity activities | grep -E 'topResumedActivity|ResumedActivity'",
        Duration::from_secs(15),
    )
    .is_some_and(|text| text.contains(&format!("{app_id}/")))
}

/// Reports `ready` and `run.ready`; returns how it got ready ("first frame
/// 411x914@2.625 after 0.9 s (source: icm_event)").
fn report_ready(ctx: &Ctx, project: &Project, ready: &Ready, launched: Instant) -> String {
    let window = ready.window.clone().unwrap_or(Value::Null);
    // Logical sizes are fractional on Android (1080 px / 2.625); one
    // decimal is plenty for a person.
    let number = |v: f64| {
        if (v - v.round()).abs() < 0.05 {
            format!("{}", v.round())
        } else {
            format!("{v:.1}")
        }
    };
    let size = window.get("size").and_then(Value::as_array).map(|size| {
        size.iter()
            .filter_map(Value::as_f64)
            .map(number)
            .collect::<Vec<_>>()
            .join("x")
    });
    let scale = window.get("scale").and_then(Value::as_f64);
    let ms = ready
        .ms
        .unwrap_or_else(|| launched.elapsed().as_millis() as u64);
    ctx.rep.ready(json!({
        "session": crate::paths::display(&session::path(project)),
        "source": ready.source,
        "ms_since_launch": launched.elapsed().as_millis() as u64,
        "window": window,
    }));
    let what = match (size, scale) {
        (Some(size), Some(scale)) => format!("first frame {size}@{scale}"),
        _ => "the app is up".to_string(),
    };
    let detail = format!(
        "{what} after {} (source: {})",
        crate::time::format_duration(Duration::from_millis(ms)),
        ready.source
    );
    ctx.rep
        .check(Check::pass(CheckId::RunReady, detail.clone()));
    detail
}

// ---- screenshots ---------------------------------------------------------------------

/// Captures `<stem>.png` and `<stem>.preview.png` in `dir`, reports them,
/// the `screen` geometry and blank detection (`run.screen_blank`, or INFO
/// `android.screen.secure` for a FLAG_SECURE window).
fn capture(
    ctx: &Ctx,
    adb: &Adb,
    dir: &Path,
    stem: &str,
    app_id: &str,
    expect_content: bool,
) -> Result<Screen> {
    std::fs::create_dir_all(dir).map_err(|e| internal("cannot create the run directory", e))?;
    let cmd = adb
        .cmd(["exec-out", "screencap", "-p"])
        .timeout(Duration::from_secs(60));
    let outcome = ctx.step("adb.screencap", &cmd)?;
    if !outcome.success() || image::png_size(&outcome.stdout).is_none() {
        let mut error = ctx.step_failure("adb.screencap", CheckId::ToolFailed, &outcome);
        if outcome.success() {
            error.detail = format!(
                "screencap returned {} bytes that are not a PNG",
                outcome.stdout.len()
            );
        }
        return Err(error);
    }
    let png_path = dir.join(format!("{stem}.png"));
    let preview_path = dir.join(format!("{stem}.preview.png"));
    std::fs::write(&png_path, &outcome.stdout)
        .map_err(|e| internal("cannot write the screenshot", e))?;
    let stats = image::write_preview(&outcome.stdout, &preview_path)
        .map_err(|e| internal("cannot write the preview", e))?;

    let scale = adb
        .shell_text("wm density", Duration::from_secs(15))
        .and_then(|text| adb::parse_density(&text))
        .unwrap_or(1.0);
    let mut screen = Screen::new(stats.px, scale);
    screen.preview = stats.preview;

    let mut extra = serde_json::Map::new();
    let _ = extra.insert("bytes".into(), json!(outcome.stdout.len()));
    let _ = extra.insert("blank".into(), json!(stats.blank));
    ctx.rep.artifact_with("screenshot", &png_path, extra);
    ctx.rep.artifact("preview", &preview_path);
    ctx.rep.set("screen", screen.to_json());

    if stats.blank {
        let secure = adb
            .shell_text("dumpsys window windows", Duration::from_secs(20))
            .is_some_and(|text| adb::focused_window_is_secure(&text, app_id));
        if secure {
            ctx.rep.check(Check::info(
                CheckId::AndroidScreenSecure,
                format!("{app_id}'s window has FLAG_SECURE, so the screenshot is black"),
            ));
        } else {
            let detail = format!(
                "{:.1}% of pixels are {}",
                stats.dominant_share * 100.0,
                stats.dominant
            );
            let status = if expect_content {
                Status::Fail
            } else {
                Status::Warn
            };
            ctx.rep.check(
                Check::new(CheckId::RunScreenBlank, status, detail)
                    .evidence(Evidence::file(&png_path))
                    .fix(
                        "Compare with a headless render; read the logs.",
                        &[
                            "icm shot --headless --json -q",
                            "icm logs android --level warn --json",
                        ],
                    ),
            );
        }
    }
    Ok(screen)
}

/// `icm shot android`: a screenshot of the device the session runs on.
pub fn shot(ctx: &mut Ctx, args: &ShotArgs) -> Result<()> {
    let (project, host, tools) = setup(ctx)?;
    let ctx: &Ctx = ctx;
    let (adb, mut session) = session_device(ctx, &project, &host, &tools)?;
    let dir = run_dir(ctx, &project);
    let stem = args.name.clone().unwrap_or_else(|| "screen".to_string());
    if stem.contains('/') || stem.is_empty() {
        return Err(IcmError::new(
            CheckId::UsageBadArgs,
            format!("--name `{stem}` must be a plain file name"),
        ));
    }
    let app_id = project.config.config.app.id.clone();
    let screen = capture(ctx, &adb, &dir, &stem, &app_id, false)?;
    let pids = adb.pids(&app_id);
    ctx.rep.set(
        "process",
        json!({"pid": pids.first(), "alive": !pids.is_empty()}),
    );
    if pids.is_empty() {
        ctx.rep.check(
            Check::warn(
                CheckId::RunAppDied,
                format!(
                    "{app_id} is not running on {}; the screenshot shows whatever is on screen instead (the launcher)",
                    adb.serial
                ),
            )
            .fix("Start the app, then take the screenshot again.", &[
                "icm run android --json -q",
            ]),
        );
    }
    if let Some(out) = &args.out {
        if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| internal("cannot create --out's directory", e))?;
        }
        std::fs::copy(dir.join(format!("{stem}.png")), out)
            .map_err(|e| internal("cannot write --out", e))?;
        ctx.rep.artifact("out", out);
    }
    if let Some(session) = session.as_mut() {
        session.screen = Some(Geometry::from(&screen));
        let _ = session::write(&project, session);
    }
    ctx.rep.set("device", json!({"serial": adb.serial}));
    ctx.rep.next(
        "icm input android tap <x> <y> --json -q",
        "act in screen.preview.png pixels",
    );
    Ok(())
}

/// The session's device when it is online, else one chosen without
/// booting.
fn session_device(
    ctx: &Ctx,
    project: &Project,
    host: &HostConfig,
    tools: &Toolset,
) -> Result<(Adb, Option<Session>)> {
    let session = session::read(project);
    let listed = adb::devices(tools)?;
    if let Some(session) = &session
        && listed
            .iter()
            .any(|device| device.serial == session.serial && device.online())
    {
        return Ok((Adb::new(tools, &session.serial)?, Some(session.clone())));
    }
    let request = device::Request {
        target_sdk: project.config.config.android.target_sdk,
        ..device::Request::default()
    };
    let chosen: Chosen = device::choose(
        ctx,
        tools,
        host,
        &ctx.env,
        &request,
        false,
        Path::new("/dev/null"),
    )?;
    Ok((Adb::new(tools, &chosen.serial)?, None))
}

// ---- logs ----------------------------------------------------------------------

/// What [`collect_logs`] found.
#[derive(Clone, Debug, Default)]
struct Collected {
    records: Vec<Record>,
    selected: Vec<(String, Record)>,
    logs: Option<PathBuf>,
}

fn query(adb: &Adb, mark: &str, buffers: &[&str]) -> Option<String> {
    let mut args: Vec<String> = vec!["logcat".into(), "-d".into()];
    for buffer in buffers {
        args.push("-b".into());
        args.push((*buffer).to_string());
    }
    args.extend([
        "-v".into(),
        "threadtime,epoch".into(),
        "-T".into(),
        mark.to_string(),
    ]);
    // Once in a while a dump fails while the next one, a moment later,
    // does not (seen right after a launch); the logs are a failed run's
    // evidence, so try twice.
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(500));
        }
        if let Some(outcome) = adb::quick(adb.cmd(&args), Duration::from_secs(60))
            && outcome.success()
        {
            return Some(outcome.stdout_text());
        }
    }
    None
}

/// Writes `logcat.txt` (raw), `logs.ndjson` and `app.log` (the app's
/// records) into the run directory.
fn collect_logs(
    ctx: &Ctx,
    adb: &Adb,
    dir: &Path,
    app_id: &str,
    mark: &str,
    pids: &BTreeSet<u32>,
) -> Collected {
    let Some(text) = query(adb, mark, &["main", "system", "crash"]) else {
        return Collected::default();
    };
    let raw = dir.join("logcat.txt");
    let _ = std::fs::write(&raw, &text);
    let records = logcat::parse(&text);
    let mut pids = pids.clone();
    // ICM_EVENT start names the app's pid even when pidof missed it.
    for record in &records {
        if record.tag == "ICM_EVENT" {
            let _ = pids.insert(record.pid);
        }
    }
    let selected: Vec<(String, Record)> = logcat::select(&records, app_id, &pids)
        .into_iter()
        .map(|(source, record)| (source.to_string(), record.clone()))
        .collect();
    let (logs, app_log) = write_records(dir, &selected);
    if let Some(path) = &logs {
        ctx.rep.artifact("logs", path);
    }
    if let Some(path) = &app_log {
        ctx.rep.artifact("app_log", path);
    }
    ctx.rep.artifact("logcat", &raw);
    Collected {
        records,
        selected,
        logs,
    }
}

fn write_records(dir: &Path, selected: &[(String, Record)]) -> (Option<PathBuf>, Option<PathBuf>) {
    let ndjson: String = selected
        .iter()
        .map(|(source, record)| format!("{}\n", record.to_json(source)))
        .collect();
    let readable: String = selected
        .iter()
        .map(|(_, record)| format!("{}\n", record.line()))
        .collect();
    let logs = dir.join("logs.ndjson");
    let app_log = dir.join("app.log");
    (
        std::fs::write(&logs, ndjson).ok().map(|()| logs),
        std::fs::write(&app_log, readable).ok().map(|()| app_log),
    )
}

/// What a relaunch of the app's activity means for the run.
#[derive(Clone, Debug)]
struct Recreated {
    /// The likely cause, for a failed run.
    cause: String,
    /// What to do about it.
    fix: String,
    /// The commands that do it.
    commands: Vec<String>,
    /// The relaunch in `events.txt`.
    evidence: Option<Evidence>,
}

/// `run.activity_recreated` (WARN): Android relaunched the app's activity
/// since the launch mark, so the app started over in the new one and lost
/// what it kept in memory. Writes the events buffer since the mark to
/// `events.txt` (design §10.4 step 12) and reports the first relaunch with
/// the configuration changes that caused it ([`describe_recreation`]).
fn recreation(
    ctx: &Ctx,
    adb: &Adb,
    dir: &Path,
    project: &Project,
    mark: &str,
) -> Option<Recreated> {
    let text = events_buffer(adb, mark, &[])?;
    let path = dir.join("events.txt");
    if std::fs::write(&path, &text).is_ok() {
        ctx.rep.artifact("events", &path);
    }
    let config = &project.config.config;
    let found = logcat::relaunches(&logcat::parse(&text), &config.app.id);
    let first = found.first()?;
    let (detail, recreated) = describe_recreation(
        &found,
        mark.parse().unwrap_or(0.0),
        config.android.target_sdk,
    )?;
    let line = text
        .lines()
        .position(|line| line.contains(&first.record.ts) && line.contains(&first.record.tag))
        .map_or(1, |index| index as u32 + 1);
    let evidence = Evidence::line(&path, line, first.record.line());
    let commands: Vec<&str> = recreated.commands.iter().map(String::as_str).collect();
    ctx.rep.check(
        Check::warn(CheckId::RunActivityRecreated, detail)
            .evidence(evidence.clone())
            .fix(recreated.fix.clone(), &commands),
    );
    Some(Recreated {
        evidence: Some(evidence),
        ..recreated
    })
}

/// The `run.activity_recreated` detail and what it means, for the
/// relaunches found since `mark` (epoch seconds), naming the changes as
/// `android:configChanges` does and comparing them with the manifest this
/// icm generates for `target_sdk`.
fn describe_recreation(
    found: &[logcat::Relaunch],
    mark: f64,
    target_sdk: u32,
) -> Option<(String, Recreated)> {
    let first = found.first()?;
    let after = crate::time::format_duration(Duration::from_secs_f64(
        (first.record.seconds() - mark).max(0.0),
    ));
    let generated = super::manifest::config_changes(target_sdk);
    let generated: Vec<&str> = generated.split('|').collect();
    let names = first
        .mask
        .map(super::manifest::config_names)
        .unwrap_or_default();
    let unlisted: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|name| !generated.contains(name))
        .collect();

    let (why, fix) = match unlisted.as_slice() {
        [] if names.is_empty() => (
            "the event log does not name the change".to_string(),
            "Rerun; if the relaunch repeats, report it with events.txt.",
        ),
        [] => (
            format!(
                "the installed APK's android:configChanges lacks {}, which the manifest this icm generates lists, so an earlier icm built it (or `--no-build` reused it)",
                names.join("|")
            ),
            "Rebuild and reinstall the APK: run without --no-build.",
        ),
        ["assetsPaths"] => (
            format!(
                "a runtime resource overlay changed (assetsPaths; SystemUI applies its theme palette during an emulator's first boots), and android:configChanges can name assetsPaths only from [android] target_sdk = 36 (this app has {target_sdk})"
            ),
            "Raise [android] target_sdk to 36 in icm.toml, or rerun once the emulator has settled (its overlays change only on its first boots).",
        ),
        unlisted => (
            format!(
                "a configuration change android:configChanges does not list for target_sdk {target_sdk} ({})",
                unlisted.join("|")
            ),
            "Rerun; if the relaunch repeats, report it with events.txt (icm's manifest should list the change).",
        ),
    };

    let mut detail = format!(
        "Android relaunched {} {after} after launch ({})",
        first.component,
        if names.is_empty() {
            "no change named".to_string()
        } else {
            names.join("|")
        }
    );
    if found.len() > 1 {
        detail.push_str(&format!(", {} relaunches", found.len()));
    }
    detail.push_str(&format!(
        ": {why}. The app ended with its activity and started over in the new one, losing what it kept in memory"
    ));
    let recreated = Recreated {
        cause: format!(
            "Android relaunched the activity {after} after launch, which ends the app and starts it over in the new activity: {why} (run.activity_recreated)"
        ),
        fix: fix.to_string(),
        commands: vec!["icm run android --json -q".to_string()],
        evidence: None,
    };
    Some((detail, recreated))
}

/// Adds the logs, a panic's location and the failure signatures to a run
/// failure.
fn attach_evidence(error: &mut IcmError, collected: &Collected, project: &Project) {
    let pids: BTreeSet<u32> = collected
        .selected
        .iter()
        .filter(|(source, _)| source == "app")
        .map(|(_, record)| record.pid)
        .collect();
    let app_records: Vec<Record> = collected
        .selected
        .iter()
        .map(|(_, record)| record.clone())
        .collect();

    let panic = logcat::panic_of(&collected.records, &pids);
    let app_id = &project.config.config.app.id;
    if error.id == CheckId::RunNotReady.id()
        && let Some(anr) = app_records
            .iter()
            .find(|record| record.msg.starts_with(&format!("ANR in {app_id}")))
    {
        let mut anr_error = IcmError::new(
            CheckId::RunAnr,
            format!("{}: {}", error.detail, anr.msg.trim()),
        );
        anr_error.evidence = std::mem::take(&mut error.evidence);
        *error = anr_error;
    }
    if error.id == CheckId::RunAppDied.id()
        && let Some((message, location)) = &panic
    {
        let mut panicked = IcmError::new(
            CheckId::RunAppPanicked,
            match location {
                Some(location) if !message.contains(location.as_str()) => {
                    format!("panicked at {location}: {message}")
                }
                _ => message.clone(),
            },
        );
        panicked.evidence = std::mem::take(&mut error.evidence);
        *error = panicked;
    }

    if let Some(logs) = &collected.logs {
        // The panic line when there is one (what follows it is the abort),
        // else the last error.
        let panic_line = app_records.iter().find(|record| {
            record.msg.contains("panicked at")
                || logcat::event(record).is_some_and(|event| logcat::kind(&event) == "panic")
        });
        let excerpt = panic_line
            .or_else(|| {
                app_records
                    .iter()
                    .rev()
                    .find(|record| record.priority == 'E' || record.priority == 'F')
            })
            .or(app_records.last())
            .map(Record::line)
            .unwrap_or_default();
        error
            .evidence
            .push(Evidence::file(logs).with_excerpt(excerpt));
    }
    let lib = project.lib_name().unwrap_or_default();
    for cause in logcat::likely_causes(&app_records, &lib) {
        // A panic aborts the process: the SIGABRT is its consequence.
        if panic.is_some() && cause.contains("Fatal signal 6") {
            continue;
        }
        error.likely_causes.push(cause);
    }
    if error.fix.commands.is_empty() {
        error
            .fix
            .commands
            .push("icm logs android --level warn --json".to_string());
    }
}

fn source_matches(filter: LogSource, source: &str) -> bool {
    match filter {
        LogSource::All => true,
        LogSource::App => source == "app",
        LogSource::System => source == "system",
        LogSource::Crash => source == "crash",
    }
}

/// `icm logs android`: re-queries logcat from the launch mark (or
/// `--since <dur>` ago) on the session's device.
pub fn logs(ctx: &mut Ctx, args: &LogsArgs) -> Result<()> {
    let (project, host, tools) = setup(ctx)?;
    let ctx: &Ctx = ctx;
    let (adb, session) = session_device(ctx, &project, &host, &tools)?;
    let app_id = project.config.config.app.id.clone();

    let mark = if args.since == "launch" {
        session
            .as_ref()
            .filter(|session| session.serial == adb.serial)
            .and_then(|session| session.log_mark.clone())
            .ok_or_else(|| {
                IcmError::new(
                    CheckId::RunNoSession,
                    format!(
                        "no `icm run android` session on {}, so there is no launch mark",
                        adb.serial
                    ),
                )
                .fix_commands([
                    "icm run android --json -q",
                    "icm logs android --since 10m --json",
                ])
            })?
    } else {
        let ago = crate::time::parse_duration(&args.since).map_err(|error| {
            IcmError::new(
                CheckId::UsageBadArgs,
                format!(
                    "--since {}: {error} (use `launch` or a duration like 10m)",
                    args.since
                ),
            )
        })?;
        let now: f64 = adb
            .epoch()
            .and_then(|epoch| epoch.parse().ok())
            .ok_or_else(|| {
                IcmError::new(
                    CheckId::AndroidDeviceNone,
                    format!("{} does not answer `date`", adb.serial),
                )
            })?;
        format!("{:.3}", (now - ago.as_secs_f64()).max(0.0))
    };
    ctx.rep.set("device", json!({"serial": adb.serial}));
    ctx.rep.set("since", json!(mark));

    let mut pids: BTreeSet<u32> = adb.pids(&app_id).into_iter().collect();
    if let Some(pid) = session.as_ref().and_then(|s| s.pid) {
        let _ = pids.insert(pid);
    }

    let dir = run_dir(ctx, &project);
    let text = query(&adb, &mark, &["main", "system", "crash"]).ok_or_else(|| {
        IcmError::new(
            CheckId::ToolFailed,
            format!("adb logcat failed on {}", adb.serial),
        )
    })?;
    let raw = dir.join("logcat.txt");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&raw, &text);
    ctx.rep.artifact("logcat", &raw);

    if args.raw {
        ctx.rep.content(&text);
        ctx.rep.summary(format!(
            "{} lines of logcat since {mark}",
            text.lines().count()
        ));
        return Ok(());
    }

    let records = logcat::parse(&text);
    for record in &records {
        if record.tag == "ICM_EVENT" {
            let _ = pids.insert(record.pid);
        }
    }
    let grep = crate::grep::Grep::new(args.grep.as_deref());
    let wanted = |source: &str, record: &Record| {
        source_matches(args.source.unwrap_or(LogSource::All), source)
            && logcat::at_least(record, args.level)
            && grep
                .as_ref()
                .is_none_or(|grep| grep.matches(&[&record.tag, &record.msg]))
    };
    let app_records = logcat::select(&records, &app_id, &pids);
    let considered = app_records.len();
    let mut selected: Vec<(String, Record)> = app_records
        .into_iter()
        .filter(|(source, record)| wanted(source, record))
        .map(|(source, record)| (source.to_string(), record.clone()))
        .collect();
    let total = selected.len();
    crate::grep::warn_unmatched(ctx, args.grep.as_deref(), total, considered);
    if selected.len() > args.tail {
        selected.drain(..selected.len() - args.tail);
    }
    let (logs, app_log) = write_records(&dir, &selected);
    if let Some(path) = logs {
        ctx.rep.artifact("logs", &path);
    }
    if let Some(path) = app_log {
        ctx.rep.artifact("app_log", &path);
    }
    ctx.rep.set(
        "records",
        Value::Array(
            selected
                .iter()
                .map(|(source, record)| record.to_json(source))
                .collect(),
        ),
    );
    let text: String = selected
        .iter()
        .map(|(_, record)| format!("{}\n", record.line()))
        .collect();
    if !text.is_empty() {
        ctx.rep.content(text);
    }
    ctx.rep.summary(format!(
        "{} of {total} record(s) from {} since {mark}",
        selected.len(),
        adb.serial
    ));

    if args.follow {
        let _ = follow(ctx, &adb, &app_id, &mark, pids, false, &wanted)?;
    }
    Ok(())
}

/// How [`follow`] ended.
enum Followed {
    /// Ctrl-C or `--timeout`.
    Ended,
    /// The app's process is gone (`until_exit`); `stopped` when it was
    /// `am force-stop` (`icm stop android`, or someone at the device).
    Gone { stopped: bool },
}

/// Streams the app's new records until the app exits (`until_exit`),
/// Ctrl-C or `--timeout`. In JSON mode each record is a `log` event.
fn follow(
    ctx: &Ctx,
    adb: &Adb,
    app_id: &str,
    mark: &str,
    mut pids: BTreeSet<u32>,
    until_exit: bool,
    wanted: &dyn Fn(&str, &Record) -> bool,
) -> Result<Followed> {
    type Key = (String, u32, u32, String);
    let key = |record: &Record| -> Key {
        (
            record.ts.clone(),
            record.pid,
            record.tid,
            record.msg.clone(),
        )
    };
    let mut since = mark.to_string();
    let mut seen: BTreeSet<Key> = BTreeSet::new();
    // Skip what was already shown.
    if let Some(text) = query(adb, &since, &["main", "system", "crash"]) {
        for record in logcat::parse(&text) {
            since = record.ts.clone();
            let _ = seen.insert(key(&record));
        }
    }
    let deadline = ctx.deadline();
    let force_stop = format!("Force stopping {app_id} ");
    let mut stopped = false;
    loop {
        if crate::signals::pending().is_some() || deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(Followed::Ended);
        }
        std::thread::sleep(Duration::from_millis(1000));
        let alive = adb.pids(app_id);
        pids.extend(alive.iter().copied());
        if let Some(text) = query(adb, &since, &["main", "system", "crash"]) {
            let records = logcat::parse(&text);
            // `-T <since>` repeats the records at `since`; older keys are
            // no longer needed.
            let floor: f64 = since.parse().unwrap_or(0.0);
            seen.retain(|(ts, ..)| ts.parse::<f64>().unwrap_or(0.0) >= floor);
            // ActivityManager logs every `am force-stop`.
            stopped |= records
                .iter()
                .any(|record| record.msg.starts_with(&force_stop));
            for (source, record) in logcat::select(&records, app_id, &pids) {
                if !seen.insert(key(record)) || !wanted(source, record) {
                    continue;
                }
                let mut event = record.to_json(source);
                event["type"] = json!("log");
                if ctx.rep.mode().json {
                    ctx.rep.emit(event);
                } else {
                    ctx.rep.content(record.line());
                }
            }
            if let Some(last) = records.last() {
                since = last.ts.clone();
            }
        }
        if until_exit && alive.is_empty() {
            ctx.rep.progress(format!("{app_id} exited"));
            return Ok(Followed::Gone { stopped });
        }
    }
}

// ---- input ---------------------------------------------------------------------

/// The geometry input coordinates refer to: the last screenshot's (what
/// the agent looked at), else the device's current display.
fn geometry(adb: &Adb, session: Option<&Session>) -> Result<Screen> {
    if let Some(geometry) = session.and_then(|session| session.screen) {
        return Ok(geometry.screen());
    }
    let displays = adb
        .shell_text("dumpsys window displays", Duration::from_secs(20))
        .unwrap_or_default();
    let wm_size = adb
        .shell_text("wm size", Duration::from_secs(15))
        .unwrap_or_default();
    let px = adb::parse_display_size(&displays, &wm_size).ok_or_else(|| {
        IcmError::new(
            CheckId::ToolFailed,
            format!("cannot read {}'s display size", adb.serial),
        )
    })?;
    let scale = adb
        .shell_text("wm density", Duration::from_secs(15))
        .and_then(|text| adb::parse_density(&text))
        .unwrap_or(1.0);
    let mut screen = Screen::new(px, scale);
    screen.preview = preview_size(px);
    Ok(screen)
}

fn keycode(key: Key) -> &'static str {
    match key {
        Key::Back => "KEYCODE_BACK",
        Key::Home => "KEYCODE_HOME",
        Key::Enter => "KEYCODE_ENTER",
        Key::Tab => "KEYCODE_TAB",
        Key::Escape => "KEYCODE_ESCAPE",
    }
}

fn space_name(space: Space) -> &'static str {
    match space {
        Space::Preview => "preview",
        Space::Px => "px",
        Space::Pt => "pt",
    }
}

/// `icm input android …`: taps and swipes in one coordinate space
/// (preview pixels by default, Appendix C item 25), text, keys and the
/// device-state helpers.
pub fn input(ctx: &mut Ctx, args: &InputArgs) -> Result<()> {
    let (project, host, tools) = setup(ctx)?;
    let ctx: &Ctx = ctx;
    let (adb, session) = session_device(ctx, &project, &host, &tools)?;
    let app_id = project.config.config.app.id.clone();
    ctx.rep.set("device", json!({"serial": adb.serial}));

    // Touches and keys go to whatever is on screen: without the app they
    // would drive the launcher.
    if matches!(
        args.action,
        InputAction::Tap { .. }
            | InputAction::Swipe { .. }
            | InputAction::Text { .. }
            | InputAction::Key { .. }
    ) && adb.pids(&app_id).is_empty()
    {
        let (id, detail) = match &session {
            Some(_) => (
                CheckId::RunAppDied,
                format!(
                    "{app_id} is not running on {} (it exited or crashed); nothing was sent",
                    adb.serial
                ),
            ),
            None => (
                CheckId::RunNoSession,
                format!(
                    "{app_id} is not running on {}; nothing was sent",
                    adb.serial
                ),
            ),
        };
        return Err(IcmError::new(id, detail).fix(
            "Start the app (and read why it stopped), then send the input again.",
            &[
                "icm logs android --level warn --json -q",
                "icm run android --json -q",
            ],
        ));
    }

    let point = |screen: &Screen, x: f64, y: f64| -> Result<(i64, i64)> {
        if !screen.contains(x, y, args.space) {
            let (w, h) = match args.space {
                Space::Preview => (f64::from(screen.preview.0), f64::from(screen.preview.1)),
                Space::Px => (f64::from(screen.px.0), f64::from(screen.px.1)),
                Space::Pt => screen.pt(),
            };
            return Err(IcmError::new(
                CheckId::UsageBadArgs,
                format!(
                    "({x}, {y}) is outside the screen, which is {w}x{h} in {} space",
                    space_name(args.space)
                ),
            ));
        }
        let (px, py) = screen.to_px(x, y, args.space);
        Ok((px.round() as i64, py.round() as i64))
    };

    let (line, what): (String, Value) = match &args.action {
        InputAction::Tap { x, y } => {
            let screen = geometry(&adb, session.as_ref())?;
            let (px, py) = point(&screen, *x, *y)?;
            ctx.rep.set("screen", screen.to_json());
            (
                format!("input tap {px} {py}"),
                json!({"action": "tap", "space": space_name(args.space), "at": [x, y], "px": [px, py]}),
            )
        }
        InputAction::Swipe { x1, y1, x2, y2, ms } => {
            let screen = geometry(&adb, session.as_ref())?;
            let (ax, ay) = point(&screen, *x1, *y1)?;
            let (bx, by) = point(&screen, *x2, *y2)?;
            let ms = ms.unwrap_or(300);
            ctx.rep.set("screen", screen.to_json());
            (
                format!("input swipe {ax} {ay} {bx} {by} {ms}"),
                json!({"action": "swipe", "space": space_name(args.space), "from": [x1, y1], "to": [x2, y2], "px": [[ax, ay], [bx, by]], "ms": ms}),
            )
        }
        InputAction::Text { text } => {
            if !text.is_ascii() || text.chars().any(char::is_control) {
                return Err(IcmError::new(
                    CheckId::UsageBadArgs,
                    "adb can type printable ASCII only",
                ));
            }
            (
                format!("input text {}", adb::input_text_arg(text)),
                json!({"action": "text", "text": text}),
            )
        }
        InputAction::Key { key } => (
            format!("input keyevent {}", keycode(*key)),
            json!({"action": "key", "key": keycode(*key)}),
        ),
        InputAction::Appearance { mode } => {
            let night = if *mode == Theme::Dark { "yes" } else { "no" };
            (
                format!("cmd uimode night {night}"),
                json!({"action": "appearance", "night": night}),
            )
        }
        InputAction::Rotate { orientation } => {
            let rotation = if *orientation == Rotation::Landscape {
                1
            } else {
                0
            };
            (
                format!(
                    "settings put system accelerometer_rotation 0 && settings put system user_rotation {rotation}"
                ),
                json!({"action": "rotate", "user_rotation": rotation}),
            )
        }
        InputAction::FontScale { scale } => {
            if !(0.5..=3.0).contains(scale) {
                return Err(IcmError::new(
                    CheckId::UsageBadArgs,
                    format!("font scale {scale} is outside 0.5 to 3.0"),
                ));
            }
            (
                format!("settings put system font_scale {scale}"),
                json!({"action": "font-scale", "scale": scale}),
            )
        }
        InputAction::Background => (
            "input keyevent KEYCODE_HOME".to_string(),
            json!({"action": "background"}),
        ),
        InputAction::Foreground => (
            format!(
                "am start -n {}",
                adb::quote(&format!("{app_id}/{ACTIVITY}"))
            ),
            json!({"action": "foreground"}),
        ),
    };

    let outcome = ctx.step(
        "adb.input",
        &adb.shell(&line).timeout(Duration::from_secs(30)),
    )?;
    let text = format!("{}{}", outcome.stdout_text(), outcome.stderr_text());
    if !outcome.success() || text.contains("Exception") || text.contains("Error:") {
        let mut error = ctx.step_failure("adb.input", CheckId::ToolFailed, &outcome);
        if outcome.success() {
            error.detail = format!("`{line}` failed: {}", text.trim());
        }
        return Err(error);
    }
    ctx.rep.set("input", what);
    ctx.rep.summary(format!("{line} on {}", adb.serial));
    ctx.rep.next("icm shot android --json -q", "see the result");
    Ok(())
}

// ---- stop ----------------------------------------------------------------------

/// `icm stop android [--shutdown]`: force-stops the app; with
/// `--shutdown`, also the emulator icm booted for this project (never an
/// AVD outside icm's `icm-` names, never one another project booted).
pub fn stop(ctx: &mut Ctx, args: &StopArgs) -> Result<()> {
    let (project, host, tools) = setup(ctx)?;
    let ctx: &Ctx = ctx;
    let stopped = stop_session(ctx, &project, &host, &tools, args.shutdown)?;
    ctx.rep.summary(if stopped.is_empty() {
        "nothing to stop on Android".to_string()
    } else {
        format!("stopped {} on Android", stopped.len())
    });
    ctx.rep.set("stopped", Value::Array(stopped));
    Ok(())
}

/// Stops this project's app on its device (`am force-stop`) and, with
/// `shutdown`, the icm-managed emulators it or the project's default AVD
/// runs on; removes the session. Returns what it stopped (for `icm stop
/// --all` too).
pub fn stop_session(
    ctx: &Ctx,
    project: &Project,
    host: &HostConfig,
    tools: &Toolset,
    shutdown: bool,
) -> Result<Vec<Value>> {
    let app_id = project.config.config.app.id.clone();
    let session = session::read(project);
    let listed = adb::devices(tools)?;
    let online = |serial: &str| {
        listed
            .iter()
            .any(|device| device.serial == serial && device.online())
    };
    let mut stopped: Vec<Value> = Vec::new();

    if let Some(session) = &session
        && online(&session.serial)
    {
        let adb = Adb::new(tools, &session.serial)?;
        let outcome = ctx.step(
            "adb.force_stop",
            &adb.shell(&format!("am force-stop {}", adb::quote(&app_id)))
                .timeout(Duration::from_secs(30)),
        )?;
        if !outcome.success() {
            return Err(ctx.step_failure("adb.force_stop", CheckId::ToolFailed, &outcome));
        }
        stopped.push(json!({"platform": "android", "app": app_id, "serial": session.serial}));
    }

    // `--shutdown` stops the emulator icm booted for this session, and
    // otherwise only icm's own AVDs: never an `icm-test-` one a test run
    // made and owns (crate::managed::is_managed), nor anyone else's.
    if shutdown {
        let mut targets: Vec<(String, Option<u32>)> = Vec::new();
        if let Some(session) = &session
            && online(&session.serial)
            && (session.booted_by_icm
                || session
                    .avd
                    .as_deref()
                    .is_some_and(crate::managed::is_managed))
        {
            targets.push((session.serial.clone(), session.emulator_pid));
        }
        let default = device::default_avd(host, project.config.config.android.target_sdk);
        if crate::managed::is_managed(&default) {
            for (serial, name) in device::running_emulators(tools, &listed) {
                if name.as_deref() == Some(default.as_str())
                    && !targets.iter().any(|(s, _)| *s == serial)
                {
                    targets.push((serial, None));
                }
            }
        }
        // Every emulator icm booted for this project, even after a plain
        // `icm stop android` removed the session that named it.
        for booted in session::booted(project) {
            if online(&booted.serial) && !targets.iter().any(|(s, _)| *s == booted.serial) {
                targets.push((booted.serial.clone(), booted.emulator_pid));
            }
        }
        if let Some(session) = &session
            && online(&session.serial)
            && session.kind == "emulator"
            && !targets.iter().any(|(serial, _)| *serial == session.serial)
        {
            ctx.rep.check(Check::info(
                CheckId::RunNoSession,
                format!(
                    "{} ({}) left running: icm did not boot it and shuts down only its own AVDs (icm-*, never icm-test-*)",
                    session.serial,
                    session.avd.as_deref().unwrap_or("unknown AVD")
                ),
            ));
        }
        for (serial, pid) in targets {
            ctx.rep.progress(format!("shutting down {serial}"));
            avd::shutdown(tools, &serial, pid)?;
            session::remove_booted(project, &serial);
            stopped.push(json!({"platform": "android", "emulator": serial}));
        }
    }

    session::remove(project);
    Ok(stopped)
}

/// Whether this project's app runs on its session's device, for `icm ps`
/// (an Android record has no host process to probe).
pub fn app_state(ctx: &mut Ctx, project: &Project) -> crate::session::AppState {
    use crate::session::AppState;
    let Some(session) = session::read(project) else {
        return AppState::Unknown;
    };
    let Ok(host) = ctx.host().cloned() else {
        return AppState::Unknown;
    };
    let Ok(tools) = Toolset::discover(&host, &ctx.env) else {
        return AppState::Unknown;
    };
    let Ok(listed) = adb::devices(&tools) else {
        return AppState::Unknown;
    };
    if !listed
        .iter()
        .any(|device| device.serial == session.serial && device.online())
    {
        return AppState::Gone("the device is offline".to_string());
    }
    let Ok(adb) = Adb::new(&tools, &session.serial) else {
        return AppState::Unknown;
    };
    if adb.pids(&session.app_id).is_empty() {
        AppState::Gone("the app is not running".to_string())
    } else {
        AppState::Running
    }
}

// ---- devices -------------------------------------------------------------------

/// `icm devices android`: online devices and the AVDs.
pub fn devices(ctx: &mut Ctx) -> Result<()> {
    let (lines, listing) = device_listing(ctx)?;
    for key in ["devices", "avds", "default_avd"] {
        ctx.rep.set(key, listing[key].clone());
    }
    ctx.rep.summary(format!(
        "android: {} device(s) online, {} AVD(s)",
        listing["devices"].as_array().map_or(0, |devices| devices
            .iter()
            .filter(|d| d["state"] == "device")
            .count()),
        listing["avds"].as_array().map_or(0, Vec::len)
    ));
    ctx.rep.content(lines);
    Ok(())
}

/// The devices adb lists and the AVDs, as text lines and as
/// `{devices, avds, default_avd}` (`icm devices [android]`).
pub fn device_listing(ctx: &mut Ctx) -> Result<(String, Value)> {
    let host = ctx.host()?.clone();
    let tools = Toolset::discover(&host, &ctx.env)?;
    let target_sdk = ctx
        .try_project()
        .map_or(36, |project| project.config.config.android.target_sdk);
    let ctx: &Ctx = ctx;
    let listed = adb::devices(&tools)?;
    let emulators = device::running_emulators(&tools, &listed);
    let mut lines = String::new();

    let devices: Vec<Value> = listed
        .iter()
        .map(|device| {
            let avd = emulators
                .iter()
                .find(|(serial, _)| *serial == device.serial)
                .and_then(|(_, avd)| avd.clone());
            let abi = if device.online() {
                Adb::new(&tools, &device.serial)
                    .ok()
                    .and_then(|adb| adb.getprop("ro.product.cpu.abi"))
            } else {
                None
            };
            lines.push_str(&format!(
                "{:16} {:12} {:12} {}\n",
                device.serial,
                device.state,
                abi.clone().unwrap_or_default(),
                avd.clone().unwrap_or_default()
            ));
            json!({
                "serial": device.serial,
                "state": device.state,
                "kind": if device.is_emulator() { "emulator" } else { "device" },
                "avd": avd,
                "abi": abi,
                "model": device.props.get("model"),
            })
        })
        .collect();

    let default = device::default_avd(&host, target_sdk);
    let avds: Vec<Value> = avd::list(&ctx.env)
        .into_iter()
        .map(|name| {
            let running = emulators
                .iter()
                .find(|(_, avd)| avd.as_deref() == Some(name.as_str()))
                .map(|(serial, _)| serial.clone());
            lines.push_str(&format!(
                "avd {name}{}{}\n",
                if name == default { " (icm's)" } else { "" },
                running
                    .as_ref()
                    .map(|serial| format!(" running as {serial}"))
                    .unwrap_or_default()
            ));
            json!({
                "name": name,
                "abi": avd::abi(&ctx.env, &name).map(Abi::as_str),
                "managed": avd::is_managed(&name),
                "default": name == default,
                "running": running,
            })
        })
        .collect();

    Ok((
        lines,
        json!({"devices": devices, "avds": avds, "default_avd": default}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relaunch(at: &str, mask: Option<u32>) -> logcat::Relaunch {
        logcat::Relaunch {
            record: logcat::parse_line(&format!(
                "{at}   660   683 I wm_relaunch_resume_activity: [0,1,8,com.example.app/android.app.NativeActivity,{}]",
                mask.map(|m| format!("{m:x}")).unwrap_or_default()
            ))
            .unwrap(),
            component: "com.example.app/android.app.NativeActivity".to_string(),
            mask,
        }
    }

    #[test]
    fn recreations_name_their_cause() {
        assert!(describe_recreation(&[], 0.0, 36).is_none());

        // The fresh-emulator case: SystemUI's overlays, with a manifest
        // linked below API 36, which cannot list assetsPaths.
        let overlays = [relaunch("100.400", Some(0x8000_0000))];
        let (detail, recreated) = describe_recreation(&overlays, 100.0, 35).unwrap();
        assert!(
            detail.starts_with(
                "Android relaunched com.example.app/android.app.NativeActivity 400ms after launch (assetsPaths)"
            ),
            "{detail}"
        );
        assert!(detail.contains("started over"), "{detail}");
        assert!(recreated.cause.contains("runtime resource overlay"));
        assert!(recreated.cause.ends_with("(run.activity_recreated)"));
        assert!(recreated.fix.contains("target_sdk to 36"));

        // From API 36 icm's manifest lists it: the installed APK is older.
        let (_, stale) = describe_recreation(&overlays, 100.0, 36).unwrap();
        assert!(
            stale.cause.contains("an earlier icm built it"),
            "{}",
            stale.cause
        );
        assert!(stale.fix.contains("without --no-build"));

        // A change no configChanges name covers, twice.
        let unknown = [
            relaunch("101.5", Some(0x0100_0000)),
            relaunch("102", Some(0x0100_0000)),
        ];
        let (detail, recreated) = describe_recreation(&unknown, 100.0, 36).unwrap();
        assert!(detail.contains("(0x1000000), 2 relaunches"), "{detail}");
        assert!(
            recreated
                .cause
                .contains("does not list for target_sdk 36 (0x1000000)")
        );

        // Older releases log no mask.
        let (detail, _) = describe_recreation(&[relaunch("100", None)], 100.0, 36).unwrap();
        assert!(detail.contains("(no change named)"), "{detail}");
    }
}
