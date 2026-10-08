//! AVDs and emulators (design §10.4 step 1, Appendix C item 10).
//!
//! - icm manages AVDs whose names start with `icm-`; the default is
//!   `icm-api<target_sdk>`, made from the system image for the host's ABI
//!   (arm64-v8a on Apple Silicon, x86_64 elsewhere). It never creates,
//!   boots-with-changes or deletes any other AVD.
//! - Emulators boot detached on the first free even port of host.toml's
//!   `android.emulator_ports` (default 5580, 5582, 5584), headless with
//!   `-gpu swiftshader_indirect` ([`DEFAULT_HEADLESS_GPU`]; host.toml
//!   `android.emulator_gpu` overrides it); `--show` gives a window and
//!   `-gpu auto`.
//! - Boot ends at `sys.boot_completed=1` and a package manager that
//!   answers.

use super::Toolset;
use super::adb::{self, Adb};
use crate::catalogue::CheckId;
use crate::config::Abi;
use crate::context::Ctx;
use crate::error::{Evidence, IcmError, Result};
use crate::process;
use crate::procid::{Identity, Verdict};
use crate::tools::{AndroidSdk, Env};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The prefix of every AVD icm creates or may shut down unasked.
pub const MANAGED_PREFIX: &str = "icm-";

/// The device profile of managed AVDs (a 1080x2424 phone at 420 dpi).
pub const DEVICE_PROFILE: &str = "pixel_9";

/// System image tags, in order of preference.
pub const IMAGE_TAGS: &[&str] = &[
    "google_apis",
    "google_apis_playstore",
    "default",
    "google_atd",
    "aosp_atd",
];

/// How long a cold boot may take.
pub const BOOT_TIMEOUT: Duration = Duration::from_secs(300);

/// The managed AVD's name for a target SDK.
pub fn managed_name(target_sdk: u32) -> String {
    format!("{MANAGED_PREFIX}api{target_sdk}")
}

/// Whether icm owns an AVD (and may create or shut it down).
pub fn is_managed(name: &str) -> bool {
    name.starts_with(MANAGED_PREFIX)
}

/// The ABI of the host's emulator images.
pub fn host_abi() -> Abi {
    if std::env::consts::ARCH == "aarch64" {
        Abi::Arm64V8a
    } else {
        Abi::X86_64
    }
}

/// Where AVDs live: `$ANDROID_AVD_HOME`, `$ANDROID_USER_HOME/avd`,
/// `$ANDROID_SDK_HOME/.android/avd`, else `~/.android/avd`.
pub fn avd_home(env: &Env) -> Option<PathBuf> {
    if let Some(dir) = env.var("ANDROID_AVD_HOME") {
        return Some(PathBuf::from(dir));
    }
    if let Some(dir) = env.var("ANDROID_USER_HOME") {
        return Some(Path::new(dir).join("avd"));
    }
    if let Some(dir) = env.var("ANDROID_SDK_HOME") {
        return Some(Path::new(dir).join(".android").join("avd"));
    }
    env.home().map(|home| home.join(".android").join("avd"))
}

/// The AVDs that exist (`<name>.ini` files).
pub fn list(env: &Env) -> Vec<String> {
    let Some(home) = avd_home(env) else {
        return Vec::new();
    };
    let mut names: Vec<String> = std::fs::read_dir(home)
        .map(|read| {
            read.flatten()
                .filter_map(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    name.strip_suffix(".ini").map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// An AVD's `config.ini` values.
pub fn config(env: &Env, name: &str) -> Option<std::collections::BTreeMap<String, String>> {
    let home = avd_home(env)?;
    let ini = std::fs::read_to_string(home.join(format!("{name}.ini"))).ok()?;
    let dir = ini
        .lines()
        .find_map(|line| line.strip_prefix("path="))
        .map(|path| PathBuf::from(path.trim()))
        .unwrap_or_else(|| home.join(format!("{name}.avd")));
    let text = std::fs::read_to_string(dir.join("config.ini")).ok()?;
    Some(
        text.lines()
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect(),
    )
}

/// An AVD's ABI.
pub fn abi(env: &Env, name: &str) -> Option<Abi> {
    config(env, name)?
        .get("abi.type")
        .and_then(|abi| Abi::from_name(abi))
}

/// An installed system image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// The sdkmanager package, e.g. `system-images;android-36;google_apis;arm64-v8a`.
    pub package: String,
    /// Its directory.
    pub path: PathBuf,
}

/// The sdkmanager package of the preferred image.
pub fn image_package(api: u32, abi: Abi) -> String {
    format!(
        "system-images;android-{api};{};{}",
        IMAGE_TAGS[0],
        abi.as_str()
    )
}

/// The installed system image for an API level and ABI, preferring
/// [`IMAGE_TAGS`] order.
pub fn find_image(sdk: &AndroidSdk, api: u32, abi: Abi) -> Option<Image> {
    for platform in [format!("android-{api}"), format!("android-{api}.0")] {
        for tag in IMAGE_TAGS {
            let path = sdk
                .root
                .join("system-images")
                .join(&platform)
                .join(tag)
                .join(abi.as_str());
            if path.join("system.img").is_file() || path.join("source.properties").is_file() {
                return Some(Image {
                    package: format!("system-images;{platform};{tag};{}", abi.as_str()),
                    path,
                });
            }
        }
    }
    None
}

/// Creates an icm AVD from an image (`avdmanager create avd`).
pub fn create(ctx: &Ctx, tools: &Toolset, name: &str, image: &Image) -> Result<()> {
    if !is_managed(name) {
        return Err(IcmError::new(
            CheckId::AndroidDeviceNone,
            format!(
                "the AVD `{name}` does not exist, and icm only creates AVDs named {MANAGED_PREFIX}*"
            ),
        )
        .fix(
            "Pass an existing AVD (`icm devices android`), or let icm use its managed AVD.",
            &["icm devices android", "icm run android"],
        ));
    }
    let base = tools
        .avdmanager()?
        .args(["create", "avd", "-n", name, "-k", &image.package])
        .timeout(Duration::from_secs(120));
    let outcome = ctx.step(
        "avdmanager.create",
        &base.clone().args(["-d", DEVICE_PROFILE]),
    )?;
    if outcome.success() {
        return Ok(());
    }
    // An older SDK without the profile: the default hardware.
    let text = format!("{}{}", outcome.stdout_text(), outcome.stderr_text());
    if text.contains("nknown device") || text.contains(DEVICE_PROFILE) {
        let retry = ctx.step("avdmanager.create", &base)?;
        if retry.success() {
            return Ok(());
        }
        return Err(ctx.step_failure("avdmanager.create", CheckId::ToolFailed, &retry));
    }
    Err(ctx.step_failure("avdmanager.create", CheckId::ToolFailed, &outcome))
}

/// Whether a TCP port on the loopback interface is free.
fn port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// The first port whose console and adb ports (port, port + 1) are free
/// and that no device uses.
pub fn free_port(ports: &[u16], listed: &[adb::Listed]) -> Option<u16> {
    ports.iter().copied().find(|port| {
        !listed.iter().any(|device| device.port() == Some(*port))
            && port_free(*port)
            && port_free(port + 1)
    })
}

/// A booting emulator.
#[derive(Clone, Debug)]
pub struct Booting {
    /// The AVD.
    pub avd: String,
    /// `emulator-<port>`.
    pub serial: String,
    /// The console port.
    pub port: u16,
    /// The emulator's pid.
    pub pid: u32,
    /// What tells that process from any other that has its pid later,
    /// read right after it started ([`crate::procid`]); an
    /// [`Identity::unavailable`] with the reason when it could not be read
    /// (the process was already gone, which a failed boot reports, or the
    /// OS would not describe it).
    pub identity: Option<Identity>,
    /// Its output.
    pub log: PathBuf,
}

/// The `-gpu` mode of headless emulators unless host.toml's
/// `android.emulator_gpu` says otherwise. Measured on Apple Silicon
/// (emulator 37.1, android-36): `swiftshader_indirect` and `host` both
/// render the template and reach `ICM_EVENT ready` in under a second, but
/// a `-gpu host -no-window` emulator stopped answering adb after about ten
/// minutes on a loaded host. SwiftShader needs no GPU and draws the same
/// on every host and CI runner, so it is the default.
pub const DEFAULT_HEADLESS_GPU: &str = "swiftshader_indirect";

/// Options for [`start`].
#[derive(Clone, Debug, Default)]
pub struct StartOptions {
    /// A window and `-gpu auto` instead of headless.
    pub show: bool,
    /// `-wipe-data`.
    pub wipe: bool,
    /// The headless `-gpu` mode ([`DEFAULT_HEADLESS_GPU`] when `None`).
    pub gpu: Option<String>,
}

/// The emulator's argv after the program.
pub fn start_args(avd: &str, port: u16, options: &StartOptions) -> Vec<String> {
    let mut args: Vec<String> = [
        "-avd",
        avd,
        "-port",
        &port.to_string(),
        "-no-boot-anim",
        "-no-audio",
        "-no-snapshot-save",
        "-no-metrics",
        "-skip-adb-auth",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    if options.show {
        args.extend(["-gpu".to_string(), "auto".to_string()]);
    } else {
        let gpu = options
            .gpu
            .clone()
            .filter(|gpu| !gpu.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_HEADLESS_GPU.to_string());
        args.extend(["-no-window".to_string(), "-gpu".to_string(), gpu]);
    }
    if options.wipe {
        args.push("-wipe-data".to_string());
    }
    args
}

/// Starts an emulator detached (its own session; it outlives icm) and
/// returns at once; [`wait_booted`] waits for it.
pub fn start(
    ctx: &Ctx,
    tools: &Toolset,
    avd: &str,
    port: u16,
    options: &StartOptions,
    log: &Path,
) -> Result<Booting> {
    let cmd = tools.emulator()?.args(start_args(avd, port, options));
    let begun = Instant::now();
    ctx.rep.step_begin(
        "emulator.start",
        &cmd.display_argv(),
        &cmd.display_env(),
        None,
    );
    let pid = process::spawn_detached(&cmd, log, log).map_err(|error| {
        IcmError::new(
            CheckId::AndroidEmulatorFailed,
            format!("cannot start the emulator: {error}"),
        )
    })?;
    ctx.rep
        .step_end_internal("emulator.start", true, begun.elapsed().as_millis() as u64);
    ctx.rep.progress(format!(
        "booting {avd} on emulator-{port} (pid {pid}; log {})",
        crate::paths::display(log)
    ));
    // Read now, while the pid is the emulator's: what `stop` verifies
    // before it takes the pid for the emulator or signals it. When the OS
    // will not say, the record says that icm could not read it.
    let identity = Some(crate::procid::capture(pid as i32));
    if let Some(check) =
        crate::session::unavailable_check("the emulator", pid as i32, identity.as_ref())
    {
        ctx.rep.check(check);
    }
    Ok(Booting {
        avd: avd.to_string(),
        serial: format!("emulator-{port}"),
        port,
        pid,
        identity,
        log: log.to_path_buf(),
    })
}

/// Whether a process has ended. The emulator icm started is its child
/// until icm exits, so a dead one is a zombie that `kill(pid, 0)` still
/// finds: reap it first.
fn exited(pid: u32) -> bool {
    let pid = pid as i32;
    let mut status = 0;
    // SAFETY: waitpid(2) with WNOHANG on a pid; it only reaps that child.
    let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    reaped == pid || !crate::signals::alive(pid)
}

fn log_tail(log: &Path, lines: usize) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let all: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// Waits until the emulator has booted: `sys.boot_completed` is 1 and the
/// package manager answers. Fails early when the emulator process dies.
pub fn wait_booted(ctx: &Ctx, tools: &Toolset, booting: &Booting) -> Result<Duration> {
    let started = Instant::now();
    let limit = ctx
        .remaining()
        .map_or(BOOT_TIMEOUT, |remaining| remaining.min(BOOT_TIMEOUT));
    let deadline = started + limit;
    let adb = Adb::new(tools, &booting.serial)?;
    let mut booted = false;

    loop {
        if let Some(signal) = crate::signals::pending() {
            return Err(crate::output::interrupted(signal));
        }
        if exited(booting.pid) {
            let tail = log_tail(&booting.log, 8);
            return Err(IcmError::new(
                CheckId::AndroidEmulatorFailed,
                format!(
                    "the emulator for {} exited while booting{}",
                    booting.avd,
                    if tail.is_empty() {
                        String::new()
                    } else {
                        format!(":\n{tail}")
                    }
                ),
            )
            .evidence(
                Evidence::file(&booting.log)
                    .with_excerpt(tail.lines().last().unwrap_or("").to_string()),
            ));
        }

        if !booted {
            booted = adb.getprop("sys.boot_completed").as_deref() == Some("1");
        }
        if booted
            && adb
                .shell_text("pm path android", Duration::from_secs(15))
                .is_some_and(|text| text.contains("package:"))
        {
            return Ok(started.elapsed());
        }

        if Instant::now() >= deadline {
            return Err(IcmError::new(
                CheckId::AndroidEmulatorBootTimeout,
                format!(
                    "{} did not finish booting within {} (sys.boot_completed {})",
                    booting.serial,
                    crate::time::format_duration(limit),
                    if booted {
                        "1, package manager not ready"
                    } else {
                        "not 1"
                    }
                ),
            )
            .evidence(Evidence::file(&booting.log)));
        }
        std::thread::sleep(Duration::from_millis(1000));
    }
}

/// Makes an icm-managed emulator predictable: no animations, awake,
/// unlocked. Never applied to other devices.
pub fn prepare(adb: &Adb) {
    let line = [
        "settings put global window_animation_scale 0",
        "settings put global transition_animation_scale 0",
        "settings put global animator_duration_scale 0",
        "svc power stayon true",
        "wm dismiss-keyguard",
    ]
    .join("; ");
    let _ = adb::quick(adb.shell(&line), Duration::from_secs(30));
}

/// How long [`shutdown`] waits for the emulator to go before it signals the
/// process (less when `--timeout` leaves less).
const SHUTDOWN_WAIT: Duration = Duration::from_secs(30);

/// How long the emulator's process gets, after SIGTERM, to exit.
const TERM_GRACE: Duration = Duration::from_secs(5);

/// How [`shutdown`] ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shutdown {
    /// The emulator is confirmed gone: its recorded process has exited or
    /// is another process now, or `adb devices` answered and no longer
    /// lists its serial.
    Done,
    /// It is confirmed still there after `adb emu kill`, the wait and, for a
    /// verified process, SIGTERM: it ignored them (or `emu kill` itself
    /// failed), or icm has no process of its to signal.
    Lingers {
        /// How `adb emu kill` failed (its exit and error), when it did; a
        /// call that exited 0 and changed nothing is an emulator that
        /// ignored it.
        kill_failed: Option<String>,
    },
    /// Neither: icm could not tell. `adb devices` could not be asked at the
    /// end (and, with no verified process of the emulator to read instead,
    /// that is all there is), so the emulator may still run. Absence is
    /// never concluded from a failed question.
    Unknown {
        /// Why `adb devices` could not say.
        why: String,
        /// How `adb emu kill` failed, when it did.
        kill_failed: Option<String>,
    },
}

/// Whether the emulator is still there.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Presence {
    /// Confirmed absent: the verified process has ended or is another one,
    /// or `adb devices` answered without the serial.
    Gone,
    /// Confirmed present.
    Present,
    /// The question could not be answered (why).
    Unknown(String),
}

/// How a call to adb failed, when it did (`None`: it succeeded).
fn failure(outcome: &Option<process::Outcome>) -> Option<String> {
    match outcome {
        Some(outcome) if outcome.success() => None,
        Some(outcome) => {
            let said = outcome.stderr_tail(2);
            Some(if said.is_empty() {
                outcome.describe()
            } else {
                format!("{}: {said}", outcome.describe())
            })
        }
        None => Some("adb could not be started".to_string()),
    }
}

/// Shuts an emulator down (`adb emu kill`), waiting up to 30 s (or what
/// `--timeout` leaves). The emulator's host process, when `process` names it
/// (its pid and the identity icm recorded for it), gets SIGTERM if it
/// lingers, and only while the pid still has that process ([`terminate`]):
/// a pid with no recorded identity, or that another process has taken, is
/// never signalled, and the wait then ends when `adb devices` stops listing
/// the serial. An emulator that is still there at the end is
/// [`Shutdown::Lingers`], which the caller reports instead of "stopped",
/// and one that icm could not ask about ([`Shutdown::Unknown`]: `adb
/// devices` fails and no verified process can be read instead) is not
/// reported as stopped either: a failed question is not an absent
/// emulator.
pub fn shutdown(
    ctx: &Ctx,
    tools: &Toolset,
    serial: &str,
    process: Option<(u32, &Identity)>,
) -> Result<Shutdown> {
    let wait = ctx
        .remaining()
        .map_or(SHUTDOWN_WAIT, |left| left.min(SHUTDOWN_WAIT));
    shutdown_within(tools, serial, process, wait, TERM_GRACE)
}

fn shutdown_within(
    tools: &Toolset,
    serial: &str,
    process: Option<(u32, &Identity)>,
    wait: Duration,
    grace: Duration,
) -> Result<Shutdown> {
    let adb = Adb::new(tools, serial)?;
    // A failed `emu kill` is no proof either way: the emulator may be on
    // its way down already, or not reachable. What decides is whether it is
    // still there, which is asked below.
    let kill_failed = failure(&adb::quick(
        adb.cmd(["emu", "kill"]),
        Duration::from_secs(20),
    ));
    let presence = || {
        presence_from(process_verdict(process), || match adb::devices(tools) {
            Ok(devices) if devices.iter().any(|d| d.serial == serial) => Presence::Present,
            Ok(_) => Presence::Gone,
            Err(error) => Presence::Unknown(error.detail),
        })
    };
    let deadline = Instant::now() + wait;
    let mut now = presence();
    while now != Presence::Gone && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        now = presence();
    }
    // It ignored `emu kill`. Its process, when icm knows it, is asked to end
    // and given a moment; any other emulator is left as it is.
    if now == Presence::Present && process.is_some_and(|(pid, identity)| terminate(pid, identity)) {
        let until = Instant::now() + grace;
        while now != Presence::Gone && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(100));
            now = presence();
        }
    }
    Ok(match now {
        Presence::Gone => Shutdown::Done,
        Presence::Present => Shutdown::Lingers { kill_failed },
        Presence::Unknown(why) => Shutdown::Unknown { why, kill_failed },
    })
}

/// What the recorded emulator process is now, or `None` when icm has none
/// to read.
fn process_verdict(process: Option<(u32, &Identity)>) -> Option<Verdict> {
    let (pid, identity) = process?;
    Some(crate::procid::check(i32::try_from(pid).ok()?, identity))
}

/// Whether the emulator is still there, from what the recorded process is
/// now and, when that does not say, from `listed` (what `adb devices` says).
/// A process that is the recorded one is the emulator; one that has ended or
/// is another process now is not, whatever `adb` lists. A process the OS
/// would not describe is neither: it settles nothing, so adb is asked, as it
/// is when icm has no process to read. An unreadable process is not an
/// absent emulator.
fn presence_from(verdict: Option<Verdict>, listed: impl FnOnce() -> Presence) -> Presence {
    match verdict {
        Some(Verdict::Same) => Presence::Present,
        Some(Verdict::Gone | Verdict::Other(_)) => Presence::Gone,
        Some(Verdict::Unknown(_)) | None => listed(),
    }
}

/// SIGTERM to the emulator process icm recorded, after reading the pid
/// again: it is sent only while the pid still has the process whose
/// identity was recorded. Returns whether it was sent.
pub fn terminate(pid: u32, identity: &Identity) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if crate::procid::check(pid, identity) != Verdict::Same {
        return false;
    }
    // SAFETY: kill(2) with a pid that was just read to be the emulator
    // icm started; it has no memory-safety preconditions.
    unsafe { libc::kill(pid, libc::SIGTERM) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_abis() {
        assert_eq!(managed_name(36), "icm-api36");
        assert!(is_managed("icm-api36") && is_managed("icm-test-api36"));
        assert!(!is_managed("cn_api36"));
        let abi = host_abi();
        assert!(matches!(abi, Abi::Arm64V8a | Abi::X86_64));
        assert_eq!(
            image_package(36, Abi::Arm64V8a),
            "system-images;android-36;google_apis;arm64-v8a"
        );
    }

    #[test]
    fn avd_configs_are_read_from_the_avd_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("avd");
        std::fs::create_dir_all(home.join("icm-api36.avd")).unwrap();
        std::fs::write(
            home.join("icm-api36.ini"),
            format!(
                "path={}\ntarget=android-36\n",
                home.join("icm-api36.avd").display()
            ),
        )
        .unwrap();
        std::fs::write(
            home.join("icm-api36.avd/config.ini"),
            "abi.type=arm64-v8a\nhw.device.name=pixel_9\n",
        )
        .unwrap();
        std::fs::write(home.join("other.ini"), "path=/nowhere\n").unwrap();
        let env = Env::from_pairs(&[("ANDROID_AVD_HOME", home.to_str().unwrap())], None);
        assert_eq!(list(&env), vec!["icm-api36", "other"]);
        assert_eq!(abi(&env, "icm-api36"), Some(Abi::Arm64V8a));
        assert_eq!(abi(&env, "other"), None);
    }

    #[test]
    fn images_prefer_google_apis() {
        let tmp = tempfile::tempdir().unwrap();
        let sdk = AndroidSdk {
            root: tmp.path().to_path_buf(),
            source: "test".into(),
        };
        assert!(find_image(&sdk, 36, Abi::Arm64V8a).is_none());
        for tag in ["default", "google_apis"] {
            let dir = tmp
                .path()
                .join("system-images/android-36")
                .join(tag)
                .join("arm64-v8a");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("system.img"), b"").unwrap();
        }
        let image = find_image(&sdk, 36, Abi::Arm64V8a).unwrap();
        assert_eq!(
            image.package,
            "system-images;android-36;google_apis;arm64-v8a"
        );
        assert!(find_image(&sdk, 36, Abi::X86_64).is_none());
    }

    /// A toolset whose adb lists `emulator-5580` and ignores `emu kill`:
    /// an emulator that lingers.
    fn lingering(dir: &Path) -> Toolset {
        use std::os::unix::fs::PermissionsExt;
        let adb = dir.join("platform-tools/adb");
        std::fs::create_dir_all(adb.parent().unwrap()).unwrap();
        std::fs::write(
            &adb,
            "#!/bin/sh\n[ \"$1\" = devices ] && printf 'List of devices attached\\nemulator-5580\\tdevice product:p model:m device:d transport_id:1\\n'\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&adb, std::fs::Permissions::from_mode(0o755)).unwrap();
        let host = crate::host::HostConfig {
            android_sdk: Some(dir.display().to_string()),
            ..Default::default()
        };
        Toolset::discover(&host, &Env::from_pairs(&[], Some(dir))).unwrap()
    }

    fn sleeper() -> std::process::Child {
        std::process::Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }

    /// A process the emulator's pid names gets SIGTERM only while it is
    /// the process icm recorded: a lingering verified emulator is
    /// signalled, and the pid of another process (the emulator exited, and
    /// a new process took its pid) is left alone, whatever adb says.
    #[test]
    fn a_lingering_emulator_is_signalled_only_while_it_is_the_recorded_process() {
        use std::os::unix::process::ExitStatusExt;
        let dir = tempfile::tempdir().unwrap();
        let tools = lingering(dir.path());
        let wait = Duration::from_millis(600);
        let grace = Duration::from_secs(5);

        let mut emulator = sleeper();
        let identity = crate::procid::of(emulator.id() as i32).unwrap();
        let ended = shutdown_within(
            &tools,
            "emulator-5580",
            Some((emulator.id(), &identity)),
            wait,
            grace,
        )
        .unwrap();
        assert_eq!(ended, Shutdown::Done);
        assert_eq!(emulator.wait().unwrap().signal(), Some(libc::SIGTERM));

        let mut another = sleeper();
        let recorded = Identity {
            start: "1791334000.000001".to_string(),
            exe: "/sdk/emulator/emulator".to_string(),
            unavailable: None,
        };
        assert!(!terminate(another.id(), &recorded));
        // Its pid is not the recorded process, so the emulator is gone as
        // far as icm can tell: the answer comes at once, not at the end of
        // a wait this long, and nothing is sent.
        let long = Duration::from_secs(60);
        let begun = Instant::now();
        let ended = shutdown_within(
            &tools,
            "emulator-5580",
            Some((another.id(), &recorded)),
            long,
            grace,
        )
        .unwrap();
        assert_eq!(ended, Shutdown::Done);
        assert!(begun.elapsed() < long / 2, "it waited for the deadline");
        assert!(another.try_wait().unwrap().is_none(), "it was signalled");
        let _ = another.kill();
        let _ = another.wait();
    }

    /// An emulator that is still there at the end was not shut down: with no
    /// process icm can verify the wait ends when `adb devices` still lists
    /// the serial, and a verified process that ignores SIGTERM too is still
    /// running after its grace. Neither is reported as done.
    #[test]
    fn an_emulator_that_ignores_everything_lingers() {
        use std::os::unix::process::ExitStatusExt;
        let dir = tempfile::tempdir().unwrap();
        let tools = lingering(dir.path());
        let wait = Duration::from_millis(600);

        // Nothing to signal: adb lists the serial to the end.
        let begun = Instant::now();
        let ended = shutdown_within(&tools, "emulator-5580", None, wait, wait).unwrap();
        assert_eq!(ended, Shutdown::Lingers { kill_failed: None });
        assert!(begun.elapsed() >= wait);

        // A verified process that ignores SIGTERM (`sleep` keeps the
        // ignored disposition across the exec; the marker says it was set).
        let marker = dir.path().join("deaf");
        let mut deaf = std::process::Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; : > \"$0\"; exec sleep 60")
            .arg(&marker)
            .stdin(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        while !marker.exists() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(marker.exists());
        let identity = crate::procid::of(deaf.id() as i32).unwrap();
        let ended = shutdown_within(
            &tools,
            "emulator-5580",
            Some((deaf.id(), &identity)),
            wait,
            wait,
        )
        .unwrap();
        assert_eq!(ended, Shutdown::Lingers { kill_failed: None });
        assert!(deaf.try_wait().unwrap().is_none(), "SIGTERM ended it");
        deaf.kill().unwrap();
        assert_eq!(deaf.wait().unwrap().signal(), Some(libc::SIGKILL));
    }

    /// A toolset whose adb lists `emulator-5580` until `gone` exists in its
    /// directory, fails `devices` while `devices-fail` does, and fails
    /// `emu kill` while `kill-fails` does (it creates `gone` otherwise). The
    /// script uses shell builtins only: the tools run without a `PATH`.
    fn controlled(dir: &Path) -> (Toolset, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let adb = dir.join("platform-tools/adb");
        std::fs::create_dir_all(adb.parent().unwrap()).unwrap();
        std::fs::write(
            &adb,
            r#"#!/bin/sh
d=${0%/*}
if [ "$1" = devices ]; then
  [ -f "$d/devices-fail" ] && { echo 'error: cannot connect to daemon' >&2; exit 1; }
  printf 'List of devices attached\n'
  [ -f "$d/gone" ] || printf 'emulator-5580\tdevice product:p model:m device:d transport_id:1\n'
  exit 0
fi
case "$*" in
  *"emu kill"*)
    [ -f "$d/kill-fails" ] && { echo 'error: device offline' >&2; exit 1; }
    : > "$d/gone"
    exit 0 ;;
esac
exit 0
"#,
        )
        .unwrap();
        std::fs::set_permissions(&adb, std::fs::Permissions::from_mode(0o755)).unwrap();
        let host = crate::host::HostConfig {
            android_sdk: Some(dir.display().to_string()),
            ..Default::default()
        };
        let tools = Toolset::discover(&host, &Env::from_pairs(&[], Some(dir))).unwrap();
        (tools, dir.join("platform-tools"))
    }

    /// A failed `emu kill` and a failed `adb devices` are not an emulator
    /// that is gone: with no verified process to read instead, the answer
    /// is unknown (it used to be "gone"), a failed kill with a listing that
    /// still has the serial is an emulator that lingers, the kill's failure
    /// said in both, and an emulator that is no longer listed is done
    /// whatever `emu kill` exited with. Once adb answers again, the next
    /// shutdown ends it.
    #[test]
    fn a_failed_question_is_not_an_emulator_that_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let (tools, state) = controlled(dir.path());
        let wait = Duration::from_millis(600);
        let touch = |name: &str| std::fs::write(state.join(name), "").unwrap();
        let remove = |name: &str| {
            let _ = std::fs::remove_file(state.join(name));
        };

        touch("kill-fails");
        touch("devices-fail");
        let ended = shutdown_within(&tools, "emulator-5580", None, wait, wait).unwrap();
        let Shutdown::Unknown { why, kill_failed } = ended else {
            panic!("a failed `adb devices` was taken for an absent emulator: {ended:?}");
        };
        assert!(why.contains("adb devices failed"), "{why}");
        let kill_failed = kill_failed.expect("the failed `emu kill` was not kept");
        assert!(kill_failed.contains("device offline"), "{kill_failed}");

        // adb answers again, and still lists the emulator.
        remove("devices-fail");
        let ended = shutdown_within(&tools, "emulator-5580", None, wait, wait).unwrap();
        assert!(
            matches!(&ended, Shutdown::Lingers { kill_failed: Some(why) } if why.contains("device offline")),
            "{ended:?}"
        );

        // The emulator went on its own while `emu kill` failed.
        touch("gone");
        let ended = shutdown_within(&tools, "emulator-5580", None, wait, wait).unwrap();
        assert_eq!(ended, Shutdown::Done);

        // After adb recovers, the retry ends it.
        remove("gone");
        remove("kill-fails");
        let ended = shutdown_within(&tools, "emulator-5580", None, wait, wait).unwrap();
        assert_eq!(ended, Shutdown::Done);
        assert!(state.join("gone").exists());
    }

    /// What the recorded process says outranks adb's list only when it
    /// settles the question: the recorded process is the emulator whatever
    /// adb lists, a process that ended or is another one is not (adb is not
    /// asked), and a process the OS would not describe settles nothing, so
    /// adb decides, as with no process, and a failed answer stays unknown.
    #[test]
    fn an_unreadable_process_is_not_an_absent_emulator() {
        let never = || -> Presence { panic!("adb was asked about a process that settles it") };
        assert_eq!(presence_from(Some(Verdict::Same), never), Presence::Present);
        assert_eq!(presence_from(Some(Verdict::Gone), never), Presence::Gone);
        assert_eq!(
            presence_from(Some(Verdict::Other(Identity::default())), never),
            Presence::Gone
        );
        let unreadable = || Some(Verdict::Unknown("proc_pidinfo: EPERM".into()));
        assert_eq!(
            presence_from(unreadable(), || Presence::Present),
            Presence::Present
        );
        assert_eq!(
            presence_from(unreadable(), || Presence::Gone),
            Presence::Gone
        );
        assert_eq!(
            presence_from(unreadable(), || Presence::Unknown("no adb".into())),
            Presence::Unknown("no adb".into())
        );
        assert_eq!(presence_from(None, || Presence::Present), Presence::Present);
    }

    /// A context whose output goes nowhere.
    fn quiet_ctx() -> Ctx {
        let rep = crate::output::Reporter::with_writers(
            crate::output::Mode {
                json: true,
                ..crate::output::Mode::default()
            },
            crate::output::RunInfo {
                run: "20261007T000000Z-run-android-0000".into(),
                command: "run".into(),
                target: Some("android".into()),
                argv: vec![],
                save: false,
            },
            Box::new(std::io::sink()),
            Box::new(std::io::sink()),
        );
        Ctx::new(crate::cli::GlobalArgs::default(), rep, vec![])
    }

    /// The emulator icm starts is recorded with its identity, which is what
    /// every later check of its pid compares against: without it a record
    /// is unverifiable, `run` would never carry it forward and `stop` could
    /// never signal it.
    #[test]
    fn a_started_emulator_carries_the_identity_stop_checks() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        // An emulator that is a long `sleep`; its launcher execs it, as the
        // real one execs qemu, so the pid is the same throughout.
        let emulator = dir.path().join("emulator/emulator");
        std::fs::create_dir_all(emulator.parent().unwrap()).unwrap();
        std::fs::write(&emulator, "#!/bin/sh\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&emulator, std::fs::Permissions::from_mode(0o755)).unwrap();
        let host = crate::host::HostConfig {
            android_sdk: Some(dir.path().display().to_string()),
            ..Default::default()
        };
        let tools = Toolset::discover(&host, &Env::from_pairs(&[], Some(dir.path()))).unwrap();

        let booting = start(
            &quiet_ctx(),
            &tools,
            "icm-test-api36",
            5580,
            &StartOptions::default(),
            &dir.path().join("emulator.log"),
        )
        .unwrap();
        let pid = i32::try_from(booting.pid).unwrap();
        let identity = booting.identity.clone();
        let verdict = identity
            .as_ref()
            .map(|identity| crate::procid::check(pid, identity));
        // SAFETY: kill(2) and waitpid(2) on the process this test started
        // (a child, so it is reaped).
        unsafe {
            let _ = libc::kill(pid, libc::SIGKILL);
            let mut status = 0;
            let _ = libc::waitpid(pid, &mut status, 0);
        }
        assert_eq!(booting.serial, "emulator-5580");
        assert!(identity.is_some(), "the identity was not read");
        assert_eq!(verdict, Some(Verdict::Same));
        // The process has ended now, and the identity says so.
        assert_eq!(crate::procid::check(pid, &identity.unwrap()), Verdict::Gone);
    }

    #[test]
    fn headless_by_default() {
        let args = start_args("icm-api36", 5580, &StartOptions::default());
        let line = args.join(" ");
        assert!(line.starts_with("-avd icm-api36 -port 5580"), "{line}");
        assert!(
            line.contains("-no-window -gpu swiftshader_indirect"),
            "{line}"
        );
        assert!(line.contains("-no-snapshot-save"));
        let shown = start_args(
            "x",
            5582,
            &StartOptions {
                show: true,
                wipe: true,
                gpu: Some("host".into()),
            },
        )
        .join(" ");
        assert!(shown.contains("-gpu auto") && !shown.contains("-no-window"));
        assert!(shown.ends_with("-wipe-data"));
        let host = start_args(
            "x",
            5584,
            &StartOptions {
                gpu: Some("host".into()),
                ..StartOptions::default()
            },
        )
        .join(" ");
        assert!(host.contains("-no-window -gpu host"), "{host}");
    }

    #[test]
    fn ports_skip_used_ones() {
        let listed = adb::parse_devices("emulator-5580 device\n");
        // Hold a port's adb port (port + 1).
        let holder = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let held = holder.local_addr().unwrap().port();
        let port = held - 1;
        assert_eq!(free_port(&[5580], &listed), None);
        assert_eq!(free_port(&[port], &[]), None);
    }
}
