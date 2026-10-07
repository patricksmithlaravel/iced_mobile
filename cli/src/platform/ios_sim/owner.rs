//! Which project icm booted a simulator for.
//!
//! icm's managed simulators are shared by name: every project on the host
//! that runs on an iPhone 17 with iOS 27.0 uses `icm-iphone-17-ios-27.0`.
//! A project's own records live in its target directory, where another
//! project cannot see them, so a run that boots a managed simulator also
//! writes the project's tag into the simulator's launchd environment
//! ([`OWNER_ENV`], through `simctl spawn <udid> launchctl setenv`), where
//! every icm that reaches the simulator can read it (`simctl getenv`). It
//! is the tag Android's emulators carry in `debug.icm.booted_by`
//! ([`crate::android::session::owner_tag`]), and like that property it
//! lasts until the simulator shuts down. `stop --shutdown` leaves a
//! simulator running when its tag names another project
//! ([`booted_for_another`]); one without a tag (booted outside icm, or by
//! an icm before this) is still shut down.

use crate::catalogue::CheckId;
use crate::context::{Ctx, Project};
use crate::error::Check;
use crate::tools::Xcode;
use std::time::Duration;

/// The launchd variable that names the project icm booted a simulator for.
pub const OWNER_ENV: &str = "ICM_BOOTED_BY";

/// This project's tag.
pub fn tag(project: &Project) -> String {
    crate::android::session::owner_tag(project)
}

/// Marks a simulator icm has just booted as this project's. A failure
/// only leaves it unmarked, as if booted outside icm.
pub fn claim(ctx: &Ctx, xcode: &Xcode, project: &Project, udid: &str) {
    let _ = ctx.probe(
        &xcode
            .xcrun()
            .args(["simctl", "spawn", udid, "launchctl", "setenv", OWNER_ENV])
            .arg(tag(project))
            .timeout(Duration::from_secs(30)),
    );
}

/// The tag in a booted simulator's [`OWNER_ENV`], if it has one (`simctl
/// getenv` prints nothing on stdout for an unset variable).
pub fn owner(ctx: &Ctx, xcode: &Xcode, udid: &str) -> Option<String> {
    let outcome = ctx
        .probe(
            &xcode
                .xcrun()
                .args(["simctl", "getenv", udid, OWNER_ENV])
                .timeout(Duration::from_secs(30)),
        )
        .ok()?;
    let value = outcome.stdout_text().trim().to_string();
    (outcome.success() && !value.is_empty()).then_some(value)
}

/// Whether `stop --shutdown` must leave the simulator `name` (`udid`)
/// running because icm booted it for another project, which may still run
/// its app there. Reports it (INFO `ios.sim.shared`), with a fix that names
/// this simulator alone.
pub fn booted_for_another(
    ctx: &Ctx,
    xcode: &Xcode,
    project: &Project,
    name: &str,
    udid: &str,
) -> bool {
    let Some(owner) = owner(ctx, xcode, udid).filter(|owner| *owner != tag(project)) else {
        return false;
    };
    ctx.rep.check(
        Check::info(
            CheckId::IosSimShared,
            format!(
                "{name} ({udid}) left running: icm booted it for another project ({OWNER_ENV} {owner}), which may still use it; `icm stop --shutdown` there shuts it down"
            ),
        )
        .fix(
            "Leave it to the project that booted it; to shut down this simulator anyway, and stop any app on it:",
            &[&format!("xcrun simctl shutdown {udid}")],
        ),
    );
    true
}
