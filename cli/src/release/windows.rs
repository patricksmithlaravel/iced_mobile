//! The Windows release pipeline (design §11.5, §9.6, Appendix C item 20):
//! an `.msi` (WiX v5) and an NSIS `-setup.exe`.
//!
//! It runs on a Windows host only (`env.unsupported_host` elsewhere): the
//! MSVC linker and C runtime, `rc.exe`, WiX and `signtool` are Windows
//! tools. This build of icm does not run on Windows hosts yet (its process,
//! signal and lock handling is Unix-only), so the pipeline is exercised by
//! icm's tests against fake tools (`ICM_HOST_OS=windows`), and its
//! real-host acceptance waits for that support (design §17, the
//! windows-latest job).
//!
//! 1. Preconditions: `windows.msi_version` (exit 3: major and minor at most
//!    255, patch at most 65535, `[app] build` at most 65535), the Windows
//!    SDK's `rc.exe` (and `signtool` when signing; `windows.sdk_missing`),
//!    WiX (`wix`) for `msi` and `makensis` for `nsis` in `[desktop.windows]
//!    formats`. The core already checked `sign_command` and its `sign_env`
//!    variables (`windows.sign.not_configured`).
//! 2. `app.ico` (PNG frames 16 to 256 px) and `app.rc` (the icon and
//!    VERSIONINFO: FileVersion `X.Y.Z.build`, ProductVersion `X.Y.Z`),
//!    compiled with `rc.exe` into `app-<hash>.res` (the name changes with
//!    the content, so cargo relinks).
//! 3. `cargo rustc --release --locked --target x86_64-pc-windows-msvc` in
//!    the release target directory, with the static C runtime for every
//!    crate (`--config target.x86_64-pc-windows-msvc.rustflags=["-C",
//!    "target-feature=+crt-static"]`) and `-- -C link-arg=<res>`.
//! 4. Gates on the executable: `windows.pe_imports` (no VC++ runtime DLL in
//!    its imports, read from the PE; a machine with Visual Studio has the
//!    runtime, so only the imports tell) and `windows.subsystem` (WARN for
//!    a console executable).
//! 5. Under `--sign auto`, `sign_command` signs the executable, then each
//!    installer: `{file}` is replaced by the path and `%VAR%` by `${VAR}`,
//!    and it runs with `sh -c` (Git for Windows' sh), so the secrets reach
//!    the tool only through the environment.
//! 6. `wix build -arch x64` of `app.wxs` (per-machine, Program Files, a
//!    Start-menu shortcut, `MajorUpgrade` with the UpgradeCode derived from
//!    `[app] id`) and `makensis` of `installer.nsi` (per-user,
//!    `%LOCALAPPDATA%\Programs\<Name>`, an uninstaller and an Apps &
//!    features entry). Both install `THIRD_PARTY_NOTICES.txt` and `[app]
//!    resources` next to the executable.
//! 7. `windows.signed`: `signtool verify /pa /v` on the executable and both
//!    installers (a WARN under `--sign none`).
//!
//! `icm verify windows` reads the PE gates on any host and runs signtool on
//! Windows.

pub mod files;

use super::desktop::{self, icons, pe};
use super::verify::Verify;
use super::{Pipeline, Release, owner_plans};
use crate::cargo::Select;
use crate::catalogue::{By, CheckId};
use crate::cli::{ReleaseTarget, SignMode};
use crate::context::Ctx;
use crate::error::{Check, Evidence, IcmError, Result};
use crate::plan::{Plan, Step};
use crate::process::{Cmd, Outcome};
use crate::tools::Env;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The Windows pipeline.
pub struct Windows;

/// The target every Windows release builds.
pub const TRIPLE: &str = "x86_64-pc-windows-msvc";

/// The `--config` that links the static C runtime into every crate
/// (Appendix C item 20).
pub const CRT_STATIC: &str =
    r#"target.x86_64-pc-windows-msvc.rustflags=["-C", "target-feature=+crt-static"]"#;

/// The WiX version the documented install pins.
pub const WIX_VERSION: &str = "5.0.2";

fn which(env: &Env, name: &str) -> Option<PathBuf> {
    let path = env.var("PATH")?;
    std::env::split_paths(path).find_map(|dir| {
        [name.to_string(), format!("{name}.exe")]
            .iter()
            .map(|file| dir.join(file))
            .find(|candidate| candidate.is_file())
    })
}

/// A Windows SDK tool (`rc`, `signtool`): `ICM_TOOL_<NAME>`, else the
/// newest `Windows Kits\10\bin\<version>\x64\<name>.exe`, else `PATH`.
pub fn kits_tool(env: &Env, name: &str) -> Option<PathBuf> {
    if let Some(path) = env.tool_override(name) {
        return Some(path);
    }
    let roots: Vec<PathBuf> = ["ProgramFiles(x86)", "ProgramFiles"]
        .iter()
        .filter_map(|var| env.var(var))
        .map(|dir| {
            PathBuf::from(dir)
                .join("Windows Kits")
                .join("10")
                .join("bin")
        })
        .collect();
    for root in roots {
        let mut versions: Vec<PathBuf> = std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|dir| dir.join("x64").join(format!("{name}.exe")).is_file())
            .collect();
        versions.sort_by_key(|dir| {
            crate::tools::numeric_parts(&dir.file_name().unwrap_or_default().to_string_lossy())
        });
        if let Some(newest) = versions.last() {
            return Some(newest.join("x64").join(format!("{name}.exe")));
        }
    }
    which(env, name)
}

/// The tools a Windows release runs.
#[derive(Clone, Debug, Default)]
pub struct Tools {
    /// rc.exe.
    pub rc: Option<PathBuf>,
    /// signtool.exe.
    pub signtool: Option<PathBuf>,
    /// wix.
    pub wix: Option<PathBuf>,
    /// makensis.
    pub makensis: Option<PathBuf>,
}

/// Finds the tools; `formats` and whether the release signs decide which
/// are required.
pub fn find_tools(env: &Env, formats: &[String], signing: bool) -> Result<Tools> {
    let tools = Tools {
        rc: kits_tool(env, "rc"),
        signtool: kits_tool(env, "signtool"),
        wix: env.tool_override("wix").or_else(|| which(env, "wix")),
        makensis: env
            .tool_override("makensis")
            .or_else(|| which(env, "makensis"))
            .or_else(|| {
                env.var("ProgramFiles(x86)")
                    .map(|dir| PathBuf::from(dir).join("NSIS").join("makensis.exe"))
                    .filter(|path| path.is_file())
            }),
    };
    let sdk = |what: &str| {
        IcmError::new(
            CheckId::WindowsSdkMissing,
            format!("{what} was not found in the Windows SDK (Windows Kits\\10\\bin\\<version>\\x64) or on PATH"),
        )
        .fix(
            "Install the Windows 10/11 SDK (Visual Studio Installer > Individual components, or `winget install Microsoft.WindowsSDK.10.0.26100`), or set ICM_TOOL_RC / ICM_TOOL_SIGNTOOL.",
            &[],
        )
    };
    if tools.rc.is_none() {
        return Err(sdk("rc.exe"));
    }
    if signing && tools.signtool.is_none() {
        return Err(sdk("signtool.exe"));
    }
    if formats.iter().any(|f| f == "msi") && tools.wix.is_none() {
        return Err(IcmError::new(
            CheckId::EnvToolMissing,
            "WiX (`wix`) is not on PATH; [desktop.windows] formats has \"msi\"",
        )
        .fix(
            "Install WiX v5 (a .NET tool), or drop \"msi\" from [desktop.windows] formats.",
            &[&format!(
                "dotnet tool install --global wix --version {WIX_VERSION}"
            )],
        )
        .by(By::Agent));
    }
    if formats.iter().any(|f| f == "nsis") && tools.makensis.is_none() {
        return Err(IcmError::new(
            CheckId::EnvToolMissing,
            "makensis is not on PATH; [desktop.windows] formats has \"nsis\"",
        )
        .fix(
            "Install NSIS 3, or drop \"nsis\" from [desktop.windows] formats.",
            &["winget install NSIS.NSIS"],
        )
        .by(By::Agent));
    }
    Ok(tools)
}

fn msi_version(rel: &Release) -> Result<(u32, u32, u32)> {
    files::msi_version(&rel.version, rel.build).map_err(|detail| {
        IcmError::new(CheckId::WindowsMsiVersion, detail)
            .evidence(Evidence::file(&rel.package.manifest_path))
            .fix(
                "Use a Cargo version with major and minor at most 255 and patch at most 65535, and an [app] build of at most 65535.",
                &[],
            )
    })
}

fn signs(rel: &Release) -> bool {
    rel.sign() == SignMode::Auto && rel.config().desktop.windows.sign_command.is_some()
}

fn formats(rel: &Release) -> Vec<String> {
    rel.config().desktop.windows.formats.clone()
}

/// `windows.pe_imports` and `windows.subsystem` on an executable.
pub fn pe_checks(exe: &Path) -> Vec<Check> {
    let shown = crate::paths::display(exe);
    let image = match pe::read(exe) {
        Ok(image) => image,
        Err(error) => {
            return vec![
                Check::fail(
                    CheckId::WindowsPeImports,
                    format!("cannot read the PE: {error}"),
                )
                .evidence(Evidence::file(exe)),
            ];
        }
    };
    let mut checks = Vec::new();
    let runtime = image.runtime_dlls();
    checks.push(if runtime.is_empty() {
        Check::pass(
            CheckId::WindowsPeImports,
            format!(
                "{shown} ({}) imports no redistributable runtime ({} DLLs{})",
                pe::machine_name(image.machine),
                image.dlls().len(),
                if image.uses_ucrt() {
                    "; the Universal CRT ships with Windows 10 and later"
                } else {
                    ""
                }
            ),
        )
    } else {
        Check::fail(
            CheckId::WindowsPeImports,
            format!(
                "{shown} imports {}: a clean Windows lacks the VC++ runtime, so the installed app would not start there",
                runtime.join(", ")
            ),
        )
        .evidence(Evidence::file(exe))
    });
    checks.push(match image.subsystem {
        pe::SUBSYSTEM_GUI => Check::pass(
            CheckId::WindowsSubsystem,
            format!("{shown}: subsystem {}", pe::subsystem_name(image.subsystem)),
        ),
        other => Check::warn(
            CheckId::WindowsSubsystem,
            format!(
                "{shown}: subsystem {}; a console window opens next to the app",
                pe::subsystem_name(other)
            ),
        )
        .evidence(Evidence::file(exe)),
    });
    checks
}

/// Signs a file with `sign_command` through `sh -c`.
fn sign_file(ctx: &Ctx, rel: &Release, file: &Path, step: &str) -> Result<()> {
    let Some(command) = rel.config().desktop.windows.sign_command.clone() else {
        return Ok(());
    };
    let cmd = Cmd::tool("sh")
        .arg("-c")
        .arg(files::sign_script(&command, file))
        .cwd(rel.project.dir())
        .timeout(Duration::from_secs(600));
    let outcome = ctx.step(step, &cmd)?;
    if outcome.success() {
        Ok(())
    } else {
        Err(ctx
            .step_failure(step, CheckId::ToolFailed, &outcome)
            .fix(
                "Read the step log: [desktop.windows] sign_command failed. Check the variables in sign_env and the signing service.",
                &[],
            ))
    }
}

fn signtool_check(ctx: &Ctx, signtool: &Path, file: &Path) -> Result<Check> {
    let shown = crate::paths::display(file);
    let outcome = ctx.step(
        "signtool.verify",
        &Cmd::new(signtool)
            .args(["verify", "/pa", "/v"])
            .arg(file)
            .timeout(Duration::from_secs(300)),
    )?;
    Ok(if outcome.success() {
        Check::pass(
            CheckId::WindowsSigned,
            format!("{shown}: signtool verify /pa passes"),
        )
    } else {
        let mut check = Check::fail(
            CheckId::WindowsSigned,
            format!(
                "{shown}: signtool verify /pa fails: {}",
                desktop::said(&outcome, 3)
            ),
        );
        if let Some(log) = &outcome.log {
            check = check.evidence(Evidence::file(log));
        }
        check
    })
}

fn run(ctx: &Ctx, name: &str, cmd: &Cmd) -> Result<Outcome> {
    desktop::run(ctx, name, cmd, CheckId::ToolFailed)
}

fn installer_names(rel: &Release) -> (String, String) {
    let stem = desktop::file_stem(&rel.config().app.name);
    (
        format!("{stem}-{}.msi", rel.version),
        format!("{stem}-{}-setup.exe", rel.version),
    )
}

fn rustc_invocation(rel: &Release, bin: &str, res: &Path) -> crate::cargo::Invocation {
    let mut invocation = rel.invocation("rustc", Select::Bin(bin.to_string()), Some(TRIPLE));
    invocation.config.push(CRT_STATIC.to_string());
    invocation.trailing = vec!["-C".to_string(), format!("link-arg={}", res.display())];
    invocation
}

fn record_version(ctx: &Ctx, rel: &mut Release, name: &str, cmd: Cmd) {
    if let Ok(outcome) = ctx.probe(&cmd.timeout(Duration::from_secs(60)))
        && outcome.success()
    {
        let text = format!("{}{}", outcome.stdout_text(), outcome.stderr_text());
        if let Some(line) = text.lines().map(str::trim).find(|l| !l.is_empty()) {
            rel.tool(name, line.to_string());
        }
    }
}

fn build(ctx: &mut Ctx, rel: &mut Release) -> Result<()> {
    let project = rel.project.clone();
    let config = rel.config().clone();
    let version = msi_version(rel)?;
    let formats = formats(rel);
    let signing = signs(rel);
    let tools = find_tools(&ctx.env, &formats, signing)?;
    let bin = project.bin_for(super::ledger::platform_key(ReleaseTarget::Windows))?;
    let work = rel.gen_dir.join("installer");
    desktop::fresh_dir(&work)?;

    // 1. The icon and the resource file.
    let icon = icons::load(&project)?;
    let ico_bytes = icons::ico(&icon);
    let ico = work.join("app.ico");
    desktop::write_file(&ico, &ico_bytes, 0o644)?;
    let mut facts = files::Facts {
        name: config.app.name.clone(),
        id: config.app.id.clone(),
        publisher: desktop::publisher(&project),
        copyright: config.app.copyright.clone(),
        description: config.app.description.clone(),
        version,
        build: rel.build,
        bin: bin.clone(),
        exe: work.join(format!("{bin}.exe")),
        icon: ico.clone(),
        notices: PathBuf::new(),
        resources: Vec::new(),
    };
    let rc_text = files::rc(&facts, "app.ico");
    desktop::write_file(&work.join("app.rc"), rc_text.as_bytes(), 0o644)?;
    let mut hashed = rc_text.clone().into_bytes();
    hashed.extend_from_slice(&ico_bytes);
    let res = work.join(format!(
        "app-{}.res",
        &crate::hash::sha256_hex(&hashed)[..12]
    ));
    let rc = tools.rc.clone().expect("found by find_tools");
    let _ = run(
        ctx,
        "rc",
        &Cmd::new(&rc)
            .args(["/nologo", "/fo"])
            .arg(&res)
            .arg("app.rc")
            .cwd(&work),
    )?;

    // 2. The executable, with the static C runtime and the resources.
    let invocation = rustc_invocation(rel, &bin, &res);
    let output = rel.cargo(ctx, "cargo.rustc", &invocation, &[], None)?;
    let built = output
        .executable(&bin)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| rel.artifacts_dir(Some(TRIPLE)).join(format!("{bin}.exe")));
    for check in pe_checks(&built) {
        rel.check(ctx, check);
    }
    desktop::copy_file(&built, &facts.exe, 0o755)?;
    if signing {
        sign_file(ctx, rel, &facts.exe, "sign.exe")?;
    }

    // 3. What the installers carry.
    facts.notices = rel.notices(ctx, Some(TRIPLE))?;
    facts.resources = desktop::resources(&project)?
        .into_iter()
        .map(|relative| (relative.clone(), project.dir().join(&relative)))
        .collect();

    // 4. The installers.
    let (msi_name, setup_name) = installer_names(rel);
    let mut installers: Vec<(PathBuf, &str)> = Vec::new();
    if formats.iter().any(|f| f == "msi") {
        let wix = tools.wix.clone().expect("found by find_tools");
        record_version(ctx, rel, "wix", Cmd::new(&wix).arg("--version"));
        let wxs = work.join("app.wxs");
        desktop::write_file(&wxs, files::wxs(&facts).as_bytes(), 0o644)?;
        let msi = rel.dist.join(&msi_name);
        let _ = run(
            ctx,
            "wix.build",
            &Cmd::new(&wix)
                .args(["build", "-arch", "x64", "-o"])
                .arg(&msi)
                .arg(&wxs)
                .cwd(&work)
                .timeout(Duration::from_secs(900)),
        )?;
        installers.push((msi, "msi"));
    }
    if formats.iter().any(|f| f == "nsis") {
        let makensis = tools.makensis.clone().expect("found by find_tools");
        record_version(ctx, rel, "makensis", Cmd::new(&makensis).arg("-VERSION"));
        let nsi = work.join("installer.nsi");
        let setup = rel.dist.join(&setup_name);
        desktop::write_file(&nsi, files::nsi(&facts, &setup).as_bytes(), 0o644)?;
        let _ = run(
            ctx,
            "makensis",
            // makensis aborts (std::bad_alloc) under the C locale on Unix
            // builds; a UTF-8 one works everywhere.
            &Cmd::new(&makensis)
                .args(["-V2", "-NOCD", "-INPUTCHARSET", "UTF8"])
                .arg(&nsi)
                .env("LC_ALL", "C.UTF-8")
                .cwd(&work)
                .timeout(Duration::from_secs(900)),
        )?;
        installers.push((setup, "exe"));
    }

    // 5. Sign the installers, then check every signature.
    if signing {
        for (path, kind) in &installers {
            sign_file(ctx, rel, path, &format!("sign.{kind}"))?;
        }
        let signtool = tools.signtool.clone().expect("found by find_tools");
        let mut all = true;
        for path in std::iter::once(&facts.exe).chain(installers.iter().map(|(p, _)| p)) {
            let check = signtool_check(ctx, &signtool, path)?;
            all &= check.status == crate::error::Status::Pass;
            rel.check(ctx, check);
        }
        rel.signed = all;
    } else {
        rel.check(
            ctx,
            Check::warn(
                CheckId::WindowsSigned,
                format!(
                    "the executable and {} are unsigned ({}); SmartScreen warns about unsigned installers",
                    installers
                        .iter()
                        .map(|(p, _)| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join(" and "),
                    if rel.sign() == SignMode::None {
                        "--sign none"
                    } else {
                        "[desktop.windows] sign_command is unset"
                    }
                ),
            ),
        );
    }

    let names: Vec<String> = installers
        .iter()
        .map(|(path, _)| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    for (path, kind) in &installers {
        rel.embed_notices(path, super::notices::FILE)?;
        rel.add_file("upload", kind, path)?;
    }
    // References only: the program and the variables it reads, never the
    // command line, which could carry a value config validation missed.
    rel.signing = match &config.desktop.windows.sign_command {
        Some(command) if signing => serde_json::json!({
            "sign_program": files::sign_program(command),
            "sign_env": config.desktop.windows.sign_env,
        }),
        _ => serde_json::Value::Null,
    };
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    rel.owner_plan = Some(owner_plans::desktop(&rel.common(), &refs));
    Ok(())
}

fn plan(rel: &Release) -> Plan {
    let mut plan = Plan::new();
    let work = rel.gen_dir.join("installer");
    let res = work.join("app-<hash>.res");
    let bin = rel
        .project
        .bin_for(super::ledger::platform_key(ReleaseTarget::Windows))
        .unwrap_or_else(|_| rel.package.name.clone());
    let (msi_name, setup_name) = installer_names(rel);
    plan.push(
        Step::internal(
            "windows.host",
            "a Windows host (else env.unsupported_host); rc.exe, signtool, wix and makensis found",
        )
        .gate(CheckId::WindowsMsiVersion),
    );
    plan.push(Step::internal(
        "windows.files",
        &format!(
            "write {} (app.ico, app.rc, app.wxs, installer.nsi)",
            crate::paths::display(&work)
        ),
    ));
    plan.push(Step::exec(
        "rc",
        Cmd::tool("rc")
            .args(["/nologo", "/fo"])
            .arg(&res)
            .arg("app.rc")
            .cwd(&work),
    ));
    plan.push(
        Step::exec("cargo.rustc", rustc_invocation(rel, &bin, &res).cmd())
            .gate(CheckId::WindowsPeImports)
            .gate(CheckId::WindowsSubsystem)
            .on_fail(CheckId::BuildCompileError),
    );
    let sign = |file: &Path| -> Option<Step> {
        let command = rel.config().desktop.windows.sign_command.as_ref()?;
        (rel.sign() == SignMode::Auto).then(|| {
            Step::exec(
                "sign",
                Cmd::tool("sh")
                    .arg("-c")
                    .arg(files::sign_script(command, file)),
            )
        })
    };
    if let Some(step) = sign(&work.join(format!("{bin}.exe"))) {
        plan.push(step);
    }
    let formats = formats(rel);
    let mut installers = Vec::new();
    if formats.iter().any(|f| f == "msi") {
        let msi = rel.dist.join(&msi_name);
        plan.push(Step::exec(
            "wix.build",
            Cmd::tool("wix")
                .args(["build", "-arch", "x64", "-o"])
                .arg(&msi)
                .arg(work.join("app.wxs")),
        ));
        installers.push(msi);
    }
    if formats.iter().any(|f| f == "nsis") {
        plan.push(Step::exec(
            "makensis",
            Cmd::tool("makensis")
                .args(["-V2", "-NOCD", "-INPUTCHARSET", "UTF8"])
                .arg(work.join("installer.nsi")),
        ));
        installers.push(rel.dist.join(&setup_name));
    }
    for installer in &installers {
        if let Some(step) = sign(installer) {
            plan.push(step);
        }
    }
    if signs(rel) {
        for file in std::iter::once(work.join(format!("{bin}.exe"))).chain(installers) {
            plan.push(
                Step::exec(
                    "signtool.verify",
                    Cmd::tool("signtool")
                        .args(["verify", "/pa", "/v"])
                        .arg(file),
                )
                .gate(CheckId::WindowsSigned),
            );
        }
    }
    plan
}

impl Pipeline for Windows {
    fn plan(&self, _ctx: &Ctx, rel: &Release) -> Result<Plan> {
        Ok(plan(rel))
    }

    fn preconditions(&self, ctx: &mut Ctx, rel: &mut Release) -> Result<()> {
        desktop::require_host(&ctx.env, ReleaseTarget::Windows, "release windows")?;
        let (x, y, z) = msi_version(rel)?;
        rel.check(
            ctx,
            Check::pass(
                CheckId::WindowsMsiVersion,
                format!(
                    "ProductVersion {x}.{y}.{z}, FileVersion {x}.{y}.{z}.{}",
                    rel.build
                ),
            ),
        );
        let _ = find_tools(&ctx.env, &formats(rel), signs(rel))?;
        Ok(())
    }

    fn build(&self, ctx: &mut Ctx, rel: &mut Release) -> Result<()> {
        build(ctx, rel)
    }

    fn verify(&self, ctx: &mut Ctx, verify: &mut Verify) -> Result<()> {
        let Some(artifact) = verify.artifact.clone() else {
            return Ok(());
        };
        let bytes = std::fs::read(&artifact).unwrap_or_default();
        if bytes.starts_with(b"MZ") {
            for check in pe_checks(&artifact) {
                verify.check(ctx, check);
            }
        }
        let shown = crate::paths::display(&artifact);
        if verify.gates.mode == SignMode::None {
            verify.check(
                ctx,
                Check::warn(
                    CheckId::WindowsSigned,
                    format!("{shown} is unsigned (released with --sign none)"),
                ),
            );
            return Ok(());
        }
        if desktop::host_os(&ctx.env) != "windows" {
            verify.check(
                ctx,
                Check::skip(
                    CheckId::WindowsSigned,
                    format!(
                        "{shown}: signtool verify runs on Windows; this host is {}",
                        desktop::host_os(&ctx.env)
                    ),
                ),
            );
            return Ok(());
        }
        let Some(signtool) = kits_tool(&ctx.env, "signtool") else {
            return Err(IcmError::new(
                CheckId::WindowsSdkMissing,
                "signtool.exe was not found in the Windows SDK or on PATH",
            ));
        };
        let check = signtool_check(ctx, &signtool, &artifact)?;
        verify.check(ctx, check);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pe_gates_name_the_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let dynamic = dir.path().join("dynamic.exe");
        std::fs::write(
            &dynamic,
            pe::synthetic(
                pe::MACHINE_AMD64,
                pe::SUBSYSTEM_CONSOLE,
                &["KERNEL32.dll", "VCRUNTIME140.dll"],
            ),
        )
        .unwrap();
        let checks = pe_checks(&dynamic);
        assert_eq!(checks[0].status, crate::error::Status::Fail);
        assert!(checks[0].error.detail.contains("vcruntime140.dll"));
        assert_eq!(checks[1].status, crate::error::Status::Warn);
        let static_crt = dir.path().join("static.exe");
        std::fs::write(
            &static_crt,
            pe::synthetic(
                pe::MACHINE_AMD64,
                pe::SUBSYSTEM_GUI,
                &["KERNEL32.dll", "USER32.dll"],
            ),
        )
        .unwrap();
        assert!(
            pe_checks(&static_crt)
                .iter()
                .all(|c| c.status == crate::error::Status::Pass)
        );
        let text = dir.path().join("text.exe");
        std::fs::write(&text, "MZ but nothing else").unwrap();
        assert_eq!(pe_checks(&text)[0].status, crate::error::Status::Fail);
    }

    #[test]
    fn tools_are_found_through_overrides_and_the_sdk() {
        let dir = tempfile::tempdir().unwrap();
        let kits = dir.path().join("Windows Kits/10/bin");
        for version in ["10.0.22621.0", "10.0.26100.0"] {
            let x64 = kits.join(version).join("x64");
            std::fs::create_dir_all(&x64).unwrap();
            std::fs::write(x64.join("rc.exe"), "").unwrap();
        }
        let root = dir.path().display().to_string();
        let env = Env::from_pairs(&[("ProgramFiles(x86)", root.as_str()), ("PATH", "")], None);
        assert_eq!(
            kits_tool(&env, "rc").unwrap(),
            kits.join("10.0.26100.0/x64/rc.exe")
        );
        let error = find_tools(&env, &["msi".to_string()], false).unwrap_err();
        assert_eq!(error.id, "env.tool_missing");
        assert!(error.fix.commands[0].contains("dotnet tool install --global wix --version 5.0.2"));
        let error = find_tools(&env, &[], true).unwrap_err();
        assert_eq!(error.id, "windows.sdk_missing");
        let none = Env::from_pairs(&[("PATH", "")], None);
        assert_eq!(
            find_tools(&none, &[], false).unwrap_err().id,
            "windows.sdk_missing"
        );
    }
}
