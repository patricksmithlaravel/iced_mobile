//! The `.ipa` (design §11.1 step 9): the signed bundle copied with `ditto`
//! into `Payload/`, then zipped with `/usr/bin/zip -X`, which writes no
//! AppleDouble `._*` entries (a plain `ditto -c -k` would, because build
//! outputs carry `com.apple.provenance`). Entries are named in sorted order
//! and every file's time is set to `SOURCE_DATE_EPOCH`, else 1980-01-02
//! UTC (zip stores local time, and 1980-01-01 is before the zip epoch west
//! of UTC), so an unsigned (`--sign none`, ad-hoc) IPA is reproducible.

use crate::catalogue::CheckId;
use crate::context::Ctx;
use crate::error::{IcmError, Result};
use crate::process::Cmd;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The time icm gives every file in an archive without `SOURCE_DATE_EPOCH`.
pub const FALLBACK_EPOCH: u64 = 315_619_200;

/// Above this many bytes of names, icm lets zip walk the tree itself.
const ARGV_BUDGET: usize = 200_000;

fn io(what: &str, path: &Path, error: impl std::fmt::Display) -> IcmError {
    IcmError::new(
        CheckId::InternalBug,
        format!("cannot {what} {}: {error}", crate::paths::display(path)),
    )
}

/// Every path under `root` (directories with a trailing `/`), relative and
/// sorted, symlinks not followed.
pub fn entries(root: &Path) -> std::io::Result<Vec<String>> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let relative = path
                .strip_prefix(base)
                .map_err(std::io::Error::other)?
                .to_string_lossy()
                .into_owned();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                out.push(format!("{relative}/"));
                walk(base, &path, out)?;
            } else {
                out.push(relative);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    out.sort();
    Ok(out)
}

/// Sets every file's and directory's modification time under `root`.
pub fn set_times(root: &Path, epoch: u64) -> std::io::Result<()> {
    let time = UNIX_EPOCH + Duration::from_secs(epoch);
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            for entry in std::fs::read_dir(&path)? {
                stack.push(entry?.path());
            }
        }
        let file = std::fs::File::open(&path)?;
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(time)
                .set_accessed(SystemTime::now()),
        )?;
    }
    Ok(())
}

/// The archive time: `SOURCE_DATE_EPOCH`, else [`FALLBACK_EPOCH`].
pub fn epoch(ctx: &Ctx) -> u64 {
    ctx.env
        .var("SOURCE_DATE_EPOCH")
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(FALLBACK_EPOCH)
}

/// Zips the contents of `staging` (paths relative to it) into `out`.
pub fn zip_cmd(staging: &Path, names: &[String], out: &Path) -> Cmd {
    let mut cmd = Cmd::new("/usr/bin/zip").args(["-q", "-X", "-y"]).arg(out);
    let budget: usize = names.iter().map(|n| n.len() + 1).sum();
    if budget > ARGV_BUDGET {
        cmd = cmd.args(["-r", "Payload"]);
    } else {
        // zip adds a directory's own entry when it is named without -r.
        cmd = cmd.args(names.iter().map(|name| name.trim_end_matches('/')));
    }
    cmd.cwd(staging).timeout(Duration::from_secs(600))
}

/// Copies the signed bundle into `<staging>/Payload/` and zips it into
/// `ipa`. Nothing touches the bundle itself.
pub fn package(ctx: &Ctx, app: &Path, staging: &Path, ipa: &Path) -> Result<()> {
    let _ = std::fs::remove_dir_all(staging);
    let payload = staging.join("Payload");
    std::fs::create_dir_all(&payload).map_err(|e| io("create", &payload, e))?;
    let name = app
        .file_name()
        .ok_or_else(|| io("name", app, "no file name"))?;
    let copy = payload.join(name);
    let outcome = ctx.step(
        "ios.ipa.ditto",
        &Cmd::new("/usr/bin/ditto")
            .arg(app)
            .arg(&copy)
            .timeout(Duration::from_secs(300)),
    )?;
    if !outcome.success() {
        return Err(ctx.step_failure("ios.ipa.ditto", CheckId::ToolFailed, &outcome));
    }
    set_times(&payload, epoch(ctx)).map_err(|e| io("set the times in", &payload, e))?;
    let names = entries(staging).map_err(|e| io("list", staging, e))?;
    let _ = std::fs::remove_file(ipa);
    let outcome = ctx.step("ios.ipa.zip", &zip_cmd(staging, &names, ipa))?;
    if !outcome.success() {
        return Err(ctx.step_failure("ios.ipa.zip", CheckId::ToolFailed, &outcome));
    }
    Ok(())
}

/// What is wrong with an IPA's entry names (`ios.ipa.layout`): everything
/// must be inside `Payload/<one>.app/`, with no `__MACOSX/` or `._*`.
/// Returns the bundle's name and the problems.
pub fn layout(names: &[String]) -> (Option<String>, Vec<String>) {
    let mut problems = Vec::new();
    let mut apps: Vec<String> = Vec::new();
    for name in names {
        if name.starts_with("__MACOSX") {
            problems.push(format!("{name}: a __MACOSX/ entry"));
            continue;
        }
        if name.split('/').any(|part| part.starts_with("._")) {
            problems.push(format!("{name}: an AppleDouble ._ entry"));
            continue;
        }
        if name == "Payload/" {
            continue;
        }
        let Some(rest) = name.strip_prefix("Payload/") else {
            problems.push(format!("{name}: outside Payload/"));
            continue;
        };
        let app = rest.split('/').next().unwrap_or_default();
        if !app.ends_with(".app") {
            problems.push(format!("{name}: not inside Payload/<Name>.app/"));
            continue;
        }
        if !apps.iter().any(|a| a == app) {
            apps.push(app.to_string());
        }
    }
    match apps.len() {
        0 => problems.push("no Payload/<Name>.app/ in the archive".to_string()),
        1 => {}
        _ => problems.push(format!(
            "more than one app in Payload/: {}",
            apps.join(", ")
        )),
    }
    (apps.into_iter().next(), problems)
}

/// Unzips an IPA into `dest` (the extracted-signature gate).
pub fn unzip(ctx: &Ctx, ipa: &Path, dest: &Path) -> Result<PathBuf> {
    let _ = std::fs::remove_dir_all(dest);
    std::fs::create_dir_all(dest).map_err(|e| io("create", dest, e))?;
    let outcome = ctx.step(
        "ios.ipa.unzip",
        &Cmd::new("/usr/bin/unzip")
            .args(["-q", "-o"])
            .arg(ipa)
            .arg("-d")
            .arg(dest)
            .timeout(Duration::from_secs(300)),
    )?;
    if !outcome.success() {
        return Err(ctx.step_failure("ios.ipa.unzip", CheckId::IosIpaLayout, &outcome));
    }
    Ok(dest.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_are_judged() {
        let good: Vec<String> = [
            "Payload/",
            "Payload/Notes.app/",
            "Payload/Notes.app/Info.plist",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(layout(&good), (Some("Notes.app".to_string()), vec![]));
        let bad: Vec<String> = [
            "Payload/Notes.app/Info.plist",
            "Payload/Notes.app/._Info.plist",
            "__MACOSX/Payload/._Notes.app",
            "Symbols/x",
            "Payload/Other.app/x",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        let (_, problems) = layout(&bad);
        assert_eq!(problems.len(), 4, "{problems:?}");
        assert!(layout(&[]).1[0].contains("no Payload"));
    }

    #[test]
    fn entries_are_sorted_with_directories_and_times_are_fixed() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Payload/Notes.app");
        std::fs::create_dir_all(app.join("b")).unwrap();
        std::fs::write(app.join("z"), "z").unwrap();
        std::fs::write(app.join("b/a"), "a").unwrap();
        assert_eq!(
            entries(dir.path()).unwrap(),
            [
                "Payload/",
                "Payload/Notes.app/",
                "Payload/Notes.app/b/",
                "Payload/Notes.app/b/a",
                "Payload/Notes.app/z"
            ]
        );
        set_times(&dir.path().join("Payload"), FALLBACK_EPOCH).unwrap();
        let modified = std::fs::metadata(app.join("b/a"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(
            modified.duration_since(UNIX_EPOCH).unwrap().as_secs(),
            FALLBACK_EPOCH
        );
        let cmd = zip_cmd(
            dir.path(),
            &entries(dir.path()).unwrap(),
            Path::new("/o/N.ipa"),
        );
        assert_eq!(
            cmd.display_argv()[1..].join(" "),
            "-q -X -y /o/N.ipa Payload Payload/Notes.app Payload/Notes.app/b Payload/Notes.app/b/a Payload/Notes.app/z"
        );
    }
}
