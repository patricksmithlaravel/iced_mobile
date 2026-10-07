//! `artifacts.json` (schema `icm.artifacts/1`, design §4.7): what a release
//! produced, from what, with which tools, how it was signed and checked,
//! and what the owner runs next. `icm verify` reads it back for the
//! severities of the release (an unsigned `--sign none` artifact verifies
//! with WARNs), and `icm ledger mark-uploaded` for the build and hashes.
//!
//! Within `/1` fields are only added; a rename or removal is `/2`.

use super::dist::FileEntry;
use super::gates::Tally;
use crate::catalogue::CheckId;
use crate::error::{Evidence, IcmError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// The schema.
pub const SCHEMA: &str = "icm.artifacts/1";

/// The file name in the dist directory.
pub const FILE: &str = "artifacts.json";

/// The app a release is of.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct App {
    /// `[app] id`.
    pub id: String,
    /// `[app] name`.
    pub name: String,
    /// The Cargo version.
    pub version: String,
    /// `[app] build`.
    pub build: u64,
}

/// Where the release came from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// `git rev-parse HEAD`, if the project is in git.
    pub git_rev: Option<String>,
    /// Whether tracked files had changes (`--allow-dirty`).
    pub dirty: Option<bool>,
    /// The sha256 of `Cargo.lock`.
    pub cargo_lock_sha256: Option<String>,
}

/// The framework the app was built with.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Framework {
    /// The `iced` source in `Cargo.lock` (`git+…?tag=…#rev`, or `path`).
    pub source: Option<String>,
}

/// Where a release carries its third-party notices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticesAt {
    /// The artifact (relative to the dist directory).
    pub artifact: String,
    /// The path inside it.
    pub path: String,
}

/// The whole file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// `icm.artifacts/1`.
    pub schema: String,
    /// The release target.
    pub target: String,
    /// When the release was made (UTC, RFC 3339).
    pub created: String,
    /// The icm that made it (`icm --version`).
    pub icm: String,
    /// The app.
    pub app: App,
    /// Its source.
    pub source: Source,
    /// The framework.
    pub framework: Framework,
    /// Tool versions (`rustc`, `xcode`, `sdk`, `ndk`, `bundletool`, ...).
    pub tools: BTreeMap<String, String>,
    /// The files, relative to the dist directory.
    pub files: Vec<FileEntry>,
    /// `--sign`: `auto` or `none`.
    pub sign: String,
    /// Whether the upload files are signed for the store.
    pub signed: bool,
    /// Whether the owner can upload them as they are: signed, every gate
    /// passed, nothing waiting for the owner.
    pub uploadable: bool,
    /// The signing identity and profile or key (references, never secrets).
    #[serde(default)]
    pub signing: Value,
    /// What the release checked.
    pub checks: Tally,
    /// Where the third-party notices are inside the artifacts.
    #[serde(default)]
    pub notices: Vec<NoticesAt>,
    /// What the owner runs next (UPLOAD.md in structured form).
    #[serde(default)]
    pub owner_steps: Vec<Value>,
}

impl Manifest {
    /// Reads and checks `artifacts.json`.
    pub fn read(path: &Path) -> Result<Manifest, IcmError> {
        let text = std::fs::read_to_string(path).map_err(|error| {
            IcmError::new(
                CheckId::ReleaseNotFound,
                format!("cannot read {}: {error}", crate::paths::display(path)),
            )
        })?;
        let manifest: Manifest = serde_json::from_str(&text).map_err(|error| {
            IcmError::new(
                CheckId::ReleaseArtifactChanged,
                format!(
                    "{} is not an {SCHEMA} manifest: {error}",
                    crate::paths::display(path)
                ),
            )
            .evidence(Evidence::file(path))
        })?;
        if manifest.schema != SCHEMA {
            return Err(IcmError::new(
                CheckId::ConfigTooNew,
                format!(
                    "{} has schema {}; this icm reads {SCHEMA}",
                    crate::paths::display(path),
                    manifest.schema
                ),
            )
            .evidence(Evidence::file(path)));
        }
        Ok(manifest)
    }

    /// Writes it (pretty, with a final newline).
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        let mut text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        text.push('\n');
        crate::output::rundir::write_atomic(path, text.as_bytes())
    }

    /// The files the owner uploads or ships.
    pub fn uploads(&self) -> impl Iterator<Item = &FileEntry> {
        self.files.iter().filter(|file| file.role == "upload")
    }

    /// A file by its path relative to the dist directory.
    pub fn file(&self, relative: &str) -> Option<&FileEntry> {
        self.files.iter().find(|file| file.path == relative)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        Manifest {
            schema: SCHEMA.to_string(),
            target: "ios".into(),
            created: "2026-10-06T21:03:11Z".into(),
            icm: "0.14.1-mobile.1 (rev abc)".into(),
            app: App {
                id: "com.acme.notes".into(),
                name: "Notes".into(),
                version: "1.0.0".into(),
                build: 12,
            },
            source: Source {
                git_rev: Some("1f2e3d4".into()),
                dirty: Some(false),
                cargo_lock_sha256: None,
            },
            framework: Framework::default(),
            tools: BTreeMap::from([("rustc".to_string(), "1.98.0".to_string())]),
            files: vec![FileEntry {
                role: "upload".into(),
                kind: "ipa".into(),
                path: "Notes.ipa".into(),
                bytes: 3,
                sha256: "ab".into(),
            }],
            sign: "auto".into(),
            signed: true,
            uploadable: true,
            signing: Value::Null,
            checks: Tally::default(),
            notices: vec![],
            owner_steps: vec![],
        }
    }

    #[test]
    fn manifests_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let manifest = sample();
        manifest.write(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"schema\": \"icm.artifacts/1\""), "{text}");
        let back = Manifest::read(&path).unwrap();
        assert_eq!(back, manifest);
        assert_eq!(back.uploads().count(), 1);
        assert!(back.file("Notes.ipa").is_some());

        std::fs::write(&path, text.replace("icm.artifacts/1", "icm.artifacts/2")).unwrap();
        assert_eq!(Manifest::read(&path).unwrap_err().id, "config.too_new");
        std::fs::write(&path, "{}").unwrap();
        assert!(Manifest::read(&path).is_err());
    }
}
