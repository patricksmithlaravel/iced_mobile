//! The Android pipeline (design §9.4, §10.4, §13; Appendix C items 2, 10,
//! 25 and 27): `icm build|run|stop|shot|logs|input|devices android`.
//! `icm doctor android` is the generic doctor ([`crate::doctor`]).
//!
//! | Module | What it owns |
//! |---|---|
//! | [`Toolset`] (here) | the SDK, JDK and NDK, and the environment every Android child gets (`JAVA_HOME`, `PATH`) |
//! | [`apk`] | `cargo rustc --crate-type cdylib`, the ELF gates, res, manifest, aapt2 → zip → zipalign → apksigner → verify |
//! | [`bundle`] | the release bundle: `base.zip`, keytool/jarsigner/`bundletool dump` parsing, the §12.3 gates (the pipeline is `crate::release::android`) |
//! | [`avd`] | the managed AVD (`icm-api<target_sdk>`, host-ABI image), ports, boot, shutdown |
//! | [`device`] | which device a command uses (design §6 order) |
//! | [`pipeline`] | the commands: install, launch, readiness, screenshot, logs, input, stop |
//! | [`plan`] | what `--dry-run` prints for each command (nothing touches a device) |
//! | [`lifecycle`] | `icm test --on android --lifecycle`: the lifecycle suite on a device |
//! | [`session`] | `target/icm/sessions/android.json` |
//! | [`logcat`] | `threadtime,epoch` records, `ICM_EVENT` lines, failure signatures |
//! | [`manifest`], [`res`] | the generated `AndroidManifest.xml` and resources |
//! | [`elf`], [`zip`], [`image`] | ELF facts, the stored ZIP writer, PNG work |
//!
//! Nothing here needs a shell: argv is built in Rust, and the NDK
//! environment is computed ([`crate::tools::ndk_env`]).

pub mod adb;
pub mod apk;
pub mod avd;
pub mod bundle;
pub mod device;
pub mod elf;
pub mod image;
pub mod lifecycle;
pub mod logcat;
pub mod manifest;
pub mod pipeline;
pub mod plan;
pub mod res;
pub mod session;
pub mod zip;

pub use pipeline::{build, device_listing, devices, input, logs, run, shot, stop, stop_session};

use crate::catalogue::CheckId;
use crate::error::{Evidence, IcmError};
use crate::host::HostConfig;
use crate::process::Cmd;
use crate::tools::{self, AndroidSdk, Env, Jdk, Ndk};
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The lowest build-tools major release icm accepts: `zipalign -P` (16 KB
/// page alignment of native libraries) arrived in 35.
pub const MIN_BUILD_TOOLS: u32 = 35;

/// The build-tools `doctor --fix --yes` installs.
pub const BUILD_TOOLS_PACKAGE: &str = crate::managed::BUILD_TOOLS_PACKAGE;

/// The Android SDK, JDK and NDK, and the environment for their children.
#[derive(Clone, Debug)]
pub struct Toolset {
    /// The SDK.
    pub sdk: AndroidSdk,
    /// A JDK 17+ (apksigner, keytool, avdmanager, sdkmanager need one).
    pub jdk: Result<Jdk, IcmError>,
    /// An NDK r28+ (linking the library needs one).
    pub ndk: Result<Ndk, IcmError>,
    env: Env,
    child_env: Vec<(String, String)>,
}

impl Toolset {
    /// Finds the SDK (required) and the JDK and NDK (reported when a step
    /// needs them).
    pub fn discover(host: &HostConfig, env: &Env) -> Result<Toolset, IcmError> {
        let sdk = tools::android_sdk(host, env)?;
        let jdk = tools::jdk(host, env, true);
        let ndk = tools::ndk(Some(&sdk), host, env);
        let child_env = tools::android_env(
            jdk.as_ref().ok(),
            Some(&sdk),
            ndk.as_ref().ok(),
            env.var("PATH").unwrap_or(""),
        );
        Ok(Toolset {
            sdk,
            jdk,
            ndk,
            env: env.clone(),
            child_env,
        })
    }

    /// The environment Android children get (Appendix C item 2):
    /// `JAVA_HOME`, `PATH` with `<jdk>/bin` first, `ANDROID_HOME`, ...
    pub fn child_env(&self) -> &[(String, String)] {
        &self.child_env
    }

    /// A command for an Android tool, with [`Toolset::child_env`].
    pub fn cmd(&self, program: impl AsRef<OsStr>) -> Cmd {
        Cmd::new(program).envs(
            self.child_env
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        )
    }

    fn package_missing(&self, package: &str, what: &str) -> IcmError {
        IcmError::new(
            CheckId::EnvAndroidPackageMissing,
            format!(
                "{what} is not installed in the Android SDK at {}",
                self.sdk.root.display()
            ),
        )
        .fix_commands([
            "icm doctor android --fix --yes".to_string(),
            format!("sdkmanager --install \"{package}\""),
        ])
        .evidence(Evidence::file(&self.sdk.root))
    }

    /// `adb` (`platform-tools`).
    pub fn adb(&self) -> Result<Cmd, IcmError> {
        let path = self.sdk.adb(&self.env);
        if self.env.tool_override("adb").is_none() && !path.exists() {
            return Err(self.package_missing("platform-tools", "platform-tools (adb)"));
        }
        Ok(self.cmd(path))
    }

    /// `emulator`.
    pub fn emulator(&self) -> Result<Cmd, IcmError> {
        let path = self.sdk.emulator(&self.env);
        if self.env.tool_override("emulator").is_none() && !path.exists() {
            return Err(self.package_missing("emulator", "the emulator"));
        }
        Ok(self.cmd(path))
    }

    /// `avdmanager` (needs the JDK).
    pub fn avdmanager(&self) -> Result<Cmd, IcmError> {
        let _ = self.jdk.clone()?;
        let path = self.sdk.avdmanager(&self.env).ok_or_else(|| {
            self.package_missing("cmdline-tools;latest", "cmdline-tools (avdmanager)")
        })?;
        Ok(self.cmd(path))
    }

    /// `sdkmanager` (needs the JDK).
    pub fn sdkmanager(&self) -> Result<Cmd, IcmError> {
        let _ = self.jdk.clone()?;
        let path = self.sdk.sdkmanager(&self.env).ok_or_else(|| {
            self.package_missing("cmdline-tools;latest", "cmdline-tools (sdkmanager)")
        })?;
        Ok(self.cmd(path))
    }

    /// The build-tools directory (35 or newer) and its version.
    pub fn build_tools(&self) -> Result<(String, PathBuf), IcmError> {
        self.sdk.build_tools(MIN_BUILD_TOOLS).ok_or_else(|| {
            let found = self.sdk.build_tools_versions();
            let mut error = self.package_missing(
                BUILD_TOOLS_PACKAGE,
                &format!("build-tools {MIN_BUILD_TOOLS} or newer"),
            );
            if !found.is_empty() {
                error
                    .detail
                    .push_str(&format!(" (found: {})", found.join(", ")));
            }
            error
        })
    }

    /// A build-tools program (`aapt2`, `zipalign`, `apksigner`).
    pub fn build_tool(&self, name: &str) -> Result<Cmd, IcmError> {
        if let Some(path) = self.env.tool_override(name) {
            return Ok(self.cmd(path));
        }
        let (_, dir) = self.build_tools()?;
        Ok(self.cmd(dir.join(name)))
    }

    /// `apksigner`, which needs the JDK.
    pub fn apksigner(&self) -> Result<Cmd, IcmError> {
        let _ = self.jdk.clone()?;
        self.build_tool("apksigner")
    }

    /// The JDK's `keytool`.
    pub fn keytool(&self) -> Result<Cmd, IcmError> {
        if let Some(path) = self.env.tool_override("keytool") {
            return Ok(self.cmd(path));
        }
        let jdk = self.jdk.clone()?;
        Ok(self.cmd(jdk.home.join("bin").join("keytool")))
    }

    /// A JDK program (`java`, `jar`, `jarsigner`), or its `ICM_TOOL_<NAME>`
    /// stand-in.
    pub fn jdk_tool(&self, name: &str) -> Result<Cmd, IcmError> {
        if let Some(path) = self.env.tool_override(name) {
            return Ok(self.cmd(path));
        }
        let jdk = self.jdk.clone()?;
        Ok(self.cmd(jdk.home.join("bin").join(name)))
    }

    /// `java -jar <bundletool>`: the pinned bundletool (design §16.2), which
    /// [`crate::pinned::require`] downloads only with `--yes`, or
    /// `ICM_TOOL_BUNDLETOOL`. Returns the command and the version (`None`
    /// for a stand-in).
    pub fn bundletool(&self, ctx: &crate::context::Ctx) -> Result<(Cmd, Option<String>), IcmError> {
        let found = crate::pinned::require(ctx, "bundletool")?;
        let cmd = self.jdk_tool("java")?.arg("-jar").arg(&found.path);
        Ok((cmd, found.version))
    }

    /// The NDK's `llvm-strip`.
    pub fn llvm_strip(&self) -> Result<Cmd, IcmError> {
        if let Some(path) = self.env.tool_override("llvm-strip") {
            return Ok(self.cmd(path));
        }
        let ndk = self.ndk.clone()?;
        Ok(self.cmd(ndk.toolchain_bin().join("llvm-strip")))
    }

    /// `platforms/android-<api>/android.jar`.
    pub fn platform_jar(&self, api: u32) -> Result<PathBuf, IcmError> {
        let jar = self.sdk.platform_jar(api);
        if jar.is_file() {
            Ok(jar)
        } else {
            Err(self.package_missing(
                &format!("platforms;android-{api}"),
                &format!("the platform android-{api} ([android] target_sdk)"),
            ))
        }
    }

    /// The result's `tools` entries.
    pub fn to_json(&self) -> Value {
        json!({
            "android_sdk": crate::paths::display(&self.sdk.root),
            "ndk": self.ndk.as_ref().ok().map(|ndk| ndk.version.clone()),
            "jdk": self.jdk.as_ref().ok().map(|jdk| jdk.version.clone()),
            "build_tools": self.sdk.build_tools(MIN_BUILD_TOOLS).map(|(version, _)| version),
        })
    }
}

/// Where icm keeps its debug keystore: next to host.toml
/// (`~/.config/icm/android/debug.keystore`), so every project installs
/// over the others' builds and `adb install -r` keeps working across runs.
/// `~/.android/debug.keystore` (Android Studio's) is never touched.
pub fn debug_keystore() -> PathBuf {
    crate::paths::host_config()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(crate::paths::cache_dir)
        .join("android")
        .join("debug.keystore")
}

/// The debug keystore's fixed, public credentials (the Android convention).
pub const DEBUG_KEYSTORE_PASS: &str = "android";
/// The debug key's alias.
pub const DEBUG_KEY_ALIAS: &str = "androiddebugkey";
