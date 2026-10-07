//! `icm doctor [<platform>…] [--fix] [--yes]` (design §6, §4.4, Appendix C
//! item 15).
//!
//! One CHECK per requirement. `--fix` runs the local, idempotent fixes
//! (`by: doctor`: the managed simulator and AVD, the debug keystore);
//! `--fix --yes` also those that download or install (`by: doctor-yes`:
//! Rust toolchains and targets, the JDK, SDK packages, the iOS runtime,
//! wasm-bindgen-cli matching the app's lock). Each fix is printed before it
//! runs and is a step with its own log; afterwards everything is probed
//! again, up to three rounds, since some fixes enable others (a system
//! image, then the AVD that uses it).
//!
//! The exit code (§4.4): 4 while anything `icm doctor --fix [--yes]` could
//! fix remains, else 9 while owner items remain, else the exit of any other
//! failure, else 0. Optional tools are WARN and never change it.

use crate::catalogue::{By, CheckId};
use crate::cli::{DoctorArgs, Platform};
use crate::context::{Ctx, Project};
use crate::doctor::{self, Fix, Probe, Requirement};
use crate::error::{Check, Evidence, IcmError, Result, Status};
use crate::exit::Exit;
use crate::plan::{Plan, Step};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// How many times fixes run before doctor gives up and reports.
const FIX_ROUNDS: usize = 3;

/// Runs `icm doctor`.
pub fn run(ctx: &mut Ctx, args: &DoctorArgs) -> Result<()> {
    let host = ctx.host()?.clone();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // The project, when there is one. A broken icm.toml is reported and
    // the machine is still checked.
    let mut config_problem: Option<IcmError> = None;
    let project: Option<Project> =
        if crate::config::locate(ctx.global.config.as_deref(), &cwd).is_ok() {
            match ctx.project() {
                Ok(project) => Some(project.clone()),
                Err(error) => {
                    config_problem = Some(error);
                    None
                }
            }
        } else {
            None
        };
    if project.is_none() {
        let _ = ctx.rep.attach(&crate::paths::cache_dir());
    }
    let dir = project
        .as_ref()
        .map(|p| p.dir().to_path_buf())
        .unwrap_or_else(|| cwd.clone());

    let explicit = !args.platforms.is_empty();
    let mut platforms = if explicit {
        args.platforms.clone()
    } else {
        doctor::default_platforms(project.as_ref())
    };
    platforms.sort();
    platforms.dedup();

    let env = ctx.env.clone();
    let downloads = ctx.global.yes && !ctx.global.offline;

    let gather = |ctx: &Ctx| -> Vec<Requirement> {
        let probe = Probe {
            ctx,
            host: &host,
            env: &env,
            project: project.as_ref(),
            dir: &dir,
            explicit,
        };
        doctor::gather(&probe, &platforms)
    };

    let mut requirements = gather(ctx);

    if ctx.dry_run() {
        let mut plan = Plan::new();
        for fix in selected(&requirements, true, &[]) {
            match fix.steps(&host, &env, ctx.global.offline) {
                Ok(steps) => {
                    for step in steps {
                        plan.push(Step::exec(&step.name, step.cmd));
                    }
                }
                Err(error) => plan.push(Step::internal("doctor.fix", &error.to_string())),
            }
        }
        plan.report(ctx);
        ctx.rep
            .set("requirements", requirements_json(&requirements));
        return Ok(());
    }

    // Fix, then look again.
    let mut attempted: Vec<Fix> = Vec::new();
    let mut fix_failures: BTreeMap<String, Vec<Evidence>> = BTreeMap::new();
    let mut fixed: Vec<String> = Vec::new();
    if args.fix {
        for _ in 0..FIX_ROUNDS {
            let fixes = selected(&requirements, downloads, &attempted);
            if fixes.is_empty() {
                break;
            }
            for fix in &fixes {
                // A fix is tried once per doctor run, even when it fails.
                let keys: Vec<String> = requirements
                    .iter()
                    .filter(|r| r.fixes.iter().any(|wanted| fix.covers(wanted)))
                    .map(|r| r.key.clone())
                    .collect();
                attempted.push(fix.clone());

                match run_fix(ctx, fix, &host, &env) {
                    Ok(names) => fixed.extend(names),
                    Err(evidence) => {
                        for key in keys {
                            fix_failures
                                .entry(key)
                                .or_default()
                                .extend(evidence.clone());
                        }
                    }
                }
            }
            requirements = gather(ctx);
        }
    }

    // Report.
    if let Some(problem) = &config_problem {
        ctx.rep
            .check(Check::from_error(problem.clone(), Status::Fail));
    }
    let mut remaining: Vec<IcmError> = Vec::new();
    for requirement in &requirements {
        let mut check = requirement.check.clone();
        if let Some(evidence) = fix_failures.get(&requirement.key)
            && requirement.failing()
        {
            for item in evidence {
                check = check.evidence(item.clone());
            }
            check
                .error
                .detail
                .push_str("; the fix failed (see its step log)");
        }
        if !args.fix && !requirement.fixes.is_empty() && requirement.failing() {
            check.error.likely_causes.push(format!(
                "`icm doctor {} --fix{}` repairs it",
                requirement
                    .platform
                    .map(Platform::as_str)
                    .unwrap_or_default(),
                if requirement.by() == By::DoctorYes {
                    " --yes"
                } else {
                    ""
                }
            ));
        }
        if check.status == Status::Fail {
            remaining.push(check.error.clone());
        }
        ctx.rep.check(check);
    }

    let probe = Probe {
        ctx,
        host: &host,
        env: &env,
        project: project.as_ref(),
        dir: &dir,
        explicit,
    };
    ctx.rep.set("tools", doctor::tools_json(&probe, &platforms));
    ctx.rep.set(
        "platforms",
        json!(platforms.iter().map(|p| p.as_str()).collect::<Vec<_>>()),
    );
    ctx.rep.set("fixed", json!(fixed));
    ctx.rep
        .set("requirements", requirements_json(&requirements));

    let names = platforms
        .iter()
        .map(|p| p.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let fixable = remaining
        .iter()
        .filter(|e| matches!(e.fix.by, By::Doctor | By::DoctorYes))
        .count();
    let needs_yes_count = remaining
        .iter()
        .filter(|e| e.fix.by == By::DoctorYes)
        .count();
    let owner = remaining.iter().filter(|e| e.fix.by == By::Owner).count();

    if fixable > 0 {
        let needs_yes = remaining.iter().any(|e| e.fix.by == By::DoctorYes);
        ctx.rep.next(
            format!(
                "icm doctor {names} --fix{} --json -q",
                if needs_yes { " --yes" } else { "" }
            ),
            if needs_yes {
                "repair what is missing; --yes allows downloads and installs"
            } else {
                "repair what is missing (local changes only)"
            },
        );
    }

    ctx.rep.summary(match (remaining.len(), fixable, owner) {
        (0, _, _) => format!(
            "{names}: ready{}",
            if fixed.is_empty() {
                String::new()
            } else {
                format!(" ({} fix(es) applied)", fixed.len())
            }
        ),
        (n, f, o) => {
            // `--fix` alone repairs the doctor items; the doctor-yes ones
            // (downloads, installs) need `--fix --yes`.
            let mut parts = Vec::new();
            if f > needs_yes_count {
                parts.push(format!("{} for `icm doctor --fix`", f - needs_yes_count));
            }
            if needs_yes_count > 0 {
                parts.push(format!("{needs_yes_count} for `icm doctor --fix --yes`"));
            }
            if o > 0 {
                parts.push(format!("{o} for the owner"));
            }
            if n - f - o > 0 {
                parts.push(format!("{} for the agent", n - f - o));
            }
            format!("{names}: {n} problem(s): {}", parts.join(", "))
        }
    });

    // Config problems keep their own exit (3) unless a machine problem
    // outranks them.
    let blocking = remaining
        .iter()
        .find(|e| matches!(e.fix.by, By::Doctor | By::DoctorYes))
        .map(|e| e.clone().exit(Exit::Environment))
        .or_else(|| {
            remaining
                .iter()
                .find(|e| e.fix.by == By::Owner)
                .map(|e| e.clone().exit(Exit::NeedsOwner))
        })
        .or_else(|| remaining.first().cloned())
        .or(config_problem);

    match blocking {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// The fixes to run this round: those of failing requirements, allowed by
/// the flags, not tried before, merged by kind and in phase order.
fn selected(requirements: &[Requirement], downloads: bool, attempted: &[Fix]) -> Vec<Fix> {
    let mut fixes: Vec<Fix> = Vec::new();
    for requirement in requirements.iter().filter(|r| r.failing()) {
        for fix in &requirement.fixes {
            if fix.by() == By::DoctorYes && !downloads {
                continue;
            }
            if attempted.iter().any(|done| done.covers(fix)) {
                continue;
            }
            if !fixes.iter_mut().any(|existing| existing.merge(fix)) {
                fixes.push(fix.clone());
            }
        }
    }
    fixes.sort_by_key(Fix::phase);
    fixes
}

/// Runs one fix's commands as steps. On failure, returns the evidence
/// (the step logs) to attach to the requirements it was for.
fn run_fix(
    ctx: &Ctx,
    fix: &Fix,
    host: &crate::host::HostConfig,
    env: &crate::tools::Env,
) -> std::result::Result<Vec<String>, Vec<Evidence>> {
    if let Err(error) = fix.prepare() {
        ctx.rep.progress(format!("fix failed: {error}"));
        return Err(Vec::new());
    }
    let steps = match fix.steps(host, env, ctx.global.offline) {
        Ok(steps) => steps,
        Err(error) => {
            ctx.rep.progress(format!("cannot fix: {error}"));
            return Err(error.evidence);
        }
    };

    let mut done = Vec::new();
    for step in steps {
        ctx.rep.progress(format!("fix: {}", step.cmd.display()));
        match ctx.step(&step.name, &step.cmd) {
            Ok(outcome) if outcome.success() => done.push(step.cmd.display()),
            Ok(outcome) => {
                let error = ctx.step_failure(&step.name, CheckId::ToolFailed, &outcome);
                ctx.rep.progress(format!("fix failed: {}", error.detail));
                return Err(error.evidence);
            }
            Err(error) => {
                ctx.rep.progress(format!("fix failed: {}", error.detail));
                return Err(error.evidence);
            }
        }
    }
    Ok(done)
}

fn requirements_json(requirements: &[Requirement]) -> serde_json::Value {
    json!(
        requirements
            .iter()
            .map(|r| json!({
                "key": r.key,
                "platform": r.platform.map(Platform::as_str),
                "id": r.check.error.id,
                "status": r.check.status,
                "by": r.by(),
                "fixable": !r.fixes.is_empty(),
            }))
            .collect::<Vec<_>>()
    )
}
