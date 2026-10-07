//! What `icm doctor` checks and how it fixes it (design §6 `doctor`,
//! Appendix C items 2, 8, 10 and 15).
//!
//! [`gather`] probes the machine for the requested platforms and returns
//! one [`Requirement`] per thing a platform needs: a check (PASS, FAIL,
//! WARN or SKIP, with the catalogue id, evidence and who fixes it) and the
//! [`Fix`]es that would repair it. Probing changes nothing. `icm doctor
//! --fix` runs the local fixes (`by: doctor`); `--fix --yes` also the ones
//! that download or install (`by: doctor-yes`). Licences, Xcode and SDK
//! installs are the owner's (`by: owner`) and are never automated.
//!
//! The rust checks use the toolchain the *project* resolves
//! (`rust-toolchain.toml` included), not icm's own (Appendix C item 8).

mod android;
pub mod fix;
mod ios;

pub use fix::Fix;

use crate::catalogue::{By, CheckId};
use crate::cli::Platform;
use crate::context::{Ctx, Project};
use crate::error::{Check, Status};
use crate::host::HostConfig;
use crate::toolchain::{self, Toolchain};
use crate::tools::{self, Env};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// One thing a platform needs.
#[derive(Clone, Debug)]
pub struct Requirement {
    /// A stable key (`rust.target:wasm32-unknown-unknown`, `android.avd`),
    /// the same across probes, so a fix's outcome can be matched to it.
    pub key: String,
    /// The platform that needs it, for the `icm doctor <platform> --fix`
    /// hint; `None` when every platform does.
    pub platform: Option<Platform>,
    /// What the probe found.
    pub check: Check,
    /// What would repair it (empty when nothing automatic can).
    pub fixes: Vec<Fix>,
}

impl Requirement {
    /// A requirement without a fix.
    pub fn new(key: impl Into<String>, platform: Option<Platform>, check: Check) -> Requirement {
        Requirement {
            key: key.into(),
            platform,
            check,
            fixes: Vec::new(),
        }
    }

    /// Adds fixes; the check's fix then names `icm doctor <platform> --fix
    /// [--yes]` and the commands the fixes run, and who acts follows the
    /// fixes (`doctor-yes` when any of them downloads).
    pub fn with_fixes(mut self, fixes: Vec<Fix>, env: &Env) -> Requirement {
        if fixes.is_empty() {
            return self;
        }
        let by = if fixes.iter().any(|fix| fix.by() == By::DoctorYes) {
            By::DoctorYes
        } else {
            By::Doctor
        };
        let doctor = format!(
            "icm doctor {}--fix{}",
            self.platform
                .map(|p| format!("{} ", p.as_str()))
                .unwrap_or_default(),
            if by == By::DoctorYes { " --yes" } else { "" }
        );
        let mut commands = vec![doctor];
        commands.extend(fixes.iter().map(|fix| fix.display(env)));
        self.check.error.fix.commands = commands;
        self.check.error.fix.by = by;
        self.fixes = fixes;
        self
    }

    /// Whether it still needs attention (FAIL or WARN).
    pub fn failing(&self) -> bool {
        matches!(self.check.status, Status::Fail | Status::Warn)
    }

    /// Who acts on it.
    pub fn by(&self) -> By {
        self.check.error.fix.by
    }
}

/// What a probe works from.
pub struct Probe<'a> {
    /// The context (for quick probes such as `simctl list`).
    pub ctx: &'a Ctx,
    /// host.toml.
    pub host: &'a HostConfig,
    /// The environment.
    pub env: &'a Env,
    /// The project, when doctor runs inside one.
    pub project: Option<&'a Project>,
    /// The project directory, or the current one.
    pub dir: &'a Path,
    /// Whether the platforms were named on the command line (an
    /// unsupported host is then a FAIL, not a SKIP).
    pub explicit: bool,
}

/// The dev platforms doctor checks when none are named: the project's
/// `[app] platforms`, or every dev platform but ios-device (phase 2); the
/// iOS Simulator only on macOS.
pub fn default_platforms(project: Option<&Project>) -> Vec<Platform> {
    use crate::config::AppPlatform;
    let mut platforms: Vec<Platform> = match project {
        Some(project) => project
            .config
            .config
            .app
            .platforms
            .iter()
            .map(|platform| match platform {
                AppPlatform::Desktop => Platform::Desktop,
                AppPlatform::Web => Platform::Web,
                AppPlatform::Ios => Platform::IosSim,
                AppPlatform::Android => Platform::Android,
            })
            .collect(),
        None => vec![
            Platform::Desktop,
            Platform::Web,
            Platform::IosSim,
            Platform::Android,
        ],
    };
    if !cfg!(target_os = "macos") {
        platforms.retain(|p| !matches!(p, Platform::IosSim | Platform::IosDevice));
    }
    platforms.sort();
    platforms.dedup();
    platforms
}

/// Probes everything the platforms need, in an order that lets each fix
/// build on the ones before it. Keys are unique.
pub fn gather(probe: &Probe<'_>, platforms: &[Platform]) -> Vec<Requirement> {
    let mut out: Vec<Requirement> = Vec::new();
    let mut push = |requirement: Requirement| {
        if !out.iter().any(|r| r.key == requirement.key) {
            out.push(requirement);
        }
    };

    let toolchain = rust_toolchain(probe, &mut push);
    push(optional_tool(
        probe,
        "git",
        "icm new skips `git init`, and results lack git_rev",
    ));

    for &platform in platforms {
        if let Some(toolchain) = &toolchain {
            for requirement in rust_targets(probe, toolchain, platform) {
                push(requirement);
            }
        }
        match platform {
            Platform::Desktop => push(linker(probe)),
            Platform::Web => {
                push(chrome(probe));
                push(wasm_bindgen(probe));
            }
            Platform::IosSim | Platform::IosDevice => {
                for requirement in ios::gather(probe, platform) {
                    push(requirement);
                }
            }
            Platform::Android => {
                for requirement in android::gather(probe) {
                    push(requirement);
                }
            }
        }
    }

    if let Some(project) = probe.project
        && let Ok(Some(lock)) = project.lock()
    {
        push(Requirement::new(
            "deps.skew",
            None,
            crate::deps::cli_framework_skew(
                &lock,
                crate::buildinfo::GIT_REV,
                crate::buildinfo::VERSION,
            ),
        ));
    }

    out
}

/// What doctor discovered, for the result's `tools` (design §3 "recorded in
/// doctor --json").
pub fn tools_json(probe: &Probe<'_>, platforms: &[Platform]) -> Value {
    let mut tools = Map::new();
    if let Ok(toolchain) = toolchain::active(probe.dir) {
        let _ = tools.insert("rustc".into(), json!(toolchain.rustc_version()));
        let _ = tools.insert("toolchain".into(), json!(toolchain.name));
    }
    if platforms
        .iter()
        .any(|p| matches!(p, Platform::IosSim | Platform::IosDevice))
        && let Ok(xcode) = tools::xcode(probe.env)
    {
        let _ = tools.insert("xcode".into(), json!(xcode.display()));
        let _ = tools.insert("developer_dir".into(), json!(xcode.developer_dir));
    }
    if platforms.contains(&Platform::Android) {
        let sdk = tools::android_sdk(probe.host, probe.env).ok();
        if let Some(sdk) = &sdk {
            let _ = tools.insert("android_sdk".into(), json!(sdk.root));
        }
        if let Ok(ndk) = tools::ndk(sdk.as_ref(), probe.host, probe.env) {
            let _ = tools.insert("ndk".into(), json!(ndk.version));
        }
        if let Ok(jdk) = tools::jdk(probe.host, probe.env, true) {
            let _ = tools.insert("jdk".into(), json!(jdk.version));
            let _ = tools.insert("java_home".into(), json!(jdk.home));
        }
    }
    if platforms.contains(&Platform::Web)
        && let Ok(chrome) = tools::chrome(probe.host, probe.env)
    {
        let _ = tools.insert("chrome".into(), json!(chrome.path));
    }
    Value::Object(tools)
}

fn rust_toolchain(probe: &Probe<'_>, push: &mut impl FnMut(Requirement)) -> Option<Toolchain> {
    match toolchain::active(probe.dir) {
        Ok(toolchain) => {
            let mut detail = format!(
                "rustc {} for {}",
                toolchain.rustc_version(),
                crate::paths::display(probe.dir)
            );
            if let Some(name) = &toolchain.name {
                detail.push_str(&format!(" (toolchain {name}"));
                if let Some(reason) = &toolchain.reason {
                    detail.push_str(&format!(", {reason}"));
                }
                detail.push(')');
            }
            push(Requirement::new(
                "rust.toolchain",
                None,
                Check::pass(CheckId::EnvRustToolchainMissing, detail),
            ));
            Some(toolchain)
        }
        Err(error) => {
            let fixes = if error.id == CheckId::EnvRustToolchainMissing.id() {
                vec![Fix::ToolchainInstall {
                    dir: probe.dir.to_path_buf(),
                    name: quoted(&error.detail),
                }]
            } else {
                Vec::new()
            };
            push(
                Requirement::new(
                    "rust.toolchain",
                    None,
                    Check::from_error(error, Status::Fail),
                )
                .with_fixes(fixes, probe.env),
            );
            None
        }
    }
}

/// The Rust targets a platform needs.
fn targets_for(probe: &Probe<'_>, platform: Platform) -> Vec<String> {
    let abis = probe
        .project
        .map(|p| p.config.config.android.abis.clone())
        .unwrap_or_else(|| vec![crate::config::Abi::Arm64V8a, crate::config::Abi::X86_64]);
    toolchain::required_targets(platform.as_str(), &abis)
}

fn rust_targets(probe: &Probe<'_>, toolchain: &Toolchain, platform: Platform) -> Vec<Requirement> {
    if matches!(platform, Platform::IosSim | Platform::IosDevice) && !cfg!(target_os = "macos") {
        return Vec::new();
    }
    let targets = targets_for(probe, platform);
    toolchain::check_targets(toolchain, &targets)
        .into_iter()
        .zip(targets)
        .map(|(mut check, target)| {
            let key = format!("rust.target:{target}");
            if !check.failed() {
                return Requirement::new(key, Some(platform), check);
            }
            match &toolchain.name {
                Some(name) => Requirement::new(key, Some(platform), check).with_fixes(
                    vec![Fix::TargetAdd {
                        dir: probe.dir.to_path_buf(),
                        toolchain: name.clone(),
                        targets: vec![target],
                    }],
                    probe.env,
                ),
                None => {
                    check.error = check
                        .error
                        .fix(
                            "This rustc is not managed by rustup; install the target's standard library for it.",
                            &[],
                        )
                        .by(By::Owner);
                    Requirement::new(key, Some(platform), check)
                }
            }
        })
        .collect()
}

/// A tool whose absence costs a feature but blocks nothing: a WARN
/// (Appendix C item 15).
fn optional_tool(probe: &Probe<'_>, name: &str, without: &str) -> Requirement {
    let key = format!("tool:{name}");
    match find_tool(probe.env, name) {
        Some(path) => Requirement::new(
            key,
            None,
            Check::pass(
                CheckId::EnvToolMissing,
                format!("{name}: {}", path.display()),
            ),
        ),
        None => {
            let mut check = Check::warn(
                CheckId::EnvToolMissing,
                format!("{name} was not found (optional; without it {without})"),
            );
            check.error = check
                .error
                .fix(
                    format!(
                        "Install {name}, or point ICM_TOOL_{} at it.",
                        name.to_ascii_uppercase()
                    ),
                    &[],
                )
                .by(By::Owner);
            Requirement::new(key, None, check)
        }
    }
}

/// `ICM_TOOL_<NAME>`, else `PATH`.
pub fn find_tool(env: &Env, name: &str) -> Option<PathBuf> {
    env.tool_override(name)
        .filter(|path| path.exists())
        .or_else(|| crate::paths::which(name))
}

fn linker(probe: &Probe<'_>) -> Requirement {
    let key = "desktop.linker";
    match find_tool(probe.env, "cc") {
        Some(path) => Requirement::new(
            key,
            Some(Platform::Desktop),
            Check::pass(
                CheckId::EnvToolMissing,
                format!("C linker: {}", path.display()),
            ),
        ),
        None => {
            let install = if cfg!(target_os = "macos") {
                "xcode-select --install"
            } else {
                "sudo apt-get install build-essential   # or your distribution's C toolchain"
            };
            let mut check = Check::fail(
                CheckId::EnvToolMissing,
                "no C compiler/linker (`cc`) on PATH; Rust needs it to link the desktop app",
            );
            check.error = check
                .error
                .fix("The owner installs a C toolchain.", &[install])
                .by(By::Owner);
            Requirement::new(key, Some(Platform::Desktop), check)
        }
    }
}

fn chrome(probe: &Probe<'_>) -> Requirement {
    let key = "web.chrome";
    match tools::chrome(probe.host, probe.env) {
        Ok(found) => Requirement::new(
            key,
            Some(Platform::Web),
            Check::pass(
                CheckId::EnvChromeMissing,
                format!("Chrome: {} ({})", found.path.display(), found.source),
            ),
        ),
        Err(error) => Requirement::new(
            key,
            Some(Platform::Web),
            Check::from_error(error, Status::Fail),
        ),
    }
}

fn wasm_bindgen(probe: &Probe<'_>) -> Requirement {
    let key = "web.wasm_bindgen";
    let platform = Some(Platform::Web);
    let Some(project) = probe.project else {
        return Requirement::new(
            key,
            platform,
            Check::skip(
                CheckId::DepsWasmBindgenCli,
                "no app here: run icm doctor in the app's directory to check the wasm-bindgen CLI",
            ),
        );
    };

    let lock_path = project.lock_path();
    let lock = match project.lock() {
        Ok(lock) => lock,
        Err(error) => {
            return Requirement::new(key, platform, Check::from_error(error, Status::Fail));
        }
    };
    let Some(lock) = lock else {
        let check = Check::fail(
            CheckId::DepsWasmBindgenCli,
            format!(
                "{} does not exist yet, so the app's wasm-bindgen version is unknown",
                crate::paths::display(&lock_path)
            ),
        );
        return Requirement::new(key, platform, check).with_fixes(
            vec![
                Fix::GenerateLockfile {
                    manifest: project.package.manifest_path.clone(),
                },
                Fix::WasmBindgen {
                    version: None,
                    lock: lock_path,
                },
            ],
            probe.env,
        );
    };

    let Some(version) = lock.version_of("wasm-bindgen").map(str::to_string) else {
        return Requirement::new(
            key,
            platform,
            Check::skip(
                CheckId::DepsWasmBindgenCli,
                "the app's Cargo.lock has no wasm-bindgen",
            ),
        );
    };

    match tools::wasm_bindgen(&version, probe.env) {
        Ok(found) => Requirement::new(
            key,
            platform,
            Check::pass(
                CheckId::DepsWasmBindgenCli,
                format!(
                    "wasm-bindgen {version}: {} ({})",
                    found.path.display(),
                    found.source
                ),
            ),
        ),
        Err(error) => {
            let mut check = Check::from_error(error, Status::Fail);
            if let Some(package) = lock.named("wasm-bindgen").next() {
                check = check.evidence(lock.evidence(package));
            }
            Requirement::new(key, platform, check).with_fixes(
                vec![Fix::WasmBindgen {
                    version: Some(version),
                    lock: lock_path,
                }],
                probe.env,
            )
        }
    }
}

/// An unsupported-host requirement: FAIL when the platform was asked for,
/// SKIP otherwise.
fn unsupported(probe: &Probe<'_>, platform: Platform, why: &str) -> Requirement {
    let check = if probe.explicit {
        Check::fail(CheckId::EnvUnsupportedHost, why)
    } else {
        Check::skip(CheckId::EnvUnsupportedHost, why)
    };
    Requirement::new(format!("host:{}", platform.as_str()), Some(platform), check)
}

/// The first `backquoted` word of a detail (the toolchain rustup named).
fn quoted(detail: &str) -> Option<String> {
    let start = detail.find('`')? + 1;
    let end = start + detail[start..].find('`')?;
    Some(detail[start..end].to_string())
}
