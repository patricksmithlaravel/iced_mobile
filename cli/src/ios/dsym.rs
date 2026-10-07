//! The dSYM of an App Store build (design §11.1 step 2, Appendix C item
//! 23): `xcrun dsymutil` before the bundled copy is stripped, then two
//! gates. Its UUID must equal the executable's (both read from `LC_UUID`),
//! and its line table must name one of the app crate's own source files:
//! an empty dSYM has the right UUID too, so only the line table proves the
//! crash reports will symbolicate. Release builds keep line tables
//! (`profile.release.debug="line-tables-only"`, `release/compile.rs`), and
//! dsymutil finds them through the object files cargo leaves in `deps/`.

use crate::process::Cmd;
use crate::tools::Xcode;
use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `xcrun dsymutil <exe> -o <dSYM>`.
pub fn dsymutil_cmd(xcode: &Xcode, exe: &Path, out: &Path) -> Cmd {
    xcode
        .xcrun()
        .arg("dsymutil")
        .arg(exe)
        .arg("-o")
        .arg(out)
        .timeout(Duration::from_secs(600))
}

/// `xcrun dwarfdump --debug-line <dSYM> -o <file>` (tens of MB of text, so
/// into a file icm streams).
pub fn line_table_cmd(xcode: &Xcode, dsym: &Path, out: &Path) -> Cmd {
    xcode
        .xcrun()
        .args(["dwarfdump", "--debug-line"])
        .arg(dsym)
        .arg("-o")
        .arg(out)
        .timeout(Duration::from_secs(300))
}

/// The DWARF file inside a dSYM bundle (`Contents/Resources/DWARF/<bin>`).
pub fn dwarf_file(dsym: &Path) -> Option<PathBuf> {
    let dir = dsym.join("Contents/Resources/DWARF");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    files.sort();
    files.into_iter().next()
}

/// The first source file of the crate (a file under `src_dir`) that the
/// line table in `dump` (`dwarfdump --debug-line` output) names.
///
/// File entries are a name and a directory index into the prologue's
/// `include_directories`; rustc writes the files of workspace members
/// relative to the workspace root (the compilation directory, which DWARF 4
/// line tables do not record), so a relative path counts when it resolves
/// against one of `src_dir`'s ancestors to an existing file under it.
pub fn crate_source(dump: &Path, src_dir: &Path) -> std::io::Result<Option<PathBuf>> {
    let file = std::fs::File::open(dump)?;
    let mut dirs: BTreeMap<u64, String> = BTreeMap::new();
    let mut name: Option<String> = None;
    for line in std::io::BufReader::new(file).lines() {
        let line = line?;
        let line = line.trim();
        if line.starts_with("Line table prologue:") {
            dirs.clear();
            name = None;
        } else if let Some(rest) = line.strip_prefix("include_directories[") {
            if let Some((index, value)) = rest.split_once("] = ")
                && let Ok(index) = index.trim().parse::<u64>()
            {
                let _ = dirs.insert(index, unquote(value));
            }
        } else if let Some(value) = line.strip_prefix("name: ") {
            name = Some(unquote(value));
        } else if let Some(value) = line.strip_prefix("dir_index: ")
            && let Some(file_name) = name.take()
        {
            let dir = value
                .trim()
                .parse::<u64>()
                .ok()
                .and_then(|index| dirs.get(&index))
                .cloned()
                .unwrap_or_default();
            if let Some(found) = resolve(&dir, &file_name, src_dir) {
                return Ok(Some(found));
            }
        }
    }
    Ok(None)
}

fn unquote(text: &str) -> String {
    text.trim().trim_matches('"').to_string()
}

fn resolve(dir: &str, name: &str, src_dir: &Path) -> Option<PathBuf> {
    if !name.ends_with(".rs") {
        return None;
    }
    let relative = Path::new(dir).join(name);
    if relative.is_absolute() {
        return (relative.starts_with(src_dir) && relative.is_file()).then_some(relative);
    }
    src_dir.ancestors().find_map(|base| {
        let candidate = base.join(&relative);
        (candidate.starts_with(src_dir) && candidate.is_file()).then_some(candidate)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_table_must_name_the_crates_sources() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let src = root.join("examples/app/src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "").unwrap();
        let dump = root.join("lines.txt");
        let table = |dirs: &str| {
            format!(
                "debug_line[0x00000000]\nLine table prologue:\n    version: 4\n{dirs}file_names[  1]:\n           name: \"mod.rs\"\n      dir_index: 1\nfile_names[  2]:\n           name: \"lib.rs\"\n      dir_index: 2\n"
            )
        };

        // Relative to the workspace root, as rustc writes a member's files.
        std::fs::write(
            &dump,
            table("include_directories[  1] = \"library/core/src/ptr\"\ninclude_directories[  2] = \"examples/app/src\"\n"),
        )
        .unwrap();
        assert_eq!(crate_source(&dump, &src).unwrap(), Some(src.join("lib.rs")));

        // Absolute, as for a path dependency.
        std::fs::write(
            &dump,
            table(&format!(
                "include_directories[  1] = \"/x\"\ninclude_directories[  2] = \"{}\"\n",
                src.display()
            )),
        )
        .unwrap();
        assert_eq!(crate_source(&dump, &src).unwrap(), Some(src.join("lib.rs")));

        // Only other crates' files (an empty or foreign line table).
        std::fs::write(
            &dump,
            table("include_directories[  1] = \"library/core/src/ptr\"\ninclude_directories[  2] = \"core/src\"\n"),
        )
        .unwrap();
        assert_eq!(crate_source(&dump, &src).unwrap(), None);
        std::fs::write(&dump, "").unwrap();
        assert_eq!(crate_source(&dump, &src).unwrap(), None);
    }
}
