//! The macOS release pipeline (design §11.4, §9.6): a signed `.app` in
//! two stages around the owner's notarization, and `icm diagnose
//! notarytool`.
//!
//! **Not implemented in this build**: every entry point fails with
//! `usage.not_implemented`. What it is to do, through [`Release`] (the
//! contract is in `release/mod.rs`):
//!
//! - preconditions: a macOS host; a Developer ID Application identity
//!   (`macos.sign.no_developer_id` through [`Release::needs_owner`],
//!   searching host.toml `signing_keychain` / `ICM_KEYCHAIN` when set);
//!   with `--universal`, the x86_64 target;
//! - stage 1: the build with `MACOSX_DEPLOYMENT_TARGET` stamped in the
//!   dedicated release target dir, `lipo` for `--universal`, the dSYM, the
//!   `.app` with its Info.plist, icns and `THIRD_PARTY_NOTICES` in
//!   `Contents/Resources`, hardened-runtime signing, the gates through
//!   [`Release::check`] (`macos.sign.verify`, `.hardened_runtime`,
//!   `.min_os`), the zip for the notary service (`upload`), and
//!   [`super::owner_plans::macos_app`];
//! - stage 2 (`--dmg`, [`super::Pipeline::keeps_dist`]): `macos.not_stapled`
//!   on the stapled app, `hdiutil` with an `Applications` link, the signed
//!   DMG, [`super::owner_plans::macos_dmg`];
//! - verify: the gates on an `.app` or `.dmg`; `--after-notarize` adds
//!   spctl and stapler validation (`macos.gatekeeper`);
//! - diagnose: notarytool's JSON and log to catalogue ids.

use super::diagnose::Input;
use super::verify::Verify;
use super::{Pipeline, Release, not_implemented};
use crate::cli::ReleaseTarget;
use crate::context::Ctx;
use crate::error::Result;
use crate::plan::Plan;

/// The macOS pipeline.
pub struct Macos;

const WHAT: &str = "the signed .app and .dmg";

impl Pipeline for Macos {
    fn plan(&self, _ctx: &Ctx, _rel: &Release) -> Result<Plan> {
        Err(not_implemented(ReleaseTarget::Macos, WHAT))
    }

    fn preconditions(&self, _ctx: &mut Ctx, _rel: &mut Release) -> Result<()> {
        Err(not_implemented(ReleaseTarget::Macos, WHAT))
    }

    fn keeps_dist(&self, rel: &Release) -> bool {
        rel.args.dmg
    }

    fn build(&self, _ctx: &mut Ctx, _rel: &mut Release) -> Result<()> {
        Err(not_implemented(ReleaseTarget::Macos, WHAT))
    }

    fn verify(&self, _ctx: &mut Ctx, _verify: &mut Verify) -> Result<()> {
        Err(not_implemented(
            ReleaseTarget::Macos,
            "the gates of `icm verify macos`",
        ))
    }
}

/// `icm diagnose notarytool`.
pub fn diagnose(_ctx: &mut Ctx, _input: &Input) -> Result<()> {
    Err(not_implemented(
        ReleaseTarget::Macos,
        "`icm diagnose notarytool`",
    ))
}
