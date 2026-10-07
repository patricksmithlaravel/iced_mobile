//! `icm stop [<platform>|--all] [--shutdown]` and `icm ps` (design §3
//! "Sessions", §6).
//!
//! Both work from the session files `icm run` writes under
//! `target/icm/sessions/`. The dev platforms stop their own sessions,
//! since they know their devices, log collectors and record formats:
//! `icm stop <platform>` goes straight to the platform (`commands/mod.rs`),
//! and `icm stop --all` here calls each of them in turn: desktop
//! ([`crate::platform::desktop::stop_session`]), ios-sim
//! ([`crate::platform::ios_sim::stop_session`]), android
//! ([`crate::android::stop_session`]) and web ([`crate::web::stop`], which
//! asks the session over its control channel first). Any other record
//! ([`crate::session`]) is ended by what it says: its stop commands run,
//! the processes icm started get SIGTERM (then SIGKILL), and the file is
//! removed. `--shutdown` also shuts down the icm-managed simulator or
//! emulator (`icm-` names only, never `icm-test-` ones and never a device
//! icm did not create). Stopping what is not running is not an error.

use crate::catalogue::CheckId;
use crate::cli::{Platform, StopArgs};
use crate::context::{Ctx, Project};
use crate::error::{Check, Evidence, IcmError, Result, Status};
use crate::managed;
use crate::process::Cmd;
use crate::session::{self, AppState, Session};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

/// How long a process gets between SIGTERM and SIGKILL.
const GRACE: Duration = Duration::from_secs(3);

/// Runs `icm stop`.
pub fn stop(ctx: &mut Ctx, args: &StopArgs) -> Result<()> {
    if args.platform.is_none() && !args.all {
        return Err(IcmError::new(
            CheckId::UsageBadArgs,
            "name a platform (`icm stop web`) or pass --all",
        )
        .fix("Stop one platform or every session.", &["icm stop --all"]));
    }
    let project = ctx.project()?.clone();
    let dir = project.sessions_dir();

    let platforms: Vec<Platform> = match args.platform {
        Some(platform) => vec![platform],
        None => Platform::ALL.to_vec(),
    };

    if ctx.dry_run() {
        plan(ctx, &dir, &platforms, args.shutdown);
        return Ok(());
    }
    let host = ctx.host()?.clone();

    let mut stopped: Vec<Value> = Vec::new();
    let mut shut_down: Vec<String> = Vec::new();
    for platform in &platforms {
        match stop_platform(ctx, &project, &host, *platform, args.shutdown) {
            Ok(entries) => {
                for entry in entries {
                    // Android reports the emulators it shut down alongside
                    // the app it stopped.
                    match entry.get("emulator").and_then(Value::as_str) {
                        Some(serial) => shut_down.push(serial.to_string()),
                        None => stopped.push(entry),
                    }
                }
            }
            // Cleanup goes on: one platform's failure is a WARN.
            Err(error) => ctx.rep.check(Check::from_error(error, Status::Warn)),
        }
    }

    // Records no platform stops itself (ios-device until phase 2, a
    // platform this icm does not know): by what the record says.
    for (path, session) in session::list(&dir) {
        let platform = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if OWN_STOP.contains(&platform.as_str()) {
            continue;
        }
        if let Some(wanted) = args.platform
            && wanted.as_str() != platform
        {
            continue;
        }
        match session {
            Ok(session) => stopped.push(stop_one(ctx, &path, &session, args.shutdown)),
            Err(message) => {
                ctx.rep.check(
                    Check::warn(CheckId::RunNoSession, message).evidence(Evidence::file(&path)),
                );
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    if args.shutdown {
        for platform in &platforms {
            shut_down.extend(shutdown_managed(ctx, &project, &host, *platform));
        }
    }

    // A record whose process had already exited stopped nothing.
    let count = stopped
        .iter()
        .filter(|entry| entry["stopped"] != "was not running" && entry["how"] != "already exited")
        .count();
    ctx.rep.set("stopped", Value::Array(stopped));
    ctx.rep.set("shutdown", json!(shut_down));
    let what = match args.platform {
        Some(platform) => platform.as_str().to_string(),
        None => "every platform".to_string(),
    };
    ctx.rep.summary(match (count, shut_down.len()) {
        (0, 0) => format!("nothing was running for {what}"),
        (n, 0) => format!("stopped {n} session(s)"),
        (n, d) => format!("stopped {n} session(s) and shut down {d} managed device(s)"),
    });
    Ok(())
}

/// The platforms whose sessions their own module stops.
const OWN_STOP: [&str; 4] = ["desktop", "ios-sim", "android", "web"];

/// `--dry-run`: what each session record would get, read from the
/// records alone (no device, browser or process is touched).
fn plan(ctx: &Ctx, dir: &Path, platforms: &[Platform], shutdown: bool) {
    let mut plan = crate::plan::Plan::new();
    let records = session::list(dir);
    for platform in platforms {
        let path = dir.join(format!("{}.json", platform.as_str()));
        let found = records.iter().any(|(record, _)| *record == path);
        let what = match platform {
            Platform::Desktop => "SIGTERM the app's process group, SIGKILL after a grace period",
            Platform::IosSim => "xcrun simctl terminate the app and stop the log collector",
            Platform::Android => "am force-stop the app on the session's device",
            Platform::Web => "ask the session host to stop, then signal it and headless Chrome",
            Platform::IosDevice => "end the processes the record names",
        };
        plan.push(crate::plan::Step::internal(
            &format!("{}.stop", platform.as_str()),
            &if found {
                format!("{what} ({})", crate::paths::display(&path))
            } else {
                format!(
                    "nothing: no {} session ({} does not exist)",
                    platform.as_str(),
                    crate::paths::display(&path)
                )
            },
        ));
        if shutdown && matches!(platform, Platform::IosSim | Platform::Android) {
            plan.push(crate::plan::Step::internal(
                &format!("{}.shutdown", platform.as_str()),
                "shut down the icm-managed simulator or emulator (icm-* names only, never icm-test-*, never one booted for another project)",
            ));
        }
    }
    plan.report(ctx);
    ctx.rep
        .summary("the plan of icm stop (--dry-run: nothing was stopped)");
}

/// Stops one dev platform's session through its own module; a platform
/// without a session costs nothing (no tool is looked up).
fn stop_platform(
    ctx: &mut Ctx,
    project: &Project,
    host: &crate::host::HostConfig,
    platform: Platform,
    shutdown: bool,
) -> Result<Vec<Value>> {
    let dir = project.sessions_dir();
    Ok(match platform {
        Platform::Desktop => crate::platform::desktop::stop_session(ctx, project)?
            .into_iter()
            .collect(),
        Platform::IosSim => {
            if !crate::platform::ios_sim::session::path(&dir).is_file() {
                return Ok(Vec::new());
            }
            crate::platform::ios_sim::stop_session(ctx, shutdown)?
                .into_iter()
                .collect()
        }
        Platform::Android => {
            // With --shutdown the managed emulator may run without a session.
            if !crate::android::session::path(project).is_file() && !shutdown {
                return Ok(Vec::new());
            }
            let tools = crate::android::Toolset::discover(host, &ctx.env)?;
            crate::android::stop_session(ctx, project, host, &tools, shutdown)?
        }
        Platform::Web => crate::web::stop(project).into_iter().collect(),
        Platform::IosDevice => Vec::new(),
    })
}

/// Stops one session and removes its file.
fn stop_one(ctx: &Ctx, path: &Path, session: &Session, shutdown: bool) -> Value {
    let written = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let platform = session.platform.clone();

    let mut commands = Vec::new();
    for argv in &session.stop {
        if let Some(cmd) = command(argv, Duration::from_secs(30)) {
            run_quietly(ctx, &format!("stop.{platform}"), &cmd);
            commands.push(cmd.display());
        }
    }

    let mut ended = Vec::new();
    let mut stale = Vec::new();
    for pid in session.all_pids() {
        if session::is_ours(pid, written) {
            if session::terminate(pid, GRACE) {
                ended.push(pid);
            } else {
                ctx.rep.check(Check::warn(
                    CheckId::ToolFailed,
                    format!("{platform}: process {pid} did not stop"),
                ));
            }
        } else {
            stale.push(pid);
        }
    }

    let mut shut = Vec::new();
    if shutdown && let Some(device) = &session.device {
        if device.managed && managed::is_managed(&device.name) {
            for argv in &session.shutdown {
                if let Some(cmd) = command(argv, Duration::from_secs(120)) {
                    run_quietly(ctx, &format!("shutdown.{platform}"), &cmd);
                    shut.push(cmd.display());
                }
            }
        } else if !device.name.is_empty() {
            ctx.rep.progress(format!(
                "{platform}: left {} running (icm did not create it)",
                device.name
            ));
        }
    }

    let _ = std::fs::remove_file(path);
    ctx.rep.progress(format!(
        "{platform}: stopped{}{}",
        if ended.is_empty() {
            String::new()
        } else {
            format!(
                " (processes {})",
                ended
                    .iter()
                    .map(i32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
        if stale.is_empty() {
            String::new()
        } else {
            format!("; {} already gone", stale.len())
        }
    ));

    json!({
        "platform": platform,
        "run": session.run,
        "processes": ended,
        "already_gone": stale,
        "commands": commands,
        "shutdown": shut,
    })
}

/// An argv from a session file as a command (`ICM_TOOL_*` honoured for
/// bare tool names).
fn command(argv: &[String], timeout: Duration) -> Option<Cmd> {
    let (program, args) = argv.split_first()?;
    let cmd = if program.contains('/') {
        Cmd::new(program)
    } else {
        Cmd::tool(program)
    };
    Some(cmd.args(args).timeout(timeout))
}

/// Runs a step; a failure is a WARN, never the command's failure.
fn run_quietly(ctx: &Ctx, name: &str, cmd: &Cmd) -> bool {
    match ctx.step(name, cmd) {
        Ok(outcome) if outcome.success() => true,
        Ok(outcome) => {
            ctx.rep.check(Check::from_error(
                ctx.step_failure(name, CheckId::ToolFailed, &outcome),
                Status::Warn,
            ));
            false
        }
        Err(error) => {
            ctx.rep.check(Check::from_error(error, Status::Warn));
            false
        }
    }
}

/// `--shutdown` without a session: the project's managed simulator or
/// emulator, when it is running.
fn shutdown_managed(
    ctx: &Ctx,
    project: &Project,
    host: &crate::host::HostConfig,
    platform: Platform,
) -> Vec<String> {
    // Android's own stop already shut down the managed emulators
    // ([`crate::android::stop_session`]).
    match platform {
        Platform::IosSim if cfg!(target_os = "macos") => shutdown_simulator(ctx, project, host),
        _ => Vec::new(),
    }
}

fn shutdown_simulator(ctx: &Ctx, project: &Project, host: &crate::host::HostConfig) -> Vec<String> {
    let Ok(xcode) = crate::tools::xcode(&ctx.env) else {
        return Vec::new();
    };
    let list = |what: &str| -> Option<String> {
        let cmd = xcode
            .xcrun()
            .args(["simctl", "list", "-j", what])
            .timeout(Duration::from_secs(60));
        ctx.probe(&cmd)
            .ok()
            .filter(|o| o.success())
            .map(|o| o.stdout_text())
    };
    let Some(runtimes) = list("runtimes").and_then(|j| crate::simctl::parse_runtimes(&j).ok())
    else {
        return Vec::new();
    };
    let Some(runtime) = crate::simctl::newest_runtime(&runtimes, &project.config.config.ios.min_os)
    else {
        return Vec::new();
    };
    let Some(device_type) =
        crate::simctl::choose_device_type(runtime, host.ios.simulator_type.as_deref())
    else {
        return Vec::new();
    };
    let name = managed::simulator_name(&device_type.name, &runtime.version);
    let Some(devices) = list("devices").and_then(|j| crate::simctl::parse_devices(&j).ok()) else {
        return Vec::new();
    };

    let mut done = Vec::new();
    for device in devices
        .iter()
        .filter(|d| d.name == name && d.state == "Booted" && managed::is_managed(&d.name))
    {
        let cmd = xcode
            .xcrun()
            .args(["simctl", "shutdown", &device.udid])
            .timeout(Duration::from_secs(120));
        if run_quietly(ctx, "shutdown.ios-sim", &cmd) {
            done.push(format!("{} ({})", device.name, device.udid));
        }
    }
    done
}

/// Whether the run `run` of this project finished with `ok: false`.
fn run_failed(project: &Project, run: &str) -> bool {
    std::fs::read_to_string(project.runs_dir().join(run).join("result.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|result| result.get("ok").and_then(Value::as_bool))
        == Some(false)
}

/// Runs `icm ps`: the sessions and whether their processes still run.
pub fn ps(ctx: &mut Ctx) -> Result<()> {
    let Some(project) = ctx.try_project().cloned() else {
        ctx.rep.set("sessions", json!([]));
        ctx.rep.summary("not in an icm project: no sessions");
        ctx.rep
            .content("no icm project here (no icm.toml), so no sessions\n");
        return Ok(());
    };
    let dir = project.sessions_dir();
    let mut sessions = Vec::new();
    let mut text = String::new();
    let mut running = 0;
    for (path, session) in session::list(&dir) {
        match session {
            Ok(session) => {
                let written = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                // A record with a command-line `marker` (the web session
                // host) also needs it in the process's command line.
                let marked = session.extra.get("marker").and_then(Value::as_str);
                let alive: Vec<i32> = session
                    .all_pids()
                    .into_iter()
                    .filter(|pid| session::is_ours(*pid, written))
                    .filter(|pid| {
                        marked.is_none_or(|marker| {
                            crate::sessions::command_line(*pid)
                                .is_some_and(|line| line.contains(marker))
                        })
                    })
                    .collect();
                // Android records the device's serial at the top level.
                let device = session
                    .device
                    .as_ref()
                    .map(|d| d.name.clone())
                    .filter(|name| !name.is_empty())
                    .or_else(|| {
                        session
                            .extra
                            .get("serial")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    });
                let mut line = format!("{}:", session.platform);
                // ios-sim keeps its record after `stop` (state `stopped`
                // or `exited`) so `icm logs ios-sim` can still read it.
                let state = session.extra.get("state").and_then(Value::as_str);
                let host_alive = !alive.is_empty();
                let has_pids = !session.all_pids().is_empty();
                // Android's app runs on a device and the web's in a page:
                // their records' host pids (none, the session host) say
                // nothing about the app, so they ask the platform.
                let app = match session.platform.as_str() {
                    _ if !host_alive && has_pids => AppState::Unknown,
                    "android" => crate::android::pipeline::app_state(ctx, &project),
                    "web" => crate::web::app_state(&project),
                    _ => AppState::Unknown,
                };
                let asks_platform = matches!(session.platform.as_str(), "android" | "web");
                let failed = if session
                    .run
                    .as_deref()
                    .is_some_and(|run| run_failed(&project, run))
                {
                    " (its run failed)"
                } else {
                    ""
                };
                let is_running = if !host_alive && matches!(state, Some("stopped" | "exited")) {
                    line.push_str(&format!(
                        " {} (its logs stay readable)",
                        state.unwrap_or_default()
                    ));
                    false
                } else if !host_alive && has_pids {
                    line.push_str(" stale (its processes are gone)");
                    false
                } else {
                    match &app {
                        AppState::Running => {
                            line.push_str(" running");
                            true
                        }
                        AppState::Gone(why) => {
                            line.push_str(&format!(" session open; {why}{failed}"));
                            false
                        }
                        AppState::Unknown if asks_platform => {
                            line.push_str(&format!(" session open (app state unknown){failed}"));
                            false
                        }
                        AppState::Unknown if !failed.is_empty() => {
                            line.push_str(" exited (its run failed)");
                            false
                        }
                        AppState::Unknown => {
                            line.push_str(" running");
                            true
                        }
                    }
                };
                if is_running {
                    running += 1;
                }
                if let Some(url) = &session.url {
                    line.push_str(&format!(" {url}"));
                }
                if let Some(device) = device.as_ref().filter(|d| !d.is_empty()) {
                    line.push_str(&format!(" on {device}"));
                }
                if let Some(run) = &session.run {
                    line.push_str(&format!(" (run {run})"));
                }
                text.push_str(&line);
                text.push('\n');
                let check = if is_running {
                    Check::pass(CheckId::RunAlive, line)
                } else {
                    Check::info(CheckId::RunNoSession, line)
                };
                ctx.rep.check(check.evidence(Evidence::file(&path)));
                sessions.push(json!({
                    "platform": session.platform,
                    "run": session.run,
                    "started": session.started,
                    "pids": session.all_pids(),
                    "alive": alive,
                    "running": is_running,
                    "app_running": match &app {
                        AppState::Running => json!(true),
                        AppState::Gone(_) => json!(false),
                        AppState::Unknown => Value::Null,
                    },
                    "run_failed": !failed.is_empty(),
                    "url": session.url,
                    "device": session.device,
                    "app": session.app,
                    "file": crate::paths::display(&path),
                }));
            }
            Err(message) => {
                ctx.rep.check(
                    Check::warn(CheckId::RunNoSession, message).evidence(Evidence::file(&path)),
                );
            }
        }
    }
    let count = sessions.len();
    if count == 0 {
        text.push_str("no sessions\n");
    }
    ctx.rep.content(text);
    ctx.rep.set("sessions", Value::Array(sessions));
    ctx.rep.summary(match count {
        0 => "no sessions".to_string(),
        n => format!("{n} session(s), {running} running"),
    });
    if running > 0 {
        ctx.rep
            .next("icm stop --all --json -q", "stop every session");
    }
    Ok(())
}
