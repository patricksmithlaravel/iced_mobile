//! Pinned external tools (design §16.2): the tools icm downloads itself,
//! listed in `cli/tools.toml` (embedded) with a version, a URL, a size and a
//! sha256 per host.
//!
//! - [`find`]: `ICM_TOOL_<NAME>`, else the copy installed in
//!   `<cache>/tools/<name>/<version>/` (it counts only with its marker, which
//!   records the sha256 it was checked against). Nothing on `PATH` is
//!   trusted: its version is unknown.
//! - [`require`]: [`find`], else, with `--yes` and without `--offline`,
//!   [`install`]; otherwise `env.tool_missing` (exit 4, `doctor-yes`) naming
//!   `icm doctor <platform> --fix --yes`.
//! - [`install`]: `curl` into a staging directory, then the size and sha256
//!   are checked before anything is unpacked (`env.tool_checksum` when they
//!   differ, and the download is deleted), then `tar` for archives, then the
//!   staging directory replaces the tool's directory and the marker is
//!   written last.
//!
//! `icm doctor <platform>` reports the tools its platform's releases need
//! (WARN while missing) and `--fix --yes` installs them; a release run with
//! `--yes` installs what it needs itself.

use crate::catalogue::{By, CheckId};
use crate::context::Ctx;
use crate::error::{Evidence, IcmError, Result};
use crate::process::Cmd;
use crate::tools::{Env, Found};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The embedded table.
pub const SOURCE: &str = include_str!("../tools.toml");

/// The table's schema version this icm reads.
const SCHEMA: u32 = 1;

/// The marker file in an installed tool's directory.
const MARKER: &str = ".icm-pinned.json";

/// How long one download may take.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How a download becomes the tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum Kind {
    /// The download is the tool.
    #[serde(rename = "file")]
    File,
    /// A gzip-compressed tar archive; `exe` is the path inside.
    #[serde(rename = "tar.gz")]
    TarGz,
}

/// One host's download.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Download {
    /// The URL (https).
    pub url: String,
    /// The sha256 of the download, lower-case hex.
    pub sha256: String,
    /// Its size in bytes.
    pub bytes: u64,
}

/// A pinned tool.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    /// The name (`ICM_TOOL_<NAME>` overrides it).
    pub name: String,
    /// The pinned version.
    pub version: String,
    /// What it is.
    pub description: String,
    /// The `icm doctor` platform that installs it.
    pub doctor: String,
    /// The release targets that run it.
    pub needed_by: Vec<String>,
    /// How the download becomes the tool.
    pub kind: Kind,
    /// The tool's path inside its directory.
    pub exe: String,
    /// Whether the file is made executable.
    #[serde(default)]
    pub executable: bool,
    /// Downloads by Rust host triple (or `any`).
    pub hosts: BTreeMap<String, Download>,
}

/// The table.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    schema: u32,
    /// The tools.
    #[serde(rename = "tool")]
    pub tools: Vec<Tool>,
}

/// Parses a tools table and checks its entries.
pub fn parse(text: &str) -> std::result::Result<Table, String> {
    let table: Table = toml::from_str(text).map_err(|error| error.to_string())?;
    if table.schema != SCHEMA {
        return Err(format!("schema {} (this icm reads {SCHEMA})", table.schema));
    }
    for (index, tool) in table.tools.iter().enumerate() {
        if table.tools[..index].iter().any(|t| t.name == tool.name) {
            return Err(format!("tool {} appears twice", tool.name));
        }
        if tool.hosts.is_empty() {
            return Err(format!("tool {} has no download", tool.name));
        }
        let safe = |text: &str| {
            !text.is_empty()
                && !text.contains("..")
                && !text.starts_with('/')
                && text
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c))
        };
        if !safe(&tool.name) || tool.name.contains('/') || !safe(&tool.version) {
            return Err(format!("tool {}: unsafe name or version", tool.name));
        }
        if !safe(&tool.exe) {
            return Err(format!(
                "tool {}: unsafe exe path `{}`",
                tool.name, tool.exe
            ));
        }
        for (host, download) in &tool.hosts {
            let hex = download.sha256.len() == 64
                && download
                    .sha256
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
            if !hex {
                return Err(format!(
                    "tool {} ({host}): sha256 must be 64 lower-case hex digits",
                    tool.name
                ));
            }
            if !download.url.starts_with("https://") && !download.url.starts_with("file://") {
                return Err(format!(
                    "tool {} ({host}): the URL must be https",
                    tool.name
                ));
            }
        }
    }
    Ok(table)
}

/// The table icm uses: `ICM_TOOLS_TOML` (a mirror, or icm's tests), else
/// the embedded one.
pub fn table() -> Result<Table> {
    let (text, origin) = match std::env::var_os("ICM_TOOLS_TOML").filter(|v| !v.is_empty()) {
        Some(path) => {
            let path = PathBuf::from(path);
            let text = std::fs::read_to_string(&path).map_err(|error| {
                IcmError::new(
                    CheckId::ConfigInvalid,
                    format!(
                        "ICM_TOOLS_TOML: cannot read {}: {error}",
                        crate::paths::display(&path)
                    ),
                )
            })?;
            (text, crate::paths::display(&path))
        }
        None => (SOURCE.to_string(), "icm's tools.toml".to_string()),
    };
    parse(&text)
        .map_err(|detail| IcmError::new(CheckId::ConfigInvalid, format!("{origin}: {detail}")))
}

/// A tool by name.
pub fn tool(name: &str) -> Result<Tool> {
    table()?
        .tools
        .into_iter()
        .find(|tool| tool.name == name)
        .ok_or_else(|| {
            IcmError::new(
                CheckId::InternalBug,
                format!("`{name}` is not a pinned tool in tools.toml"),
            )
        })
}

impl Tool {
    /// The download for this host, if the tool runs here.
    pub fn download(&self) -> Option<&Download> {
        self.hosts
            .get(crate::toolchain::host_triple())
            .or_else(|| self.hosts.get("any"))
    }

    /// `<cache>/tools/<name>/<version>`.
    pub fn dir(&self) -> PathBuf {
        crate::paths::tools_dir()
            .join(&self.name)
            .join(&self.version)
    }

    /// The installed tool's path.
    pub fn path(&self) -> PathBuf {
        self.dir().join(&self.exe)
    }

    /// Whether this version is installed and was checked against this
    /// table's sha256.
    pub fn installed(&self) -> bool {
        let Some(download) = self.download() else {
            return false;
        };
        let marker = std::fs::read_to_string(self.dir().join(MARKER))
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        marker.is_some_and(|marker| marker["sha256"] == download.sha256.as_str())
            && self.path().is_file()
    }

    /// The `ICM_TOOL_<NAME>` variable.
    pub fn override_var(&self) -> String {
        format!(
            "ICM_TOOL_{}",
            self.name.to_ascii_uppercase().replace(['-', '.'], "_")
        )
    }

    /// `icm release a|b` for messages.
    fn needed_by_text(&self) -> String {
        format!("`icm release {}`", self.needed_by.join("|"))
    }
}

/// The download command: curl, failing on HTTP errors, https only (a
/// `file://` URL from `ICM_TOOLS_TOML` excepted), with retries.
pub fn download_cmd(download: &Download, out: &Path) -> Cmd {
    let mut cmd = Cmd::tool("curl").args(["-fsSL", "--retry", "2", "--connect-timeout", "30"]);
    if download.url.starts_with("https://") {
        cmd = cmd.args(["--proto", "=https", "--tlsv1.2"]);
    }
    cmd.arg("-o")
        .arg(out)
        .arg(&download.url)
        .timeout(DOWNLOAD_TIMEOUT)
}

/// The error for a tool that is not installed: who installs it and how.
pub fn missing(tool: &Tool) -> IcmError {
    let mut error = IcmError::new(
        CheckId::EnvToolMissing,
        format!(
            "{} {} is not installed; {} needs it ({})",
            tool.name,
            tool.version,
            tool.needed_by_text(),
            tool.description
        ),
    )
    .by(By::DoctorYes);
    match tool.download() {
        Some(download) => {
            error = error
                .fix(
                    format!(
                        "Run `icm doctor {} --fix --yes` (downloads {} {}, {} bytes, sha256-checked), add --yes to the release, or point {} at a copy.",
                        tool.doctor,
                        tool.name,
                        tool.version,
                        download.bytes,
                        tool.override_var()
                    ),
                    &[],
                )
                .fix_commands([
                    format!("icm doctor {} --fix --yes", tool.doctor),
                    download_cmd(download, &tool.dir().join("<download>")).display(),
                ]);
        }
        None => {
            error = IcmError::new(
                CheckId::EnvUnsupportedHost,
                format!(
                    "{} {} has no pinned download for this host ({}); {} needs it",
                    tool.name,
                    tool.version,
                    crate::toolchain::host_triple(),
                    tool.needed_by_text()
                ),
            )
            .fix(
                format!(
                    "Run the release on a host the tool supports ({}), or point {} at a copy.",
                    tool.hosts.keys().cloned().collect::<Vec<_>>().join(", "),
                    tool.override_var()
                ),
                &[],
            );
        }
    }
    error
}

/// Finds a pinned tool: `ICM_TOOL_<NAME>`, else the installed copy.
pub fn find(env: &Env, name: &str) -> Result<Found> {
    let tool = tool(name)?;
    if let Some(path) = env.tool_override(&tool.name) {
        return Ok(Found {
            path,
            version: None,
            source: format!("${}", tool.override_var()),
        });
    }
    if tool.installed() {
        return Ok(Found {
            path: tool.path(),
            version: Some(tool.version.clone()),
            source: "icm cache (pinned)".to_string(),
        });
    }
    Err(missing(&tool))
}

/// [`find`], else, with `--yes` and online, [`install`].
pub fn require(ctx: &Ctx, name: &str) -> Result<Found> {
    match find(&ctx.env, name) {
        Ok(found) => Ok(found),
        Err(error) if error.id != CheckId::EnvToolMissing.id() => Err(error),
        Err(error) if !ctx.global.yes || ctx.global.offline => Err(error),
        Err(_) => install(ctx, &tool(name)?),
    }
}

fn io_error(what: &str, path: &Path, error: &std::io::Error) -> IcmError {
    IcmError::new(
        CheckId::InternalBug,
        format!("cannot {what} {}: {error}", crate::paths::display(path)),
    )
}

/// Downloads, checks and unpacks a tool into its directory. Callers check
/// consent (`--yes`) first; `--offline` refuses.
pub fn install(ctx: &Ctx, tool: &Tool) -> Result<Found> {
    let Some(download) = tool.download().cloned() else {
        return Err(missing(tool));
    };
    if ctx.global.offline {
        return Err(IcmError::new(
            CheckId::EnvToolMissing,
            format!(
                "{} {} is not installed and --offline forbids the download",
                tool.name, tool.version
            ),
        )
        .by(By::DoctorYes));
    }

    let parent = tool
        .dir()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(crate::paths::tools_dir);
    let staging = parent.join(format!(".partial-{}-{}", tool.version, std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let out = staging.join("out");
    std::fs::create_dir_all(&out).map_err(|e| io_error("create", &out, &e))?;
    let cleanup = || {
        let _ = std::fs::remove_dir_all(&staging);
    };

    let file_name = download
        .url
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("download");
    let file = staging.join(file_name);
    ctx.rep.progress(format!(
        "downloading {} {} ({} bytes) from {}",
        tool.name, tool.version, download.bytes, download.url
    ));
    let step = format!("pinned.{}.download", tool.name);
    let outcome = match ctx.step(&step, &download_cmd(&download, &file)) {
        Ok(outcome) => outcome,
        Err(error) => {
            cleanup();
            return Err(error);
        }
    };
    if !outcome.success() {
        let mut error = ctx.step_failure(&step, CheckId::EnvToolMissing, &outcome);
        error.detail = format!(
            "downloading {} {} from {} failed ({})",
            tool.name,
            tool.version,
            download.url,
            outcome.describe()
        );
        cleanup();
        return Err(error.by(By::DoctorYes).fix(
            "Check the network and rerun; the download is sha256-checked, so a retry is safe.",
            &[],
        ));
    }

    // The size and sha256, before anything is unpacked.
    let bytes = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
    let sha256 = crate::hash::sha256_file(&file).map_err(|e| io_error("read", &file, &e))?;
    if sha256 != download.sha256 || bytes != download.bytes {
        cleanup();
        return Err(IcmError::new(
            CheckId::EnvToolChecksum,
            format!(
                "{} {} from {}: expected {} bytes with sha256 {}, got {bytes} bytes with sha256 {sha256}; the download was deleted",
                tool.name, tool.version, download.url, download.bytes, download.sha256
            ),
        ));
    }
    ctx.rep.progress(format!(
        "{} {}: sha256 {} matches the pin",
        tool.name, tool.version, sha256
    ));

    match tool.kind {
        Kind::File => {
            let target = out.join(&tool.exe);
            if let Some(dir) = target.parent() {
                std::fs::create_dir_all(dir).map_err(|e| io_error("create", dir, &e))?;
            }
            std::fs::rename(&file, &target).map_err(|e| io_error("move", &file, &e))?;
        }
        Kind::TarGz => {
            let step = format!("pinned.{}.unpack", tool.name);
            let unpack = Cmd::tool("tar")
                .arg("-xzf")
                .arg(&file)
                .arg("-C")
                .arg(&out)
                .timeout(Duration::from_secs(600));
            let outcome = ctx.step(&step, &unpack)?;
            if !outcome.success() {
                cleanup();
                return Err(ctx.step_failure(&step, CheckId::ToolFailed, &outcome));
            }
            let _ = std::fs::remove_file(&file);
        }
    }

    let exe = out.join(&tool.exe);
    if !exe.is_file() {
        cleanup();
        return Err(IcmError::new(
            CheckId::InternalBug,
            format!(
                "{} {}: the download has no {}",
                tool.name, tool.version, tool.exe
            ),
        ));
    }
    if tool.executable {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| io_error("chmod", &exe, &e))?;
    }

    let dir = tool.dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::rename(&out, &dir).map_err(|e| io_error("install", &dir, &e))?;
    let marker = json!({
        "name": tool.name,
        "version": tool.version,
        "url": download.url,
        "sha256": download.sha256,
        "bytes": download.bytes,
        "installed": crate::time::Utc::now().rfc3339(),
        "by": crate::buildinfo::VERSION_LINE,
    });
    let text = serde_json::to_string_pretty(&marker).unwrap_or_default();
    crate::output::rundir::write_atomic(&dir.join(MARKER), text.as_bytes())
        .map_err(|e| io_error("write", &dir.join(MARKER), &e))?;
    cleanup();

    Ok(Found {
        path: tool.path(),
        version: Some(tool.version.clone()),
        source: "icm cache (pinned)".to_string(),
    })
}

/// The pinned tools `icm doctor <platform>` reports: those it installs that
/// have a download for this host.
pub fn for_doctor(platform: &str) -> Vec<Tool> {
    table()
        .map(|table| {
            table
                .tools
                .into_iter()
                .filter(|tool| tool.doctor == platform && tool.download().is_some())
                .collect()
        })
        .unwrap_or_default()
}

/// Every pinned tool for `icm print tools`.
pub fn report(env: &Env) -> Vec<(String, std::result::Result<Value, IcmError>)> {
    let table = match table() {
        Ok(table) => table,
        Err(error) => return vec![("tools.toml".to_string(), Err(error))],
    };
    table
        .tools
        .iter()
        .map(|tool| {
            let found = find(env, &tool.name).map(|found| {
                json!({
                    "path": found.path,
                    "version": found.version.unwrap_or_else(|| tool.version.clone()),
                    "source": found.source,
                    "pinned": tool.version,
                })
            });
            (tool.name.clone(), found)
        })
        .collect()
}

/// Evidence pointing at an installed tool's marker.
pub fn evidence(tool: &Tool) -> Evidence {
    Evidence::file(tool.dir().join(MARKER))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_table_is_valid() {
        let table = parse(SOURCE).unwrap();
        let names: Vec<&str> = table.tools.iter().map(|t| t.name.as_str()).collect();
        for name in ["bundletool", "wasm-opt", "appimagetool", "appimage-runtime"] {
            assert!(names.contains(&name), "{name} missing from {names:?}");
        }
        for tool in &table.tools {
            assert!(
                ["android", "web", "desktop"].contains(&tool.doctor.as_str()),
                "{}",
                tool.name
            );
            for target in &tool.needed_by {
                assert!(
                    ["ios", "android", "web", "macos", "windows", "linux"]
                        .contains(&target.as_str()),
                    "{}: {target}",
                    tool.name
                );
            }
            for download in tool.hosts.values() {
                assert!(download.url.starts_with("https://github.com/"));
                assert!(download.bytes > 0);
            }
        }
        // wasm-opt runs on every host icm supports.
        let wasm_opt = table.tools.iter().find(|t| t.name == "wasm-opt").unwrap();
        for host in [
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
        ] {
            assert!(wasm_opt.hosts.contains_key(host), "{host}");
        }
    }

    #[test]
    fn bad_tables_are_refused() {
        let good = parse(SOURCE).unwrap();
        assert!(!good.tools.is_empty());
        assert!(parse(&SOURCE.replace("schema = 1", "schema = 9")).is_err());
        let short = SOURCE.replacen(
            "a099cfa1543f55593bc2ed16a70a7c67fe54b1747bb7301f37fdfd6d91028e29",
            "a099",
            1,
        );
        assert!(parse(&short).unwrap_err().contains("64 lower-case hex"));
        let http = SOURCE.replacen("https://github.com/google", "http://github.com/google", 1);
        assert!(parse(&http).unwrap_err().contains("https"));
        let escape = SOURCE.replacen(
            "exe = \"bundletool-all-1.18.3.jar\"",
            "exe = \"../../evil\"",
            1,
        );
        assert!(parse(&escape).unwrap_err().contains("unsafe"));
    }

    #[test]
    fn missing_tools_name_the_doctor_fix() {
        let tool = parse(SOURCE)
            .unwrap()
            .tools
            .into_iter()
            .find(|t| t.name == "bundletool")
            .unwrap();
        let error = missing(&tool);
        assert_eq!(error.id, "env.tool_missing");
        assert_eq!(error.fix.by, By::DoctorYes);
        assert_eq!(error.fix.commands[0], "icm doctor android --fix --yes");
        assert!(
            error.fix.commands[1].starts_with("curl -fsSL"),
            "{:?}",
            error.fix
        );
        assert!(error.detail.contains("`icm release android` needs it"));
        assert_eq!(tool.override_var(), "ICM_TOOL_BUNDLETOOL");
    }
}
