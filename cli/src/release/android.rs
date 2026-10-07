//! The Android release pipeline (design §11.2, §12.3, §9.4): an `.aab`
//! for Google Play, `--apk` for sideloading, and `icm diagnose play`.
//!
//! 1. Preconditions: `[android] target_sdk` at or above the policy's
//!    `play.target_sdk`, arm64-v8a among `[android] abis`, a JDK 17+, the
//!    NDK, build-tools, the platform's android.jar, the Rust targets and
//!    bundletool (pinned; `--yes` downloads it). The core has already
//!    checked `[android.signing] upload` (deferred: the unsigned bundle is
//!    still built, then exit 9).
//! 2. Every ABI: `cargo rustc --lib --crate-type cdylib --release` in the
//!    release target directory; the unstripped library goes into
//!    `native-debug-symbols.zip` (`<abi>/lib<lib>.so`), the stripped one
//!    (`llvm-strip --strip-unneeded`) into the bundle.
//! 3. `aapt2 link --proto-format` (never `--debug-mode`), `base.zip`
//!    ([`bundle::base_entries`], with `THIRD_PARTY_NOTICES.txt` in
//!    `assets/`), `BundleConfig.json` (`PAGE_ALIGNMENT_16K`), `bundletool
//!    build-bundle` and `validate`.
//! 4. Signing: `keytool -list -v` reads the upload key (its algorithm
//!    picks `-sigalg`, its certificate is compared later), `jarsigner
//!    -storepass:env -keypass:env` signs, `jarsigner -verify -verbose
//!    -certs` (no `-strict`) and `keytool -printcert -jarfile` verify by
//!    parsing. Without the key (or with `--sign none`) the dist holds
//!    `<name>-unsigned.aab` and UPLOAD.md the jarsigner line.
//! 5. The §12.3 gates on the linked bundle ([`gate_bundle`], shared with
//!    `icm verify android`): `bundletool dump manifest|config` and the ELF
//!    gates on every library extracted from it.
//! 6. `play-icon-512.png`, a copy of the manifest, `--apk` (a universal
//!    APK from `bundletool build-apks --mode=universal`, signed with
//!    `apksigner` and the upload key, or icm's debug key when the bundle is
//!    unsigned), the smoke install on a running emulator unless
//!    `--no-smoke`, and the owner's plan ([`super::owner_plans::android`]).

use super::diagnose::Input;
use super::verify::Verify;
use super::{Pipeline, Release, owner_plans};
use crate::android::bundle::{self, Expect, UploadKey};
use crate::android::{self, Toolset, adb, apk, zip};
use crate::cargo::Select;
use crate::catalogue::CheckId;
use crate::cli::SignMode;
use crate::config::{Abi, KeystoreRef};
use crate::context::Ctx;
use crate::error::{Check, Evidence, IcmError, Result, Status};
use crate::plan::{Plan, Step};
use crate::process::Cmd;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The Android pipeline.
pub struct Android;

/// Where the bundle's intermediates go: `gen/android/release/bundle/`.
struct Work {
    root: PathBuf,
}

impl Work {
    fn new(rel: &Release) -> Work {
        Work {
            root: rel.gen_dir.join("bundle"),
        }
    }
    fn libs(&self) -> PathBuf {
        self.root.join("lib")
    }
    fn proto(&self) -> PathBuf {
        self.root.join("proto")
    }
    fn base_zip(&self) -> PathBuf {
        self.root.join("base.zip")
    }
    fn config(&self) -> PathBuf {
        self.root.join("BundleConfig.json")
    }
    fn unsigned(&self) -> PathBuf {
        self.root.join("app-unsigned.aab")
    }
    fn gates(&self) -> PathBuf {
        self.root.join("gates")
    }
    fn universal(&self) -> PathBuf {
        self.root.join("universal")
    }

    /// Removes the outputs of an earlier release (the resource tree and its
    /// stamp stay, so aapt2 compiles only what changed).
    fn reset(&self) -> Result<()> {
        for dir in [self.libs(), self.proto(), self.gates(), self.universal()] {
            if dir.exists() {
                std::fs::remove_dir_all(&dir).map_err(|e| io("clean", &dir, &e))?;
            }
        }
        for file in [
            self.base_zip(),
            self.unsigned(),
            self.root.join("device.apks"),
        ] {
            let _ = std::fs::remove_file(file);
        }
        std::fs::create_dir_all(&self.root).map_err(|e| io("create", &self.root, &e))
    }
}

fn io(what: &str, path: &Path, error: &std::io::Error) -> IcmError {
    IcmError::new(
        CheckId::InternalBug,
        format!("cannot {what} {}: {error}", crate::paths::display(path)),
    )
}

/// The Android SDK, JDK and NDK.
fn toolset(ctx: &mut Ctx) -> Result<Toolset> {
    let host = ctx.host()?.clone();
    Toolset::discover(&host, &ctx.env)
}

/// Google Play's targetSdk floor today (`play.target_sdk`).
fn target_sdk_floor() -> Option<u32> {
    crate::policy::get()
        .int("play.target_sdk")
        .and_then(|value| u32::try_from(value).ok())
}

/// `[android] abis` without repeats, in their order.
fn abis(rel: &Release) -> Vec<Abi> {
    let mut out: Vec<Abi> = Vec::new();
    for abi in &rel.config().android.abis {
        if !out.contains(abi) {
            out.push(*abi);
        }
    }
    out
}

/// `<package>-<version>-<build>`: the stem of the dist files.
fn stem(rel: &Release) -> String {
    format!("{}-{}-{}", rel.package.name, rel.version, rel.build)
}

/// The upload key, when this release can sign with it: `--sign auto`, the
/// reference configured, the keystore present and its variables set (the
/// core reported what is missing).
struct Key {
    keystore: PathBuf,
    alias: String,
    store_pass_env: String,
    key_pass_env: String,
}

fn upload_key(ctx: &Ctx, rel: &Release) -> Option<Key> {
    if rel.sign() == SignMode::None {
        return None;
    }
    let upload: &KeystoreRef = rel.config().android.signing.as_ref()?.upload.as_ref()?;
    let keystore = super::resolve_path(&rel.project, &upload.keystore);
    let key_pass_env = upload
        .key_pass_env
        .clone()
        .unwrap_or_else(|| upload.store_pass_env.clone());
    let ready = keystore.is_file()
        && ctx.env.var(&upload.store_pass_env).is_some()
        && ctx.env.var(&key_pass_env).is_some();
    ready.then(|| Key {
        keystore,
        alias: upload.alias.clone(),
        store_pass_env: upload.store_pass_env.clone(),
        key_pass_env,
    })
}

/// `keytool -list -v` of the upload key.
fn keytool_list(tools: &Toolset, key: &Key) -> Result<Cmd> {
    Ok(tools
        .keytool()?
        .args(["-J-Duser.language=en", "-list", "-v", "-keystore"])
        .arg(&key.keystore)
        .args(["-alias", &key.alias, "-storepass:env", &key.store_pass_env])
        .timeout(Duration::from_secs(120)))
}

/// `jarsigner` signing `unsigned` into `signed`.
fn jarsigner_sign(
    tools: &Toolset,
    key: &Key,
    sigalg: &str,
    unsigned: &Path,
    signed: &Path,
) -> Result<Cmd> {
    Ok(tools
        .jdk_tool("jarsigner")?
        .args(["-J-Duser.language=en", "-keystore"])
        .arg(&key.keystore)
        .args([
            "-storepass:env",
            &key.store_pass_env,
            "-keypass:env",
            &key.key_pass_env,
            "-sigalg",
            sigalg,
            "-digestalg",
            "SHA-256",
            "-signedjar",
        ])
        .arg(signed)
        .arg(unsigned)
        .arg(&key.alias)
        .timeout(Duration::from_secs(600)))
}

/// The owner-dependent error for a key keytool or jarsigner cannot use.
fn unreadable(rel: &Release, key: &Key, why: &str, log: Option<&Path>) -> IcmError {
    let mut error = IcmError::new(
        CheckId::AndroidKeystoreUnreadable,
        format!(
            "the upload key `{}` in {} cannot be used: {why}",
            key.alias,
            crate::paths::display(&key.keystore)
        ),
    )
    .evidence(
        rel.project
            .config
            .evidence("android.signing.upload.keystore"),
    );
    if let Some(log) = log {
        error = error.evidence(Evidence::file(log));
    }
    error
}

// ---- plan ---------------------------------------------------------------------------

impl Pipeline for Android {
    fn plan(&self, ctx: &Ctx, rel: &Release) -> Result<Plan> {
        let config = rel.config();
        let host = crate::host::load().ok().map(|loaded| loaded.config);
        let tools = host
            .as_ref()
            .and_then(|host| Toolset::discover(host, &ctx.env).ok());
        let work = Work::new(rel);
        let lib = rel.project.lib_name()?;
        let mut plan = Plan::new();
        plan.push(Step::internal(
            "android.preconditions",
            &format!(
                "[android] target_sdk {} at or above Google Play's floor ({}); arm64-v8a among [android] abis ({}); a JDK 17+, an NDK r28+, build-tools 35+, platforms/android-{}/android.jar, the Rust targets; bundletool 1.18.3 (pinned: `icm doctor android --fix --yes` or --yes downloads it)",
                config.android.target_sdk,
                target_sdk_floor().map_or("unknown".to_string(), |floor| floor.to_string()),
                abis(rel).iter().map(|abi| abi.as_str()).collect::<Vec<_>>().join(", "),
                config.android.target_sdk
            ),
        ));
        for abi in abis(rel) {
            let mut invocation = rel.invocation("rustc", Select::Lib, Some(abi.triple()));
            invocation.flags = vec!["--crate-type".into(), "cdylib".into()];
            invocation.offline |= ctx.global.offline;
            let mut env: Vec<(String, String)> = tools
                .as_ref()
                .map(|t| t.child_env().to_vec())
                .unwrap_or_default();
            if let Some(ndk) = tools.as_ref().and_then(|t| t.ndk.as_ref().ok()) {
                env.extend(crate::tools::ndk_env(
                    ndk,
                    abi.triple(),
                    config.android.min_sdk,
                ));
            }
            let cmd = invocation
                .cmd()
                .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
            plan.push(
                Step::exec(&format!("cargo.rustc.{}", abi.as_str()), cmd)
                    .on_fail(CheckId::BuildCompileError),
            );
            plan.push(Step::internal(
                &format!("llvm-strip.{}", abi.as_str()),
                &format!(
                    "keep the unstripped lib{lib}.so for native-debug-symbols.zip ({}/lib{lib}.so); llvm-strip --strip-unneeded into {}",
                    abi.as_str(),
                    crate::paths::display(&work.libs().join(abi.as_str()))
                ),
            ));
        }
        plan.push(Step::internal(
            "release.notices",
            "THIRD_PARTY_NOTICES.txt from cargo metadata, into the bundle's assets/",
        ));
        plan.push(Step::internal(
            "aapt2.link.proto",
            &format!(
                "AndroidManifest.xml and res/ (design §9.4), aapt2 compile, aapt2 link --proto-format -I android-{}.jar --version-code {} --version-name {} (no --debug-mode) into {}",
                config.android.target_sdk,
                rel.build,
                rel.version,
                crate::paths::display(&work.proto())
            ),
        ));
        plan.push(Step::internal(
            "android.base_zip",
            &format!(
                "{}: manifest/AndroidManifest.xml, resources.pb, res/, lib/<abi>/lib{lib}.so, assets/ (no dex/); {} with PAGE_ALIGNMENT_16K",
                crate::paths::display(&work.base_zip()),
                crate::paths::display(&work.config())
            ),
        ));
        plan.push(
            Step::internal(
                "bundletool.build_bundle",
                &format!(
                    "java -jar bundletool build-bundle --modules={} --config={} --output={}, then bundletool validate",
                    crate::paths::display(&work.base_zip()),
                    crate::paths::display(&work.config()),
                    crate::paths::display(&work.unsigned())
                ),
            )
            .gate(CheckId::AndroidAabValidate),
        );
        let signing = config
            .android
            .signing
            .as_ref()
            .and_then(|signing| signing.upload.as_ref());
        match (rel.sign(), signing) {
            (SignMode::Auto, Some(upload)) => {
                let key_pass = upload
                    .key_pass_env
                    .as_deref()
                    .unwrap_or(&upload.store_pass_env);
                plan.push(
                    Step::internal(
                        "android.sign",
                        &format!(
                            "keytool -list -v -keystore {ks} -alias {alias} -storepass:env {store} (the key's algorithm and certificate); jarsigner -keystore {ks} -storepass:env {store} -keypass:env {key_pass} -sigalg SHA256withRSA|SHA256withECDSA -digestalg SHA-256 -signedjar {} …; jarsigner -verify -verbose -certs and keytool -printcert -jarfile, parsed",
                            crate::paths::display(&rel.dist.join(format!("{}.aab", stem(rel)))),
                            ks = upload.keystore,
                            alias = upload.alias,
                            store = upload.store_pass_env,
                        ),
                    )
                    .gate(CheckId::AndroidAabSigned),
                );
            }
            _ => plan.push(Step::internal(
                "android.unsigned",
                &format!(
                    "no signing ({}): {} goes to the dist, and UPLOAD.md shows the jarsigner line",
                    if rel.sign() == SignMode::None {
                        "--sign none"
                    } else {
                        "[android.signing] upload is unset"
                    },
                    crate::paths::display(&rel.dist.join(format!("{}-unsigned.aab", stem(rel))))
                ),
            )),
        }
        plan.push(
            Step::internal(
                "android.gates",
                "bundletool dump manifest and dump config, and the ELF gates on every library extracted from the bundle (design §12.3)",
            )
            .gate(CheckId::AndroidManifestTargetSdk)
            .gate(CheckId::AndroidManifestConfigChanges)
            .gate(CheckId::AndroidManifestHasCode)
            .gate(CheckId::AndroidManifestDebuggable)
            .gate(CheckId::AndroidManifestLibName)
            .gate(CheckId::AndroidManifestVersion)
            .gate(CheckId::AndroidBundleAlignment)
            .gate(CheckId::AndroidSoAlign16k)
            .gate(CheckId::AndroidSoExport)
            .gate(CheckId::AndroidSoAbis)
            .gate(CheckId::StoreNoAgentBridge),
        );
        plan.push(Step::internal(
            "android.listing",
            "play-icon-512.png from [app] icon, and a copy of AndroidManifest.xml",
        ));
        if rel.args.apk {
            plan.push(Step::internal(
                "android.apk",
                &format!(
                    "bundletool build-apks --mode=universal --output-format=directory, then apksigner sign with the upload key (icm's debug key when unsigned) into {}",
                    crate::paths::display(&rel.dist.join(format!("{}-universal.apk", stem(rel))))
                ),
            ));
        }
        if !rel.args.no_smoke {
            plan.push(Step::internal(
                "android.smoke",
                "when icm's emulator (or $ANDROID_SERIAL, host.toml android.device) is online: bundletool build-apks --connected-device with icm's debug key, install-apks, launch, ICM_EVENT ready, screenshot (never boots a device)",
            ));
        }
        Ok(plan)
    }

    // ---- preconditions ---------------------------------------------------------------

    fn preconditions(&self, ctx: &mut Ctx, rel: &mut Release) -> Result<()> {
        let config = rel.config().clone();
        if let Some(floor) = target_sdk_floor()
            && config.android.target_sdk < floor
        {
            return Err(IcmError::new(
                CheckId::AndroidManifestTargetSdk,
                format!(
                    "{}: [android] target_sdk is {}; Google Play takes new apps and updates only from {floor}",
                    rel.project.config.source.location_for("android.target_sdk"),
                    config.android.target_sdk
                ),
            )
            .evidence(rel.project.config.evidence("android.target_sdk"))
            .fix(format!("Set [android] target_sdk = {floor} in icm.toml."), &[]));
        }
        let abis = abis(rel);
        if !abis.contains(&Abi::Arm64V8a) {
            return Err(IcmError::new(
                CheckId::AndroidSoAbis,
                format!(
                    "{}: [android] abis is [{}]; Google Play requires arm64-v8a",
                    rel.project.config.source.location_for("android.abis"),
                    abis.iter()
                        .map(|abi| abi.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
            .evidence(rel.project.config.evidence("android.abis"))
            .fix(
                "Set [android] abis = [\"arm64-v8a\", \"x86_64\"] in icm.toml.",
                &[],
            ));
        }

        let tools = toolset(ctx)?;
        let jdk = tools.jdk.clone()?;
        let ndk = tools.ndk.clone()?;
        let (build_tools, _) = tools.build_tools()?;
        let _ = tools.platform_jar(config.android.target_sdk)?;
        let toolchain = crate::toolchain::active(rel.project.dir())?;
        let triples: Vec<String> = abis.iter().map(|abi| abi.triple().to_string()).collect();
        if let Some(failed) = crate::toolchain::check_targets(&toolchain, &triples)
            .into_iter()
            .find(Check::failed)
        {
            return Err(failed.into_error());
        }
        let (_, version) = tools.bundletool(ctx)?;
        rel.tool("jdk", jdk.version);
        rel.tool("ndk", ndk.version);
        rel.tool("build_tools", build_tools);
        rel.tool(
            "bundletool",
            version.unwrap_or_else(|| "ICM_TOOL_BUNDLETOOL".to_string()),
        );
        Ok(())
    }

    // ---- build -----------------------------------------------------------------------

    fn build(&self, ctx: &mut Ctx, rel: &mut Release) -> Result<()> {
        let tools = toolset(ctx)?;
        let (bundletool, _) = tools.bundletool(ctx)?;
        let host = ctx.host()?.clone();
        let aab = {
            let ctx: &Ctx = ctx;
            bundle_and_gate(ctx, rel, &tools, &bundletool)?
        };

        // The smoke install, on a running device only (design §11.2 step 9).
        if rel.args.no_smoke {
            rel.check(
                ctx,
                Check::skip(
                    CheckId::AndroidSmoke,
                    "--no-smoke: the bundle was not installed on a device",
                ),
            );
        } else {
            smoke(ctx, rel, &tools, &host, &bundletool, &aab)?;
        }

        owner_plan(rel);
        Ok(())
    }

    // ---- verify ----------------------------------------------------------------------

    fn verify(&self, ctx: &mut Ctx, verify: &mut Verify) -> Result<()> {
        let Some(aab) = verify.artifact.clone() else {
            return Err(IcmError::new(
                CheckId::UsageBadArgs,
                "`icm verify android` checks an .aab: pass --artifact <path> or release first",
            ));
        };
        if aab.extension().is_none_or(|ext| ext != "aab") {
            return Err(IcmError::new(
                CheckId::UsageBadArgs,
                format!(
                    "{} is not an .aab; `icm verify android` checks the bundle Google Play takes",
                    crate::paths::display(&aab)
                ),
            ));
        }
        let tools = toolset(ctx)?;
        let (bundletool, _) = tools.bundletool(ctx)?;
        let ctx: &Ctx = ctx;
        // The dumps are the gates' evidence: keep them in a run directory
        // (the project's, which verify attached, else icm's cache), as other
        // commands that work do.
        if ctx.rep.run_dir().is_none() {
            let root = verify
                .project
                .as_ref()
                .map_or_else(crate::paths::cache_dir, |project| project.icm_dir.clone());
            let _ = ctx.rep.attach(&root);
        }
        let work = verify_dir(ctx, &aab);
        std::fs::create_dir_all(&work).map_err(|e| io("create", &work, &e))?;

        // What the release that made it promised, when artifacts.json and
        // the project say.
        let mut expect = Expect {
            target_sdk_floor: target_sdk_floor(),
            ..Expect::default()
        };
        let mut expected_cert = None;
        if let Some(project) = &verify.project {
            let config = &project.config.config;
            expect.lib = project.lib_name().ok();
            expect.abis = config.android.abis.clone();
            expect.native = config.android.activity == "native";
            if let Some(manifest) = &verify.manifest {
                expect.version = Some(manifest.app.version.clone());
                expect.build = Some(manifest.app.build);
            }
            if let Some(recorded) = verify
                .manifest
                .as_ref()
                .and_then(|m| m.signing.get("certificate_sha256"))
                .and_then(Value::as_str)
            {
                expected_cert = Some(recorded.to_string());
            }
        }

        // A `--sign none` release's bundle is unsigned on purpose, and a
        // `--sign auto` release that recorded `signed: false` wrote it
        // unsigned because the owner's upload key was missing (its exit 9):
        // either way the signature is the owner's to add, not the agent's.
        let unsigned_expected = if verify.gates.mode == SignMode::None {
            Some("as its release (--sign none) made it")
        } else if verify.manifest.as_ref().is_some_and(|m| !m.signed) {
            Some(
                "as its release recorded (signed: false; the owner's upload key, keystore or password variable was missing)",
            )
        } else {
            None
        };
        let mut report = |check: Check| verify.check(ctx, check);
        validate(ctx, &bundletool, &aab, &mut report)?;
        signature_checks(
            ctx,
            &tools,
            &aab,
            expected_cert.as_deref(),
            unsigned_expected,
            &mut report,
        )?;
        gate_bundle(ctx, &tools, &bundletool, &aab, &expect, &work, &mut report)?;
        Ok(())
    }
}

/// Where `icm verify android` keeps its dumps: the run directory, else
/// (when none could be made) a directory under the system's temp dir.
fn verify_dir(ctx: &Ctx, aab: &Path) -> PathBuf {
    match ctx.rep.run_dir() {
        Some(dir) => dir.join("verify"),
        None => std::env::temp_dir().join(format!(
            "icm-verify-{}-{}",
            std::process::id(),
            aab.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        )),
    }
}

// ---- the bundle -----------------------------------------------------------------------

/// Steps 2 to 6 of the module docs: the libraries, the symbols, the
/// notices, the proto link, `base.zip`, the bundle, its signature, its
/// gates, the listing files and `--apk`. Returns the bundle in the dist.
fn bundle_and_gate(
    ctx: &Ctx,
    rel: &mut Release,
    tools: &Toolset,
    bundletool: &Cmd,
) -> Result<PathBuf> {
    let project = rel.project.clone();
    let config = rel.config().clone();
    let lib = project.lib_name()?;
    let abis = abis(rel);
    let work = Work::new(rel);
    work.reset()?;
    let ndk = tools.ndk.clone()?;
    let jar = tools.platform_jar(config.android.target_sdk)?;

    // 1. The libraries, one release build per ABI.
    let mut stripped: Vec<(Abi, PathBuf)> = Vec::new();
    let mut symbols: Vec<zip::Entry> = Vec::new();
    for abi in &abis {
        let triple = abi.triple();
        let mut invocation = rel.invocation("rustc", Select::Lib, Some(triple));
        invocation.flags = vec!["--crate-type".into(), "cdylib".into()];
        let mut env = tools.child_env().to_vec();
        env.extend(crate::tools::ndk_env(&ndk, triple, config.android.min_sdk));
        let output = rel.cargo(
            ctx,
            &format!("cargo.rustc.{}", abi.as_str()),
            &invocation,
            &env,
            None,
        )?;
        let crate_name = lib.replace('-', "_");
        let library = output
            .artifacts
            .iter()
            .filter(|artifact| artifact.target_name.replace('-', "_") == crate_name)
            .flat_map(|artifact| artifact.filenames.iter())
            .find(|path| path.extension().is_some_and(|ext| ext == "so"))
            .cloned()
            .ok_or_else(|| {
                IcmError::new(
                    CheckId::ToolFailed,
                    format!("cargo built no lib{crate_name}.so for {triple}"),
                )
                .evidence(Evidence::file(&rel.package.manifest_path))
            })?;
        let dir = work.libs().join(abi.as_str());
        std::fs::create_dir_all(&dir).map_err(|e| io("create", &dir, &e))?;
        let out = dir.join(format!("lib{lib}.so"));
        let strip = tools
            .llvm_strip()?
            .arg("--strip-unneeded")
            .arg("-o")
            .arg(&out)
            .arg(&library)
            .timeout(Duration::from_secs(300));
        apk::run_tool(
            ctx,
            &format!("llvm-strip.{}", abi.as_str()),
            &strip,
            CheckId::ToolFailed,
        )?;
        stripped.push((*abi, out));
        symbols.push(zip::Entry {
            name: format!("{}/lib{lib}.so", abi.as_str()),
            source: zip::Source::File(library),
        });
    }

    // 2. native-debug-symbols.zip: the unstripped libraries, for the Play
    // Console's App bundle explorer.
    let symbols_zip = rel.dist.join("native-debug-symbols.zip");
    zip::write(&symbols_zip, &symbols).map_err(|e| io("write", &symbols_zip, &e))?;
    rel.add_file("symbols", "symbols", &symbols_zip)?;

    // 3. The notices, the resources and the proto link.
    let notices = rel.notices(ctx, abis.first().map(|abi| abi.triple()))?;
    let resources = apk::resources(ctx, &project, tools, &work.root, &rel.version, &lib)?;
    for check in resources.checks.clone() {
        rel.check(ctx, check);
    }
    apk::link(
        ctx,
        tools,
        &jar,
        &resources,
        &work.proto(),
        rel.build,
        &rel.version,
        apk::Link::Proto,
    )?;

    // 4. base.zip, BundleConfig.json, the bundle.
    let mut assets = vec![(notices, crate::release::notices::FILE.to_string())];
    assets.extend(apk::collect_resources(project.dir(), &config.app.resources));
    let entries = bundle::base_entries(&work.proto(), &stripped, &lib, &assets)
        .map_err(|e| io("pack", &work.base_zip(), &e))?;
    zip::write(&work.base_zip(), &entries).map_err(|e| io("write", &work.base_zip(), &e))?;
    std::fs::write(work.config(), bundle::BUNDLE_CONFIG)
        .map_err(|e| io("write", &work.config(), &e))?;
    let build_bundle = bundletool
        .clone()
        .arg("build-bundle")
        .arg(format!("--modules={}", work.base_zip().display()))
        .arg(format!("--config={}", work.config().display()))
        .arg(format!("--output={}", work.unsigned().display()))
        .timeout(Duration::from_secs(600));
    apk::run_tool(
        ctx,
        "bundletool.build_bundle",
        &build_bundle,
        CheckId::AndroidBundletoolFailed,
    )?;
    validate(ctx, bundletool, &work.unsigned(), &mut |check| {
        rel.check(ctx, check)
    })?;

    // 5. The signature.
    let name = stem(rel);
    let signed_path = rel.dist.join(format!("{name}.aab"));
    let unsigned_path = rel.dist.join(format!("{name}-unsigned.aab"));
    let key = upload_key(ctx, rel);
    let mut signed_with: Option<UploadKey> = None;
    if let Some(key) = &key {
        signed_with = sign(ctx, rel, tools, key, &work, &signed_path)?;
    }
    let aab = if signed_with.is_some() {
        rel.signed = true;
        signed_path
    } else {
        std::fs::copy(work.unsigned(), &unsigned_path)
            .map_err(|e| io("copy", &unsigned_path, &e))?;
        let why = match rel.sign() {
            SignMode::None => "--sign none".to_string(),
            SignMode::Auto if key.is_some() => "the upload key could not sign it".to_string(),
            SignMode::Auto => {
                "[android.signing] upload, its keystore or its password variables are missing"
                    .to_string()
            }
        };
        rel.check(
            ctx,
            Check::warn(
                CheckId::AndroidAabUnsigned,
                format!(
                    "{} is unsigned ({why}); Google Play takes only a bundle signed with the upload key",
                    crate::paths::display(&unsigned_path)
                ),
            )
            .fix(
                "The owner signs it with the jarsigner line in UPLOAD.md, or releases again with [android.signing] upload and its password variables set.",
                &["icm upload-commands android"],
            ),
        );
        unsigned_path
    };
    rel.add_file("upload", "aab", &aab)?;
    rel.embed_notices(&aab, &format!("base/{}", bundle::NOTICES_ENTRY))?;
    rel.signing = match &signed_with {
        Some(found) => json!({
            "kind": "upload-key",
            "keystore": key.as_ref().map(|k| crate::paths::display(&k.keystore)),
            "alias": key.as_ref().map(|k| k.alias.clone()),
            "certificate_sha256": found.sha256,
            "owner": found.owner,
            "algorithm": found.algorithm.map(|a| a.sigalg()),
        }),
        None => Value::Null,
    };
    if let Some(found) = &signed_with {
        signature_checks(
            ctx,
            tools,
            &aab,
            found.sha256.as_deref(),
            None,
            &mut |check| rel.check(ctx, check),
        )?;
    }

    // 6. The store gates on the linked bundle.
    let expect = Expect {
        version: Some(rel.version.clone()),
        build: Some(rel.build),
        lib: Some(lib.clone()),
        abis: abis.clone(),
        target_sdk_floor: target_sdk_floor(),
        native: config.android.activity == "native",
    };
    gate_bundle(
        ctx,
        tools,
        bundletool,
        &aab,
        &expect,
        &work.gates(),
        &mut |check| rel.check(ctx, check),
    )?;

    // 7. The listing icon and a copy of the manifest.
    let icon = rel.dist.join("play-icon-512.png");
    play_icon(&project, &icon)?;
    rel.add_file("listing", "play_icon", &icon)?;
    let manifest_copy = rel.dist.join("AndroidManifest.xml");
    std::fs::copy(&resources.manifest, &manifest_copy)
        .map_err(|e| io("copy", &manifest_copy, &e))?;
    rel.add_file("metadata", "android_manifest", &manifest_copy)?;

    // 8. --apk.
    if rel.args.apk {
        universal_apk(
            ctx,
            rel,
            tools,
            bundletool,
            &aab,
            key.as_ref().filter(|_| signed_with.is_some()),
            &work,
        )?;
    }
    Ok(aab)
}

/// `bundletool validate` (`android.aab.validate`).
fn validate(ctx: &Ctx, bundletool: &Cmd, aab: &Path, report: &mut dyn FnMut(Check)) -> Result<()> {
    let cmd = bundletool
        .clone()
        .arg("validate")
        .arg(format!("--bundle={}", aab.display()))
        .timeout(Duration::from_secs(300));
    let outcome = ctx.step("bundletool.validate", &cmd)?;
    let check = if outcome.success() {
        Check::pass(
            CheckId::AndroidAabValidate,
            format!("bundletool validate accepts {}", crate::paths::display(aab)),
        )
    } else {
        let mut check = Check::fail(
            CheckId::AndroidAabValidate,
            format!(
                "bundletool validate rejects {}: {}",
                crate::paths::display(aab),
                outcome.stderr_tail(3)
            ),
        );
        if let Some(log) = &outcome.log {
            check = check.evidence(Evidence::file(log));
        }
        check
    };
    report(check);
    Ok(())
}

/// Reads the upload key and signs the bundle into `signed`. `Ok(None)`
/// when the key cannot be used: an owner item, deferred (the unsigned
/// bundle ships).
fn sign(
    ctx: &Ctx,
    rel: &mut Release,
    tools: &Toolset,
    key: &Key,
    work: &Work,
    signed: &Path,
) -> Result<Option<UploadKey>> {
    let outcome = ctx.step("keytool.list", &keytool_list(tools, key)?)?;
    let text = format!("{}\n{}", outcome.stdout_text(), outcome.stderr_text());
    if !outcome.success() {
        let why = bundle::key_failure(&text).unwrap_or_else(|| outcome.stderr_tail(2));
        let error = unreadable(rel, key, &why, outcome.log.as_deref());
        rel.needs_owner_later(ctx, error);
        return Ok(None);
    }
    let found = bundle::parse_keytool_list(&text);
    let Some(algorithm) = found.algorithm else {
        let error = unreadable(
            rel,
            key,
            "keytool did not name the key's algorithm (RSA, EC or DSA)",
            outcome.log.as_deref(),
        );
        rel.needs_owner_later(ctx, error);
        return Ok(None);
    };

    let _ = std::fs::remove_file(signed);
    let cmd = jarsigner_sign(tools, key, algorithm.sigalg(), &work.unsigned(), signed)?;
    let outcome = ctx.step("jarsigner.sign", &cmd)?;
    let text = format!("{}\n{}", outcome.stdout_text(), outcome.stderr_text());
    if !outcome.success() || !signed.is_file() {
        let _ = std::fs::remove_file(signed);
        match bundle::key_failure(&text) {
            Some(why) => {
                let error = unreadable(rel, key, &why, outcome.log.as_deref());
                rel.needs_owner_later(ctx, error);
            }
            None => {
                let mut check = Check::fail(
                    CheckId::AndroidAabSigned,
                    format!(
                        "jarsigner could not sign the bundle: {}",
                        outcome.stderr_tail(3)
                    ),
                );
                if let Some(log) = &outcome.log {
                    check = check.evidence(Evidence::file(log));
                }
                rel.check(ctx, check);
            }
        }
        return Ok(None);
    }
    Ok(Some(found))
}

/// `android.aab.signed` (design §11.2 step 7): `jarsigner -verify -verbose
/// -certs` without `-strict` must say `jar verified.` (and not `jar is
/// unsigned`, which exits 0), and `keytool -printcert -jarfile` must show
/// the upload key's certificate when it is known. A missing timestamp is
/// INFO. An unsigned bundle is FAIL `android.aab.signed`, or WARN
/// `android.aab.unsigned` (the owner's) when it is expected to be, saying
/// why (`icm verify` of a `--sign none` release, or of one that recorded
/// `signed: false`).
fn signature_checks(
    ctx: &Ctx,
    tools: &Toolset,
    aab: &Path,
    expected_sha256: Option<&str>,
    unsigned_expected: Option<&str>,
    report: &mut dyn FnMut(Check),
) -> Result<()> {
    let shown = crate::paths::display(aab);
    let verify = tools
        .jdk_tool("jarsigner")?
        .args(["-J-Duser.language=en", "-verify", "-verbose", "-certs"])
        .arg(aab)
        .timeout(Duration::from_secs(600));
    let outcome = ctx.step("jarsigner.verify", &verify)?;
    let verified = bundle::parse_jarsigner_verify(&outcome.stdout_text());
    let evidence = outcome.log.clone();
    let with_log = |check: Check| match &evidence {
        Some(log) => check.evidence(Evidence::file(log)),
        None => check,
    };
    if verified.unsigned
        && let Some(why) = unsigned_expected
    {
        report(
            Check::warn(
                CheckId::AndroidAabUnsigned,
                format!(
                    "{shown} is unsigned, {why}; Google Play takes only a bundle signed with the upload key"
                ),
            )
            .fix(
                "The owner signs it with the jarsigner line in UPLOAD.md, or releases again with [android.signing] upload and its password variables set.",
                &["icm upload-commands android"],
            ),
        );
        return Ok(());
    }
    if !verified.verified {
        let detail = if verified.unsigned {
            format!("{shown} is unsigned (jarsigner: jar is unsigned)")
        } else {
            format!(
                "jarsigner does not verify {shown}: {}",
                outcome.stderr_tail(3)
            )
        };
        report(with_log(Check::fail(CheckId::AndroidAabSigned, detail)));
        return Ok(());
    }

    let printcert = tools
        .keytool()?
        .args(["-J-Duser.language=en", "-printcert", "-jarfile"])
        .arg(aab)
        .timeout(Duration::from_secs(120));
    let outcome = ctx.step("keytool.printcert", &printcert)?;
    let found = bundle::parse_printcert(&outcome.stdout_text());
    let signer = verified.signers.first().cloned().unwrap_or_default();
    let check = match (found.as_deref(), expected_sha256) {
        (None, _) => Check::fail(
            CheckId::AndroidAabSigned,
            format!("keytool -printcert finds no signer certificate in {shown}"),
        ),
        (Some(found), Some(expected)) if !found.eq_ignore_ascii_case(expected) => Check::fail(
            CheckId::AndroidAabSigned,
            format!(
                "{shown} is signed by {signer} with certificate SHA-256 {found}, not the upload key's {expected}"
            ),
        ),
        (Some(found), _) => Check::pass(
            CheckId::AndroidAabSigned,
            format!("{shown} verifies, signed by {signer} (certificate SHA-256 {found})"),
        ),
    };
    report(with_log(check));
    if verified.no_timestamp {
        report(
            Check::info(
                CheckId::AndroidAabSigned,
                "the signature has no timestamp; Google Play does not need one (it re-signs the APKs it serves)",
            )
            .fix("Nothing to do.", &[]),
        );
    }
    Ok(())
}

/// The §12.3 gates on a bundle: `bundletool dump manifest` and `dump
/// config` (saved in `work` as the evidence), its entries, and the ELF
/// gates on every library extracted from it with the JDK's `jar`.
fn gate_bundle(
    ctx: &Ctx,
    tools: &Toolset,
    bundletool: &Cmd,
    aab: &Path,
    expect: &Expect,
    work: &Path,
    report: &mut dyn FnMut(Check),
) -> Result<()> {
    std::fs::create_dir_all(work).map_err(|e| io("create", work, &e))?;
    let dump = |what: &str, file: &str| -> Result<PathBuf> {
        let cmd = bundletool
            .clone()
            .args(["dump", what])
            .arg(format!("--bundle={}", aab.display()))
            .timeout(Duration::from_secs(300));
        let name = format!("bundletool.dump_{what}");
        let outcome = ctx.step(&name, &cmd)?;
        if !outcome.success() {
            return Err(ctx.step_failure(&name, CheckId::AndroidBundletoolFailed, &outcome));
        }
        let path = work.join(file);
        std::fs::write(&path, outcome.stdout_text()).map_err(|e| io("write", &path, &e))?;
        Ok(path)
    };
    let manifest_path = dump("manifest", "manifest.xml")?;
    let config_path = dump("config", "config.json")?;
    let names = crate::release::notices::zip_names(aab).ok_or_else(|| {
        IcmError::new(
            CheckId::AndroidAabValidate,
            format!("{} is not a zip archive", crate::paths::display(aab)),
        )
    })?;

    let manifest_text = std::fs::read_to_string(&manifest_path).unwrap_or_default();
    let dumped = bundle::parse_manifest(&manifest_text);
    for check in bundle::manifest_checks(&dumped, &names, expect, &manifest_path) {
        report(check);
    }
    let config_text = std::fs::read_to_string(&config_path).unwrap_or_default();
    report(bundle::config_check(&config_text, &config_path));

    // The libraries, as Google Play will see them.
    let libraries = bundle::libraries(&names);
    if libraries.is_empty() {
        return Ok(());
    }
    let extracted = work.join("extracted");
    if extracted.exists() {
        std::fs::remove_dir_all(&extracted).map_err(|e| io("clean", &extracted, &e))?;
    }
    std::fs::create_dir_all(&extracted).map_err(|e| io("create", &extracted, &e))?;
    let absolute = std::path::absolute(aab).unwrap_or_else(|_| aab.to_path_buf());
    let mut jar = tools
        .jdk_tool("jar")?
        .arg("xf")
        .arg(&absolute)
        .cwd(&extracted)
        .timeout(Duration::from_secs(300));
    for (_, entry) in &libraries {
        jar = jar.arg(entry);
    }
    apk::run_tool(ctx, "jar.extract", &jar, CheckId::ToolFailed)?;
    for (abi, entry) in &libraries {
        for check in bundle::library_checks(abi, &extracted.join(entry), entry) {
            report(check);
        }
    }
    Ok(())
}

/// `play-icon-512.png`: `[app] icon` flattened onto `[app] background` at
/// 512 px (the Play Console's listing icon), or the background alone.
fn play_icon(project: &crate::context::Project, out: &Path) -> Result<()> {
    use crate::android::image::{self, Rgba};
    let config = &project.config.config;
    let background = image::parse_hex_color(&config.app.background).unwrap_or([255, 255, 255]);
    let source = match &config.app.icon {
        Some(icon) => {
            let path = project.dir().join(icon);
            let bytes = std::fs::read(&path).map_err(|e| io("read", &path, &e))?;
            image::decode(&bytes)
                .map_err(|error| {
                    IcmError::new(
                        CheckId::AppIconInvalid,
                        format!(
                            "{} is not a readable PNG: {error}",
                            crate::paths::display(&path)
                        ),
                    )
                    .evidence(Evidence::file(&path))
                })?
                .flatten(background)
        }
        None => Rgba::filled(512, 512, [background[0], background[1], background[2], 255]),
    };
    let bytes = image::encode(&source.resize(512, 512)).map_err(|error| {
        IcmError::new(
            CheckId::InternalBug,
            format!("cannot encode the Play icon: {error}"),
        )
    })?;
    std::fs::write(out, bytes).map_err(|e| io("write", out, &e))
}

/// `--apk`: a universal APK for sideloading (design §11.2 step 10).
/// bundletool signs what it builds, so it signs with icm's debug key (never
/// `~/.android/debug.keystore`), and `apksigner` then replaces that
/// signature with the upload key's when the bundle is signed.
fn universal_apk(
    ctx: &Ctx,
    rel: &mut Release,
    tools: &Toolset,
    bundletool: &Cmd,
    aab: &Path,
    key: Option<&Key>,
    work: &Work,
) -> Result<()> {
    let debug = apk::ensure_debug_keystore(ctx, tools)?;
    let out = work.universal();
    let build = bundletool
        .clone()
        .arg("build-apks")
        .arg(format!("--bundle={}", aab.display()))
        .arg(format!("--output={}", out.display()))
        .args(["--output-format=directory", "--mode=universal"])
        .arg(format!("--ks={}", debug.display()))
        .arg(format!("--ks-pass=pass:{}", android::DEBUG_KEYSTORE_PASS))
        .arg(format!("--ks-key-alias={}", android::DEBUG_KEY_ALIAS))
        .arg(format!("--key-pass=pass:{}", android::DEBUG_KEYSTORE_PASS))
        .timeout(Duration::from_secs(600));
    apk::run_tool(
        ctx,
        "bundletool.build_apks.universal",
        &build,
        CheckId::AndroidBundletoolFailed,
    )?;
    let universal = out.join("universal.apk");
    if !universal.is_file() {
        return Err(IcmError::new(
            CheckId::AndroidBundletoolFailed,
            format!(
                "bundletool build-apks left no {}",
                crate::paths::display(&universal)
            ),
        ));
    }

    let name = stem(rel);
    let target = match key {
        Some(key) => {
            let target = rel.dist.join(format!("{name}-universal.apk"));
            let _ = std::fs::remove_file(&target);
            let sign = tools
                .apksigner()?
                .args(["sign", "--ks"])
                .arg(&key.keystore)
                .args(["--ks-key-alias", &key.alias])
                .arg("--ks-pass")
                .arg(format!("env:{}", key.store_pass_env))
                .arg("--key-pass")
                .arg(format!("env:{}", key.key_pass_env))
                // No `.idsig` beside the APK: v4 signatures are for
                // incremental adb installs, and a sideloaded APK is one file.
                .args(["--v4-signing-enabled", "false"])
                .arg("--out")
                .arg(&target)
                .arg(&universal)
                .timeout(Duration::from_secs(300));
            apk::run_tool(ctx, "apksigner.sign", &sign, CheckId::AndroidApkSignature)?;
            target
        }
        None => {
            let target = rel.dist.join(format!("{name}-universal-debugkey.apk"));
            std::fs::copy(&universal, &target).map_err(|e| io("copy", &target, &e))?;
            target
        }
    };
    let verify = tools
        .apksigner()?
        .args(["verify", "--print-certs"])
        .arg(&target)
        .timeout(Duration::from_secs(300));
    let outcome = ctx.step("apksigner.verify", &verify)?;
    let shown = crate::paths::display(&target);
    rel.check(
        ctx,
        if outcome.success() {
            Check::pass(
                CheckId::AndroidApkSignature,
                format!(
                    "{shown} verifies, signed with {}",
                    if key.is_some() {
                        "the upload key"
                    } else {
                        "icm's debug key (the bundle is unsigned): for testing only"
                    }
                ),
            )
        } else {
            Check::fail(
                CheckId::AndroidApkSignature,
                format!(
                    "apksigner does not verify {shown}: {}",
                    outcome.stderr_tail(3)
                ),
            )
        },
    );
    let align = tools
        .build_tool("zipalign")?
        .args(["-c", "-P", "16", "4"])
        .arg(&target)
        .timeout(Duration::from_secs(180));
    let outcome = ctx.step("zipalign.check", &align)?;
    rel.check(
        ctx,
        if outcome.success() {
            Check::pass(
                CheckId::AndroidApkZipalign,
                format!("{shown}: native libraries are aligned to 16 KB pages"),
            )
        } else {
            Check::fail(
                CheckId::AndroidApkZipalign,
                format!(
                    "{shown} is not aligned to 16 KB pages: {}",
                    outcome.stderr_tail(3)
                ),
            )
        },
    );
    rel.add_file("sideload", "apk", &target)?;
    rel.embed_notices(&target, bundle::NOTICES_ENTRY)?;
    Ok(())
}

// ---- the smoke install ------------------------------------------------------------------

/// The device a smoke install may use: one the owner chose
/// (`$ANDROID_SERIAL`, host.toml `android.device`) or a running emulator
/// of the AVD icm boots. Never the single online device: a release must
/// not install over whatever phone happens to be plugged in.
fn smoke_device(
    ctx: &Ctx,
    tools: &Toolset,
    host: &crate::host::HostConfig,
    target_sdk: u32,
) -> std::result::Result<(String, String), String> {
    let listed = adb::devices(tools).map_err(|error| error.detail)?;
    let online = |serial: &str| listed.iter().any(|d| d.serial == serial && d.online());
    if let Some(serial) = ctx.env.var("ANDROID_SERIAL") {
        return if online(serial) {
            Ok((serial.to_string(), "$ANDROID_SERIAL".to_string()))
        } else {
            Err(format!(
                "$ANDROID_SERIAL names {serial}, which is not online"
            ))
        };
    }
    if let Some(serial) = host.android.device.as_deref().filter(|s| !s.is_empty()) {
        return if online(serial) {
            Ok((serial.to_string(), "host.toml android.device".to_string()))
        } else {
            Err(format!(
                "host.toml android.device names {serial}, which is not online"
            ))
        };
    }
    let default = android::device::default_avd(host, target_sdk);
    android::device::running_emulators(tools, &listed)
        .into_iter()
        .find(|(_, avd)| avd.as_deref() == Some(default.as_str()))
        .map(|(serial, _)| (serial, format!("icm's emulator ({default})")))
        .ok_or_else(|| {
            format!("no emulator of {default} is running (and neither $ANDROID_SERIAL nor host.toml android.device names a device)")
        })
}

/// Installs the bundle through bundletool on a running device and checks
/// that the app draws (design §11.2 step 9). A failure is a FAIL check
/// (the release is not uploadable), never the end of the release.
fn smoke(
    ctx: &mut Ctx,
    rel: &mut Release,
    tools: &Toolset,
    host: &crate::host::HostConfig,
    bundletool: &Cmd,
    aab: &Path,
) -> Result<()> {
    let target_sdk = rel.config().android.target_sdk;
    let (serial, reason) = match smoke_device(ctx, tools, host, target_sdk) {
        Ok(found) => found,
        Err(why) => {
            rel.check(
                ctx,
                Check::skip(
                    CheckId::AndroidSmoke,
                    format!("smoke install skipped: {why}"),
                )
                .fix(
                    "Start icm's emulator and release again, or run the bundle with `icm run android --from-aab`.",
                    &["icm run android --from-aab --json -q"],
                ),
            );
            return Ok(());
        }
    };
    let _lock = ctx.lock_platform("android")?;
    let ctx: &Ctx = ctx;
    ctx.rep
        .progress(format!("smoke install on {serial} ({reason})"));
    let adb = adb::Adb::new(tools, &serial)?;
    let dir = ctx
        .rep
        .run_dir()
        .unwrap_or_else(|| rel.project.runs_dir().join(ctx.rep.run_id()));
    let app_id = rel.config().app.id.clone();
    let outcome = android::pipeline::install_bundle(
        ctx,
        tools,
        &adb,
        bundletool,
        aab,
        &app_id,
        &Work::new(rel).root,
    )
    .and_then(|()| android::pipeline::launch_installed(ctx, &rel.project, &adb, &dir, "smoke"));
    match outcome {
        Ok(started) => rel.check(
            ctx,
            Check::pass(
                CheckId::AndroidSmoke,
                format!(
                    "installed through bundletool on {serial} ({reason}); {}",
                    started
                ),
            ),
        ),
        Err(error) => rel.check(ctx, Check::from_error(error, Status::Fail)),
    }
    Ok(())
}

// ---- the owner's plan ---------------------------------------------------------------------

fn owner_plan(rel: &mut Release) {
    let name = stem(rel);
    let signed = format!("{name}.aab");
    let symbols = "native-debug-symbols.zip";
    let icon = "play-icon-512.png";
    let plan = {
        let common = rel.common();
        let mut plan = owner_plans::android(&common, &signed, Some(symbols), Some(icon));
        if !rel.signed
            && let Some(step) =
                owner_plans::android_sign(&common, &format!("{name}-unsigned.aab"), &signed)
        {
            plan.steps.insert(0, step);
        }
        plan
    };
    rel.owner_plan = Some(plan);
}

// ---- icm diagnose play ------------------------------------------------------------------

/// What one line of an upload's output means.
struct Finding {
    id: CheckId,
    line: usize,
    text: String,
    fix: &'static str,
}

/// Google Play's answers that icm knows, by the text it prints (the Play
/// Developer API's error messages, as fastlane and `curl` show them).
const PLAY_ERRORS: &[(&[&str], CheckId, &str)] = &[
    (
        &["version code", "already been used"],
        CheckId::VersionBuildNotIncreased,
        "Raise [app] build in icm.toml above every build Google Play has, release again and upload the new bundle.",
    ),
    (
        &["package not found"],
        CheckId::AndroidPlayAppMissing,
        "The owner creates the app in the Play Console and uploads the first bundle there by hand (UPLOAD.md), then records it with `icm ledger mark-uploaded android`.",
    ),
    (
        &["wrong key"],
        CheckId::AndroidPlayWrongKey,
        "The owner signs with the upload key registered in the Play Console (Setup > App signing), or asks Google to reset the upload key.",
    ),
    (
        &["signed with", "certificate", "not the"],
        CheckId::AndroidPlayWrongKey,
        "The owner signs with the upload key registered in the Play Console (Setup > App signing), or asks Google to reset the upload key.",
    ),
    (
        &["not signed"],
        CheckId::AndroidAabSigned,
        "Release with [android.signing] upload and its password variables set, so icm signs and verifies the bundle.",
    ),
    (
        &["target api level"],
        CheckId::AndroidManifestTargetSdk,
        "Raise [android] target_sdk to Google Play's floor (`icm print policy`) and release again.",
    ),
    (
        &["targets api level"],
        CheckId::AndroidManifestTargetSdk,
        "Raise [android] target_sdk to Google Play's floor (`icm print policy`) and release again.",
    ),
    (
        &["16 kb"],
        CheckId::AndroidSoAlign16k,
        "Build with NDK r28 or newer and release again; `icm verify android` shows the library at fault.",
    ),
    (
        &["caller does not have permission"],
        CheckId::AndroidPlayPermission,
        "The owner grants the service account release permissions for this app (Play Console > Users and permissions).",
    ),
    (
        &["permission_denied"],
        CheckId::AndroidPlayPermission,
        "The owner grants the service account release permissions for this app (Play Console > Users and permissions).",
    ),
    (
        &["invalid_grant"],
        CheckId::AndroidPlayPermission,
        "The owner creates a new JSON key for the service account and points the key variable at it.",
    ),
    (
        &["unauthenticated"],
        CheckId::AndroidPlayPermission,
        "The owner creates a new JSON key for the service account and points the key variable at it.",
    ),
    (
        &["google play android developer api has not been used"],
        CheckId::AndroidPlayPermission,
        "The owner enables the Google Play Android Developer API in the service account's Google Cloud project.",
    ),
];

/// What the output says the upload did.
fn read_play_output(text: &str) -> (Vec<Finding>, bool) {
    let mut findings: Vec<Finding> = Vec::new();
    let mut succeeded = false;
    for (index, line) in text.lines().enumerate() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("successfully finished the upload")
            || lower.contains("successfully uploaded")
            || lower.contains("\"expirytimeseconds\"")
        {
            succeeded = true;
        }
        let known = PLAY_ERRORS
            .iter()
            .find(|(needles, _, _)| needles.iter().all(|needle| lower.contains(needle)));
        if let Some((_, id, fix)) = known {
            if !findings.iter().any(|f| f.id == *id) {
                findings.push(Finding {
                    id: *id,
                    line: index + 1,
                    text: line.trim().to_string(),
                    fix,
                });
            }
            continue;
        }
        let error_line = lower.contains("google api error")
            || lower.contains("\"error\"")
            || lower.starts_with("[!]")
            || lower.contains("error: ");
        if error_line && !findings.iter().any(|f| f.line == index + 1) {
            findings.push(Finding {
                id: CheckId::AndroidPlayRejected,
                line: index + 1,
                text: line.trim().to_string(),
                fix: "Read the line: it is Google Play's own reason. Fix what it names, release a new build (a higher [app] build) and upload again.",
            });
        }
    }
    // A JSON error body names its message on its own line.
    if findings.is_empty()
        && let Ok(value) = serde_json::from_str::<Value>(text.trim())
        && let Some(message) = value
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
    {
        let (mut inner, _) = read_play_output(message);
        if inner.is_empty() {
            inner.push(Finding {
                id: CheckId::AndroidPlayRejected,
                line: 1,
                text: message.to_string(),
                fix: "Read the message: it is Google Play's own reason. Fix what it names and upload again.",
            });
        }
        for finding in &mut inner {
            finding.line = 1;
        }
        findings = inner;
    }
    (findings, succeeded)
}

/// `icm diagnose play <file|->`: reads what the owner's upload printed
/// (`upload.sh` saves fastlane's output as `supply.log`; a Play Developer
/// API error body works too) and maps Google Play's answer to catalogue
/// ids. The tool's exit code is never trusted.
pub fn diagnose(ctx: &mut Ctx, input: &Input) -> Result<()> {
    let (findings, succeeded) = read_play_output(&input.text);
    let evidence = |line: usize, text: &str| match &input.evidence {
        Some(file) => Evidence {
            line: Some(line as u32),
            excerpt: Some(text.to_string()),
            ..file.clone()
        },
        None => Evidence {
            path: "-".to_string(),
            line: Some(line as u32),
            excerpt: Some(text.to_string()),
        },
    };
    ctx.rep.set(
        "diagnosis",
        json!({
            "tool": "play",
            "uploaded": succeeded && findings.is_empty(),
            "findings": findings.iter().map(|f| json!({"id": f.id.id(), "line": f.line, "text": f.text})).collect::<Vec<_>>(),
        }),
    );
    if findings.is_empty() {
        if succeeded {
            ctx.rep.check(Check::pass(
                CheckId::AndroidPlayRejected,
                "Google Play took the upload",
            ));
            ctx.rep.summary(
                "Google Play took the upload; record it with `icm ledger mark-uploaded android`",
            );
            ctx.rep.next(
                "icm ledger mark-uploaded android --json -q",
                "record the upload, so the next release needs a higher build",
            );
            return Ok(());
        }
        return Err(IcmError::new(
            CheckId::AndroidPlayRejected,
            "the output shows neither a finished upload nor an error icm knows; read it, or run the upload again with its output saved",
        )
        .fix("Read the saved output; rerun upload.sh, which saves it as supply.log.", &[]));
    }
    let mut errors: Vec<IcmError> = Vec::new();
    for finding in &findings {
        let error = IcmError::new(finding.id, format!("Google Play: {}", finding.text))
            .evidence(evidence(finding.line, &finding.text))
            .fix(finding.fix, &[]);
        ctx.rep
            .check(Check::from_error(error.clone(), Status::Fail));
        errors.push(error);
    }
    ctx.rep.summary(format!(
        "Google Play refused the upload: {} ({})",
        findings[0].text,
        findings[0].id.id()
    ));
    Err(errors.remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_answers_map_to_ids() {
        let fastlane = "[12:00:01]: Preparing to upload for language 'en-US'...\n[12:00:03]: Uploading AAB to Google Play...\n[!] Google Api Error: Invalid request - APK specifies a version code that has already been used.\n";
        let (findings, ok) = read_play_output(fastlane);
        assert!(!ok);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, CheckId::VersionBuildNotIncreased);
        assert_eq!(findings[0].line, 3);

        let (findings, _) = read_play_output(
            "[!] Google Api Error: forbidden: The caller does not have permission\n",
        );
        assert_eq!(findings[0].id, CheckId::AndroidPlayPermission);
        let (findings, _) = read_play_output(
            "Google Api Error: Invalid request - Package not found: com.acme.notes.\n",
        );
        assert_eq!(findings[0].id, CheckId::AndroidPlayAppMissing);
        let (findings, _) = read_play_output(
            "Google Api Error: Your Android App Bundle is signed with the wrong key.\n",
        );
        assert_eq!(findings[0].id, CheckId::AndroidPlayWrongKey);
        let (findings, _) = read_play_output(
            "Google Api Error: Your app currently targets API level 34 and must target at least API level 35\n",
        );
        assert_eq!(findings[0].id, CheckId::AndroidManifestTargetSdk);
        let (findings, _) =
            read_play_output("[!] Google Api Error: Invalid request - Something new.\n");
        assert_eq!(findings[0].id, CheckId::AndroidPlayRejected);

        // The Play Developer API's own error body.
        let (findings, _) = read_play_output(
            "{\"error\": {\"code\": 403, \"message\": \"The caller does not have permission\", \"status\": \"PERMISSION_DENIED\"}}",
        );
        assert_eq!(findings[0].id, CheckId::AndroidPlayPermission);

        let (findings, ok) =
            read_play_output("[12:01:00]: Successfully finished the upload to Google Play\n");
        assert!(ok && findings.is_empty());
        let (findings, ok) = read_play_output("nothing here\n");
        assert!(!ok && findings.is_empty());
    }
}
