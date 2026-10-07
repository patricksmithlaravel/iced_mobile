//! Release builds (Appendix C items 4 and 6), for every pipeline:
//!
//! - **Profiles through `--config`.** Cargo ignores `[profile.*]` in a
//!   workspace member, so icm passes its settings on the command line:
//!   [`profile`] per target (thin LTO; line tables for the targets that
//!   ship symbols; the web's own size-optimized `icm-web` profile).
//! - **A dedicated target directory**, `target/icm/release-target`
//!   ([`Release::cargo_target_dir`]), so dev and release artifacts never
//!   mix and a release never reuses a dev build linked with another
//!   deployment target.
//! - **Deployment-target stamps.** Cargo does not relink when
//!   `IPHONEOS_DEPLOYMENT_TARGET` or `MACOSX_DEPLOYMENT_TARGET` changes, so
//!   [`Release::cargo`] stamps the value per triple and profile in the
//!   release directory and cleans the app package (with the same
//!   `--target-dir`) when it changes.
//! - `--locked`: a release builds exactly the committed `Cargo.lock`.

use super::Release;
use crate::cargo::{Invocation, Select};
use crate::catalogue::CheckId;
use crate::cli::ReleaseTarget;
use crate::context::{CargoOutput, Ctx};
use crate::error::{IcmError, Result};
use std::path::PathBuf;

/// A cargo profile and the `--config` settings that define it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    /// `release`, or a custom profile (`icm-web`).
    pub name: &'static str,
    /// `--config KEY=VALUE` settings.
    pub config: Vec<String>,
}

/// The profile a target's release builds with.
pub fn profile(target: ReleaseTarget) -> Profile {
    let lto = r#"profile.release.lto="thin""#.to_string();
    // Line tables: the iOS dSYM, macOS's dSYM and Android's
    // native-debug-symbols.zip need them (design §11.1, Appendix A 33);
    // the shipped binaries are stripped by the pipelines.
    let lines = r#"profile.release.debug="line-tables-only""#.to_string();
    match target {
        ReleaseTarget::Ios | ReleaseTarget::Macos | ReleaseTarget::Android => Profile {
            name: "release",
            config: vec![lto, lines],
        },
        ReleaseTarget::Windows | ReleaseTarget::Linux => Profile {
            name: "release",
            config: vec![lto],
        },
        ReleaseTarget::Web => Profile {
            name: "icm-web",
            config: [
                r#"profile.icm-web.inherits="release""#,
                r#"profile.icm-web.opt-level="z""#,
                "profile.icm-web.lto=true",
                "profile.icm-web.codegen-units=1",
                "profile.icm-web.debug=false",
            ]
            .iter()
            .map(ToString::to_string)
            .collect(),
        },
    }
}

impl Release {
    /// `target/icm/release-target`: the cargo target directory of release
    /// builds.
    pub fn cargo_target_dir(&self) -> PathBuf {
        self.project.icm_dir.join("release-target")
    }

    /// Where cargo puts a triple's release artifacts (`None`: the host).
    pub fn artifacts_dir(&self, triple: Option<&str>) -> PathBuf {
        crate::cargo::artifacts_dir(&self.cargo_target_dir(), triple, profile(self.target).name)
    }

    /// A cargo invocation of the release's package with the target's
    /// profile, its `--config` settings, `--locked` and the dedicated
    /// target directory.
    pub fn invocation(&self, subcommand: &str, select: Select, triple: Option<&str>) -> Invocation {
        let profile = profile(self.target);
        let mut invocation =
            Invocation::new(subcommand, &self.package.manifest_path, &self.package.name);
        invocation.select = select;
        invocation.triple = triple.map(str::to_string);
        invocation.profile = profile.name.to_string();
        invocation.config = profile.config;
        invocation.locked = true;
        invocation.target_dir = Some(self.cargo_target_dir());
        invocation
    }

    /// Runs a release cargo invocation as a step. For an Apple triple,
    /// `min_os` is the deployment target: it goes into cargo's environment,
    /// and a value different from the last build's relinks the app first.
    /// The tool versions go into `artifacts.json`.
    pub fn cargo(
        &mut self,
        ctx: &Ctx,
        name: &str,
        invocation: &Invocation,
        env: &[(String, String)],
        min_os: Option<&str>,
    ) -> Result<CargoOutput> {
        let mut env = env.to_vec();
        let mut stamp = None;
        if let Some(min_os) = min_os {
            let target_dir = self.cargo_target_dir();
            if let Some((pair, prepared)) = ctx.deployment_target_in(
                &self.project,
                &invocation.package,
                invocation.triple.as_deref(),
                &invocation.profile,
                min_os,
                Some(&target_dir),
            )? {
                env.push(pair);
                stamp = Some(prepared);
            }
        }
        if let Ok(toolchain) = crate::toolchain::active(self.project.dir()) {
            self.tool("rustc", toolchain.rustc_version());
        }

        let output = ctx.cargo(name, invocation, &env)?;
        if let Some(stamp) = stamp {
            stamp.write().map_err(|error| {
                IcmError::new(
                    CheckId::InternalBug,
                    format!(
                        "cannot write {}: {error}",
                        crate::paths::display(&stamp.path)
                    ),
                )
            })?;
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_target_has_a_profile() {
        for target in ReleaseTarget::ALL {
            let profile = profile(target);
            assert!(!profile.config.is_empty(), "{target:?}");
            for setting in &profile.config {
                assert!(
                    setting.starts_with(&format!("profile.{}.", profile.name)),
                    "{target:?}: {setting}"
                );
            }
        }
        let ios = profile(ReleaseTarget::Ios);
        assert_eq!(ios.name, "release");
        assert!(
            ios.config
                .contains(&r#"profile.release.debug="line-tables-only""#.to_string())
        );
        let web = profile(ReleaseTarget::Web);
        assert_eq!(web.name, "icm-web");
        assert!(
            web.config
                .contains(&r#"profile.icm-web.inherits="release""#.to_string())
        );
        assert!(
            web.config
                .contains(&r#"profile.icm-web.opt-level="z""#.to_string())
        );
    }

    #[test]
    fn invocations_carry_the_profile_lock_and_target_dir() {
        let mut invocation = Invocation::new("build", std::path::Path::new("/p/Cargo.toml"), "app");
        let profile = profile(ReleaseTarget::Web);
        invocation.profile = profile.name.to_string();
        invocation.config = profile.config;
        invocation.locked = true;
        invocation.target_dir = Some(PathBuf::from("/p/target/icm/release-target"));
        invocation.triple = Some("wasm32-unknown-unknown".into());
        let args = invocation.args().join(" ");
        assert!(
            args.starts_with("build --config profile.icm-web.inherits=\"release\""),
            "{args}"
        );
        assert!(args.contains("--profile icm-web"), "{args}");
        assert!(args.contains("--locked"), "{args}");
        assert!(
            args.contains("--target-dir /p/target/icm/release-target"),
            "{args}"
        );
    }
}
