//! `icm verify <target> [--artifact <path>]` (design §6 `verify`, §12):
//! the store gates on an existing artifact.
//!
//! The artifact is `--artifact`, else the upload file of
//! `dist/latest/<target>`. When an `artifacts.json` sits next to it, the
//! release's severities apply (an unsigned `--sign none` artifact verifies
//! with WARNs) and every file it lists is checked against its recorded
//! size and sha256 (`release.artifact_changed`). An artifact built
//! elsewhere gets every gate at full severity. The target's own gates are
//! [`super::Pipeline::verify`].

use super::gates::Gates;
use super::manifest::{self, Manifest};
use super::{Pipeline, dist, pipeline};
use crate::catalogue::CheckId;
use crate::cli::{ReleaseTarget, SignMode, VerifyArgs};
use crate::context::{Ctx, Project};
use crate::error::{Check, Evidence, IcmError, Result};
use serde_json::json;
use std::path::PathBuf;

/// One verify in progress.
pub struct Verify {
    /// The target.
    pub target: ReleaseTarget,
    /// The gates, with the release's severities.
    pub gates: Gates,
    /// The artifact (none for `verify web --url`).
    pub artifact: Option<PathBuf>,
    /// The release's dist directory, when its `artifacts.json` is known.
    pub dir: Option<PathBuf>,
    /// The release's `artifacts.json`.
    pub manifest: Option<Manifest>,
    /// macOS: also check notarization and Gatekeeper.
    pub after_notarize: bool,
    /// Web: the deployed site to check instead of the files.
    pub url: Option<String>,
    /// The project, when verify runs inside one.
    pub project: Option<Project>,
}

impl Verify {
    /// Reports a gate (see [`Gates::check`]).
    pub fn check(&mut self, ctx: &Ctx, check: Check) {
        self.gates.check(ctx, check);
    }
}

/// Runs `icm verify`.
pub fn run(ctx: &mut Ctx, args: &VerifyArgs) -> Result<()> {
    run_with(ctx, args, pipeline(args.target))
}

/// Runs verify with a pipeline (`icm __test verify` passes a stand-in).
pub fn run_with(ctx: &mut Ctx, args: &VerifyArgs, pipeline: &dyn Pipeline) -> Result<()> {
    if args.after_notarize && args.target != ReleaseTarget::Macos {
        return Err(bad_flag("--after-notarize", ReleaseTarget::Macos));
    }
    if args.url.is_some() && args.target != ReleaseTarget::Web {
        return Err(bad_flag("--url", ReleaseTarget::Web));
    }
    if let Some(url) = &args.url
        && !(url.starts_with("https://") || url.starts_with("http://"))
    {
        return Err(IcmError::new(
            CheckId::UsageBadArgs,
            format!("--url `{url}` is not an http(s) URL"),
        ));
    }

    let mut verify = locate(ctx, args)?;
    files(ctx, &mut verify);
    notices(ctx, &mut verify);
    ctx.rep.set(
        "verify",
        json!({
            "target": args.target.as_str(),
            "artifact": verify.artifact.as_deref().map(crate::paths::display),
            "manifest": verify.dir.as_deref().map(|dir| crate::paths::display(&dir.join(manifest::FILE))),
            "sign": verify.gates.mode.as_str(),
            "url": verify.url,
        }),
    );
    if let Some(artifact) = verify.artifact.clone() {
        ctx.rep.artifact("artifact", &artifact);
    }

    pipeline.verify(ctx, &mut verify)?;

    ctx.rep.summary(match (&verify.artifact, &verify.url) {
        (_, Some(url)) => format!("verified {url}"),
        (Some(artifact), None) => format!(
            "verified {} ({} pass, {} warn, {} fail)",
            crate::paths::display(artifact),
            verify.gates.tally.pass,
            verify.gates.tally.warn,
            verify.gates.tally.fail
        ),
        (None, None) => "verified".to_string(),
    });
    verify.gates.finish()
}

fn bad_flag(flag: &str, target: ReleaseTarget) -> IcmError {
    IcmError::new(
        CheckId::UsageBadArgs,
        format!("{flag} applies to `icm verify {}` only", target.as_str()),
    )
    .fix("Drop the flag.", &["icm verify --help"])
}

fn not_found(detail: String, target: ReleaseTarget) -> IcmError {
    IcmError::new(CheckId::ReleaseNotFound, detail).fix(
        "Make the release first, or pass the artifact's path.",
        &[
            &format!("icm release {} --json -q", target.as_str()),
            &format!("icm verify {} --artifact <path>", target.as_str()),
        ],
    )
}

/// Finds the artifact and the release that made it.
fn locate(ctx: &mut Ctx, args: &VerifyArgs) -> Result<Verify> {
    let target = args.target;
    let project = ctx.try_project().cloned();
    let mut verify = Verify {
        target,
        gates: Gates::new(SignMode::Auto),
        artifact: None,
        dir: None,
        manifest: None,
        after_notarize: args.after_notarize,
        url: args.url.clone(),
        project: project.clone(),
    };

    let artifact = match (&args.artifact, &args.url) {
        (Some(path), _) => {
            let path = std::path::absolute(path).unwrap_or_else(|_| path.clone());
            if !path.exists() {
                return Err(not_found(
                    format!("{} does not exist", crate::paths::display(&path)),
                    target,
                ));
            }
            Some(path)
        }
        (None, Some(_)) => None,
        (None, None) => {
            // Inside a project: the newest release's upload file.
            let project = ctx.project()?.clone();
            let Some(dir) = dist::resolve_latest(&project, target.as_str()) else {
                return Err(not_found(
                    format!(
                        "there is no {} release in {}",
                        target.as_str(),
                        crate::paths::display(&dist::latest(&project, target.as_str()))
                    ),
                    target,
                ));
            };
            let manifest = Manifest::read(&dir.join(manifest::FILE))?;
            let Some(upload) = manifest.uploads().next() else {
                return Err(not_found(
                    format!(
                        "{} lists no upload file",
                        crate::paths::display(&dir.join(manifest::FILE))
                    ),
                    target,
                ));
            };
            Some(upload.absolute(&dir))
        }
    };

    // The release that made it: artifacts.json next to it, listing it.
    if let Some(artifact) = &artifact
        && let Some(dir) = artifact.parent()
    {
        let path = dir.join(manifest::FILE);
        if path.is_file() {
            let manifest = Manifest::read(&path)?;
            let listed = manifest
                .files
                .iter()
                .any(|file| &file.absolute(dir) == artifact);
            if listed {
                if manifest.target != target.as_str() {
                    return Err(IcmError::new(
                        CheckId::UsageBadArgs,
                        format!(
                            "{} is a {} release, not {}",
                            crate::paths::display(artifact),
                            manifest.target,
                            target.as_str()
                        ),
                    )
                    .evidence(Evidence::file(&path)));
                }
                verify.gates = Gates::new(if manifest.sign == "none" {
                    SignMode::None
                } else {
                    SignMode::Auto
                });
                verify.dir = Some(dir.to_path_buf());
                verify.manifest = Some(manifest);
            }
        }
    }
    verify.artifact = artifact;
    Ok(verify)
}

/// `release.notices`: the places `artifacts.json` records, or, for an
/// artifact built elsewhere, a THIRD_PARTY_NOTICES anywhere inside it (when
/// icm can look inside).
fn notices(ctx: &Ctx, verify: &mut Verify) {
    use super::notices::{self, Presence};
    if let (Some(dir), Some(manifest)) = (verify.dir.clone(), verify.manifest.clone()) {
        for check in notices::checks(&dir, &manifest.notices) {
            verify.check(ctx, check);
        }
        return;
    }
    let Some(artifact) = verify.artifact.clone() else {
        return;
    };
    let shown = crate::paths::display(&artifact);
    let check = match notices::find_any(&artifact) {
        Presence::Present => Check::pass(
            CheckId::ReleaseNotices,
            format!("{shown} carries {}", notices::FILE),
        ),
        Presence::Missing => Check::fail(
            CheckId::ReleaseNotices,
            format!("{shown} carries no {}", notices::FILE),
        )
        .evidence(Evidence::file(&artifact)),
        Presence::Unknown => Check::skip(
            CheckId::ReleaseNotices,
            format!("icm does not look inside {shown} for {}", notices::FILE),
        ),
    };
    verify.check(ctx, check);
}

/// `release.artifact_changed`: every file `artifacts.json` lists still has
/// its recorded size and sha256.
fn files(ctx: &Ctx, verify: &mut Verify) {
    let (Some(dir), Some(manifest)) = (verify.dir.clone(), verify.manifest.clone()) else {
        if verify.artifact.is_some() {
            ctx.rep.progress(
                "no artifacts.json lists this artifact: every gate runs at full severity",
            );
        }
        return;
    };
    for file in &manifest.files {
        let path = file.absolute(&dir);
        let check = match dist::digest(&path) {
            Ok((bytes, sha256)) if bytes == file.bytes && sha256 == file.sha256 => Check::pass(
                CheckId::ReleaseArtifactChanged,
                format!("{}: {bytes} bytes, sha256 as recorded", file.path),
            ),
            Ok((bytes, sha256)) => Check::fail(
                CheckId::ReleaseArtifactChanged,
                format!(
                    "{} changed since the release: {bytes} bytes with sha256 {sha256}, recorded {} with {}",
                    file.path, file.bytes, file.sha256
                ),
            )
            .evidence(Evidence::file(&path)),
            Err(error) => Check::fail(
                CheckId::ReleaseArtifactChanged,
                format!("{} is gone or unreadable: {error}", file.path),
            )
            .evidence(Evidence::file(dir.join(manifest::FILE))),
        };
        verify.check(ctx, check);
    }
}
