//! The fixes `icm doctor --fix [--yes]` runs. Each is printed before it
//! runs and becomes a reported step with its own log. Commands are built
//! when the fix runs, from a fresh look at the machine, so a JDK or an SDK
//! package installed by an earlier fix in the same run is used by the
//! later ones.

use crate::catalogue::{By, CheckId};
use crate::error::IcmError;
use crate::host::HostConfig;
use crate::process::Cmd;
use crate::tools::{self, Env};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A repair doctor can run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fix {
    /// `rustup toolchain install [<name>]` in the project directory (the
    /// toolchain file's own targets and components come with it).
    ToolchainInstall {
        /// Where to run it.
        dir: PathBuf,
        /// The toolchain, when rustup named it.
        name: Option<String>,
    },
    /// `rustup target add --toolchain <name> <targets…>`.
    TargetAdd {
        /// Where to run it.
        dir: PathBuf,
        /// The project's toolchain.
        toolchain: String,
        /// The targets.
        targets: Vec<String>,
    },
    /// `brew install <formula>` (the JDK).
    Brew {
        /// The formula.
        formula: String,
    },
    /// `sdkmanager --sdk_root=<root> --install <packages…>`.
    SdkManager {
        /// sdkmanager.
        sdkmanager: PathBuf,
        /// The SDK root.
        sdk_root: PathBuf,
        /// The packages.
        packages: Vec<String>,
    },
    /// keytool creates the standard debug keystore.
    DebugKeystore {
        /// Where.
        path: PathBuf,
    },
    /// `avdmanager create avd` for the managed emulator.
    AvdCreate {
        /// avdmanager.
        avdmanager: PathBuf,
        /// The AVD name (`icm-api36`).
        name: String,
        /// The system-image package.
        image: String,
        /// The hardware profile.
        device: String,
    },
    /// `xcodebuild -downloadPlatform iOS` (about 8 GB).
    DownloadIosPlatform {
        /// The Xcode developer directory.
        developer_dir: PathBuf,
    },
    /// `xcrun simctl create` for the managed simulator.
    SimCreate {
        /// The Xcode developer directory.
        developer_dir: PathBuf,
        /// The name (`icm-iphone-17-ios-27.0`).
        name: String,
        /// The device type identifier.
        device_type: String,
        /// The runtime identifier.
        runtime: String,
    },
    /// `cargo generate-lockfile` (the first resolve of a new app).
    GenerateLockfile {
        /// The app's Cargo.toml.
        manifest: PathBuf,
    },
    /// `cargo install wasm-bindgen-cli --version <lock> --locked --root <cache>`.
    WasmBindgen {
        /// The version; `None` reads it from the lock when the fix runs.
        version: Option<String>,
        /// The app's Cargo.lock.
        lock: PathBuf,
    },
    /// A pinned tool from icm's tools.toml: downloaded with curl, checked
    /// against its sha256, unpacked into the tool cache
    /// ([`crate::pinned::install`]).
    Pinned {
        /// The tool's name.
        name: String,
    },
}

/// One command a fix runs.
pub struct FixStep {
    /// The step name (`doctor.sdkmanager`).
    pub name: String,
    /// The command.
    pub cmd: Cmd,
}

impl Fix {
    /// Who may run it: `doctor` for local, idempotent changes, `doctor-yes`
    /// for downloads and installs.
    pub fn by(&self) -> By {
        match self {
            Fix::DebugKeystore { .. } | Fix::AvdCreate { .. } | Fix::SimCreate { .. } => By::Doctor,
            _ => By::DoctorYes,
        }
    }

    /// The order fixes run in: each may need the ones before it.
    pub fn phase(&self) -> u8 {
        match self {
            Fix::ToolchainInstall { .. } => 0,
            Fix::TargetAdd { .. } => 1,
            Fix::Brew { .. } => 2,
            Fix::SdkManager { .. } => 3,
            Fix::DebugKeystore { .. } => 4,
            Fix::AvdCreate { .. } => 5,
            Fix::DownloadIosPlatform { .. } => 6,
            Fix::SimCreate { .. } => 7,
            Fix::GenerateLockfile { .. } => 8,
            Fix::WasmBindgen { .. } => 9,
            Fix::Pinned { .. } => 10,
        }
    }

    /// Merges a fix of the same kind into this one (one sdkmanager or
    /// rustup call for many packages or targets). Returns whether it did.
    pub fn merge(&mut self, other: &Fix) -> bool {
        match (self, other) {
            (
                Fix::SdkManager {
                    packages, sdk_root, ..
                },
                Fix::SdkManager {
                    packages: more,
                    sdk_root: other_root,
                    ..
                },
            ) if sdk_root == other_root => {
                for package in more {
                    if !packages.contains(package) {
                        packages.push(package.clone());
                    }
                }
                true
            }
            (
                Fix::TargetAdd {
                    toolchain, targets, ..
                },
                Fix::TargetAdd {
                    toolchain: other_toolchain,
                    targets: more,
                    ..
                },
            ) if toolchain == other_toolchain => {
                for target in more {
                    if !targets.contains(target) {
                        targets.push(target.clone());
                    }
                }
                true
            }
            (this, other) => *this == *other,
        }
    }

    /// Whether running this fix also does what `other` would.
    pub fn covers(&self, other: &Fix) -> bool {
        match (self, other) {
            (
                Fix::SdkManager {
                    packages, sdk_root, ..
                },
                Fix::SdkManager {
                    packages: wanted,
                    sdk_root: other_root,
                    ..
                },
            ) => sdk_root == other_root && wanted.iter().all(|p| packages.contains(p)),
            (
                Fix::TargetAdd {
                    toolchain, targets, ..
                },
                Fix::TargetAdd {
                    toolchain: other_toolchain,
                    targets: wanted,
                    ..
                },
            ) => toolchain == other_toolchain && wanted.iter().all(|t| targets.contains(t)),
            // Installing "the lock's version" covers installing that version.
            (
                Fix::WasmBindgen {
                    version: None,
                    lock,
                },
                Fix::WasmBindgen {
                    lock: other_lock, ..
                },
            ) => lock == other_lock,
            (this, other) => this == other,
        }
    }

    /// The command line, for fix hints and `--dry-run`.
    pub fn display(&self, env: &Env) -> String {
        if let Fix::WasmBindgen { version: None, .. } = self {
            return format!(
                "cargo install wasm-bindgen-cli --version <the lock's wasm-bindgen> --locked --root {}",
                crate::process::shell_quote(
                    &tools::wasm_bindgen_root("<version>").display().to_string()
                )
            );
        }
        match self.build(&HostConfig::default(), env, false, true) {
            Ok(steps) => steps
                .iter()
                .map(|step| step.cmd.display())
                .collect::<Vec<_>>()
                .join(" && "),
            Err(error) => format!("({})", error.detail),
        }
    }

    /// The commands to run, built from a fresh look at the machine.
    pub fn steps(
        &self,
        host: &HostConfig,
        env: &Env,
        offline: bool,
    ) -> Result<Vec<FixStep>, IcmError> {
        self.build(host, env, offline, false)
    }

    fn build(
        &self,
        host: &HostConfig,
        env: &Env,
        offline: bool,
        display: bool,
    ) -> Result<Vec<FixStep>, IcmError> {
        // A hint is shown even before the JDK it needs exists.
        let java = || -> Result<Vec<(String, String)>, IcmError> {
            match java_env(host, env) {
                Err(_) if display => Ok(Vec::new()),
                other => other,
            }
        };
        let step = |name: &str, cmd: Cmd| FixStep {
            name: name.to_string(),
            cmd,
        };
        Ok(match self {
            Fix::ToolchainInstall { dir, name } => {
                let mut cmd = Cmd::tool("rustup")
                    .args(["toolchain", "install"])
                    .cwd(dir)
                    .timeout(Duration::from_secs(45 * 60));
                if let Some(name) = name {
                    cmd = cmd.arg(name);
                }
                vec![step("doctor.rustup.toolchain", cmd)]
            }
            Fix::TargetAdd {
                dir,
                toolchain,
                targets,
            } => vec![step(
                "doctor.rustup.targets",
                Cmd::tool("rustup")
                    .args(["target", "add", "--toolchain", toolchain])
                    .args(targets)
                    .cwd(dir)
                    .timeout(Duration::from_secs(45 * 60)),
            )],
            Fix::Brew { formula } => vec![step(
                "doctor.brew",
                Cmd::tool("brew")
                    .args(["install", formula])
                    .env("HOMEBREW_NO_AUTO_UPDATE", "1")
                    .env("NONINTERACTIVE", "1")
                    .timeout(Duration::from_secs(45 * 60)),
            )],
            Fix::SdkManager {
                sdkmanager,
                sdk_root,
                packages,
            } => vec![step(
                "doctor.sdkmanager",
                Cmd::new(sdkmanager)
                    .arg(format!("--sdk_root={}", sdk_root.display()))
                    .arg("--install")
                    .args(packages)
                    .envs(java()?)
                    .timeout(Duration::from_secs(3 * 60 * 60)),
            )],
            Fix::DebugKeystore { path } => {
                let keytool = match (env.tool_override("keytool"), tools::jdk(host, env, true)) {
                    (Some(path), _) => path,
                    (None, Ok(jdk)) => jdk.home.join("bin").join("keytool"),
                    (None, Err(_)) if display => PathBuf::from("keytool"),
                    (None, Err(error)) => return Err(error),
                };
                let dir = path.parent().unwrap_or(Path::new("."));
                vec![
                    step(
                        "doctor.keystore_dir",
                        Cmd::new("/bin/mkdir")
                            .arg("-p")
                            .arg(dir)
                            .timeout(Duration::from_secs(30)),
                    ),
                    step(
                        "doctor.keytool",
                        Cmd::new(keytool)
                            .args(["-genkeypair", "-noprompt", "-keystore"])
                            .arg(path)
                            .args([
                                "-storetype",
                                "PKCS12",
                                "-storepass",
                                "android",
                                "-alias",
                                "androiddebugkey",
                                "-keypass",
                                "android",
                                "-keyalg",
                                "RSA",
                                "-keysize",
                                "2048",
                                "-validity",
                                "10000",
                                "-dname",
                                "CN=Android Debug,O=Android,C=US",
                            ])
                            .envs(java()?)
                            .timeout(Duration::from_secs(120)),
                    ),
                ]
            }
            Fix::AvdCreate {
                avdmanager,
                name,
                image,
                device,
            } => vec![step(
                "doctor.avdmanager",
                Cmd::new(avdmanager)
                    .args(["create", "avd", "-n", name, "-k", image, "-d", device])
                    .envs(java()?)
                    .timeout(Duration::from_secs(5 * 60)),
            )],
            Fix::DownloadIosPlatform { developer_dir } => vec![step(
                "doctor.xcodebuild.download_platform",
                Cmd::tool("xcodebuild")
                    .args(["-downloadPlatform", "iOS"])
                    .env("DEVELOPER_DIR", developer_dir)
                    .timeout(Duration::from_secs(4 * 60 * 60)),
            )],
            Fix::SimCreate {
                developer_dir,
                name,
                device_type,
                runtime,
            } => vec![step(
                "doctor.simctl.create",
                Cmd::tool("xcrun")
                    .args(["simctl", "create", name, device_type, runtime])
                    .env("DEVELOPER_DIR", developer_dir)
                    .timeout(Duration::from_secs(120)),
            )],
            Fix::GenerateLockfile { manifest } => {
                let mut cmd = Cmd::tool("cargo")
                    .arg("generate-lockfile")
                    .arg("--manifest-path")
                    .arg(manifest)
                    .cwd(manifest.parent().unwrap_or(Path::new(".")))
                    .timeout(Duration::from_secs(20 * 60));
                if offline {
                    cmd = cmd.arg("--offline");
                }
                vec![step("doctor.cargo.lockfile", cmd)]
            }
            Fix::WasmBindgen { version, lock } => {
                let version = match version {
                    Some(version) => version.clone(),
                    None => crate::cargo::Lock::read(lock)?
                        .and_then(|lock| lock.version_of("wasm-bindgen").map(str::to_string))
                        .ok_or_else(|| {
                            IcmError::new(
                                CheckId::DepsWasmBindgenCli,
                                format!(
                                    "{} has no wasm-bindgen to match",
                                    crate::paths::display(lock)
                                ),
                            )
                        })?,
                };
                let root = tools::wasm_bindgen_root(&version);
                let tools_dir = crate::paths::tools_dir();
                let mut cmd = Cmd::tool("cargo")
                    .args(["install", "wasm-bindgen-cli", "--version"])
                    .arg(version)
                    .args(["--locked", "--root"])
                    .arg(&root)
                    .cwd(tools_dir)
                    .timeout(Duration::from_secs(45 * 60));
                if offline {
                    cmd = cmd.arg("--offline");
                }
                vec![step("doctor.cargo.install.wasm_bindgen", cmd)]
            }
            // What the fix runs first; `crate::pinned::install` then checks
            // the sha256 and unpacks (commands/doctor.rs runs it).
            Fix::Pinned { name } => {
                let tool = crate::pinned::tool(name)?;
                let Some(download) = tool.download() else {
                    return Err(crate::pinned::missing(&tool));
                };
                vec![step(
                    &format!("pinned.{name}.download"),
                    crate::pinned::download_cmd(download, &tool.dir().join("<download>")),
                )]
            }
        })
    }

    /// Prepares the file system before the fix runs (directories the tool
    /// expects to exist).
    pub fn prepare(&self) -> std::io::Result<()> {
        match self {
            Fix::DebugKeystore { path } => match path.parent() {
                Some(parent) => std::fs::create_dir_all(parent),
                None => Ok(()),
            },
            Fix::WasmBindgen { .. } => std::fs::create_dir_all(crate::paths::tools_dir()),
            _ => Ok(()),
        }
    }
}

/// `JAVA_HOME` and `PATH` for Android's Java tools (Appendix C item 2).
fn java_env(host: &HostConfig, env: &Env) -> Result<Vec<(String, String)>, IcmError> {
    let jdk = tools::jdk(host, env, true)?;
    let sdk = tools::android_sdk(host, env).ok();
    let base = env.var("PATH").unwrap_or("/usr/bin:/bin").to_string();
    Ok(tools::android_env(Some(&jdk), sdk.as_ref(), None, &base))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixes_merge_by_kind() {
        let mut sdk = Fix::SdkManager {
            sdkmanager: "/s/sdkmanager".into(),
            sdk_root: "/s".into(),
            packages: vec!["platform-tools".into()],
        };
        assert!(sdk.merge(&Fix::SdkManager {
            sdkmanager: "/s/sdkmanager".into(),
            sdk_root: "/s".into(),
            packages: vec!["emulator".into(), "platform-tools".into()],
        }));
        match &sdk {
            Fix::SdkManager { packages, .. } => {
                assert_eq!(packages, &["platform-tools", "emulator"])
            }
            other => panic!("{other:?}"),
        }
        assert!(!sdk.merge(&Fix::Brew {
            formula: "openjdk@21".into()
        }));

        let mut targets = Fix::TargetAdd {
            dir: "/a".into(),
            toolchain: "1.98.0".into(),
            targets: vec!["wasm32-unknown-unknown".into()],
        };
        assert!(targets.merge(&Fix::TargetAdd {
            dir: "/a".into(),
            toolchain: "1.98.0".into(),
            targets: vec!["aarch64-linux-android".into()],
        }));
        assert_eq!(
            targets.display(&Env::default()),
            "rustup target add --toolchain 1.98.0 wasm32-unknown-unknown aarch64-linux-android"
        );
    }

    #[test]
    fn local_fixes_need_no_consent() {
        assert_eq!(
            Fix::SimCreate {
                developer_dir: "/x".into(),
                name: "icm-iphone-17-ios-27.0".into(),
                device_type: "t".into(),
                runtime: "r".into()
            }
            .by(),
            By::Doctor
        );
        assert_eq!(
            Fix::WasmBindgen {
                version: Some("0.2.105".into()),
                lock: "/a/Cargo.lock".into()
            }
            .by(),
            By::DoctorYes
        );
        let wasm = Fix::WasmBindgen {
            version: Some("0.2.105".into()),
            lock: "/a/Cargo.lock".into(),
        }
        .display(&Env::default());
        assert!(
            wasm.starts_with("cargo install wasm-bindgen-cli --version 0.2.105 --locked --root "),
            "{wasm}"
        );
    }
}
