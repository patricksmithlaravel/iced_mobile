//! `icm check [<platform>…|--all] [--release] [--clippy]` (design §6
//! `check`, Appendix C items 8, 11 and 29).
//!
//! Without a device, in this order, each stage stopping the command when it
//! fails:
//!
//! 1. config: icm.toml (with `file:line`), the icon, the package, binary and
//!    library each platform builds (exit 3);
//! 2. toolchain: the project's toolchain and each platform's Rust target
//!    (exit 4, `icm doctor <platform> --fix --yes`);
//! 3. dependencies: `Cargo.lock` (resolved first with `cargo
//!    generate-lockfile` when the app has none) and the lockfile checks:
//!    one iced, from the fork, pinned; one winit at its floor (exit 3);
//! 4. `cargo check` per platform (`cargo clippy` with `--clippy`): `--lib`
//!    for Android, `--bin` elsewhere. Every platform is checked even when
//!    one fails; each failure carries its first rustc errors (file, line,
//!    rendered text) in `errors[]`, in every output mode (exit 5).
//!
//! `--all`, or no platform, checks every platform in `[app] platforms`.

use crate::cargo::{self, Invocation, Message, Select};
use crate::catalogue::CheckId;
use crate::cli::{CheckArgs, Platform};
use crate::config::AppPlatform;
use crate::context::{Ctx, Project};
use crate::error::{Check, Diagnostic, Evidence, IcmError, Result, Status};
use crate::toolchain;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// How many rendered errors each failed platform carries.
const MAX_ERRORS: usize = 10;

/// Runs `icm check`.
pub fn run(ctx: &mut Ctx, args: &CheckArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    let host = ctx.host()?.clone();
    let platforms = platforms(&project, args);
    let profile = if args.release { "release" } else { "dev" };
    ctx.rep.set("profile", json!(cargo::profile_dir(profile)));
    ctx.rep.set(
        "platforms",
        json!(platforms.iter().map(|p| p.as_str()).collect::<Vec<_>>()),
    );

    // 1. config
    let builds = config_checks(ctx, &project, &platforms)?;

    // 2. toolchain and targets
    let toolchain = toolchain::active(project.dir())?;
    ctx.rep.set(
        "tools",
        json!({"rustc": toolchain.rustc_version(), "toolchain": toolchain.name}),
    );
    let mut missing: Vec<IcmError> = Vec::new();
    for (check, build) in toolchain::check_targets(
        &toolchain,
        &builds
            .iter()
            .map(|b| {
                b.triple
                    .clone()
                    .unwrap_or_else(|| toolchain::host_triple().to_string())
            })
            .collect::<Vec<_>>(),
    )
    .into_iter()
    .zip(&builds)
    {
        let mut check = check;
        if check.failed() {
            check.error.fix.commands[0] =
                format!("icm doctor {} --fix --yes", build.platform.as_str());
            missing.push(check.error.clone());
        }
        ctx.rep.check(check);
    }
    if let Some(first) = missing.into_iter().next() {
        return Err(first);
    }

    // 3. dependencies
    let lock_path = project.lock_path();
    if !lock_path.exists() {
        resolve_lock(ctx, &project)?;
    }
    if let Some(lock) = project.lock()? {
        let android = platforms.contains(&Platform::Android);
        let checks = crate::deps::check_lock(&lock, android);
        let first = checks.iter().find(|c| c.failed()).cloned();
        for check in checks {
            ctx.rep.check(check);
        }
        if let Some(failure) = first {
            return Err(failure.into_error());
        }
    }

    // 4. cargo check, every platform
    let mut failures: Vec<IcmError> = Vec::new();
    let mut targets: Vec<Value> = Vec::new();
    let mut warnings = 0usize;
    for build in &builds {
        let started = std::time::Instant::now();
        match compile(ctx, &project, &host, build, profile, args.clippy) {
            Ok(compiled) => {
                warnings += compiled.warnings;
                ctx.rep.check(Check::pass(
                    CheckId::BuildCompileError,
                    format!(
                        "{} {} ({}): no errors{}",
                        if args.clippy {
                            "cargo clippy"
                        } else {
                            "cargo check"
                        },
                        build.platform.as_str(),
                        build.label(),
                        match compiled.warnings {
                            0 => String::new(),
                            n => format!(", {n} warning(s)"),
                        }
                    ),
                ));
                targets.push(json!({
                    "platform": build.platform.as_str(),
                    "triple": build.label(),
                    "ok": true,
                    "errors": 0,
                    "warnings": compiled.warnings,
                    "ms": started.elapsed().as_millis() as u64,
                }));
            }
            Err(error) if error.exit == crate::exit::Exit::Build => {
                targets.push(json!({
                    "platform": build.platform.as_str(),
                    "triple": build.label(),
                    "ok": false,
                    "errors": error.diagnostics.len(),
                    "ms": started.elapsed().as_millis() as u64,
                }));
                ctx.rep
                    .check(Check::from_error(error.clone(), Status::Fail));
                failures.push(error);
            }
            Err(error) => return Err(error),
        }
    }
    ctx.rep.set("targets", Value::Array(targets));

    let names = platforms
        .iter()
        .map(|p| p.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    // errors[0] is a failure with rustc errors when there is one.
    if let Some(first) = failures
        .iter()
        .find(|f| !f.diagnostics.is_empty())
        .or(failures.first())
    {
        ctx.rep.next(
            "icm check --json -q".to_string(),
            "after fixing the errors in errors[].diagnostics",
        );
        return Err(first.clone());
    }

    if let Some(first) = platforms.first() {
        ctx.rep.next(
            format!("icm run {} --json -q", first.as_str()),
            "build, launch and screenshot the app",
        );
    }
    ctx.rep.summary(format!(
        "checked {names}: no errors{}",
        match warnings {
            0 => String::new(),
            n => format!(", {n} warning(s)"),
        }
    ));
    Ok(())
}

/// The platforms to check: those named, or every one in `[app] platforms`
/// (iOS as the simulator).
fn platforms(project: &Project, args: &CheckArgs) -> Vec<Platform> {
    let mut platforms: Vec<Platform> = if args.all || args.platforms.is_empty() {
        project
            .app()
            .platforms
            .iter()
            .map(|platform| match platform {
                AppPlatform::Desktop => Platform::Desktop,
                AppPlatform::Web => Platform::Web,
                AppPlatform::Ios => Platform::IosSim,
                AppPlatform::Android => Platform::Android,
            })
            .collect()
    } else {
        args.platforms.clone()
    };
    platforms.sort();
    platforms.dedup();
    platforms
}

/// What one platform compiles.
#[derive(Clone, Debug)]
struct Build {
    platform: Platform,
    /// The package.
    package: String,
    /// Its Cargo.toml.
    manifest: PathBuf,
    /// `--bin <name>` or `--lib`.
    select: Select,
    /// The target triple; `None` is the host.
    triple: Option<String>,
}

impl Build {
    fn label(&self) -> String {
        self.triple
            .clone()
            .unwrap_or_else(|| toolchain::host_triple().to_string())
    }
}

/// The config stage: icm.toml is valid (it loaded), the icon, and what
/// each platform builds. Fails (exit 3) when anything is wrong.
fn config_checks(ctx: &Ctx, project: &Project, platforms: &[Platform]) -> Result<Vec<Build>> {
    let config = &project.config;
    let mut failures: Vec<IcmError> = Vec::new();

    let mut valid = format!(
        "{} is valid (schema {}",
        crate::paths::display(&config.path),
        config.config.schema
    );
    if let Some(min) = config.config.min_icm() {
        valid.push_str(&format!(", min_icm {min}"));
    }
    valid.push(')');
    ctx.rep.check(Check::pass(CheckId::ConfigInvalid, valid));

    if config.config.id_is_placeholder() {
        ctx.rep.check(
            Check::warn(
                CheckId::AppIdPlaceholder,
                format!(
                    "{}: [app] id {} is a placeholder (a release needs the owner's permanent id)",
                    config.source.location_for("app.id"),
                    config.config.app.id
                ),
            )
            .evidence(config.evidence("app.id")),
        );
    }

    let mut push = |check: Check| {
        if check.failed() {
            failures.push(check.error.clone());
        }
        ctx.rep.check(check);
    };
    push(icon_check(project));

    let mut builds = Vec::new();
    for &platform in platforms {
        let package = match project.package_for(platform.as_str()) {
            Ok(package) => package.clone(),
            Err(error) => {
                push(Check::from_error(error, Status::Fail));
                continue;
            }
        };
        let (select, what) = if platform == Platform::Android {
            match project.lib_name() {
                Ok(lib) => (Select::Lib, format!("library {lib}")),
                Err(error) => {
                    push(Check::from_error(error, Status::Fail));
                    continue;
                }
            }
        } else {
            match project.bin_for(platform.as_str()) {
                Ok(bin) => (Select::Bin(bin.clone()), format!("binary {bin}")),
                Err(error) => {
                    push(Check::from_error(error, Status::Fail));
                    continue;
                }
            }
        };
        let id = if platform == Platform::Android {
            CheckId::ConfigLibMissing
        } else {
            CheckId::ConfigBinMissing
        };
        push(Check::pass(
            id,
            format!(
                "{} builds package {}, {what}",
                platform.as_str(),
                package.name
            ),
        ));
        if platform == Platform::Android {
            push(android_entry(&package));
        }
        builds.push(Build {
            platform,
            package: package.name.clone(),
            manifest: package.manifest_path.clone(),
            select,
            triple: triple(project, platform),
        });
    }

    match failures.into_iter().next() {
        Some(first) => Err(first),
        None => Ok(builds),
    }
}

/// The triple a platform checks: the host for the desktop, the simulator
/// for ios-sim, the first `[android] abis` entry for Android.
fn triple(project: &Project, platform: Platform) -> Option<String> {
    match platform {
        Platform::Desktop => None,
        Platform::Web => Some("wasm32-unknown-unknown".to_string()),
        Platform::IosSim => Some(toolchain::ios_sim_triple().to_string()),
        Platform::IosDevice => Some("aarch64-apple-ios".to_string()),
        Platform::Android => Some(
            project
                .config
                .config
                .android
                .abis
                .first()
                .copied()
                .unwrap_or(crate::config::Abi::Arm64V8a)
                .triple()
                .to_string(),
        ),
    }
}

/// `app.icon.invalid` (exit 3) unless `[app] icon` is a square PNG of at
/// least 1024x1024; `app.icon.placeholder` (WARN) while it is the
/// template's.
fn icon_check(project: &Project) -> Check {
    let config = &project.config;
    let Some(icon) = config.config.app.icon.as_deref() else {
        return Check::warn(
            CheckId::AppIconPlaceholder,
            "[app] icon is not set; the app gets a placeholder icon",
        )
        .evidence(config.evidence("app"));
    };
    let path = project.dir().join(icon);
    let location = config.source.location_for("app.icon");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Check::fail(
                CheckId::AppIconInvalid,
                format!(
                    "{location}: cannot read {}: {error}",
                    crate::paths::display(&path)
                ),
            )
            .evidence(config.evidence("app.icon"));
        }
    };
    let Some((width, height)) = png_size(&bytes) else {
        return Check::fail(
            CheckId::AppIconInvalid,
            format!("{location}: {} is not a PNG", crate::paths::display(&path)),
        )
        .evidence(Evidence::file(&path));
    };
    if width != height || width < 1024 {
        return Check::fail(
            CheckId::AppIconInvalid,
            format!(
                "{location}: {} is {width}x{height}; it must be square and at least 1024x1024",
                crate::paths::display(&path)
            ),
        )
        .evidence(Evidence::file(&path));
    }
    if crate::template::placeholder_icon_sha256().as_deref()
        == Some(crate::hash::sha256_hex(&bytes).as_str())
    {
        return Check::warn(
            CheckId::AppIconPlaceholder,
            format!(
                "{} is still the template's placeholder icon",
                crate::paths::display(&path)
            ),
        )
        .evidence(Evidence::file(&path));
    }
    Check::pass(
        CheckId::AppIconInvalid,
        format!("{} is a {width}x{height} PNG", crate::paths::display(&path)),
    )
}

/// A PNG's size from its IHDR chunk.
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || &bytes[..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((width, height))
}

/// Android starts the app through `iced::android_main!`: INFO
/// `deps.legacy_entry` for a hand-written `android_main`, WARN
/// `android.so.export` when neither is in the package's sources.
fn android_entry(package: &crate::cargo::Package) -> Check {
    let mut sources = Vec::new();
    collect_sources(&package.dir().join("src"), &mut sources, 400);
    let mut hand_written: Option<(PathBuf, u32)> = None;
    for path in &sources {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        if text.contains("android_main!") {
            return Check::pass(
                CheckId::AndroidSoExport,
                format!(
                    "{} starts the app on Android with android_main!",
                    crate::paths::display(path)
                ),
            );
        }
        if hand_written.is_none()
            && let Some(index) = text.lines().position(|l| l.contains("fn android_main"))
        {
            hand_written = Some((path.clone(), index as u32 + 1));
        }
    }
    match hand_written {
        Some((path, line)) => Check::info(
            CheckId::DepsLegacyEntry,
            format!(
                "{}:{line} defines android_main by hand",
                crate::paths::display(&path)
            ),
        )
        .evidence(Evidence::line(&path, line, "fn android_main")),
        None => Check::warn(
            CheckId::AndroidSoExport,
            format!(
                "no `iced::android_main!(run);` in {}; Android cannot start the app",
                crate::paths::display(&package.dir().join("src"))
            ),
        )
        .fix(
            "Add `iced::android_main!(run);` to src/lib.rs, as the template does.",
            &[],
        ),
    }
}

fn collect_sources(dir: &Path, out: &mut Vec<PathBuf>, limit: usize) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if out.len() >= limit {
            return;
        }
        if path.is_dir() {
            collect_sources(&path, out, limit);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// A new app has no Cargo.lock: resolve it first (Appendix C item 29).
fn resolve_lock(ctx: &Ctx, project: &Project) -> Result<()> {
    let manifest = project.metadata.workspace_root.join("Cargo.toml");
    let mut cmd = crate::process::Cmd::tool("cargo")
        .arg("generate-lockfile")
        .arg("--manifest-path")
        .arg(&manifest)
        .cwd(project.dir())
        .timeout(std::time::Duration::from_secs(20 * 60));
    if ctx.global.offline {
        cmd = cmd.arg("--offline");
    }
    ctx.rep
        .progress("resolving Cargo.lock (cargo generate-lockfile)");
    let outcome = ctx.step("cargo.lockfile", &cmd)?;
    if outcome.success() {
        Ok(())
    } else {
        Err(ctx.step_failure("cargo.lockfile", CheckId::BuildCargoFailed, &outcome))
    }
}

/// A successful compile.
struct Compiled {
    warnings: usize,
}

/// The environment one platform compiles with.
fn build_env(
    ctx: &Ctx,
    project: &Project,
    host: &crate::host::HostConfig,
    build: &Build,
) -> Vec<(String, String)> {
    let config = &project.config.config;
    let mut env = Vec::new();
    match build.platform {
        Platform::Desktop => {
            if cfg!(target_os = "macos") {
                env.push((
                    "MACOSX_DEPLOYMENT_TARGET".to_string(),
                    config.desktop.macos.min_os.clone(),
                ));
            }
        }
        Platform::Web => {
            if !config.web.rustflags.is_empty() {
                env.push((
                    "CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS".to_string(),
                    config.web.rustflags.join(" "),
                ));
            }
        }
        Platform::IosSim | Platform::IosDevice => {
            env.push((
                "IPHONEOS_DEPLOYMENT_TARGET".to_string(),
                config.ios.min_os.clone(),
            ));
        }
        Platform::Android => {
            // `cargo check` needs no NDK, but a dependency's build script
            // that compiles C does; give it one when there is one.
            if let Ok(ndk) = crate::tools::ndk(
                crate::tools::android_sdk(host, &ctx.env).ok().as_ref(),
                host,
                &ctx.env,
            ) && let Some(triple) = &build.triple
            {
                env.extend(crate::tools::ndk_env(&ndk, triple, config.android.min_sdk));
            }
        }
    }
    env
}

/// `cargo check` (or clippy) for one platform. A compile failure is a
/// `build.*` error carrying this platform's first rustc errors.
fn compile(
    ctx: &Ctx,
    project: &Project,
    host: &crate::host::HostConfig,
    build: &Build,
    profile: &str,
    clippy: bool,
) -> Result<Compiled> {
    let mut invocation = Invocation::new(
        if clippy { "clippy" } else { "check" },
        &build.manifest,
        &build.package,
    );
    invocation.select = build.select.clone();
    invocation.triple = build.triple.clone();
    invocation.profile = profile.to_string();
    invocation.offline = ctx.global.offline;

    let env = build_env(ctx, project, host, build);
    let cmd = invocation
        .cmd()
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));

    let rep = ctx.rep.clone();
    let mut errors: Vec<Diagnostic> = Vec::new();
    let mut warnings = 0usize;
    let mut on_line = |line: &str| {
        if let Some(Message::Diagnostic(diagnostic)) = cargo::parse_message(line) {
            let summary = diagnostic.file.is_none()
                && (diagnostic.message.starts_with("aborting due to")
                    || diagnostic.message.contains("warning emitted")
                    || diagnostic.message.contains("warnings emitted"));
            if !summary {
                match diagnostic.level.as_str() {
                    "error" | "error: internal compiler error" => errors.push(diagnostic.clone()),
                    "warning" => warnings += 1,
                    _ => {}
                }
            }
            rep.diagnostic(diagnostic);
        }
    };

    let name = format!(
        "cargo.{}.{}",
        if clippy { "clippy" } else { "check" },
        build.platform.as_str()
    );
    let outcome = ctx.step_with(&name, &cmd, Some(&mut on_line))?;
    if outcome.success() {
        return Ok(Compiled { warnings });
    }

    let stderr = outcome.stderr_text();
    let id = if cargo::is_link_failure(&stderr, &errors) {
        CheckId::BuildLinkError
    } else if errors.is_empty() {
        CheckId::BuildCargoFailed
    } else {
        CheckId::BuildCompileError
    };

    let mut error = ctx.step_failure(&name, id, &outcome);
    if let Some(first) = errors.first() {
        error.detail = format!(
            "{} {} ({}): {} error(s); first: {}{}",
            if clippy {
                "cargo clippy"
            } else {
                "cargo check"
            },
            build.platform.as_str(),
            build.label(),
            errors.len(),
            first.message,
            match (&first.file, first.line) {
                (Some(file), Some(line)) => format!(" at {file}:{line}"),
                _ => String::new(),
            }
        );
        // The rendered errors, and each one's file:line as evidence, ahead
        // of the step log.
        let log = std::mem::take(&mut error.evidence);
        for diagnostic in errors.iter().take(MAX_ERRORS) {
            if let (Some(file), Some(line)) = (&diagnostic.file, diagnostic.line) {
                let path = project.metadata.workspace_root.join(file);
                error
                    .evidence
                    .push(Evidence::line(&path, line, diagnostic.message.clone()));
            }
        }
        error.evidence.extend(log);
        error.diagnostics = errors.into_iter().take(MAX_ERRORS).collect();
    } else {
        error.detail = format!(
            "{} {} ({}) failed: {}",
            if clippy {
                "cargo clippy"
            } else {
                "cargo check"
            },
            build.platform.as_str(),
            build.label(),
            error.detail
        );
    }
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_sizes_come_from_the_header() {
        let icon = crate::template::file("assets/icon.png").unwrap();
        assert_eq!(png_size(icon), Some((1024, 1024)));
        assert_eq!(png_size(b"GIF89a.................."), None);
        assert_eq!(png_size(&icon[..10]), None);
    }
}
