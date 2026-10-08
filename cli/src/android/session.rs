//! `target/icm/sessions/android.json`: what `run` left running, for `logs`,
//! `shot`, `input` and `stop` (design §3 "Sessions", §4.6). Android needs
//! no daemon: the device runs the app, and the session records the device,
//! the pid, the launch mark and the last screenshot's geometry.

use crate::context::Project;
use crate::procid::{self, Identity, Verdict};
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
    /// What tells that process from any other that has its pid later
    /// ([`crate::procid`]), read when icm started it. A session written
    /// before this has none, and its pid verifies nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emulator_identity: Option<Identity>,
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

/// The system property that names the project icm booted an emulator for,
/// set once it has booted. A project's own records live in its target
/// directory, where another project cannot see them; the property lives on
/// the emulator, so every icm that reaches the emulator can tell whose it
/// is, and `stop --shutdown` leaves another project's running.
pub const OWNER_PROP: &str = "debug.icm.booted_by";

/// This project's value for [`OWNER_PROP`]: the first 16 hex digits of the
/// SHA-256 of its sessions directory. Projects that share a target
/// directory share their sessions and booted records, and so this too.
pub fn owner_tag(project: &Project) -> String {
    tag_for(&project.sessions_dir())
}

fn tag_for(sessions_dir: &std::path::Path) -> String {
    crate::hash::sha256_hex(sessions_dir.to_string_lossy().as_bytes())[..16].to_string()
}

/// What a recorded emulator process is now. A pid says only that some
/// process has the number: once the emulator has exited, any other can have
/// it, and the serial can hold another emulator, so a record counts as the
/// emulator on its serial only while its process is [`Process::Verified`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Process {
    /// The process icm started, still running: its pid and the identity
    /// that was recorded for it, which a signal re-checks.
    Verified {
        /// The emulator's pid on the host.
        pid: u32,
        /// What the pid was checked against.
        identity: Identity,
    },
    /// The process has exited, or another process has its pid: the serial
    /// may hold any emulator now.
    Gone,
    /// The record cannot say: it holds no pid, or no identity to compare
    /// the pid's process with (an older icm wrote it), or the OS would not
    /// describe that process. The pid is never signalled, and the record
    /// is no proof that the emulator on the serial is this project's.
    Unverified,
}

/// What the process a record names is now: [`Process::Verified`] only when
/// the record holds an identity and the pid still has that process.
pub fn process(pid: Option<u32>, identity: Option<&Identity>) -> Process {
    let Some(pid) = pid else {
        return Process::Unverified;
    };
    let Ok(signed) = i32::try_from(pid) else {
        return Process::Unverified;
    };
    match identity {
        Some(identity) => match procid::check(signed, identity) {
            Verdict::Same => Process::Verified {
                pid,
                identity: identity.clone(),
            },
            Verdict::Gone | Verdict::Other(_) => Process::Gone,
            Verdict::Unknown(_) => Process::Unverified,
        },
        // Nothing to compare: a pid with no process is the emulator gone,
        // any other might be anything.
        None if crate::signals::alive(signed) => Process::Unverified,
        None => Process::Gone,
    }
}

impl Session {
    /// What the emulator process this session recorded is now.
    pub fn emulator_process(&self) -> Process {
        process(self.emulator_pid, self.emulator_identity.as_ref())
    }
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
    /// What tells that process from any other that has its pid later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emulator_identity: Option<Identity>,
    /// Its log.
    pub emulator_log: Option<PathBuf>,
}

impl Booted {
    /// What the emulator process this record names is now.
    pub fn process(&self) -> Process {
        process(self.emulator_pid, self.emulator_identity.as_ref())
    }

    /// Whether this emulator still runs, as the process icm started: a
    /// pid that another process has taken, or one icm cannot compare with
    /// what it recorded, is not that emulator.
    pub fn verified(&self) -> bool {
        matches!(self.process(), Process::Verified { .. })
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

/// The emulators icm booted for this project whose records may still hold:
/// every record except those whose process has exited or whose pid another
/// process has taken. [`Booted::verified`] tells the ones that are known to
/// run from the ones an older icm wrote.
pub fn booted(project: &Project) -> Vec<Booted> {
    let Ok(read) = std::fs::read_dir(booted_dir(project)) else {
        return Vec::new();
    };
    read.flatten()
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .filter_map(|text| serde_json::from_str::<Booted>(&text).ok())
        .filter(|booted| booted.process() != Process::Gone)
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
    fn owner_tags_fit_a_property_and_tell_projects_apart() {
        let a = tag_for(std::path::Path::new("/work/a/target/icm/sessions"));
        let b = tag_for(std::path::Path::new("/work/b/target/icm/sessions"));
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert_eq!(
            a,
            tag_for(std::path::Path::new("/work/a/target/icm/sessions"))
        );
    }

    /// A record's emulator process counts only while its pid still has the
    /// process icm recorded: another process's pid, an exited one and a pid
    /// with no identity to compare are never the emulator.
    #[test]
    fn a_recorded_process_is_the_emulator_only_while_it_is_verified() {
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let identity = procid::of(pid as i32).unwrap();
        assert_eq!(
            process(Some(pid), Some(&identity)),
            Process::Verified {
                pid,
                identity: identity.clone()
            }
        );
        // The pid of a live process the record has no identity for.
        assert_eq!(process(Some(pid), None), Process::Unverified);
        assert_eq!(process(None, Some(&identity)), Process::Unverified);
        assert_eq!(process(None, None), Process::Unverified);
        // Another process's start time.
        let other = Identity {
            start: "1791334000.000001".to_string(),
            exe: String::new(),
        };
        assert_eq!(process(Some(pid), Some(&other)), Process::Gone);

        child.kill().unwrap();
        let _ = child.wait().unwrap();
        assert_eq!(process(Some(pid), Some(&identity)), Process::Gone);
        assert_eq!(process(Some(pid), None), Process::Gone);

        let booted = Booted {
            serial: "emulator-5580".to_string(),
            avd: "icm-api36".to_string(),
            emulator_pid: Some(std::process::id()),
            emulator_identity: procid::of(std::process::id() as i32),
            emulator_log: None,
        };
        assert!(booted.verified());
        // A record an older icm wrote, whose pid is alive, is not.
        let older: Booted = serde_json::from_str(
            &serde_json::to_string(&Booted {
                emulator_identity: None,
                ..booted.clone()
            })
            .unwrap(),
        )
        .unwrap();
        assert!(!older.verified());
        assert_eq!(older.process(), Process::Unverified);
    }

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
