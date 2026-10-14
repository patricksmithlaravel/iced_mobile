//! Tool discovery (design §3 "Tool discovery", Appendix C items 1 and 2).
//!
//! | Tool | Search order |
//! |---|---|
//! | Android SDK | host.toml `android_sdk` → `$ANDROID_HOME` → `$ANDROID_SDK_ROOT` → `~/Library/Android/sdk` → `/opt/homebrew/share/android-commandlinetools` → `/usr/local/share/android-commandlinetools` → `~/Android/Sdk` |
//! | NDK | host.toml `android_ndk` → `$ANDROID_NDK_HOME` → `$ANDROID_NDK_ROOT` → highest `$SDK/ndk/*` ≥ r28 |
//! | JDK (17+) | host.toml `java_home` → `$JAVA_HOME` → `/usr/libexec/java_home -v 17+` → Homebrew `openjdk@21`/`@17`/`openjdk` → `/Library/Java/JavaVirtualMachines/*` → Android Studio's JBR → `/usr/lib/jvm/*` |
//! | Xcode | `$DEVELOPER_DIR` → `xcode-select -p` |
//! | Chrome | host.toml `chrome` → `$ICM_CHROME` → `/Applications/Google Chrome.app` → `~/Applications/...` → Chromium → `PATH` |
//! | wasm-bindgen | `<cache>/tools/wasm-bindgen/<lock version>/bin/wasm-bindgen`; a `PATH` copy only if its version matches |
//!
//! Every candidate is verified (a JDK's version is read, never assumed:
//! `java_home -v 17+` returns a Java 8 applet plugin on this host).
//! `ICM_TOOL_<NAME>` overrides any external tool path.
//!
//! Android children get `JAVA_HOME=<jdk>` and `<jdk>/bin` first on `PATH`
//! ([`android_env`]): `/usr/bin/java` may be Java 8, and sdkmanager,
//! avdmanager, apksigner, keytool and jarsigner all need 17+.

use crate::catalogue::CheckId;
use crate::error::{Evidence, IcmError};
use crate::host::HostConfig;
use crate::process::{self, Cmd};
use serde::Serialize;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The lowest NDK major release icm accepts (r28: 16 KB pages by default).
pub const MIN_NDK_MAJOR: u32 = 28;

/// The lowest JDK major release the Android tools accept.
pub const MIN_JDK_MAJOR: u32 = 17;

/// The environment discovery reads (a snapshot, so tests can fake it).
#[derive(Clone, Debug, Default)]
pub struct Env {
    vars: BTreeMap<String, String>,
    home: Option<PathBuf>,
}

impl Env {
    /// The process environment.
    pub fn from_process() -> Env {
        Env {
            vars: std::env::vars().collect(),
            home: crate::paths::home(),
        }
    }

    /// An explicit environment (tests).
    pub fn from_pairs(pairs: &[(&str, &str)], home: Option<&Path>) -> Env {
        Env {
            vars: pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            home: home.map(Path::to_path_buf),
        }
    }

    /// A non-empty variable.
    pub fn var(&self, name: &str) -> Option<&str> {
        self.vars
            .get(name)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    /// The home directory.
    pub fn home(&self) -> Option<&Path> {
        self.home.as_deref()
    }

    /// `ICM_TOOL_<NAME>`, if set.
    pub fn tool_override(&self, name: &str) -> Option<PathBuf> {
        let var = format!(
            "ICM_TOOL_{}",
            name.to_ascii_uppercase().replace(['-', '.'], "_")
        );
        self.var(&var).map(PathBuf::from)
    }

    fn expand(&self, path: &str) -> PathBuf {
        match (path.strip_prefix("~/"), &self.home) {
            (Some(rest), Some(home)) => home.join(rest),
            _ => PathBuf::from(path),
        }
    }
}

/// A discovered tool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Found {
    /// The executable (or directory).
    pub path: PathBuf,
    /// Its version, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Where it was found (`host.toml`, `$ANDROID_HOME`, `autodetect`, ...).
    pub source: String,
}

// ---- Android SDK ---------------------------------------------------------------

/// An Android SDK.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AndroidSdk {
    /// The SDK root.
    pub root: PathBuf,
    /// Where it was found.
    pub source: String,
}

fn is_sdk(dir: &Path) -> bool {
    [
        "platform-tools",
        "cmdline-tools",
        "build-tools",
        "platforms",
        "tools",
    ]
    .iter()
    .any(|sub| dir.join(sub).is_dir())
}

/// Finds the Android SDK.
pub fn android_sdk(host: &HostConfig, env: &Env) -> Result<AndroidSdk, IcmError> {
    let mut candidates: Vec<(PathBuf, String)> = Vec::new();
    if let Some(path) = host.android_sdk.as_deref().filter(|p| !p.is_empty()) {
        candidates.push((env.expand(path), "host.toml android_sdk".to_string()));
    }
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(path) = env.var(var) {
            candidates.push((env.expand(path), format!("${var}")));
        }
    }
    if let Some(home) = env.home() {
        candidates.push((home.join("Library/Android/sdk"), "autodetect".to_string()));
    }
    candidates.push((
        PathBuf::from("/opt/homebrew/share/android-commandlinetools"),
        "autodetect".to_string(),
    ));
    candidates.push((
        PathBuf::from("/usr/local/share/android-commandlinetools"),
        "autodetect".to_string(),
    ));
    if let Some(home) = env.home() {
        candidates.push((home.join("Android/Sdk"), "autodetect".to_string()));
    }

    let tried: Vec<String> = candidates
        .iter()
        .map(|(path, _)| path.display().to_string())
        .collect();

    candidates
        .into_iter()
        .find(|(path, _)| is_sdk(path))
        .map(|(root, source)| AndroidSdk { root, source })
        .ok_or_else(|| {
            IcmError::new(
                CheckId::EnvAndroidSdkMissing,
                format!("no Android SDK found; tried {}", tried.join(", ")),
            )
            .fix_commands(["brew install --cask android-commandlinetools"])
        })
}

impl AndroidSdk {
    fn tool(&self, env: &Env, name: &str, relative: &str) -> PathBuf {
        env.tool_override(name)
            .unwrap_or_else(|| self.root.join(relative))
    }

    /// `platform-tools/adb`.
    pub fn adb(&self, env: &Env) -> PathBuf {
        self.tool(env, "adb", "platform-tools/adb")
    }

    /// `emulator/emulator`.
    pub fn emulator(&self, env: &Env) -> PathBuf {
        self.tool(env, "emulator", "emulator/emulator")
    }

    /// The `cmdline-tools` bin directory: `latest`, else the highest
    /// version, else the legacy `tools/bin`.
    pub fn cmdline_bin(&self) -> Option<PathBuf> {
        let base = self.root.join("cmdline-tools");
        let latest = base.join("latest").join("bin");
        if latest.is_dir() {
            return Some(latest);
        }
        let mut versions: Vec<(Vec<u32>, PathBuf)> = std::fs::read_dir(&base)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.join("bin").is_dir())
            .map(|path| {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                (numeric_parts(&name), path.join("bin"))
            })
            .collect();
        versions.sort();
        versions.pop().map(|(_, path)| path).or_else(|| {
            let legacy = self.root.join("tools").join("bin");
            legacy.is_dir().then_some(legacy)
        })
    }

    /// `sdkmanager` (or its `ICM_TOOL_SDKMANAGER` override).
    pub fn sdkmanager(&self, env: &Env) -> Option<PathBuf> {
        env.tool_override("sdkmanager")
            .or_else(|| self.cmdline_bin().map(|bin| bin.join("sdkmanager")))
            .filter(|path| path.exists())
    }

    /// `avdmanager` (or its `ICM_TOOL_AVDMANAGER` override).
    pub fn avdmanager(&self, env: &Env) -> Option<PathBuf> {
        env.tool_override("avdmanager")
            .or_else(|| self.cmdline_bin().map(|bin| bin.join("avdmanager")))
            .filter(|path| path.exists())
    }

    /// Installed build-tools versions, highest first.
    pub fn build_tools_versions(&self) -> Vec<String> {
        let mut versions: Vec<(Vec<u32>, String)> =
            std::fs::read_dir(self.root.join("build-tools"))
                .map(|read| {
                    read.flatten()
                        .filter(|entry| entry.path().is_dir())
                        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
                        .map(|name| (numeric_parts(&name), name))
                        .collect()
                })
                .unwrap_or_default();
        versions.sort();
        versions.reverse();
        versions.into_iter().map(|(_, name)| name).collect()
    }

    /// The highest build-tools directory whose major version is at least `min_major`.
    pub fn build_tools(&self, min_major: u32) -> Option<(String, PathBuf)> {
        self.build_tools_versions()
            .into_iter()
            .find(|version| numeric_parts(version).first().copied().unwrap_or(0) >= min_major)
            .map(|version| {
                let dir = self.root.join("build-tools").join(&version);
                (version, dir)
            })
    }

    /// `platforms/android-<api>/android.jar`.
    pub fn platform_jar(&self, api: u32) -> PathBuf {
        self.root
            .join("platforms")
            .join(format!("android-{api}"))
            .join("android.jar")
    }

    /// `system-images/android-<api>/<tag>/<abi>`.
    pub fn system_image(&self, api: &str, tag: &str, abi: &str) -> PathBuf {
        self.root
            .join("system-images")
            .join(format!("android-{api}"))
            .join(tag)
            .join(abi)
    }

    /// The NDKs installed under `ndk/` (and the legacy `ndk-bundle`).
    pub fn ndks(&self) -> Vec<Ndk> {
        let mut found: Vec<Ndk> = std::fs::read_dir(self.root.join("ndk"))
            .map(|read| {
                read.flatten()
                    .filter_map(|entry| Ndk::at(&entry.path(), "$SDK/ndk"))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(bundle) = Ndk::at(&self.root.join("ndk-bundle"), "$SDK/ndk-bundle") {
            found.push(bundle);
        }
        found.sort_by_key(|ndk| std::cmp::Reverse(numeric_parts(&ndk.version)));
        found
    }
}

// ---- NDK ---------------------------------------------------------------------

/// An Android NDK.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Ndk {
    /// The NDK root.
    pub root: PathBuf,
    /// `Pkg.Revision`, e.g. `29.0.14206865`.
    pub version: String,
    /// The major release, e.g. 29.
    pub major: u32,
    /// Where it was found.
    pub source: String,
}

impl Ndk {
    /// Reads an NDK's `source.properties`.
    pub fn at(root: &Path, source: &str) -> Option<Ndk> {
        let properties = std::fs::read_to_string(root.join("source.properties")).ok()?;
        let version = properties.lines().find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "Pkg.Revision").then(|| value.trim().to_string())
        })?;
        let major = numeric_parts(&version).first().copied()?;
        Some(Ndk {
            root: root.to_path_buf(),
            version,
            major,
            source: source.to_string(),
        })
    }

    /// The prebuilt host tag (`darwin-x86_64` holds universal binaries).
    pub fn host_tag() -> &'static str {
        if cfg!(target_os = "macos") {
            "darwin-x86_64"
        } else {
            "linux-x86_64"
        }
    }

    /// `toolchains/llvm/prebuilt/<host>/bin`.
    pub fn toolchain_bin(&self) -> PathBuf {
        self.root
            .join("toolchains")
            .join("llvm")
            .join("prebuilt")
            .join(Ndk::host_tag())
            .join("bin")
    }
}

/// Finds an NDK r28 or newer.
pub fn ndk(sdk: Option<&AndroidSdk>, host: &HostConfig, env: &Env) -> Result<Ndk, IcmError> {
    let mut explicit: Vec<(PathBuf, String)> = Vec::new();
    if let Some(path) = host.android_ndk.as_deref().filter(|p| !p.is_empty()) {
        explicit.push((env.expand(path), "host.toml android_ndk".to_string()));
    }
    for var in ["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT"] {
        if let Some(path) = env.var(var) {
            explicit.push((env.expand(path), format!("${var}")));
        }
    }

    let mut too_old: Vec<Ndk> = Vec::new();
    for (path, source) in explicit {
        match Ndk::at(&path, &source) {
            Some(found) if found.major >= MIN_NDK_MAJOR => return Ok(found),
            Some(found) => too_old.push(found),
            None => {}
        }
    }

    if let Some(sdk) = sdk {
        for found in sdk.ndks() {
            if found.major >= MIN_NDK_MAJOR {
                return Ok(found);
            }
            too_old.push(found);
        }
    }

    let detail = if too_old.is_empty() {
        "no Android NDK found".to_string()
    } else {
        format!(
            "only older NDKs found: {} (need r{MIN_NDK_MAJOR} or newer)",
            too_old
                .iter()
                .map(|n| format!("{} at {}", n.version, n.root.display()))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    Err(IcmError::new(CheckId::EnvNdkTooOld, detail)
        .fix_commands(["icm doctor android --fix --yes"]))
}

/// The NDK environment for building `triple` at API `min_sdk`, computed in
/// Rust (no shell). `icm print env android` prints the same values.
pub fn ndk_env(ndk: &Ndk, triple: &str, min_sdk: u32) -> Vec<(String, String)> {
    let bin = ndk.toolchain_bin();
    let clang_prefix = match triple {
        "armv7-linux-androideabi" => "armv7a-linux-androideabi",
        other => other,
    };
    let upper = triple.to_ascii_uppercase().replace('-', "_");
    let lower = triple.replace('-', "_");
    let clang = bin.join(format!("{clang_prefix}{min_sdk}-clang"));
    let clangxx = bin.join(format!("{clang_prefix}{min_sdk}-clang++"));
    let show = |path: PathBuf| path.display().to_string();

    vec![
        (format!("CARGO_TARGET_{upper}_LINKER"), show(clang.clone())),
        (format!("CC_{lower}"), show(clang)),
        (format!("CXX_{lower}"), show(clangxx)),
        (format!("AR_{lower}"), show(bin.join("llvm-ar"))),
        (format!("RANLIB_{lower}"), show(bin.join("llvm-ranlib"))),
        (
            "ANDROID_NDK_HOME".to_string(),
            ndk.root.display().to_string(),
        ),
    ]
}

// ---- JDK -----------------------------------------------------------------------

/// A JDK.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Jdk {
    /// `JAVA_HOME`.
    pub home: PathBuf,
    /// The full version, e.g. `21.0.12.1`.
    pub version: String,
    /// The major release, e.g. 21.
    pub major: u32,
    /// Where it was found.
    pub source: String,
}

/// The major release of a Java version string (`1.8.0_431` → 8, `21.0.1` → 21).
pub fn java_major(version: &str) -> Option<u32> {
    let parts = numeric_parts(version);
    match parts.as_slice() {
        [1, minor, ..] => Some(*minor),
        [major, ..] => Some(*major),
        [] => None,
    }
}

/// The time limit of each JDK probe: a home's `java -version` and
/// `/usr/libexec/java_home -v 17+`.
const JDK_PROBE_LIMIT: Duration = Duration::from_secs(20);

/// The tool that names the JDK 17+ the system picks.
const JAVA_HOME_TOOL: &str = "/usr/libexec/java_home";

/// Reads a JDK home's version from its `release` file, or by running
/// `bin/java -version`. `Ok(None)` when the home holds no readable JDK; a
/// `java -version` that runs out of its limit is an error (`step.timeout`),
/// not a home without a JDK.
pub fn jdk_at(home: &Path, source: &str) -> Result<Option<Jdk>, IcmError> {
    jdk_at_within(home, source, JDK_PROBE_LIMIT)
}

/// [`jdk_at`] with the limit of its `java -version` probe.
fn jdk_at_within(home: &Path, source: &str, limit: Duration) -> Result<Option<Jdk>, IcmError> {
    if !home.join("bin").join("java").exists() {
        return Ok(None);
    }

    let from_release = std::fs::read_to_string(home.join("release"))
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                let value = line.strip_prefix("JAVA_VERSION=")?;
                Some(value.trim().trim_matches('"').to_string())
            })
        });

    let version = match from_release {
        Some(version) => version,
        None => {
            let cmd = Cmd::new(home.join("bin").join("java"))
                .arg("-version")
                .timeout(limit);
            let Ok(outcome) = process::run(&cmd, None, None) else {
                return Ok(None);
            };
            if let Some(error) =
                crate::context::probe_end_error("java -version", &cmd, &outcome, false)
            {
                return Err(error);
            }
            let text = format!("{}{}", outcome.stdout_text(), outcome.stderr_text());
            let Some(start) = text.find('"').map(|at| at + 1) else {
                return Ok(None);
            };
            let Some(end) = text[start..].find('"').map(|at| start + at) else {
                return Ok(None);
            };
            text[start..end].to_string()
        }
    };

    Ok(java_major(&version).map(|major| Jdk {
        home: home.to_path_buf(),
        major,
        version,
        source: source.to_string(),
    }))
}

/// Finds a JDK 17 or newer. `probe_java_home` runs
/// `/usr/libexec/java_home -v 17+` (off in tests).
pub fn jdk(host: &HostConfig, env: &Env, probe_java_home: bool) -> Result<Jdk, IcmError> {
    jdk_within(
        host,
        env,
        probe_java_home,
        Path::new(JAVA_HOME_TOOL),
        JDK_PROBE_LIMIT,
    )
}

/// [`jdk`] with the `java_home` that runs and the limit of the JDK probes (a
/// test passes fake ones that run out of time).
///
/// A home whose `java -version` runs out of its limit does not end the
/// search: that home may hold any JDK, so a JDK 17+ further on is still the
/// answer, and the timeout is returned only when none is found. A timed-out
/// `java_home -v 17+` does end it, at its place in the order, since a home
/// below it could be another JDK than the one it would have named.
fn jdk_within(
    host: &HostConfig,
    env: &Env,
    probe_java_home: bool,
    java_home: &Path,
    limit: Duration,
) -> Result<Jdk, IcmError> {
    let mut candidates: Vec<(PathBuf, String)> = Vec::new();

    if let Some(path) = host.java_home.as_deref().filter(|p| !p.is_empty()) {
        candidates.push((env.expand(path), "host.toml java_home".to_string()));
    }
    if let Some(path) = env.var("JAVA_HOME") {
        candidates.push((env.expand(path), "$JAVA_HOME".to_string()));
    }
    if probe_java_home {
        // Its place in the order, marked by its source (no other candidate has
        // it): `java_home_v17` runs when the search gets there, so the homes
        // above it come first.
        candidates.push((PathBuf::new(), JAVA_HOME_V17.to_string()));
    }
    for prefix in ["/opt/homebrew/opt", "/usr/local/opt"] {
        for formula in ["openjdk@21", "openjdk@17", "openjdk"] {
            candidates.push((
                Path::new(prefix)
                    .join(formula)
                    .join("libexec/openjdk.jdk/Contents/Home"),
                format!("Homebrew {formula}"),
            ));
        }
    }
    for base in ["/Library/Java/JavaVirtualMachines"]
        .iter()
        .map(PathBuf::from)
        .chain(
            env.home()
                .map(|h| h.join("Library/Java/JavaVirtualMachines")),
        )
    {
        let mut homes: Vec<PathBuf> = std::fs::read_dir(&base)
            .map(|read| {
                read.flatten()
                    .map(|entry| entry.path().join("Contents/Home"))
                    .collect()
            })
            .unwrap_or_default();
        homes.sort();
        homes.reverse();
        candidates.extend(
            homes
                .into_iter()
                .map(|home| (home, base.display().to_string())),
        );
    }
    candidates.push((
        PathBuf::from("/Applications/Android Studio.app/Contents/jbr/Contents/Home"),
        "Android Studio JBR".to_string(),
    ));
    let mut linux: Vec<PathBuf> = std::fs::read_dir("/usr/lib/jvm")
        .map(|read| read.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    linux.sort();
    linux.reverse();
    candidates.extend(
        linux
            .into_iter()
            .map(|home| (home, "/usr/lib/jvm".to_string())),
    );

    let mut rejected: Vec<String> = Vec::new();
    let mut stalled: Option<IcmError> = None;
    for (home, source) in candidates {
        // java_home -v 17+ ends the search when it runs out of its limit (see
        // `jdk_within`); a home that stalls is remembered and the search goes on.
        let (home, source) = if source == JAVA_HOME_V17 {
            match java_home_v17(java_home, limit)? {
                Some(candidate) => candidate,
                None => continue,
            }
        } else {
            (home, source)
        };
        match jdk_at_within(&home, &source, limit) {
            Ok(Some(found)) if found.major >= MIN_JDK_MAJOR => return Ok(found),
            Ok(Some(found)) => rejected.push(format!(
                "{} is Java {} ({})",
                found.home.display(),
                found.version,
                found.source
            )),
            Ok(None) => {}
            Err(error) => {
                stalled.get_or_insert(error);
            }
        }
    }
    if let Some(error) = stalled {
        return Err(error);
    }

    let mut detail = format!("no JDK {MIN_JDK_MAJOR} or newer found");
    if !rejected.is_empty() {
        detail.push_str(&format!("; too old: {}", rejected.join("; ")));
    }
    Err(IcmError::new(CheckId::EnvJdkMissing, detail).fix_commands(["brew install openjdk@21"]))
}

/// The source of the JDK that `/usr/libexec/java_home -v 17+` names.
const JAVA_HOME_V17: &str = "/usr/libexec/java_home -v 17+";

/// The home `java_home -v 17+` (`program`) names, with its source, when it
/// names one (`None` when the tool is absent or names nothing). A run that
/// runs out of its limit is an error (`step.timeout`).
fn java_home_v17(program: &Path, limit: Duration) -> Result<Option<(PathBuf, String)>, IcmError> {
    if !program.exists() {
        return Ok(None);
    }
    let cmd = Cmd::new(program).args(["-v", "17+"]).timeout(limit);
    let Ok(outcome) = process::run(&cmd, None, None) else {
        return Ok(None);
    };
    if let Some(error) = crate::context::probe_end_error(JAVA_HOME_V17, &cmd, &outcome, false) {
        return Err(error);
    }
    let path = outcome.stdout_text().trim().to_string();
    Ok((outcome.success() && !path.is_empty())
        .then(|| (PathBuf::from(path), JAVA_HOME_V17.to_string())))
}

/// The environment Android tool children need (Appendix C item 2):
/// `JAVA_HOME`, `PATH` with `<jdk>/bin` first and the SDK tool directories
/// after it, `ANDROID_HOME`/`ANDROID_SDK_ROOT` and `ANDROID_NDK_HOME`.
pub fn android_env(
    jdk: Option<&Jdk>,
    sdk: Option<&AndroidSdk>,
    ndk: Option<&Ndk>,
    base_path: &str,
) -> Vec<(String, String)> {
    let mut vars = Vec::new();
    let mut path: Vec<String> = Vec::new();

    if let Some(jdk) = jdk {
        vars.push(("JAVA_HOME".to_string(), jdk.home.display().to_string()));
        path.push(jdk.home.join("bin").display().to_string());
    }
    if let Some(sdk) = sdk {
        let root = sdk.root.display().to_string();
        vars.push(("ANDROID_HOME".to_string(), root.clone()));
        vars.push(("ANDROID_SDK_ROOT".to_string(), root));
        path.push(sdk.root.join("platform-tools").display().to_string());
        path.push(sdk.root.join("emulator").display().to_string());
        if let Some(bin) = sdk.cmdline_bin() {
            path.push(bin.display().to_string());
        }
        if let Some((_, dir)) = sdk.build_tools(0) {
            path.push(dir.display().to_string());
        }
    }
    if let Some(ndk) = ndk {
        vars.push((
            "ANDROID_NDK_HOME".to_string(),
            ndk.root.display().to_string(),
        ));
    }

    for entry in base_path.split(':').filter(|e| !e.is_empty()) {
        if !path.iter().any(|p| p == entry) {
            path.push(entry.to_string());
        }
    }
    vars.push(("PATH".to_string(), path.join(":")));
    vars
}

// ---- Xcode -----------------------------------------------------------------------

/// The selected Xcode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Xcode {
    /// `.../Xcode.app/Contents/Developer`.
    pub developer_dir: PathBuf,
    /// e.g. `27.0`.
    pub version: String,
    /// e.g. `27A266a`.
    pub build: String,
    /// Whether it looks like a beta (Appendix C item 1).
    pub beta: bool,
    /// Where it was found.
    pub source: String,
}

impl Xcode {
    /// The major version.
    pub fn major(&self) -> u32 {
        numeric_parts(&self.version).first().copied().unwrap_or(0)
    }

    /// `27.0 (27A266a)`.
    pub fn display(&self) -> String {
        format!("{} ({})", self.version, self.build)
    }

    /// `xcrun` (or `ICM_TOOL_XCRUN`) bound to this Xcode through
    /// `DEVELOPER_DIR`, so every Xcode tool (simctl, actool, plutil,
    /// codesign helpers) comes from the same Xcode.
    pub fn xcrun(&self) -> Cmd {
        Cmd::tool("xcrun").env("DEVELOPER_DIR", &self.developer_dir)
    }

    /// The Xcode app's `Info.plist` (DTXcode and friends derive from it).
    pub fn info_plist(&self) -> PathBuf {
        self.developer_dir
            .parent()
            .unwrap_or(&self.developer_dir)
            .join("Info.plist")
    }
}

/// Whether an Xcode is a beta: the build's numeric part after the first
/// letter has 4+ digits and starts with 5 (`16A5230g`), or the path says
/// "beta". A trailing lowercase letter alone (`27A266a`, a release) is not.
pub fn is_beta_xcode(build: &str, path: &Path) -> bool {
    if path.to_string_lossy().to_ascii_lowercase().contains("beta") {
        return true;
    }
    let after_letter: String = build
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start_matches(|c: char| c.is_ascii_alphabetic())
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    after_letter.len() >= 4 && after_letter.starts_with('5')
}

/// Parses `xcodebuild -version` output into (version, build).
pub fn parse_xcodebuild_version(text: &str) -> Option<(String, String)> {
    let version = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("Xcode "))?
        .trim()
        .to_string();
    let build = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("Build version "))?
        .trim()
        .to_string();
    Some((version, build))
}

/// `xcode-select -p`: the selected developer directory. `program` is the
/// tool that runs and `limit` its time limit (a test passes a fake that runs
/// out of time). A run killed at its limit is `step.timeout`, not a missing
/// Xcode.
fn selected_developer_dir(program: &OsStr, limit: Duration) -> Result<PathBuf, IcmError> {
    let cmd = Cmd::new(program).arg("-p").timeout(limit);
    let outcome = process::run(&cmd, None, None).map_err(|error| {
        IcmError::new(CheckId::EnvXcodeMissing, format!("xcode-select: {error}"))
    })?;
    if let Some(error) = crate::context::probe_end_error("xcode-select -p", &cmd, &outcome, false) {
        return Err(error);
    }
    if !outcome.success() {
        return Err(IcmError::new(
            CheckId::EnvXcodeMissing,
            format!("xcode-select -p failed: {}", outcome.reason(3)),
        ));
    }
    Ok(PathBuf::from(outcome.stdout_text().trim()))
}

/// Finds the selected Xcode and reads its version.
pub fn xcode(env: &Env) -> Result<Xcode, IcmError> {
    let (developer_dir, source) = match env.var("DEVELOPER_DIR") {
        Some(dir) => (PathBuf::from(dir), "$DEVELOPER_DIR".to_string()),
        None => (
            selected_developer_dir(&process::tool_path("xcode-select"), Duration::from_secs(20))?,
            "xcode-select -p".to_string(),
        ),
    };

    if developer_dir.ends_with("CommandLineTools") || !developer_dir.join("usr/bin").is_dir() {
        return Err(IcmError::new(
            CheckId::EnvXcodeMissing,
            format!(
                "the selected developer directory {} is not a full Xcode",
                developer_dir.display()
            ),
        )
        .fix_commands(["sudo xcode-select -s /Applications/Xcode.app"]));
    }

    let version = Cmd::tool("xcodebuild")
        .arg("-version")
        .env("DEVELOPER_DIR", &developer_dir)
        .timeout(Duration::from_secs(60));
    let outcome = process::run(&version, None, None)
        .map_err(|error| IcmError::new(CheckId::EnvXcodeMissing, format!("xcodebuild: {error}")))?;
    // Killed at its limit or by a signal to icm: Xcode may be fine.
    if let Some(error) =
        crate::context::probe_end_error("xcodebuild.version", &version, &outcome, false)
    {
        return Err(error);
    }

    let text = format!("{}{}", outcome.stdout_text(), outcome.stderr_text());
    if text.to_ascii_lowercase().contains("license") && !outcome.success() {
        return Err(IcmError::new(
            CheckId::EnvLicensesNotAccepted,
            "the Xcode licence has not been accepted",
        )
        .fix_commands(["sudo xcodebuild -license"])
        .evidence(Evidence::file(&developer_dir)));
    }

    let (version, build) = parse_xcodebuild_version(&text).ok_or_else(|| {
        // Silent output says so, and how the command ended, rather than
        // leaving the detail empty.
        let shown = if text.trim().is_empty() {
            format!("the version output was empty ({})", outcome.describe())
        } else {
            text.trim().to_string()
        };
        IcmError::new(
            CheckId::EnvXcodeMissing,
            format!("cannot read the Xcode version: {shown}"),
        )
    })?;

    Ok(Xcode {
        beta: is_beta_xcode(&build, &developer_dir),
        developer_dir,
        version,
        build,
        source,
    })
}

// ---- Chrome ------------------------------------------------------------------------

/// Finds Chrome or Chromium.
pub fn chrome(host: &HostConfig, env: &Env) -> Result<Found, IcmError> {
    let mut candidates: Vec<(PathBuf, String)> = Vec::new();
    if let Some(path) = host.chrome.as_deref().filter(|p| !p.is_empty()) {
        candidates.push((env.expand(path), "host.toml chrome".to_string()));
    }
    if let Some(path) = env.var("ICM_CHROME") {
        candidates.push((env.expand(path), "$ICM_CHROME".to_string()));
    }
    let app = "Google Chrome.app/Contents/MacOS/Google Chrome";
    candidates.push((
        Path::new("/Applications").join(app),
        "autodetect".to_string(),
    ));
    if let Some(home) = env.home() {
        candidates.push((
            home.join("Applications").join(app),
            "autodetect".to_string(),
        ));
    }
    candidates.push((
        PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"),
        "autodetect".to_string(),
    ));
    for name in [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
    ] {
        if let Some(path) = crate::paths::which(name) {
            candidates.push((path, "PATH".to_string()));
        }
    }

    candidates
        .into_iter()
        .find(|(path, _)| crate::paths::is_executable(path))
        .map(|(path, source)| Found {
            path,
            version: None,
            source,
        })
        .ok_or_else(|| {
            IcmError::new(CheckId::EnvChromeMissing, "no Chrome or Chromium found")
                .fix_commands(["brew install --cask google-chrome"])
        })
}

// ---- wasm-bindgen ------------------------------------------------------------------

/// Where `icm doctor web --fix --yes` installs wasm-bindgen `version`
/// (`cargo install --root <this>`).
pub fn wasm_bindgen_root(version: &str) -> PathBuf {
    crate::paths::tools_dir().join("wasm-bindgen").join(version)
}

/// The command that installs the wasm-bindgen CLI matching the app's lock.
pub fn wasm_bindgen_install(version: &str) -> String {
    format!(
        "cargo install wasm-bindgen-cli --version {version} --locked --root {}",
        process::shell_quote(&wasm_bindgen_root(version).display().to_string())
    )
}

/// The time limit of the `wasm-bindgen --version` probe.
const WASM_BINDGEN_PROBE_LIMIT: Duration = Duration::from_secs(20);

/// Finds a wasm-bindgen CLI whose version equals the app's lock version.
pub fn wasm_bindgen(version: &str, env: &Env) -> Result<Found, IcmError> {
    wasm_bindgen_on(
        version,
        env,
        crate::paths::which("wasm-bindgen"),
        WASM_BINDGEN_PROBE_LIMIT,
    )
}

/// [`wasm_bindgen`] with the CLI found on `PATH` (`on_path`) and the limit of
/// its `--version` probe (a test passes a fake CLI that runs out of time).
fn wasm_bindgen_on(
    version: &str,
    env: &Env,
    on_path: Option<PathBuf>,
    limit: Duration,
) -> Result<Found, IcmError> {
    if let Some(path) = env.tool_override("wasm-bindgen") {
        return Ok(Found {
            path,
            version: Some(version.to_string()),
            source: "$ICM_TOOL_WASM_BINDGEN".to_string(),
        });
    }

    let cached = wasm_bindgen_root(version).join("bin").join("wasm-bindgen");
    if crate::paths::is_executable(&cached) {
        return Ok(Found {
            path: cached,
            version: Some(version.to_string()),
            source: "icm cache".to_string(),
        });
    }

    let mut mismatch = None;
    if let Some(path) = on_path {
        let cmd = Cmd::new(&path).arg("--version").timeout(limit);
        if let Ok(outcome) = process::run(&cmd, None, None) {
            // Killed at its limit: the CLI was not read, so it is a timeout,
            // not a CLI of another version.
            if let Some(error) =
                crate::context::probe_end_error("wasm-bindgen --version", &cmd, &outcome, false)
            {
                return Err(error);
            }
            let found = outcome
                .stdout_text()
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .to_string();
            if found.is_empty() {
                mismatch = Some(format!(
                    "{} printed no version ({})",
                    path.display(),
                    outcome.reason(3)
                ));
            } else if found == version {
                return Ok(Found {
                    path,
                    version: Some(found),
                    source: "PATH".to_string(),
                });
            } else {
                mismatch = Some(format!("{} is version {found}", path.display()));
            }
        }
    }

    let mut detail = format!("no wasm-bindgen CLI {version} (the app's Cargo.lock version)");
    if let Some(mismatch) = mismatch {
        detail.push_str(&format!("; {mismatch}"));
    }
    Err(
        IcmError::new(CheckId::DepsWasmBindgenCli, detail).fix_commands([
            "icm doctor web --fix --yes".to_string(),
            wasm_bindgen_install(version),
        ]),
    )
}

// ---- helpers -------------------------------------------------------------------------

/// The numbers in a version string: `29.0.14206865` → [29, 0, 14206865].
pub fn numeric_parts(version: &str) -> Vec<u32> {
    version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn touch_exe(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn fake_sdk(root: &Path, ndks: &[&str]) {
        std::fs::create_dir_all(root.join("platform-tools")).unwrap();
        touch_exe(&root.join("platform-tools/adb"));
        touch_exe(&root.join("cmdline-tools/latest/bin/sdkmanager"));
        touch_exe(&root.join("cmdline-tools/latest/bin/avdmanager"));
        std::fs::create_dir_all(root.join("build-tools/35.0.0")).unwrap();
        std::fs::create_dir_all(root.join("build-tools/36.0.0")).unwrap();
        std::fs::create_dir_all(root.join("platforms/android-36")).unwrap();
        for version in ndks {
            let dir = root.join("ndk").join(version);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("source.properties"),
                format!("Pkg.Desc = Android NDK\nPkg.Revision = {version}\n"),
            )
            .unwrap();
        }
    }

    #[test]
    fn sdk_search_order() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let from_env = tmp.path().join("env-sdk");
        let from_host = tmp.path().join("host-sdk");
        fake_sdk(&from_env, &[]);
        fake_sdk(&from_host, &[]);
        let env = Env::from_pairs(&[("ANDROID_HOME", from_env.to_str().unwrap())], Some(&home));

        let sdk = android_sdk(&HostConfig::default(), &env).unwrap();
        assert_eq!(sdk.root, from_env);
        assert_eq!(sdk.source, "$ANDROID_HOME");

        let host = HostConfig {
            android_sdk: Some(from_host.display().to_string()),
            ..HostConfig::default()
        };
        assert_eq!(android_sdk(&host, &env).unwrap().root, from_host);

        // The home-relative default.
        let in_home = home.join("Library/Android/sdk");
        fake_sdk(&in_home, &[]);
        let env = Env::from_pairs(&[], Some(&home));
        assert_eq!(
            android_sdk(&HostConfig::default(), &env).unwrap().root,
            in_home
        );
    }

    #[test]
    fn sdk_tools_resolve_under_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        fake_sdk(tmp.path(), &[]);
        let sdk = AndroidSdk {
            root: tmp.path().to_path_buf(),
            source: "test".into(),
        };
        let env = Env::default();
        assert_eq!(sdk.adb(&env), tmp.path().join("platform-tools/adb"));
        assert_eq!(sdk.emulator(&env), tmp.path().join("emulator/emulator"));
        assert_eq!(
            sdk.sdkmanager(&env).unwrap(),
            tmp.path().join("cmdline-tools/latest/bin/sdkmanager")
        );
        assert_eq!(sdk.build_tools_versions(), vec!["36.0.0", "35.0.0"]);
        assert_eq!(sdk.build_tools(36).unwrap().0, "36.0.0");
        assert!(sdk.build_tools(37).is_none());
        assert!(
            sdk.platform_jar(36)
                .ends_with("platforms/android-36/android.jar")
        );

        let overridden = Env::from_pairs(&[("ICM_TOOL_ADB", "/fake/adb")], None);
        assert_eq!(sdk.adb(&overridden), PathBuf::from("/fake/adb"));
    }

    #[test]
    fn the_highest_recent_ndk_wins() {
        let tmp = tempfile::tempdir().unwrap();
        fake_sdk(
            tmp.path(),
            &["27.2.12479018", "29.0.14206865", "28.1.13356709"],
        );
        let sdk = AndroidSdk {
            root: tmp.path().to_path_buf(),
            source: "test".into(),
        };
        let found = ndk(Some(&sdk), &HostConfig::default(), &Env::default()).unwrap();
        assert_eq!(found.version, "29.0.14206865");
        assert_eq!(found.major, 29);

        let old_only = tempfile::tempdir().unwrap();
        fake_sdk(old_only.path(), &["27.2.12479018"]);
        let sdk = AndroidSdk {
            root: old_only.path().to_path_buf(),
            source: "test".into(),
        };
        let error = ndk(Some(&sdk), &HostConfig::default(), &Env::default()).unwrap_err();
        assert_eq!(error.id, "env.ndk_too_old");
        assert!(error.detail.contains("27.2.12479018"));
    }

    #[test]
    fn ndk_env_names_the_triple() {
        let ndk = Ndk {
            root: PathBuf::from("/ndk"),
            version: "29.0.1".into(),
            major: 29,
            source: "test".into(),
        };
        let env: BTreeMap<String, String> = ndk_env(&ndk, "aarch64-linux-android", 26)
            .into_iter()
            .collect();
        let bin = format!("/ndk/toolchains/llvm/prebuilt/{}/bin", Ndk::host_tag());
        assert_eq!(
            env["CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER"],
            format!("{bin}/aarch64-linux-android26-clang")
        );
        assert_eq!(
            env["CXX_aarch64_linux_android"],
            format!("{bin}/aarch64-linux-android26-clang++")
        );
        assert_eq!(env["AR_aarch64_linux_android"], format!("{bin}/llvm-ar"));
        assert_eq!(env["ANDROID_NDK_HOME"], "/ndk");

        let armv7: BTreeMap<String, String> = ndk_env(&ndk, "armv7-linux-androideabi", 26)
            .into_iter()
            .collect();
        assert!(armv7["CC_armv7_linux_androideabi"].ends_with("armv7a-linux-androideabi26-clang"));
    }

    fn fake_jdk(home: &Path, version: &str) {
        touch_exe(&home.join("bin/java"));
        std::fs::write(
            home.join("release"),
            format!("JAVA_VERSION=\"{version}\"\n"),
        )
        .unwrap();
    }

    #[test]
    fn jdks_are_verified_not_assumed() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("jdk8");
        let new = tmp.path().join("jdk21");
        fake_jdk(&old, "1.8.0_431");
        fake_jdk(&new, "21.0.12.1");

        let env = Env::from_pairs(&[("JAVA_HOME", old.to_str().unwrap())], None);
        let host = HostConfig {
            java_home: Some(new.display().to_string()),
            ..HostConfig::default()
        };
        let found = jdk(&host, &env, false).unwrap();
        assert_eq!(found.home, new);
        assert_eq!(found.major, 21);

        assert_eq!(java_major("1.8.0_431"), Some(8));
        assert_eq!(java_major("17"), Some(17));
        assert_eq!(java_major("21.0.12.1"), Some(21));
        assert_eq!(jdk_at(&old, "x").unwrap().unwrap().major, 8);
    }

    #[test]
    fn a_java_that_runs_out_of_time_is_a_timeout_not_a_missing_jdk() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("jdk");
        std::fs::create_dir_all(home.join("bin")).unwrap();
        crate::process::fake_script(&home.join("bin"), "java", "sleep 30");
        let error = jdk_at_within(&home, "test", Duration::from_millis(300)).unwrap_err();
        assert_eq!(error.id, CheckId::StepTimeout.id(), "{error:?}");
        assert_eq!(error.exit.code(), 8);
        assert!(error.detail.contains("java -version"), "{}", error.detail);
        assert!(error.detail.contains("300ms"), "{}", error.detail);
    }

    #[test]
    fn a_selected_developer_dir_that_runs_out_of_time_is_a_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = crate::process::fake_script(tmp.path(), "xcode-select", "sleep 30");
        let error =
            selected_developer_dir(tool.as_os_str(), Duration::from_millis(300)).unwrap_err();
        assert_eq!(error.id, CheckId::StepTimeout.id(), "{error:?}");
        assert_eq!(error.exit.code(), 8);
        assert!(error.detail.contains("xcode-select -p"), "{}", error.detail);
        assert!(error.detail.contains("300ms"), "{}", error.detail);
    }

    #[test]
    fn a_silent_xcode_select_failure_says_its_status() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = crate::process::fake_script(tmp.path(), "xcode-select", "exit 1");
        let error = selected_developer_dir(tool.as_os_str(), Duration::from_secs(20)).unwrap_err();
        assert_eq!(error.id, CheckId::EnvXcodeMissing.id(), "{error:?}");
        assert!(error.detail.ends_with("failed: exit 1"), "{}", error.detail);
    }

    #[test]
    fn android_children_get_java_home_and_path() {
        let jdk = Jdk {
            home: PathBuf::from("/jdk"),
            version: "21".into(),
            major: 21,
            source: "test".into(),
        };
        let tmp = tempfile::tempdir().unwrap();
        fake_sdk(tmp.path(), &[]);
        let sdk = AndroidSdk {
            root: tmp.path().to_path_buf(),
            source: "test".into(),
        };
        let vars: BTreeMap<String, String> =
            android_env(Some(&jdk), Some(&sdk), None, "/usr/bin:/bin:/jdk/bin")
                .into_iter()
                .collect();
        assert_eq!(vars["JAVA_HOME"], "/jdk");
        let path: Vec<&str> = vars["PATH"].split(':').collect();
        assert_eq!(path[0], "/jdk/bin");
        assert_eq!(path[1], tmp.path().join("platform-tools").to_str().unwrap());
        assert!(path.contains(&"/usr/bin"));
        assert_eq!(path.iter().filter(|p| **p == "/jdk/bin").count(), 1);
        assert_eq!(vars["ANDROID_HOME"], tmp.path().to_str().unwrap());
    }

    #[test]
    fn xcode_beta_rule() {
        let release = Path::new("/Applications/Xcode.app/Contents/Developer");
        assert!(!is_beta_xcode("27A266a", release));
        assert!(is_beta_xcode("16C5032a", release));
        assert!(is_beta_xcode("16A5230g", release));
        assert!(!is_beta_xcode("16A242d", release));
        assert!(is_beta_xcode(
            "27A266a",
            Path::new("/Applications/Xcode-beta.app/Contents/Developer")
        ));
        assert_eq!(
            parse_xcodebuild_version("Xcode 27.0\nBuild version 27A266a\n"),
            Some(("27.0".to_string(), "27A266a".to_string()))
        );
    }

    #[test]
    fn chrome_search_honours_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("chrome");
        touch_exe(&fake);
        let host = HostConfig {
            chrome: Some(fake.display().to_string()),
            ..HostConfig::default()
        };
        let found = chrome(&host, &Env::default()).unwrap();
        assert_eq!(found.path, fake);
        assert_eq!(found.source, "host.toml chrome");
    }

    #[test]
    fn wasm_bindgen_install_command_pins_the_version() {
        let command = wasm_bindgen_install("0.2.105");
        assert!(
            command
                .starts_with("cargo install wasm-bindgen-cli --version 0.2.105 --locked --root ")
        );
        assert!(command.contains("wasm-bindgen/0.2.105"));
    }

    /// A JDK home at `dir` whose `bin/java` says it is `version`, on stderr as
    /// `java -version` does.
    fn fake_jdk_home(dir: &Path, version: &str) -> PathBuf {
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        crate::process::fake_script(
            &dir.join("bin"),
            "java",
            &format!("echo 'openjdk version \"{version}\"' >&2"),
        );
        dir.to_path_buf()
    }

    /// A JDK home at `dir` whose `bin/java` never answers.
    fn stalled_jdk_home(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        crate::process::fake_script(&dir.join("bin"), "java", "sleep 30");
        dir.to_path_buf()
    }

    #[test]
    fn a_stalled_java_home_does_not_hide_a_jdk_further_on() {
        let tmp = tempfile::tempdir().unwrap();
        let stalled = stalled_jdk_home(&tmp.path().join("stalled"));
        let good = fake_jdk_home(&tmp.path().join("good"), "21.0.4");
        let host = HostConfig {
            java_home: Some(stalled.display().to_string()),
            ..HostConfig::default()
        };
        let good_path = good.display().to_string();
        let env = Env::from_pairs(&[("JAVA_HOME", good_path.as_str())], None);
        // The search ends at the JDK after the stall, so `java_home` (a path
        // that does not exist here) is never reached.
        let missing = tmp.path().join("no-such-java_home");
        let found = jdk_within(&host, &env, true, &missing, Duration::from_millis(300)).unwrap();
        assert_eq!(found.major, 21);
        assert_eq!(found.source, "$JAVA_HOME");
    }

    #[test]
    fn a_java_home_that_runs_out_of_time_ends_the_search_as_a_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let java_home = crate::process::fake_script(tmp.path(), "java_home", "sleep 30");
        let error = jdk_within(
            &HostConfig::default(),
            &Env::default(),
            true,
            &java_home,
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert_eq!(error.id, CheckId::StepTimeout.id(), "{error:?}");
        assert_eq!(error.exit.code(), 8);
        assert!(error.detail.contains("-v 17+"), "{}", error.detail);
        assert!(error.detail.contains("300ms"), "{}", error.detail);
        assert!(error.fix.summary.contains("-v 17+"), "{error:?}");
    }

    #[test]
    fn a_java_home_that_names_a_jdk_is_used() {
        let tmp = tempfile::tempdir().unwrap();
        let home = fake_jdk_home(&tmp.path().join("jdk21"), "21.0.4");
        let script = format!(
            "echo {}",
            crate::process::shell_quote(&home.display().to_string())
        );
        let java_home = crate::process::fake_script(tmp.path(), "java_home", &script);
        let found = jdk_within(
            &HostConfig::default(),
            &Env::default(),
            true,
            &java_home,
            JDK_PROBE_LIMIT,
        )
        .unwrap();
        assert_eq!(found.major, 21);
        assert_eq!(found.source, JAVA_HOME_V17);
    }

    /// A version no cache holds, so a test never finds a real CLI.
    const UNCACHED_BINDGEN: &str = "0.0.0-icm-probe-test";

    #[test]
    fn a_wasm_bindgen_that_runs_out_of_time_is_a_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = crate::process::fake_script(tmp.path(), "wasm-bindgen", "sleep 30");
        let error = wasm_bindgen_on(
            UNCACHED_BINDGEN,
            &Env::default(),
            Some(cli),
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert_eq!(error.id, CheckId::StepTimeout.id(), "{error:?}");
        assert_eq!(error.exit.code(), 8);
        assert!(
            error.detail.contains("wasm-bindgen --version"),
            "{}",
            error.detail
        );
        assert!(error.detail.contains("300ms"), "{}", error.detail);
    }

    #[test]
    fn a_wasm_bindgen_that_prints_no_version_says_why() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = crate::process::fake_script(tmp.path(), "wasm-bindgen", "echo boom >&2; exit 1");
        let error = wasm_bindgen_on(
            UNCACHED_BINDGEN,
            &Env::default(),
            Some(cli),
            Duration::from_secs(20),
        )
        .unwrap_err();
        assert_eq!(error.id, CheckId::DepsWasmBindgenCli.id(), "{error:?}");
        assert!(
            error.detail.contains("printed no version (boom)"),
            "{}",
            error.detail
        );
    }

    #[test]
    fn a_wasm_bindgen_of_another_version_is_a_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let cli =
            crate::process::fake_script(tmp.path(), "wasm-bindgen", "echo 'wasm-bindgen 0.2.1'");
        let error = wasm_bindgen_on(
            UNCACHED_BINDGEN,
            &Env::default(),
            Some(cli),
            Duration::from_secs(20),
        )
        .unwrap_err();
        assert_eq!(error.id, CheckId::DepsWasmBindgenCli.id(), "{error:?}");
        assert!(
            error.detail.contains("is version 0.2.1"),
            "{}",
            error.detail
        );
    }

    #[test]
    fn a_wasm_bindgen_of_the_locked_version_is_found_on_path() {
        let tmp = tempfile::tempdir().unwrap();
        let script = format!("echo 'wasm-bindgen {UNCACHED_BINDGEN}'");
        let cli = crate::process::fake_script(tmp.path(), "wasm-bindgen", &script);
        let found = wasm_bindgen_on(
            UNCACHED_BINDGEN,
            &Env::default(),
            Some(cli.clone()),
            Duration::from_secs(20),
        )
        .unwrap();
        assert_eq!(found.path, cli);
        assert_eq!(found.source, "PATH");
    }
}
