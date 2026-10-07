//! THIRD_PARTY_NOTICES (Appendix C item 18): every release ships the
//! licences of what it contains.
//!
//! [`collect`] reads `cargo metadata --filter-platform <triple>` and walks
//! the app package's normal dependencies (build and dev dependencies, and
//! proc macros, never reach the artifact). For each package it lists the
//! licence expression and repository, and gathers its licence files
//! (`license-file`, else `LICENSE*`, `LICENCE*`, `COPYING*`, `NOTICE*`,
//! `UNLICENSE`, `COPYRIGHT*`; for git and path packages without one, the
//! nearest one up to their checkout or the app's workspace root), each
//! text printed once with the packages that use it. When Fira Sans is
//! embedded (iced_graphics with `fira-sans`, or `mobile-fira-sans` on
//! Android and iOS) its SIL Open Font License comes first: the copy next to
//! the font in the framework, else the one icm embeds.
//!
//! A pipeline calls [`Release::notices`] once it has built (the sources
//! are then on disk), puts the file inside its artifacts and records where
//! with [`Release::embed_notices`]. The release core then gates it
//! (`release.notices`): a release without notices fails, and every
//! recorded place is checked inside directories and zip archives (`.ipa`,
//! `.aab`, `.apk`, site and app zips); other containers are taken as
//! declared. `icm verify` repeats the check.

use super::Release;
use super::manifest::NoticesAt;
use crate::catalogue::CheckId;
use crate::context::Ctx;
use crate::error::{Check, Evidence, IcmError, Result};
use crate::process::Cmd;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The file name, in the dist directory and inside artifacts.
pub const FILE: &str = "THIRD_PARTY_NOTICES.txt";

/// Fira Sans's licence as the fork ships it, for when the framework's copy
/// is not on disk.
const OFL_FALLBACK: &str = include_str!(concat!(env!("OUT_DIR"), "/ofl.txt"));

/// One package in the notices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    /// The name.
    pub name: String,
    /// The version.
    pub version: String,
    /// The SPDX expression, if declared.
    pub license: Option<String>,
    /// The repository, if declared.
    pub repository: Option<String>,
    /// Its licence files' texts.
    pub texts: Vec<String>,
}

/// What the notices cover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notices {
    /// The rendered file.
    pub text: String,
    /// The packages, by name and version.
    pub packages: Vec<Package>,
    /// Embedded fonts and their licences.
    pub fonts: Vec<String>,
}

impl Notices {
    /// Packages that declare no licence at all.
    pub fn unlicensed(&self) -> Vec<String> {
        self.packages
            .iter()
            .filter(|p| p.license.is_none() && p.texts.is_empty())
            .map(|p| format!("{} {}", p.name, p.version))
            .collect()
    }

    /// Packages with an expression but no licence file.
    pub fn without_text(&self) -> Vec<String> {
        self.packages
            .iter()
            .filter(|p| p.license.is_some() && p.texts.is_empty())
            .map(|p| format!("{} {}", p.name, p.version))
            .collect()
    }
}

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<MetaPackage>,
    resolve: Option<Resolve>,
    workspace_root: PathBuf,
}

#[derive(Deserialize)]
struct MetaPackage {
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    license_file: Option<PathBuf>,
    #[serde(default)]
    repository: Option<String>,
    manifest_path: PathBuf,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    targets: Vec<MetaTarget>,
}

#[derive(Deserialize)]
struct MetaTarget {
    #[serde(default)]
    kind: Vec<String>,
}

#[derive(Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}

#[derive(Deserialize)]
struct Node {
    id: String,
    #[serde(default)]
    deps: Vec<NodeDep>,
    #[serde(default)]
    features: Vec<String>,
}

#[derive(Deserialize)]
struct NodeDep {
    pkg: String,
    #[serde(default)]
    dep_kinds: Vec<DepKind>,
}

#[derive(Deserialize)]
struct DepKind {
    #[serde(default)]
    kind: Option<String>,
}

/// Whether a file name is a licence file.
fn is_licence_file(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "LICENSE",
        "LICENCE",
        "COPYING",
        "NOTICE",
        "UNLICENSE",
        "COPYRIGHT",
    ]
    .iter()
    .any(|prefix| upper.starts_with(prefix))
}

/// The licence files in a directory, sorted.
fn licence_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
                .filter(|entry| is_licence_file(&entry.file_name().to_string_lossy()))
                .map(|entry| entry.path())
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// A package's licence texts: its `license-file`, its own licence files,
/// else (git and path packages only) the nearest ones up to its checkout
/// root (`.git` or `.cargo-ok`) or `stop`.
fn licence_texts(package: &MetaPackage, stop: &Path) -> Vec<String> {
    let dir = package
        .manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let read = |path: &Path| std::fs::read_to_string(path).ok();
    if let Some(file) = &package.license_file
        && let Some(text) = read(&dir.join(file))
    {
        return vec![text];
    }
    let own: Vec<String> = licence_files(&dir).iter().filter_map(|p| read(p)).collect();
    if !own.is_empty() {
        return own;
    }
    let registry = package
        .source
        .as_deref()
        .is_some_and(|source| source.starts_with("registry+") || source.starts_with("sparse+"));
    if registry {
        return Vec::new();
    }
    let mut current = dir.parent();
    for _ in 0..4 {
        let Some(up) = current else {
            break;
        };
        let texts: Vec<String> = licence_files(up).iter().filter_map(|p| read(p)).collect();
        if !texts.is_empty() {
            return texts;
        }
        if up == stop || up.join(".git").exists() || up.join(".cargo-ok").exists() {
            break;
        }
        current = up.parent();
    }
    Vec::new()
}

/// Reads `cargo metadata` for the package and triple and renders the
/// notices.
pub fn collect(
    ctx: &Ctx,
    manifest: &Path,
    package: &str,
    triple: &str,
    title: &str,
) -> Result<Notices> {
    let mut cmd = Cmd::tool("cargo")
        .args(["metadata", "--format-version", "1", "--locked"])
        .args(["--filter-platform", triple])
        .arg("--manifest-path")
        .arg(manifest)
        .cwd(manifest.parent().unwrap_or(Path::new(".")))
        .timeout(Duration::from_secs(300));
    if ctx.global.offline {
        cmd = cmd.arg("--offline");
    }
    let outcome = ctx.step("cargo.metadata.notices", &cmd)?;
    if !outcome.success() {
        return Err(ctx.step_failure(
            "cargo.metadata.notices",
            CheckId::BuildCargoFailed,
            &outcome,
        ));
    }
    let metadata: Metadata = serde_json::from_slice(&outcome.stdout).map_err(|error| {
        IcmError::new(
            CheckId::InternalBug,
            format!("cannot read cargo metadata: {error}"),
        )
    })?;
    Ok(from_metadata(&metadata, package, triple, title))
}

fn from_metadata(metadata: &Metadata, package: &str, triple: &str, title: &str) -> Notices {
    let by_id: HashMap<&str, &MetaPackage> = metadata
        .packages
        .iter()
        .map(|p| (p.id.as_str(), p))
        .collect();
    let nodes: HashMap<&str, &Node> = metadata
        .resolve
        .iter()
        .flat_map(|resolve| resolve.nodes.iter())
        .map(|node| (node.id.as_str(), node))
        .collect();

    let root = metadata
        .packages
        .iter()
        .find(|p| p.name == package && p.source.is_none())
        .or_else(|| metadata.packages.iter().find(|p| p.name == package));
    let proc_macro = |p: &MetaPackage| {
        !p.targets.is_empty()
            && p.targets
                .iter()
                .all(|target| target.kind.iter().any(|kind| kind == "proc-macro"))
    };

    // The normal dependencies reachable from the app, proc macros left out.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut queue: Vec<&str> = root.map(|p| p.id.as_str()).into_iter().collect();
    while let Some(id) = queue.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(node) = nodes.get(id) else {
            continue;
        };
        for dep in &node.deps {
            let normal =
                dep.dep_kinds.is_empty() || dep.dep_kinds.iter().any(|kind| kind.kind.is_none());
            let is_macro = by_id.get(dep.pkg.as_str()).is_some_and(|p| proc_macro(p));
            if normal && !is_macro {
                queue.push(dep.pkg.as_str());
            }
        }
    }

    let mut packages: Vec<Package> = seen
        .iter()
        .filter(|id| root.is_none_or(|root| root.id != **id))
        .filter_map(|id| by_id.get(id))
        .map(|p| Package {
            name: p.name.clone(),
            version: p.version.clone(),
            license: p.license.clone(),
            repository: p.repository.clone(),
            texts: licence_texts(p, &metadata.workspace_root),
        })
        .collect();
    packages.sort_by(|a, b| a.name.cmp(&b.name).then(a.version.cmp(&b.version)));

    // Fira Sans: embedded on every target with `fira-sans`, on the phones
    // with `mobile-fira-sans`.
    let phone = triple.contains("-android") || triple.contains("-apple-ios");
    let graphics = metadata
        .packages
        .iter()
        .filter(|p| p.name == "iced_graphics")
        .find(|p| {
            nodes.get(p.id.as_str()).is_some_and(|node| {
                seen.contains(p.id.as_str())
                    && node.features.iter().any(|feature| {
                        feature == "fira-sans" || (phone && feature == "mobile-fira-sans")
                    })
            })
        });
    let mut fonts = Vec::new();
    let mut ofl = None;
    if let Some(graphics) = graphics {
        let beside = graphics
            .manifest_path
            .parent()
            .map(|dir| dir.join("fonts").join("OFL.txt"));
        ofl = beside
            .and_then(|path| std::fs::read_to_string(path).ok())
            .or_else(|| (!OFL_FALLBACK.is_empty()).then(|| OFL_FALLBACK.to_string()));
        fonts.push("Fira Sans: SIL Open Font License 1.1".to_string());
    }

    let text = render(title, triple, &packages, &fonts, ofl.as_deref());
    Notices {
        text,
        packages,
        fonts,
    }
}

fn rule(text: &mut String, title: &str) {
    let line = "=".repeat(72);
    text.push_str(&format!("\n{line}\n{title}\n{line}\n\n"));
}

/// The file's text.
fn render(
    title: &str,
    triple: &str,
    packages: &[Package],
    fonts: &[String],
    ofl: Option<&str>,
) -> String {
    let mut text = format!(
        "THIRD-PARTY NOTICES\n\n{title} contains the third-party software below, each used under the\n\
         licence named next to it. The licence texts follow the list.\n\n\
         (Generated by icm {} from the {triple} dependencies of the build.)\n",
        crate::buildinfo::VERSION
    );

    if !fonts.is_empty() {
        rule(&mut text, "Fonts");
        for font in fonts {
            text.push_str(&format!("{font}\n"));
        }
        text.push('\n');
        match ofl {
            Some(ofl) => text.push_str(ofl.trim_end()),
            None => text.push_str(
                "The SIL Open Font License 1.1: https://openfontlicense.org/open-font-license-official-text/",
            ),
        }
        text.push('\n');
    }

    rule(&mut text, &format!("Rust crates ({})", packages.len()));
    for package in packages {
        text.push_str(&format!(
            "{} {}: {}{}\n",
            package.name,
            package.version,
            package.license.as_deref().unwrap_or("no licence declared"),
            package
                .repository
                .as_deref()
                .map(|repo| format!(" ({repo})"))
                .unwrap_or_default()
        ));
    }

    // Each distinct text once, with who uses it.
    let mut texts: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for package in packages {
        for body in &package.texts {
            let key = crate::hash::sha256_hex(body.trim().as_bytes());
            let entry = texts
                .entry(key)
                .or_insert_with(|| (body.trim().to_string(), Vec::new()));
            let label = format!("{} {}", package.name, package.version);
            if !entry.1.contains(&label) {
                entry.1.push(label);
            }
        }
    }
    let mut texts: Vec<(String, Vec<String>)> = texts.into_values().collect();
    texts.sort_by(|a, b| a.1.cmp(&b.1));
    if !texts.is_empty() {
        rule(&mut text, "Licence texts");
        for (body, users) in texts {
            text.push_str(&format!(
                "{}\nUsed by: {}\n{}\n\n{body}\n\n",
                "-".repeat(72),
                users.join(", "),
                "-".repeat(72)
            ));
        }
    }

    let without: Vec<String> = packages
        .iter()
        .filter(|p| p.texts.is_empty())
        .map(|p| {
            format!(
                "{} {} ({})",
                p.name,
                p.version,
                p.license.as_deref().unwrap_or("no licence declared")
            )
        })
        .collect();
    if !without.is_empty() {
        rule(&mut text, "Packages without a licence file");
        text.push_str(
            "These packages ship no licence file; their declared licence applies, with the\n\
             standard text of each SPDX identifier (https://spdx.org/licenses/):\n\n",
        );
        for line in without {
            text.push_str(&format!("{line}\n"));
        }
    }
    text
}

/// Whether a path inside an artifact is there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Presence {
    /// Found.
    Present,
    /// Not there.
    Missing,
    /// icm cannot look inside this kind of file.
    Unknown,
}

/// The names in a zip archive's central directory, if `path` is a zip
/// (bounds-checked: any file may be handed to `icm verify`).
pub fn zip_names(path: &Path) -> Option<Vec<String>> {
    let bytes = std::fs::read(path).ok()?;
    if !bytes.starts_with(b"PK\x03\x04") && !bytes.starts_with(b"PK\x05\x06") {
        return None;
    }
    let get16 = |at: usize| -> Option<usize> {
        Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?) as usize)
    };
    let get32 = |at: usize| -> Option<usize> {
        Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize)
    };
    let floor = bytes.len().saturating_sub(65_557);
    let end = (floor..bytes.len().saturating_sub(3))
        .rev()
        .find(|&at| bytes[at..at + 4] == 0x0605_4b50u32.to_le_bytes())?;
    let count = get16(end + 10)?;
    let mut at = get32(end + 16)?;
    let mut names = Vec::with_capacity(count.min(65_536));
    for _ in 0..count {
        if get32(at)? != 0x0201_4b50 {
            return None;
        }
        let name_len = get16(at + 28)?;
        let extra = get16(at + 30)?;
        let comment = get16(at + 32)?;
        let name = bytes.get(at + 46..at + 46 + name_len)?;
        names.push(String::from_utf8_lossy(name).into_owned());
        at += 46 + name_len + extra + comment;
    }
    Some(names)
}

/// Whether `inner` is inside `artifact` (a directory or a zip).
pub fn presence(artifact: &Path, inner: &str) -> Presence {
    if artifact.is_dir() {
        return if artifact.join(inner).is_file() {
            Presence::Present
        } else {
            Presence::Missing
        };
    }
    match zip_names(artifact) {
        Some(names) if names.iter().any(|name| name == inner) => Presence::Present,
        Some(_) => Presence::Missing,
        None => Presence::Unknown,
    }
}

/// A file named like the notices anywhere inside a directory or zip
/// (an artifact `artifacts.json` does not describe).
pub fn find_any(artifact: &Path) -> Presence {
    let matches = |name: &str| name.rsplit('/').next() == Some(FILE);
    if artifact.is_dir() {
        let mut stack = vec![artifact.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if entry.file_name().to_string_lossy() == FILE {
                    return Presence::Present;
                }
            }
        }
        return Presence::Missing;
    }
    match zip_names(artifact) {
        Some(names) if names.iter().any(|name| matches(name)) => Presence::Present,
        Some(_) => Presence::Missing,
        None => Presence::Unknown,
    }
}

/// The `release.notices` checks for recorded places.
pub fn checks(dist: &Path, places: &[NoticesAt]) -> Vec<Check> {
    if places.is_empty() {
        return vec![
            Check::fail(
                CheckId::ReleaseNotices,
                format!(
                    "no artifact carries {FILE}: the release ships third-party code (and Fira Sans) without their licences"
                ),
            )
            .fix(
                "The target's pipeline must put the notices inside the artifact (Release::notices, then Release::embed_notices).",
                &[],
            ),
        ];
    }
    places
        .iter()
        .map(|place| {
            let artifact = dist.join(&place.artifact);
            match presence(&artifact, &place.path) {
                Presence::Present => Check::pass(
                    CheckId::ReleaseNotices,
                    format!("{} carries {}", place.artifact, place.path),
                ),
                Presence::Missing => Check::fail(
                    CheckId::ReleaseNotices,
                    format!("{} lacks {}", place.artifact, place.path),
                )
                .evidence(Evidence::file(&artifact)),
                Presence::Unknown => Check::info(
                    CheckId::ReleaseNotices,
                    format!(
                        "{} carries {} (declared by the pipeline; icm does not look inside this kind of file)",
                        place.artifact, place.path
                    ),
                ),
            }
        })
        .collect()
}

impl Release {
    /// Generates THIRD_PARTY_NOTICES for the build's triple (`None`: the
    /// host) once, copies it into the dist directory (role `notices`) and
    /// returns the generated file for the pipeline to put inside its
    /// artifacts. Reports `release.licence_unknown` (WARN) for packages
    /// that declare no licence.
    pub fn notices(&mut self, ctx: &Ctx, triple: Option<&str>) -> Result<PathBuf> {
        let path = self.gen_dir.join(FILE);
        if self.notices_ready && path.is_file() {
            return Ok(path);
        }
        let triple = triple.unwrap_or(crate::toolchain::host_triple());
        let title = format!(
            "{} {} ({})",
            self.config().app.name,
            self.version,
            self.target.as_str()
        );
        let notices = collect(
            ctx,
            &self.package.manifest_path,
            &self.package.name,
            triple,
            &title,
        )?;
        let io = |what: &str, at: &Path, error: std::io::Error| {
            IcmError::new(
                CheckId::InternalBug,
                format!("cannot {what} {}: {error}", crate::paths::display(at)),
            )
        };
        std::fs::create_dir_all(&self.gen_dir).map_err(|e| io("create", &self.gen_dir, e))?;
        std::fs::write(&path, &notices.text).map_err(|e| io("write", &path, e))?;
        let copy = self.dist.join(FILE);
        std::fs::write(&copy, &notices.text).map_err(|e| io("write", &copy, e))?;
        self.add_file("notices", "notices", &copy)?;

        let unlicensed = notices.unlicensed();
        if !unlicensed.is_empty() {
            self.check(
                ctx,
                Check::warn(
                    CheckId::ReleaseLicenceUnknown,
                    format!(
                        "{} package(s) in the build declare no licence: {}",
                        unlicensed.len(),
                        unlicensed.join(", ")
                    ),
                )
                .evidence(Evidence::file(&copy)),
            );
        }
        ctx.rep.progress(format!(
            "{FILE}: {} package(s){}{}",
            notices.packages.len(),
            if notices.fonts.is_empty() {
                String::new()
            } else {
                format!(", {}", notices.fonts.join(", "))
            },
            match notices.without_text().len() {
                0 => String::new(),
                n => format!(", {n} without a licence file"),
            }
        ));
        self.notices_ready = true;
        Ok(path)
    }

    /// Records that `artifact` (inside the dist directory) carries the
    /// notices at `inner`; the release core checks it.
    pub fn embed_notices(&mut self, artifact: &Path, inner: &str) -> Result<()> {
        let relative = artifact.strip_prefix(&self.dist).map_err(|_| {
            IcmError::new(
                CheckId::InternalBug,
                format!(
                    "{} is not inside {}",
                    crate::paths::display(artifact),
                    crate::paths::display(&self.dist)
                ),
            )
        })?;
        let place = NoticesAt {
            artifact: relative.to_string_lossy().into_owned(),
            path: inner.to_string(),
        };
        if !self.notices.contains(&place) {
            self.notices.push(place);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(json: &str) -> Metadata {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn only_shipped_packages_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (name, licence) in [
            ("app", None),
            ("lib", Some("LICENSE-MIT")),
            ("build", Some("LICENSE")),
            ("dev", Some("LICENSE")),
            ("mac", Some("LICENSE")),
            ("bare", None),
        ] {
            let pkg = root.join(name);
            std::fs::create_dir_all(&pkg).unwrap();
            std::fs::write(pkg.join("Cargo.toml"), "").unwrap();
            if let Some(file) = licence {
                std::fs::write(pkg.join(file), format!("{name} licence text")).unwrap();
            }
        }
        let pkg = |name: &str, license: &str, kind: &str| {
            format!(
                r#"{{"id":"{name} 1.0","name":"{name}","version":"1.0.0","license":{license},"manifest_path":"{}/{name}/Cargo.toml","targets":[{{"kind":["{kind}"]}}]}}"#,
                root.display()
            )
        };
        let json = format!(
            r#"{{"workspace_root":"{root}","packages":[{},{},{},{},{},{}],"resolve":{{"nodes":[
              {{"id":"app 1.0","deps":[
                 {{"pkg":"lib 1.0","dep_kinds":[{{"kind":null}}]}},
                 {{"pkg":"build 1.0","dep_kinds":[{{"kind":"build"}}]}},
                 {{"pkg":"dev 1.0","dep_kinds":[{{"kind":"dev"}}]}},
                 {{"pkg":"mac 1.0","dep_kinds":[{{"kind":null}}]}}],"features":[]}},
              {{"id":"lib 1.0","deps":[{{"pkg":"bare 1.0","dep_kinds":[{{"kind":null}}]}}],"features":[]}},
              {{"id":"build 1.0","deps":[],"features":[]}},
              {{"id":"dev 1.0","deps":[],"features":[]}},
              {{"id":"mac 1.0","deps":[],"features":[]}},
              {{"id":"bare 1.0","deps":[],"features":[]}}]}}}}"#,
            pkg("app", "null", "bin"),
            pkg("lib", "\"MIT\"", "lib"),
            pkg("build", "\"MIT\"", "lib"),
            pkg("dev", "\"MIT\"", "lib"),
            pkg("mac", "\"MIT\"", "proc-macro"),
            pkg("bare", "null", "lib"),
            root = root.display(),
        );
        let notices = from_metadata(
            &metadata(&json),
            "app",
            "x86_64-unknown-linux-gnu",
            "App 1.0.0",
        );
        let names: Vec<&str> = notices.packages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["bare", "lib"]);
        assert_eq!(notices.unlicensed(), ["bare 1.0.0"]);
        assert!(notices.text.contains("lib 1.0.0: MIT"), "{}", notices.text);
        assert!(
            notices.text.contains("lib licence text"),
            "{}",
            notices.text
        );
        assert!(
            notices.text.contains("Used by: lib 1.0.0"),
            "{}",
            notices.text
        );
        assert!(!notices.text.contains("build licence"), "{}", notices.text);
        assert!(notices.fonts.is_empty());
    }

    #[test]
    fn zips_and_directories_are_searched() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("a.ipa");
        crate::android::zip::write(
            &zip,
            &[crate::android::zip::Entry {
                name: format!("Payload/A.app/{FILE}"),
                source: crate::android::zip::Source::Bytes(b"x".to_vec()),
            }],
        )
        .unwrap();
        assert_eq!(
            presence(&zip, &format!("Payload/A.app/{FILE}")),
            Presence::Present
        );
        assert_eq!(presence(&zip, FILE), Presence::Missing);
        assert_eq!(find_any(&zip), Presence::Present);

        let site = dir.path().join("site");
        std::fs::create_dir_all(site.join("pkg")).unwrap();
        assert_eq!(find_any(&site), Presence::Missing);
        std::fs::write(site.join(FILE), "x").unwrap();
        assert_eq!(presence(&site, FILE), Presence::Present);

        let other = dir.path().join("a.msi");
        std::fs::write(&other, b"\xd0\xcf\x11\xe0 not a zip").unwrap();
        assert_eq!(presence(&other, FILE), Presence::Unknown);
        // A truncated zip is not trusted, and does not panic.
        let bytes = std::fs::read(&zip).unwrap();
        std::fs::write(&zip, &bytes[..bytes.len() - 10]).unwrap();
        assert_eq!(presence(&zip, FILE), Presence::Unknown);

        let checks = checks(dir.path(), &[]);
        assert_eq!(checks[0].status, crate::error::Status::Fail);
    }
}
