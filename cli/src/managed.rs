//! The devices icm manages: their names, where Android keeps its per-user
//! files, and which system image the managed emulator uses.
//!
//! Every simulator and AVD icm creates has a name starting with `icm-`, and
//! icm never modifies, boots or shuts down a device it did not create
//! (`icm-test-` devices belong to icm's own test runs and are left alone
//! too). `icm doctor <platform> --fix` creates the managed devices; `icm
//! run` uses them; `icm stop --shutdown` shuts them down.

use crate::config::Abi;
use crate::tools::Env;
use std::path::PathBuf;

/// The prefix of every device icm creates.
pub const PREFIX: &str = "icm-";

/// The prefix of devices icm's own tests create, which `stop --shutdown`
/// leaves alone.
pub const TEST_PREFIX: &str = "icm-test-";

/// The hardware profile of the managed emulator (`avdmanager list device`).
pub const EMULATOR_DEVICE: &str = "pixel_9";

/// The system-image tag of the managed emulator.
pub const SYSTEM_IMAGE_TAG: &str = "google_apis";

/// The NDK `icm doctor android --fix --yes` installs.
pub const NDK_PACKAGE: &str = "ndk;29.0.14206865";

/// The build-tools `icm doctor android --fix --yes` installs.
pub const BUILD_TOOLS_PACKAGE: &str = "build-tools;36.0.0";

/// The managed AVD for a target SDK: `icm-api36`.
pub fn avd_name(target_sdk: u32) -> String {
    format!("{PREFIX}api{target_sdk}")
}

/// The managed simulator for a device type and runtime:
/// `icm-iPhone 17 (iOS 27.0)`.
pub fn simulator_name(device_type: &str, runtime_version: &str) -> String {
    format!("{PREFIX}{device_type} (iOS {runtime_version})")
}

/// Whether icm created a device of this name and may shut it down.
pub fn is_managed(name: &str) -> bool {
    name.starts_with(PREFIX) && !name.starts_with(TEST_PREFIX)
}

/// The emulator ABI for this host (Appendix C item 10): an arm64 image on
/// Apple Silicon and other arm64 hosts, x86_64 elsewhere.
pub fn host_abi() -> Abi {
    if std::env::consts::ARCH == "aarch64" {
        Abi::Arm64V8a
    } else {
        Abi::X86_64
    }
}

/// The sdkmanager package of the managed emulator's system image.
pub fn system_image_package(api: u32, abi: Abi) -> String {
    format!(
        "system-images;android-{api};{SYSTEM_IMAGE_TAG};{}",
        abi.as_str()
    )
}

/// Android's per-user directory: `$ANDROID_USER_HOME`, else
/// `$ANDROID_SDK_HOME/.android`, else `~/.android`.
pub fn android_user_home(env: &Env) -> Option<PathBuf> {
    if let Some(dir) = env.var("ANDROID_USER_HOME") {
        return Some(PathBuf::from(dir));
    }
    if let Some(dir) = env.var("ANDROID_SDK_HOME") {
        return Some(PathBuf::from(dir).join(".android"));
    }
    env.home().map(|home| home.join(".android"))
}

/// Where AVDs live: `$ANDROID_AVD_HOME`, else `<android user home>/avd`.
pub fn avd_home(env: &Env) -> Option<PathBuf> {
    if let Some(dir) = env.var("ANDROID_AVD_HOME") {
        return Some(PathBuf::from(dir));
    }
    android_user_home(env).map(|dir| dir.join("avd"))
}

/// Whether an AVD exists (its `<name>.ini` in the AVD home).
pub fn avd_exists(env: &Env, name: &str) -> bool {
    avd_home(env).is_some_and(|home| home.join(format!("{name}.ini")).is_file())
}

/// The debug keystore every icm debug APK is signed with.
pub fn debug_keystore(env: &Env) -> Option<PathBuf> {
    android_user_home(env).map(|dir| dir.join("debug.keystore"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn names_carry_the_prefix() {
        assert_eq!(avd_name(36), "icm-api36");
        assert_eq!(
            simulator_name("iPhone 17", "27.0"),
            "icm-iPhone 17 (iOS 27.0)"
        );
        assert!(is_managed("icm-api36"));
        assert!(!is_managed("icm-test-proto"));
        assert!(!is_managed("cn_api36"));
        assert!(!is_managed("iPhone 17"));
        assert_eq!(
            system_image_package(36, Abi::Arm64V8a),
            "system-images;android-36;google_apis;arm64-v8a"
        );
    }

    #[test]
    fn android_homes_follow_the_variables() {
        let home = Path::new("/home/me");
        let env = Env::from_pairs(&[], Some(home));
        assert_eq!(android_user_home(&env), Some(home.join(".android")));
        assert_eq!(avd_home(&env), Some(home.join(".android/avd")));
        assert_eq!(
            debug_keystore(&env),
            Some(home.join(".android/debug.keystore"))
        );

        let env = Env::from_pairs(
            &[("ANDROID_USER_HOME", "/u"), ("ANDROID_AVD_HOME", "/avds")],
            Some(home),
        );
        assert_eq!(android_user_home(&env), Some(PathBuf::from("/u")));
        assert_eq!(avd_home(&env), Some(PathBuf::from("/avds")));

        let env = Env::from_pairs(&[("ANDROID_SDK_HOME", "/s")], Some(home));
        assert_eq!(avd_home(&env), Some(PathBuf::from("/s/.android/avd")));
    }
}
