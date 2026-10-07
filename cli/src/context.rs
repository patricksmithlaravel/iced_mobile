//! What every command gets: the global flags, the reporter, the
//! environment, the overall deadline, host.toml and the project (resolved on
//! first use), and helpers that run processes as reported steps.

use crate::cargo::{self, Artifact, Invocation, Message};
use crate::catalogue::CheckId;
use crate::cli::GlobalArgs;
use crate::config::{self, AppConfig, Loaded};
use crate::error::{Check, Evidence, IcmError, Result, Status};
use crate::host::{self, HostConfig, LoadedHost};
use crate::locks;
use crate::output::{self, Reporter};
use crate::process::{self, Cmd, End, Outcome};
use crate::tools::Env;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A command's context.
pub struct Ctx {
    /// The global flags.
    pub global: GlobalArgs,
    /// The reporter.
    pub rep: Reporter,
    /// The environment snapshot.
    pub env: Env,
    /// icm's argv.
    pub argv: Vec<String>,
    /// When the command started.
    pub started: Instant,
    deadline: Option<Instant>,
    host: Option<LoadedHost>,
    project: Option<Project>,
}

impl Ctx {
    /// A context.
    pub fn new(global: GlobalArgs, rep: Reporter, argv: Vec<String>) -> Ctx {
        let started = Instant::now();
        Ctx {
            deadline: global.timeout.map(|timeout| started + timeout),
            global,
            rep,
            env: Env::from_process(),
            argv,
            started,
            host: None,
            project: None,
        }
    }

    /// Whether `--dry-run` was given.
    pub fn dry_run(&self) -> bool {
        self.global.dry_run
    }

    /// The overall deadline (`--timeout`).
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Time left before the overall deadline.
    pub fn remaining(&self) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    /// Fails with `env.consent_required` unless `--yes` was given.
    pub fn require_yes(&self, what: &str) -> Result<()> {
        if self.global.yes {
            Ok(())
        } else {
            Err(IcmError::new(
                CheckId::EnvConsentRequired,
                format!("{what} downloads or installs; rerun with --yes"),
            ))
        }
    }

    /// host.toml (loaded once).
    pub fn host(&mut self) -> Result<&HostConfig> {
        if self.host.is_none() {
            match host::load() {
                Ok(loaded) => self.host = Some(loaded),
                Err(errors) => return Err(self.report_all(errors)),
            }
        }
        Ok(&self.host.as_ref().expect("loaded").config)
    }

    /// The project (resolved once): icm.toml, `cargo metadata`, the app
    /// package. Attaches the run directory under `<target>/icm` and sets the
    /// result's `app` and `inputs`.
    pub fn project(&mut self) -> Result<&Project> {
        if self.project.is_none() {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            match resolve(
                self.global.config.as_deref(),
                &cwd,
                self.global.offline,
                self.remaining(),
            ) {
                Ok(project) => {
                    let _ = self.rep.attach(&project.icm_dir);
                    self.rep.set("app", project.app_json());
                    self.rep
                        .set_workspace_root(&project.metadata.workspace_root);
                    self.rep.set("inputs", project.inputs_json());
                    self.project = Some(project);
                }
                Err(failure) => {
                    if let Some(root) = failure.root {
                        let _ = self.rep.attach(&root);
                    }
                    return Err(self.report_all(failure.errors));
                }
            }
        }
        Ok(self.project.as_ref().expect("resolved"))
    }

    /// The project if one resolves, without reporting a failure.
    pub fn try_project(&mut self) -> Option<&Project> {
        if self.project.is_none() {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            if let Ok(project) = resolve(
                self.global.config.as_deref(),
                &cwd,
                self.global.offline,
                self.remaining(),
            ) {
                self.project = Some(project);
            }
        }
        self.project.as_ref()
    }

    /// Reports every error but the first as a check and returns the first,
    /// which the command returns to block.
    pub fn report_all(&self, mut errors: Vec<IcmError>) -> IcmError {
        if errors.is_empty() {
            return IcmError::new(CheckId::InternalBug, "an empty error list was reported");
        }
        let first = errors.remove(0);
        for error in errors {
            self.rep.check(Check::from_error(error, Status::Fail));
        }
        first
    }

    /// Takes this project's lock for a platform (`--wait-lock` honoured).
    pub fn lock_platform(&mut self, platform: &str) -> Result<locks::Lock> {
        let wait = self.global.wait_lock;
        let run = self.rep.run_id();
        let dir = self.project()?.locks_dir();
        locks::acquire(&dir, platform, wait, &run).inspect_err(|error| {
            // A detached holder: its `icm wait` is the next step.
            for command in &error.fix.commands {
                self.rep
                    .next(command.clone(), "wait for the run that holds the lock");
            }
        })
    }

    fn bounded(&self, cmd: &Cmd) -> (Cmd, bool) {
        let mut cmd = cmd.clone();
        let mut overall = false;
        if let Some(remaining) = self.remaining()
            && cmd.timeout.is_none_or(|timeout| remaining < timeout)
        {
            cmd.timeout = Some(remaining);
            overall = true;
        }
        (cmd, overall)
    }

    /// Runs a process as a reported step (`STEP` line, `step` events, a
    /// step log). A non-zero exit is returned as an outcome for the caller
    /// to judge (see [`Ctx::step_failure`]); a timeout, a signal or a
    /// missing program is an error.
    pub fn step(&self, name: &str, cmd: &Cmd) -> Result<Outcome> {
        self.step_with(name, cmd, None)
    }

    /// [`Ctx::step`] with a callback for each stdout line.
    pub fn step_with(
        &self,
        name: &str,
        cmd: &Cmd,
        on_line: Option<&mut dyn FnMut(&str)>,
    ) -> Result<Outcome> {
        let (cmd, overall) = self.bounded(cmd);
        let cmd = cmd.mirrored(self.global.verbose);
        let log = self.rep.step_log(name);
        self.rep.step_begin(
            name,
            &cmd.display_argv(),
            &cmd.display_env(),
            cmd.cwd.as_deref(),
        );

        let outcome = process::run(&cmd, log.as_deref(), on_line)
            .map_err(|error| spawn_error(&cmd, &error))?;
        self.rep.step_end(name, &outcome);
        self.judge_end(name, &cmd, &outcome, overall)?;
        Ok(outcome)
    }

    /// Runs a quick probe (no step event, no log). Errors as for [`Ctx::step`].
    pub fn probe(&self, cmd: &Cmd) -> Result<Outcome> {
        let (cmd, overall) = self.bounded(cmd);
        let outcome = process::run(&cmd, None, None).map_err(|error| spawn_error(&cmd, &error))?;
        self.judge_end(&cmd.program_name(), &cmd, &outcome, overall)?;
        Ok(outcome)
    }

    fn judge_end(&self, name: &str, cmd: &Cmd, outcome: &Outcome, overall: bool) -> Result<()> {
        match end_error(name, cmd, outcome, overall) {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// The error for a step that exited unsuccessfully: the given id, the
    /// exit status, the last lines of its output and its log as evidence.
    pub fn step_failure(&self, name: &str, id: CheckId, outcome: &Outcome) -> IcmError {
        let tail = outcome.stderr_tail(6);
        let mut detail = format!("{name} failed ({})", outcome.describe());
        if !tail.is_empty() {
            detail.push_str(&format!(":\n{tail}"));
        }
        let mut error = IcmError::new(id, detail);
        if let Some(log) = &outcome.log {
            let excerpt = tail.lines().last().unwrap_or("").to_string();
            error = error.evidence(Evidence::file(log).with_excerpt(excerpt));
        }
        error
    }

    /// Prepares an Apple build's deployment target (Appendix C item 6).
    /// Cargo does not rebuild when `IPHONEOS_DEPLOYMENT_TARGET` or
    /// `MACOSX_DEPLOYMENT_TARGET` changes, so when the stamp for this triple
    /// and profile differs (or a build exists that icm did not stamp), the
    /// app package is cleaned first (`cargo clean -p <pkg> --target <t>`)
    /// so only the app relinks.
    ///
    /// Returns the `(VAR, value)` to put in cargo's environment and the
    /// stamp to [`write`](cargo::DeploymentStamp::write) after a successful
    /// build; `None` for triples without a deployment target (Android,
    /// wasm, Linux). `triple: None` is the host.
    pub fn deployment_target(
        &self,
        project: &Project,
        package: &str,
        triple: Option<&str>,
        profile: &str,
        min_os: &str,
    ) -> Result<Option<((String, String), cargo::DeploymentStamp)>> {
        let resolved = triple.unwrap_or(crate::toolchain::host_triple());
        let Some(var) = cargo::deployment_var(resolved) else {
            return Ok(None);
        };

        let stamp = cargo::DeploymentStamp::new(&project.icm_dir, triple, profile, var, min_os);
        let artifacts = cargo::artifacts_dir(&project.target_dir, triple, profile);
        if let cargo::StampAction::Clean { reason } = stamp.action(&artifacts) {
            self.rep.progress(format!("relinking {package}: {reason}"));
            let manifest = &project.package_for_name(package)?.manifest_path;
            let cmd = cargo::clean_cmd(manifest, package, triple, profile);
            let outcome = self.step("cargo.clean.deployment_target", &cmd)?;
            if !outcome.success() {
                return Err(self.step_failure(
                    "cargo.clean.deployment_target",
                    CheckId::BuildCargoFailed,
                    &outcome,
                ));
            }
        }

        Ok(Some(((var.to_string(), min_os.to_string()), stamp)))
    }

    /// Runs a cargo invocation as a step: diagnostics become `diagnostic`
    /// events (first ones attached to `errors[0]`), artifacts are collected,
    /// and a failure is `build.compile_error`, `build.link_error` or
    /// `build.cargo_failed` (exit 5).
    pub fn cargo(
        &self,
        name: &str,
        invocation: &Invocation,
        env: &[(String, String)],
    ) -> Result<CargoOutput> {
        let mut invocation = invocation.clone();
        invocation.offline |= self.global.offline;
        let cmd = invocation
            .cmd()
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));

        let rep = self.rep.clone();
        let mut artifacts: Vec<Artifact> = Vec::new();
        let mut on_line = |line: &str| match cargo::parse_message(line) {
            Some(Message::Artifact(artifact)) => artifacts.push(artifact),
            Some(Message::Diagnostic(diagnostic)) => rep.diagnostic(diagnostic),
            _ => {}
        };

        let outcome = self.step_with(name, &cmd, Some(&mut on_line))?;
        if outcome.success() {
            return Ok(CargoOutput { artifacts, outcome });
        }

        let diagnostics = self.rep.error_diagnostics();
        let stderr = outcome.stderr_text();
        let id = if cargo::is_link_failure(&stderr, &diagnostics) {
            CheckId::BuildLinkError
        } else if diagnostics.is_empty() {
            CheckId::BuildCargoFailed
        } else {
            CheckId::BuildCompileError
        };
        let mut error = self.step_failure(name, id, &outcome);
        if let Some(first) = diagnostics.first() {
            error.detail = format!(
                "{name}: {} error(s); first: {}{}",
                diagnostics.len(),
                first.message,
                match (&first.file, first.line) {
                    (Some(file), Some(line)) => format!(" at {file}:{line}"),
                    _ => String::new(),
                }
            );
        }
        Err(error)
    }
}

fn spawn_error(cmd: &Cmd, error: &std::io::Error) -> IcmError {
    if error.kind() == std::io::ErrorKind::NotFound {
        IcmError::new(
            CheckId::EnvToolMissing,
            format!("`{}` was not found", cmd.program.to_string_lossy()),
        )
        .fix_commands([format!(
            "icm doctor --fix --yes   # or set ICM_TOOL_{}",
            cmd.program_name()
                .to_ascii_uppercase()
                .replace(['-', '.'], "_")
        )])
    } else {
        IcmError::new(
            CheckId::ToolFailed,
            format!("cannot run `{}`: {error}", cmd.display()),
        )
    }
}

/// A finished cargo invocation.
#[derive(Clone, Debug)]
pub struct CargoOutput {
    /// The `compiler-artifact` messages.
    pub artifacts: Vec<Artifact>,
    /// The process outcome.
    pub outcome: Outcome,
}

impl CargoOutput {
    /// The executable of a binary target.
    pub fn executable(&self, bin: &str) -> Option<&Path> {
        self.artifacts
            .iter()
            .filter(|artifact| artifact.target_name == bin)
            .find_map(|artifact| artifact.executable.as_deref())
    }

    /// A produced file with an extension (`so`, `wasm`, `dylib`).
    pub fn file_with_extension(&self, target: &str, extension: &str) -> Option<&Path> {
        self.artifacts
            .iter()
            .filter(|artifact| artifact.target_name == target)
            .flat_map(|artifact| artifact.filenames.iter())
            .map(PathBuf::as_path)
            .find(|path| path.extension().is_some_and(|ext| ext == extension))
    }
}

/// A resolved project.
#[derive(Clone, Debug)]
pub struct Project {
    /// icm.toml.
    pub config: Loaded,
    /// `cargo metadata --no-deps`.
    pub metadata: cargo::Metadata,
    /// The app package (default for every platform).
    pub package: cargo::Package,
    /// Cargo's target directory.
    pub target_dir: PathBuf,
    /// `<target>/icm`.
    pub icm_dir: PathBuf,
}

/// Why a project did not resolve, and where its runs should go.
#[derive(Debug)]
pub struct ResolveFailure {
    /// Every problem, the first first.
    pub errors: Vec<IcmError>,
    /// The icm root to keep the run in, once icm.toml was found.
    pub root: Option<PathBuf>,
}

/// The error for a process that icm killed: at its time limit
/// (`step.timeout`; `overall` when the limit was what was left of
/// `--timeout`) or because icm got a signal (`run.interrupted`). `None` for
/// a process that exited by itself, which the caller judges. Callers that
/// run a process without [`Ctx::step`] or [`Ctx::probe`] check this before
/// reading a failure as their own.
pub fn end_error(name: &str, cmd: &Cmd, outcome: &Outcome, overall: bool) -> Option<IcmError> {
    match outcome.end {
        End::TimedOut(limit) => {
            let what = if overall {
                format!("the overall --timeout ran out during step {name}")
            } else {
                format!(
                    "step {name} exceeded its {} limit",
                    crate::time::format_duration(limit)
                )
            };
            let mut error = IcmError::new(
                CheckId::StepTimeout,
                format!("{what}; its process group was killed ({})", cmd.display()),
            );
            if let Some(log) = &outcome.log {
                error = error.evidence(Evidence::file(log));
            }
            Some(error)
        }
        End::Interrupted(signal) => Some(output::interrupted(signal)),
        End::Exited(_) | End::Signaled(_) => None,
    }
}

/// Resolves the project from `--config` or the current directory.
/// `limit` is what is left of `--timeout`, if given.
pub fn resolve(
    explicit: Option<&Path>,
    cwd: &Path,
    offline: bool,
    limit: Option<Duration>,
) -> std::result::Result<Project, ResolveFailure> {
    let path = config::locate(explicit, cwd).map_err(|error| ResolveFailure {
        errors: vec![error],
        root: None,
    })?;

    let dir = std::path::absolute(&path)
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| cwd.to_path_buf());
    let fallback_root = std::env::var_os("CARGO_TARGET_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join("target"))
        .join("icm");
    let fail = |errors: Vec<IcmError>| ResolveFailure {
        errors,
        root: Some(fallback_root.clone()),
    };

    let loaded = config::load(&path).map_err(&fail)?;
    let metadata =
        cargo::metadata(&loaded.dir, offline, limit).map_err(|error| fail(vec![error]))?;

    let package = match loaded.config.app.package.as_deref() {
        Some(name) => metadata.member(name).cloned().ok_or_else(|| {
            fail(vec![package_not_found(&loaded, &metadata, "app.package", name)])
        })?,
        None => metadata.member_in(&loaded.dir).cloned().ok_or_else(|| {
            fail(vec![
                IcmError::new(
                    CheckId::ConfigPackageNotFound,
                    format!(
                        "no workspace package has its Cargo.toml next to {}; set [app] package to one of: {}",
                        crate::paths::display(&loaded.path),
                        member_names(&metadata)
                    ),
                )
                .evidence(Evidence::file(&loaded.path)),
            ])
        })?,
    };

    let target_dir = metadata.target_directory.clone();
    Ok(Project {
        icm_dir: target_dir.join("icm"),
        target_dir,
        config: loaded,
        metadata,
        package,
    })
}

fn member_names(metadata: &cargo::Metadata) -> String {
    metadata
        .members()
        .map(|p| p.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn package_not_found(
    loaded: &Loaded,
    metadata: &cargo::Metadata,
    key: &str,
    name: &str,
) -> IcmError {
    IcmError::new(
        CheckId::ConfigPackageNotFound,
        format!(
            "{}: `{key}` names `{name}`, which is not a workspace package (members: {})",
            loaded.source.location_for(key),
            member_names(metadata)
        ),
    )
    .evidence(loaded.evidence(key))
}

impl Project {
    /// The project directory (where icm.toml is).
    pub fn dir(&self) -> &Path {
        &self.config.dir
    }

    /// `[app]`.
    pub fn app(&self) -> &AppConfig {
        &self.config.config.app
    }

    /// The package and override key for a platform.
    fn override_for(&self, platform: &str) -> (Option<&str>, &'static str) {
        let config = &self.config.config;
        match platform {
            "ios-sim" | "ios-device" | "ios" => (config.ios.package.as_deref(), "ios.package"),
            "android" => (config.android.package.as_deref(), "android.package"),
            "web" => (config.web.package.as_deref(), "web.package"),
            "desktop" => (config.desktop.package.as_deref(), "desktop.package"),
            _ => (None, "app.package"),
        }
    }

    /// A workspace package by name.
    pub fn package_for_name(&self, name: &str) -> Result<&cargo::Package> {
        self.metadata.member(name).ok_or_else(|| {
            IcmError::new(
                CheckId::ConfigPackageNotFound,
                format!(
                    "`{name}` is not a workspace package (members: {})",
                    member_names(&self.metadata)
                ),
            )
        })
    }

    /// The package a platform builds.
    pub fn package_for(&self, platform: &str) -> Result<&cargo::Package> {
        match self.override_for(platform) {
            (Some(name), key) => self
                .metadata
                .member(name)
                .ok_or_else(|| package_not_found(&self.config, &self.metadata, key, name)),
            (None, _) => Ok(&self.package),
        }
    }

    /// The binary target a platform runs (iOS, desktop, web).
    pub fn bin_for(&self, platform: &str) -> Result<String> {
        let config = &self.config.config;
        let (name, key) = match platform {
            "ios-sim" | "ios-device" | "ios" => (config.ios.bin.as_deref(), "ios.bin"),
            "web" => (config.web.bin.as_deref(), "web.bin"),
            "desktop" => (config.desktop.bin.as_deref(), "desktop.bin"),
            _ => (None, "app.bin"),
        };
        let (name, key) = match name {
            Some(name) => (Some(name), key),
            None => (config.app.bin.as_deref(), "app.bin"),
        };
        let package = self.package_for(platform)?;
        let bins: Vec<&str> = package.bins().map(|t| t.name.as_str()).collect();

        match name {
            Some(name) if bins.contains(&name) => Ok(name.to_string()),
            Some(name) => Err(IcmError::new(
                CheckId::ConfigBinMissing,
                format!(
                    "{}: `{key}` names `{name}`, but package `{}` has binaries: [{}]",
                    self.config.source.location_for(key),
                    package.name,
                    bins.join(", ")
                ),
            )
            .evidence(self.config.evidence(key))),
            None if bins.contains(&package.name.as_str()) => Ok(package.name.clone()),
            None if bins.len() == 1 => Ok(bins[0].to_string()),
            None if bins.is_empty() => Err(IcmError::new(
                CheckId::ConfigBinMissing,
                format!(
                    "package `{}` has no binary target (src/main.rs)",
                    package.name
                ),
            )
            .evidence(Evidence::file(&package.manifest_path))),
            None => Err(IcmError::new(
                CheckId::ConfigBinMissing,
                format!(
                    "package `{}` has several binaries ({}); set [app] bin",
                    package.name,
                    bins.join(", ")
                ),
            )
            .evidence(self.config.evidence("app"))),
        }
    }

    /// The library target Android loads (`lib<name>.so`).
    pub fn lib_name(&self) -> Result<String> {
        let config = &self.config.config;
        let (name, key) = match config.android.lib.as_deref() {
            Some(name) => (Some(name), "android.lib"),
            None => (config.app.lib.as_deref(), "app.lib"),
        };
        let package = self.package_for("android")?;
        let Some(lib) = package.lib() else {
            return Err(IcmError::new(
                CheckId::ConfigLibMissing,
                format!(
                    "package `{}` has no library target (src/lib.rs)",
                    package.name
                ),
            )
            .evidence(Evidence::file(&package.manifest_path)));
        };
        match name {
            Some(name) if name != lib.name => Err(IcmError::new(
                CheckId::ConfigLibMissing,
                format!(
                    "{}: `{key}` is `{name}`, but package `{}`'s library is `{}`",
                    self.config.source.location_for(key),
                    package.name,
                    lib.name
                ),
            )
            .evidence(self.config.evidence(key))),
            _ => Ok(lib.name.clone()),
        }
    }

    /// The workspace's Cargo.lock.
    pub fn lock_path(&self) -> PathBuf {
        self.metadata.lock_path()
    }

    /// The parsed Cargo.lock, if it exists.
    pub fn lock(&self) -> Result<Option<cargo::Lock>> {
        cargo::Lock::read(&self.lock_path())
    }

    /// `target/icm/runs`.
    pub fn runs_dir(&self) -> PathBuf {
        output::rundir::runs_dir(&self.icm_dir)
    }

    /// `target/icm/latest/<platform>`.
    pub fn latest_dir(&self, platform: &str) -> PathBuf {
        self.icm_dir.join("latest").join(platform)
    }

    /// `target/icm/gen/<platform>/<profile>`.
    pub fn gen_dir(&self, platform: &str, profile: &str) -> PathBuf {
        self.icm_dir
            .join("gen")
            .join(platform)
            .join(cargo::profile_dir(profile))
    }

    /// `target/icm/build/<platform>/<profile>`.
    pub fn build_dir(&self, platform: &str, profile: &str) -> PathBuf {
        self.icm_dir
            .join("build")
            .join(platform)
            .join(cargo::profile_dir(profile))
    }

    /// `target/icm/sessions`.
    pub fn sessions_dir(&self) -> PathBuf {
        self.icm_dir.join("sessions")
    }

    /// `target/icm/locks`.
    pub fn locks_dir(&self) -> PathBuf {
        self.icm_dir.join("locks")
    }

    /// `target/icm/stamps`.
    pub fn stamps_dir(&self) -> PathBuf {
        self.icm_dir.join("stamps")
    }

    /// The result's `app` object.
    pub fn app_json(&self) -> Value {
        let app = self.app();
        json!({
            "id": app.id,
            "name": app.name,
            "version": self.package.version,
            "build": app.build,
        })
    }

    /// The result's `inputs` object.
    pub fn inputs_json(&self) -> Value {
        let git = |args: &[&str]| -> Option<String> {
            let outcome = process::run(
                &Cmd::tool("git")
                    .arg("-C")
                    .arg(self.dir())
                    .args(args)
                    .timeout(Duration::from_secs(10)),
                None,
                None,
            )
            .ok()?;
            outcome
                .success()
                .then(|| outcome.stdout_text().trim().to_string())
        };
        let rev = git(&["rev-parse", "HEAD"]);
        let dirty = rev.as_ref().and(
            git(&["status", "--porcelain", "--untracked-files=no"])
                .map(|status| !status.is_empty()),
        );
        let lock_sha = crate::hash::sha256_file(&self.lock_path()).ok();

        json!({
            "git_rev": rev,
            "dirty": dirty,
            "cargo_lock_sha256": lock_sha,
            "icm_toml_sha256": self.config.sha256(),
        })
    }
}
