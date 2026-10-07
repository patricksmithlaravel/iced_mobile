//! The Android release pipeline (design §11.2, §12.3, §9.4): an `.aab`
//! for Google Play, `--apk` for sideloading, and `icm diagnose play`.
//!
//! **Not implemented in this build**: every entry point fails with
//! `usage.not_implemented`. What it is to do, through [`Release`] (the
//! contract is in `release/mod.rs`):
//!
//! - preconditions: `[android] target_sdk` at or above the policy's
//!   `play.target_sdk`; bundletool through `crate::pinned::require` (it
//!   downloads only with `--yes`); a JDK 17+; the NDK. The core already
//!   checked `[android.signing] upload`, its keystore and its password
//!   variables (deferred: the unsigned bundle is still built, then exit
//!   9);
//! - build: every `[android] abis` entry in the dedicated release target
//!   dir, `native-debug-symbols.zip` (`symbols`), the proto link, base.zip
//!   with `THIRD_PARTY_NOTICES` under `assets/`, BundleConfig with
//!   `PAGE_ALIGNMENT_16K`, `bundletool build-bundle` and `validate`, the
//!   jarsigner signature with `-storepass:env`/`-keypass:env` and its
//!   parsed verification, the §12.3 gates through [`Release::check`] on the
//!   linked artifact, the smoke install unless `--no-smoke`, `--apk`
//!   (`sideload`), `play-icon-512.png` (`listing`), and
//!   [`super::owner_plans::android`] ([`super::owner_plans::android_sign`]
//!   when the bundle stays unsigned);
//! - verify: the §12.3 gates on an existing `.aab`;
//! - diagnose: Google Play's upload errors to catalogue ids.

use super::diagnose::Input;
use super::verify::Verify;
use super::{Pipeline, Release, not_implemented};
use crate::cli::ReleaseTarget;
use crate::context::Ctx;
use crate::error::Result;
use crate::plan::Plan;

/// The Android pipeline.
pub struct Android;

const WHAT: &str = "the .aab for Google Play";

impl Pipeline for Android {
    fn plan(&self, _ctx: &Ctx, _rel: &Release) -> Result<Plan> {
        Err(not_implemented(ReleaseTarget::Android, WHAT))
    }

    fn preconditions(&self, _ctx: &mut Ctx, _rel: &mut Release) -> Result<()> {
        Err(not_implemented(ReleaseTarget::Android, WHAT))
    }

    fn build(&self, _ctx: &mut Ctx, _rel: &mut Release) -> Result<()> {
        Err(not_implemented(ReleaseTarget::Android, WHAT))
    }

    fn verify(&self, _ctx: &mut Ctx, _verify: &mut Verify) -> Result<()> {
        Err(not_implemented(
            ReleaseTarget::Android,
            "the Google Play gates of `icm verify android`",
        ))
    }
}

/// `icm diagnose play`.
pub fn diagnose(_ctx: &mut Ctx, _input: &Input) -> Result<()> {
    Err(not_implemented(
        ReleaseTarget::Android,
        "`icm diagnose play`",
    ))
}
