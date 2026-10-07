//! `target/icm/sessions/ios-sim.json`: what `icm run ios-sim` left running
//! (design §3 "Sessions", §4.6), so `logs`, `shot` and `stop` find the
//! simulator, the app, the launch mark and the live log files.
//!
//! The app's live files live in `sessions/ios-sim/<run-id>/`, not in the
//! run directory, so pruning old runs never removes files the app is
//! still writing. `run` copies a snapshot into its run directory.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The session file's schema.
pub const SCHEMA: &str = "icm.session/1";

/// The simulator a session runs on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDevice {
    /// Its UDID.
    pub udid: String,
    /// Its name.
    pub name: String,
    /// Its iOS version.
    pub os: String,
    /// Its device type, e.g. `iPhone 17`.
    #[serde(rename = "type")]
    pub device_type: String,
    /// Whether icm created it (`icm-` prefix): `stop --shutdown` may shut it down.
    pub managed: bool,
    /// Whether `--fresh` created it: `stop` deletes it.
    pub fresh: bool,
    /// The host directory holding its data.
    pub data_path: Option<PathBuf>,
}

/// The live log files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLogs {
    /// The app's stdout.
    pub stdout: PathBuf,
    /// The app's stderr.
    pub stderr: PathBuf,
    /// The unified-log collector's output.
    pub oslog: PathBuf,
}

/// A session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// [`SCHEMA`].
    pub schema: String,
    /// `ios-sim`.
    pub platform: String,
    /// The run that started it.
    pub run: String,
    /// That run's directory.
    pub run_dir: Option<PathBuf>,
    /// `running` or `stopped`.
    pub state: String,
    /// The simulator.
    pub device: SessionDevice,
    /// The bundle id.
    pub app_id: String,
    /// The executable's name (the unified log's process name).
    pub exe: String,
    /// The installed bundle.
    pub bundle: PathBuf,
    /// The app's pid on the host, once launched.
    pub pid: Option<i64>,
    /// The launch mark, milliseconds since the epoch (host clock).
    pub launch_unix_ms: i64,
    /// The live log files.
    pub logs: SessionLogs,
    /// The `log stream` collector's pid (its own process group).
    pub collector_pid: Option<i32>,
    /// The result's `screen` object from the last screenshot.
    #[serde(default)]
    pub screen: Option<serde_json::Value>,
}

/// `<icm>/sessions/ios-sim.json`.
pub fn path(sessions_dir: &Path) -> PathBuf {
    sessions_dir.join("ios-sim.json")
}

/// `<icm>/sessions/ios-sim/<run>/`, where the live files go.
pub fn files_dir(sessions_dir: &Path, run: &str) -> PathBuf {
    sessions_dir.join("ios-sim").join(run)
}

impl Session {
    /// Reads the session, if there is one.
    pub fn read(sessions_dir: &Path) -> Option<Session> {
        let text = std::fs::read_to_string(path(sessions_dir)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Writes it atomically.
    pub fn write(&self, sessions_dir: &Path) -> std::io::Result<()> {
        let mut text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        text.push('\n');
        crate::output::rundir::write_atomic(&path(sessions_dir), text.as_bytes())
    }

    /// Whether the app's process is alive on the host (simulator apps are
    /// host processes).
    pub fn app_alive(&self) -> bool {
        self.pid
            .and_then(|pid| i32::try_from(pid).ok())
            .is_some_and(crate::signals::alive)
    }
}

/// Removes the live-file directories of runs other than `keep`.
pub fn prune_files(sessions_dir: &Path, keep: &str) {
    let Ok(read) = std::fs::read_dir(sessions_dir.join("ios-sim")) else {
        return;
    };
    for entry in read.flatten() {
        if entry.file_name().to_string_lossy() != keep
            && entry.file_type().is_ok_and(|t| t.is_dir())
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Session::read(dir.path()).is_none());
        let files = files_dir(dir.path(), "run-a");
        let session = Session {
            schema: SCHEMA.into(),
            platform: "ios-sim".into(),
            run: "run-a".into(),
            run_dir: None,
            state: "running".into(),
            device: SessionDevice {
                udid: "AAA".into(),
                name: "icm-iphone-17-ios-27.0".into(),
                os: "27.0".into(),
                device_type: "iPhone 17".into(),
                managed: true,
                fresh: false,
                data_path: None,
            },
            app_id: "com.example.app".into(),
            exe: "app".into(),
            bundle: "/b/App.app".into(),
            pid: Some(i64::from(std::process::id())),
            launch_unix_ms: 1,
            logs: SessionLogs {
                stdout: files.join("app.stdout"),
                stderr: files.join("app.stderr"),
                oslog: files.join("oslog.ndjson"),
            },
            collector_pid: None,
            screen: None,
        };
        session.write(dir.path()).unwrap();
        let read = Session::read(dir.path()).unwrap();
        assert_eq!(read, session);
        assert!(read.app_alive());
        let text = std::fs::read_to_string(path(dir.path())).unwrap();
        assert!(text.contains("\"type\": \"iPhone 17\""));

        std::fs::create_dir_all(files_dir(dir.path(), "run-a")).unwrap();
        std::fs::create_dir_all(files_dir(dir.path(), "run-old")).unwrap();
        prune_files(dir.path(), "run-a");
        assert!(files_dir(dir.path(), "run-a").exists());
        assert!(!files_dir(dir.path(), "run-old").exists());
    }
}
