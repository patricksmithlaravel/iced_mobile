//! The macOS release pipeline (design §11.4, §9.6, §12.4): a signed `.app`
//! in two stages around the owner's notarization, `icm verify macos` and
//! `icm diagnose notarytool`.
//!
//! **Stage 1, `icm release macos`** (on a macOS host, else
//! `env.unsupported_host`):
//!
//! 1. the signing identity ([`sign`]): `[desktop.macos] identity` from
//!    host.toml `signing_keychain` / `ICM_KEYCHAIN` or the user's search
//!    list; `--sign none` signs ad hoc;
//! 2. `cargo build` for the host's Apple triple (`--universal` or
//!    `[desktop.macos] universal`: `aarch64-apple-darwin` and
//!    `x86_64-apple-darwin`, then `lipo -create`) in the release target
//!    directory, with `MACOSX_DEPLOYMENT_TARGET` = `[desktop.macos]
//!    min_os` stamped (a changed value relinks the app);
//! 3. gates on the binary: `macos.arch` (every slice the release needs)
//!    and `macos.min_os` (each slice's `LC_BUILD_VERSION` minos equals
//!    `min_os`; arm64 cannot go below 11.0);
//! 4. the dSYM (`dsymutil`, `macos.dsym` WARN when `dwarfdump
//!    --debug-line` names none of the package's sources), zipped as
//!    `symbols`;
//! 5. `<Name>.app`: `Contents/{Info.plist, PkgInfo, MacOS/<bin> (strip
//!    -S), Resources/{AppIcon.icns (iconutil), THIRD_PARTY_NOTICES.txt,
//!    [app] resources}}`, `plutil -lint` (`macos.bundle`);
//! 6. `xattr -cr`, then `codesign --force --options runtime` with the
//!    identity (a secure timestamp for a Developer ID) and the
//!    entitlements `[app.permissions]` needs; a codesign that waits two
//!    minutes is `macos.sign.keychain_prompt` (the owner's);
//! 7. gates: `macos.sign.verify` (`codesign --verify --strict --deep`),
//!    `macos.hardened_runtime` (`codesign -d` shows the `runtime` flag),
//!    and Gatekeeper's verdict explained (INFO `macos.gatekeeper`: spctl
//!    rejects every app until it is notarized);
//! 8. `ditto -c -k --keepParent` into `<Name>-<version>.app.zip`, the file
//!    the owner submits to the notary service (`upload`); the `.app` stays
//!    in the dist directory for stapling (`stage`); UPLOAD.md has the
//!    notarize and staple commands ([`super::owner_plans::macos_app`]).
//!
//! **Stage 2, `icm release macos --dmg`**, after the owner stapled the
//! app: `macos.not_stapled` (`xcrun stapler validate`, the owner's),
//! `codesign --verify` on the app again, the app and an `Applications`
//! link in `hdiutil create -format UDZO`, the DMG signed, then
//! `macos.sign.verify` and `macos.dmg` (`hdiutil verify`), and UPLOAD.md
//! with stage 2 ([`super::owner_plans::macos_dmg`]). The dist directory
//! keeps stage 1's files.
//!
//! **`icm verify macos`** runs the gates on a `.app`, a `.app.zip` (unzipped
//! with ditto) or a `.dmg` (`hdiutil verify`, then the app inside, attached
//! read-only and detached again); `--after-notarize` requires a stapled
//! ticket (`macos.not_stapled`) and Gatekeeper's acceptance
//! (`macos.gatekeeper`) instead of explaining its verdict. `artifacts.json`
//! records the cdhash of the app's and the DMG's signatures, so a file the
//! owner stapled since the release, whose sha256 changed, passes
//! `release.artifact_changed` when codesign still verifies it with that
//! cdhash and `stapler validate` finds the ticket ([`stapled_change`]).
//!
//! icm never notarizes or staples: those commands exist only in
//! `owner_plans.rs`.

pub mod bundle;
pub mod notary;
pub mod sign;

use super::desktop::{self, icons};
use super::diagnose::Input;
use super::dist::FileEntry;
use super::manifest::NoticesAt;
use super::verify::Verify;
use super::{Pipeline, Release, notices, owner_plans};
use crate::cargo::Select;
use crate::catalogue::CheckId;
use crate::cli::{ReleaseTarget, SignMode};
use crate::context::Ctx;
use crate::error::{Check, Evidence, IcmError, Result, Status};
use crate::plan::{Plan, Step};
use crate::platform::ios_sim::macho;
use crate::process::{Cmd, Outcome};
use serde_json::{Value, json};
use sign::Signer;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The macOS pipeline.
pub struct Macos;

/// The lowest minimum macOS of an arm64 slice.
const ARM64_FLOOR: (u32, u32, u32) = (11, 0, 0);

/// The Apple triples a release builds, with their Mach-O arch names.
pub fn triples(universal: bool) -> Vec<(&'static str, &'static str)> {
    if universal {
        return vec![
            ("aarch64-apple-darwin", "arm64"),
            ("x86_64-apple-darwin", "x86_64"),
        ];
    }
    match crate::toolchain::host_triple() {
        "x86_64-apple-darwin" => vec![("x86_64-apple-darwin", "x86_64")],
        _ => vec![("aarch64-apple-darwin", "arm64")],
    }
}

fn universal(rel: &Release) -> bool {
    rel.args.universal || rel.config().desktop.macos.universal
}

/// `<Name>.app`.
pub fn app_name(rel: &Release) -> String {
    crate::platform::ios_sim::bundle::bundle_name(&rel.config().app.name)
}

fn stem(app: &str) -> &str {
    app.strip_suffix(".app").unwrap_or(app)
}

fn app_zip_name(rel: &Release) -> String {
    format!("{}-{}.app.zip", stem(&app_name(rel)), rel.version)
}

fn dmg_name(rel: &Release) -> String {
    format!("{}-{}.dmg", stem(&app_name(rel)), rel.version)
}

fn dsym_zip_name(rel: &Release) -> String {
    format!("{}-{}.dSYM.zip", stem(&app_name(rel)), rel.version)
}

/// The notices' path inside the `.app`.
pub const NOTICES_IN_APP: &str = "Contents/Resources/THIRD_PARTY_NOTICES.txt";

/// codesign, with a timeout read as a keychain prompt.
fn codesign(ctx: &Ctx, name: &str, cmd: &Cmd, signer: &Signer) -> Result<()> {
    match ctx.step(name, cmd) {
        Ok(outcome) if outcome.success() => Ok(()),
        Ok(outcome) => Err(ctx.step_failure(name, CheckId::ToolFailed, &outcome)),
        Err(error) if error.id == CheckId::StepTimeout.id() && signer.identity.is_some() => {
            Err(IcmError::new(
                CheckId::MacosSignKeychainPrompt,
                format!(
                    "codesign did not finish within {}: it is most likely waiting for someone to allow access to the private key of \"{}\"",
                    crate::time::format_duration(sign::CODESIGN_TIMEOUT),
                    signer.identity.as_ref().map(|i| i.name.as_str()).unwrap_or("")
                ),
            )
            .evidence(error.evidence.first().cloned().unwrap_or_else(|| Evidence::file(".")))
            .fix(
                "The owner unlocks the keychain and allows codesign (\"Always Allow\"), or sets host.toml signing_keychain (or ICM_KEYCHAIN) to an unlocked build keychain whose key partition list admits codesign.",
                &[],
            ))
        }
        Err(error) => Err(error),
    }
}

// ---- the identity ------------------------------------------------------------------

/// Resolves the signer and reports the identity check (see [`sign`]).
fn resolve_signer(ctx: &mut Ctx, rel: &mut Release) -> Result<Signer> {
    let env = ctx.env.clone();
    let keychain = ctx.host()?.signing_keychain(&env);
    let configured = rel.config().desktop.macos.identity.clone();
    let none = rel.sign() == SignMode::None;

    if let Some(keychain) = &keychain
        && !keychain.is_file()
    {
        let error = IcmError::new(
            CheckId::MacosSignNoDeveloperId,
            format!(
                "the signing keychain {} (host.toml signing_keychain or ICM_KEYCHAIN) does not exist",
                crate::paths::display(keychain)
            ),
        )
        .fix(
            "The owner points host.toml signing_keychain (or ICM_KEYCHAIN) at the keychain that holds the Developer ID Application identity.",
            &[],
        );
        rel.needs_owner(ctx, error);
        return Ok(Signer::ad_hoc());
    }

    let outcome = ctx.step(
        "security.find-identity",
        &sign::find_identity_cmd(keychain.as_deref()),
    )?;
    if !outcome.success() {
        let error = ctx.step_failure(
            "security.find-identity",
            CheckId::MacosSignNoDeveloperId,
            &outcome,
        );
        rel.needs_owner(ctx, error);
        return Ok(Signer::ad_hoc());
    }
    let identities = sign::parse_identities(&outcome.stdout_text());
    match sign::choose(&configured, &identities, keychain.as_deref()) {
        Ok(identity) if none => {
            rel.check(
                ctx,
                Check::info(
                    CheckId::MacosSignNoDeveloperId,
                    format!(
                        "\"{}\" is in {}; --sign none signs ad hoc",
                        identity.name,
                        sign::searched(keychain.as_deref())
                    ),
                )
                .fix("Release without --sign none to sign with it.", &[]),
            );
            Ok(Signer::ad_hoc())
        }
        Ok(identity) => {
            if identity.is_developer_id() {
                rel.check(
                    ctx,
                    Check::pass(
                        CheckId::MacosSignNoDeveloperId,
                        format!("signing with \"{}\" ({})", identity.name, identity.sha1),
                    ),
                );
            } else {
                let error = IcmError::new(
                    CheckId::MacosSignNoDeveloperId,
                    format!(
                        "[desktop.macos] identity names \"{}\"{}, which is not a Developer ID Application identity Apple issued: the app is signed with it, but Apple's notary service and Gatekeeper refuse it",
                        identity.name,
                        identity
                            .problem
                            .as_deref()
                            .map(|p| format!(" ({p})"))
                            .unwrap_or_default()
                    ),
                )
                .fix(
                    "The owner installs a Developer ID Application identity and sets [desktop.macos] identity = \"auto\" (or names it).",
                    &["security find-identity -v -p codesigning"],
                );
                rel.needs_owner_later(ctx, error);
            }
            Ok(Signer {
                identity: Some(identity),
                keychain,
            })
        }
        Err(error) => {
            rel.needs_owner(ctx, error);
            Ok(Signer::ad_hoc())
        }
    }
}

/// The signer preconditions chose (kept in `rel.signing`).
fn signer_of(rel: &Release) -> Signer {
    let value = &rel.signing;
    match (
        value.get("identity_sha1").and_then(Value::as_str),
        value.get("identity").and_then(Value::as_str),
    ) {
        (Some(sha1), Some(name)) => Signer {
            identity: Some(sign::Identity {
                sha1: sha1.to_string(),
                name: name.to_string(),
                valid: value["developer_id"].as_bool().unwrap_or(false),
                problem: None,
            }),
            keychain: value
                .get("keychain")
                .and_then(Value::as_str)
                .map(PathBuf::from),
        },
        _ => Signer::ad_hoc(),
    }
}

fn signing_json(signer: &Signer) -> Value {
    signer.json()
}

// ---- gates -------------------------------------------------------------------------

fn parse_os(text: &str) -> (u32, u32, u32) {
    let mut parts = text
        .trim()
        .split('.')
        .map(|p| p.parse::<u32>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

fn os_string((major, minor, patch): (u32, u32, u32)) -> String {
    if patch == 0 {
        format!("{major}.{minor}")
    } else {
        format!("{major}.{minor}.{patch}")
    }
}

/// `macos.arch` and `macos.min_os` on a binary: the slices `expected`
/// names (any when `None`), each a macOS slice whose minos is `min_os`
/// (arm64: at least 11.0).
pub fn binary_checks(binary: &Path, expected: Option<&[&str]>, min_os: &str) -> Vec<Check> {
    let shown = crate::paths::display(binary);
    let versions = match macho::build_versions(binary) {
        Ok(versions) if !versions.is_empty() => versions,
        Ok(_) => {
            return vec![
                Check::fail(
                    CheckId::MacosMinOs,
                    format!("{shown} has no LC_BUILD_VERSION: it was not linked for macOS by a current toolchain"),
                )
                .evidence(Evidence::file(binary)),
            ];
        }
        Err(error) => {
            return vec![
                Check::fail(
                    CheckId::MacosArch,
                    format!("cannot read the Mach-O: {error}"),
                )
                .evidence(Evidence::file(binary)),
            ];
        }
    };
    let mut checks = Vec::new();
    let archs: Vec<&str> = versions.iter().map(|v| v.arch.as_str()).collect();
    match expected {
        Some(expected) => {
            let missing: Vec<&str> = expected
                .iter()
                .copied()
                .filter(|arch| !archs.contains(arch))
                .collect();
            checks.push(if missing.is_empty() {
                Check::pass(CheckId::MacosArch, format!("{shown}: {}", archs.join(", ")))
            } else {
                Check::fail(
                    CheckId::MacosArch,
                    format!(
                        "{shown} has {} but lacks {}",
                        archs.join(", "),
                        missing.join(", ")
                    ),
                )
                .evidence(Evidence::file(binary))
            });
        }
        None => checks.push(
            Check::info(CheckId::MacosArch, format!("{shown}: {}", archs.join(", ")))
                .fix("Nothing to fix: the architectures the binary has.", &[]),
        ),
    }
    let floor = parse_os(min_os);
    let mut wrong = Vec::new();
    for version in &versions {
        if version.platform != macho::PLATFORM_MACOS {
            wrong.push(format!(
                "the {} slice is for {}, not macOS",
                version.arch,
                macho::platform_name(version.platform)
            ));
            continue;
        }
        let expected = if version.arch.starts_with("arm64") {
            floor.max(ARM64_FLOOR)
        } else {
            floor
        };
        if version.minos != expected {
            wrong.push(format!(
                "the {} slice has minos {} where {} is expected",
                version.arch,
                version.minos_string(),
                os_string(expected)
            ));
        }
    }
    checks.push(if wrong.is_empty() {
        Check::pass(
            CheckId::MacosMinOs,
            format!(
                "minos {} ({}), as [desktop.macos] min_os {min_os} wants",
                versions
                    .iter()
                    .map(|v| format!("{} {}", v.arch, v.minos_string()))
                    .collect::<Vec<_>>()
                    .join(", "),
                shown
            ),
        )
    } else {
        Check::fail(
            CheckId::MacosMinOs,
            format!("{shown}: {} (LSMinimumSystemVersion {min_os})", wrong.join("; ")),
        )
        .evidence(Evidence::file(binary))
        .fix(
            "Build through icm, which sets MACOSX_DEPLOYMENT_TARGET and relinks when it changes; a binary built by plain cargo keeps the default.",
            &["icm release macos --json -q"],
        )
    });
    checks
}

/// `macos.sign.verify` and, for an app, `macos.hardened_runtime`, from
/// codesign; returns codesign's description of the signature.
fn signature_checks(ctx: &Ctx, path: &Path, checks: &mut Vec<Check>) -> Result<sign::Display> {
    let shown = crate::paths::display(path);
    let outcome = ctx.step("codesign.verify", &sign::verify_cmd(path))?;
    checks.push(if outcome.success() {
        Check::pass(
            CheckId::MacosSignVerify,
            format!("{shown}: codesign --verify --strict passes"),
        )
    } else {
        let mut check = Check::fail(
            CheckId::MacosSignVerify,
            format!(
                "{shown}: codesign --verify fails: {}",
                desktop::said(&outcome, 3)
            ),
        );
        if let Some(log) = &outcome.log {
            check = check.evidence(Evidence::file(log));
        }
        check
    });
    let outcome = ctx.step("codesign.display", &sign::display_cmd(path))?;
    let display = sign::parse_display(&format!(
        "{}\n{}",
        outcome.stdout_text(),
        outcome.stderr_text()
    ));
    if path.extension().is_some_and(|ext| ext == "app") {
        checks.push(if display.hardened() {
            Check::pass(
                CheckId::MacosHardenedRuntime,
                format!("{shown}: flags {}", display.flags.join(",")),
            )
        } else {
            let mut check = Check::fail(
                CheckId::MacosHardenedRuntime,
                format!(
                    "{shown} is not signed with the hardened runtime (flags: {}); Apple's notary service refuses it",
                    if display.flags.is_empty() {
                        "none".to_string()
                    } else {
                        display.flags.join(",")
                    }
                ),
            );
            if let Some(log) = &outcome.log {
                check = check.evidence(Evidence::file(log));
            }
            check
        });
    }
    Ok(display)
}

/// Gatekeeper's verdict: explained (INFO) before notarization, a gate
/// (`macos.gatekeeper`) after it.
fn gatekeeper_check(ctx: &Ctx, path: &Path, required: bool) -> Result<Check> {
    let shown = crate::paths::display(path);
    let outcome = ctx.step("spctl.assess", &sign::spctl_cmd(path))?;
    let text = format!("{}\n{}", outcome.stdout_text(), outcome.stderr_text());
    let assessment = sign::assess(&text, outcome.success());
    let detail = format!(
        "{shown}: {}{}",
        assessment.explanation,
        assessment
            .source
            .as_deref()
            .map(|s| format!(" (source={s})"))
            .unwrap_or_default()
    );
    Ok(match (assessment.accepted, required) {
        (true, _) => Check::pass(CheckId::MacosGatekeeper, detail),
        (false, false) => Check::info(CheckId::MacosGatekeeper, detail).fix(
            "Nothing to fix before notarization: `icm verify macos --after-notarize` requires Gatekeeper's acceptance once the owner has notarized and stapled it.",
            &[],
        ),
        (false, true) => {
            let mut check = Check::fail(CheckId::MacosGatekeeper, detail);
            if let Some(log) = &outcome.log {
                check = check.evidence(Evidence::file(log));
            }
            check
        }
    })
}

/// `macos.not_stapled`: `xcrun stapler validate`.
fn stapled_check(ctx: &Ctx, path: &Path) -> Result<Check> {
    let shown = crate::paths::display(path);
    let outcome = ctx.step("stapler.validate", &sign::stapler_validate_cmd(path))?;
    Ok(if outcome.success() {
        Check::pass(
            CheckId::MacosNotStapled,
            format!("{shown} has its notarization ticket stapled"),
        )
    } else {
        let mut check = Check::fail(
            CheckId::MacosNotStapled,
            format!(
                "{shown} has no stapled notarization ticket: {}",
                desktop::said(&outcome, 2)
            ),
        )
        .fix(
            "The owner notarizes and staples it with the commands in UPLOAD.md.",
            &["icm upload-commands macos"],
        );
        if let Some(log) = &outcome.log {
            check = check.evidence(Evidence::file(log));
        }
        check
    })
}

/// `macos.dsym`: `dwarfdump --debug-line` names a source of the package.
fn dsym_check(ctx: &Ctx, rel: &Release, dsym: &Path) -> Result<Check> {
    let outcome = ctx.step(
        "dwarfdump.debug-line",
        &Cmd::tool("dwarfdump")
            .arg("--debug-line")
            .arg(dsym)
            .timeout(Duration::from_secs(300)),
    )?;
    // The line tables name the crate's own files relative to its
    // directory (`"src/lib.rs"`); std's and dependencies' sit elsewhere.
    let dir = rel.package.manifest_path.parent().unwrap_or(Path::new("/"));
    // DWARF 4 may also split it: `include_directories[n] = "src"` and
    // `name: "main.rs"`.
    let text = outcome.stdout_text();
    let named = rel
        .package
        .targets
        .iter()
        .filter(|target| {
            target
                .kind
                .iter()
                .any(|kind| kind == "lib" || kind == "bin")
        })
        .filter_map(|target| target.src_path.strip_prefix(dir).ok())
        .any(|relative| {
            let whole = format!("\"{}\"", relative.display());
            let split = match (relative.parent(), relative.file_name()) {
                (Some(parent), Some(file)) if !parent.as_os_str().is_empty() => {
                    text.contains(&format!("= \"{}\"", parent.display()))
                        && text.contains(&format!("\"{}\"", file.to_string_lossy()))
                }
                _ => false,
            };
            text.contains(&whole) || split
        });
    let package_dir = dir.display().to_string();
    Ok(if outcome.success() && named {
        Check::pass(
            CheckId::MacosDsym,
            format!(
                "{}: line tables name the package's sources",
                crate::paths::display(dsym)
            ),
        )
    } else {
        Check::warn(
            CheckId::MacosDsym,
            format!(
                "{}: dwarfdump --debug-line names no source under {package_dir}; crash reports will not symbolicate",
                crate::paths::display(dsym)
            ),
        )
        .evidence(Evidence::file(dsym))
    })
}

// ---- stage 1 -----------------------------------------------------------------------

/// `ditto -c -k --keepParent ... <src> <zip>`.
fn ditto_zip(src: &Path, zip: &Path) -> Cmd {
    Cmd::tool("ditto")
        .args([
            "-c",
            "-k",
            "--keepParent",
            "--norsrc",
            "--noextattr",
            "--noqtn",
            "--noacl",
        ])
        .arg(src)
        .arg(zip)
        .timeout(Duration::from_secs(600))
}

fn build_app(ctx: &mut Ctx, rel: &mut Release) -> Result<()> {
    let config = rel.config().clone();
    let project = rel.project.clone();
    let bin = project.bin_for(super::ledger::platform_key(ReleaseTarget::Macos))?;
    let min_os = config.desktop.macos.min_os.clone();
    let triples = triples(universal(rel));
    let signer = signer_of(rel);
    let work = rel.gen_dir.join("app");
    desktop::fresh_dir(&work)?;

    // 1. Build each slice.
    let mut slices = Vec::new();
    for (triple, arch) in &triples {
        let invocation = rel.invocation("build", Select::Bin(bin.clone()), Some(triple));
        let output = rel.cargo(
            ctx,
            &format!("cargo.build.{arch}"),
            &invocation,
            &[],
            Some(&min_os),
        )?;
        let exe = output
            .executable(&bin)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| rel.artifacts_dir(Some(triple)).join(&bin));
        slices.push(exe);
    }
    let binary = work.join(&bin);
    if slices.len() > 1 {
        let mut cmd = Cmd::tool("lipo").arg("-create").arg("-output").arg(&binary);
        for slice in &slices {
            cmd = cmd.arg(slice);
        }
        let _ = desktop::run(ctx, "lipo.create", &cmd, CheckId::ToolFailed)?;
    } else {
        desktop::copy_file(&slices[0], &binary, 0o755)?;
    }
    let expected: Vec<&str> = triples.iter().map(|(_, arch)| *arch).collect();
    for check in binary_checks(&binary, Some(&expected), &min_os) {
        rel.check(ctx, check);
    }

    // 2. The dSYM, before stripping.
    let app = app_name(rel);
    let dsym = work.join(format!("{app}.dSYM"));
    let _ = desktop::run(
        ctx,
        "dsymutil",
        &Cmd::tool("dsymutil")
            .arg(&binary)
            .arg("-o")
            .arg(&dsym)
            .timeout(Duration::from_secs(600)),
        CheckId::ToolFailed,
    )?;
    let check = dsym_check(ctx, rel, &dsym)?;
    rel.check(ctx, check);
    let dsym_zip = rel.dist.join(dsym_zip_name(rel));
    let _ = desktop::run(
        ctx,
        "ditto.dsym",
        &ditto_zip(&dsym, &dsym_zip),
        CheckId::ToolFailed,
    )?;
    rel.add_file("symbols", "dsym", &dsym_zip)?;

    // 3. The bundle.
    let bundle = rel.dist.join(&app);
    desktop::fresh_dir(&bundle)?;
    let contents = bundle.join("Contents");
    let executable = contents.join("MacOS").join(&bin);
    desktop::copy_file(&binary, &executable, 0o755)?;
    let _ = desktop::run(
        ctx,
        "strip",
        &Cmd::tool("strip").arg("-S").arg(&executable),
        CheckId::ToolFailed,
    )?;
    desktop::write_file(&contents.join("PkgInfo"), bundle::PKG_INFO, 0o644)?;

    let icon = icons::load(&project)?;
    let iconset = work.join(format!("{}.iconset", bundle::ICON));
    icons::iconset(&icon, &iconset)?;
    let resources = contents.join("Resources");
    std::fs::create_dir_all(&resources).map_err(|e| desktop::io_error("create", &resources, e))?;
    let _ = desktop::run(
        ctx,
        "iconutil",
        &Cmd::tool("iconutil")
            .args(["-c", "icns"])
            .arg(&iconset)
            .arg("-o")
            .arg(resources.join(format!("{}.icns", bundle::ICON))),
        CheckId::ToolFailed,
    )?;

    let notices_file = rel.notices(ctx, Some(triples[0].0))?;
    desktop::copy_file(&notices_file, &bundle.join(NOTICES_IN_APP), 0o644)?;
    for relative in desktop::resources(&project)? {
        desktop::copy_file(
            &project.dir().join(&relative),
            &resources.join(&relative),
            0o644,
        )?;
    }

    let plist = bundle::info_plist(
        &config,
        &bundle::Inputs {
            executable: &bin,
            version: &rel.version,
            icon: true,
        },
    );
    let info = contents.join("Info.plist");
    desktop::write_file(
        &info,
        crate::platform::ios_sim::plist::to_xml(&Value::Object(plist.clone())).as_bytes(),
        0o644,
    )?;
    let lint = ctx.step("plutil.lint", &Cmd::tool("plutil").arg("-lint").arg(&info))?;
    let missing = bundle::missing_required(&plist);
    rel.check(
        ctx,
        if lint.success() && missing.is_empty() {
            Check::pass(
                CheckId::MacosBundle,
                format!("{app}: Info.plist lints, {} keys", plist.len()),
            )
        } else {
            Check::fail(
                CheckId::MacosBundle,
                format!(
                    "{app}: Info.plist {}{}",
                    if lint.success() {
                        "lints"
                    } else {
                        "fails plutil -lint"
                    },
                    if missing.is_empty() {
                        String::new()
                    } else {
                        format!(", lacks {}", missing.join(", "))
                    }
                ),
            )
            .evidence(Evidence::file(&info))
        },
    );

    // 4. Sign.
    let entitlements = bundle::entitlements(&config.app.permissions);
    let entitlements_path = (!entitlements.is_empty()).then(|| work.join("entitlements.plist"));
    if let Some(path) = &entitlements_path {
        desktop::write_file(
            path,
            crate::platform::ios_sim::plist::to_xml(&Value::Object(entitlements)).as_bytes(),
            0o644,
        )?;
    }
    let _ = desktop::run(
        ctx,
        "xattr.clear",
        &Cmd::tool("xattr").arg("-cr").arg(&bundle),
        CheckId::ToolFailed,
    )?;
    codesign(
        ctx,
        "codesign.app",
        &signer.codesign(&bundle, true, entitlements_path.as_deref()),
        &signer,
    )?;

    // 5. Gates on the signed app.
    let mut checks = Vec::new();
    let display = signature_checks(ctx, &bundle, &mut checks)?;
    checks.push(gatekeeper_check(ctx, &bundle, false)?);
    for check in checks {
        rel.check(ctx, check);
    }

    // 6. The zip the owner notarizes.
    let zip = rel.dist.join(app_zip_name(rel));
    let _ = desktop::run(
        ctx,
        "ditto.app",
        &ditto_zip(&bundle, &zip),
        CheckId::ToolFailed,
    )?;

    rel.embed_notices(&bundle, NOTICES_IN_APP)?;
    rel.embed_notices(&zip, &format!("{app}/{NOTICES_IN_APP}"))?;
    rel.add_file("stage", "app", &bundle)?;
    rel.set_cdhash(&bundle, display.cdhash.clone());
    rel.add_file("upload", "app_zip", &zip)?;

    let mut signing = signing_json(&signer);
    signing["hardened_runtime"] = json!(display.hardened());
    signing["team"] = json!(display.team);
    signing["authority"] = json!(display.authority);
    rel.signing = signing;
    rel.signed = rel.sign() == SignMode::Auto && signer.developer_id();
    rel.owner_plan = Some(owner_plans::macos_app(
        &rel.common(),
        &app_zip_name(rel),
        &app,
    ));
    Ok(())
}

// ---- stage 2 -----------------------------------------------------------------------

/// The stage-1 `.app` in the dist directory, from its `artifacts.json`.
fn staged_app(rel: &Release) -> Result<PathBuf> {
    let missing = |why: String| {
        IcmError::new(
            CheckId::ReleaseNotFound,
            format!(
                "{why}: `icm release macos --dmg` packages the notarized app of `icm release macos` ({} {}+{})",
                rel.config().app.name,
                rel.version,
                rel.build
            ),
        )
        .fix(
            "Run stage 1 first, have the owner notarize and staple the app (UPLOAD.md), then run --dmg.",
            &["icm release macos --json -q", "icm upload-commands macos"],
        )
    };
    let Some(manifest) = rel.previous_manifest() else {
        return Err(missing(format!(
            "{} has no artifacts.json",
            crate::paths::display(&rel.dist)
        )));
    };
    let Some(entry) = manifest.files.iter().find(|file| file.kind == "app") else {
        return Err(missing(format!(
            "{} lists no .app",
            crate::paths::display(&rel.dist.join(super::manifest::FILE))
        )));
    };
    let app = entry.absolute(&rel.dist);
    if !app.is_dir() {
        return Err(missing(format!("{} is gone", crate::paths::display(&app))));
    }
    Ok(app)
}

fn build_dmg(ctx: &mut Ctx, rel: &mut Release) -> Result<()> {
    let app = staged_app(rel)?;
    let previous = rel.previous_manifest().expect("checked by staged_app");
    let signer = signer_of(rel);
    let app_file = app
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| app_name(rel));

    // The stage-1 files, the zip now an input of this stage.
    for (name, version) in &previous.tools {
        if !rel.tools.contains_key(name) {
            rel.tool(name, version.clone());
        }
    }
    for file in &previous.files {
        if file.kind == "dmg" || !file.absolute(&rel.dist).exists() {
            continue;
        }
        let role = if file.kind == "app_zip" {
            "stage"
        } else {
            file.role.as_str()
        };
        // The app is stapled now: new bytes, the same signature.
        let path = file.absolute(&rel.dist);
        rel.add_file(role, &file.kind, &path)?;
        rel.set_cdhash(&path, file.cdhash.clone());
    }
    for place in &previous.notices {
        if place.artifact.ends_with(".dmg") {
            continue;
        }
        rel.notices.push(NoticesAt {
            artifact: place.artifact.clone(),
            path: place.path.clone(),
        });
    }

    let stage = rel.gen_dir.join("dmg");
    desktop::fresh_dir(&stage)?;
    let _ = desktop::run(
        ctx,
        "ditto.stage",
        &Cmd::tool("ditto").arg(&app).arg(stage.join(&app_file)),
        CheckId::ToolFailed,
    )?;
    std::os::unix::fs::symlink("/Applications", stage.join("Applications"))
        .map_err(|e| desktop::io_error("link", &stage.join("Applications"), e))?;
    let dmg = rel.dist.join(dmg_name(rel));
    let _ = std::fs::remove_file(&dmg);
    let _ = desktop::run(
        ctx,
        "hdiutil.create",
        &Cmd::tool("hdiutil")
            .args(["create", "-volname"])
            .arg(&rel.config().app.name)
            .arg("-srcfolder")
            .arg(&stage)
            .args(["-fs", "HFS+", "-format", "UDZO", "-ov"])
            .arg(&dmg)
            .timeout(Duration::from_secs(900)),
        CheckId::ToolFailed,
    )?;
    codesign(
        ctx,
        "codesign.dmg",
        &signer.codesign(&dmg, false, None),
        &signer,
    )?;

    let mut checks = Vec::new();
    let display = signature_checks(ctx, &dmg, &mut checks)?;
    checks.push(dmg_check(ctx, &dmg)?);
    checks.push(gatekeeper_check(ctx, &dmg, false)?);
    for check in checks {
        rel.check(ctx, check);
    }

    rel.embed_notices(&dmg, &format!("{app_file}/{NOTICES_IN_APP}"))?;
    rel.add_file("upload", "dmg", &dmg)?;
    rel.set_cdhash(&dmg, display.cdhash);
    rel.signing = signing_json(&signer);
    rel.signed = rel.sign() == SignMode::Auto && signer.developer_id();
    rel.owner_plan = Some(owner_plans::macos_dmg(&rel.common(), &dmg_name(rel)));
    Ok(())
}

/// `macos.dmg`: `hdiutil verify`.
fn dmg_check(ctx: &Ctx, dmg: &Path) -> Result<Check> {
    let outcome = ctx.step(
        "hdiutil.verify",
        &Cmd::tool("hdiutil")
            .arg("verify")
            .arg(dmg)
            .timeout(Duration::from_secs(600)),
    )?;
    let shown = crate::paths::display(dmg);
    Ok(if outcome.success() {
        Check::pass(CheckId::MacosDmg, format!("{shown}: hdiutil verify passes"))
    } else {
        let mut check = Check::fail(
            CheckId::MacosDmg,
            format!(
                "{shown}: hdiutil verify fails: {}",
                desktop::said(&outcome, 2)
            ),
        );
        if let Some(log) = &outcome.log {
            check = check.evidence(Evidence::file(log));
        }
        check
    })
}

// ---- the plan ----------------------------------------------------------------------

fn plan_signer(rel: &Release) -> Signer {
    match rel.sign() {
        SignMode::None => Signer::ad_hoc(),
        SignMode::Auto => Signer {
            identity: Some(sign::Identity {
                sha1: "<sha1 of the Developer ID Application identity>".to_string(),
                name: rel.config().desktop.macos.identity.clone(),
                valid: true,
                problem: None,
            }),
            keychain: None,
        },
    }
}

fn plan(rel: &Release) -> Plan {
    let mut plan = Plan::new();
    let app = app_name(rel);
    let bundle = rel.dist.join(&app);
    let signer = plan_signer(rel);
    plan.push(Step::internal(
        "macos.host",
        "a macOS host (else env.unsupported_host)",
    ));
    plan.push(
        Step::exec("security.find-identity", sign::find_identity_cmd(None))
            .gate(CheckId::MacosSignNoDeveloperId),
    );
    if rel.args.dmg {
        plan.push(
            Step::exec("stapler.validate", sign::stapler_validate_cmd(&bundle))
                .gate(CheckId::MacosNotStapled),
        );
        let stage = rel.gen_dir.join("dmg");
        let dmg = rel.dist.join(dmg_name(rel));
        plan.push(Step::exec(
            "ditto.stage",
            Cmd::tool("ditto").arg(&bundle).arg(stage.join(&app)),
        ));
        plan.push(Step::exec(
            "hdiutil.create",
            Cmd::tool("hdiutil")
                .args(["create", "-volname"])
                .arg(&rel.config().app.name)
                .arg("-srcfolder")
                .arg(&stage)
                .args(["-fs", "HFS+", "-format", "UDZO", "-ov"])
                .arg(&dmg),
        ));
        plan.push(Step::exec(
            "codesign.dmg",
            signer.codesign(&dmg, false, None),
        ));
        plan.push(
            Step::exec("codesign.verify", sign::verify_cmd(&dmg)).gate(CheckId::MacosSignVerify),
        );
        plan.push(
            Step::exec(
                "hdiutil.verify",
                Cmd::tool("hdiutil").arg("verify").arg(&dmg),
            )
            .gate(CheckId::MacosDmg),
        );
        plan.push(Step::exec("spctl.assess", sign::spctl_cmd(&dmg)).gate(CheckId::MacosGatekeeper));
        return plan;
    }
    let bin = rel
        .project
        .bin_for(super::ledger::platform_key(ReleaseTarget::Macos))
        .unwrap_or_else(|_| rel.package.name.clone());
    let triples = triples(universal(rel));
    for (triple, arch) in &triples {
        let invocation = rel.invocation("build", Select::Bin(bin.clone()), Some(triple));
        plan.push(
            Step::exec(
                &format!("cargo.build.{arch}"),
                invocation.cmd().env(
                    "MACOSX_DEPLOYMENT_TARGET",
                    &rel.config().desktop.macos.min_os,
                ),
            )
            .on_fail(CheckId::BuildCompileError),
        );
    }
    let work = rel.gen_dir.join("app");
    let binary = work.join(&bin);
    if triples.len() > 1 {
        let mut cmd = Cmd::tool("lipo").arg("-create").arg("-output").arg(&binary);
        for (triple, _) in &triples {
            cmd = cmd.arg(rel.artifacts_dir(Some(triple)).join(&bin));
        }
        plan.push(Step::exec("lipo.create", cmd));
    }
    plan.push(
        Step::internal("macos.binary", "read each slice's LC_BUILD_VERSION")
            .gate(CheckId::MacosArch)
            .gate(CheckId::MacosMinOs),
    );
    let dsym = work.join(format!("{app}.dSYM"));
    plan.push(Step::exec(
        "dsymutil",
        Cmd::tool("dsymutil").arg(&binary).arg("-o").arg(&dsym),
    ));
    plan.push(
        Step::exec(
            "dwarfdump.debug-line",
            Cmd::tool("dwarfdump").arg("--debug-line").arg(&dsym),
        )
        .gate(CheckId::MacosDsym),
    );
    plan.push(Step::exec(
        "ditto.dsym",
        ditto_zip(&dsym, &rel.dist.join(dsym_zip_name(rel))),
    ));
    plan.push(Step::internal(
        "macos.bundle",
        &format!(
            "assemble {}: Info.plist, PkgInfo, MacOS/{bin} (strip -S), Resources/AppIcon.icns, Resources/{}, [app] resources",
            crate::paths::display(&bundle),
            notices::FILE
        ),
    ));
    plan.push(Step::exec(
        "iconutil",
        Cmd::tool("iconutil")
            .args(["-c", "icns"])
            .arg(work.join("AppIcon.iconset"))
            .arg("-o")
            .arg(bundle.join("Contents/Resources/AppIcon.icns")),
    ));
    plan.push(
        Step::exec(
            "plutil.lint",
            Cmd::tool("plutil")
                .arg("-lint")
                .arg(bundle.join("Contents/Info.plist")),
        )
        .gate(CheckId::MacosBundle),
    );
    plan.push(Step::exec(
        "xattr.clear",
        Cmd::tool("xattr").arg("-cr").arg(&bundle),
    ));
    plan.push(Step::exec(
        "codesign.app",
        signer.codesign(&bundle, true, None),
    ));
    plan.push(
        Step::exec("codesign.verify", sign::verify_cmd(&bundle)).gate(CheckId::MacosSignVerify),
    );
    plan.push(
        Step::exec("codesign.display", sign::display_cmd(&bundle))
            .gate(CheckId::MacosHardenedRuntime),
    );
    plan.push(Step::exec("spctl.assess", sign::spctl_cmd(&bundle)).gate(CheckId::MacosGatekeeper));
    plan.push(Step::exec(
        "ditto.app",
        ditto_zip(&bundle, &rel.dist.join(app_zip_name(rel))),
    ));
    plan
}

// ---- the pipeline ------------------------------------------------------------------

impl Pipeline for Macos {
    fn plan(&self, _ctx: &Ctx, rel: &Release) -> Result<Plan> {
        Ok(plan(rel))
    }

    fn preconditions(&self, ctx: &mut Ctx, rel: &mut Release) -> Result<()> {
        desktop::require_host(&ctx.env, ReleaseTarget::Macos, "release macos")?;
        if let Some(version) = ctx
            .probe(&Cmd::tool("sw_vers").arg("-productVersion"))
            .ok()
            .filter(Outcome::success)
            .map(|outcome| outcome.stdout_text().trim().to_string())
            .filter(|version| !version.is_empty())
        {
            rel.tool("macos", version);
        }

        if !rel.args.dmg && universal(rel) {
            let toolchain = crate::toolchain::active(rel.project.dir())?;
            let wanted: Vec<String> = triples(true).iter().map(|(t, _)| t.to_string()).collect();
            let checks = crate::toolchain::check_targets(&toolchain, &wanted);
            let mut first = None;
            for check in checks {
                if check.status == Status::Fail && first.is_none() {
                    first = Some(check.error.clone());
                }
                rel.check(ctx, check);
            }
            if let Some(error) = first {
                return Err(error);
            }
        }

        let signer = resolve_signer(ctx, rel)?;
        rel.signing = signing_json(&signer);

        if rel.args.dmg {
            let app = staged_app(rel)?;
            let check = stapled_check(ctx, &app)?;
            if check.status == Status::Fail {
                rel.needs_owner(ctx, check.error);
            } else {
                rel.check(ctx, check);
            }
            let mut checks = Vec::new();
            let _ = signature_checks(ctx, &app, &mut checks)?;
            let mut broken = None;
            for check in checks {
                if check.status == Status::Fail && broken.is_none() {
                    broken = Some(check.error.clone());
                }
                rel.check(ctx, check);
            }
            if let Some(error) = broken {
                return Err(error.fix(
                    "The app changed after stage 1: rerun `icm release macos`, then notarize and staple it again.",
                    &["icm release macos --json -q"],
                ));
            }
        }
        Ok(())
    }

    fn keeps_dist(&self, rel: &Release) -> bool {
        rel.args.dmg
    }

    fn build(&self, ctx: &mut Ctx, rel: &mut Release) -> Result<()> {
        if rel.args.dmg {
            build_dmg(ctx, rel)
        } else {
            build_app(ctx, rel)
        }
    }

    fn verify(&self, ctx: &mut Ctx, verify: &mut Verify) -> Result<()> {
        desktop::require_host(&ctx.env, ReleaseTarget::Macos, "verify macos")?;
        let Some(artifact) = verify.artifact.clone() else {
            return Ok(());
        };
        let scratch = scratch_dir(verify)?;
        let result = verify_artifact(ctx, verify, &artifact, &scratch);
        let _ = std::fs::remove_dir_all(&scratch);
        result
    }

    fn changed_file(
        &self,
        ctx: &Ctx,
        file: &FileEntry,
        path: &Path,
        now: (u64, &str),
    ) -> Result<Option<Check>> {
        let Some(recorded) = file.cdhash.as_deref() else {
            return Ok(None);
        };
        if !matches!(file.kind.as_str(), "app" | "dmg")
            || desktop::require_host(&ctx.env, ReleaseTarget::Macos, "verify macos").is_err()
        {
            return Ok(None);
        }
        stapled_change(ctx, file, path, recorded, now).map(Some)
    }
}

/// `release.artifact_changed` for a signed `.app` or `.dmg` whose bytes
/// changed since the release. Stapling a notarization ticket does that (it
/// adds `Contents/CodeResources` to an app and grows a disk image's
/// signature) without changing the signature, so the change passes when
/// codesign still verifies the file, its code directory hash is the one
/// `artifacts.json` recorded and a ticket is stapled to it.
fn stapled_change(
    ctx: &Ctx,
    file: &FileEntry,
    path: &Path,
    recorded: &str,
    (bytes, sha256): (u64, &str),
) -> Result<Check> {
    let verified = ctx
        .step("codesign.verify", &sign::verify_cmd(path))?
        .success();
    let outcome = ctx.step("codesign.display", &sign::display_cmd(path))?;
    let display = sign::parse_display(&format!(
        "{}\n{}",
        outcome.stdout_text(),
        outcome.stderr_text()
    ));
    let stapled = ctx
        .step("stapler.validate", &sign::stapler_validate_cmd(path))?
        .success();
    let same = display
        .cdhash
        .as_deref()
        .is_some_and(|cdhash| cdhash.eq_ignore_ascii_case(recorded));
    if verified && same && stapled {
        return Ok(Check::pass(
            CheckId::ReleaseArtifactChanged,
            format!(
                "{}: stapled since the release (now {bytes} bytes with sha256 {sha256}); its signature is the recorded one (cdhash {recorded}) and verifies",
                file.path
            ),
        ));
    }
    let mut why = Vec::new();
    if !verified {
        why.push("codesign --verify fails".to_string());
    }
    if !same {
        why.push(match &display.cdhash {
            Some(cdhash) => {
                format!("its signature's cdhash is {cdhash}, not the recorded {recorded}")
            }
            None => format!("codesign shows no cdhash (recorded {recorded})"),
        });
    }
    if !stapled {
        why.push(
            "no notarization ticket is stapled to it, so stapling does not explain the change"
                .to_string(),
        );
    }
    Ok(Check::fail(
        CheckId::ReleaseArtifactChanged,
        format!(
            "{} changed since the release: {bytes} bytes with sha256 {sha256}, recorded {} with {}; {}",
            file.path,
            file.bytes,
            file.sha256,
            why.join("; ")
        ),
    )
    .evidence(Evidence::file(path)))
}

/// A scratch directory for unzipping or mounting: under the project's
/// `target/icm/tmp`, else icm's cache.
fn scratch_dir(verify: &Verify) -> Result<PathBuf> {
    let base = verify
        .project
        .as_ref()
        .map(|project| project.icm_dir.join("tmp"))
        .unwrap_or_else(|| crate::paths::cache_dir().join("tmp"));
    let dir = base.join(format!("verify-macos-{}", std::process::id()));
    desktop::fresh_dir(&dir)?;
    Ok(dir)
}

fn verify_artifact(
    ctx: &mut Ctx,
    verify: &mut Verify,
    artifact: &Path,
    scratch: &Path,
) -> Result<()> {
    let name = artifact
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if artifact.is_dir() && name.ends_with(".app") {
        return verify_app(ctx, verify, artifact);
    }
    if name.ends_with(".zip") {
        let out = scratch.join("unzipped");
        let _ = desktop::run(
            ctx,
            "ditto.unzip",
            &Cmd::tool("ditto")
                .args(["-x", "-k"])
                .arg(artifact)
                .arg(&out),
            CheckId::ToolFailed,
        )?;
        let app = find_app(&out).ok_or_else(|| {
            IcmError::new(
                CheckId::MacosBundle,
                format!("{name} holds no .app at its top level"),
            )
            .evidence(Evidence::file(artifact))
        })?;
        return verify_app(ctx, verify, &app);
    }
    if name.ends_with(".dmg") {
        return verify_dmg(ctx, verify, artifact, scratch);
    }
    Err(IcmError::new(
        CheckId::UsageBadArgs,
        format!("{name} is not a .app, an .app.zip or a .dmg"),
    )
    .evidence(Evidence::file(artifact)))
}

fn find_app(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.is_dir() && path.extension().is_some_and(|ext| ext == "app"))
}

/// The gates on an `.app`.
fn verify_app(ctx: &mut Ctx, verify: &mut Verify, app: &Path) -> Result<()> {
    let info = app.join("Contents/Info.plist");
    let outcome = ctx.step(
        "plutil.json",
        &Cmd::tool("plutil")
            .args(["-convert", "json", "-o", "-"])
            .arg(&info),
    )?;
    let plist: serde_json::Map<String, Value> =
        serde_json::from_str(outcome.stdout_text().trim()).unwrap_or_default();
    let missing = bundle::missing_required(&plist);
    let shown = crate::paths::display(app);
    verify.check(
        ctx,
        if outcome.success() && missing.is_empty() {
            Check::pass(
                CheckId::MacosBundle,
                format!("{shown}: Info.plist has the required keys"),
            )
        } else {
            Check::fail(
                CheckId::MacosBundle,
                format!(
                    "{shown}: Info.plist {}",
                    if outcome.success() {
                        format!("lacks {}", missing.join(", "))
                    } else {
                        "cannot be read".to_string()
                    }
                ),
            )
            .evidence(Evidence::file(&info))
        },
    );
    let min_os = verify
        .project
        .as_ref()
        .map(|project| project.config.config.desktop.macos.min_os.clone())
        .or_else(|| {
            plist
                .get("LSMinimumSystemVersion")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    if let (Some(executable), Some(min_os)) = (
        plist.get("CFBundleExecutable").and_then(Value::as_str),
        min_os,
    ) {
        for check in binary_checks(&app.join("Contents/MacOS").join(executable), None, &min_os) {
            verify.check(ctx, check);
        }
    }
    let mut checks = Vec::new();
    let _ = signature_checks(ctx, app, &mut checks)?;
    if verify.after_notarize {
        checks.push(stapled_check(ctx, app)?);
    }
    checks.push(gatekeeper_check(ctx, app, verify.after_notarize)?);
    for check in checks {
        verify.check(ctx, check);
    }
    if verify.dir.is_none() {
        let check = match notices::presence(app, NOTICES_IN_APP) {
            notices::Presence::Present => Check::pass(
                CheckId::ReleaseNotices,
                format!("{shown} carries {NOTICES_IN_APP}"),
            ),
            _ => Check::fail(
                CheckId::ReleaseNotices,
                format!("{shown} lacks {NOTICES_IN_APP}"),
            )
            .evidence(Evidence::file(app)),
        };
        verify.check(ctx, check);
    }
    Ok(())
}

/// The gates on a `.dmg`, then on the app inside it.
fn verify_dmg(ctx: &mut Ctx, verify: &mut Verify, dmg: &Path, scratch: &Path) -> Result<()> {
    let mut checks = Vec::new();
    let _ = signature_checks(ctx, dmg, &mut checks)?;
    checks.push(dmg_check(ctx, dmg)?);
    if verify.after_notarize {
        checks.push(stapled_check(ctx, dmg)?);
    }
    checks.push(gatekeeper_check(ctx, dmg, verify.after_notarize)?);
    for check in checks {
        verify.check(ctx, check);
    }

    let mount = scratch.join("mount");
    std::fs::create_dir_all(&mount).map_err(|e| desktop::io_error("create", &mount, e))?;
    let _ = desktop::run(
        ctx,
        "hdiutil.attach",
        &Cmd::tool("hdiutil")
            .args([
                "attach",
                "-nobrowse",
                "-readonly",
                "-noautoopen",
                "-mountpoint",
            ])
            .arg(&mount)
            .arg(dmg)
            .timeout(Duration::from_secs(300)),
        CheckId::MacosDmg,
    )?;
    let result = match find_app(&mount) {
        Some(app) => verify_app(ctx, verify, &app),
        None => {
            verify.check(
                ctx,
                Check::fail(
                    CheckId::MacosDmg,
                    format!("{} holds no .app", crate::paths::display(dmg)),
                )
                .evidence(Evidence::file(dmg)),
            );
            Ok(())
        }
    };
    let detach = ctx.step(
        "hdiutil.detach",
        &Cmd::tool("hdiutil")
            .arg("detach")
            .arg(&mount)
            .timeout(Duration::from_secs(120)),
    );
    if !detach.as_ref().is_ok_and(Outcome::success) {
        let _ = ctx.step(
            "hdiutil.detach.force",
            &Cmd::tool("hdiutil")
                .args(["detach", "-force"])
                .arg(&mount)
                .timeout(Duration::from_secs(120)),
        );
    }
    result
}

/// `icm diagnose notarytool`.
pub fn diagnose(ctx: &mut Ctx, input: &Input) -> Result<()> {
    let (checks, error) = notary::report(&input.text, input.evidence.as_ref());
    let accepted = checks
        .first()
        .is_some_and(|check| check.status == Status::Pass);
    let count = checks.len();
    for check in checks {
        ctx.rep.check(check);
    }
    if accepted {
        ctx.rep.next(
            "icm upload-commands macos",
            "the next step in UPLOAD.md staples the ticket",
        );
    }
    match error {
        Some(error) => {
            ctx.rep.summary(format!(
                "notarytool: {} ({count} finding(s))",
                error.detail.lines().next().unwrap_or("")
            ));
            Err(error)
        }
        None => {
            ctx.rep.summary(format!(
                "notarytool: {}",
                if accepted {
                    "accepted"
                } else {
                    "nothing blocks"
                }
            ));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slices_are_gated_on_arch_and_minimum_os() {
        let dir = tempfile::tempdir().unwrap();
        let thin = dir.path().join("thin");
        std::fs::write(
            &thin,
            macho::synthetic(macho::PLATFORM_MACOS, (12, 0, 0), (26, 0, 0)),
        )
        .unwrap();
        let checks = binary_checks(&thin, Some(&["arm64"]), "12.0");
        assert!(
            checks.iter().all(|c| c.status == Status::Pass),
            "{checks:?}"
        );
        // A floor below 11.0: arm64 slices are linked at 11.0.
        let low = dir.path().join("low");
        std::fs::write(
            &low,
            macho::synthetic(macho::PLATFORM_MACOS, (11, 0, 0), (26, 0, 0)),
        )
        .unwrap();
        assert!(
            binary_checks(&low, None, "10.15")
                .iter()
                .all(|c| c.status != Status::Fail)
        );
        let checks = binary_checks(&thin, Some(&["arm64", "x86_64"]), "13.0");
        let failed: Vec<&str> = checks
            .iter()
            .filter(|c| c.status == Status::Fail)
            .map(|c| c.id())
            .collect();
        assert_eq!(failed, ["macos.arch", "macos.min_os"]);
        assert!(checks[1].error.detail.contains("minos 12.0 where 13.0"));
        // An iOS slice is not a macOS binary.
        let ios = dir.path().join("ios");
        std::fs::write(
            &ios,
            macho::synthetic(macho::PLATFORM_IOS, (12, 0, 0), (26, 0, 0)),
        )
        .unwrap();
        let checks = binary_checks(&ios, None, "12.0");
        assert!(checks[1].error.detail.contains("not macOS"));
        // Not a Mach-O.
        let text = dir.path().join("text");
        std::fs::write(&text, "hello, this is not a binary at all").unwrap();
        assert_eq!(binary_checks(&text, None, "12.0")[0].status, Status::Fail);
    }

    #[test]
    fn universal_builds_name_both_triples() {
        assert_eq!(triples(true).len(), 2);
        assert_eq!(triples(false).len(), 1);
        assert_eq!(parse_os("12.0"), (12, 0, 0));
        assert_eq!(os_string((10, 15, 7)), "10.15.7");
    }
}
