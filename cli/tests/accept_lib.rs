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
