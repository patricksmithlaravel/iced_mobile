//! The web release pipeline (design §11.3, §9.5): a static site for any
//! host.
//!
//! **Not implemented in this build**: every entry point fails with
//! `usage.not_implemented`. What it is to do, through [`Release`] (the
//! contract is in `release/mod.rs`):
//!
//! - preconditions: the wasm32 target; the wasm-bindgen CLI of the app's
//!   lock; wasm-opt through `crate::pinned::require` (it downloads only
//!   with `--yes`); Chrome for the serve smoke;
//! - build: the size-optimized release profile in the dedicated release
//!   target dir, wasm-bindgen, `wasm-opt -Oz` with the features rustc
//!   reports, content-hashed names, the release `index.html` with an
//!   explicit `module_or_path`, `_headers`, `404.html`, `.nojekyll`, the
//!   manifest and icons, `THIRD_PARTY_NOTICES` in the site, the site
//!   directory (`upload`) and its deterministic zip, the gates through
//!   [`Release::check`] (`web.size_budget`, `.fonts_embedded`,
//!   `.renderer_fallback`, `.hashed_assets`, `.mime`, `.serve_smoke`), and
//!   [`super::owner_plans::web`];
//! - verify: the same gates on a site directory, or on the deployed host
//!   with `--url`.

use super::verify::Verify;
use super::{Pipeline, Release, not_implemented};
use crate::cli::ReleaseTarget;
use crate::context::Ctx;
use crate::error::Result;
use crate::plan::Plan;

/// The web pipeline.
pub struct Web;

const WHAT: &str = "the static site";

impl Pipeline for Web {
    fn plan(&self, _ctx: &Ctx, _rel: &Release) -> Result<Plan> {
        Err(not_implemented(ReleaseTarget::Web, WHAT))
    }

    fn preconditions(&self, _ctx: &mut Ctx, _rel: &mut Release) -> Result<()> {
        Err(not_implemented(ReleaseTarget::Web, WHAT))
    }

    fn build(&self, _ctx: &mut Ctx, _rel: &mut Release) -> Result<()> {
        Err(not_implemented(ReleaseTarget::Web, WHAT))
    }

    fn verify(&self, _ctx: &mut Ctx, _verify: &mut Verify) -> Result<()> {
        Err(not_implemented(
            ReleaseTarget::Web,
            "the site gates of `icm verify web`",
        ))
    }
}
