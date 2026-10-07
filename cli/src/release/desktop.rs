//! What the three desktop release pipelines share (design §9.6, §11.4 to
//! §11.6): the host each target needs, the icon at every size, the binary
//! readers the gates use, and the mapping of `[app] category`.
//!
//! - **Hosts.** A macOS release runs on macOS, a Windows release on
//!   Windows and a Linux release on Linux: each pipeline drives the host's
//!   own toolchain and packaging tools (codesign and hdiutil; the MSVC
//!   linker, rc.exe, WiX and signtool; dpkg-deb, dpkg-shlibdeps and
//!   appimagetool). [`require_host`] refuses any other host with
//!   `env.unsupported_host` (exit 4) before anything is built.
//!   `ICM_HOST_OS` (`macos`, `windows`, `linux`) stands in for the host in
//!   icm's own tests, which run the Windows and Linux pipelines against
//!   fake tools on a Mac.
//! - **Icons** ([`icons`]): `[app] icon` (else the template's placeholder)
//!   resampled with its alpha kept, as PNGs, a macOS iconset and a Windows
//!   ICO.
//! - **Binaries** ([`pe`], [`elf`], [`ar`]): the PE imports and subsystem of
//!   a Windows executable, the glibc versions an ELF executable needs, and
//!   the members of a `.deb`, read without any external tool.

pub mod ar;
pub mod elf;
pub mod icons;
pub mod pe;

use crate::catalogue::CheckId;
use crate::cli::ReleaseTarget;
use crate::context::{Ctx, Project};
use crate::error::{IcmError, Result};
use crate::process::{Cmd, Outcome};
use crate::tools::Env;
use std::path::{Path, PathBuf};

/// The host's operating system as the desktop pipelines name it: `macos`,
/// `windows`, `linux` (or another `std::env::consts::OS` value).
/// `ICM_HOST_OS` overrides it for icm's tests.
pub fn host_os(env: &Env) -> String {
    match env.var("ICM_HOST_OS") {
        Some(os) if !os.is_empty() => os.to_string(),
        _ => std::env::consts::OS.to_string(),
    }
}

/// The host a desktop target is built on.
pub fn required_os(target: ReleaseTarget) -> Option<&'static str> {
    match target {
        ReleaseTarget::Macos => Some("macos"),
        ReleaseTarget::Windows => Some("windows"),
        ReleaseTarget::Linux => Some("linux"),
        _ => None,
    }
}

fn os_name(os: &str) -> &str {
    match os {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    }
}

/// What a target needs from its host, for the error.
fn host_needs(target: ReleaseTarget) -> &'static str {
    match target {
        ReleaseTarget::Macos => {
            "codesign, iconutil, hdiutil and the macOS SDK the binary links against exist only on macOS"
        }
        ReleaseTarget::Windows => {
            "the MSVC linker and its static C runtime, rc.exe, WiX v5 and signtool run only on Windows"
        }
        ReleaseTarget::Linux => {
            "the binary links against the host's glibc, and dpkg-deb, dpkg-shlibdeps and appimagetool are Linux tools"
        }
        _ => "",
    }
}

/// `env.unsupported_host` (exit 4) unless this host builds `target`.
pub fn require_host(env: &Env, target: ReleaseTarget, what: &str) -> Result<()> {
    let Some(needed) = required_os(target) else {
        return Ok(());
    };
    let host = host_os(env);
    if host == needed {
        return Ok(());
    }
    let mut fix = format!(
        "Run `icm {what}` on a {} host: the {} job of .github/workflows/icm-desktop.yml is one.",
        os_name(needed),
        match target {
            ReleaseTarget::Windows => "windows-latest",
            ReleaseTarget::Linux => "ubuntu:22.04 container",
            _ => "macos-latest",
        }
    );
    if target == ReleaseTarget::Windows {
        fix.push_str(
            " This build of icm does not run on Windows hosts yet (its process, signal and lock handling is Unix-only), so Windows installers wait for that support.",
        );
    }
    Err(IcmError::new(
        CheckId::EnvUnsupportedHost,
        format!(
            "{} releases are built on {}: {}; this host is {}",
            target.as_str(),
            os_name(needed),
            host_needs(target),
            os_name(&host)
        ),
    )
    .fix(fix, &[]))
}

/// The files `[app] resources` matches, relative to the project (the web
/// site's glob rules), or `config.invalid`.
pub fn resources(project: &Project) -> Result<Vec<PathBuf>> {
    let files =
        crate::web::site::resources(project.dir(), &project.app().resources).map_err(|detail| {
            IcmError::new(CheckId::ConfigInvalid, detail)
                .evidence(project.config.evidence("app.resources"))
        })?;
    Ok(files
        .into_iter()
        .filter_map(|file| file.strip_prefix(project.dir()).ok().map(Path::to_path_buf))
        .filter(|relative| !relative.as_os_str().is_empty())
        .collect())
}

/// `[app] publisher`, else the app's name (installers need a publisher).
pub fn publisher(project: &Project) -> String {
    let app = project.app();
    app.publisher.clone().unwrap_or_else(|| app.name.clone())
}

/// A file name for installers and packages: the app's name without the
/// characters Windows refuses.
pub fn file_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').to_string();
    if cleaned.is_empty() {
        "App".to_string()
    } else {
        cleaned
    }
}

/// The App Store categories macOS knows (`LSApplicationCategoryType`
/// without its `public.app-category.` prefix).
const MACOS_CATEGORIES: &[&str] = &[
    "business",
    "developer-tools",
    "education",
    "entertainment",
    "finance",
    "games",
    "graphics-design",
    "healthcare-fitness",
    "lifestyle",
    "medical",
    "music",
    "news",
    "photography",
    "productivity",
    "reference",
    "social-networking",
    "sports",
    "travel",
    "utilities",
    "video",
    "weather",
];

/// `LSApplicationCategoryType` for `[app] category` (`utilities` or
/// `public.app-category.utilities`), if macOS knows it.
pub fn macos_category(category: &str) -> Option<String> {
    let bare = category
        .trim()
        .strip_prefix("public.app-category.")
        .unwrap_or(category.trim())
        .to_ascii_lowercase();
    MACOS_CATEGORIES
        .contains(&bare.as_str())
        .then(|| format!("public.app-category.{bare}"))
}

/// The freedesktop.org `Categories=` of a `.desktop` file for `[app]
/// category` (registered main categories only, each additional one after
/// the main one it requires).
pub fn linux_categories(category: Option<&str>) -> &'static str {
    let bare = category
        .map(|c| {
            c.trim()
                .strip_prefix("public.app-category.")
                .unwrap_or(c.trim())
                .to_ascii_lowercase()
        })
        .unwrap_or_default();
    match bare.as_str() {
        "developer-tools" => "Development;",
        "education" => "Education;",
        "entertainment" | "games" => "Game;",
        "finance" => "Office;Finance;",
        "business" | "productivity" => "Office;",
        "graphics-design" | "photography" => "Graphics;",
        "music" => "AudioVideo;Audio;",
        "video" => "AudioVideo;Video;",
        "news" | "social-networking" => "Network;",
        "reference" | "medical" | "healthcare-fitness" => "Education;",
        _ => "Utility;",
    }
}

/// The Debian `Section:` for `[app] category`.
pub fn deb_section(category: Option<&str>) -> &'static str {
    match linux_categories(category) {
        "Development;" => "devel",
        "Game;" => "games",
        "Graphics;" => "graphics",
        "AudioVideo;Audio;" | "AudioVideo;Video;" => "video",
        "Network;" => "net",
        "Education;" => "education",
        "Utility;" => "utils",
        _ => "misc",
    }
}

/// Runs a step and turns a non-zero exit into `id`.
pub fn run(ctx: &Ctx, name: &str, cmd: &Cmd, id: CheckId) -> Result<Outcome> {
    let outcome = ctx.step(name, cmd)?;
    if outcome.success() {
        Ok(outcome)
    } else {
        Err(ctx.step_failure(name, id, &outcome))
    }
}

/// The last `lines` non-empty lines a tool printed (stdout, then stderr),
/// on one line.
pub fn said(outcome: &Outcome, lines: usize) -> String {
    let text = format!("{}\n{}", outcome.stdout_text(), outcome.stderr_text());
    let all: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    all[all.len().saturating_sub(lines)..].join(" / ")
}

/// A failed file operation (exit 70: icm's own work).
pub fn io_error(what: &str, path: &Path, error: std::io::Error) -> IcmError {
    IcmError::new(
        CheckId::InternalBug,
        format!("cannot {what} {}: {error}", crate::paths::display(path)),
    )
}

/// Empties (or creates) a directory.
pub fn fresh_dir(dir: &Path) -> Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|e| io_error("empty", dir, e))?;
    }
    std::fs::create_dir_all(dir).map_err(|e| io_error("create", dir, e))
}

/// Writes a file, creating its directory, with a Unix mode.
pub fn write_file(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_error("create", parent, e))?;
    }
    std::fs::write(path, bytes).map_err(|e| io_error("write", path, e))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| io_error("chmod", path, e))
}

/// Copies a file, creating its directory, with a Unix mode.
pub fn copy_file(from: &Path, to: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if let (Ok(a), Ok(b)) = (std::fs::canonicalize(from), std::fs::canonicalize(to))
        && a == b
    {
        return Err(IcmError::new(
            CheckId::InternalBug,
            format!(
                "refusing to copy {} onto itself",
                crate::paths::display(from)
            ),
        ));
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_error("create", parent, e))?;
    }
    let _ = std::fs::copy(from, to).map_err(|e| io_error("copy", from, e))?;
    std::fs::set_permissions(to, std::fs::Permissions::from_mode(mode))
        .map_err(|e| io_error("chmod", to, e))
}

/// Every file under `dir`, relative and sorted.
pub fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).into_iter().flatten().flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => stack.push(path),
                Ok(_) => {
                    if let Ok(relative) = path.strip_prefix(dir) {
                        found.push(relative.to_path_buf());
                    }
                }
                Err(_) => {}
            }
        }
    }
    found.sort();
    found
}

/// The total size of the files under `dir`, in bytes.
pub fn size_under(dir: &Path) -> u64 {
    files_under(dir)
        .iter()
        .filter_map(|relative| std::fs::symlink_metadata(dir.join(relative)).ok())
        .map(|meta| meta.len())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_are_refused_with_the_reason() {
        let mac = Env::from_pairs(&[("ICM_HOST_OS", "macos")], None);
        assert!(require_host(&mac, ReleaseTarget::Macos, "release macos").is_ok());
        let error = require_host(&mac, ReleaseTarget::Windows, "release windows").unwrap_err();
        assert_eq!(error.id, "env.unsupported_host");
        assert!(error.detail.contains("on Windows"), "{}", error.detail);
        assert!(
            error.detail.contains("this host is macOS"),
            "{}",
            error.detail
        );
        assert!(error.fix.summary.contains("windows-latest"));
        assert!(
            error
                .fix
                .summary
                .contains("does not run on Windows hosts yet")
        );
        let error = require_host(&mac, ReleaseTarget::Linux, "release linux").unwrap_err();
        assert!(error.detail.contains("glibc"), "{}", error.detail);
        let linux = Env::from_pairs(&[("ICM_HOST_OS", "linux")], None);
        assert!(require_host(&linux, ReleaseTarget::Linux, "release linux").is_ok());
        assert!(require_host(&linux, ReleaseTarget::Macos, "release macos").is_err());
        // Other targets have no desktop host.
        assert!(require_host(&linux, ReleaseTarget::Web, "release web").is_ok());
        // Without the override: the real host.
        assert_eq!(host_os(&Env::default()), std::env::consts::OS);
    }

    #[test]
    fn categories_map_per_platform() {
        assert_eq!(
            macos_category("utilities").as_deref(),
            Some("public.app-category.utilities")
        );
        assert_eq!(
            macos_category("public.app-category.games").as_deref(),
            Some("public.app-category.games")
        );
        assert_eq!(macos_category("gadgets"), None);
        assert_eq!(linux_categories(Some("utilities")), "Utility;");
        assert_eq!(linux_categories(Some("finance")), "Office;Finance;");
        assert_eq!(linux_categories(None), "Utility;");
        assert_eq!(deb_section(Some("developer-tools")), "devel");
        assert_eq!(deb_section(None), "utils");
        assert_eq!(deb_section(Some("finance")), "misc");
        assert_eq!(file_stem("My: App?"), "My- App-");
        assert_eq!(file_stem("..."), "App");
    }

    #[test]
    fn files_are_listed_and_sized() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a/b.txt"), b"abc", 0o644).unwrap();
        write_file(&dir.path().join("c"), b"de", 0o755).unwrap();
        assert_eq!(
            files_under(dir.path()),
            [PathBuf::from("a/b.txt"), PathBuf::from("c")]
        );
        assert_eq!(size_under(dir.path()), 5);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("c"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
}
