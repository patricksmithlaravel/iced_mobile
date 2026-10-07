//! The Windows release pipeline (design §11.5, §9.6, Appendix C item 20):
//! an `.msi` and an NSIS `.exe`.
//!
//! **Not implemented in this build**: every entry point fails with
//! `usage.not_implemented`. What it is to do, through [`Release`] (the
//! contract is in `release/mod.rs`):
//!
//! - preconditions: the Windows SDK's `rc.exe` (`windows.sdk_missing`),
//!   WiX v5 and makensis; `windows.msi_version` (exit 3). The core already
//!   checked `[desktop.windows] sign_command` and its `sign_env`
//!   variables;
//! - build: `app.rc` with the icon and VERSIONINFO, the exe linked with
//!   its `.res` and the static C runtime in the dedicated release target
//!   dir (a gate reads its PE imports), `sign_command` with `{file}`, the
//!   `.wxs` and `.nsi` with `THIRD_PARTY_NOTICES` installed next to the
//!   exe, both installers (`upload`) and signed (`windows.signed`), and
//!   [`super::owner_plans::desktop`];
//! - verify: `windows.signed` and the PE gates.
//!
//! Its real-host acceptance runs on a Windows runner (design §17).

use super::verify::Verify;
use super::{Pipeline, Release, not_implemented};
use crate::cli::ReleaseTarget;
use crate::context::Ctx;
use crate::error::Result;
use crate::plan::Plan;

/// The Windows pipeline.
pub struct Windows;

const WHAT: &str = "the .msi and NSIS installers";

impl Pipeline for Windows {
    fn plan(&self, _ctx: &Ctx, _rel: &Release) -> Result<Plan> {
        Err(not_implemented(ReleaseTarget::Windows, WHAT))
    }

    fn preconditions(&self, _ctx: &mut Ctx, _rel: &mut Release) -> Result<()> {
        Err(not_implemented(ReleaseTarget::Windows, WHAT))
    }

    fn build(&self, _ctx: &mut Ctx, _rel: &mut Release) -> Result<()> {
        Err(not_implemented(ReleaseTarget::Windows, WHAT))
    }

    fn verify(&self, _ctx: &mut Ctx, _verify: &mut Verify) -> Result<()> {
        Err(not_implemented(
            ReleaseTarget::Windows,
            "the gates of `icm verify windows`",
        ))
    }
}
