//! Well-known directories and path display.

use std::path::{Component, Path, PathBuf};

/// The user's home directory.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// icm's cache directory: `ICM_CACHE_DIR`, else `~/Library/Caches/icm` on
/// macOS and `$XDG_CACHE_HOME/icm` (or `~/.cache/icm`) elsewhere. Pinned
/// tools and the runs of commands outside a project live here.
pub fn cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ICM_CACHE_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }

    if cfg!(target_os = "macos")
        && let Some(home) = home()
    {
        return home.join("Library").join("Caches").join("icm");
    }

    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(xdg).join("icm");
    }

    home()
        .map(|home| home.join(".cache").join("icm"))
        .unwrap_or_else(|| std::env::temp_dir().join("icm-cache"))
}

/// Where pinned external tools are installed: `<cache>/tools`.
pub fn tools_dir() -> PathBuf {
    cache_dir().join("tools")
}

/// The machine config file: `ICM_HOST_CONFIG`, else
/// `$XDG_CONFIG_HOME/icm/host.toml`, else `~/.config/icm/host.toml`.
pub fn host_config() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ICM_HOST_CONFIG").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }

    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(xdg).join("icm").join("host.toml"));
    }

    home().map(|home| home.join(".config").join("icm").join("host.toml"))
}

/// A path for output: relative to the current directory when it is inside
/// it, absolute otherwise.
pub fn display(path: &Path) -> String {
    let Ok(cwd) = std::env::current_dir() else {
        return path.display().to_string();
    };
    display_from(path, &cwd)
}

/// [`display`] against an explicit base directory.
pub fn display_from(path: &Path, base: &Path) -> String {
    let absolute = if path.is_absolute() {
        normalize(path)
    } else {
        normalize(&base.join(path))
    };

    let relative_to = |path: &Path, base: &Path| -> Option<String> {
        let relative = path.strip_prefix(base).ok()?;
        Some(if relative.as_os_str().is_empty() {
            ".".to_string()
        } else {
            relative.display().to_string()
        })
    };

    if let Some(relative) = relative_to(&absolute, &normalize(base)) {
        return relative;
    }

    // The same directory under another name (macOS: /var is /private/var).
    if let (Some(path), Ok(base)) = (canonical_prefix(&absolute), std::fs::canonicalize(base))
        && let Some(relative) = relative_to(&path, &base)
    {
        return relative;
    }

    absolute.display().to_string()
}

/// Canonicalizes the longest existing ancestor and appends the rest.
fn canonical_prefix(path: &Path) -> Option<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(&existing) {
            let mut out = canonical;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return Some(out);
        }
        rest.push(existing.file_name()?.to_os_string());
        if !existing.pop() {
            return None;
        }
    }
}

/// Removes `.` and resolves `..` lexically (no filesystem access).
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Expands a leading `~/`.
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = home()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

/// Searches `PATH` for an executable.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

/// Whether a path is an executable file.
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_inside_the_base_are_relative() {
        let base = Path::new("/work/app");
        assert_eq!(
            display_from(Path::new("/work/app/target/icm/last.json"), base),
            "target/icm/last.json"
        );
        assert_eq!(
            display_from(Path::new("/elsewhere/x"), base),
            "/elsewhere/x"
        );
        assert_eq!(display_from(Path::new("target/./icm"), base), "target/icm");
        assert_eq!(display_from(Path::new("/work/app"), base), ".");
        assert_eq!(
            display_from(Path::new("/work/app/../lib/x"), base),
            "/work/lib/x"
        );
    }

    #[test]
    fn symlinked_bases_still_give_relative_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(real.join("target/icm")).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(
            display_from(&link.join("target/icm/new.json"), &real),
            "target/icm/new.json"
        );
        assert_eq!(display_from(&real.join("target/icm"), &link), "target/icm");
    }
}
