//! The iOS release pipeline (design §11.1, §12.2, §9.1-§9.3): an `.ipa`
//! for App Store Connect, and `icm diagnose altool`.
//!
//! **Not implemented in this build**: every entry point fails with
//! `usage.not_implemented`. What it is to do, through [`Release`] (the
//! contract is in `release/mod.rs`):
//!
//! - preconditions: a macOS host; Xcode at or above the policy's
//!   `app_store.min_sdk`, not a beta (`ios.xcode.not_beta`, WARN); an
//!   Apple Distribution identity (`ios.sign.no_identity`) and an App Store
//!   profile (`ios.sign.no_profile`, `.profile_expired`,
//!   `.profile_mismatch`) through [`Release::needs_owner`], searching
//!   host.toml `signing_keychain` / `ICM_KEYCHAIN` when set;
//! - build: `aarch64-apple-ios` in the dedicated release target dir with
//!   `IPHONEOS_DEPLOYMENT_TARGET` stamped, a dSYM, the Mach-O and privacy
//!   gates, the flattened icon through actool, the device Info.plist with
//!   DT keys, the distribution entitlements, `embedded.mobileprovision`,
//!   `THIRD_PARTY_NOTICES` in the bundle root, sign last under the 60 s
//!   keychain watchdog, the `.ipa` with `zip -X`, the §12.2 gates through
//!   [`Release::check`], [`Release::add_file`] for the `.ipa` (`upload`)
//!   and the dSYM zip (`symbols`), and [`super::owner_plans::ios`];
//! - verify: the §12.2 gates on an existing `.ipa`;
//! - diagnose: altool's JSON (errors, the delivery id) to catalogue ids.

use super::diagnose::Input;
use super::verify::Verify;
use super::{Pipeline, Release, not_implemented};
use crate::cli::ReleaseTarget;
use crate::context::Ctx;
use crate::error::Result;
use crate::plan::Plan;

/// The iOS pipeline.
pub struct Ios;

impl Pipeline for Ios {
    fn plan(&self, _ctx: &Ctx, _rel: &Release) -> Result<Plan> {
        Err(not_implemented(
            ReleaseTarget::Ios,
            "the .ipa for App Store Connect",
        ))
    }

    fn preconditions(&self, _ctx: &mut Ctx, _rel: &mut Release) -> Result<()> {
        Err(not_implemented(
            ReleaseTarget::Ios,
            "the .ipa for App Store Connect",
        ))
    }

    fn build(&self, _ctx: &mut Ctx, _rel: &mut Release) -> Result<()> {
        Err(not_implemented(
            ReleaseTarget::Ios,
            "the .ipa for App Store Connect",
        ))
    }

    fn verify(&self, _ctx: &mut Ctx, _verify: &mut Verify) -> Result<()> {
        Err(not_implemented(
            ReleaseTarget::Ios,
            "the App Store gates of `icm verify ios`",
        ))
    }
}

/// `icm diagnose altool`.
pub fn diagnose(_ctx: &mut Ctx, _input: &Input) -> Result<()> {
    Err(not_implemented(ReleaseTarget::Ios, "`icm diagnose altool`"))
}
