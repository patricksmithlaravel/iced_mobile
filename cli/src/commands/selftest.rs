//! `icm __test <scenario>`: hidden scenarios that exercise the core
//! (runner, signals, plans, locks, the output contract) end to end for
//! icm's own integration tests.

use crate::catalogue::CheckId;
use crate::cli::{Scenario, SelfTestArgs};
use crate::context::Ctx;
use crate::error::{Check, IcmError, Result};
use crate::plan::{Plan, Step};
use crate::process::Cmd;
use serde_json::json;
use std::time::{Duration, Instant};

/// Runs a scenario.
pub fn run(ctx: &mut Ctx, args: SelfTestArgs) -> Result<()> {
    match args.scenario {
        Scenario::Sleep { duration } => sleep(ctx, duration),
        Scenario::Panic => panic!("icm __test panic: a deliberate panic"),
        Scenario::Fail { id } => match CheckId::from_id(&id) {
            Some(check) => Err(IcmError::new(check, "a deliberate failure from icm __test")),
            None => Err(IcmError::new(
                CheckId::UsageBadArgs,
                format!("`{id}` is not a catalogue id"),
            )),
        },
        Scenario::Checks { fail } => {
            ensure_run_dir(ctx);
            ctx.rep
                .check(Check::pass(CheckId::DepsSingleIced, "a passing check"));
            ctx.rep
                .check(Check::warn(CheckId::RunScreenBlank, "a warning"));
            if fail {
                ctx.rep.check(Check::fail(
                    CheckId::WebSizeBudget,
                    "a non-blocking failure",
                ));
            }
            Ok(())
        }
        Scenario::Plan => {
            ensure_run_dir(ctx);
            let mut plan = Plan::new();
            plan.push(Step::exec(
                "selftest.echo",
                Cmd::new("/bin/echo").arg("one"),
            ));
            plan.push(Step::internal("selftest.internal", "does nothing"));
            plan.push(
                Step::exec(
                    "selftest.env",
                    Cmd::new("/bin/sh")
                        .args(["-c", "echo \"$ICM_SELFTEST_TOKEN\" >&2"])
                        .env("ICM_SELFTEST_TOKEN", "do-not-print-me"),
                )
                .gate(CheckId::RunReady),
            );
            if ctx.dry_run() {
                plan.report(ctx);
                return Ok(());
            }
            let _ = plan.execute(ctx, |_| Ok(()))?;
            Ok(())
        }
        Scenario::Project => {
            let project = ctx.project()?.clone();
            ctx.rep.set(
                "project",
                json!({
                    "package": project.package.name,
                    "dir": crate::paths::display(project.dir()),
                    "icm_dir": crate::paths::display(&project.icm_dir),
                    "bin": project.bin_for("desktop").ok(),
                    "lib": project.lib_name().ok(),
                }),
            );
            if let Some(lock) = project.lock()? {
                let checks = crate::deps::check_lock(&lock, true);
                let first_failure = checks.iter().find(|check| check.failed()).cloned();
                for check in checks {
                    ctx.rep.check(check);
                }
                if let Some(failure) = first_failure {
                    return Err(failure.into_error());
                }
            }
            Ok(())
        }
        Scenario::Lock { platform, duration } => {
            let _lock = ctx.lock_platform(platform.as_str())?;
            wait(duration)
        }
        Scenario::Busy { duration } => {
            ensure_run_dir(ctx);
            let until = Instant::now() + duration;
            while Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(())
        }
        Scenario::Deployment { min_os } => {
            let project = ctx.project()?.clone();
            let package = project.package.name.clone();
            let triple = crate::toolchain::ios_sim_triple();
            let prepared =
                ctx.deployment_target(&project, &package, Some(triple), "dev", &min_os)?;
            if let Some(((var, value), stamp)) = prepared {
                let mut env = serde_json::Map::new();
                let _ = env.insert(var, json!(value));
                ctx.rep.set("env", serde_json::Value::Object(env));
                stamp.write().map_err(|error| {
                    IcmError::new(
                        CheckId::InternalBug,
                        format!("cannot write the stamp: {error}"),
                    )
                })?;
            }
            Ok(())
        }
    }
}

/// Keeps the run in the project (if any) or the cache dir from the start,
/// so steps get logs.
fn ensure_run_dir(ctx: &mut Ctx) {
    if ctx.rep.run_dir().is_some() {
        return;
    }
    let root = match ctx.try_project() {
        Some(project) => project.icm_dir.clone(),
        None => crate::paths::cache_dir(),
    };
    let _ = ctx.rep.attach(&root);
}

fn sleep(ctx: &mut Ctx, duration: Duration) -> Result<()> {
    ensure_run_dir(ctx);
    let dir = ctx
        .rep
        .run_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("icm-selftest"));
    let _ = std::fs::create_dir_all(&dir);
    let secs = duration.as_secs_f64();
    let script = format!(
        "echo $$ > '{dir}/child.pid'; sleep {secs} & echo $! > '{dir}/grandchild.pid'; wait",
        dir = dir.display()
    );
    let outcome = ctx.step("selftest.sleep", &Cmd::new("/bin/sh").arg("-c").arg(script))?;
    if !outcome.success() {
        return Err(ctx.step_failure("selftest.sleep", CheckId::ToolFailed, &outcome));
    }
    Ok(())
}

fn wait(duration: Duration) -> Result<()> {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        if let Some(signal) = crate::signals::pending() {
            return Err(crate::output::interrupted(signal));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}
