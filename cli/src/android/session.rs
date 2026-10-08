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
    /// ([`crate::procid`]), read when icm started it, or the reason icm
    /// could not read it ([`Identity::unavailable`]). A session written
    /// before this has none. Neither verifies its pid.
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
    /// The process has exited, or another process has its pid. That is all
    /// icm knows: whether the emulator on the serial is still the one it
    /// started, a later one or somebody else's, only the device's owner
    /// property says, so it decides as for [`Process::Unverified`]. The pid
    /// is never signalled.
    Gone,
    /// The record cannot say: it holds no pid, or no identity to compare
    /// the pid's process with (an older icm wrote it, or icm could not read
    /// the identity when it started the emulator: [`Identity::unavailable`]),
    /// or the OS would not describe that process. The pid is never
    /// signalled, and the record is no proof that the emulator on the
    /// serial is this project's.
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

    /// The emulator icm booted that this session runs on, as its record:
    /// `None` for a session on a device icm did not boot.
    pub fn booted(&self) -> Option<Booted> {
        self.booted_by_icm.then(|| Booted {
            serial: self.serial.clone(),
            avd: self.avd.clone().unwrap_or_default(),
            emulator_pid: self.emulator_pid,
            emulator_identity: self.emulator_identity.clone(),
            emulator_log: self.emulator_log.clone(),
        })
    }

    /// Records that the session runs on `booted`, an emulator icm booted,
    /// with the process and the identity to check it by.
    pub fn run_on(&mut self, booted: &Booted) {
        self.booted_by_icm = true;
        self.emulator_pid = booted.emulator_pid;
        self.emulator_identity = booted.emulator_identity.clone();
        self.emulator_log = booted.emulator_log.clone();
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
    /// The record of the emulator icm has just started, with the identity
    /// of its process read right after it started.
    pub fn of(booting: &super::avd::Booting) -> Booted {
        Booted {
            serial: booting.serial.clone(),
            avd: booting.avd.clone(),
            emulator_pid: Some(booting.pid),
            emulator_identity: booting.identity.clone(),
            emulator_log: Some(booting.log.clone()),
        }
    }

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

/// The emulators icm booted for this project that have a record, whatever
/// their recorded process is now ([`Booted::process`]): [`Booted::verified`]
/// tells the ones that are known to run from the rest. A record whose
/// process has ended or was replaced still names a serial that may hold the
/// emulator, so `stop` reads that device's owner; a rerun carries forward
/// only the verified ones.
pub fn booted(project: &Project) -> Vec<Booted> {
    let Ok(read) = std::fs::read_dir(booted_dir(project)) else {
        return Vec::new();
    };
    read.flatten()
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .filter_map(|text| serde_json::from_str::<Booted>(&text).ok())
        .collect()
}

/// Forgets an emulator (it was shut down).
pub fn remove_booted(project: &Project, serial: &str) {
    let _ = std::fs::remove_file(booted_dir(project).join(format!("{serial}.json")));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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
            unavailable: None,
        };
        assert_eq!(process(Some(pid), Some(&other)), Process::Gone);
        // An identity icm could not read when it started the process is
        // no identity to compare either, as for a record that has none:
        // the live pid is unverified, never the emulator.
        let unread = Identity::unavailable("proc_pidinfo: Operation not permitted");
        assert_eq!(process(Some(pid), Some(&unread)), Process::Unverified);

        child.kill().unwrap();
        let _ = child.wait().unwrap();
        assert_eq!(process(Some(pid), Some(&identity)), Process::Gone);
        assert_eq!(process(Some(pid), None), Process::Gone);
        assert_eq!(process(Some(pid), Some(&unread)), Process::Gone);

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

    /// The identity icm reads when it starts an emulator is carried into
    /// the per-serial record and the session, and out of a session again
    /// when a rerun carries the record forward: without it a record is
    /// unverifiable.
    #[test]
    fn the_emulators_identity_is_carried_from_the_boot_to_the_next_run() {
        let me = std::process::id();
        let booting = super::super::avd::Booting {
            avd: "icm-api36".to_string(),
            serial: "emulator-5580".to_string(),
            port: 5580,
            pid: me,
            identity: procid::of(me as i32),
            log: PathBuf::from("/runs/r1/emulator.log"),
        };
        assert!(booting.identity.is_some());

        let booted = Booted::of(&booting);
        assert_eq!(booted.serial, "emulator-5580");
        assert_eq!(booted.avd, "icm-api36");
        assert_eq!(booted.emulator_pid, Some(me));
        assert_eq!(booted.emulator_identity, booting.identity);
        assert_eq!(
            booted.emulator_log.as_deref(),
            Some(Path::new("/runs/r1/emulator.log"))
        );
        assert!(booted.verified());

        let mut session = Session {
            schema: SCHEMA.to_string(),
            serial: booted.serial.clone(),
            avd: Some(booted.avd.clone()),
            ..Session::default()
        };
        assert_eq!(session.booted(), None, "a device icm did not boot");
        session.run_on(&booted);
        assert!(session.booted_by_icm);
        assert_eq!(session.emulator_pid, Some(me));
        assert_eq!(session.emulator_identity, booting.identity);
        assert_eq!(session.emulator_log, booted.emulator_log);
        assert!(matches!(
            session.emulator_process(),
            Process::Verified { .. }
        ));

        // What the next run reads back from the session file.
        let text = serde_json::to_string(&session).unwrap();
        let read: Session = serde_json::from_str(&text).unwrap();
        assert_eq!(read.booted().as_ref(), Some(&booted));
        assert!(read.booted().unwrap().verified());
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
