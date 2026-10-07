//! `icm input ios-sim` (design §13.5): the device-state helpers simctl
//! offers, with no extra tool:
//!
//! - `appearance light|dark`: `simctl ui <udid> appearance`;
//! - `font-scale <f>`: `simctl ui <udid> content_size <category>` (1.0 is
//!   `large`, the default);
//! - `background`: opens Settings over the app;
//! - `foreground`: `simctl launch` of the running app, which brings it back
//!   with the same pid.
//!
//! Touches, text and keys need AXe, which phase 1 leaves out (Appendix C
//! item 30): they exit 2 `input.unsupported`, pointing at the headless
//! harness.

use super::{PLATFORM, read_session, session_device, simctl};
use crate::catalogue::CheckId;
use crate::cli::{InputAction, InputArgs, Theme};
use crate::context::Ctx;
use crate::error::{IcmError, Result};
use serde_json::json;
use std::time::Duration;

/// How long `background` and `foreground` wait for the transition.
const SETTLE: Duration = Duration::from_millis(1_200);

/// The content size category closest to a font scale.
pub fn content_size(scale: f64) -> &'static str {
    const STEPS: &[(f64, &str)] = &[
        (0.82, "extra-small"),
        (0.90, "small"),
        (0.97, "medium"),
        (1.05, "large"),
        (1.15, "extra-large"),
        (1.25, "extra-extra-large"),
        (1.40, "extra-extra-extra-large"),
        (1.70, "accessibility-medium"),
        (2.00, "accessibility-large"),
        (2.40, "accessibility-extra-large"),
        (2.80, "accessibility-extra-extra-large"),
    ];
    STEPS
        .iter()
        .find(|(limit, _)| scale < *limit)
        .map_or("accessibility-extra-extra-extra-large", |(_, name)| name)
}

/// `icm input ios-sim`.
pub fn input(ctx: &mut Ctx, args: &InputArgs) -> Result<()> {
    if ctx.dry_run() {
        return super::dry_run(
            ctx,
            &[(
                "ios-sim.input",
                format!(
                    "send {:?} to the session's simulator through xcrun simctl",
                    args.action
                ),
            )],
        );
    }
    let unsupported = |what: &str| {
        Err(IcmError::new(
            CheckId::InputUnsupported,
            format!(
                "`icm input {PLATFORM} {what}` needs AXe, which this phase of icm does not drive; \
                 appearance, font-scale, background and foreground work"
            ),
        )
        .fix(
            "Exercise the UI through the headless harness (`icm ui --headless`, .ice flows with `icm test`), or use Android or web for real input.",
            &["icm ui --headless tree --json", "icm test --json -q"],
        ))
    };
    match &args.action {
        InputAction::Tap { .. } => return unsupported("tap"),
        InputAction::Swipe { .. } => return unsupported("swipe"),
        InputAction::Text { .. } => return unsupported("text"),
        InputAction::Key { .. } => return unsupported("key"),
        InputAction::Rotate { .. } => return unsupported("rotate"),
        _ => {}
    }

    let (_project, session) = read_session(ctx)?;
    let xcode = crate::tools::xcode(&ctx.env)?;
    ctx.rep.set("device", session_device(&session));
    let udid = session.device.udid.clone();

    let (name, cmd, detail) = match &args.action {
        InputAction::Appearance { mode } => {
            let mode = match mode {
                Theme::Light => "light",
                Theme::Dark => "dark",
            };
            (
                "simctl.ui.appearance",
                simctl(&xcode).args(["ui", &udid, "appearance", mode]),
                format!("appearance set to {mode}"),
            )
        }
        InputAction::FontScale { scale } => {
            let category = content_size(*scale);
            (
                "simctl.ui.content_size",
                simctl(&xcode).args(["ui", &udid, "content_size", category]),
                format!("content size set to {category} (font scale {scale})"),
            )
        }
        InputAction::Background => (
            "simctl.background",
            simctl(&xcode).args(["launch", &udid, "com.apple.Preferences"]),
            format!(
                "{} sent to the background (Settings opened)",
                session.app_id
            ),
        ),
        InputAction::Foreground => {
            if !session.app_alive() {
                return Err(IcmError::new(
                    CheckId::RunNoSession,
                    format!("{} is not running", session.app_id),
                )
                .fix("Start it again.", &["icm run ios-sim --json -q"]));
            }
            (
                "simctl.foreground",
                simctl(&xcode).args(["launch", &udid, &session.app_id]),
                format!("{} brought to the foreground", session.app_id),
            )
        }
        _ => unreachable!("handled above"),
    };

    let outcome = ctx.step(name, &cmd.timeout(Duration::from_secs(60)))?;
    if !outcome.success() {
        return Err(ctx.step_failure(name, CheckId::ToolFailed, &outcome));
    }
    // Let the app-switch animation finish, so the next screenshot shows
    // the end state.
    if matches!(
        args.action,
        InputAction::Background | InputAction::Foreground
    ) {
        super::sleep_checked(SETTLE)?;
    }

    let mut pid = session.pid;
    if matches!(args.action, InputAction::Foreground) {
        pid = outcome
            .stdout_text()
            .lines()
            .find_map(|line| {
                line.strip_prefix(&format!("{}:", session.app_id))
                    .and_then(|pid| pid.trim().parse::<i64>().ok())
            })
            .or(pid);
        ctx.rep.set("same_pid", json!(pid == session.pid));
    }
    ctx.rep.set(
        "process",
        json!({"pid": pid, "alive": session.app_alive(), "ready": null}),
    );
    ctx.rep.summary(detail);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_scales_map_to_content_sizes() {
        assert_eq!(content_size(1.0), "large");
        assert_eq!(content_size(1.3), "extra-extra-extra-large");
        assert_eq!(content_size(0.8), "extra-small");
        assert_eq!(content_size(2.1), "accessibility-extra-large");
        assert_eq!(content_size(9.0), "accessibility-extra-extra-extra-large");
    }
}
