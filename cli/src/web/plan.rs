//! What `--dry-run` (and `icm print plan`) shows for the web commands: the
//! steps each would run, and nothing more. A dry run starts no session or
//! browser, takes no lock and writes nothing.

use super::{PLATFORM, TRIPLE, bindgen_cmd, invocation, profile_name, site};
use crate::catalogue::CheckId;
use crate::cli::{BuildArgs, InputArgs, LogsArgs, RunArgs, ShotArgs};
use crate::context::{Ctx, Project};
use crate::error::Result;
use crate::plan::{Plan, Step};
use crate::time::format_duration;
use serde_json::json;
use std::path::PathBuf;

/// The steps of a web build.
fn build_steps(ctx: &Ctx, project: &Project, release: bool, plan: &mut Plan) -> Result<()> {
    let profile = profile_name(release);
    let package = project.package_for(PLATFORM)?.clone();
    let bin = project.bin_for(PLATFORM)?;
    plan.push(Step::internal(
        "web.toolchain",
        &format!("check the {TRIPLE} target on the project's toolchain"),
    ));

    let version = project
        .lock()?
        .and_then(|lock| lock.version_of("wasm-bindgen").map(str::to_string));
    let bindgen = match &version {
        Some(version) => match crate::tools::wasm_bindgen(version, &ctx.env) {
            Ok(found) => found.path,
            Err(_) => {
                plan.push(
                    Step::internal(
                        "deps.wasm_bindgen_cli",
                        &format!(
                            "no wasm-bindgen CLI {version} yet: the build would fail with deps.wasm_bindgen_cli (`icm doctor web --fix --yes`)"
                        ),
                    )
                    .gate(CheckId::DepsWasmBindgenCli),
                );
                PathBuf::from("wasm-bindgen")
            }
        },
        None => PathBuf::from("wasm-bindgen"),
    };

    let (invocation, env) = invocation(project, &package, &bin, release);
    plan.push(
        Step::exec(
            "cargo.build",
            invocation
                .cmd()
                .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str()))),
        )
        .on_fail(CheckId::BuildCompileError),
    );
    let wasm = crate::cargo::artifacts_dir(&project.target_dir, Some(TRIPLE), &invocation.profile)
        .join(format!("{bin}.wasm"));
    let site_dir = project.build_dir(PLATFORM, profile).join("site");
    plan.push(Step::exec(
        "wasm-bindgen",
        bindgen_cmd(&bindgen, &wasm, &site_dir.join("pkg"), release),
    ));
    plan.push(Step::internal(
        "site.generate",
        &format!(
            "write index.html, manifest.webmanifest, the icon and [app] resources into {} ({}.js, {}_bg.wasm)",
            crate::paths::display(&site_dir),
            site::OUT_NAME,
            site::OUT_NAME
        ),
    ));
    plan.push(
        Step::internal(
            "web.features",
            "read iced's features for wasm32 (cargo metadata): fira-sans and webgl",
        )
        .gate(CheckId::WebFontsEmbedded)
        .gate(CheckId::WebRendererFallback),
    );
    Ok(())
}

/// `icm build web --dry-run`.
pub fn build(ctx: &mut Ctx, args: &BuildArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    let mut plan = Plan::new();
    build_steps(ctx, &project, args.release, &mut plan)?;
    finish(ctx, plan, "build web");
    Ok(())
}

/// `icm run web --dry-run`.
pub fn run(ctx: &mut Ctx, args: &RunArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    let host = ctx.host()?.clone();
    let mut plan = Plan::new();
    if !args.no_build {
        build_steps(ctx, &project, args.release, &mut plan)?;
    }
    let chrome = crate::tools::chrome(&host, &ctx.env)
        .map(|found| crate::paths::display(&found.path))
        .unwrap_or_else(|error| format!("no Chrome: {}", error.detail));
    plan.push(Step::internal(
        "web.session",
        &format!(
            "replace this project's web session ({}); serve {} on 127.0.0.1:{}; start headless Chrome ({chrome}) at the {} viewport",
            crate::paths::display(&project.sessions_dir().join("web.json")),
            crate::paths::display(&project.build_dir(PLATFORM, profile_name(args.release)).join("site")),
            args.port,
            args.viewport.as_deref().unwrap_or(super::viewport::DEFAULT)
        ),
    ));
    plan.push(
        Step::internal(
            "web.ready",
            &format!(
                "wait up to {} for ICM_EVENT ready on the page's console",
                format_duration(args.wait_ready)
            ),
        )
        .gate(CheckId::RunReady)
        .on_fail(CheckId::RunNotReady),
    );
    if !args.no_shot {
        plan.push(
            Step::internal(
                "web.screenshot",
                "Page.captureScreenshot into the run directory, then the preview and blank check",
            )
            .gate(CheckId::RunScreenBlank),
        );
    }
    plan.push(Step::internal(
        "hooks",
        "run the [checks] web scripts, if any",
    ));
    finish(ctx, plan, "run web");
    Ok(())
}

/// `icm shot web --dry-run`.
pub fn shot(ctx: &mut Ctx, args: &ShotArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    let mut plan = Plan::new();
    plan.push(Step::internal(
        "web.screenshot",
        &format!(
            "ask the web session ({}) for a screenshot into {}",
            crate::paths::display(&project.sessions_dir().join("web.json")),
            args.out
                .as_deref()
                .map(crate::paths::display)
                .unwrap_or_else(|| "the run directory".to_string())
        ),
    ));
    finish(ctx, plan, "shot web");
    Ok(())
}

/// `icm input web --dry-run`.
pub fn input(ctx: &mut Ctx, args: &InputArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    let mut plan = Plan::new();
    plan.push(Step::internal(
        "web.input",
        &format!(
            "send {:?} to the page through the web session ({})",
            args.action,
            crate::paths::display(&project.sessions_dir().join("web.json"))
        ),
    ));
    finish(ctx, plan, "input web");
    Ok(())
}

/// `icm logs web --dry-run`.
pub fn logs(ctx: &mut Ctx, args: &LogsArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    let mut plan = Plan::new();
    plan.push(Step::internal(
        "web.logs",
        &format!(
            "read the session's console.ndjson ({}), the last {} records{}",
            crate::paths::display(&project.sessions_dir().join(PLATFORM)),
            args.tail,
            if args.follow { ", then follow" } else { "" }
        ),
    ));
    finish(ctx, plan, "logs web");
    Ok(())
}

fn finish(ctx: &Ctx, plan: Plan, what: &str) {
    plan.report(ctx);
    ctx.rep.set("platform", json!(PLATFORM));
    ctx.rep
        .summary(format!("the plan of icm {what} (--dry-run: nothing ran)"));
}
