//! The iOS release pipeline (design §11.1, §12.2, §9.1-§9.3): an `.ipa`
//! for App Store Connect, its dSYM, and `icm diagnose altool`.
//!
//! - **Preconditions**: a macOS host; Xcode at or above the policy's
//!   `app_store.min_sdk` (`env.xcode_too_old`, owner) and not a beta
//!   (`ios.xcode.not_beta`, a WARN: Appendix C item 1, `altool
//!   --validate-app` is the real gate); the `aarch64-apple-ios` target; the
//!   `DT*` keys of that Xcode; an Apple Distribution identity and an App
//!   Store profile by reference ([`crate::ios::identity`],
//!   [`crate::ios::profile`]), searched in host.toml `signing_keychain` /
//!   `ICM_KEYCHAIN` and `ICM_PROVISIONING_PROFILES` when set. Missing
//!   signing assets are owner items: exit 9 under `--sign auto`, WARNs
//!   under `--sign none`. A named identity the system does not trust is
//!   still used, and the release ends with exit 9 once it is written.
//! - **Build**: `IPHONEOS_DEPLOYMENT_TARGET=<min_os> cargo build --release
//!   --locked --target aarch64-apple-ios` in the release target directory
//!   with line tables; the Mach-O gates; `store.no_agent_bridge`; the
//!   privacy scan; `dsymutil` with the UUID and line-table gates (Appendix
//!   C item 23); a stripped copy for the bundle; THIRD_PARTY_NOTICES; the
//!   device bundle with the flattened icon, Info.plist with `DT*` keys,
//!   PrivacyInfo, entitlements and `embedded.mobileprovision`
//!   ([`crate::ios::bundle`]); the plist, icon and usage gates.
//! - **Sign last** (`xattr -cr`, `codesign` under the 60 s keychain
//!   watchdog; ad hoc under `--sign none`), verify, read the entitlements
//!   back, then `Payload/<Name>.app` and `zip -X` into the `.ipa`, whose
//!   layout and extracted signature are gated again.
//! - **Outputs**: `<Name>.ipa` (upload), `<Name>.app.dSYM.zip` (symbols),
//!   copies of `Info.plist` and `PrivacyInfo.xcprivacy` (metadata), and the
//!   owner's plan ([`super::owner_plans::ios`]).
//!
//! `icm verify ios` runs the same gates on an `.ipa` ([`verify_ipa`]).

use super::diagnose::Input;
use super::notices;
use super::verify::Verify;
use super::{Pipeline, Release, owner_plans};
use crate::cargo::Select;
use crate::catalogue::CheckId;
use crate::cli::SignMode;
use crate::context::Ctx;
use crate::error::{Check, Evidence, IcmError, Result, Status};
use crate::ios::dt::DtKeys;
use crate::ios::identity::{Identity, Role};
use crate::ios::profile::{Kind, Profile, Want};
use crate::ios::{
    bundle, codesign, dsym, dt, entitlements, identity, ipa, macho, privacy, profile,
};
use crate::plan::{Plan, Step};
use crate::platform::ios_sim::macho::{BuildVersion, PLATFORM_IOS, platform_name};
use crate::process::Cmd;
use crate::tools::Xcode;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// The device triple.
pub const TRIPLE: &str = "aarch64-apple-ios";

/// The marker the debug-only agent bridge puts in a binary (design §13.7).
pub const AGENT_MARKER: &[u8] = b"ICM_AGENT_BRIDGE_V1";

/// The iOS pipeline.
pub struct Ios;

/// What the preconditions found, for the build.
struct Prepared {
    xcode: Xcode,
    dt: DtKeys,
    identity: Option<Identity>,
    profile: Option<Profile>,
    keychain: Option<PathBuf>,
}

static PREPARED: Mutex<Option<Prepared>> = Mutex::new(None);

fn internal(detail: impl Into<String>) -> IcmError {
    IcmError::new(CheckId::InternalBug, detail)
}

fn io(what: &str, path: &Path, error: impl std::fmt::Display) -> IcmError {
    internal(format!(
        "cannot {what} {}: {error}",
        crate::paths::display(path)
    ))
}

impl Pipeline for Ios {
    fn plan(&self, _ctx: &Ctx, rel: &Release) -> Result<Plan> {
        Ok(plan(rel))
    }

    fn preconditions(&self, ctx: &mut Ctx, rel: &mut Release) -> Result<()> {
        let prepared = prepare(ctx, rel)?;
        *PREPARED.lock().unwrap_or_else(|e| e.into_inner()) = Some(prepared);
        Ok(())
    }

    fn build(&self, ctx: &mut Ctx, rel: &mut Release) -> Result<()> {
        let prepared = PREPARED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .ok_or_else(|| internal("the iOS build ran without its preconditions"))?;
        build(ctx, rel, prepared)
    }

    fn verify(&self, ctx: &mut Ctx, verify: &mut Verify) -> Result<()> {
        verify_ipa(ctx, verify)
    }
}

// ---- plan ----------------------------------------------------------------------------------

fn plan(rel: &Release) -> Plan {
    let config = rel.config();
    let name = config.app.name.clone();
    let app = crate::platform::ios_sim::bundle::bundle_name(&name);
    let stem = app.trim_end_matches(".app").to_string();
    let bin = rel
        .project
        .bin_for("ios")
        .unwrap_or_else(|_| "<bin>".into());
    let xcrun = || Cmd::tool("xcrun");
    let mut plan = Plan::new();
    plan.push(
        Step::internal(
            "ios.preconditions",
            "Xcode (not beta, at or above the App Store SDK floor), the aarch64-apple-ios target, the DT* keys, the Apple Distribution identity and the App Store profile by reference",
        )
        .gate(CheckId::IosXcodeNotBeta)
        .gate(CheckId::IosSignNoIdentity)
        .gate(CheckId::IosSignNoProfile),
    );
    plan.push(
        Step::exec(
            "cargo.build",
            rel.invocation("build", Select::Bin(bin.clone()), Some(TRIPLE))
                .cmd()
                .env("IPHONEOS_DEPLOYMENT_TARGET", &config.ios.min_os),
        )
        .gate(CheckId::IosMachoPlatform)
        .gate(CheckId::IosMachoMinos)
        .gate(CheckId::IosMachoSdkFloor)
        .gate(CheckId::IosMachoArch)
        .gate(CheckId::StoreNoAgentBridge)
        .gate(CheckId::IosPrivacyReasons)
        .on_fail(CheckId::BuildCompileError),
    );
    let dsym = rel.gen_dir.join(format!("{app}.dSYM"));
    plan.push(
        Step::exec(
            "ios.dsymutil",
            xcrun().arg("dsymutil").arg(&bin).arg("-o").arg(&dsym),
        )
        .gate(CheckId::IosDsymUuid),
    );
    plan.push(
        Step::exec(
            "ios.dwarfdump",
            xcrun()
                .args(["dwarfdump", "--debug-line"])
                .arg(&dsym)
                .args(["-o", "lines.txt"]),
        )
        .gate(CheckId::IosDsymLineTables),
    );
    plan.push(Step::exec(
        "ios.dsym.zip",
        Cmd::new("/usr/bin/zip")
            .args(["-qry", "-X"])
            .arg(rel.dist.join(format!("{app}.dSYM.zip")))
            .arg(format!("{app}.dSYM")),
    ));
    plan.push(Step::exec(
        "ios.strip",
        xcrun().args(["strip", "-S", "-x"]).arg(&bin),
    ));
    plan.push(Step::internal(
        "release.notices",
        "THIRD_PARTY_NOTICES.txt from cargo metadata, into the bundle root",
    ));
    plan.push(
        Step::exec(
            "ios.actool",
            xcrun().args([
                "actool",
                "Assets.xcassets",
                "--compile",
                "actool-out",
                "--platform",
                "iphoneos",
                "--minimum-deployment-target",
                &config.ios.min_os,
                "--app-icon",
                "AppIcon",
                "--target-device",
                "iphone",
            ]),
        )
        .gate(CheckId::IosIconOpaque1024)
        .on_fail(CheckId::IosActoolFailed),
    );
    plan.push(
        Step::internal(
            "ios.generate",
            "Info.plist (managed keys, scene manifest, DT* keys, ITSAppUsesNonExemptEncryption), PrivacyInfo.xcprivacy, the distribution entitlements (checked against the profile), embedded.mobileprovision",
        )
        .gate(CheckId::IosPlistLint)
        .gate(CheckId::IosPlistRequiredKeys)
        .gate(CheckId::IosPlistSceneManifest)
        .gate(CheckId::IosPlistDtKeys)
        .gate(CheckId::IosPlistExportCompliance)
        .gate(CheckId::IosPlistUsageDescriptions)
        .gate(CheckId::IosPrivacyPresent)
        .gate(CheckId::IosEntitlementsNotInProfile),
    );
    plan.push(Step::exec(
        "ios.xattr",
        Cmd::tool("xattr").arg("-cr").arg(&app),
    ));
    let identity = match rel.sign() {
        SignMode::None => codesign::AD_HOC.to_string(),
        SignMode::Auto => format!(
            "<{} identity: {}>",
            Role::Distribution.label(),
            config.ios.signing.distribution.identity
        ),
    };
    let entitlements = Path::new("entitlements.plist");
    plan.push(
        Step::exec(
            "ios.codesign",
            codesign::sign_cmd(
                Path::new(&app),
                &identity,
                config.ios.team_id.as_ref().map(|_| entitlements),
                None,
            ),
        )
        .on_fail(CheckId::IosSignKeychainPrompt),
    );
    plan.push(
        Step::exec("ios.codesign.verify", codesign::verify_cmd(Path::new(&app)))
            .gate(CheckId::IosSignVerify)
            .gate(CheckId::IosEntitlementsGetTaskAllow),
    );
    plan.push(
        Step::exec(
            "ios.ipa.zip",
            Cmd::new("/usr/bin/zip")
                .args(["-q", "-X", "-y"])
                .arg(rel.dist.join(format!("{stem}.ipa")))
                .arg(format!("Payload/{app}")),
        )
        .gate(CheckId::IosIpaLayout)
        .gate(CheckId::IosIpaSignature),
    );
    plan
}

// ---- preconditions -------------------------------------------------------------------------

fn prepare(ctx: &mut Ctx, rel: &mut Release) -> Result<Prepared> {
    if !cfg!(target_os = "macos") {
        return Err(IcmError::new(
            CheckId::EnvUnsupportedHost,
            "an iOS release needs a macOS host with Xcode",
        ));
    }
    let xcode = crate::tools::xcode(&ctx.env)?;
    rel.tool("xcode", xcode.display());
    let floor = crate::policy::get().int("app_store.min_sdk").unwrap_or(26);
    if i64::from(xcode.major()) < floor {
        rel.needs_owner(
            ctx,
            IcmError::new(
                CheckId::EnvXcodeTooOld,
                format!(
                    "Xcode {} is older than {floor}, the oldest App Store Connect accepts (icm print policy)",
                    xcode.display()
                ),
            )
            .evidence(Evidence::file(&xcode.developer_dir)),
        );
    } else {
        rel.check(
            ctx,
            Check::pass(
                CheckId::EnvXcodeTooOld,
                format!("Xcode {} (App Store floor {floor})", xcode.display()),
            ),
        );
    }
    rel.check(
        ctx,
        if xcode.beta {
            Check::warn(
                CheckId::IosXcodeNotBeta,
                format!(
                    "Xcode {} at {} looks like a beta; App Store Connect rejects builds from beta tools",
                    xcode.display(),
                    xcode.developer_dir.display()
                ),
            )
        } else {
            Check::pass(
                CheckId::IosXcodeNotBeta,
                format!("Xcode {} is a release", xcode.display()),
            )
        },
    );

    let toolchain = crate::toolchain::active(rel.package.dir())?;
    for check in crate::toolchain::check_targets(&toolchain, &[TRIPLE.to_string()]) {
        if check.failed() {
            return Err(check.into_error());
        }
        rel.check(ctx, check);
    }

    let dt = dt::read(ctx, &xcode)?;
    rel.tool("sdk", dt.sdk_name.clone());

    let host = ctx.host()?.clone();
    let keychain = host.signing_keychain(&ctx.env);
    let (identity, profile) = signing(ctx, rel, keychain.as_deref())?;
    Ok(Prepared {
        xcode,
        dt,
        identity,
        profile,
        keychain,
    })
}

/// The profile reference with a relative path resolved against the
/// project.
fn profile_reference(rel: &Release, reference: &str) -> String {
    let reference = reference.trim();
    let is_uuid = reference.len() == 36 && reference.matches('-').count() == 4;
    if reference == "auto" || is_uuid {
        reference.to_string()
    } else {
        super::resolve_path(&rel.project, reference)
            .display()
            .to_string()
    }
}

/// The identity and the profile (design §11.1 preconditions).
fn signing(
    ctx: &Ctx,
    rel: &mut Release,
    keychain: Option<&Path>,
) -> Result<(Option<Identity>, Option<Profile>)> {
    let config = rel.config().clone();
    let reference = config.ios.signing.distribution.clone();
    let team = config.ios.team_id.clone();

    let listed = match identity::list(ctx, keychain) {
        Ok(listed) => listed,
        Err(error) => {
            rel.needs_owner(ctx, error);
            return Ok((None, None));
        }
    };
    let candidates = match identity::candidates(
        &reference.identity,
        &listed,
        Role::Distribution,
        team.as_deref(),
    ) {
        Ok(candidates) => candidates,
        Err(error) => {
            rel.needs_owner(
                ctx,
                error.evidence(rel.project.config.evidence("ios.signing.distribution")),
            );
            Vec::new()
        }
    };

    let Some(team) = team else {
        // `[ios] team_id` is the owner's (config.owner_decision, above).
        rel.check(
            ctx,
            Check::skip(
                CheckId::IosSignNoProfile,
                "no [ios] team_id yet: the App Store profile cannot be matched",
            ),
        );
        return Ok((None, None));
    };

    let certificates: Vec<String> = candidates.iter().map(|i| i.sha1.clone()).collect();
    let now = crate::ios::now_unix();
    let want = Want {
        kind: Kind::AppStore,
        team: &team,
        bundle_id: &config.app.id,
        certificates: &certificates,
        device: None,
        now,
    };
    let dirs = profile::search_dirs(&ctx.env);
    let chosen = match profile::choose(&profile_reference(rel, &reference.profile), &dirs, &want) {
        Ok(chosen) => chosen.profile,
        Err(error) => {
            rel.needs_owner(
                ctx,
                error.evidence(rel.project.config.evidence("ios.signing.distribution")),
            );
            return Ok((None, None));
        }
    };

    let days = chosen.days_left(now).unwrap_or(i64::MAX);
    let expires = chosen.expires.clone().unwrap_or_else(|| "?".into());
    if days < profile::EXPIRY_FAIL_DAYS {
        rel.needs_owner(
            ctx,
            IcmError::new(
                CheckId::IosSignProfileExpired,
                format!(
                    "{} expires on {expires}, in {days} day(s): renew it before releasing",
                    chosen.label()
                ),
            )
            .evidence(Evidence::file(&chosen.path))
            .fix(
                "The owner renews the profile in the Apple Developer portal and downloads it.",
                &[],
            ),
        );
    } else if days < profile::EXPIRY_WARN_DAYS {
        rel.check(
            ctx,
            Check::warn(
                CheckId::IosSignProfileExpired,
                format!("{} expires on {expires}, in {days} days", chosen.label()),
            )
            .evidence(Evidence::file(&chosen.path)),
        );
    }
    rel.check(
        ctx,
        Check::pass(
            CheckId::IosSignNoProfile,
            format!(
                "App Store profile {} for {}, expires {expires}",
                chosen.label(),
                chosen.application_identifier().unwrap_or("?")
            ),
        ),
    );

    // The identity: one whose certificate the profile includes.
    let identity = candidates.into_iter().find(|i| {
        chosen
            .certificates
            .iter()
            .any(|c| c.eq_ignore_ascii_case(&i.sha1))
    });
    match &identity {
        Some(identity) if !identity.has_role(Role::Distribution) => {
            rel.needs_owner(
                ctx,
                IcmError::new(
                    CheckId::IosSignNoIdentity,
                    format!("{} is not an Apple Distribution identity", identity.label()),
                )
                .evidence(rel.project.config.evidence("ios.signing.distribution")),
            );
        }
        Some(identity) if !identity.valid() => {
            // Built and signed all the same; the owner fixes the certificate.
            rel.needs_owner_later(
                ctx,
                IcmError::new(
                    CheckId::IosSignNoIdentity,
                    format!(
                        "{} is not a valid identity ({}): App Store Connect will not accept its signature",
                        identity.label(),
                        identity.problem.as_deref().unwrap_or("invalid")
                    ),
                )
                .fix(
                    "The owner installs a valid Apple Distribution certificate (with Apple's intermediate certificates) and points [ios.signing] distribution.identity at it.",
                    &[],
                ),
            );
        }
        Some(identity) => rel.check(
            ctx,
            Check::pass(
                CheckId::IosSignNoIdentity,
                format!("signing identity {}", identity.label()),
            ),
        ),
        None => {}
    }
    Ok((identity, Some(chosen)))
}

// ---- build ---------------------------------------------------------------------------------

/// `1.2` and `1.2.0` are the same version.
fn same_version(a: &str, b: &str) -> bool {
    let parts = |text: &str| -> Vec<u32> {
        let mut parts: Vec<u32> = text
            .trim()
            .split('.')
            .map(|p| p.parse().unwrap_or(u32::MAX))
            .collect();
        while parts.len() > 1 && parts.last() == Some(&0) {
            let _ = parts.pop();
        }
        parts
    };
    parts(a) == parts(b)
}

fn at_least(version: &str, floor: &str) -> bool {
    let parts = |text: &str| -> Vec<u32> {
        text.trim()
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    let (mut a, mut b) = (parts(version), parts(floor));
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    a >= b
}

/// The Mach-O gates (design §12.2): platform IOS (blocking), minos =
/// MinimumOSVersion at or above the store floor, the SDK floor, arm64
/// only, and the SDK the `DT*` keys name.
fn macho_checks(
    exe: &Path,
    versions: &[BuildVersion],
    slices: &[macho::Slice],
    minimum_os: &str,
    dt_sdk: Option<&str>,
) -> Result<Vec<Check>> {
    let shown = crate::paths::display(exe);
    let device: Vec<&BuildVersion> = versions
        .iter()
        .filter(|v| v.platform == PLATFORM_IOS)
        .collect();
    if device.is_empty() || device.len() != versions.len() {
        let found: Vec<String> = versions
            .iter()
            .map(|v| format!("{} {}", v.arch, platform_name(v.platform)))
            .collect();
        return Err(IcmError::new(
            CheckId::IosMachoPlatform,
            format!(
                "{shown} is built for {}, not IOS (devices)",
                if found.is_empty() {
                    "no Apple platform".to_string()
                } else {
                    found.join(", ")
                }
            ),
        )
        .evidence(Evidence::file(exe)));
    }
    let mut checks = vec![Check::pass(
        CheckId::IosMachoPlatform,
        format!(
            "IOS {} minos {} sdk {}",
            device[0].arch,
            device[0].minos_string(),
            device[0].sdk_string()
        ),
    )];

    let floor = crate::policy::get()
        .text("app_store.min_deployment")
        .unwrap_or("13.0")
        .to_string();
    let minos = device[0].minos_string();
    checks.push(if !same_version(&minos, minimum_os) {
        Check::fail(
            CheckId::IosMachoMinos,
            format!("the executable's minos is {minos}, but MinimumOSVersion is {minimum_os}"),
        )
        .evidence(Evidence::file(exe))
    } else if !at_least(&minos, &floor) {
        Check::fail(
            CheckId::IosMachoMinos,
            format!("minos {minos} is below {floor}, the lowest App Store Connect accepts"),
        )
        .evidence(Evidence::file(exe))
    } else {
        Check::pass(
            CheckId::IosMachoMinos,
            format!("minos {minos} = MinimumOSVersion (App Store floor {floor})"),
        )
    });

    let sdk_floor = crate::policy::get().int("app_store.min_sdk").unwrap_or(26);
    let sdk = device[0].sdk_string();
    checks.push(if i64::from(device[0].sdk.0) >= sdk_floor {
        Check::pass(
            CheckId::IosMachoSdkFloor,
            format!("linked with the iOS {sdk} SDK (App Store floor {sdk_floor})"),
        )
    } else {
        Check::fail(
            CheckId::IosMachoSdkFloor,
            format!("linked with the iOS {sdk} SDK; App Store Connect needs {sdk_floor} or newer"),
        )
        .evidence(Evidence::file(exe))
    });

    let archs: Vec<&str> = slices.iter().map(|s| s.arch.as_str()).collect();
    checks.push(
        if !archs.is_empty() && archs.iter().all(|a| *a == "arm64") {
            Check::pass(CheckId::IosMachoArch, "arm64 only")
        } else {
            Check::fail(
                CheckId::IosMachoArch,
                format!(
                    "architectures {}; the App Store takes arm64 only",
                    archs.join(", ")
                ),
            )
            .evidence(Evidence::file(exe))
        },
    );

    if let Some(dt_sdk) = dt_sdk {
        checks.push(if same_version(&sdk, dt_sdk) {
            Check::pass(
                CheckId::IosMachoSdkMatchesDt,
                format!("LC_BUILD_VERSION sdk {sdk} = DTSDKName"),
            )
        } else {
            Check::fail(
                CheckId::IosMachoSdkMatchesDt,
                format!("the executable was linked with SDK {sdk}, but DTSDKName says {dt_sdk}"),
            )
            .evidence(Evidence::file(exe))
        });
    }
    Ok(checks)
}

/// `store.no_agent_bridge`: the debug-only bridge is not in the binary.
fn bridge_check(exe: &Path, bytes: &[u8]) -> Check {
    if macho::contains(bytes, AGENT_MARKER) {
        Check::fail(
            CheckId::StoreNoAgentBridge,
            format!(
                "{} contains the agent bridge (ICM_AGENT_BRIDGE_V1)",
                crate::paths::display(exe)
            ),
        )
        .evidence(Evidence::file(exe))
    } else {
        Check::pass(
            CheckId::StoreNoAgentBridge,
            "no agent bridge in the executable",
        )
    }
}

/// `ios.privacy.reasons`: every required-reason category the executable
/// uses has a declared reason.
fn privacy_check(
    undefined: &[String],
    bytes: &[u8],
    declared: &std::collections::BTreeMap<String, Vec<String>>,
    evidence: Evidence,
) -> Check {
    let found = privacy::scan(undefined, bytes);
    let missing = privacy::undeclared(&found, declared);
    let names: Vec<String> = found
        .iter()
        .map(|f| format!("{} ({})", f.category, f.evidence.join(", ")))
        .collect();
    if missing.is_empty() {
        Check::pass(
            CheckId::IosPrivacyReasons,
            if names.is_empty() {
                "the executable uses no required-reason API".to_string()
            } else {
                format!("declared: {}", names.join("; "))
            },
        )
    } else {
        let line = privacy::suggested_line(declared, &missing);
        let missing: Vec<String> = missing
            .iter()
            .map(|f| format!("{} ({})", f.category, f.evidence.join(", ")))
            .collect();
        Check::fail(
            CheckId::IosPrivacyReasons,
            format!(
                "the executable uses required-reason APIs without a declared reason: {} (ITMS-91053)",
                missing.join("; ")
            ),
        )
        .evidence(evidence)
        .fix(
            format!("Set this in icm.toml [ios.privacy], checking each reason against Apple's list: {line}"),
            &[],
        )
    }
}

/// The package's source directory (the dSYM gate's "crate's own files").
fn source_dir(rel: &Release) -> PathBuf {
    let src = rel.package.dir().join("src");
    let dir = if src.is_dir() {
        src
    } else {
        rel.package.dir().to_path_buf()
    };
    dir.canonicalize().unwrap_or(dir)
}

/// dsymutil, then the UUID and line-table gates.
fn dsym(
    ctx: &Ctx,
    rel: &mut Release,
    xcode: &Xcode,
    exe: &Path,
    slices: &[macho::Slice],
    app_name: &str,
) -> Result<PathBuf> {
    let dsym = rel.gen_dir.join(format!("{app_name}.dSYM"));
    let _ = std::fs::remove_dir_all(&dsym);
    let outcome = ctx.step("ios.dsymutil", &dsym::dsymutil_cmd(xcode, exe, &dsym))?;
    if !outcome.success() || !dsym.is_dir() {
        return Err(ctx.step_failure("ios.dsymutil", CheckId::ToolFailed, &outcome));
    }

    let exe_uuids: Vec<String> = slices.iter().filter_map(|s| s.uuid.clone()).collect();
    let dwarf = dsym::dwarf_file(&dsym);
    let dsym_uuids: Vec<String> = dwarf
        .as_deref()
        .and_then(|file| macho::slices(file).ok())
        .map(|slices| slices.into_iter().filter_map(|s| s.uuid).collect())
        .unwrap_or_default();
    rel.check(
        ctx,
        if !exe_uuids.is_empty() && exe_uuids == dsym_uuids {
            Check::pass(
                CheckId::IosDsymUuid,
                format!("dSYM UUID {} = the executable's", exe_uuids.join(", ")),
            )
        } else {
            Check::fail(
                CheckId::IosDsymUuid,
                format!(
                    "the dSYM's UUID ({}) differs from the executable's ({})",
                    if dsym_uuids.is_empty() {
                        "none".to_string()
                    } else {
                        dsym_uuids.join(", ")
                    },
                    if exe_uuids.is_empty() {
                        "none".to_string()
                    } else {
                        exe_uuids.join(", ")
                    }
                ),
            )
            .evidence(Evidence::file(&dsym))
        },
    );

    let lines = rel.gen_dir.join("dsym-lines.txt");
    let _ = std::fs::remove_file(&lines);
    let outcome = ctx.step("ios.dwarfdump", &dsym::line_table_cmd(xcode, &dsym, &lines))?;
    let src = source_dir(rel);
    let found = if outcome.success() {
        dsym::crate_source(&lines, &src).map_err(|e| io("read", &lines, e))?
    } else {
        None
    };
    rel.check(
        ctx,
        match found {
            Some(file) => Check::pass(
                CheckId::IosDsymLineTables,
                format!(
                    "the dSYM's line table names {}",
                    crate::paths::display(&file)
                ),
            ),
            None => Check::fail(
                CheckId::IosDsymLineTables,
                format!(
                    "the dSYM's line table names no file of {} ({}): crash reports would not symbolicate",
                    rel.package.name,
                    crate::paths::display(&src)
                ),
            )
            .evidence(Evidence::file(if lines.is_file() { &lines } else { &dsym })),
        },
    );
    let _ = std::fs::remove_file(&lines);
    Ok(dsym)
}

/// Zips a directory next to it into the dist directory with `zip -X`.
fn zip_dir(ctx: &Ctx, step: &str, dir: &Path, out: &Path) -> Result<()> {
    let parent = dir.parent().ok_or_else(|| io("zip", dir, "no parent"))?;
    let name = dir.file_name().ok_or_else(|| io("zip", dir, "no name"))?;
    let _ = std::fs::remove_file(out);
    let outcome = ctx.step(
        step,
        &Cmd::new("/usr/bin/zip")
            .args(["-qry", "-X"])
            .arg(out)
            .arg(name)
            .cwd(parent)
            .timeout(Duration::from_secs(600)),
    )?;
    if !outcome.success() {
        return Err(ctx.step_failure(step, CheckId::ToolFailed, &outcome));
    }
    Ok(())
}

/// `ios.plist.export_compliance`: the answer is in the plist. Without it
/// (only possible under `--sign none`, where `config.owner_decision`
/// already warned) a WARN.
fn export_compliance(info: &Map<String, Value>, mode: SignMode, info_path: &Path) -> Check {
    match info
        .get("ITSAppUsesNonExemptEncryption")
        .and_then(Value::as_bool)
    {
        Some(answer) => Check::pass(
            CheckId::IosPlistExportCompliance,
            format!("ITSAppUsesNonExemptEncryption = {answer}"),
        ),
        None => {
            let check = Check::fail(
                CheckId::IosPlistExportCompliance,
                "ITSAppUsesNonExemptEncryption is missing: App Store Connect asks the export-compliance question for every build",
            )
            .evidence(Evidence::file(info_path))
            .fix(
                "The owner answers [ios] uses_non_exempt_encryption in icm.toml.",
                &[],
            );
            if mode == SignMode::None {
                Check {
                    status: Status::Warn,
                    ..check
                }
            } else {
                check
            }
        }
    }
}

fn build(ctx: &mut Ctx, rel: &mut Release, prepared: Prepared) -> Result<()> {
    let project = rel.project.clone();
    let config = rel.config().clone();
    let xcode = prepared.xcode.clone();
    let bin = project.bin_for("ios")?;
    let app_name = crate::platform::ios_sim::bundle::bundle_name(&config.app.name);
    let stem = app_name.trim_end_matches(".app").to_string();
    std::fs::create_dir_all(&rel.gen_dir).map_err(|e| io("create", &rel.gen_dir, e))?;

    // 1. The build.
    let invocation = rel.invocation("build", Select::Bin(bin.clone()), Some(TRIPLE));
    let output = rel.cargo(
        ctx,
        "cargo.build",
        &invocation,
        &[],
        Some(&config.ios.min_os),
    )?;
    let exe = output
        .executable(&bin)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| rel.artifacts_dir(Some(TRIPLE)).join(&bin));
    if !exe.is_file() {
        return Err(internal(format!(
            "cargo reported success but {} does not exist",
            crate::paths::display(&exe)
        )));
    }

    // 2. What the binary says.
    let bytes = std::fs::read(&exe).map_err(|e| io("read", &exe, e))?;
    let slices = macho::parse(&bytes).map_err(|e| {
        IcmError::new(CheckId::BuildWrongPlatform, e).evidence(Evidence::file(&exe))
    })?;
    let versions = macho::build_versions(&exe).map_err(|e| {
        IcmError::new(CheckId::BuildWrongPlatform, e).evidence(Evidence::file(&exe))
    })?;
    for check in macho_checks(
        &exe,
        &versions,
        &slices,
        &config.ios.min_os,
        Some(prepared.dt.sdk_version()),
    )? {
        rel.check(ctx, check);
    }
    rel.check(ctx, bridge_check(&exe, &bytes));
    let undefined: Vec<String> = slices.iter().flat_map(|s| s.undefined.clone()).collect();
    rel.check(
        ctx,
        privacy_check(
            &undefined,
            &bytes,
            &config.ios.privacy.api_reasons,
            rel.project.config.evidence("ios.privacy"),
        ),
    );

    // 3. The dSYM, before the bundled copy is stripped.
    let dsym_dir = dsym(ctx, rel, &xcode, &exe, &slices, &app_name)?;
    let dsym_zip = rel.dist.join(format!("{app_name}.dSYM.zip"));
    zip_dir(ctx, "ios.dsym.zip", &dsym_dir, &dsym_zip)?;
    rel.add_file("symbols", "dsym", &dsym_zip)?;

    // 4. A stripped copy for the bundle.
    let stripped_dir = rel.gen_dir.join("exe");
    std::fs::create_dir_all(&stripped_dir).map_err(|e| io("create", &stripped_dir, e))?;
    let stripped = stripped_dir.join(&bin);
    let _ = std::fs::copy(&exe, &stripped).map_err(|e| io("copy", &exe, e))?;
    let outcome = ctx.step(
        "ios.strip",
        &xcode
            .xcrun()
            .args(["strip", "-S", "-x"])
            .arg(&stripped)
            .timeout(Duration::from_secs(300)),
    )?;
    if !outcome.success() {
        return Err(ctx.step_failure("ios.strip", CheckId::ToolFailed, &outcome));
    }

    // 5. The bundle.
    let notices_file = rel.notices(ctx, Some(TRIPLE))?;
    let mut extra = vec![(notices_file, notices::FILE.to_string())];
    if let Some(profile) = &prepared.profile {
        extra.push((profile.path.clone(), "embedded.mobileprovision".to_string()));
    }
    let out_dir = rel.gen_dir.join("app");
    let device = bundle::assemble(
        ctx,
        &project,
        &xcode,
        &bundle::Inputs {
            gen_dir: &rel.gen_dir,
            out_dir: &out_dir,
            exe: &stripped,
            bin: &bin,
            cargo_version: &rel.version,
            dt: &prepared.dt,
            release: true,
            extra,
        },
    )?;
    let app = device.app.clone();
    let info_path = app.join("Info.plist");
    rel.check(ctx, bundle::lint(ctx, &app)?);
    for check in bundle::plist_checks(&app, &device.info) {
        rel.check(ctx, check);
    }
    let (missing, differ) = dt::compare(&device.info, &prepared.dt);
    rel.check(
        ctx,
        if missing.is_empty() && differ.is_empty() {
            Check::pass(
                CheckId::IosPlistDtKeys,
                format!(
                    "DT* keys of Xcode {} ({}, {})",
                    prepared.dt.xcode_build, prepared.dt.sdk_name, prepared.dt.sdk_build
                ),
            )
        } else {
            Check::fail(
                CheckId::IosPlistDtKeys,
                format!(
                    "Info.plist DT* keys: {}",
                    missing
                        .iter()
                        .map(|k| format!("{k} missing"))
                        .chain(differ)
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            )
            .evidence(Evidence::file(&info_path))
        },
    );
    rel.check(ctx, export_compliance(&device.info, rel.sign(), &info_path));
    rel.check(
        ctx,
        bundle::usage_descriptions(&device.info, &bytes, &info_path),
    );
    let icon = bundle::icon_check(ctx, &xcode, &app, &device.info)?;
    rel.check(ctx, icon);

    // 6. The entitlements.
    let expected = config
        .ios
        .team_id
        .as_deref()
        .map(|team| entitlements::distribution(&config, team));
    let entitlements_path = rel.gen_dir.join("entitlements.plist");
    let _ = std::fs::remove_file(&entitlements_path);
    if let Some(expected) = &expected {
        std::fs::write(
            &entitlements_path,
            crate::platform::ios_sim::plist::to_xml(&Value::Object(expected.clone())),
        )
        .map_err(|e| io("write", &entitlements_path, e))?;
        if let Some(profile) = &prepared.profile {
            let missing = entitlements::not_in_profile(expected, &profile.entitlements);
            rel.check(
                ctx,
                if missing.is_empty() {
                    Check::pass(
                        CheckId::IosEntitlementsNotInProfile,
                        format!(
                            "the profile allows every entitlement ({})",
                            expected.keys().cloned().collect::<Vec<_>>().join(", ")
                        ),
                    )
                } else {
                    Check::fail(
                        CheckId::IosEntitlementsNotInProfile,
                        format!("{} does not allow: {}", profile.label(), missing.join("; ")),
                    )
                    .evidence(Evidence::file(&entitlements_path))
                },
            );
        }
    }

    // 7. Sign last.
    codesign::clear_xattrs(ctx, &app)?;
    let (identity, keychain) = match (rel.sign(), &prepared.identity) {
        (SignMode::Auto, Some(identity)) => (identity.sha1.clone(), prepared.keychain.as_deref()),
        (SignMode::Auto, None) => {
            return Err(internal(
                "a signed release reached signing without an identity",
            ));
        }
        (SignMode::None, _) => (codesign::AD_HOC.to_string(), None),
    };
    codesign::sign(
        ctx,
        &app,
        &identity,
        expected.as_ref().map(|_| entitlements_path.as_path()),
        keychain,
    )?;

    // 8. Verify the signature and what it carries.
    let outcome = codesign::verify(ctx, "ios.codesign.verify", &app)?;
    let signature = codesign::signature(ctx, &app)?;
    let signed_with = if signature.ad_hoc {
        "an ad-hoc signature (--sign none)".to_string()
    } else {
        signature
            .authorities
            .first()
            .cloned()
            .unwrap_or_else(|| "an unknown identity".into())
    };
    rel.check(
        ctx,
        if outcome.success() {
            Check::pass(
                CheckId::IosSignVerify,
                format!("codesign --verify --strict --deep: valid, signed with {signed_with}"),
            )
        } else {
            Check::fail(
                CheckId::IosSignVerify,
                format!("codesign --verify failed: {}", outcome.stderr_tail(4)),
            )
            .evidence(Evidence::file(outcome.log.as_deref().unwrap_or(&app)))
        },
    );
    let signed = codesign::entitlements(ctx, &app)?;
    rel.check(
        ctx,
        match (&expected, &signed) {
            (Some(expected), Some(signed)) if expected == signed => Check::pass(
                CheckId::IosEntitlementsGetTaskAllow,
                "the signature carries the distribution entitlements (get-task-allow false)",
            ),
            (None, None) => Check::pass(
                CheckId::IosEntitlementsGetTaskAllow,
                "no entitlements (no [ios] team_id yet)",
            ),
            (_, Some(signed)) if signed.get("get-task-allow") == Some(&Value::Bool(true)) => {
                Check::fail(
                    CheckId::IosEntitlementsGetTaskAllow,
                    "the release is signed with get-task-allow = true",
                )
                .evidence(Evidence::file(&app))
            }
            _ => Check::fail(
                CheckId::IosEntitlementsGetTaskAllow,
                format!(
                    "the signed entitlements differ from {}",
                    crate::paths::display(&entitlements_path)
                ),
            )
            .evidence(Evidence::file(&app)),
        },
    );

    // 9. The IPA.
    let ipa_name = format!("{stem}.ipa");
    let ipa_path = rel.dist.join(&ipa_name);
    ipa::package(ctx, &app, &rel.gen_dir.join("ipa"), &ipa_path)?;
    for check in ipa_checks(ctx, &ipa_path, &rel.gen_dir.join("ipa-check"))? {
        rel.check(ctx, check);
    }

    // 10. The outputs.
    for (from, name, kind) in [
        (&device.info_path, "Info.plist", "info_plist"),
        (&device.privacy_path, "PrivacyInfo.xcprivacy", "privacy"),
    ] {
        let to = rel.dist.join(name);
        let _ = std::fs::copy(from, &to).map_err(|e| io("copy", from, e))?;
        rel.add_file("metadata", kind, &to)?;
    }
    rel.add_file("upload", "ipa", &ipa_path)?;
    rel.embed_notices(&ipa_path, &format!("Payload/{app_name}/{}", notices::FILE))?;
    rel.signed = rel.sign() == SignMode::Auto && prepared.identity.is_some();
    rel.signing = json!({
        "identity": prepared.identity.as_ref().map(|i| i.name.clone()),
        "identity_sha1": prepared.identity.as_ref().map(|i| i.sha1.clone()),
        "keychain": prepared.keychain.as_deref().map(crate::paths::display),
        "signed_with": signed_with,
        "profile": prepared.profile.as_ref().map(Profile::to_json),
        "entitlements": expected.map(Value::Object),
    });
    rel.owner_plan = Some(owner_plans::ios(&rel.common(), &ipa_name));
    Ok(())
}

/// `ios.ipa.layout` and `ios.ipa.signature` on a written IPA (the release
/// and `icm verify`); the extracted app is left in `scratch`.
fn ipa_checks(ctx: &Ctx, ipa_path: &Path, scratch: &Path) -> Result<Vec<Check>> {
    let mut checks = Vec::new();
    let names = notices::zip_names(ipa_path).unwrap_or_default();
    let (app, problems) = ipa::layout(&names);
    checks.push(if problems.is_empty() {
        Check::pass(
            CheckId::IosIpaLayout,
            format!(
                "Payload/{} only, no __MACOSX or ._ entries ({} entries)",
                app.as_deref().unwrap_or("?"),
                names.len()
            ),
        )
    } else {
        Check::fail(
            CheckId::IosIpaLayout,
            format!(
                "{}: {}",
                crate::paths::display(ipa_path),
                problems.join("; ")
            ),
        )
        .evidence(Evidence::file(ipa_path))
    });
    let Some(app) = app else {
        return Ok(checks);
    };
    ipa::unzip(ctx, ipa_path, scratch)?;
    let extracted = scratch.join("Payload").join(&app);
    let outcome = codesign::verify(ctx, "ios.ipa.verify", &extracted)?;
    checks.push(if outcome.success() {
        Check::pass(
            CheckId::IosIpaSignature,
            "the app extracted from the .ipa verifies (codesign --verify --strict --deep)",
        )
    } else {
        Check::fail(
            CheckId::IosIpaSignature,
            format!(
                "the app extracted from the .ipa does not verify: {}",
                outcome.stderr_tail(4)
            ),
        )
        .evidence(Evidence::file(ipa_path))
    });
    Ok(checks)
}

// ---- verify --------------------------------------------------------------------------------

/// `icm verify ios`: the §12.2 gates on an `.ipa`.
pub fn verify_ipa(ctx: &mut Ctx, verify: &mut Verify) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Err(IcmError::new(
            CheckId::EnvUnsupportedHost,
            "icm verify ios needs a macOS host (codesign, plutil, assetutil)",
        ));
    }
    let artifact = verify
        .artifact
        .clone()
        .ok_or_else(|| IcmError::new(CheckId::UsageBadArgs, "icm verify ios needs an .ipa"))?;
    // The extracted app stays where its evidence paths point, replaced by
    // the next verify: the project's gen dir, else icm's cache.
    let scratch = match &verify.project {
        Some(project) => project.icm_dir.join("gen").join("ios").join("verify"),
        None => crate::paths::cache_dir().join("verify").join("ios"),
    };
    let mode = verify.gates.mode;
    for check in ipa_checks(ctx, &artifact, &scratch)? {
        verify.check(ctx, check);
    }
    let names = notices::zip_names(&artifact).unwrap_or_default();
    let Some(app_name) = ipa::layout(&names).0 else {
        return Ok(());
    };
    let app = scratch.join("Payload").join(&app_name);
    let info_path = app.join("Info.plist");
    let info = crate::platform::ios_sim::bundle::read_plist(ctx, &info_path)?;

    verify.check(ctx, bundle::lint(ctx, &app)?);
    for check in bundle::plist_checks(&app, &info) {
        verify.check(ctx, check);
    }
    verify.check(ctx, export_compliance(&info, mode, &info_path));
    verify.check(ctx, version_format(&info, &info_path));

    // The DT keys: present, and compared with this host's Xcode.
    let xcode = crate::tools::xcode(&ctx.env).ok();
    let host_dt = xcode.as_ref().and_then(|x| dt::read(ctx, x).ok());
    let missing: Vec<&str> = dt::KEYS
        .iter()
        .copied()
        .filter(|key| !info.contains_key(*key))
        .collect();
    verify.check(
        ctx,
        if !missing.is_empty() {
            Check::fail(
                CheckId::IosPlistDtKeys,
                format!("Info.plist lacks {}", missing.join(", ")),
            )
            .evidence(Evidence::file(&info_path))
        } else {
            match &host_dt {
                Some(host) => {
                    let (_, differ) = dt::compare(&info, host);
                    if differ.is_empty() {
                        Check::pass(CheckId::IosPlistDtKeys, "DT* keys equal this Xcode's")
                    } else {
                        Check::warn(
                            CheckId::IosPlistDtKeys,
                            format!(
                                "built with another Xcode than this host's: {}",
                                differ.join("; ")
                            ),
                        )
                    }
                }
                None => Check::pass(CheckId::IosPlistDtKeys, "DT* keys present"),
            }
        },
    );

    // The executable.
    let exe_name = info
        .get("CFBundleExecutable")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let exe = app.join(&exe_name);
    let bytes = std::fs::read(&exe).map_err(|error| {
        IcmError::new(
            CheckId::IosPlistRequiredKeys,
            format!("the executable {exe_name:?} is not in the bundle: {error}"),
        )
        .evidence(Evidence::file(&info_path))
    })?;
    let slices = macho::parse(&bytes)
        .map_err(|e| IcmError::new(CheckId::IosMachoPlatform, e).evidence(Evidence::file(&exe)))?;
    let versions = macho::build_versions(&exe)
        .map_err(|e| IcmError::new(CheckId::IosMachoPlatform, e).evidence(Evidence::file(&exe)))?;
    let minimum_os = info
        .get("MinimumOSVersion")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    for check in macho_checks(
        &exe,
        &versions,
        &slices,
        &minimum_os,
        dt::sdk_version_of(&info),
    )? {
        verify.check(ctx, check);
    }
    verify.check(ctx, bridge_check(&exe, &bytes));
    verify.check(ctx, bundle::usage_descriptions(&info, &bytes, &info_path));

    // Privacy: what the manifest declares against what the binary uses.
    let privacy_path = app.join("PrivacyInfo.xcprivacy");
    let declared: std::collections::BTreeMap<String, Vec<String>> =
        match crate::platform::ios_sim::bundle::read_plist(ctx, &privacy_path) {
            Ok(manifest) => manifest
                .get("NSPrivacyAccessedAPITypes")
                .and_then(Value::as_array)
                .map(|types| {
                    types
                        .iter()
                        .filter_map(|entry| {
                            let category = entry.get("NSPrivacyAccessedAPIType")?.as_str()?;
                            let reasons = entry
                                .get("NSPrivacyAccessedAPITypeReasons")?
                                .as_array()?
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect();
                            Some((privacy::short_name(category).to_string(), reasons))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            Err(_) => Default::default(),
        };
    let undefined: Vec<String> = slices.iter().flat_map(|s| s.undefined.clone()).collect();
    verify.check(
        ctx,
        privacy_check(&undefined, &bytes, &declared, Evidence::file(&privacy_path)),
    );
    if let Some(xcode) = &xcode {
        let icon = bundle::icon_check(ctx, xcode, &app, &info)?;
        verify.check(ctx, icon);
    }

    // The signature, the entitlements and the profile.
    let signature = codesign::signature(ctx, &app)?;
    verify.check(
        ctx,
        match signature.authorities.first() {
            Some(leaf)
                if Role::Distribution
                    .prefixes()
                    .iter()
                    .any(|p| leaf.starts_with(p)) =>
            {
                Check::pass(CheckId::IosSignNoIdentity, format!("signed by {leaf}"))
            }
            leaf => Check::fail(
                CheckId::IosSignNoIdentity,
                format!(
                    "signed {}, not by an Apple Distribution identity",
                    match leaf {
                        Some(leaf) => format!("by {leaf}"),
                        None if signature.ad_hoc => "ad hoc".to_string(),
                        None => "by nobody".to_string(),
                    }
                ),
            )
            .evidence(Evidence::file(&artifact)),
        },
    );
    let signed = codesign::entitlements(ctx, &app)?.unwrap_or_default();
    verify.check(
        ctx,
        if signed.get("get-task-allow") == Some(&Value::Bool(true)) {
            Check::fail(
                CheckId::IosEntitlementsGetTaskAllow,
                "the app is signed with get-task-allow = true (a development signature)",
            )
            .evidence(Evidence::file(&artifact))
        } else {
            Check::pass(CheckId::IosEntitlementsGetTaskAllow, "no get-task-allow")
        },
    );
    let embedded = app.join("embedded.mobileprovision");
    let bundle_id = info
        .get("CFBundleIdentifier")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    for check in embedded_profile_checks(&embedded, &bundle_id, &signed, signature.team.as_deref())
    {
        verify.check(ctx, check);
    }
    Ok(())
}

/// `ios.version.format` on a bundle: CFBundleShortVersionString is
/// X[.Y[.Z]] and CFBundleVersion is dot-separated numbers.
fn version_format(info: &Map<String, Value>, info_path: &Path) -> Check {
    let numeric = |text: &str, max: usize| {
        let parts: Vec<&str> = text.split('.').collect();
        !text.is_empty()
            && parts.len() <= max
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    };
    let short = info
        .get("CFBundleShortVersionString")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let build = info
        .get("CFBundleVersion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if numeric(short, 3) && numeric(build, 3) {
        Check::pass(
            CheckId::IosVersionFormat,
            format!("version {short} ({build})"),
        )
    } else {
        Check::fail(
            CheckId::IosVersionFormat,
            format!(
                "CFBundleShortVersionString {short:?} / CFBundleVersion {build:?} are not X[.Y[.Z]]"
            ),
        )
        .evidence(Evidence::file(info_path))
    }
}

/// The `ios.sign.profile_*` and `ios.entitlements.not_in_profile` gates
/// on an app's `embedded.mobileprovision`.
fn embedded_profile_checks(
    path: &Path,
    bundle_id: &str,
    signed: &Map<String, Value>,
    signature_team: Option<&str>,
) -> Vec<Check> {
    if !path.is_file() {
        return vec![
            Check::fail(
                CheckId::IosSignNoProfile,
                "the app has no embedded.mobileprovision (an App Store build embeds its profile)",
            )
            .evidence(Evidence::file(path.parent().unwrap_or(path))),
        ];
    }
    let profile = match Profile::read(path) {
        Ok(profile) => profile,
        Err(error) => {
            return vec![
                Check::fail(
                    CheckId::IosSignProfileMismatch,
                    format!("embedded.mobileprovision cannot be read: {error}"),
                )
                .evidence(Evidence::file(path)),
            ];
        }
    };
    let team = signature_team
        .map(str::to_string)
        .or_else(|| profile.teams.first().cloned())
        .unwrap_or_default();
    let want = Want {
        kind: Kind::AppStore,
        team: &team,
        bundle_id,
        certificates: &[],
        device: None,
        now: crate::ios::now_unix(),
    };
    let mut checks = vec![match profile::mismatch(&profile, &want) {
        None => Check::pass(
            CheckId::IosSignNoProfile,
            format!(
                "embedded App Store profile {} expires {}",
                profile.label(),
                profile.expires.as_deref().unwrap_or("?")
            ),
        ),
        Some((id, detail)) => Check::fail(id, detail).evidence(Evidence::file(path)),
    }];
    let missing = entitlements::not_in_profile(signed, &profile.entitlements);
    checks.push(if missing.is_empty() {
        Check::pass(
            CheckId::IosEntitlementsNotInProfile,
            "the embedded profile allows every signed entitlement",
        )
    } else {
        Check::fail(
            CheckId::IosEntitlementsNotInProfile,
            format!(
                "the embedded profile does not allow: {}",
                missing.join("; ")
            ),
        )
        .evidence(Evidence::file(path))
    });
    checks
}

// ---- diagnose ------------------------------------------------------------------------------

/// ITMS codes and the catalogue ids that prevent them.
const ITMS: &[(&str, CheckId)] = &[
    ("90022", CheckId::IosIconOpaque1024),
    ("90023", CheckId::IosIconOpaque1024),
    ("90704", CheckId::IosIconOpaque1024),
    ("90713", CheckId::IosIconOpaque1024),
    ("90717", CheckId::IosIconOpaque1024),
    ("91053", CheckId::IosPrivacyReasons),
    ("91061", CheckId::IosPrivacyPresent),
    ("90683", CheckId::IosPlistUsageDescriptions),
    ("90474", CheckId::IosPlistIpadOrientations),
    ("90062", CheckId::VersionBuildNotIncreased),
    ("90186", CheckId::VersionBuildNotIncreased),
    ("90189", CheckId::VersionBuildNotIncreased),
    ("90060", CheckId::IosVersionFormat),
    ("90725", CheckId::IosMachoSdkFloor),
    ("90534", CheckId::IosXcodeNotBeta),
    ("90208", CheckId::IosMachoMinos),
    ("90161", CheckId::IosSignProfileMismatch),
    ("90046", CheckId::IosEntitlementsNotInProfile),
    ("90045", CheckId::IosEntitlementsNotInProfile),
    ("90035", CheckId::IosSignVerify),
    ("90034", CheckId::IosSignVerify),
    ("90087", CheckId::IosMachoArch),
];

/// Every string in a JSON value, depth first.
fn strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| strings(item, out)),
        Value::Object(map) => map.values().for_each(|item| strings(item, out)),
        _ => {}
    }
}

/// The value of the first key named one of `keys`, anywhere in `value`.
fn find_key<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    match value {
        Value::Object(map) => keys
            .iter()
            .find_map(|key| map.get(*key))
            .or_else(|| map.values().find_map(|v| find_key(v, keys))),
        Value::Array(items) => items.iter().find_map(|v| find_key(v, keys)),
        _ => None,
    }
}

/// The ITMS code in a message (`ITMS-90713`, `(90713)`, `(ID: …)` aside).
fn itms_code(message: &str) -> Option<String> {
    let bytes = message.as_bytes();
    for (at, window) in bytes.windows(5).enumerate() {
        if window.iter().all(u8::is_ascii_digit) && window[0] == b'9' {
            let before = at.checked_sub(1).map(|i| bytes[i]);
            let after = bytes.get(at + 5).copied();
            let bounded = !before.is_some_and(|b| b.is_ascii_alphanumeric())
                && !after.is_some_and(|b| b.is_ascii_alphanumeric());
            let tagged = message[..at].ends_with("ITMS-") || before == Some(b'(');
            if bounded && tagged {
                return Some(String::from_utf8_lossy(window).into_owned());
            }
        }
    }
    None
}

/// What `icm diagnose altool` found.
#[derive(Debug, Default, PartialEq)]
pub struct Diagnosis {
    /// `success-message`, when altool reported success and no error.
    pub success: Option<String>,
    /// The delivery id of an upload.
    pub delivery_id: Option<String>,
    /// The build's processing state (`--build-status`).
    pub build_status: Option<String>,
    /// Each error: its catalogue id and message.
    pub errors: Vec<(CheckId, String)>,
}

/// Reads altool's `--output-format json` (or, failing that, its text).
pub fn diagnose_text(text: &str) -> Diagnosis {
    let mut diagnosis = Diagnosis::default();
    let json: Option<Value> = text
        .find('{')
        .and_then(|start| serde_json::from_str(&text[start..text.rfind('}')? + 1]).ok());
    let messages: Vec<String> = match &json {
        Some(json) => {
            diagnosis.delivery_id =
                find_key(json, &["delivery-uuid", "delivery-id", "deliveryUUID"])
                    .and_then(Value::as_str)
                    .map(str::to_string);
            diagnosis.build_status =
                find_key(json, &["build-status", "processing-state", "status"])
                    .and_then(Value::as_str)
                    .map(str::to_string);
            let mut messages = Vec::new();
            for key in ["product-errors", "errors"] {
                if let Some(errors) = json.get(key).and_then(Value::as_array) {
                    for error in errors {
                        let mut parts = Vec::new();
                        strings(error, &mut parts);
                        let reason = find_key(error, &["NSLocalizedFailureReason"])
                            .or_else(|| find_key(error, &["message"]))
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| parts.join(" "));
                        let all = parts.join(" ");
                        messages.push(format!("{reason}\u{1f}{all}"));
                    }
                }
            }
            if messages.is_empty() {
                diagnosis.success = json
                    .get("success-message")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            messages
        }
        None => text
            .lines()
            .filter(|line| {
                let lower = line.to_ascii_lowercase();
                lower.contains("error") || lower.contains("itms-")
            })
            .map(|line| format!("{0}\u{1f}{0}", line.trim()))
            .collect(),
    };
    for message in messages {
        let (reason, all) = message.split_once('\u{1f}').unwrap_or((&message, &message));
        let lower = all.to_ascii_lowercase();
        let id = if let Some(code) = itms_code(all)
            && let Some((_, id)) = ITMS.iter().find(|(itms, _)| *itms == code)
        {
            *id
        } else if lower.contains("authenticat")
            || lower.contains("not_authorized")
            || lower.contains("401")
            || lower.contains("private key")
        {
            CheckId::IosAscAuth
        } else if lower.contains("no suitable application records")
            || lower.contains("cannot determine the apple id")
            || lower.contains("could not find the app")
        {
            CheckId::IosAscAppRecord
        } else if lower.contains("bundle version must be higher")
            || lower.contains("redundant binary")
            || lower.contains("has already been used")
        {
            CheckId::VersionBuildNotIncreased
        } else {
            CheckId::IosAscRejected
        };
        diagnosis.errors.push((id, reason.trim().to_string()));
    }
    diagnosis
}

/// `icm diagnose altool`.
pub fn diagnose(ctx: &mut Ctx, input: &Input) -> Result<()> {
    let diagnosis = diagnose_text(&input.text);
    ctx.rep.set("delivery_id", json!(diagnosis.delivery_id));
    ctx.rep.set("build_status", json!(diagnosis.build_status));
    let mut first: Option<IcmError> = None;
    for (id, message) in &diagnosis.errors {
        let mut check = Check::fail(*id, message.clone());
        if let Some(evidence) = &input.evidence {
            check = check.evidence(evidence.clone());
        }
        if first.is_none() {
            first = Some(check.error.clone());
        }
        ctx.rep.check(check);
    }
    if let Some(error) = first {
        ctx.rep.summary(format!(
            "altool reported {} error(s); the first is {}",
            diagnosis.errors.len(),
            error.id
        ));
        return Err(error);
    }
    let detail = match (
        &diagnosis.success,
        &diagnosis.delivery_id,
        &diagnosis.build_status,
    ) {
        (_, Some(id), _) => format!("uploaded: delivery id {id}"),
        (_, None, Some(status)) => format!("build status: {status}"),
        (Some(message), None, None) => message.clone(),
        (None, None, None) => {
            return Err(IcmError::new(
                CheckId::UsageBadArgs,
                "this is not altool output: no product-errors, success-message, delivery id or build status",
            ));
        }
    };
    ctx.rep
        .check(Check::pass(CheckId::IosAscRejected, detail.clone()));
    ctx.rep.summary(format!("altool: {detail}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare() {
        assert!(same_version("16.0", "16"));
        assert!(same_version("16.0.0", "16.0"));
        assert!(!same_version("16.1", "16.0"));
        assert!(at_least("16.0", "13.0"));
        assert!(at_least("13", "13.0"));
        assert!(!at_least("12.4", "13.0"));
    }

    #[test]
    fn macho_gates_judge_the_executable() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("app");
        let bytes = macho::synthetic(
            PLATFORM_IOS,
            (16, 0, 0),
            (27, 0, 0),
            [1; 16],
            &["_stat"],
            b"",
        );
        std::fs::write(&exe, &bytes).unwrap();
        let versions = macho::build_versions(&exe).unwrap();
        let slices = macho::parse(&bytes).unwrap();
        let checks = macho_checks(&exe, &versions, &slices, "16.0", Some("27.0")).unwrap();
        assert!(checks.iter().all(|c| !c.failed()), "{checks:?}");
        let wrong = macho_checks(&exe, &versions, &slices, "17.0", Some("26.0")).unwrap();
        let failed: Vec<&str> = wrong.iter().filter(|c| c.failed()).map(Check::id).collect();
        assert_eq!(failed, ["ios.macho.minos", "ios.macho.sdk_matches_dt"]);

        let old = macho::synthetic(PLATFORM_IOS, (12, 0, 0), (18, 0, 0), [1; 16], &[], b"");
        std::fs::write(&exe, &old).unwrap();
        let versions = macho::build_versions(&exe).unwrap();
        let checks =
            macho_checks(&exe, &versions, &macho::parse(&old).unwrap(), "12.0", None).unwrap();
        let failed: Vec<&str> = checks
            .iter()
            .filter(|c| c.failed())
            .map(Check::id)
            .collect();
        assert_eq!(failed, ["ios.macho.minos", "ios.macho.sdk_floor"]);

        let sim = crate::platform::ios_sim::macho::synthetic(7, (16, 0, 0), (27, 0, 0));
        std::fs::write(&exe, &sim).unwrap();
        let versions = macho::build_versions(&exe).unwrap();
        let error =
            macho_checks(&exe, &versions, &macho::parse(&sim).unwrap(), "16.0", None).unwrap_err();
        assert_eq!(error.id, "ios.macho.platform");
        assert!(error.detail.contains("IOSSIMULATOR"));
    }

    #[test]
    fn the_bridge_and_privacy_gates() {
        let exe = Path::new("app");
        assert!(!bridge_check(exe, b"plain").failed());
        assert!(bridge_check(exe, b"..ICM_AGENT_BRIDGE_V1..").failed());
        let mut declared = std::collections::BTreeMap::new();
        let _ = declared.insert("FileTimestamp".to_string(), vec!["C617.1".to_string()]);
        let undefined = vec!["_stat".to_string(), "_mach_absolute_time".to_string()];
        let check = privacy_check(&undefined, b"", &declared, Evidence::file("icm.toml"));
        assert!(check.failed());
        assert!(
            check.error.fix.summary.contains(
                "api_reasons = { FileTimestamp = [\"C617.1\"], SystemBootTime = [\"35F9.1\"] }"
            ),
            "{}",
            check.error.fix.summary
        );
        let _ = declared.insert("SystemBootTime".to_string(), vec!["35F9.1".to_string()]);
        assert!(!privacy_check(&undefined, b"", &declared, Evidence::file("icm.toml")).failed());
    }

    #[test]
    fn export_compliance_warns_only_for_unsigned_releases() {
        let path = Path::new("Info.plist");
        let empty = Map::new();
        assert_eq!(
            export_compliance(&empty, SignMode::None, path).status,
            Status::Warn
        );
        assert_eq!(
            export_compliance(&empty, SignMode::Auto, path).status,
            Status::Fail
        );
        let mut answered = Map::new();
        let _ = answered.insert("ITSAppUsesNonExemptEncryption".into(), json!(false));
        assert_eq!(
            export_compliance(&answered, SignMode::Auto, path).status,
            Status::Pass
        );
    }

    #[test]
    fn version_formats() {
        let path = Path::new("Info.plist");
        let info = |short: &str, build: &str| -> Map<String, Value> {
            serde_json::from_value(
                json!({"CFBundleShortVersionString": short, "CFBundleVersion": build}),
            )
            .unwrap()
        };
        assert!(!version_format(&info("1.2.3", "12"), path).failed());
        assert!(!version_format(&info("1", "1.0.1"), path).failed());
        assert!(version_format(&info("1.2.3-beta", "12"), path).failed());
        assert!(version_format(&info("1.2.3.4", "12"), path).failed());
        assert!(version_format(&info("1.2", ""), path).failed());
    }

    #[test]
    fn embedded_profiles_are_judged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("embedded.mobileprovision");
        assert_eq!(
            embedded_profile_checks(&path, "com.acme.notes", &Map::new(), None)[0].id(),
            "ios.sign.no_profile"
        );
        let fixture = profile::Fixture {
            name: "Notes Store".into(),
            uuid: "11111111-2222-3333-4444-555555555555".into(),
            team: "ABCDE12345".into(),
            app_id: "com.acme.notes".into(),
            certificates: vec![b"cert".to_vec()],
            devices: vec![],
            get_task_allow: false,
            expires: "2099-01-01T00:00:00Z".into(),
        };
        std::fs::write(&path, fixture.bytes()).unwrap();
        let signed: Map<String, Value> = serde_json::from_value(json!({
            "application-identifier": "ABCDE12345.com.acme.notes",
            "get-task-allow": false,
            "beta-reports-active": true,
        }))
        .unwrap();
        let checks = embedded_profile_checks(&path, "com.acme.notes", &signed, Some("ABCDE12345"));
        assert!(checks.iter().all(|c| !c.failed()), "{checks:?}");
        let checks = embedded_profile_checks(&path, "com.acme.other", &signed, Some("ABCDE12345"));
        assert_eq!(checks[0].id(), "ios.sign.profile_mismatch");

        let dev = profile::Fixture {
            devices: vec!["UDID".into()],
            get_task_allow: true,
            ..fixture
        };
        std::fs::write(&path, dev.bytes()).unwrap();
        let checks = embedded_profile_checks(&path, "com.acme.notes", &signed, None);
        assert_eq!(checks[0].id(), "ios.sign.profile_mismatch");
        assert!(checks[0].error.detail.contains("development"));
    }

    #[test]
    fn altool_output_maps_to_ids() {
        let failed = r#"{
  "tool-version" : "27.0.5 (170005)",
  "product-errors" : [
    {"code" : -19208, "message" : "Validation failed",
     "userInfo" : {"NSLocalizedDescription" : "Validation failed",
       "NSLocalizedFailureReason" : "Missing required icon file. The bundle does not contain an app icon for iPhone of exactly '120x120' pixels (ID: 0a1b) (90022)"}},
    {"code" : -19208, "message" : "Validation failed",
     "userInfo" : {"NSLocalizedFailureReason" : "ITMS-91053: Missing API declaration - Your app's code references one or more APIs that require reasons, including: NSPrivacyAccessedAPICategoryDiskSpace."}},
    {"code" : -19209, "message" : "Unable to authenticate with App Store Connect (401)", "userInfo" : {}},
    {"code" : -1, "message" : "Something new", "userInfo" : {}}
  ]
}"#;
        let diagnosis = diagnose_text(failed);
        let ids: Vec<&str> = diagnosis.errors.iter().map(|(id, _)| id.id()).collect();
        assert_eq!(
            ids,
            [
                "ios.icon.opaque_1024",
                "ios.privacy.reasons",
                "ios.asc.auth",
                "ios.asc.rejected"
            ]
        );
        assert!(
            diagnosis.errors[0]
                .1
                .starts_with("Missing required icon file")
        );
        assert_eq!(diagnosis.success, None);

        let uploaded = r#"{"success-message":"No errors uploading 'Notes.ipa'","details":{"delivery-uuid":"9f1c2b3a-0000-4000-8000-000000000001","transferred":"9.4 MB"}}"#;
        let diagnosis = diagnose_text(uploaded);
        assert!(diagnosis.errors.is_empty());
        assert_eq!(
            diagnosis.delivery_id.as_deref(),
            Some("9f1c2b3a-0000-4000-8000-000000000001")
        );

        let text = diagnose_text(
            "ERROR ITMS-90189: \"Redundant Binary Upload. You've already uploaded a build with build number '12'\"",
        );
        assert_eq!(text.errors[0].0, CheckId::VersionBuildNotIncreased);
        assert_eq!(
            itms_code("see (ID: 12345) and ITMS-90713"),
            Some("90713".into())
        );
        assert_eq!(itms_code("build 912345678"), None);
    }
}
