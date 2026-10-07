//! Run directories (design §4.6): `runs/<run-id>/` with `events.ndjson`,
//! `result.json` and `steps/`, plus `last.json` and `latest/<platform>`.

use crate::time::Utc;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// How many run directories are kept.
pub const KEEP_RUNS: usize = 30;

/// A new run id: `YYYYMMDDTHHMMSSZ-<cmd>[-<target>]-<4 hex>`. Ids sort by
/// time.
pub fn new_run_id(command: &str, target: Option<&str>) -> String {
    let now = SystemTime::now();
    let nanos = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    let mixed = (u64::from(nanos) ^ u64::from(std::process::id()).wrapping_mul(2_654_435_761))
        .wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let hex = (mixed >> 48) as u16;

    let mut id = Utc::from_system(now).stamp();
    id.push('-');
    id.push_str(&sanitize(command));
    if let Some(target) = target {
        id.push('-');
        id.push_str(&sanitize(target));
    }
    id.push_str(&format!("-{hex:04x}"));
    id
}

fn sanitize(part: &str) -> String {
    let cleaned: String = part
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "x".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Whether a string has the shape of a run id.
pub fn is_run_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    id.len() >= 22
        && bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[8] == b'T'
        && bytes[9..15].iter().all(u8::is_ascii_digit)
        && bytes[15] == b'Z'
        && bytes[16] == b'-'
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The runs directory under an icm root (`target/icm` or the cache).
pub fn runs_dir(root: &Path) -> PathBuf {
    root.join("runs")
}

/// Writes a file atomically (temp file + rename).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Points `<root>/latest/<platform>` at a run directory (a relative symlink).
pub fn link_latest(root: &Path, platform: &str, run_dir: &Path) -> io::Result<()> {
    let latest = root.join("latest");
    std::fs::create_dir_all(&latest)?;

    let target = match run_dir.strip_prefix(root) {
        Ok(relative) => Path::new("..").join(relative),
        Err(_) => run_dir.to_path_buf(),
    };

    let link = latest.join(platform);
    let tmp = latest.join(format!(".{platform}.tmp-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(&target, &tmp)?;
    std::fs::rename(&tmp, &link)
}

/// The file naming the icm that writes a run directory (`{"pid": N}`).
pub const OWNER: &str = "owner.json";

/// Records this icm as the run directory's writer, so `prune` in another
/// icm leaves the directory alone while this one still runs.
pub fn write_owner(dir: &Path) -> io::Result<()> {
    write_atomic(
        &dir.join(OWNER),
        format!("{{\"pid\":{}}}\n", std::process::id()).as_bytes(),
    )
}

/// Removes run directories beyond the newest `keep`. Never removes the
/// current one, one whose icm (detached or not) is still running, or one
/// that a session under `<root>/sessions` names (an app launched by that
/// run may still use files there).
pub fn prune(root: &Path, keep: usize, current: &str) -> io::Result<usize> {
    let runs = runs_dir(root);
    let Ok(read_dir) = std::fs::read_dir(&runs) else {
        return Ok(0);
    };

    let mut ids: Vec<String> = read_dir
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| is_run_id(name))
        .collect();
    ids.sort();
    ids.reverse();

    let sessions = session_runs(&root.join("sessions"));
    let mut removed = 0;
    for id in ids.into_iter().skip(keep) {
        if id == current || sessions.contains(&id) {
            continue;
        }
        let dir = runs.join(&id);
        if in_progress(&dir) {
            continue;
        }
        if std::fs::remove_dir_all(&dir).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// The runs that the session records in `sessions_dir` name (`run`).
fn session_runs(sessions_dir: &Path) -> Vec<String> {
    let Ok(read_dir) = std::fs::read_dir(sessions_dir) else {
        return Vec::new();
    };
    read_dir
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .filter_map(|value| value.get("run")?.as_str().map(str::to_string))
        .collect()
}

/// Whether a run has not finished and the icm writing it still runs.
fn in_progress(dir: &Path) -> bool {
    if dir.join("result.json").exists() {
        return false;
    }
    let owner = std::fs::read_to_string(dir.join(OWNER))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| value.get("pid")?.as_i64())
        .map(|pid| pid as i32);
    detached_alive(dir) || owner.is_some_and(crate::signals::alive)
}

/// Whether a run directory belongs to a detached icm that is still running.
pub fn detached_alive(dir: &Path) -> bool {
    if dir.join("result.json").exists() {
        return false;
    }
    detached_pid(dir).is_some_and(crate::signals::alive)
}

/// The pid of a run's detached icm, from `detached.json`.
pub fn detached_pid(dir: &Path) -> Option<i32> {
    let text = std::fs::read_to_string(dir.join("detached.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("pid")?.as_i64().map(|pid| pid as i32)
}

/// Finds a run directory by id (in any of `roots`) or by path.
pub fn find_run(roots: &[PathBuf], id_or_path: &str) -> Option<PathBuf> {
    let as_path = Path::new(id_or_path);
    if as_path.join("events.ndjson").exists() || as_path.join("detached.json").exists() {
        return Some(as_path.to_path_buf());
    }

    roots
        .iter()
        .map(|root| runs_dir(root).join(id_or_path))
        .find(|dir| dir.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_ids_have_the_documented_shape() {
        let id = new_run_id("run", Some("ios-sim"));
        assert!(is_run_id(&id), "{id}");
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts[1], "run");
        assert_eq!(parts[2], "ios");
        assert_eq!(parts[3], "sim");
        assert_eq!(parts[4].len(), 4);
        assert!(is_run_id(&new_run_id("explain", None)));
        assert!(!is_run_id("../../etc"));
        assert!(!is_run_id("20261006T210311Z"));
    }

    #[test]
    fn latest_links_and_pruning() {
        let root = tempfile::tempdir().unwrap();
        let runs = runs_dir(root.path());
        for i in 0..5 {
            let dir = runs.join(format!("2026100{i}T000000Z-run-web-000{i}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("result.json"), "{}").unwrap();
        }
        let newest = runs.join("20261004T000000Z-run-web-0004");
        link_latest(root.path(), "web", &newest).unwrap();
        let link = root.path().join("latest").join("web");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            Path::new("../runs/20261004T000000Z-run-web-0004")
        );
        assert!(link.join("result.json").exists());

        // A running detached run is kept even when old.
        let running = runs.join("20261000T000000Z-run-web-0000");
        std::fs::remove_file(running.join("result.json")).unwrap();
        std::fs::write(
            running.join("detached.json"),
            format!("{{\"pid\":{}}}", std::process::id()),
        )
        .unwrap();

        // So is a foreground run whose icm still runs, and a run that a
        // session names (its app may still write there).
        let foreground = runs.join("20261001T000000Z-run-web-0001");
        std::fs::remove_file(foreground.join("result.json")).unwrap();
        write_owner(&foreground).unwrap();
        let sessions = root.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join("desktop.json"),
            r#"{"platform":"desktop","run":"20261002T000000Z-run-web-0002"}"#,
        )
        .unwrap();

        let removed = prune(root.path(), 2, "20261003T000000Z-run-web-0003").unwrap();
        assert_eq!(removed, 0);
        assert!(running.exists());
        assert!(foreground.exists());
        assert!(runs.join("20261002T000000Z-run-web-0002").exists());

        std::fs::write(foreground.join("result.json"), "{}").unwrap();
        std::fs::remove_file(sessions.join("desktop.json")).unwrap();
        let removed = prune(root.path(), 2, "20261003T000000Z-run-web-0003").unwrap();
        assert_eq!(removed, 2);
        assert!(running.exists());
        assert!(runs.join("20261003T000000Z-run-web-0003").exists());
        assert!(runs.join("20261004T000000Z-run-web-0004").exists());
        assert!(!runs.join("20261001T000000Z-run-web-0001").exists());

        assert_eq!(
            find_run(
                &[root.path().to_path_buf()],
                "20261004T000000Z-run-web-0004"
            ),
            Some(newest)
        );
        assert_eq!(find_run(&[root.path().to_path_buf()], "nope"), None);
    }
}
