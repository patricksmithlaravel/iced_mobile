//! The acceptance scripts' shared helpers (`tests/accept/lib.sh`) against
//! the real icm and a fake Android SDK, NDK and JDK under a path with a
//! space: `icm print env android` shell-quotes such a path, and the scripts
//! once cut `export NAME=` off its lines, kept the quotes, found no adb and
//! let a foreign device pass the guard. Needs bash and jq (the scripts'
//! own requirements); skips without jq.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_icm");

fn lib_sh() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/accept/lib.sh")
}

fn has_jq() -> bool {
    Path::new("/usr/bin/jq").is_file()
        || Command::new("sh")
            .args(["-c", "command -v jq"])
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

fn executable(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Host {
    root: tempfile::TempDir,
    sdk: PathBuf,
    jdk: PathBuf,
}

impl Host {
    /// A fake SDK whose adb lists `devices` (`serial<TAB>state` lines;
    /// `emulator-5554` runs the AVD `icm-api36`), an NDK r28 and a JDK 21,
    /// all under `with space/`.
    fn new(devices: &str) -> Host {
        let root = tempfile::tempdir().unwrap();
        let spaced = root.path().join("with space");
        let sdk = spaced.join("sdk");
        let jdk = spaced.join("jdk");
        executable(
            &sdk.join("platform-tools/adb"),
            &format!(
                "#!/bin/sh\ncase \"$*\" in\ndevices) printf 'List of devices attached\\n{devices}' ;;\n\"-s emulator-5554 emu avd name\") printf 'icm-api36\\r\\nOK\\r\\n' ;;\n*) exit 1 ;;\nesac\n"
            ),
        );
        std::fs::create_dir_all(sdk.join("ndk/28.0.1")).unwrap();
        std::fs::write(
            sdk.join("ndk/28.0.1/source.properties"),
            "Pkg.Revision = 28.0.1\n",
        )
        .unwrap();
        executable(
            &jdk.join("bin/java"),
            "#!/bin/sh\necho 'openjdk version \"21.0.2\" 2024-01-16' >&2\n",
        );
        std::fs::write(jdk.join("release"), "JAVA_VERSION=\"21.0.2\"\n").unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(BIN, bin.join("icm")).unwrap();
        Host { root, sdk, jdk }
    }

    /// Runs `script` after sourcing lib.sh, as a step of a script does.
    fn bash(&self, script: &str) -> Output {
        let path = format!(
            "{}:{}",
            self.root.path().join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut command = Command::new("bash");
        let _ = command
            .args(["-c", &format!("set -euo pipefail\n. \"$LIB\"\n{script}")])
            .env("LIB", lib_sh())
            .env("ACCEPT", self.root.path())
            .env("PATH", path)
            .env("ANDROID_HOME", &self.sdk)
            .env("JAVA_HOME", &self.jdk)
            .env("ICM_CACHE_DIR", self.root.path().join("cache"))
            .env("ICM_HOST_CONFIG", self.root.path().join("no-host.toml"))
            .stdin(Stdio::null());
        for var in [
            "ANDROID_SDK_ROOT",
            "ANDROID_NDK_HOME",
            "ANDROID_NDK_ROOT",
            "ICM_JSON",
            "ICM_CONFIG",
            "ICM_RUN_ID",
            "ICM_RUN_DIR",
            "ICM_RUN_ROOT",
            "ICM_DETACHED",
        ] {
            let _ = command.env_remove(var);
        }
        command.output().unwrap()
    }
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn helpers_read_icms_environment_from_json_with_spaced_paths() {
    if !has_jq() {
        eprintln!("skipped: no jq");
        return;
    }
    let host = Host::new("emulator-5554\\tdevice\\n");

    // The shell form quotes the paths; the helpers give the paths.
    let printed = host.bash("icm print env android");
    assert!(
        text(&printed).contains(&format!("export ANDROID_HOME='{}'", host.sdk.display())),
        "{}",
        text(&printed)
    );
    let output = host.bash("sdk_adb; java_home");
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!(
            "{}\n{}\n",
            host.sdk.join("platform-tools/adb").display(),
            host.jdk.display()
        )
    );

    // Only icm's emulator online: the guard passes and names it.
    let output = host.bash("no_foreign_android");
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains("emulator-5554 runs icm-api36"),
        "{}",
        text(&output)
    );
}

#[test]
fn the_foreign_device_guard_fails_closed() {
    if !has_jq() {
        eprintln!("skipped: no jq");
        return;
    }
    // A phone next to icm's emulator: refused.
    let host = Host::new("emulator-5554\\tdevice\\nR58M1234\\tdevice\\n");
    let output = host.bash("no_foreign_android");
    assert!(!output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains("R58M1234 (not an emulator) is not icm's managed emulator"),
        "{}",
        text(&output)
    );

    // No adb to ask: refused too, never read as "no device online".
    std::fs::remove_file(host.sdk.join("platform-tools/adb")).unwrap();
    let output = host.bash("no_foreign_android");
    assert!(!output.status.success(), "{}", text(&output));
    assert!(text(&output).contains("no adb at"), "{}", text(&output));

    // A variable icm does not print: `icm_env` fails rather than give "".
    let output = host.bash("icm_env NO_SUCH_VARIABLE");
    assert!(!output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains("has no NO_SUCH_VARIABLE"),
        "{}",
        text(&output)
    );
}

/// phase1.sh's final check on simulators (`simulators_before`,
/// `simulators_left`) against a fake `xcrun`: an icm simulator booted
/// before the run is another process's and stays out of the verdict,
/// unless the run's session used it and no project claimed it; one booted
/// since the run started fails it. The check once failed on any booted
/// icm simulator, so a simulator another process kept booted (a simulator
/// panel streaming its log) failed every run.
#[test]
fn the_simulator_check_leaves_out_what_was_booted_before_the_run() {
    if !has_jq() {
        eprintln!("skipped: no jq");
        return;
    }
    let host = Host::new("");
    let accept = host.root.path();
    // `simctl list devices booted -j` from booted.json; `simctl getenv`
    // from owner-<udid>, empty (and exit 0) when unset, as simctl does.
    executable(
        &accept.join("bin/xcrun"),
        "#!/bin/sh\ncase \"$1 $2\" in\n\"simctl list\") cat \"$ACCEPT/booted.json\" ;;\n\"simctl getenv\") cat \"$ACCEPT/owner-$3\" 2>/dev/null ;;\n*) exit 1 ;;\nesac\nexit 0\n",
    );
    let boot = |devices: &[(&str, &str)]| {
        let list: Vec<String> = devices
            .iter()
            .map(|(udid, name)| {
                format!(r#"{{"udid": "{udid}", "name": "{name}", "state": "Booted"}}"#)
            })
            .collect();
        std::fs::write(
            accept.join("booted.json"),
            format!(
                r#"{{"devices": {{"com.apple.CoreSimulator.SimRuntime.iOS-27-0": [{}]}}}}"#,
                list.join(", ")
            ),
        )
        .unwrap();
    };
    let shared = ("SHARED-UDID", "icm-iphone-17-ios-27.0");
    let test = ("TEST-UDID", "icm-test-accept");

    // Before the run: the shared simulator is recorded, the test one never.
    boot(&[shared, test]);
    let output = host.bash("simulators_before");
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        std::fs::read_to_string(accept.join("simulators-before.txt")).unwrap(),
        "SHARED-UDID icm-iphone-17-ios-27.0\n"
    );
    assert!(
        text(&output).contains("booted before the run: SHARED-UDID icm-iphone-17-ios-27.0"),
        "{}",
        text(&output)
    );

    // The run used the test simulator: the shared one is left out.
    let output = host.bash("simulators_left TEST-UDID");
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains(
            "left icm-iphone-17-ios-27.0 (SHARED-UDID) running: booted before the run, which did not use it"
        ),
        "{}",
        text(&output)
    );

    // One booted since the run started fails the check.
    boot(&[shared, test, ("NEW-UDID", "icm-iphone-air-ios-27.0")]);
    let output = host.bash("simulators_left TEST-UDID");
    assert!(!output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains(
            "the icm simulator icm-iphone-air-ios-27.0 (NEW-UDID) is still booted, and was not when the run started"
        ),
        "{}",
        text(&output)
    );

    // The run used the shared one: `stop --shutdown` shuts it down unless
    // another project's icm booted it.
    boot(&[shared, test]);
    let output = host.bash("simulators_left SHARED-UDID");
    assert!(!output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains("which no project claimed, and it is still booted"),
        "{}",
        text(&output)
    );
    std::fs::write(accept.join("owner-SHARED-UDID"), "0123456789abcdef\n").unwrap();
    let output = host.bash("simulators_left SHARED-UDID");
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains("ICM_BOOTED_BY 0123456789abcdef"),
        "{}",
        text(&output)
    );

    // Without the record of what was booted before, every icm simulator
    // counts.
    std::fs::remove_file(accept.join("simulators-before.txt")).unwrap();
    let output = host.bash("simulators_left");
    assert!(!output.status.success(), "{}", text(&output));
}

/// Every phase but phase1.sh, which drives all four dev platforms, shuts
/// down only the devices of the platforms it drives. `icm stop --all
/// --shutdown` also shuts down, with no ios-sim session, the managed
/// simulator the project's runs would pick, and asks Android to shut down
/// icm's emulator: devices such a phase never booted, which another process
/// may be using. phase2.sh and phase3.sh once cleaned up that way. (A plain
/// `stop --all` shuts nothing down.)
#[test]
fn only_phase1_shuts_down_every_platform() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/accept");
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.starts_with("phase") || name == "phase1.sh" {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (n, line) in text.lines().enumerate() {
            assert!(
                line.trim_start().starts_with('#')
                    || !(line.contains("stop --all") && line.contains("--shutdown")),
                "{name}:{}: shut down only the platforms the phase drives: {line}",
                n + 1
            );
        }
    }
}

/// `matches`, lib.sh's `grep -q` for the end of a pipe, reads its input to
/// the end. A step runs with pipefail, and in `PRODUCER | grep -q` grep
/// leaves at the first match: a producer still writing dies of SIGPIPE, and
/// the step fails (exit 141) although the line was there. phase2.sh's
/// ipa-layout step (`zipinfo -1 | grep -q`) failed so in about one run in
/// ten.
#[test]
fn matches_reads_its_input_to_the_end() {
    let host = Host::new("");
    // Line 1 of about 1.3 MB: grep -q leaves with the rest unwritten.
    let output = host.bash("seq 1 200000 | grep -q -x 1");
    assert!(!output.status.success(), "{}", text(&output));

    let output = host.bash("seq 1 200000 | matches -x 1");
    assert!(output.status.success(), "{}", text(&output));
    let output = host.bash("seq 1 200000 | matches -x 0");
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    let output =
        host.bash("if seq 1 200000 | matches -E '^2$'; then echo found; else echo missing; fi");
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "found\n");
}

/// No line of the scripts pipes into a reader that can leave before the end
/// of its input, `grep -q` or `head`: under pipefail the producer's SIGPIPE
/// fails the step. `matches` and `sed -n` read to the end.
#[test]
fn no_pipe_ends_in_a_reader_that_leaves_early() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/accept");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|extension| extension != "sh") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&path).unwrap();
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with('#') {
                continue;
            }
            // What follows each `|` (a `||` leaves an empty piece).
            for piece in line.split('|').skip(1) {
                let words: Vec<&str> = piece.split_whitespace().collect();
                let leaves_early = match words.first() {
                    Some(&"head") => true,
                    Some(&"grep") => words[1..]
                        .iter()
                        .take_while(|word| word.starts_with('-'))
                        .any(|flag| {
                            matches!(*flag, "--quiet" | "--silent")
                                || (!flag.starts_with("--") && flag.contains('q'))
                        }),
                    _ => false,
                };
                if leaves_early {
                    found.push(format!("{name}:{}: {}", n + 1, line.trim()));
                }
            }
        }
    }
    found.sort();
    assert!(
        found.is_empty(),
        "pipe into `matches` or `sed -n` instead:\n{}",
        found.join("\n")
    );
}
