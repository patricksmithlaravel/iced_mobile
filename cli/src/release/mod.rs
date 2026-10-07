//! `icm release <target>` (design §11, §12): the release core every target
//! shares, and the contract each target's pipeline fills in.
//!
//! ## What the core does
//!
//! 1. Flags that do not apply to the target are usage errors; `--dry-run`
//!    prints the plan (the core's steps around the pipeline's) and stops.
//! 2. Preconditions, before any build:
//!    - `version.format` (exit 3): the Cargo version is `X.Y.Z` with no
//!      pre-release or build part;
//!    - owner items (exit 9 under `--sign auto`, WARN under `--sign
//!      none`; [`gates`]): a placeholder `[app] id` or icon (WARN for the
//!      web), and per target the owner's decisions (`config.owner_decision`:
//!      `[ios] team_id`, `uses_non_exempt_encryption`; `[android.signing]
//!      upload`, its keystore and password variables, deferred so the
//!      unsigned bundle is still built; `[desktop.windows] sign_command` and
//!      `sign_env`; the Linux pipeline adds `[desktop.linux] maintainer`
//!      for a `.deb`);
//!    - `store.metadata_missing` (WARN): the listing URLs `[store]` lacks;
//!    - the store policy table (`env.policy_stale`, `store.policy_upcoming`);
//!    - the pipeline's own preconditions ([`Pipeline::preconditions`]);
//!    - then every owner item blocks at once (exit 9, `owner_steps` lists
//!      them), and after that `version.build_not_increased` (the ledger,
//!      exit 1; a WARN under `--sign none`), `release.lock_missing` (the
//!      build is `--locked`, exit 1) and `release.dirty_tree` (changed
//!      tracked files or an uncommitted Cargo.lock; exit 1 unless
//!      `--allow-dirty`).
//! 3. The dist directory `target/icm/dist/<version>+<build>/<target>/` is
//!    emptied (unless [`Pipeline::keeps_dist`]) and the pipeline builds
//!    into it ([`Pipeline::build`]).
//! 4. `artifacts.json`, `UPLOAD.md` and `upload.sh` are written, every file
//!    is reported as an artifact (its kind is its key in the result's
//!    `artifacts`), `owner_steps` holds the owner's plan, and
//!    `dist/latest/<target>` moves to the new directory. Owner items that
//!    were deferred then end the command with exit 9.
//!
//! ## The pipeline contract
//!
//! A target's module implements [`Pipeline`]. It works through
//! [`Release`]:
//!
//! - build into [`Release::dist`] (shipped files) and [`Release::gen_dir`]
//!   (intermediates), and register each shipped file with
//!   [`Release::add_file`] (`upload` for what the owner uploads);
//! - report every gate with [`Release::check`] (never `ctx.rep.check`), so
//!   `artifacts.json` counts it and `--sign none` turns owner-dependent
//!   failures into WARNs; owner preconditions go through
//!   [`Release::needs_owner`] or [`Release::needs_owner_later`];
//! - build with [`Release::invocation`] and [`Release::cargo`] (the
//!   target's profile, `--locked`, the dedicated target directory, the
//!   deployment-target stamp; [`compile`]);
//! - once built, generate `THIRD_PARTY_NOTICES.txt` with
//!   [`Release::notices`], put it inside the artifacts and record where
//!   with [`Release::embed_notices`]; the core gates `release.notices`
//!   ([`notices`]);
//! - fetch pinned tools (bundletool, wasm-opt, appimagetool) with
//!   `crate::pinned::require`, which downloads only with `--yes`, and read
//!   store floors from `crate::policy`;
//! - set [`Release::signed`], [`Release::signing`] (references only) and
//!   record tool versions with [`Release::tool`];
//! - set [`Release::owner_plan`] from [`owner_plans`], the only file that
//!   may hold upload, publish or notarize argv;
//! - never upload, publish or notarize; a secret reaches a tool only
//!   through an environment variable or the keychain.
//!
//! Every target has its pipeline: [`ios`], [`android`], [`web`],
//! [`macos`], [`windows`] and [`linux`] (with [`desktop`], what the three
//! desktop ones share). `icm __test release <target>` runs the core with a
//! stand-in pipeline ([`fake`]) for icm's tests.

pub mod android;
pub mod compile;
pub mod desktop;
pub mod diagnose;
pub mod dist;
pub mod fake;
pub mod gates;
pub mod ios;
pub mod ledger;
pub mod linux;
pub mod macos;
pub mod manifest;
pub mod notices;
pub mod owner_plans;
pub mod upload;
pub mod verify;
pub mod web;
pub mod windows;

use crate::catalogue::CheckId;
use crate::cli::{ReleaseArgs, ReleaseTarget, SignMode};
use crate::config::IcmToml;
use crate::context::{Ctx, Project};
use crate::error::{Check, Evidence, IcmError, Result, Status};
use crate::plan::{Plan, Step};
use dist::FileEntry;
use gates::Gates;
use manifest::Manifest;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use upload::OwnerPlan;

/// A target's release pipeline (see the module docs for the contract).
pub trait Pipeline {
    /// The steps `--dry-run` prints between the core's.
    fn plan(&self, ctx: &Ctx, rel: &Release) -> Result<Plan>;

    /// Checks before the build: tools, SDKs, signing assets. Missing owner
    /// assets go through [`Release::needs_owner`]; anything else that stops
    /// the release is returned.
    fn preconditions(&self, ctx: &mut Ctx, rel: &mut Release) -> Result<()>;

    /// Whether the dist directory keeps what is in it (macOS stage 2
    /// builds the DMG next to the stapled app).
    fn keeps_dist(&self, _rel: &Release) -> bool {
        false
    }

    /// Builds, signs and gates the artifacts into [`Release::dist`].
    fn build(&self, ctx: &mut Ctx, rel: &mut Release) -> Result<()>;

    /// The target's store gates on an existing artifact (`icm verify`).
    fn verify(&self, ctx: &mut Ctx, verify: &mut verify::Verify) -> Result<()>;

    /// A file `artifacts.json` lists whose size or sha256 changed since
    /// the release: the `release.artifact_changed` check to report when the
    /// target can tell why (macOS: a stapled notarization ticket), else
    /// `None` and verify reports the change as a FAIL.
    fn changed_file(
        &self,
        _ctx: &Ctx,
        _file: &FileEntry,
        _path: &Path,
        _now: (u64, &str),
    ) -> Result<Option<Check>> {
        Ok(None)
    }
}

/// The pipeline of a target.
pub fn pipeline(target: ReleaseTarget) -> &'static dyn Pipeline {
    match target {
        ReleaseTarget::Ios => &ios::Ios,
        ReleaseTarget::Android => &android::Android,
        ReleaseTarget::Web => &web::Web,
        ReleaseTarget::Macos => &macos::Macos,
        ReleaseTarget::Windows => &windows::Windows,
        ReleaseTarget::Linux => &linux::Linux,
    }
}

/// The error of a pipeline this build does not implement yet.
pub fn not_implemented(target: ReleaseTarget, what: &str) -> IcmError {
    IcmError::new(
        CheckId::UsageNotImplemented,
        format!(
            "the {} release pipeline ({what}) is not implemented in this build of icm ({})",
            target.as_str(),
            crate::buildinfo::VERSION_LINE
        ),
    )
    .fix(
        "Install an icm whose release pipeline for this target is implemented; `icm release --help` lists the targets.",
        &["icm release --help"],
    )
}

/// One release in progress.
pub struct Release {
    /// The target.
    pub target: ReleaseTarget,
    /// The flags.
    pub args: ReleaseArgs,
    /// The project.
    pub project: Project,
    /// The package the target builds.
    pub package: crate::cargo::Package,
    /// The Cargo version.
    pub version: String,
    /// `[app] build`.
    pub build: u64,
    /// `target/icm/dist/<version>+<build>/<target>`: what ships.
    pub dist: PathBuf,
    /// `target/icm/gen/<target>/release`: intermediates.
    pub gen_dir: PathBuf,
    /// When the release started (UTC, RFC 3339).
    pub created: String,
    /// The gates so far.
    pub gates: Gates,
    /// Whether the upload files are signed for the store.
    pub signed: bool,
    /// The identity and profile or key used (references only).
    pub signing: Value,
    /// Tool versions for `artifacts.json`.
    pub tools: BTreeMap<String, String>,
    /// What the owner runs next.
    pub owner_plan: Option<OwnerPlan>,
    /// The upload ledger.
    pub ledger: ledger::Ledger,
    /// Where the artifacts carry the third-party notices.
    pub notices: Vec<manifest::NoticesAt>,
    /// Whether the build's Cargo.lock is not committed (`--allow-dirty`
    /// records the release as dirty).
    lock_uncommitted: bool,
    notices_ready: bool,
    files: Vec<FileEntry>,
}

impl Release {
    /// A release of the project for `args.target`.
    pub fn new(project: &Project, args: &ReleaseArgs) -> Result<Release> {
        let target = args.target;
        let package = project.package_for(ledger::platform_key(target))?.clone();
        let version = package.version.clone();
        let build = project.app().build;
        let mut tools = BTreeMap::new();
        let _ = tools.insert(
            "icm".to_string(),
            crate::buildinfo::VERSION_LINE.to_string(),
        );
        Ok(Release {
            target,
            args: args.clone(),
            dist: dist::dir(project, &version, build, target.as_str()),
            gen_dir: project
                .icm_dir
                .join("gen")
                .join(target.as_str())
                .join("release"),
            created: crate::time::Utc::now().rfc3339(),
            gates: Gates::new(args.sign),
            signed: false,
            signing: Value::Null,
            tools,
            owner_plan: None,
            ledger: ledger::read(project.dir())?,
            notices: Vec::new(),
            lock_uncommitted: false,
            notices_ready: false,
            files: Vec::new(),
            project: project.clone(),
            package,
            version,
            build,
        })
    }

    /// icm.toml.
    pub fn config(&self) -> &IcmToml {
        &self.project.config.config
    }

    /// `--sign`.
    pub fn sign(&self) -> SignMode {
        self.args.sign
    }

    /// Reports a gate (see [`Gates::check`]).
    pub fn check(&mut self, ctx: &Ctx, check: Check) {
        self.gates.check(ctx, check);
    }

    /// An owner precondition that blocks before the build (exit 9; WARN
    /// under `--sign none`).
    pub fn needs_owner(&mut self, ctx: &Ctx, error: IcmError) {
        self.gates.needs_owner(ctx, error);
    }

    /// An owner precondition that ends the release with exit 9 after its
    /// (unsigned) artifacts are written.
    pub fn needs_owner_later(&mut self, ctx: &Ctx, error: IcmError) {
        self.gates.needs_owner_later(ctx, error);
    }

    /// Records a tool's version for `artifacts.json`.
    pub fn tool(&mut self, name: &str, version: impl Into<String>) {
        let _ = self.tools.insert(name.to_string(), version.into());
    }

    /// Registers a shipped file or directory inside [`Release::dist`]
    /// (replacing an earlier entry for the same path).
    pub fn add_file(&mut self, role: &str, kind: &str, path: &Path) -> Result<()> {
        let entry = FileEntry::new(&self.dist, role, kind, path).map_err(|error| {
            IcmError::new(
                CheckId::InternalBug,
                format!("cannot record {}: {error}", crate::paths::display(path)),
            )
        })?;
        self.files.retain(|file| file.path != entry.path);
        self.files.push(entry);
        Ok(())
    }

    /// Records the code directory hash of a registered file's signature
    /// (macOS; see [`FileEntry::cdhash`]).
    pub fn set_cdhash(&mut self, path: &Path, cdhash: Option<String>) {
        let dist = self.dist.clone();
        if let Some(entry) = self
            .files
            .iter_mut()
            .find(|file| file.absolute(&dist) == path)
        {
            entry.cdhash = cdhash;
        }
    }

    /// The registered files.
    pub fn files(&self) -> &[FileEntry] {
        &self.files
    }

    /// Whether no upload of this target is in the ledger yet.
    pub fn first_upload(&self) -> bool {
        self.ledger.max_build(self.target.as_str()).is_none()
    }

    /// What every owner plan needs.
    pub fn common(&self) -> owner_plans::Common<'_> {
        owner_plans::Common {
            target: self.target,
            config: self.config(),
            version: &self.version,
            build: self.build,
            first_upload: self.first_upload(),
            icm_toml: &self.project.config.path,
        }
    }

    /// The previous `artifacts.json` in the dist directory (macOS stage 2).
    pub fn previous_manifest(&self) -> Option<Manifest> {
        Manifest::read(&self.dist.join(manifest::FILE)).ok()
    }

    fn app_label(&self) -> String {
        format!(
            "{} {} (build {})",
            self.config().app.name,
            self.version,
            self.build
        )
    }

    /// The result's `release` object.
    fn json(&self) -> Value {
        json!({
            "target": self.target.as_str(),
            "version": self.version,
            "build": self.build,
            "sign": self.sign().as_str(),
            "dist": crate::paths::display(&self.dist),
        })
    }
}

/// Runs `icm release <target>`.
pub fn run(ctx: &mut Ctx, args: &ReleaseArgs) -> Result<()> {
    run_with(ctx, args, pipeline(args.target))
}

/// Runs the release core around a pipeline (`icm __test release` passes
/// a stand-in).
pub fn run_with(ctx: &mut Ctx, args: &ReleaseArgs, pipeline: &dyn Pipeline) -> Result<()> {
    check_flags(args)?;
    let project = ctx.project()?.clone();
    let mut rel = Release::new(&project, args)?;
    ctx.rep.set("profile", json!("release"));
    ctx.rep.set("release", rel.json());

    if ctx.dry_run() {
        let mut plan = preconditions_plan(&rel, pipeline.keeps_dist(&rel));
        plan.steps.extend(pipeline.plan(ctx, &rel)?.steps);
        plan.steps.extend(finish_plan(&rel).steps);
        plan.report(ctx);
        ctx.rep.summary(format!(
            "the plan for {} on {} (nothing was built)",
            rel.app_label(),
            args.target.as_str()
        ));
        return Ok(());
    }

    let _lock = ctx.lock_platform(&format!("release-{}", args.target.as_str()))?;
    version_format(&rel)?;
    owner_gates(ctx, &mut rel)?;
    store_metadata(ctx, &mut rel);
    for check in crate::policy::get().checks(crate::time::Day::today()) {
        rel.check(ctx, check);
    }
    pipeline.preconditions(ctx, &mut rel)?;
    if let Err(error) = rel.gates.checkpoint() {
        return Err(owner_exit(ctx, &rel, error, false));
    }
    build_number(ctx, &mut rel)?;
    source(ctx, &mut rel)?;

    prepare_dist(&rel.dist, !pipeline.keeps_dist(&rel))?;
    pipeline.build(ctx, &mut rel)?;
    finish(ctx, &mut rel)?;

    match rel.gates.finish() {
        Ok(()) => Ok(()),
        Err(error) => Err(owner_exit(ctx, &rel, error, true)),
    }
}

/// The flags each target takes.
fn check_flags(args: &ReleaseArgs) -> Result<()> {
    let only = |set: bool, flag: &str, target: ReleaseTarget| -> Result<()> {
        if set && args.target != target {
            return Err(IcmError::new(
                CheckId::UsageBadArgs,
                format!("{flag} applies to `icm release {}` only", target.as_str()),
            )
            .fix("Drop the flag.", &["icm release --help"]));
        }
        Ok(())
    };
    only(args.apk, "--apk", ReleaseTarget::Android)?;
    only(args.no_smoke, "--no-smoke", ReleaseTarget::Android)?;
    only(args.dmg, "--dmg", ReleaseTarget::Macos)?;
    only(args.universal, "--universal", ReleaseTarget::Macos)?;
    only(
        args.via_xcode_export,
        "--via-xcode-export",
        ReleaseTarget::Ios,
    )?;
    Ok(())
}

/// Evidence for the package's `version =` line.
fn version_evidence(manifest: &Path) -> Evidence {
    let text = std::fs::read_to_string(manifest).unwrap_or_default();
    let mut section = String::new();
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            section = trimmed.to_string();
            continue;
        }
        if section == "[package]" && trimmed.starts_with("version") && trimmed.contains('=') {
            return Evidence::line(manifest, index as u32 + 1, trimmed);
        }
    }
    Evidence::file(manifest)
}

/// `version.format`: the stores take `X.Y.Z`, without a pre-release or
/// build part.
fn version_format(rel: &Release) -> Result<()> {
    let ok = semver::Version::parse(&rel.version)
        .is_ok_and(|version| version.pre.is_empty() && version.build.is_empty());
    if ok {
        return Ok(());
    }
    Err(IcmError::new(
        CheckId::VersionFormat,
        format!(
            "package {} has version `{}`; store releases need X.Y.Z with no pre-release or build part",
            rel.package.name, rel.version
        ),
    )
    .evidence(version_evidence(&rel.package.manifest_path))
    .fix(
        "Set `version = \"X.Y.Z\"` in the package's Cargo.toml.",
        &[],
    ))
}

fn owner_decision(rel: &Release, key: &str, detail: String, fix: &str) -> IcmError {
    IcmError::new(
        CheckId::ConfigOwnerDecision,
        format!("{}: {detail}", rel.project.config.source.location_for(key)),
    )
    .evidence(rel.project.config.evidence(key))
    .fix(fix, &[])
}

/// Expands `~/` and resolves a relative path against the project.
pub fn resolve_path(project: &Project, path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), crate::paths::home()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => project.dir().join(path),
    }
}

/// Placeholders and the owner's per-target decisions.
fn owner_gates(ctx: &Ctx, rel: &mut Release) -> Result<()> {
    let config = rel.config().clone();
    let web = rel.target == ReleaseTarget::Web;

    // The placeholder id and icon: a WARN for the web, else the owner's.
    if config.id_is_placeholder() {
        let error = IcmError::new(
            CheckId::AppIdPlaceholder,
            format!(
                "{}: [app] id `{}` is a placeholder; a store id is permanent after the first upload",
                rel.project.config.source.location_for("app.id"),
                config.app.id
            ),
        )
        .evidence(rel.project.config.evidence("app.id"))
        .fix(
            "The owner chooses the permanent reverse-DNS id and sets [app] id in icm.toml.",
            &[],
        );
        if web {
            rel.check(ctx, Check::from_error(error, Status::Warn));
        } else {
            rel.needs_owner(ctx, error);
        }
    } else {
        rel.check(
            ctx,
            Check::pass(
                CheckId::AppIdPlaceholder,
                format!("[app] id {}", config.app.id),
            ),
        );
    }
    let icon = crate::commands::check::icon_check(&rel.project);
    match icon.status {
        Status::Fail => return Err(icon.into_error()),
        Status::Warn if !web => rel.needs_owner(ctx, icon.into_error()),
        _ => rel.check(ctx, icon),
    }

    match rel.target {
        ReleaseTarget::Ios => {
            if config.ios.team_id.is_none() {
                let error = owner_decision(
                    rel,
                    "ios.team_id",
                    "[ios] team_id is unset; App Store signing needs the Apple team".to_string(),
                    "The owner sets [ios] team_id = \"<10-character team id>\" (Apple Developer > Membership details).",
                );
                rel.needs_owner(ctx, error);
            }
            match config.ios.uses_non_exempt_encryption {
                None => {
                    let error = owner_decision(
                        rel,
                        "ios.uses_non_exempt_encryption",
                        "[ios] uses_non_exempt_encryption is unanswered; App Store Connect asks it for every build (export compliance)".to_string(),
                        "The owner answers it in icm.toml: [ios] uses_non_exempt_encryption = false (or true, with export_compliance_code).",
                    );
                    rel.needs_owner(ctx, error);
                }
                Some(true) if config.ios.export_compliance_code.is_none() => {
                    rel.check(
                        ctx,
                        Check::warn(
                            CheckId::IosExportComplianceDocumentation,
                            "the app uses non-exempt encryption and [ios] export_compliance_code is unset: App Store Connect asks for the export documentation",
                        )
                        .evidence(rel.project.config.evidence("ios.uses_non_exempt_encryption")),
                    );
                }
                Some(_) => {}
            }
        }
        ReleaseTarget::Android => android_signing(ctx, rel),
        ReleaseTarget::Windows => {
            let windows = &config.desktop.windows;
            match &windows.sign_command {
                None => {
                    let error = IcmError::new(
                        CheckId::WindowsSignNotConfigured,
                        "[desktop.windows] sign_command is unset; Windows releases are signed with the owner's command",
                    )
                    .evidence(rel.project.config.evidence("desktop.windows"))
                    .fix(
                        "The owner sets [desktop.windows] sign_command (with {file}) and lists the variables it reads in sign_env.",
                        &[],
                    );
                    rel.needs_owner(ctx, error);
                }
                Some(_) => {
                    for (index, name) in windows.sign_env.iter().enumerate() {
                        if ctx.env.var(name).is_none() {
                            let error = IcmError::new(
                                CheckId::WindowsSignNotConfigured,
                                format!("{name}, which sign_command reads, is not set"),
                            )
                            .evidence(
                                rel.project
                                    .config
                                    .evidence(&format!("desktop.windows.sign_env[{index}]")),
                            )
                            .fix(
                                format!(
                                    "The owner exports {name} in the shell that runs the release."
                                ),
                                &[],
                            );
                            rel.needs_owner(ctx, error);
                        }
                    }
                }
            }
        }
        ReleaseTarget::Web | ReleaseTarget::Macos | ReleaseTarget::Linux => {}
    }
    Ok(())
}

/// `[android.signing] upload`: configured, its keystore present, its
/// password variables set. Deferred: the unsigned bundle is still built
/// (design §11.2).
fn android_signing(ctx: &Ctx, rel: &mut Release) {
    let config = rel.config().clone();
    let Some(upload) = config
        .android
        .signing
        .as_ref()
        .and_then(|s| s.upload.as_ref())
    else {
        let error = owner_decision(
            rel,
            "android.signing",
            "[android.signing] upload is unset; Google Play needs the bundle signed with the owner's upload key".to_string(),
            "The owner creates the upload key (UPLOAD.md shows the keytool line) and sets [android.signing] upload = { keystore, alias, store_pass_env }.",
        );
        rel.needs_owner_later(ctx, error);
        return;
    };
    let keystore = resolve_path(&rel.project, &upload.keystore);
    if !keystore.is_file() {
        let error = IcmError::new(
            CheckId::AndroidKeystoreMissing,
            format!(
                "the upload keystore {} does not exist",
                crate::paths::display(&keystore)
            ),
        )
        .evidence(
            rel.project
                .config
                .evidence("android.signing.upload.keystore"),
        )
        .fix(
            "The owner creates it with keytool -genkeypair (UPLOAD.md shows the line) or fixes the path.",
            &[],
        );
        rel.needs_owner_later(ctx, error);
    }
    let mut names = vec![("store_pass_env", upload.store_pass_env.clone())];
    if let Some(key) = &upload.key_pass_env {
        names.push(("key_pass_env", key.clone()));
    }
    for (key, name) in names {
        if ctx.env.var(&name).is_none() {
            let error = IcmError::new(
                CheckId::AndroidKeystorePasswordEnvUnset,
                format!("{name}, which [android.signing] upload names for its password, is not set"),
            )
            .evidence(
                rel.project
                    .config
                    .evidence(&format!("android.signing.upload.{key}")),
            )
            .fix(
                format!("The owner exports {name} (from their password manager) in the shell that runs the release."),
                &[],
            );
            rel.needs_owner_later(ctx, error);
        }
    }
}

/// `store.metadata_missing`: the URLs the store listing requires.
fn store_metadata(ctx: &Ctx, rel: &mut Release) {
    let store = &rel.config().store;
    let wanted: &[(&str, bool)] = match rel.target {
        ReleaseTarget::Ios => &[
            ("privacy_policy_url", store.privacy_policy_url.is_some()),
            ("support_url", store.support_url.is_some()),
        ],
        ReleaseTarget::Android => &[("privacy_policy_url", store.privacy_policy_url.is_some())],
        _ => return,
    };
    let missing: Vec<&str> = wanted
        .iter()
        .filter(|(_, set)| !set)
        .map(|(key, _)| *key)
        .collect();
    let store_name = if rel.target == ReleaseTarget::Ios {
        "App Store Connect"
    } else {
        "Google Play"
    };
    let check = if missing.is_empty() {
        Check::pass(
            CheckId::StoreMetadataMissing,
            format!("[store] has the URLs {store_name}'s listing requires"),
        )
    } else {
        Check::warn(
            CheckId::StoreMetadataMissing,
            format!(
                "[store] {} unset; {store_name}'s listing requires {}",
                missing.join(" and "),
                if missing.len() > 1 { "them" } else { "it" }
            ),
        )
        .evidence(rel.project.config.evidence("store"))
    };
    rel.check(ctx, check);
}

/// `version.build_not_increased`: `[app] build` above the ledger's
/// highest build of the target.
fn build_number(ctx: &Ctx, rel: &mut Release) -> Result<()> {
    let target = rel.target.as_str();
    let ledger = crate::paths::display(&ledger::path(rel.project.dir()));
    match rel.ledger.max_build(target) {
        Some(max) if rel.build <= max => {
            let error = IcmError::new(
                CheckId::VersionBuildNotIncreased,
                format!(
                    "{}: [app] build {} is not above {max}, the highest {target} build in {ledger}",
                    rel.project.config.source.location_for("app.build"),
                    rel.build
                ),
            )
            .evidence(rel.project.config.evidence("app.build"))
            .fix(
                format!("Set [app] build = {} (or higher) in icm.toml.", max + 1),
                &[],
            );
            if rel.sign() == SignMode::None {
                rel.check(ctx, Check::from_error(error, Status::Warn));
                Ok(())
            } else {
                Err(error)
            }
        }
        Some(max) => {
            rel.check(
                ctx,
                Check::pass(
                    CheckId::VersionBuildNotIncreased,
                    format!(
                        "build {} is above {max}, the last {target} upload",
                        rel.build
                    ),
                ),
            );
            Ok(())
        }
        None => {
            rel.check(
                ctx,
                Check::pass(
                    CheckId::VersionBuildNotIncreased,
                    format!("build {}: no {target} upload is recorded yet", rel.build),
                ),
            );
            Ok(())
        }
    }
}

/// The `icm check` platform that resolves the lock of a target's package.
fn check_platform(target: ReleaseTarget) -> &'static str {
    match target {
        ReleaseTarget::Ios => "ios-device",
        ReleaseTarget::Android => "android",
        ReleaseTarget::Web => "web",
        ReleaseTarget::Macos | ReleaseTarget::Windows | ReleaseTarget::Linux => "desktop",
    }
}

/// `release.lock_missing`: a release builds the lock with `--locked`, so
/// it must exist before the build (a new app has none until `icm check`
/// or `icm run` resolves it).
fn lock_present(rel: &Release) -> Result<()> {
    let lock = rel.project.lock_path();
    if lock.is_file() {
        return Ok(());
    }
    let shown = crate::paths::display(&lock);
    let create = match rel.target {
        ReleaseTarget::Web => "icm doctor web --fix --yes".to_string(),
        target => format!("icm check {} --json -q", check_platform(target)),
    };
    Err(IcmError::new(
        CheckId::ReleaseLockMissing,
        format!("there is no {shown}; a release builds the committed lock with --locked, so it would fail in cargo"),
    )
    .evidence(Evidence::file(&rel.package.manifest_path))
    .fix(
        format!("Create Cargo.lock (`{create}` resolves it), then commit it, so the release names a commit that can rebuild it."),
        &[&create, &format!("git add {shown}")],
    ))
}

/// `release.dirty_tree`: a release comes from a commit, which holds the
/// Cargo.lock it builds with.
fn source(ctx: &Ctx, rel: &mut Release) -> Result<()> {
    lock_present(rel)?;
    let inputs = rel.project.inputs_json();
    let rev = inputs["git_rev"].as_str().map(str::to_string);
    let dirty = inputs["dirty"].as_bool();
    let Some(rev) = rev else {
        rel.check(
            ctx,
            Check::warn(
                CheckId::ReleaseDirtyTree,
                "the project has no git commit; artifacts.json records no source revision",
            )
            .fix(
                "Commit the project to git, so a release names the commit it was built from.",
                &[],
            ),
        );
        return Ok(());
    };
    let lock = rel.project.lock_path();
    let shown = crate::paths::display(&lock);
    let lock_uncommitted = rel.project.git_tracks(&lock) == Some(false);
    let mut problems = Vec::new();
    if dirty == Some(true) {
        problems.push(format!(
            "tracked files have uncommitted changes on top of {rev}"
        ));
    }
    if lock_uncommitted {
        problems.push(format!(
            "{shown} is not committed, so {rev} cannot rebuild what --locked builds"
        ));
    }
    if problems.is_empty() {
        rel.check(
            ctx,
            Check::pass(
                CheckId::ReleaseDirtyTree,
                format!("built from commit {rev}"),
            ),
        );
        return Ok(());
    }
    if rel.args.allow_dirty {
        rel.lock_uncommitted = lock_uncommitted;
        rel.check(
            ctx,
            Check::info(
                CheckId::ReleaseDirtyTree,
                format!(
                    "{} (--allow-dirty): artifacts.json records dirty: true",
                    problems.join("; ")
                ),
            ),
        );
        return Ok(());
    }
    let mut commands = vec!["git status --short".to_string()];
    if lock_uncommitted {
        commands.push(format!("git add {shown}"));
    }
    commands.push(format!("icm release {} --allow-dirty", rel.target.as_str()));
    Err(IcmError::new(
        CheckId::ReleaseDirtyTree,
        format!("{}; a release is built from a commit", problems.join("; ")),
    )
    .fix(
        if lock_uncommitted {
            "Commit Cargo.lock (remove it from .gitignore if it is listed there) and any other changes, or pass --allow-dirty (artifacts.json then records dirty: true)."
        } else {
            "Commit the changes, or pass --allow-dirty (artifacts.json then records dirty: true)."
        },
        &[],
    )
    .fix_commands(commands))
}

fn prepare_dist(dist: &Path, clean: bool) -> Result<()> {
    let io = |what: &str, error: std::io::Error| {
        IcmError::new(
            CheckId::InternalBug,
            format!("cannot {what} {}: {error}", crate::paths::display(dist)),
        )
    };
    if clean && dist.exists() {
        std::fs::remove_dir_all(dist).map_err(|e| io("empty", e))?;
    }
    std::fs::create_dir_all(dist).map_err(|e| io("create", e))
}

/// The exit-9 end. Before the build, `owner_steps` lists every owner item;
/// after it (deferred items: the artifacts are written but unsigned),
/// `owner_steps` keeps the owner items followed by the owner's plan.
fn owner_exit(ctx: &Ctx, rel: &Release, error: IcmError, built: bool) -> IcmError {
    let steps = rel.gates.owner_steps();
    let first = format!(
        "{} item(s), the first {} ({})",
        steps.len(),
        error.id,
        error.detail.lines().next().unwrap_or("")
    );
    if built {
        ctx.rep.summary(format!(
            "built {} for {} in {}, but it is not uploadable until the owner acts: {first}",
            rel.app_label(),
            rel.target.as_str(),
            crate::paths::display(&rel.dist)
        ));
    } else {
        ctx.rep.summary(format!(
            "the owner must act before {} can be released on {}: {first}",
            rel.app_label(),
            rel.target.as_str()
        ));
        ctx.rep.set("owner_steps", Value::Array(steps));
    }
    error
}

fn preconditions_plan(rel: &Release, keeps_dist: bool) -> Plan {
    let config = rel.config();
    let mut owner: Vec<String> = vec!["[app] id and icon are not placeholders".to_string()];
    match rel.target {
        ReleaseTarget::Ios => owner.push("[ios] team_id and uses_non_exempt_encryption are set".into()),
        ReleaseTarget::Android => owner.push(
            "[android.signing] upload, its keystore and password variables (unsigned bundle otherwise)".into(),
        ),
        ReleaseTarget::Windows => owner.push("[desktop.windows] sign_command and its sign_env variables".into()),
        _ => {}
    }
    let ledger = match rel.ledger.max_build(rel.target.as_str()) {
        Some(max) => format!("[app] build {} above the ledger's {max}", rel.build),
        None => format!("[app] build {} (no upload recorded yet)", rel.build),
    };
    let mut plan = Plan::new();
    plan.push(Step::internal(
        "release.preconditions",
        &format!(
            "version {} is X.Y.Z; {}; [store] listing URLs; the store policy table; {ledger}; a Cargo.lock, committed, and a clean git tree{} ({} {})",
            rel.version,
            owner.join("; "),
            if rel.args.allow_dirty { " (or --allow-dirty)" } else { "" },
            config.app.name,
            rel.sign().as_str(),
        ),
    ));
    plan.push(Step::internal(
        "release.dist",
        &if keeps_dist {
            format!(
                "keep {} (this stage builds next to the files of the one before, such as macOS's stapled app)",
                crate::paths::display(&rel.dist)
            )
        } else {
            format!("empty {}", crate::paths::display(&rel.dist))
        },
    ));
    plan
}

fn finish_plan(rel: &Release) -> Plan {
    let mut plan = Plan::new();
    plan.push(Step::internal(
        "release.manifest",
        &format!(
            "write {} ({})",
            crate::paths::display(&rel.dist.join(manifest::FILE)),
            manifest::SCHEMA
        ),
    ));
    plan.push(Step::internal(
        "release.upload",
        "write UPLOAD.md and upload.sh for the owner (icm never uploads, publishes or notarizes)",
    ));
    plan.push(Step::internal(
        "release.latest",
        &format!(
            "point {} at the new release",
            crate::paths::display(&dist::latest(&rel.project, rel.target.as_str()))
        ),
    ));
    plan
}

/// Why the release is not uploadable, if it is not.
fn not_uploadable(rel: &Release) -> Option<String> {
    if rel.sign() == SignMode::None {
        return Some("it is unsigned (`icm release --sign none`)".to_string());
    }
    if !rel.signed {
        return Some("it is not signed for the store".to_string());
    }
    let owner: Vec<String> = rel
        .gates
        .owner_items()
        .map(|error| error.id.to_string())
        .collect();
    if !owner.is_empty() {
        return Some(format!("the owner has to act first ({})", owner.join(", ")));
    }
    if !rel.gates.tally.ids_fail.is_empty() {
        return Some(format!(
            "gates failed ({})",
            rel.gates.tally.ids_fail.join(", ")
        ));
    }
    None
}

/// The listing facts UPLOAD.md shows.
fn listing(rel: &Release) -> Vec<(String, String)> {
    let store = &rel.config().store;
    let url = |value: &Option<String>, key: &str| match value {
        Some(url) => url.clone(),
        None => format!("missing: set [store] {key} in icm.toml"),
    };
    match rel.target {
        ReleaseTarget::Ios => vec![
            (
                "Privacy policy URL".to_string(),
                url(&store.privacy_policy_url, "privacy_policy_url"),
            ),
            (
                "Support URL".to_string(),
                url(&store.support_url, "support_url"),
            ),
            (
                "Marketing URL (optional)".to_string(),
                store.marketing_url.clone().unwrap_or_else(|| "none".into()),
            ),
        ],
        ReleaseTarget::Android => vec![(
            "Privacy policy URL".to_string(),
            url(&store.privacy_policy_url, "privacy_policy_url"),
        )],
        _ => Vec::new(),
    }
}

/// The framework source in Cargo.lock (`iced`'s).
fn framework_source(project: &Project) -> Option<String> {
    let lock = project.lock().ok()??;
    let iced = lock.named("iced").next()?;
    Some(iced.source.clone().unwrap_or_else(|| "path".to_string()))
}

/// artifacts.json, UPLOAD.md, upload.sh, the result, `dist/latest`.
fn finish(ctx: &Ctx, rel: &mut Release) -> Result<()> {
    let io = |what: &str, path: &Path, error: std::io::Error| {
        IcmError::new(
            CheckId::InternalBug,
            format!("cannot {what} {}: {error}", crate::paths::display(path)),
        )
    };

    // `release.notices`: every release carries THIRD_PARTY_NOTICES.
    for check in notices::checks(&rel.dist, &rel.notices) {
        rel.check(ctx, check);
    }

    let plan = rel.owner_plan.take().unwrap_or_else(|| {
        let mut plan = OwnerPlan::new("its destination");
        plan.push(owner_plans::mark_uploaded(&rel.common()));
        plan
    });
    let blocked = not_uploadable(rel);
    let uploadable = blocked.is_none();

    let inputs = rel.project.inputs_json();
    let manifest = Manifest {
        schema: manifest::SCHEMA.to_string(),
        target: rel.target.as_str().to_string(),
        created: rel.created.clone(),
        icm: crate::buildinfo::VERSION_LINE.to_string(),
        app: manifest::App {
            id: rel.config().app.id.clone(),
            name: rel.config().app.name.clone(),
            version: rel.version.clone(),
            build: rel.build,
        },
        source: manifest::Source {
            git_rev: inputs["git_rev"].as_str().map(str::to_string),
            dirty: inputs["dirty"]
                .as_bool()
                .map(|dirty| dirty || rel.lock_uncommitted),
            cargo_lock_sha256: inputs["cargo_lock_sha256"].as_str().map(str::to_string),
        },
        framework: manifest::Framework {
            source: framework_source(&rel.project),
        },
        tools: rel.tools.clone(),
        files: rel.files.clone(),
        sign: rel.sign().as_str().to_string(),
        signed: rel.signed,
        uploadable,
        signing: rel.signing.clone(),
        checks: rel.gates.tally.clone(),
        notices: rel.notices.clone(),
        owner_steps: plan.to_json(&rel.dist),
    };
    let manifest_path = rel.dist.join(manifest::FILE);
    manifest
        .write(&manifest_path)
        .map_err(|e| io("write", &manifest_path, e))?;

    let facts = upload::Facts {
        app: rel.app_label(),
        target: rel.target.as_str(),
        dist: &rel.dist,
        icm: crate::buildinfo::VERSION_LINE,
        created: &rel.created,
        source: match (&manifest.source.git_rev, manifest.source.dirty) {
            (Some(rev), Some(true)) => format!("commit {rev} with uncommitted changes"),
            (Some(rev), _) => format!("commit {rev}"),
            (None, _) => "a tree without a git commit".to_string(),
        },
        files: rel
            .files
            .iter()
            .map(|file| {
                (
                    file.role.clone(),
                    file.path.clone(),
                    file.bytes,
                    file.sha256.clone(),
                )
            })
            .collect(),
        not_uploadable: blocked.clone(),
        listing: listing(rel),
    };
    let md_path = rel.dist.join("UPLOAD.md");
    let sh_path = rel.dist.join("upload.sh");
    crate::output::rundir::write_atomic(&md_path, upload::upload_md(&plan, &facts).as_bytes())
        .map_err(|e| io("write", &md_path, e))?;
    crate::output::rundir::write_atomic(&sh_path, upload::upload_sh(&plan, &facts).as_bytes())
        .map_err(|e| io("write", &sh_path, e))?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sh_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| io("chmod", &sh_path, e))?;
    }

    for file in &rel.files {
        let mut extra = Map::new();
        let _ = extra.insert("bytes".into(), json!(file.bytes));
        let _ = extra.insert("sha256".into(), json!(file.sha256));
        let _ = extra.insert("role".into(), json!(file.role));
        ctx.rep
            .artifact_with(&file.kind, &file.absolute(&rel.dist), extra);
    }
    ctx.rep.artifact("manifest", &manifest_path);
    ctx.rep.artifact("upload_md", &md_path);
    ctx.rep.artifact("upload_sh", &sh_path);
    ctx.rep.artifact("dist", &rel.dist);

    let mut release = rel.json();
    release["signed"] = json!(rel.signed);
    release["uploadable"] = json!(uploadable);
    release["not_uploadable"] = json!(blocked);
    ctx.rep.set("release", release);
    ctx.rep.set("tools", json!(rel.tools));
    let mut steps = rel.gates.owner_steps();
    steps.extend(plan.to_json(&rel.dist));
    ctx.rep.set("owner_steps", Value::Array(steps));

    if let Err(error) = dist::link_latest(&rel.project, rel.target.as_str(), &rel.dist) {
        ctx.rep
            .progress(format!("could not update dist/latest: {error}"));
    }

    ctx.rep.summary(format!(
        "released {} for {}: {} file(s) in {}; {}",
        rel.app_label(),
        rel.target.as_str(),
        rel.files.len(),
        crate::paths::display(&rel.dist),
        match &blocked {
            None => "uploadable: hand UPLOAD.md (or upload.sh) to the owner".to_string(),
            Some(why) => format!("not uploadable: {why}"),
        }
    ));
    ctx.rep.next(
        format!("icm verify {} --json -q", rel.target.as_str()),
        "run the store gates again on the artifact",
    );
    ctx.rep.next(
        format!("icm upload-commands {}", rel.target.as_str()),
        "what the owner runs (UPLOAD.md); icm never uploads",
    );
    Ok(())
}

/// `icm upload-commands <target>`: UPLOAD.md of `dist/latest/<target>`.
pub fn upload_commands(ctx: &mut Ctx, target: ReleaseTarget) -> Result<()> {
    let project = ctx.project()?.clone();
    let Some(dir) = dist::resolve_latest(&project, target.as_str()) else {
        return Err(IcmError::new(
            CheckId::ReleaseNotFound,
            format!(
                "there is no {} release in {}",
                target.as_str(),
                crate::paths::display(&dist::latest(&project, target.as_str()))
            ),
        )
        .fix(
            "Make the release first.",
            &[&format!("icm release {} --json -q", target.as_str())],
        ));
    };
    let md = dir.join("UPLOAD.md");
    let text = std::fs::read_to_string(&md).map_err(|error| {
        IcmError::new(
            CheckId::ReleaseNotFound,
            format!("cannot read {}: {error}", crate::paths::display(&md)),
        )
    })?;
    if let Ok(manifest) = Manifest::read(&dir.join(manifest::FILE)) {
        ctx.rep.set("owner_steps", json!(manifest.owner_steps));
        ctx.rep.set("uploadable", json!(manifest.uploadable));
    }
    ctx.rep.set("upload_md", json!(crate::paths::display(&md)));
    ctx.rep.set(
        "upload_sh",
        json!(crate::paths::display(&dir.join("upload.sh"))),
    );
    ctx.rep.summary(format!(
        "UPLOAD.md of the newest {} release ({})",
        target.as_str(),
        crate::paths::display(&dir)
    ));
    ctx.rep.content(text);
    Ok(())
}
