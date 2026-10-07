//! The project's active Rust toolchain and its targets (Appendix C item 8).
//!
//! Targets are checked against the toolchain the *project* resolves
//! (`rust-toolchain.toml` included), not icm's own: the template's pinned
//! `1.98.0` lacks the Android and wasm targets that `stable` has here. Every
//! child runs with `RUSTUP_AUTO_INSTALL=0`, so a missing toolchain is
//! reported (`env.rust_toolchain_missing`) instead of downloaded in the
//! background; `icm doctor --fix --yes` installs it.

use crate::catalogue::CheckId;
use crate::config::Abi;
use crate::error::{Check, IcmError};
use crate::process::{self, Cmd};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The toolchain a directory resolves.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Toolchain {
    /// The rustup toolchain name (`1.98.0-aarch64-apple-darwin`), when
    /// rustup manages it.
    pub name: Option<String>,
    /// Why rustup chose it (`overridden by '<dir>/rust-toolchain.toml'`).
    pub reason: Option<String>,
    /// `rustc --print sysroot`.
    pub sysroot: PathBuf,
    /// `rustc --version`, e.g. `rustc 1.98.0 (88d9e12ae 2026-08-18)`.
    pub rustc: String,
}

impl Toolchain {
    /// The rustc version number, e.g. `1.98.0`.
    pub fn rustc_version(&self) -> &str {
        self.rustc.split_whitespace().nth(1).unwrap_or("")
    }

    /// The targets whose standard library is installed.
    pub fn installed_targets(&self) -> BTreeSet<String> {
        installed_targets(&self.sysroot)
    }

    /// The command that adds a target to this toolchain.
    pub fn add_target_command(&self, target: &str) -> String {
        match &self.name {
            Some(name) => format!("rustup target add --toolchain {name} {target}"),
            None => format!("rustup target add {target}"),
        }
    }
}

/// Resolves the active toolchain for `dir`.
pub fn active(dir: &Path) -> Result<Toolchain, IcmError> {
    let sysroot = rustc(dir, &["--print", "sysroot"])?;
    let rustc_line = rustc(dir, &["--version"])?;

    let mut name = None;
    let mut reason = None;
    if let Ok(outcome) = process::run(
        &Cmd::tool("rustup")
            .args(["show", "active-toolchain"])
            .cwd(dir)
            .timeout(Duration::from_secs(30)),
        None,
        None,
    ) && outcome.success()
    {
        let text = outcome.stdout_text();
        let line = text.lines().next().unwrap_or("").trim();
        if let Some((first, rest)) = line.split_once(' ') {
            name = Some(first.to_string());
            reason = Some(rest.trim().trim_matches(['(', ')']).to_string());
        } else if !line.is_empty() {
            name = Some(line.to_string());
        }
    }

    Ok(Toolchain {
        name,
        reason,
        sysroot: PathBuf::from(sysroot),
        rustc: rustc_line,
    })
}

fn rustc(dir: &Path, args: &[&str]) -> Result<String, IcmError> {
    let outcome = process::run(
        &Cmd::tool("rustc")
            .args(args)
            .cwd(dir)
            .timeout(Duration::from_secs(60)),
        None,
        None,
    )
    .map_err(|error| {
        IcmError::new(
            CheckId::EnvToolMissing,
            format!("cannot run rustc: {error}"),
        )
        .fix("Install Rust with rustup (https://rustup.rs).", &[])
        .by(crate::catalogue::By::Owner)
    })?;

    if outcome.success() {
        return Ok(outcome.stdout_text().trim().to_string());
    }

    let stderr = outcome.stderr_text();
    if let Some(toolchain) = missing_toolchain(&stderr) {
        return Err(IcmError::new(
            CheckId::EnvRustToolchainMissing,
            format!(
                "the toolchain `{toolchain}` that {} selects is not installed",
                dir.display()
            ),
        )
        .fix_commands([
            "icm doctor --fix --yes".to_string(),
            format!("rustup toolchain install {toolchain}"),
        ]));
    }

    Err(IcmError::new(
        CheckId::EnvToolMissing,
        format!(
            "rustc {} failed: {}",
            args.join(" "),
            outcome.stderr_tail(5)
        ),
    ))
}

/// Reads the toolchain name from rustup's "not installed" error.
pub fn missing_toolchain(stderr: &str) -> Option<String> {
    let line = stderr
        .lines()
        .find(|line| line.contains("is not installed"))?;
    let start = line.find('\'')? + 1;
    let end = start + line[start..].find('\'')?;
    Some(line[start..end].to_string())
}

/// The targets with an installed standard library under a sysroot.
pub fn installed_targets(sysroot: &Path) -> BTreeSet<String> {
    std::fs::read_dir(sysroot.join("lib").join("rustlib"))
        .map(|read| {
            read.flatten()
                .filter(|entry| entry.path().join("lib").is_dir())
                .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
                .filter(|name| name.contains('-'))
                .collect()
        })
        .unwrap_or_default()
}

/// The host's target triple.
pub fn host_triple() -> &'static str {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => "aarch64-apple-darwin",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("x86_64", "windows") => "x86_64-pc-windows-msvc",
        ("aarch64", "windows") => "aarch64-pc-windows-msvc",
        _ => "unknown",
    }
}

/// The iOS simulator triple for this host.
pub fn ios_sim_triple() -> &'static str {
    if std::env::consts::ARCH == "x86_64" {
        "x86_64-apple-ios"
    } else {
        "aarch64-apple-ios-sim"
    }
}

/// The Rust targets a dev platform needs. For Android these are the ABIs'
/// triples (a dev build needs only the device's; pass just that ABI).
pub fn required_targets(platform: &str, abis: &[Abi]) -> Vec<String> {
    match platform {
        "desktop" => vec![host_triple().to_string()],
        "web" => vec!["wasm32-unknown-unknown".to_string()],
        "ios-sim" => vec![ios_sim_triple().to_string()],
        "ios-device" | "ios" => vec!["aarch64-apple-ios".to_string()],
        "android" => abis.iter().map(|abi| abi.triple().to_string()).collect(),
        _ => Vec::new(),
    }
}

/// One check per target: PASS, or FAIL `env.rust_target_missing` with the
/// `rustup target add --toolchain <name> <target>` fix.
pub fn check_targets(toolchain: &Toolchain, targets: &[String]) -> Vec<Check> {
    let installed = toolchain.installed_targets();
    let label = toolchain
        .name
        .clone()
        .unwrap_or_else(|| toolchain.rustc.clone());

    targets
        .iter()
        .map(|target| {
            if installed.contains(target) {
                Check::pass(
                    CheckId::EnvRustTargetMissing,
                    format!("{target} is installed for {label}"),
                )
            } else {
                Check::from_error(
                    IcmError::new(
                        CheckId::EnvRustTargetMissing,
                        format!("{target} is not installed for the project's toolchain {label}"),
                    )
                    .fix_commands([
                        "icm doctor --fix --yes".to_string(),
                        toolchain.add_target_command(target),
                    ]),
                    crate::error::Status::Fail,
                )
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_toolchains_are_named() {
        let stderr = "error: toolchain '1.97.0-aarch64-apple-darwin' is not installed\nhelp: run `rustup toolchain install` to install it\n";
        assert_eq!(
            missing_toolchain(stderr).as_deref(),
            Some("1.97.0-aarch64-apple-darwin")
        );
        assert_eq!(missing_toolchain("error: something else"), None);
    }

    #[test]
    fn targets_come_from_the_sysroot() {
        let tmp = tempfile::tempdir().unwrap();
        for target in ["aarch64-apple-darwin", "aarch64-apple-ios-sim"] {
            std::fs::create_dir_all(tmp.path().join("lib/rustlib").join(target).join("lib"))
                .unwrap();
        }
        std::fs::create_dir_all(tmp.path().join("lib/rustlib/etc")).unwrap();
        std::fs::write(tmp.path().join("lib/rustlib/multirust-config.toml"), "").unwrap();

        let toolchain = Toolchain {
            name: Some("1.98.0-aarch64-apple-darwin".into()),
            reason: None,
            sysroot: tmp.path().to_path_buf(),
            rustc: "rustc 1.98.0 (88d9e12ae 2026-08-18)".into(),
        };
        assert_eq!(toolchain.rustc_version(), "1.98.0");
        assert_eq!(
            toolchain
                .installed_targets()
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["aarch64-apple-darwin", "aarch64-apple-ios-sim"]
        );

        let checks = check_targets(
            &toolchain,
            &[
                "aarch64-apple-ios-sim".to_string(),
                "aarch64-linux-android".to_string(),
            ],
        );
        assert!(!checks[0].failed());
        assert!(checks[1].failed());
        assert_eq!(
            checks[1].error.fix.commands[1],
            "rustup target add --toolchain 1.98.0-aarch64-apple-darwin aarch64-linux-android"
        );
        assert_eq!(checks[1].error.fix.by, crate::catalogue::By::DoctorYes);
    }

    #[test]
    fn platforms_map_to_targets() {
        assert_eq!(required_targets("web", &[]), vec!["wasm32-unknown-unknown"]);
        assert_eq!(
            required_targets("android", &[Abi::Arm64V8a, Abi::X86_64]),
            vec!["aarch64-linux-android", "x86_64-linux-android"]
        );
        assert_eq!(
            required_targets("ios-device", &[]),
            vec!["aarch64-apple-ios"]
        );
        assert_eq!(required_targets("desktop", &[]), vec![host_triple()]);
    }

    #[test]
    fn the_active_toolchain_resolves_here() {
        // Uses the real rustc that is building these tests.
        let toolchain = active(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert!(toolchain.rustc.starts_with("rustc "));
        assert!(toolchain.installed_targets().contains(host_triple()));
    }
}
