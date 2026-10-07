//! `~/.config/icm/host.toml` (design §7.6): per-machine settings, never
//! committed. A missing file means "autodetect everything".

use crate::catalogue::CheckId;
use crate::config::source::Source;
use crate::error::IcmError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The file's contents.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    /// The Android SDK root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub android_sdk: Option<String>,
    /// The Android NDK root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub android_ndk: Option<String>,
    /// A JDK 17+ home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub java_home: Option<String>,
    /// The Chrome (or Chromium) executable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chrome: Option<String>,
    /// A keychain file Apple signing searches instead of the user's
    /// keychain search list (a CI keychain); `ICM_KEYCHAIN` overrides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_keychain: Option<String>,
    /// `[ios]`.
    #[serde(default)]
    pub ios: HostIos,
    /// `[android]`.
    #[serde(default)]
    pub android: HostAndroid,
}

/// `[ios]` in host.toml.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostIos {
    /// The simulator device type, e.g. `iPhone 17`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulator_type: Option<String>,
    /// Pin an existing simulator instead of the managed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulator_udid: Option<String>,
}

/// `[android]` in host.toml.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostAndroid {
    /// The AVD to boot instead of the managed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avd: Option<String>,
    /// Pin a device serial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// The emulator console ports icm may use (even numbers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emulator_ports: Option<Vec<u16>>,
    /// The `-gpu` mode of headless emulators icm boots (default
    /// `swiftshader_indirect`; `host` uses the host GPU).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emulator_gpu: Option<String>,
}

/// The default emulator ports (Appendix C item 10: configurable).
pub const DEFAULT_EMULATOR_PORTS: &[u16] = &[5580, 5582, 5584];

impl HostConfig {
    /// The keychain Apple signing searches: `ICM_KEYCHAIN`, else
    /// `signing_keychain`; `None` means the user's keychain search list.
    /// Release pipelines pass it to `security find-identity` and
    /// `codesign --keychain`, so a CI or test keychain never has to join
    /// the search list.
    pub fn signing_keychain(&self, env: &crate::tools::Env) -> Option<PathBuf> {
        env.var("ICM_KEYCHAIN").map(PathBuf::from).or_else(|| {
            self.signing_keychain
                .as_deref()
                .filter(|path| !path.trim().is_empty())
                .map(PathBuf::from)
        })
    }

    /// The emulator ports to try, in order.
    pub fn emulator_ports(&self) -> Vec<u16> {
        self.android
            .emulator_ports
            .clone()
            .unwrap_or_else(|| DEFAULT_EMULATOR_PORTS.to_vec())
    }
}

/// A loaded host.toml.
#[derive(Clone, Debug, Default)]
pub struct LoadedHost {
    /// The file, if it exists.
    pub path: Option<PathBuf>,
    /// The settings (defaults when there is no file).
    pub config: HostConfig,
}

/// Loads host.toml from its standard location (see [`crate::paths::host_config`]).
pub fn load() -> Result<LoadedHost, Vec<IcmError>> {
    match crate::paths::host_config() {
        Some(path) if path.is_file() => {
            let text = std::fs::read_to_string(&path).map_err(|error| {
                vec![IcmError::new(
                    CheckId::ConfigInvalid,
                    format!("cannot read {}: {error}", crate::paths::display(&path)),
                )]
            })?;
            parse(&path, &text)
        }
        _ => Ok(LoadedHost::default()),
    }
}

/// Parses host.toml text.
pub fn parse(path: &Path, text: &str) -> Result<LoadedHost, Vec<IcmError>> {
    let source = Source::new(path, text.to_string());
    let config: HostConfig =
        toml::from_str(text).map_err(|error| vec![crate::config::toml_error(&source, &error)])?;

    let mut problems = Vec::new();
    if let Some(ports) = &config.android.emulator_ports {
        if ports.is_empty() {
            problems.push(invalid(
                &source,
                "android.emulator_ports",
                "must not be empty",
            ));
        }
        for (index, port) in ports.iter().enumerate() {
            if port % 2 != 0 || !(5554..=5682).contains(port) {
                problems.push(invalid(
                    &source,
                    &format!("android.emulator_ports[{index}]"),
                    &format!("{port} must be an even port between 5554 and 5682"),
                ));
            }
        }
    }

    if problems.is_empty() {
        Ok(LoadedHost {
            path: Some(path.to_path_buf()),
            config,
        })
    } else {
        Err(problems)
    }
}

fn invalid(source: &Source, key: &str, message: &str) -> IcmError {
    IcmError::new(
        CheckId::ConfigInvalid,
        format!("{}: `{key}` {message}", source.location_for(key)),
    )
    .evidence(source.evidence_for(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_documented_file_parses() {
        let text = r#"android_sdk = "/opt/homebrew/share/android-commandlinetools"
chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
[ios]
simulator_type = "iPhone 17"
simulator_udid = ""
[android]
avd = ""
device = ""
emulator_ports = [5580, 5582, 5600]
"#;
        let host = parse(Path::new("/h/host.toml"), text).unwrap();
        assert_eq!(host.config.emulator_ports(), vec![5580, 5582, 5600]);
        assert_eq!(host.config.ios.simulator_type.as_deref(), Some("iPhone 17"));
    }

    #[test]
    fn unknown_keys_and_bad_ports_are_reported() {
        let error = parse(Path::new("/h/host.toml"), "andriod_sdk = \"/x\"\n").unwrap_err();
        assert_eq!(error[0].id, "config.unknown_key");
        assert_eq!(error[0].evidence[0].line, Some(1));

        let errors = parse(
            Path::new("/h/host.toml"),
            "[android]\nemulator_ports = [5581, 9000]\n",
        )
        .unwrap_err();
        assert_eq!(errors.len(), 2);
        assert!(errors[0].detail.contains("5581"));
        assert_eq!(errors[0].evidence[0].line, Some(2));
    }

    #[test]
    fn defaults_without_a_file() {
        assert_eq!(
            HostConfig::default().emulator_ports(),
            DEFAULT_EMULATOR_PORTS
        );
    }

    #[test]
    fn the_signing_keychain_comes_from_the_env_then_the_file() {
        use crate::tools::Env;
        let host = parse(
            Path::new("/h/host.toml"),
            "signing_keychain = \"/ci/build.keychain-db\"\n",
        )
        .unwrap()
        .config;
        assert_eq!(
            host.signing_keychain(&Env::default()),
            Some(PathBuf::from("/ci/build.keychain-db"))
        );
        let env = Env::from_pairs(&[("ICM_KEYCHAIN", "/tmp/test.keychain-db")], None);
        assert_eq!(
            host.signing_keychain(&env),
            Some(PathBuf::from("/tmp/test.keychain-db"))
        );
        assert_eq!(
            HostConfig::default().signing_keychain(&Env::default()),
            None
        );
    }
}
