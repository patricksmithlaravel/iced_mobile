//! The Linux release pipeline (design §11.6, §9.6): a `.deb` and an
//! AppImage.
//!
//! **Not implemented in this build**: every entry point fails with
//! `usage.not_implemented`. What it is to do, through [`Release`] (the
//! contract is in `release/mod.rs`):
//!
//! - preconditions: `[desktop.linux] maintainer` for a `.deb` (an owner
//!   decision); dpkg-deb and dpkg-shlibdeps; appimagetool and the AppImage
//!   runtime through `crate::pinned::require` (they download only with
//!   `--yes`);
//! - build: the release build (an ubuntu:22.04 container in CI) in the
//!   dedicated release target dir, `linux.glibc_floor` from the ELF's
//!   version needs, the `.deb` (Depends from dpkg-shlibdeps, the
//!   Recommends for dlopened libraries, `THIRD_PARTY_NOTICES` under
//!   `usr/share/doc/<package>/`), the AppDir with `AppRun` and the bundled
//!   libraries, the AppImage with `--runtime-file`, both `upload`, the
//!   gates (`linux.desktop_file`, `linux.deb.lint` WARN) through
//!   [`Release::check`], and [`super::owner_plans::desktop`];
//! - verify: `linux.glibc_floor` and the package gates.
//!
//! Its real-host acceptance runs on a Linux runner (design §17).

use super::verify::Verify;
use super::{Pipeline, Release, not_implemented};
use crate::cli::ReleaseTarget;
use crate::context::Ctx;
use crate::error::Result;
use crate::plan::Plan;

/// The Linux pipeline.
pub struct Linux;

const WHAT: &str = "the .deb and AppImage";

impl Pipeline for Linux {
    fn plan(&self, _ctx: &Ctx, _rel: &Release) -> Result<Plan> {
        Err(not_implemented(ReleaseTarget::Linux, WHAT))
    }

    fn preconditions(&self, _ctx: &mut Ctx, _rel: &mut Release) -> Result<()> {
        Err(not_implemented(ReleaseTarget::Linux, WHAT))
    }

    fn build(&self, _ctx: &mut Ctx, _rel: &mut Release) -> Result<()> {
        Err(not_implemented(ReleaseTarget::Linux, WHAT))
    }

    fn verify(&self, _ctx: &mut Ctx, _verify: &mut Verify) -> Result<()> {
        Err(not_implemented(
            ReleaseTarget::Linux,
            "the gates of `icm verify linux`",
        ))
    }
}
