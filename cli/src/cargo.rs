//! Cargo: `cargo metadata` (own serde structs), `Cargo.lock`, build
//! invocations with `--message-format=json` (rustc diagnostics arrive as
//! JSON with their rendered text; `json-render-diagnostics` would print
//! them to stderr instead and leave no `compiler-message`), and
//! deployment-target stamping (Appendix C item 6).

use crate::catalogue::CheckId;
use crate::error::{Diagnostic, Evidence, IcmError};
use crate::process::{self, Cmd};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

// ---- cargo metadata ----------------------------------------------------------------

/// The parts of `cargo metadata --format-version 1` icm reads.
#[derive(Clone, Debug, Deserialize)]
pub struct Metadata {
    /// Packages (with `--no-deps`: the workspace members).
    pub packages: Vec<Package>,
    /// Workspace member ids.
    #[serde(default)]
    pub workspace_members: Vec<String>,
    /// The workspace root.
    pub workspace_root: PathBuf,
    /// Cargo's target directory.
    pub target_directory: PathBuf,
}

/// A package.
#[derive(Clone, Debug, Deserialize)]
pub struct Package {
    /// Its name.
    pub name: String,
    /// Its version.
    pub version: String,
    /// Its package id.
    pub id: String,
    /// Its Cargo.toml.
    pub manifest_path: PathBuf,
    /// Its targets.
    #[serde(default)]
    pub targets: Vec<Target>,
    /// Its declared dependencies.
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    /// Its features.
    #[serde(default)]
    pub features: BTreeMap<String, Vec<String>>,
    /// Its source (`None` for path packages).
    #[serde(default)]
    pub source: Option<String>,
}

/// A build target of a package.
#[derive(Clone, Debug, Deserialize)]
pub struct Target {
    /// The target name.
    pub name: String,
    /// `lib`, `bin`, `test`, `example`, `cdylib`, ...
    #[serde(default)]
    pub kind: Vec<String>,
    /// Crate types.
    #[serde(default)]
    pub crate_types: Vec<String>,
    /// The root source file.
    pub src_path: PathBuf,
    /// Features the target requires.
    #[serde(default)]
    pub required_features: Vec<String>,
}

impl Target {
    /// Whether it is a library target.
    pub fn is_lib(&self) -> bool {
        self.kind.iter().any(|kind| {
            matches!(
                kind.as_str(),
                "lib" | "rlib" | "dylib" | "cdylib" | "staticlib"
            )
        })
    }

    /// Whether it is a binary target.
    pub fn is_bin(&self) -> bool {
        self.kind.iter().any(|kind| kind == "bin")
    }
}

/// A declared dependency.
#[derive(Clone, Debug, Deserialize)]
pub struct Dependency {
    /// The package name.
    pub name: String,
    /// Its source (registry or git URL), `None` for path dependencies.
    #[serde(default)]
    pub source: Option<String>,
    /// The version requirement.
    #[serde(default)]
    pub req: String,
    /// `dev`, `build`, or `None` for normal.
    #[serde(default)]
    pub kind: Option<String>,
    /// The `package = ...` rename.
    #[serde(default)]
    pub rename: Option<String>,
    /// Whether it is optional.
    #[serde(default)]
    pub optional: bool,
    /// Whether default features are on.
    #[serde(default = "yes")]
    pub uses_default_features: bool,
    /// Enabled features.
    #[serde(default)]
    pub features: Vec<String>,
    /// The `cfg(...)` or triple it is limited to.
    #[serde(default)]
    pub target: Option<String>,
    /// Its path, for path dependencies.
    #[serde(default)]
    pub path: Option<PathBuf>,
}

fn yes() -> bool {
    true
}

impl Metadata {
    /// The workspace members.
    pub fn members(&self) -> impl Iterator<Item = &Package> {
        self.packages
            .iter()
            .filter(|package| self.workspace_members.contains(&package.id))
    }

    /// A member by name.
    pub fn member(&self, name: &str) -> Option<&Package> {
        self.members().find(|package| package.name == name)
    }

    /// The member whose Cargo.toml is in `dir`.
    pub fn member_in(&self, dir: &Path) -> Option<&Package> {
        let dir = canonical(dir);
        self.members().find(|package| {
            package
                .manifest_path
                .parent()
                .is_some_and(|parent| canonical(parent) == dir)
        })
    }

    /// The workspace's Cargo.lock.
    pub fn lock_path(&self) -> PathBuf {
        self.workspace_root.join("Cargo.lock")
    }
}

impl Package {
    /// Its binary targets.
    pub fn bins(&self) -> impl Iterator<Item = &Target> {
        self.targets.iter().filter(|target| target.is_bin())
    }

    /// Its library target.
    pub fn lib(&self) -> Option<&Target> {
        self.targets.iter().find(|target| target.is_lib())
    }

    /// Its directory.
    pub fn dir(&self) -> &Path {
        self.manifest_path.parent().unwrap_or(Path::new("."))
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Runs `cargo metadata --no-deps` for the package or workspace in `dir`
/// (its `Cargo.toml`, or the nearest one above it).
pub fn metadata(dir: &Path, offline: bool) -> Result<Metadata, IcmError> {
    let manifest = dir.join("Cargo.toml");
    let mut cmd = Cmd::tool("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .cwd(dir)
        .timeout(Duration::from_secs(120));
    if manifest.is_file() {
        cmd = cmd.arg("--manifest-path").arg(&manifest);
    }
    if offline {
        cmd = cmd.arg("--offline");
    }

    let outcome = process::run(&cmd, None, None).map_err(|error| {
        IcmError::new(
            CheckId::EnvToolMissing,
            format!("cannot run cargo: {error}"),
        )
    })?;

    if !outcome.success() {
        let stderr = outcome.stderr_text();
        if let Some(toolchain) = crate::toolchain::missing_toolchain(&stderr) {
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
        return Err(IcmError::new(
            CheckId::ConfigInvalid,
            format!("cargo metadata failed: {}", outcome.stderr_tail(8)),
        )
        .evidence(Evidence::file(&manifest)));
    }

    serde_json::from_slice(&outcome.stdout).map_err(|error| {
        IcmError::new(
            CheckId::InternalBug,
            format!("cannot read cargo metadata output: {error}"),
        )
    })
}

// ---- Cargo.lock ----------------------------------------------------------------------

/// A parsed Cargo.lock.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Lock {
    /// The lockfile format version.
    #[serde(default)]
    pub version: Option<u32>,
    /// The locked packages.
    #[serde(default, rename = "package")]
    pub packages: Vec<LockPackage>,
    /// The file.
    #[serde(skip)]
    pub path: PathBuf,
    /// Its text (for line numbers).
    #[serde(skip)]
    pub text: String,
}

/// One locked package.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct LockPackage {
    /// Its name.
    pub name: String,
    /// Its version.
    pub version: String,
    /// Its source, `None` for path packages.
    #[serde(default)]
    pub source: Option<String>,
    /// The registry checksum.
    #[serde(default)]
    pub checksum: Option<String>,
    /// Its dependencies (`name` or `name version`).
    #[serde(default)]
    pub dependencies: Vec<String>,
}

impl Lock {
    /// Parses lockfile text.
    pub fn parse(path: &Path, text: &str) -> Result<Lock, IcmError> {
        let mut lock: Lock = toml::from_str(text).map_err(|error| {
            let source = crate::config::source::Source::new(path, text.to_string());
            let mut problem = crate::config::toml_error(&source, &error);
            problem.id = std::borrow::Cow::Borrowed(CheckId::ConfigInvalid.id());
            problem.exit = crate::exit::Exit::Config;
            problem
        })?;
        lock.path = path.to_path_buf();
        lock.text = text.to_string();
        Ok(lock)
    }

    /// Reads a lockfile; `None` when it does not exist yet.
    pub fn read(path: &Path) -> Result<Option<Lock>, IcmError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Lock::parse(path, &text).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(IcmError::new(
                CheckId::ConfigInvalid,
                format!("cannot read {}: {error}", crate::paths::display(path)),
            )),
        }
    }

    /// The packages with a name.
    pub fn named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a LockPackage> + 'a {
        self.packages
            .iter()
            .filter(move |package| package.name == name)
    }

    /// The first locked version of a package.
    pub fn version_of(&self, name: &str) -> Option<&str> {
        self.packages
            .iter()
            .find(|package| package.name == name)
            .map(|package| package.version.as_str())
    }

    /// The 1-based line of a package's `name = ` entry.
    pub fn line_of(&self, package: &LockPackage) -> Option<u32> {
        let name_line = format!("name = \"{}\"", package.name);
        let version_line = format!("version = \"{}\"", package.version);
        let lines: Vec<&str> = self.text.lines().collect();
        lines
            .iter()
            .enumerate()
            .find(|(index, line)| {
                line.trim() == name_line
                    && lines
                        .get(index + 1)
                        .is_some_and(|next| next.trim() == version_line)
            })
            .map(|(index, _)| index as u32 + 1)
    }

    /// Evidence pointing at a package's entry.
    pub fn evidence(&self, package: &LockPackage) -> Evidence {
        let excerpt = format!(
            "{} {} {}",
            package.name,
            package.version,
            package.source.as_deref().unwrap_or("(path)")
        );
        match self.line_of(package) {
            Some(line) => Evidence::line(&self.path, line, excerpt),
            None => Evidence::file(&self.path).with_excerpt(excerpt),
        }
    }

    /// The sha256 of the file.
    pub fn sha256(&self) -> String {
        crate::hash::sha256_hex(self.text.as_bytes())
    }
}

/// Where a locked package comes from.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    /// A path dependency (no `source` in the lock).
    Path,
    /// A registry.
    Registry {
        /// The index URL.
        url: String,
    },
    /// A git repository.
    Git {
        /// The repository URL.
        url: String,
        /// How it was pinned.
        reference: GitRef,
        /// The locked commit.
        commit: String,
    },
    /// Anything else.
    Other(String),
}

/// How a git dependency is pinned.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GitRef {
    /// `tag = "..."`.
    Tag(String),
    /// `rev = "..."`.
    Rev(String),
    /// `branch = "..."`.
    Branch(String),
    /// No tag, rev or branch: the default branch.
    DefaultBranch,
}

impl Source {
    /// Parses a lockfile `source` value.
    pub fn parse(source: Option<&str>) -> Source {
        let Some(source) = source else {
            return Source::Path;
        };

        if let Some(rest) = source.strip_prefix("git+") {
            let (base, commit) = rest.split_once('#').unwrap_or((rest, ""));
            let (url, query) = base.split_once('?').unwrap_or((base, ""));
            let mut reference = GitRef::DefaultBranch;
            for pair in query.split('&').filter(|p| !p.is_empty()) {
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                reference = match key {
                    "tag" => GitRef::Tag(value.to_string()),
                    "rev" => GitRef::Rev(value.to_string()),
                    "branch" => GitRef::Branch(value.to_string()),
                    _ => reference,
                };
            }
            return Source::Git {
                url: url.to_string(),
                reference,
                commit: commit.to_string(),
            };
        }

        for prefix in ["registry+", "sparse+"] {
            if let Some(url) = source.strip_prefix(prefix) {
                return Source::Registry {
                    url: url.to_string(),
                };
            }
        }

        Source::Other(source.to_string())
    }

    /// Whether it is crates.io.
    pub fn is_crates_io(&self) -> bool {
        matches!(self, Source::Registry { url }
            if url.contains("github.com/rust-lang/crates.io-index") || url.contains("index.crates.io"))
    }

    /// Whether it is upstream iced's repository.
    pub fn is_upstream_iced(&self) -> bool {
        matches!(self, Source::Git { url, .. }
            if crate::gitinfo::normalize_git_url(url).eq_ignore_ascii_case("https://github.com/iced-rs/iced"))
    }
}

// ---- builds --------------------------------------------------------------------------

/// Which targets of the package a cargo invocation builds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Select {
    /// `--bin <name>`.
    Bin(String),
    /// `--lib`.
    Lib,
    /// `--test <name>`.
    Test(String),
    /// Every target (no selection flag).
    All,
}

/// A cargo build-like invocation (`build`, `rustc`, `check`, `clippy`,
/// `test`), always with `--message-format=json`.
#[derive(Clone, Debug)]
pub struct Invocation {
    /// The cargo subcommand.
    pub subcommand: String,
    /// The package's Cargo.toml.
    pub manifest: PathBuf,
    /// The package.
    pub package: String,
    /// The targets.
    pub select: Select,
    /// The target triple; `None` builds for the host.
    pub triple: Option<String>,
    /// The profile: `dev`, `release` or a custom one.
    pub profile: String,
    /// Features to enable.
    pub features: Vec<String>,
    /// `--offline`.
    pub offline: bool,
    /// `--locked`.
    pub locked: bool,
    /// A separate `--target-dir`.
    pub target_dir: Option<PathBuf>,
    /// Arguments after `--`.
    pub trailing: Vec<String>,
    /// `--config <KEY=VALUE>` settings, e.g. icm's profiles
    /// ([`profile_config`]).
    pub config: Vec<String>,
}

impl Invocation {
    /// A new invocation with the dev profile.
    pub fn new(subcommand: &str, manifest: &Path, package: &str) -> Invocation {
        Invocation {
            subcommand: subcommand.to_string(),
            manifest: manifest.to_path_buf(),
            package: package.to_string(),
            select: Select::All,
            triple: None,
            profile: "dev".to_string(),
            features: Vec::new(),
            offline: false,
            locked: false,
            target_dir: None,
            trailing: Vec::new(),
            config: Vec::new(),
        }
    }

    /// The argv after `cargo`.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![self.subcommand.clone()];
        for setting in &self.config {
            args.extend(["--config".to_string(), setting.clone()]);
        }
        args.extend([
            "--manifest-path".to_string(),
            self.manifest.display().to_string(),
            "-p".to_string(),
            self.package.clone(),
        ]);
        match &self.select {
            Select::Bin(name) => args.extend(["--bin".to_string(), name.clone()]),
            Select::Lib => args.push("--lib".to_string()),
            Select::Test(name) => args.extend(["--test".to_string(), name.clone()]),
            Select::All => {}
        }
        if let Some(triple) = &self.triple {
            args.extend(["--target".to_string(), triple.clone()]);
        }
        match self.profile.as_str() {
            "dev" | "debug" => {}
            "release" => args.push("--release".to_string()),
            other => args.extend(["--profile".to_string(), other.to_string()]),
        }
        if !self.features.is_empty() {
            args.extend(["--features".to_string(), self.features.join(",")]);
        }
        if self.offline {
            args.push("--offline".to_string());
        }
        if self.locked {
            args.push("--locked".to_string());
        }
        if let Some(dir) = &self.target_dir {
            args.extend(["--target-dir".to_string(), dir.display().to_string()]);
        }
        args.push("--message-format=json".to_string());
        if !self.trailing.is_empty() {
            args.push("--".to_string());
            args.extend(self.trailing.iter().cloned());
        }
        args
    }

    /// The command (honours `ICM_TOOL_CARGO`), run from the package dir.
    pub fn cmd(&self) -> Cmd {
        let dir = self
            .manifest
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();
        Cmd::tool("cargo").args(self.args()).cwd(dir)
    }
}

/// icm's settings for a profile, passed with `--config` because Cargo
/// ignores `[profile.*]` in a workspace member (Appendix C item 4):
/// dependencies at `opt-level = 2` in dev builds, so debug builds stay
/// usable, and thin LTO in release builds.
pub fn profile_config(profile: &str) -> Vec<String> {
    match profile {
        "dev" | "debug" | "test" => vec![r#"profile.dev.package."*".opt-level=2"#.to_string()],
        "release" => vec![r#"profile.release.lto="thin""#.to_string()],
        _ => Vec::new(),
    }
}

/// A `compiler-artifact` message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    /// The package id.
    pub package_id: String,
    /// The target name.
    pub target_name: String,
    /// The target kinds.
    pub target_kind: Vec<String>,
    /// The produced files.
    pub filenames: Vec<PathBuf>,
    /// The executable, for binaries.
    pub executable: Option<PathBuf>,
    /// Whether it was up to date.
    pub fresh: bool,
}

/// A cargo JSON message icm cares about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// A built artifact.
    Artifact(Artifact),
    /// A compiler diagnostic.
    Diagnostic(Diagnostic),
    /// The end of the build.
    BuildFinished {
        /// Whether it succeeded.
        success: bool,
    },
}

/// Parses one line of `--message-format=json` output.
pub fn parse_message(line: &str) -> Option<Message> {
    let line = line.trim();
    if !line.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let str_of = |v: &serde_json::Value| v.as_str().map(str::to_string);

    match value.get("reason")?.as_str()? {
        "compiler-artifact" => {
            let target = value.get("target")?;
            Some(Message::Artifact(Artifact {
                package_id: str_of(value.get("package_id")?)?,
                target_name: str_of(target.get("name")?)?,
                target_kind: target
                    .get("kind")
                    .and_then(|k| k.as_array())
                    .map(|kinds| kinds.iter().filter_map(str_of).collect())
                    .unwrap_or_default(),
                filenames: value
                    .get("filenames")
                    .and_then(|f| f.as_array())
                    .map(|files| files.iter().filter_map(str_of).map(PathBuf::from).collect())
                    .unwrap_or_default(),
                executable: value.get("executable").and_then(str_of).map(PathBuf::from),
                fresh: value
                    .get("fresh")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            }))
        }
        "compiler-message" => {
            let message = value.get("message")?;
            let target = value
                .get("target")
                .and_then(|t| t.get("name"))
                .and_then(str_of)
                .unwrap_or_default();
            let primary = message
                .get("spans")
                .and_then(|s| s.as_array())
                .and_then(|spans| {
                    spans
                        .iter()
                        .find(|span| {
                            span.get("is_primary").and_then(serde_json::Value::as_bool)
                                == Some(true)
                        })
                        .or_else(|| spans.first())
                });
            Some(Message::Diagnostic(Diagnostic {
                level: message.get("level").and_then(str_of).unwrap_or_default(),
                code: message
                    .get("code")
                    .and_then(|c| c.get("code"))
                    .and_then(str_of),
                message: message.get("message").and_then(str_of).unwrap_or_default(),
                rendered: message.get("rendered").and_then(str_of).unwrap_or_default(),
                file: primary.and_then(|s| s.get("file_name")).and_then(str_of),
                line: primary
                    .and_then(|s| s.get("line_start"))
                    .and_then(serde_json::Value::as_u64)
                    .map(|l| l as u32),
                col: primary
                    .and_then(|s| s.get("column_start"))
                    .and_then(serde_json::Value::as_u64)
                    .map(|c| c as u32),
                targets: if target.is_empty() {
                    Vec::new()
                } else {
                    vec![target]
                },
            }))
        }
        "build-finished" => Some(Message::BuildFinished {
            success: value.get("success")?.as_bool()?,
        }),
        _ => None,
    }
}

/// Whether failed cargo output points at the linker rather than rustc.
pub fn is_link_failure(stderr: &str, diagnostics: &[Diagnostic]) -> bool {
    let mentions = |text: &str| {
        text.contains("linking with")
            || text.contains("linker `")
            || text.contains("ld: ")
            || text.contains("undefined reference")
            || text.contains("Undefined symbols")
    };
    mentions(stderr)
        || diagnostics
            .iter()
            .any(|d| d.level == "error" && (mentions(&d.message) || mentions(&d.rendered)))
}

// ---- deployment targets ------------------------------------------------------------------

/// The deployment-target variable for an Apple triple.
pub fn deployment_var(triple: &str) -> Option<&'static str> {
    if triple.contains("-apple-ios") {
        Some("IPHONEOS_DEPLOYMENT_TARGET")
    } else if triple.ends_with("-apple-darwin") {
        Some("MACOSX_DEPLOYMENT_TARGET")
    } else {
        None
    }
}

/// Cargo's output directory name for a profile.
pub fn profile_dir(profile: &str) -> &str {
    match profile {
        "dev" | "debug" | "test" => "debug",
        "release" | "bench" => "release",
        other => other,
    }
}

/// Where cargo puts a triple's and profile's artifacts.
pub fn artifacts_dir(target_dir: &Path, triple: Option<&str>, profile: &str) -> PathBuf {
    let mut dir = target_dir.to_path_buf();
    if let Some(triple) = triple {
        dir.push(triple);
    }
    dir.push(profile_dir(profile));
    dir
}

/// What to do before building with a deployment target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StampAction {
    /// The last build used the same value.
    UpToDate,
    /// Nothing was built yet for this triple and profile.
    Fresh,
    /// Relink: `cargo clean -p <pkg> --target <triple>` first.
    Clean {
        /// Why.
        reason: String,
    },
}

/// The deployment target a triple and profile were last built with.
///
/// Cargo does not rebuild when `MACOSX_DEPLOYMENT_TARGET` or
/// `IPHONEOS_DEPLOYMENT_TARGET` changes (Appendix C item 6), so icm stamps
/// the value per triple and profile and cleans the app package when it
/// changes, or when artifacts exist that icm did not stamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeploymentStamp {
    /// The stamp file.
    pub path: PathBuf,
    /// The `VAR=value` it records.
    pub line: String,
}

impl DeploymentStamp {
    /// The stamp for a triple (`None`: the host) and profile, under the icm dir.
    pub fn new(
        icm_dir: &Path,
        triple: Option<&str>,
        profile: &str,
        var: &str,
        value: &str,
    ) -> Self {
        DeploymentStamp {
            path: icm_dir.join("stamps").join(format!(
                "deployment-{}-{}.txt",
                triple.unwrap_or("host"),
                profile_dir(profile)
            )),
            line: format!("{var}={value}"),
        }
    }

    /// Compares with the last build. `artifacts` is cargo's output
    /// directory for the triple and profile.
    pub fn action(&self, artifacts: &Path) -> StampAction {
        match std::fs::read_to_string(&self.path) {
            Ok(previous) if previous.trim() == self.line => StampAction::UpToDate,
            Ok(previous) => StampAction::Clean {
                reason: format!(
                    "the last build used {}; this one uses {}",
                    previous.trim(),
                    self.line
                ),
            },
            Err(_) if artifacts.exists() => StampAction::Clean {
                reason: format!(
                    "{} holds a build icm did not stamp; relinking with {}",
                    artifacts.display(),
                    self.line
                ),
            },
            Err(_) => StampAction::Fresh,
        }
    }

    /// Records the value after a successful build.
    pub fn write(&self) -> std::io::Result<()> {
        crate::output::rundir::write_atomic(&self.path, format!("{}\n", self.line).as_bytes())
    }
}

/// `cargo clean -p <pkg> [--target <t>] [--release|--profile <p>]`: removes
/// only the app package's artifacts, so only the app relinks.
pub fn clean_cmd(manifest: &Path, package: &str, triple: Option<&str>, profile: &str) -> Cmd {
    let mut args = vec![
        "clean".to_string(),
        "--manifest-path".to_string(),
        manifest.display().to_string(),
        "-p".to_string(),
        package.to_string(),
    ];
    if let Some(triple) = triple {
        args.extend(["--target".to_string(), triple.to_string()]);
    }
    match profile {
        "dev" | "debug" => {}
        "release" => args.push("--release".to_string()),
        other => args.extend(["--profile".to_string(), other.to_string()]),
    }
    let dir = manifest.parent().unwrap_or(Path::new(".")).to_path_buf();
    Cmd::tool("cargo").args(args).cwd(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = r#"# This file is automatically @generated by Cargo.
version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = [
 "iced",
]

[[package]]
name = "iced"
version = "0.14.1"
source = "git+https://github.com/patricksmithlaravel/iced_mobile?tag=v0.14.1-mobile.1#e8bc51b5f0e3a1c2d4b5a6978877665544332211"

[[package]]
name = "winit"
version = "0.30.13"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "abc"
"#;

    #[test]
    fn locks_parse_with_lines() {
        let lock = Lock::parse(Path::new("/p/Cargo.lock"), LOCK).unwrap();
        assert_eq!(lock.version, Some(4));
        assert_eq!(lock.packages.len(), 3);
        let iced = lock.named("iced").next().unwrap();
        assert_eq!(lock.line_of(iced), Some(12));
        assert_eq!(lock.version_of("winit"), Some("0.30.13"));
        let evidence = lock.evidence(iced);
        assert_eq!(evidence.line, Some(12));
        assert!(evidence.excerpt.unwrap().starts_with("iced 0.14.1 git+"));
    }

    #[test]
    fn sources_parse() {
        assert_eq!(Source::parse(None), Source::Path);
        assert!(
            Source::parse(Some(
                "registry+https://github.com/rust-lang/crates.io-index"
            ))
            .is_crates_io()
        );
        assert!(Source::parse(Some("sparse+https://index.crates.io/")).is_crates_io());
        assert_eq!(
            Source::parse(Some(
                "git+https://github.com/a/b?rev=71f00e8#71f00e8475815532b25e7e07083abb73bd845cde"
            )),
            Source::Git {
                url: "https://github.com/a/b".into(),
                reference: GitRef::Rev("71f00e8".into()),
                commit: "71f00e8475815532b25e7e07083abb73bd845cde".into(),
            }
        );
        assert!(matches!(
            Source::parse(Some("git+https://github.com/a/b?branch=main#abc")),
            Source::Git {
                reference: GitRef::Branch(_),
                ..
            }
        ));
        assert!(matches!(
            Source::parse(Some("git+https://github.com/a/b#abc")),
            Source::Git {
                reference: GitRef::DefaultBranch,
                ..
            }
        ));
        assert!(
            Source::parse(Some(
                "git+https://github.com/iced-rs/iced.git?tag=0.13.1#abc"
            ))
            .is_upstream_iced()
        );
    }

    #[test]
    fn invocations_build_argv() {
        let mut build = Invocation::new("rustc", Path::new("/p/Cargo.toml"), "app");
        build.select = Select::Lib;
        build.triple = Some("aarch64-linux-android".into());
        build.profile = "release".into();
        build.trailing = vec!["--crate-type".into(), "cdylib".into()];
        assert_eq!(
            build.args().join(" "),
            "rustc --manifest-path /p/Cargo.toml -p app --lib --target aarch64-linux-android --release \
             --message-format=json -- --crate-type cdylib"
        );

        let mut desktop = Invocation::new("build", Path::new("/p/Cargo.toml"), "app");
        desktop.config = profile_config("dev");
        assert_eq!(
            desktop.args().join(" "),
            "build --config profile.dev.package.\"*\".opt-level=2 --manifest-path /p/Cargo.toml \
             -p app --message-format=json"
        );
        assert_eq!(
            profile_config("release"),
            vec!["profile.release.lto=\"thin\"".to_string()]
        );

        let mut web = Invocation::new("build", Path::new("/p/Cargo.toml"), "app");
        web.select = Select::Bin("app".into());
        web.profile = "icm-web".into();
        web.offline = true;
        assert!(
            web.args()
                .join(" ")
                .contains("--bin app --profile icm-web --offline")
        );
    }

    #[test]
    fn messages_parse() {
        let artifact = r#"{"reason":"compiler-artifact","package_id":"path+file:///p#app@0.1.0","target":{"name":"app","kind":["bin"]},"filenames":["/p/target/debug/app"],"executable":"/p/target/debug/app","fresh":false}"#;
        match parse_message(artifact) {
            Some(Message::Artifact(a)) => {
                assert_eq!(a.executable, Some(PathBuf::from("/p/target/debug/app")));
                assert_eq!(a.target_kind, vec!["bin"]);
            }
            other => panic!("{other:?}"),
        }

        let diagnostic = r#"{"reason":"compiler-message","package_id":"x","target":{"name":"app","kind":["lib"]},"message":{"rendered":"error[E0308]: mismatched types\n","level":"error","message":"mismatched types","code":{"code":"E0308","explanation":null},"spans":[{"file_name":"src/other.rs","line_start":1,"column_start":1,"is_primary":false},{"file_name":"src/lib.rs","line_start":41,"column_start":9,"is_primary":true}]}}"#;
        match parse_message(diagnostic) {
            Some(Message::Diagnostic(d)) => {
                assert_eq!(d.level, "error");
                assert_eq!(d.code.as_deref(), Some("E0308"));
                assert_eq!(d.file.as_deref(), Some("src/lib.rs"));
                assert_eq!((d.line, d.col), (Some(41), Some(9)));
                assert_eq!(d.targets, vec!["app"]);
            }
            other => panic!("{other:?}"),
        }

        assert_eq!(
            parse_message(r#"{"reason":"build-finished","success":false}"#),
            Some(Message::BuildFinished { success: false })
        );
        assert_eq!(parse_message("   Compiling app v0.1.0"), None);
    }

    #[test]
    fn link_failures_are_recognised() {
        assert!(is_link_failure(
            "error: linking with `cc` failed: exit status: 1",
            &[]
        ));
        assert!(!is_link_failure("error[E0308]: mismatched types", &[]));
    }

    #[test]
    fn deployment_stamps() {
        let tmp = tempfile::tempdir().unwrap();
        let icm = tmp.path().join("target/icm");
        let artifacts = artifacts_dir(
            &tmp.path().join("target"),
            Some("aarch64-apple-ios-sim"),
            "dev",
        );
        assert!(artifacts.ends_with("target/aarch64-apple-ios-sim/debug"));
        assert_eq!(
            deployment_var("aarch64-apple-ios-sim"),
            Some("IPHONEOS_DEPLOYMENT_TARGET")
        );
        assert_eq!(
            deployment_var("aarch64-apple-darwin"),
            Some("MACOSX_DEPLOYMENT_TARGET")
        );
        assert_eq!(deployment_var("aarch64-linux-android"), None);

        let stamp = DeploymentStamp::new(
            &icm,
            Some("aarch64-apple-ios-sim"),
            "dev",
            "IPHONEOS_DEPLOYMENT_TARGET",
            "16.0",
        );
        assert_eq!(stamp.action(&artifacts), StampAction::Fresh);

        // A raw cargo build left artifacts icm did not stamp.
        std::fs::create_dir_all(&artifacts).unwrap();
        assert!(matches!(
            stamp.action(&artifacts),
            StampAction::Clean { .. }
        ));

        stamp.write().unwrap();
        assert_eq!(stamp.action(&artifacts), StampAction::UpToDate);

        let changed = DeploymentStamp::new(
            &icm,
            Some("aarch64-apple-ios-sim"),
            "dev",
            "IPHONEOS_DEPLOYMENT_TARGET",
            "17.0",
        );
        match changed.action(&artifacts) {
            StampAction::Clean { reason } => assert!(
                reason.contains("16.0") && reason.contains("17.0"),
                "{reason}"
            ),
            other => panic!("{other:?}"),
        }

        let clean = clean_cmd(
            Path::new("/p/Cargo.toml"),
            "app",
            Some("aarch64-apple-ios-sim"),
            "dev",
        );
        assert_eq!(
            clean.display(),
            "cargo clean --manifest-path /p/Cargo.toml -p app --target aarch64-apple-ios-sim"
        );
    }
}
