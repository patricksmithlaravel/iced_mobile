//! The upload ledger (design §11, Appendix A item 17): `.icm/ledger.toml`
//! in the project, one `[[upload]]` per build the owner uploaded. Only
//! `icm ledger mark-uploaded` writes it (the last line of `upload.sh`);
//! `icm release` refuses an `[app] build` that is not above the highest
//! one recorded for its target (`version.build_not_increased`). The file
//! belongs in git.

use super::dist;
use super::manifest::Manifest;
use crate::catalogue::CheckId;
use crate::cli::{LedgerAction, LedgerArgs, ReleaseTarget};
use crate::context::Ctx;
use crate::error::{Check, Evidence, IcmError, Result};
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};

/// The ledger's path in the project.
pub const FILE: &str = ".icm/ledger.toml";

/// One recorded upload.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// The release target.
    pub target: String,
    /// The Cargo version.
    pub version: String,
    /// `[app] build`.
    pub build: u64,
    /// When it was recorded (UTC, RFC 3339).
    pub date: String,
    /// The uploaded file, relative to its dist directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
    /// Its sha256.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// The commit it was built from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_rev: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    schema: u32,
    #[serde(default, rename = "upload")]
    uploads: Vec<Entry>,
}

/// The ledger.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ledger {
    /// The uploads, oldest first.
    pub uploads: Vec<Entry>,
}

impl Ledger {
    /// The highest build recorded for a target.
    pub fn max_build(&self, target: &str) -> Option<u64> {
        self.uploads
            .iter()
            .filter(|entry| entry.target == target)
            .map(|entry| entry.build)
            .max()
    }

    /// Whether a target's build is recorded.
    pub fn has(&self, target: &str, build: u64) -> bool {
        self.uploads
            .iter()
            .any(|entry| entry.target == target && entry.build == build)
    }

    /// The file's text.
    pub fn to_toml(&self) -> String {
        let mut text = String::from(
            "# The builds the owner uploaded, one [[upload]] each. Written only by\n\
             # `icm ledger mark-uploaded` (the last line of upload.sh); `icm release`\n\
             # refuses an [app] build that is not above the highest one recorded for\n\
             # its target. Commit this file.\n\
             schema = 1\n",
        );
        for entry in &self.uploads {
            text.push_str(&format!(
                "\n[[upload]]\ntarget = {}\nversion = {}\nbuild = {}\ndate = {}\n",
                quote(&entry.target),
                quote(&entry.version),
                entry.build,
                quote(&entry.date)
            ));
            for (key, value) in [
                ("artifact", &entry.artifact),
                ("sha256", &entry.sha256),
                ("git_rev", &entry.git_rev),
            ] {
                if let Some(value) = value {
                    text.push_str(&format!("{key} = {}\n", quote(value)));
                }
            }
        }
        text
    }
}

/// A TOML basic string.
fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The ledger's path for a project directory.
pub fn path(project_dir: &Path) -> PathBuf {
    project_dir.join(FILE)
}

/// Reads the ledger (none yet: empty).
pub fn read(project_dir: &Path) -> Result<Ledger> {
    let path = path(project_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Ledger::default());
        }
        Err(error) => {
            return Err(IcmError::new(
                CheckId::ConfigInvalid,
                format!("cannot read {}: {error}", crate::paths::display(&path)),
            ));
        }
    };
    let source = crate::config::source::Source::new(&path, text.clone());
    let file: File =
        toml::from_str(&text).map_err(|error| crate::config::toml_error(&source, &error))?;
    if file.schema != 1 {
        return Err(IcmError::new(
            CheckId::ConfigTooNew,
            format!(
                "{} has schema {}; this icm reads schema 1",
                crate::paths::display(&path),
                file.schema
            ),
        )
        .evidence(Evidence::file(&path)));
    }
    Ok(Ledger {
        uploads: file.uploads,
    })
}

/// Records an upload (once per target and build).
pub fn record(project_dir: &Path, entry: Entry) -> Result<bool> {
    let mut ledger = read(project_dir)?;
    if ledger.has(&entry.target, entry.build) {
        return Ok(false);
    }
    ledger.uploads.push(entry);
    let path = path(project_dir);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| {
            IcmError::new(
                CheckId::InternalBug,
                format!("cannot create {}: {error}", crate::paths::display(dir)),
            )
        })?;
    }
    crate::output::rundir::write_atomic(&path, ledger.to_toml().as_bytes()).map_err(|error| {
        IcmError::new(
            CheckId::InternalBug,
            format!("cannot write {}: {error}", crate::paths::display(&path)),
        )
    })?;
    Ok(true)
}

/// `icm ledger show|mark-uploaded`.
pub fn run(ctx: &mut Ctx, args: &LedgerArgs) -> Result<()> {
    match &args.action {
        LedgerAction::Show => show(ctx),
        LedgerAction::MarkUploaded { target, build } => mark_uploaded(ctx, *target, *build),
    }
}

fn show(ctx: &mut Ctx) -> Result<()> {
    let project = ctx.project()?.clone();
    let ledger = read(project.dir())?;
    let mut text = String::new();
    for entry in &ledger.uploads {
        text.push_str(&format!(
            "{:8} {:12} build {:<6} {}  {}\n",
            entry.target,
            entry.version,
            entry.build,
            entry.date,
            entry.artifact.as_deref().unwrap_or("-")
        ));
    }
    if text.is_empty() {
        text.push_str("no uploads recorded yet\n");
    }
    ctx.rep.set("uploads", json!(ledger.uploads));
    ctx.rep
        .set("ledger", json!(crate::paths::display(&path(project.dir()))));
    ctx.rep
        .summary(format!("{} upload(s) recorded", ledger.uploads.len()));
    ctx.rep.content(text);
    Ok(())
}

fn mark_uploaded(ctx: &mut Ctx, target: ReleaseTarget, build: Option<u64>) -> Result<()> {
    let project = ctx.project()?.clone();
    let name = target.as_str();

    // The release: dist/latest/<target>, or the newest release of --build.
    let found: Option<(PathBuf, Manifest)> = match build {
        None => dist::resolve_latest(&project, name).and_then(|dir| {
            Manifest::read(&dir.join(super::manifest::FILE))
                .ok()
                .map(|manifest| (dir, manifest))
        }),
        Some(build) => dist::releases(&project, name)
            .into_iter()
            .filter(|(_, b, _)| *b == build)
            .find_map(|(_, _, dir)| {
                Manifest::read(&dir.join(super::manifest::FILE))
                    .ok()
                    .map(|manifest| (dir, manifest))
            }),
    };

    let entry = match (&found, build) {
        (Some((_, manifest)), _) => {
            let upload = manifest.uploads().next();
            Entry {
                target: name.to_string(),
                version: manifest.app.version.clone(),
                build: manifest.app.build,
                date: crate::time::Utc::now().rfc3339(),
                artifact: upload.map(|file| file.path.clone()),
                sha256: upload.map(|file| file.sha256.clone()),
                git_rev: manifest.source.git_rev.clone(),
            }
        }
        (None, Some(build)) => {
            ctx.rep.check(Check::warn(
                CheckId::ReleaseNotFound,
                format!(
                    "no {name} release with build {build} in {}; recording it without its file and hash",
                    crate::paths::display(&dist::root(&project))
                ),
            ));
            Entry {
                target: name.to_string(),
                version: project.package_for(platform_key(target))?.version.clone(),
                build,
                date: crate::time::Utc::now().rfc3339(),
                artifact: None,
                sha256: None,
                git_rev: None,
            }
        }
        (None, None) => {
            return Err(IcmError::new(
                CheckId::ReleaseNotFound,
                format!(
                    "there is no {name} release in {} to mark",
                    crate::paths::display(&dist::latest(&project, name))
                ),
            )
            .fix(
                "Pass the build that was uploaded, or make the release first.",
                &[&format!("icm ledger mark-uploaded {name} --build <n>")],
            ));
        }
    };

    let added = record(project.dir(), entry.clone())?;
    let ledger_path = path(project.dir());
    ctx.rep.artifact("ledger", &ledger_path);
    ctx.rep.set("upload", json!(entry));
    ctx.rep.summary(if added {
        format!(
            "recorded the {name} upload of {} build {} in {}",
            entry.version,
            entry.build,
            crate::paths::display(&ledger_path)
        )
    } else {
        format!("{name} build {} was already recorded", entry.build)
    });
    ctx.rep.next(
        format!("git add {}", crate::paths::display(&ledger_path)),
        "commit the ledger, so every checkout knows the build was used",
    );
    Ok(())
}

/// The config key whose package a target builds.
pub fn platform_key(target: ReleaseTarget) -> &'static str {
    match target {
        ReleaseTarget::Ios => "ios",
        ReleaseTarget::Android => "android",
        ReleaseTarget::Web => "web",
        ReleaseTarget::Macos | ReleaseTarget::Windows | ReleaseTarget::Linux => "desktop",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(target: &str, build: u64) -> Entry {
        Entry {
            target: target.into(),
            version: "1.0.0".into(),
            build,
            date: "2026-10-07T00:00:00Z".into(),
            artifact: Some("Notes \"final\".ipa".into()),
            sha256: Some("ab".into()),
            git_rev: None,
        }
    }

    #[test]
    fn the_ledger_round_trips_and_keeps_one_entry_per_build() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()).unwrap(), Ledger::default());
        assert!(record(dir.path(), entry("ios", 12)).unwrap());
        assert!(!record(dir.path(), entry("ios", 12)).unwrap());
        assert!(record(dir.path(), entry("ios", 13)).unwrap());
        assert!(record(dir.path(), entry("android", 4)).unwrap());

        let ledger = read(dir.path()).unwrap();
        assert_eq!(ledger.uploads.len(), 3);
        assert_eq!(ledger.max_build("ios"), Some(13));
        assert_eq!(ledger.max_build("android"), Some(4));
        assert_eq!(ledger.max_build("web"), None);
        assert_eq!(
            ledger.uploads[0].artifact.as_deref(),
            Some("Notes \"final\".ipa")
        );
        let text = std::fs::read_to_string(path(dir.path())).unwrap();
        assert!(
            text.starts_with("# The builds the owner uploaded"),
            "{text}"
        );

        std::fs::write(path(dir.path()), "schema = 1\n[[upload]]\ntarget = 3\n").unwrap();
        assert_eq!(read(dir.path()).unwrap_err().id, "config.invalid");
    }
}
