//! `icm build --all` (or `icm build` with no platform): every platform in
//! `[app] platforms` in turn, iOS as the simulator (macOS hosts only), so
//! an agent can prewarm every build with `icm build --all --detach`
//! (Appendix C item 24). `icm build <platform>` goes straight to the
//! platform (`commands/mod.rs`).
//!
//! Every platform is built even after one fails; each failure is a FAIL
//! check, and the first one is the command's error.

use crate::catalogue::CheckId;
use crate::cli::{BuildArgs, Platform};
use crate::config::AppPlatform;
use crate::context::Ctx;
use crate::error::{Check, IcmError, Result, Status};
use serde_json::json;

/// Runs `icm build --all`.
pub fn all(ctx: &mut Ctx, args: &BuildArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    let mut platforms: Vec<Platform> = project
        .app()
        .platforms
        .iter()
        .map(|platform| match platform {
            AppPlatform::Desktop => Platform::Desktop,
            AppPlatform::Web => Platform::Web,
            AppPlatform::Ios => Platform::IosSim,
            AppPlatform::Android => Platform::Android,
        })
        .collect();
    platforms.sort();
    platforms.dedup();

    let mut built: Vec<&str> = Vec::new();
    let mut skipped: Vec<&str> = Vec::new();
    let mut first_error: Option<IcmError> = None;
    for platform in platforms {
        if platform == Platform::IosSim && !cfg!(target_os = "macos") {
            ctx.rep.check(Check::skip(
                CheckId::EnvUnsupportedHost,
                "ios-sim builds need a macOS host",
            ));
            skipped.push(platform.as_str());
            continue;
        }
        ctx.rep.progress(format!("building {}", platform.as_str()));
        let one = BuildArgs {
            platform: Some(platform),
            all: false,
            release: args.release,
            device: args.device.clone(),
            abi: args.abi.clone(),
        };
        let result = match platform {
            Platform::Desktop => crate::platform::desktop::build(ctx, &one),
            Platform::Web => crate::web::build_command(ctx, &one),
            Platform::IosSim => crate::platform::ios_sim::build(ctx, &one),
            Platform::Android => crate::android::build(ctx, &one),
            Platform::IosDevice => continue,
        };
        match result {
            Ok(()) => built.push(platform.as_str()),
            Err(error) => {
                ctx.rep
                    .check(Check::from_error(error.clone(), Status::Fail));
                let _ = first_error.get_or_insert(error);
            }
        }
    }

    if ctx.dry_run() {
        // Each platform reported its steps; nothing was built.
        ctx.rep.set("planned", json!(built));
        ctx.rep.set("skipped", json!(skipped));
        ctx.rep.summary(format!(
            "the plan of icm build --all for {} (--dry-run: nothing was built)",
            built.join(", ")
        ));
        return match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        };
    }
    ctx.rep.set("built", json!(built));
    ctx.rep.set("skipped", json!(skipped));
    let failed = first_error.is_some();
    ctx.rep.summary(match (built.is_empty(), failed) {
        (true, false) => "nothing to build: [app] platforms is empty".to_string(),
        (_, false) => format!("built {}", built.join(", ")),
        (true, true) => "no platform built".to_string(),
        (false, true) => format!("built {}; a platform failed", built.join(", ")),
    });
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
