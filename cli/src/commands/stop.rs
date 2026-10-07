//! `icm stop [<platform>|--all] [--shutdown]` and `icm ps` (design §3
//! "Sessions", §6).
//!
//! Both work from the session files `icm run` writes
//! ([`crate::session`]), so they need no platform knowledge: `stop` runs a
//! session's recorded stop commands, ends the processes icm started and
//! removes the file; `--shutdown` also shuts down the icm-managed
//! simulator or emulator (`icm-` names only, never `icm-test-` ones and
//! never a device icm did not create). Stopping what is not running is not
//! an error.

use crate::catalogue::CheckId;
use crate::cli::{Platform, StopArgs};
use crate::context::{Ctx, Project};
use crate::error::{Check, Evidence, IcmError, Result, Status};
use crate::managed;
use crate::process::Cmd;
use crate::session::{self, Session};
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
    let host = ctx.host()?.clone();
    let dir = project.sessions_dir();

    let mut stopped: Vec<Value> = Vec::new();
    for (path, session) in session::list(&dir) {
        let platform = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
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

    let mut shut_down: Vec<String> = Vec::new();
    if args.shutdown {
        let platforms: Vec<Platform> = match args.platform {
            Some(platform) => vec![platform],
            None => Platform::ALL.to_vec(),
        };
        for platform in platforms {
            shut_down.extend(shutdown_managed(ctx, &project, &host, platform));
        }
    }

    let count = stopped.len();
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
    match platform {
        Platform::IosSim if cfg!(target_os = "macos") => shutdown_simulator(ctx, project, host),
        Platform::Android => shutdown_emulator(ctx, project, host),
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

fn shutdown_emulator(ctx: &Ctx, project: &Project, host: &crate::host::HostConfig) -> Vec<String> {
    let Ok(sdk) = crate::tools::android_sdk(host, &ctx.env) else {
        return Vec::new();
    };
    let adb = sdk.adb(&ctx.env);
    if !adb.exists() {
        return Vec::new();
    }
    let name = managed::avd_name(project.config.config.android.target_sdk);
    let Ok(devices) = ctx.probe(
        &Cmd::new(&adb)
            .arg("devices")
            .timeout(Duration::from_secs(30)),
    ) else {
        return Vec::new();
    };

    let mut done = Vec::new();
    for serial in devices
        .stdout_text()
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|serial| serial.starts_with("emulator-"))
    {
        let avd = ctx
            .probe(
                &Cmd::new(&adb)
                    .args(["-s", serial, "emu", "avd", "name"])
                    .timeout(Duration::from_secs(30)),
            )
            .ok()
            .map(|o| o.stdout_text())
            .and_then(|text| text.lines().next().map(|l| l.trim().to_string()))
            .unwrap_or_default();
        if avd == name && managed::is_managed(&avd) {
            let cmd = Cmd::new(&adb)
                .args(["-s", serial, "emu", "kill"])
                .timeout(Duration::from_secs(60));
            if run_quietly(ctx, "shutdown.android", &cmd) {
                done.push(format!("{avd} ({serial})"));
            }
        }
    }
    done
}

/// Runs `icm ps`: the sessions and whether their processes still run.
pub fn ps(ctx: &mut Ctx) -> Result<()> {
    let project = ctx.project()?.clone();
    let dir = project.sessions_dir();
    let mut sessions = Vec::new();
    let mut running = 0;
    for (path, session) in session::list(&dir) {
        match session {
            Ok(session) => {
                let written = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                let alive: Vec<i32> = session
                    .all_pids()
                    .into_iter()
                    .filter(|pid| session::is_ours(*pid, written))
                    .collect();
                let device = session.device.as_ref().map(|d| d.name.clone());
                let mut line = format!("{}:", session.platform);
                if alive.is_empty() && !session.all_pids().is_empty() {
                    line.push_str(" stale (its processes are gone)");
                } else {
                    line.push_str(" running");
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
                let check = if alive.is_empty() && !session.all_pids().is_empty() {
                    Check::info(CheckId::RunNoSession, line)
                } else {
                    Check::pass(CheckId::RunAlive, line)
                };
                ctx.rep.check(check.evidence(Evidence::file(&path)));
                sessions.push(json!({
                    "platform": session.platform,
                    "run": session.run,
                    "started": session.started,
                    "pids": session.all_pids(),
                    "alive": alive,
                    "running": !alive.is_empty() || session.all_pids().is_empty(),
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
