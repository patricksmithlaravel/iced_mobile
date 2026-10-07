//! `target/icm/sessions/android.json`: what `run` left running, for `logs`,
//! `shot`, `input` and `stop` (design §3 "Sessions", §4.6). Android needs
//! no daemon: the device runs the app, and the session records the device,
//! the pid, the launch mark and the last screenshot's geometry.

use crate::context::Project;
use crate::screen::Screen;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The schema of the file.
pub const SCHEMA: &str = "icm.session.android/1";

/// The last screenshot's geometry (Appendix C item 25).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Geometry {
    /// Device pixels.
    pub px: (u32, u32),
    /// Pixels per dp.
    pub scale: f64,
    /// The preview's pixels.
    pub preview: (u32, u32),
}

impl Geometry {
    /// As a [`Screen`].
    pub fn screen(&self) -> Screen {
        let mut screen = Screen::new(self.px, self.scale);
        screen.preview = self.preview;
        screen
    }
}

impl From<&Screen> for Geometry {
    fn from(screen: &Screen) -> Self {
        Geometry {
            px: screen.px,
            scale: screen.scale,
            preview: screen.preview,
        }
    }
}

/// The session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// [`SCHEMA`].
    pub schema: String,
    /// The run that started it.
    pub run: String,
    /// That run's directory.
    pub run_dir: Option<PathBuf>,
    /// The device serial.
    pub serial: String,
    /// `emulator` or `device`.
    pub kind: String,
    /// The emulator's AVD.
    pub avd: Option<String>,
    /// Whether icm booted this emulator (so `stop --shutdown` stops it).
    pub booted_by_icm: bool,
    /// The emulator's pid, when icm booted it.
    pub emulator_pid: Option<u32>,
    /// The emulator's log, when icm booted it.
    pub emulator_log: Option<PathBuf>,
    /// The device's ABI.
    pub abi: String,
    /// The app id.
    pub app_id: String,
    /// The app's pid at launch, on the device. Written as `app_pid`: a
    /// top-level `pid` in a session file is a host process that `icm stop`
    /// and `icm ps` may signal or probe ([`crate::session`]).
    #[serde(rename = "app_pid", alias = "pid")]
    pub pid: Option<u32>,
    /// The device clock at launch (`seconds.nanoseconds`).
    pub log_mark: Option<String>,
    /// The installed APK.
    pub apk: Option<PathBuf>,
    /// The last screenshot's geometry.
    pub screen: Option<Geometry>,
    /// When the session started (RFC 3339).
    pub started: String,
}

/// The session file of a project.
pub fn path(project: &Project) -> PathBuf {
    project.sessions_dir().join("android.json")
}

/// Reads the session, if there is one.
pub fn read(project: &Project) -> Option<Session> {
    let text = std::fs::read_to_string(path(project)).ok()?;
    let session: Session = serde_json::from_str(&text).ok()?;
    (session.schema == SCHEMA).then_some(session)
}

/// Writes the session atomically.
pub fn write(project: &Project, session: &Session) -> std::io::Result<PathBuf> {
    let path = path(project);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string_pretty(session).map_err(std::io::Error::other)?;
    text.push('\n');
    crate::output::rundir::write_atomic(&path, text.as_bytes())?;
    Ok(path)
}

/// Removes the session file.
pub fn remove(project: &Project) {
    let _ = std::fs::remove_file(path(project));
}

/// An emulator icm booted for this project:
/// `target/icm/sessions/android-booted/<serial>.json`. It outlives the
/// session (`icm stop android` without `--shutdown` removes the session,
/// not the emulator), so a later run on the same emulator, and a later
/// `--shutdown`, still know icm started it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Booted {
    /// The serial, `emulator-<port>`.
    pub serial: String,
    /// The AVD.
    pub avd: String,
    /// The emulator's pid on the host.
    pub emulator_pid: Option<u32>,
    /// Its log.
    pub emulator_log: Option<PathBuf>,
}

impl Booted {
    /// Whether this emulator still runs: its recorded process is alive,
    /// so a port another emulator reuses is not taken for it.
    pub fn alive(&self) -> bool {
        self.emulator_pid
            .is_some_and(|pid| i32::try_from(pid).is_ok_and(crate::signals::alive))
    }
}

fn booted_dir(project: &Project) -> PathBuf {
    project.sessions_dir().join("android-booted")
}

/// Records an emulator icm booted.
pub fn write_booted(project: &Project, booted: &Booted) {
    let path = booted_dir(project).join(format!("{}.json", booted.serial));
    if let Ok(mut text) = serde_json::to_string_pretty(booted) {
        text.push('\n');
        let _ = crate::output::rundir::write_atomic(&path, text.as_bytes());
    }
}

/// The emulators icm booted for this project that still run.
pub fn booted(project: &Project) -> Vec<Booted> {
    let Ok(read) = std::fs::read_dir(booted_dir(project)) else {
        return Vec::new();
    };
    read.flatten()
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .filter_map(|text| serde_json::from_str::<Booted>(&text).ok())
        .filter(Booted::alive)
        .collect()
}

/// Forgets an emulator (it was shut down).
pub fn remove_booted(project: &Project, serial: &str) {
    let _ = std::fs::remove_file(booted_dir(project).join(format!("{serial}.json")));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_round_trips_through_screen() {
        let screen = Screen::new((1080, 2424), 2.625);
        let geometry = Geometry::from(&screen);
        assert_eq!(geometry.preview, (456, 1024));
        assert_eq!(geometry.screen(), screen);
        let text = serde_json::to_string(&Session {
            schema: SCHEMA.into(),
            screen: Some(geometry),
            ..Session::default()
        })
        .unwrap();
        let back: Session = serde_json::from_str(&text).unwrap();
        assert_eq!(back.screen, Some(geometry));
    }
}
