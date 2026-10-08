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
//! simulator running when its tag names another project, or when the tag
//! cannot be read ([`keep_running`]); one without a tag (booted outside
//! icm, or by an icm before this) is still shut down. A run that cannot
//! write the tag says so ([`claim`]).

use crate::catalogue::CheckId;
use crate::context::{Ctx, Project};
use crate::error::Check;
use crate::managed::{Owner, failure};
use crate::tools::Xcode;
use std::time::Duration;

/// The launchd variable that names the project icm booted a simulator for.
pub const OWNER_ENV: &str = "ICM_BOOTED_BY";

/// This project's tag.
pub fn tag(project: &Project) -> String {
    crate::android::session::owner_tag(project)
}

/// Marks a simulator icm has just booted as this project's. When that
/// fails, the simulator stays unmarked, as if booted outside icm, and
/// another project's `stop --shutdown` would shut it down under this run's
/// app: a WARN `ios.sim.owner_unknown` says so.
pub fn claim(ctx: &Ctx, xcode: &Xcode, project: &Project, name: &str, udid: &str) {
    let tag = tag(project);
    let why = match ctx.probe(
        &xcode
            .xcrun()
            .args(["simctl", "spawn", udid, "launchctl", "setenv", OWNER_ENV])
            .arg(&tag)
            .timeout(Duration::from_secs(30)),
    ) {
        Ok(outcome) if outcome.success() => return,
        Ok(outcome) => failure(&outcome),
        Err(error) => error.detail,
    };
    ctx.rep.check(
        Check::warn(
            CheckId::IosSimOwnerUnknown,
            format!(
                "could not mark {name} ({udid}) as this project's ({OWNER_ENV}, `simctl spawn launchctl setenv` {why}): another project's `icm stop --shutdown` may shut it down while this app runs"
            ),
        )
        .fix(
            "Mark it once the simulator answers (a later run on it does not), or give this project its own simulator (`--sim`, or host.toml [ios] simulator_udid):",
            &[&format!(
                "xcrun simctl spawn {udid} launchctl setenv {OWNER_ENV} {tag}"
            )],
        ),
    );
}

/// Whose a booted simulator is, by its [`OWNER_ENV`] (`simctl getenv`
/// prints nothing on stdout and exits 0 for an unset variable, and fails
/// for a simulator it cannot ask).
pub fn owner(ctx: &Ctx, xcode: &Xcode, udid: &str) -> Owner {
    match ctx.probe(
        &xcode
            .xcrun()
            .args(["simctl", "getenv", udid, OWNER_ENV])
            .timeout(Duration::from_secs(30)),
    ) {
        Ok(outcome) => Owner::from_query(&outcome),
        Err(error) => Owner::Unknown(error.detail),
    }
}

/// Whether simctl lists the simulator `udid` as booted (`None` when it
/// cannot tell).
fn booted(ctx: &Ctx, xcode: &Xcode, udid: &str) -> Option<bool> {
    let outcome = ctx
        .probe(
            &xcode
                .xcrun()
                .args(["simctl", "list", "-j", "devices"])
                .timeout(Duration::from_secs(60)),
        )
        .ok()
        .filter(crate::process::Outcome::success)?;
    let devices = crate::simctl::parse_devices(&outcome.stdout_text()).ok()?;
    let device = devices
        .iter()
        .find(|device| device.udid.eq_ignore_ascii_case(udid))?;
    Some(device.state == "Booted")
}

/// Whether `stop --shutdown` must leave the simulator `name` (`udid`)
/// running, and why ("for another project"): icm booted it for another
/// project, which may still run its app there (INFO `ios.sim.shared`), or
/// its owner cannot be read while it is booted, so it may be (WARN
/// `ios.sim.owner_unknown`). Both come with a fix that names this
/// simulator alone. `None`: this project's, nobody's, or not booted.
pub fn keep_running(
    ctx: &Ctx,
    xcode: &Xcode,
    project: &Project,
    name: &str,
    udid: &str,
) -> Option<&'static str> {
    let anyway = format!("xcrun simctl shutdown {udid}");
    match owner(ctx, xcode, udid) {
        Owner::Nobody => None,
        Owner::Project(owner) if owner == tag(project) => None,
        Owner::Project(owner) => {
            ctx.rep.check(
                Check::info(
                    CheckId::IosSimShared,
                    format!(
                        "{name} ({udid}) left running: icm booted it for another project ({OWNER_ENV} {owner}), which may still use it; `icm stop --shutdown` there shuts it down"
                    ),
                )
                .fix(
                    "Leave it to the project that booted it; to shut down this simulator anyway, and stop any app on it:",
                    &[&anyway],
                ),
            );
            Some("for another project")
        }
        // A simulator that is no longer booted has nothing to keep (and
        // simctl cannot ask one that is not booted).
        Owner::Unknown(_) if booted(ctx, xcode, udid) == Some(false) => None,
        Owner::Unknown(why) => {
            ctx.rep.check(
                Check::warn(
                    CheckId::IosSimOwnerUnknown,
                    format!(
                        "{name} ({udid}) left running: icm could not read which project booted it ({OWNER_ENV}, `simctl getenv` {why}), and another may still use it"
                    ),
                )
                .fix(
                    "Rerun `icm stop --shutdown` once the simulator answers; to shut down this simulator anyway, and stop any app on it:",
                    &[&anyway],
                ),
            );
            Some("because icm could not read which project booted it")
        }
    }
}
