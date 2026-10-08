//! `release.secret_in_artifacts` (design §12, Appendix D): what a release
//! ships holds no value of a secret-named variable in icm's environment.
//!
//! A release build inherits icm's environment, so an app that reads such a
//! variable at build time (`option_env!`, `env!`, a build script) bakes its
//! value into the binary, the wasm or the site, and the owner would get an
//! uploadable artifact holding a secret that icm redacts everywhere else.
//! The release searches every file it ships (each file `artifacts.json`
//! lists, inside a `.app` or the web `site/` too) and the binaries cargo
//! built for it (before an `.ipa`, `.aab`, `.deb`, `.msi` or AppImage
//! compresses them) for each value raw, JSON-escaped and percent-encoded
//! ([`crate::process::secret_forms`]). icm's own `artifacts.json`,
//! `UPLOAD.md` and `upload.sh`, which name variables and keychain profiles,
//! are not among them. `icm verify` searches the listed files, or the
//! artifact, with the variables of its own environment.
//!
//! A value found is a FAIL and the release is not uploadable; under `--sign
//! none` it is a WARN. Only the variables' names are reported, never their
//! values. Without a secret-named variable in the environment there is
//! nothing to search for and no check.

use crate::catalogue::CheckId;
use crate::cli::SignMode;
use crate::error::{Check, Evidence};
use std::io::Read;
use std::path::{Path, PathBuf};

/// How much of a file is searched at once.
const CHUNK: usize = 1 << 20;

/// How many files the evidence names.
const EVIDENCE: usize = 10;

/// The gate over `roots` (files or directories), for the secret values of
/// icm's environment; `None` when it has none. Paths under `base` are
/// shown relative to it.
pub fn check(roots: &[PathBuf], base: &Path, mode: SignMode) -> Option<Check> {
    check_with(
        &crate::process::environment_secret_variables(),
        roots,
        base,
        mode,
    )
}

/// [`check`] for given variables (name, value).
pub fn check_with(
    variables: &[(String, String)],
    roots: &[PathBuf],
    base: &Path,
    mode: SignMode,
) -> Option<Check> {
    if variables.is_empty() {
        return None;
    }
    let needles: Vec<(&str, Vec<Vec<u8>>)> = variables
        .iter()
        .map(|(name, value)| {
            let forms = crate::process::secret_forms(value)
                .into_iter()
                .map(String::into_bytes)
                .collect();
            (name.as_str(), forms)
        })
        .collect();
    let mut files = Vec::new();
    for root in roots {
        collect(root, &mut files);
    }
    files.sort();
    files.dedup();

    let mut found: Vec<(PathBuf, Vec<&str>)> = Vec::new();
    for file in &files {
        let names = search(file, &needles);
        if !names.is_empty() {
            found.push((file.clone(), names));
        }
    }
    let show = |path: &Path| match path.strip_prefix(base) {
        Ok(relative) => relative.display().to_string(),
        Err(_) => crate::paths::display(path),
    };
    if found.is_empty() {
        return Some(Check::pass(
            CheckId::ReleaseSecretInArtifacts,
            format!(
                "no shipped file holds the value of a secret-named variable in icm's environment ({} variable{} searched in {} files)",
                variables.len(),
                if variables.len() == 1 { "" } else { "s" },
                files.len()
            ),
        ));
    }

    let mut names: Vec<&str> = found
        .iter()
        .flat_map(|(_, names)| names.iter().copied())
        .collect();
    names.sort_unstable();
    names.dedup();
    let listed: Vec<String> = found
        .iter()
        .take(EVIDENCE)
        .map(|(path, _)| show(path))
        .collect();
    let more = found.len().saturating_sub(EVIDENCE);
    let mut detail = format!(
        "the value of {} (secret-named, in icm's environment, which the build inherits) is in {} shipped file{}: {}{}",
        names
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", "),
        found.len(),
        if found.len() == 1 { "" } else { "s" },
        listed.join(", "),
        if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        }
    );
    let mut check = match mode {
        SignMode::Auto => Check::fail(CheckId::ReleaseSecretInArtifacts, detail),
        SignMode::None => {
            detail.push_str(" (a WARN under --sign none: the artifacts are not uploadable)");
            Check::warn(CheckId::ReleaseSecretInArtifacts, detail)
        }
    };
    for (path, _) in found.iter().take(EVIDENCE) {
        check = check.evidence(Evidence::file(path));
    }
    Some(check)
}

/// The regular files under `root` (or `root` itself), symlinks left out.
fn collect(root: &Path, files: &mut Vec<PathBuf>) {
    let Ok(meta) = std::fs::symlink_metadata(root) else {
        return;
    };
    if meta.is_file() {
        files.push(root.to_path_buf());
    } else if meta.is_dir()
        && let Ok(entries) = std::fs::read_dir(root)
    {
        for entry in entries.flatten() {
            collect(&entry.path(), files);
        }
    }
}

/// The names of the variables one of whose forms `file` holds.
fn search<'a>(file: &Path, needles: &[(&'a str, Vec<Vec<u8>>)]) -> Vec<&'a str> {
    let Ok(mut reader) = std::fs::File::open(file) else {
        return Vec::new();
    };
    let longest = needles
        .iter()
        .flat_map(|(_, forms)| forms.iter().map(Vec::len))
        .max()
        .unwrap_or(1);
    let mut found: Vec<&str> = Vec::new();
    let mut window: Vec<u8> = Vec::with_capacity(CHUNK + longest);
    let mut chunk = vec![0u8; CHUNK];
    loop {
        let read = match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        window.extend_from_slice(&chunk[..read]);
        for (name, forms) in needles {
            if !found.contains(name) && forms.iter().any(|form| contains(&window, form)) {
                found.push(*name);
            }
        }
        if found.len() == needles.len() {
            break;
        }
        // Keep the tail a form could start in.
        let keep = longest.saturating_sub(1).min(window.len());
        let _ = window.drain(..window.len() - keep);
    }
    found
}

/// Whether `haystack` holds `needle`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    let Some(&first) = needle.first() else {
        return false;
    };
    if haystack.len() < needle.len() {
        return false;
    }
    let last = haystack.len() - needle.len();
    let mut at = 0;
    while at <= last {
        match haystack[at..=last].iter().position(|&byte| byte == first) {
            None => return false,
            Some(offset) => {
                let start = at + offset;
                if &haystack[start..start + needle.len()] == needle {
                    return true;
                }
                at = start + 1;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Status;

    fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn a_baked_in_value_fails_and_names_only_the_variable() {
        let dist = tempfile::tempdir().unwrap();
        let token = "Zq9XMARKERa1b2c3/Tok\"en";
        let pkg = dist.path().join("site/pkg");
        std::fs::create_dir_all(&pkg).unwrap();
        // Raw in a wasm, across the boundary of two chunks.
        let mut wasm = vec![0u8; CHUNK - 5];
        wasm.extend_from_slice(token.as_bytes());
        wasm.extend_from_slice(&[0u8; 64]);
        std::fs::write(pkg.join("app_bg.wasm"), &wasm).unwrap();
        // JSON-escaped in a script.
        std::fs::write(
            pkg.join("app.js"),
            format!("const t = {};", serde_json::Value::String(token.into())),
        )
        .unwrap();
        std::fs::write(dist.path().join("UPLOAD.md"), "API_TOKEN is a name").unwrap();
        let built = tempfile::tempdir().unwrap();
        let exe = built.path().join("app");
        std::fs::write(&exe, format!("\0\0{token}\0")).unwrap();

        let variables = vars(&[("API_TOKEN", token), ("DB_PASSWORD", "not-in-any-file-7")]);
        let roots = [dist.path().to_path_buf(), exe.clone()];
        let check = check_with(&variables, &roots, dist.path(), SignMode::Auto).unwrap();
        assert_eq!(check.status, Status::Fail);
        assert_eq!(check.id(), "release.secret_in_artifacts");
        let detail = &check.error.detail;
        assert!(detail.starts_with("the value of `API_TOKEN` "), "{detail}");
        assert!(detail.contains("in 3 shipped files"), "{detail}");
        assert!(detail.contains("site/pkg/app_bg.wasm"), "{detail}");
        assert!(
            !detail.contains("Zq9X") && !detail.contains("DB_PASSWORD"),
            "{detail}"
        );
        assert_eq!(check.error.evidence.len(), 3);

        let unsigned = check_with(&variables, &roots, dist.path(), SignMode::None).unwrap();
        assert_eq!(unsigned.status, Status::Warn);

        let clean = vars(&[("DB_PASSWORD", "not-in-any-file-7")]);
        let pass = check_with(&clean, &roots, dist.path(), SignMode::Auto).unwrap();
        assert_eq!(pass.status, Status::Pass);
        assert!(
            pass.error
                .detail
                .ends_with("(1 variable searched in 4 files)"),
            "{}",
            pass.error.detail
        );

        assert!(check_with(&[], &roots, dist.path(), SignMode::Auto).is_none());
    }

    #[test]
    fn contains_finds_needles() {
        assert!(contains(b"abcabd", b"abd"));
        assert!(!contains(b"abcab", b"abd"));
        assert!(!contains(b"ab", b"abd"));
        assert!(!contains(b"abc", b""));
    }
}
