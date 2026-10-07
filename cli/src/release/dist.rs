//! The dist layout (design §4.6): `target/icm/dist/<version>+<build>/<target>/`
//! holds a release's artifacts with `artifacts.json`, `UPLOAD.md` and
//! `upload.sh`; `dist/latest/<target>` is a relative symlink to the newest.
//!
//! Every file a release records is described by a [`FileEntry`]: its path
//! relative to the dist directory, its size and its sha256. A directory
//! (a web `site/`, a macOS `.app`) is hashed as the sha256 of a sorted
//! listing of its files' hashes (`<sha256>  <relative path>` lines, the
//! format of `shasum -a 256`), with symlinks as `link:<target>`.

use crate::context::Project;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// `target/icm/dist`.
pub fn root(project: &Project) -> PathBuf {
    project.icm_dir.join("dist")
}

/// `target/icm/dist/<version>+<build>/<target>`.
pub fn dir(project: &Project, version: &str, build: u64, target: &str) -> PathBuf {
    root(project)
        .join(format!("{version}+{build}"))
        .join(target)
}

/// `target/icm/dist/latest/<target>` (the link itself).
pub fn latest(project: &Project, target: &str) -> PathBuf {
    root(project).join("latest").join(target)
}

/// Points `dist/latest/<target>` at `dir` (a relative symlink, replaced
/// atomically).
pub fn link_latest(project: &Project, target: &str, dir: &Path) -> io::Result<()> {
    let link = latest(project, target);
    let parent = link.parent().expect("latest has a parent");
    std::fs::create_dir_all(parent)?;
    let relative = match dir.strip_prefix(root(project)) {
        Ok(inside) => Path::new("..").join(inside),
        Err(_) => dir.to_path_buf(),
    };
    let temporary = parent.join(format!(".{target}.{}", std::process::id()));
    let _ = std::fs::remove_file(&temporary);
    std::os::unix::fs::symlink(&relative, &temporary)?;
    std::fs::rename(&temporary, &link)
}

/// The directory `dist/latest/<target>` points at, if it exists.
pub fn resolve_latest(project: &Project, target: &str) -> Option<PathBuf> {
    let link = latest(project, target);
    let resolved = std::fs::canonicalize(&link).ok()?;
    resolved.is_dir().then_some(resolved)
}

/// Every release directory of a target, newest build first:
/// `(version, build, dir)`.
pub fn releases(project: &Project, target: &str) -> Vec<(String, u64, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(root(project)) else {
        return Vec::new();
    };
    let mut found: Vec<(String, u64, PathBuf, std::time::SystemTime)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let (version, build) = name.rsplit_once('+')?;
            let build: u64 = build.parse().ok()?;
            let dir = entry.path().join(target);
            let modified = dir
                .join(super::manifest::FILE)
                .metadata()
                .ok()?
                .modified()
                .ok()?;
            Some((version.to_string(), build, dir, modified))
        })
        .collect();
    found.sort_by(|a, b| b.1.cmp(&a.1).then(b.3.cmp(&a.3)));
    found
        .into_iter()
        .map(|(version, build, dir, _)| (version, build, dir))
        .collect()
}

/// One recorded file of a release (`artifacts.json` `files[]`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// What it is for: `upload` (what the owner uploads or ships),
    /// `symbols`, `notices`, `listing` (store listing assets), `metadata`
    /// (copies of generated files), `sideload`, `stage` (an input to a
    /// later stage, such as the app notarytool checks before the DMG).
    pub role: String,
    /// The kind, which is also its key in the result's `artifacts`: `ipa`,
    /// `dsym`, `aab`, `apk`, `symbols`, `site`, `site_zip`, `app`,
    /// `app_zip`, `dmg`, `msi`, `exe`, `deb`, `appimage`, `notices`, ...
    pub kind: String,
    /// The path relative to the dist directory.
    pub path: String,
    /// Its size (a directory: the sum of its files).
    pub bytes: u64,
    /// Its sha256 (a directory: see the module docs).
    pub sha256: String,
    /// macOS: the code directory hash of its signature (`codesign -d
    /// -vvv`'s `CDHash`). Stapling a notarization ticket changes a signed
    /// `.app` or `.dmg` but not its signature, so `icm verify` accepts a
    /// changed file whose signature still verifies, still has this hash
    /// and carries a stapled ticket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cdhash: Option<String>,
}

impl FileEntry {
    /// Describes a file or directory inside `dist`.
    pub fn new(dist: &Path, role: &str, kind: &str, path: &Path) -> io::Result<FileEntry> {
        let relative = path.strip_prefix(dist).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{} is not inside the dist directory {}",
                    path.display(),
                    dist.display()
                ),
            )
        })?;
        let (bytes, sha256) = digest(path)?;
        Ok(FileEntry {
            role: role.to_string(),
            kind: kind.to_string(),
            path: relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/"),
            bytes,
            sha256,
            cdhash: None,
        })
    }

    /// The absolute path under `dist`.
    pub fn absolute(&self, dist: &Path) -> PathBuf {
        dist.join(&self.path)
    }
}

/// The size and sha256 of a file, or of a directory (module docs).
pub fn digest(path: &Path) -> io::Result<(u64, String)> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_file() {
        return Ok((meta.len(), crate::hash::sha256_file(path)?));
    }
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is neither a file nor a directory", path.display()),
        ));
    }
    let mut lines: Vec<String> = Vec::new();
    let mut total = 0u64;
    walk(path, path, &mut lines, &mut total)?;
    lines.sort();
    let listing: String = lines.iter().map(|line| format!("{line}\n")).collect();
    Ok((total, crate::hash::sha256_hex(listing.as_bytes())))
}

fn walk(root: &Path, dir: &Path, lines: &mut Vec<String>, total: &mut u64) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .expect("inside the root")
            .to_string_lossy()
            .into_owned();
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            let target = std::fs::read_link(&path)?;
            lines.push(format!("link:{}  {relative}", target.display()));
        } else if kind.is_dir() {
            walk(root, &path, lines, total)?;
        } else {
            *total += entry.metadata()?.len();
            lines.push(format!("{}  {relative}", crate::hash::sha256_file(&path)?));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_and_directories_are_described() {
        let dist = tempfile::tempdir().unwrap();
        let file = dist.path().join("App.ipa");
        std::fs::write(&file, b"ipa").unwrap();
        let entry = FileEntry::new(dist.path(), "upload", "ipa", &file).unwrap();
        assert_eq!(entry.path, "App.ipa");
        assert_eq!(entry.bytes, 3);
        assert_eq!(entry.sha256, crate::hash::sha256_hex(b"ipa"));
        assert_eq!(entry.absolute(dist.path()), file);

        let site = dist.path().join("site");
        std::fs::create_dir_all(site.join("pkg")).unwrap();
        std::fs::write(site.join("index.html"), b"<html>").unwrap();
        std::fs::write(site.join("pkg/app.wasm"), b"wasm").unwrap();
        let (bytes, first) = digest(&site).unwrap();
        assert_eq!(bytes, 10);
        // Stable, and changed by any byte.
        assert_eq!(digest(&site).unwrap().1, first);
        std::fs::write(site.join("pkg/app.wasm"), b"wasn").unwrap();
        assert_ne!(digest(&site).unwrap().1, first);

        let outside = tempfile::tempdir().unwrap();
        let stray = outside.path().join("x");
        std::fs::write(&stray, b"x").unwrap();
        assert!(FileEntry::new(dist.path(), "upload", "x", &stray).is_err());
    }
}
