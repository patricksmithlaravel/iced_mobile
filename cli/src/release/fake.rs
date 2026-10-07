//! A stand-in pipeline for icm's own tests (`icm __test release|verify
//! <target>`): it writes a small file where the real pipeline's upload
//! artifact would go, so the release core (preconditions, gates, dist,
//! `artifacts.json`, `UPLOAD.md`, `upload.sh`, `dist/latest`, the ledger,
//! verify) runs end to end without a toolchain, a device or a key.

use super::upload::OwnerPlan;
use super::verify::Verify;
use super::{Pipeline, Release, owner_plans};
use crate::catalogue::CheckId;
use crate::cli::{ReleaseTarget, SignMode};
use crate::context::Ctx;
use crate::error::{Check, IcmError, Result};
use crate::plan::{Plan, Step};
use std::path::PathBuf;

/// The stand-in pipeline.
pub struct Fake;

/// One release cargo build of the project's binary for a target
/// (`icm __test release-build`): the profile, `--config`, `--locked`, the
/// dedicated target directory and, with `min_os`, the deployment-target
/// stamp, as a real pipeline's first step runs them.
pub fn release_build(ctx: &mut Ctx, target: ReleaseTarget, min_os: Option<&str>) -> Result<()> {
    let project = ctx.project()?.clone();
    let args = crate::cli::ReleaseArgs {
        target,
        sign: SignMode::None,
        allow_dirty: true,
        no_smoke: false,
        apk: false,
        dmg: false,
        universal: false,
        via_xcode_export: false,
    };
    let mut rel = Release::new(&project, &args)?;
    let triple = match target {
        ReleaseTarget::Ios => Some("aarch64-apple-ios"),
        ReleaseTarget::Android => Some("aarch64-linux-android"),
        ReleaseTarget::Web => Some("wasm32-unknown-unknown"),
        ReleaseTarget::Windows => Some("x86_64-pc-windows-msvc"),
        ReleaseTarget::Macos | ReleaseTarget::Linux => None,
    };
    let bin = project.bin_for(super::ledger::platform_key(target))?;
    let invocation = rel.invocation("build", crate::cargo::Select::Bin(bin), triple);
    let _ = rel.cargo(ctx, "cargo.build", &invocation, &[], min_os)?;
    ctx.rep.set(
        "cargo_target_dir",
        serde_json::json!(crate::paths::display(&rel.cargo_target_dir())),
    );
    ctx.rep.set("tools", serde_json::json!(rel.tools));
    Ok(())
}

/// The upload file's name and kind for a target.
fn upload_file(rel: &Release) -> (String, &'static str) {
    let name = &rel.config().app.name;
    let version = &rel.version;
    match rel.target {
        ReleaseTarget::Ios => (format!("{name}.ipa"), "ipa"),
        ReleaseTarget::Android => (
            format!("{}-{version}-{}.aab", rel.package.name, rel.build),
            "aab",
        ),
        ReleaseTarget::Web => ("site".to_string(), "site"),
        ReleaseTarget::Macos => (format!("{name}-{version}.app.zip"), "app_zip"),
        ReleaseTarget::Windows => (format!("{name}-{version}.msi"), "msi"),
        ReleaseTarget::Linux => (
            format!("{}_{version}-{}_amd64.deb", rel.package.name, rel.build),
            "deb",
        ),
    }
}

fn io(path: &std::path::Path, error: std::io::Error) -> IcmError {
    IcmError::new(
        CheckId::InternalBug,
        format!("cannot write {}: {error}", crate::paths::display(path)),
    )
}

impl Pipeline for Fake {
    fn plan(&self, _ctx: &Ctx, rel: &Release) -> Result<Plan> {
        let mut plan = Plan::new();
        plan.push(Step::internal(
            "fake.build",
            &format!("write a stand-in {} artifact", rel.target.as_str()),
        ));
        Ok(plan)
    }

    fn preconditions(&self, _ctx: &mut Ctx, rel: &mut Release) -> Result<()> {
        rel.tool("fake", "1");
        Ok(())
    }

    fn build(&self, ctx: &mut Ctx, rel: &mut Release) -> Result<()> {
        let (name, kind) = upload_file(rel);
        let path: PathBuf = rel.dist.join(&name);
        let content = format!(
            "icm stand-in {} release of {} {}+{}\n",
            rel.target.as_str(),
            rel.config().app.id,
            rel.version,
            rel.build
        );
        // The notices go where the real pipeline puts them: inside the
        // bundle for zips and the site, declared for the rest.
        let triple = match rel.target {
            ReleaseTarget::Ios => "aarch64-apple-ios",
            ReleaseTarget::Android => "aarch64-linux-android",
            ReleaseTarget::Web => "wasm32-unknown-unknown",
            ReleaseTarget::Macos => crate::toolchain::host_triple(),
            ReleaseTarget::Windows => "x86_64-pc-windows-msvc",
            ReleaseTarget::Linux => "x86_64-unknown-linux-gnu",
        };
        let notices = rel.notices(ctx, Some(triple))?;
        let app = &rel.config().app.name;
        let inner = match rel.target {
            ReleaseTarget::Ios => format!("Payload/{app}.app/{}", super::notices::FILE),
            ReleaseTarget::Android => format!("base/assets/{}", super::notices::FILE),
            ReleaseTarget::Macos => {
                format!("{app}.app/Contents/Resources/{}", super::notices::FILE)
            }
            ReleaseTarget::Web => super::notices::FILE.to_string(),
            ReleaseTarget::Windows | ReleaseTarget::Linux => {
                format!("doc/{}", super::notices::FILE)
            }
        };
        match kind {
            "site" => {
                std::fs::create_dir_all(&path).map_err(|e| io(&path, e))?;
                std::fs::write(path.join("index.html"), &content)
                    .map_err(|e| io(&path.join("index.html"), e))?;
                std::fs::copy(&notices, path.join(&inner)).map_err(|e| io(&path, e))?;
            }
            "ipa" | "aab" | "app_zip" => {
                use crate::android::zip::{Entry, Source, write};
                write(
                    &path,
                    &[
                        Entry {
                            name: "stand-in.txt".to_string(),
                            source: Source::Bytes(content.into_bytes()),
                        },
                        Entry {
                            name: inner.clone(),
                            source: Source::File(notices.clone()),
                        },
                    ],
                )
                .map_err(|e| io(&path, e))?;
            }
            _ => std::fs::write(&path, &content).map_err(|e| io(&path, e))?,
        }
        rel.embed_notices(&path, &inner)?;
        rel.add_file("upload", kind, &path)?;
        rel.signed = rel.sign() == SignMode::Auto;
        rel.check(
            ctx,
            Check::pass(
                CheckId::ReleaseArtifactChanged,
                format!("{name}: the stand-in artifact is written"),
            ),
        );

        let common = rel.common();
        let plan: OwnerPlan = match rel.target {
            ReleaseTarget::Ios => owner_plans::ios(&common, &name),
            ReleaseTarget::Android => owner_plans::android(&common, &name, None, None),
            ReleaseTarget::Web => owner_plans::web(&common, &name),
            ReleaseTarget::Macos => {
                owner_plans::macos_app(&common, &name, &format!("{}.app", rel.config().app.name))
            }
            ReleaseTarget::Windows | ReleaseTarget::Linux => {
                owner_plans::desktop(&common, &[name.as_str()])
            }
        };
        rel.owner_plan = Some(plan);
        Ok(())
    }

    fn verify(&self, ctx: &mut Ctx, verify: &mut Verify) -> Result<()> {
        if let Some(artifact) = verify.artifact.clone() {
            let check = if artifact.exists() {
                Check::pass(
                    CheckId::ReleaseArtifactChanged,
                    format!("{} exists", crate::paths::display(&artifact)),
                )
            } else {
                Check::fail(
                    CheckId::ReleaseArtifactChanged,
                    format!("{} is gone", crate::paths::display(&artifact)),
                )
            };
            verify.check(ctx, check);
        }
        Ok(())
    }
}
