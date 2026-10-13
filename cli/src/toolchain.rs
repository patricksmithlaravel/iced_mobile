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
use crate::context::{Ctx, probe_end_error};
use crate::error::{Check, IcmError};
use crate::process::{self, Cmd};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The time limit of each `rustc` probe (its sysroot and its version).
const RUSTC_LIMIT: Duration = Duration::from_secs(60);
/// The time limit of `rustup show active-toolchain`.
const RUSTUP_LIMIT: Duration = Duration::from_secs(30);

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

/// Resolves the active toolchain for `dir`. A `rustup show active-toolchain`
/// that runs out of its limit WARNs `step.timeout` through `ctx` and leaves
/// the toolchain unnamed, as a rustup that fails does. The rustc probes stop
/// with their timeout as an error.
pub fn active(ctx: &Ctx, dir: &Path) -> Result<Toolchain, IcmError> {
    active_with(
        ctx,
        dir,
        &process::tool_path("rustc"),
        RUSTC_LIMIT,
        &process::tool_path("rustup"),
        RUSTUP_LIMIT,
    )
}

/// [`active`] with the programs that run and the limits of its probes (a
/// test passes fake ones that run out of time).
fn active_with(
    ctx: &Ctx,
    dir: &Path,
    rustc_program: &OsStr,
    rustc_limit: Duration,
    rustup_program: &OsStr,
    rustup_limit: Duration,
) -> Result<Toolchain, IcmError> {
    let sysroot = rustc(dir, rustc_program, &["--print", "sysroot"], rustc_limit)?;
    let rustc_line = rustc(dir, rustc_program, &["--version"], rustc_limit)?;

    let mut name = None;
    let mut reason = None;
    let rustup = Cmd::new(rustup_program)
        .args(["show", "active-toolchain"])
        .cwd(dir)
        .timeout(rustup_limit);
    if let Ok(outcome) = process::run(&rustup, None, None) {
        // A rustup that runs out of its limit leaves the toolchain unnamed, as
        // one that fails does, and WARNs so the stall shows (Appendix D item 23).
        // Only a timeout is reported here; any other error is left as it was.
        if let Some(error) =
            probe_end_error("rustup show active-toolchain", &rustup, &outcome, false)
        {
            ctx.warn_if_timeout(error);
        }
        if outcome.success() {
            let text = outcome.stdout_text();
            let line = text.lines().next().unwrap_or("").trim();
            if let Some((first, rest)) = line.split_once(' ') {
                name = Some(first.to_string());
                reason = Some(rest.trim().trim_matches(['(', ')']).to_string());
            } else if !line.is_empty() {
                name = Some(line.to_string());
            }
        }
    }

    Ok(Toolchain {
        name,
        reason,
        sysroot: PathBuf::from(sysroot),
        rustc: rustc_line,
    })
}

/// One `rustc` probe. A process that runs out of `limit` is a timeout
/// (`step.timeout`, its fix naming the probe and the limit), not a missing
/// rustc. A program that cannot start is `env.tool_missing`, and so is one
/// that exits non-zero (its detail says why: the stderr tail, or the status
/// when stderr is silent).
fn rustc(dir: &Path, program: &OsStr, args: &[&str], limit: Duration) -> Result<String, IcmError> {
    let cmd = Cmd::new(program).args(args).cwd(dir).timeout(limit);
    let name = format!("rustc {}", args.join(" "));
    let outcome = process::run(&cmd, None, None).map_err(|error| {
        IcmError::new(
            CheckId::EnvToolMissing,
            format!("cannot run rustc: {error}"),
        )
        .fix("Install Rust with rustup (https://rustup.rs).", &[])
        .by(crate::catalogue::By::Owner)
    })?;
    if let Some(error) = probe_end_error(&name, &cmd, &outcome, false) {
        return Err(error);
    }

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
        format!("{name} failed: {}", outcome.reason(5)),
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

    /// A `rustc` that answers both probes, for the tests of `rustup`.
    fn fake_rustc(dir: &Path) -> PathBuf {
        crate::process::fake_script(
            dir,
            "rustc",
            "if [ \"$1\" = \"--print\" ]; then echo /test/sysroot; else echo \"rustc 1.0.0 (test)\"; fi",
        )
    }

    #[test]
    fn a_rustc_that_runs_out_of_time_is_a_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let rustc = crate::process::fake_script(tmp.path(), "rustc", "sleep 30");
        let rustup = tmp.path().join("no-such-rustup");
        let error = active_with(
            &crate::context::test_ctx(),
            tmp.path(),
            rustc.as_os_str(),
            Duration::from_millis(300),
            rustup.as_os_str(),
            RUSTUP_LIMIT,
        )
        .unwrap_err();
        assert_eq!(error.id, CheckId::StepTimeout.id(), "{error:?}");
        assert_eq!(error.exit.code(), 8);
        assert!(
            error.detail.contains("rustc --print sysroot"),
            "{}",
            error.detail
        );
        assert!(error.detail.contains("300ms"), "{}", error.detail);
        assert!(
            error.fix.summary.contains("rustc --print sysroot"),
            "{error:?}"
        );
        assert!(error.fix.summary.contains("300ms"), "{error:?}");
    }

    #[test]
    fn a_stalled_rustup_warns_and_the_run_goes_on_unnamed() {
        let tmp = tempfile::tempdir().unwrap();
        let rustc = fake_rustc(tmp.path());
        let rustup = crate::process::fake_script(tmp.path(), "rustup", "sleep 30");
        let ctx = crate::context::test_ctx();
        let toolchain = active_with(
            &ctx,
            tmp.path(),
            rustc.as_os_str(),
            RUSTC_LIMIT,
            rustup.as_os_str(),
            Duration::from_millis(300),
        )
        .unwrap();
        assert_eq!(toolchain.name, None);
        assert_eq!(toolchain.reason, None);
        assert_eq!(toolchain.rustc_version(), "1.0.0");
        assert_eq!(
            toolchain.add_target_command("aarch64-linux-android"),
            "rustup target add aarch64-linux-android"
        );
        assert_eq!(
            ctx.rep.warning_ids(),
            vec![CheckId::StepTimeout.id().to_string()]
        );
    }

    #[test]
    fn a_rustup_that_names_the_toolchain_is_read() {
        let tmp = tempfile::tempdir().unwrap();
        let rustc = fake_rustc(tmp.path());
        let rustup = crate::process::fake_script(
            tmp.path(),
            "rustup",
            "echo \"1.98.0-aarch64-apple-darwin (overridden by '/x/rust-toolchain.toml')\"",
        );
        let ctx = crate::context::test_ctx();
        let toolchain = active_with(
            &ctx,
            tmp.path(),
            rustc.as_os_str(),
            RUSTC_LIMIT,
            rustup.as_os_str(),
            RUSTUP_LIMIT,
        )
        .unwrap();
        assert_eq!(
            toolchain.name.as_deref(),
            Some("1.98.0-aarch64-apple-darwin")
        );
        assert_eq!(
            toolchain.reason.as_deref(),
            Some("overridden by '/x/rust-toolchain.toml'")
        );
        assert!(ctx.rep.warning_ids().is_empty());
    }

    #[test]
    fn a_rustup_that_fails_or_is_missing_leaves_the_toolchain_unnamed_silently() {
        let tmp = tempfile::tempdir().unwrap();
        let rustc = fake_rustc(tmp.path());
        let failing = crate::process::fake_script(tmp.path(), "rustup", "exit 1");
        let missing = tmp.path().join("no-such-rustup");
        for rustup in [failing, missing] {
            let ctx = crate::context::test_ctx();
            let toolchain = active_with(
                &ctx,
                tmp.path(),
                rustc.as_os_str(),
                RUSTC_LIMIT,
                rustup.as_os_str(),
                RUSTUP_LIMIT,
            )
            .unwrap();
            assert_eq!(toolchain.name, None);
            assert_eq!(toolchain.rustc_version(), "1.0.0");
            assert!(ctx.rep.warning_ids().is_empty());
        }
    }

    #[test]
    fn a_missing_rustc_is_still_a_missing_tool() {
        let tmp = tempfile::tempdir().unwrap();
        let rustc = tmp.path().join("no-such-rustc");
        let rustup = tmp.path().join("no-such-rustup");
        let error = active_with(
            &crate::context::test_ctx(),
            tmp.path(),
            rustc.as_os_str(),
            RUSTC_LIMIT,
            rustup.as_os_str(),
            RUSTUP_LIMIT,
        )
        .unwrap_err();
        assert_eq!(error.id, CheckId::EnvToolMissing.id(), "{error:?}");
        assert_eq!(error.exit.code(), 4);
    }

    #[test]
    fn a_rustc_that_fails_without_stderr_says_its_status() {
        let tmp = tempfile::tempdir().unwrap();
        let rustc = crate::process::fake_script(tmp.path(), "rustc", "exit 1");
        let rustup = tmp.path().join("no-such-rustup");
        let error = active_with(
            &crate::context::test_ctx(),
            tmp.path(),
            rustc.as_os_str(),
            RUSTC_LIMIT,
            rustup.as_os_str(),
            RUSTUP_LIMIT,
        )
        .unwrap_err();
        assert_eq!(error.id, CheckId::EnvToolMissing.id(), "{error:?}");
        assert!(error.detail.ends_with("failed: exit 1"), "{}", error.detail);
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
        let toolchain = active(
            &crate::context::test_ctx(),
            Path::new(env!("CARGO_MANIFEST_DIR")),
        )
        .unwrap();
        assert!(toolchain.rustc.starts_with("rustc "));
        assert!(toolchain.installed_targets().contains(host_triple()));
    }
}
