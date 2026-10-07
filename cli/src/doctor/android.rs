//! What Android needs: an SDK with its licences accepted (the owner's), a
//! JDK 17+ for the SDK's Java tools, the SDK packages (platform-tools,
//! emulator, build-tools, the target platform, an NDK r28+ and the managed
//! emulator's system image), the debug keystore and icm's managed AVD.

use super::{Fix, Probe, Requirement, find_tool};
use crate::catalogue::{By, CheckId};
use crate::cli::Platform;
use crate::error::{Check, Evidence, Status};
use crate::managed;
use crate::tools::{self, AndroidSdk};
use std::path::{Path, PathBuf};

const PLATFORM: Option<Platform> = Some(Platform::Android);

pub(super) fn gather(probe: &Probe<'_>) -> Vec<Requirement> {
    let mut out = Vec::new();
    let target_sdk = probe
        .project
        .map(|p| p.config.config.android.target_sdk)
        .unwrap_or(36);

    let sdk = match tools::android_sdk(probe.host, probe.env) {
        Ok(sdk) => {
            out.push(Requirement::new(
                "android.sdk",
                PLATFORM,
                Check::pass(
                    CheckId::EnvAndroidSdkMissing,
                    format!("Android SDK at {} ({})", sdk.root.display(), sdk.source),
                ),
            ));
            sdk
        }
        Err(error) => {
            out.push(Requirement::new(
                "android.sdk",
                PLATFORM,
                Check::from_error(error, Status::Fail),
            ));
            return out;
        }
    };

    // The JDK every SDK Java tool needs (Appendix C item 2).
    let jdk_ok = match tools::jdk(probe.host, probe.env, true) {
        Ok(jdk) => {
            out.push(Requirement::new(
                "android.jdk",
                PLATFORM,
                Check::pass(
                    CheckId::EnvJdkMissing,
                    format!(
                        "Java {} at {} ({}); passed to Android tools as JAVA_HOME",
                        jdk.version,
                        jdk.home.display(),
                        jdk.source
                    ),
                ),
            ));
            true
        }
        Err(error) => {
            let check = Check::from_error(error, Status::Fail);
            let requirement = if find_tool(probe.env, "brew").is_some() {
                Requirement::new("android.jdk", PLATFORM, check).with_fixes(
                    vec![Fix::Brew {
                        formula: "openjdk@21".to_string(),
                    }],
                    probe.env,
                )
            } else {
                let mut check = check;
                check.error = check
                    .error
                    .fix(
                        "Install a JDK 17 or newer and set JAVA_HOME (or `java_home` in ~/.config/icm/host.toml).",
                        &[],
                    )
                    .by(By::Owner);
                Requirement::new("android.jdk", PLATFORM, check)
            };
            out.push(requirement);
            false
        }
    };

    // sdkmanager and avdmanager come with the command-line tools, which
    // only the owner can install (they bootstrap sdkmanager itself).
    let sdkmanager = sdk.sdkmanager(probe.env);
    let avdmanager = sdk.avdmanager(probe.env);
    match (&sdkmanager, &avdmanager) {
        (Some(sdkmanager), Some(_)) => out.push(Requirement::new(
            "android.cmdline_tools",
            PLATFORM,
            Check::pass(
                CheckId::EnvAndroidPackageMissing,
                format!(
                    "cmdline-tools: {}",
                    sdkmanager.parent().unwrap_or(sdkmanager).display()
                ),
            ),
        )),
        _ => {
            let mut check = Check::fail(
                CheckId::EnvAndroidPackageMissing,
                format!(
                    "the SDK at {} has no command-line tools (sdkmanager, avdmanager)",
                    sdk.root.display()
                ),
            );
            check.error = check
                .error
                .fix(
                    "The owner installs the Android command-line tools into the SDK.",
                    &["brew install --cask android-commandlinetools"],
                )
                .by(By::Owner);
            out.push(Requirement::new("android.cmdline_tools", PLATFORM, check));
        }
    }

    // Licences are a person's decision: never accepted by icm.
    let licence = sdk.root.join("licenses").join("android-sdk-license");
    let licensed = licence.is_file();
    if licensed {
        out.push(Requirement::new(
            "android.licenses",
            PLATFORM,
            Check::pass(
                CheckId::EnvLicensesNotAccepted,
                "the Android SDK licence is accepted",
            ),
        ));
    } else {
        let command = format!(
            "{} --sdk_root={} --licenses",
            sdkmanager
                .as_deref()
                .map_or_else(|| "sdkmanager".to_string(), |p| p.display().to_string()),
            sdk.root.display()
        );
        out.push(Requirement::new(
            "android.licenses",
            PLATFORM,
            Check::fail(
                CheckId::EnvLicensesNotAccepted,
                format!(
                    "the Android SDK licences are not accepted ({} is missing)",
                    crate::paths::display(&licence)
                ),
            )
            .fix(
                "The owner reads and accepts the SDK licences (JAVA_HOME must name a JDK 17+).",
                &[&command],
            )
            .evidence(Evidence::file(&licence)),
        ));
    }

    // sdkmanager can install packages only with its licences accepted and
    // itself present.
    let installer = sdkmanager.filter(|_| licensed);
    let abi = managed::host_abi();
    // The AVD is created from the image that is installed, the way `icm
    // run android` creates it (google_apis first), else from the one the
    // fix installs.
    let installed = crate::android::avd::find_image(&sdk, target_sdk, abi);
    let image = installed.as_ref().map_or_else(
        || managed::system_image_package(target_sdk, abi),
        |image| image.package.clone(),
    );
    let image_installed = installed.is_some();

    for (key, present, package, what) in [
        (
            "android.platform_tools",
            sdk.root
                .join("platform-tools")
                .join("adb")
                .exists()
                .then(|| sdk.root.join("platform-tools")),
            "platform-tools".to_string(),
            "adb",
        ),
        (
            "android.emulator",
            sdk.root
                .join("emulator")
                .join("emulator")
                .exists()
                .then(|| sdk.root.join("emulator")),
            "emulator".to_string(),
            "the emulator",
        ),
        (
            "android.build_tools",
            sdk.build_tools(35).map(|(_, dir)| dir),
            managed::BUILD_TOOLS_PACKAGE.to_string(),
            "build-tools 35 or newer (aapt2, zipalign, apksigner)",
        ),
        (
            "android.platform",
            platform_dir(&sdk, target_sdk),
            format!("platforms;android-{target_sdk}"),
            "the platform of [android] target_sdk (android.jar)",
        ),
        (
            "android.system_image",
            system_image_dir(&sdk, target_sdk, abi.as_str()),
            image.clone(),
            "the managed emulator's system image",
        ),
    ] {
        out.push(package_requirement(
            probe,
            &sdk,
            installer.as_deref(),
            key,
            present,
            &package,
            what,
        ));
    }

    match tools::ndk(Some(&sdk), probe.host, probe.env) {
        Ok(ndk) => out.push(Requirement::new(
            "android.ndk",
            PLATFORM,
            Check::pass(
                CheckId::EnvNdkTooOld,
                format!(
                    "NDK {} at {} ({})",
                    ndk.version,
                    ndk.root.display(),
                    ndk.source
                ),
            ),
        )),
        Err(error) => {
            let fixes = installer
                .as_ref()
                .map(|sdkmanager| {
                    vec![Fix::SdkManager {
                        sdkmanager: sdkmanager.clone(),
                        sdk_root: sdk.root.clone(),
                        packages: vec![managed::NDK_PACKAGE.to_string()],
                    }]
                })
                .unwrap_or_default();
            out.push(
                Requirement::new(
                    "android.ndk",
                    PLATFORM,
                    Check::from_error(error, Status::Fail),
                )
                .with_fixes(fixes, probe.env),
            );
        }
    }

    // The debug keystore every icm debug APK is signed with, so
    // `adb install -r` keeps working across runs.
    {
        let keystore = managed::debug_keystore();
        if keystore.is_file() {
            out.push(Requirement::new(
                "android.debug_keystore",
                PLATFORM,
                Check::pass(
                    CheckId::EnvDebugKeystoreMissing,
                    format!("debug keystore: {}", keystore.display()),
                ),
            ));
        } else {
            out.push(
                Requirement::new(
                    "android.debug_keystore",
                    PLATFORM,
                    Check::fail(
                        CheckId::EnvDebugKeystoreMissing,
                        format!("{} does not exist", keystore.display()),
                    ),
                )
                .with_fixes(vec![Fix::DebugKeystore { path: keystore }], probe.env),
            );
        }
    }

    out.push(avd(
        probe,
        avdmanager.as_deref(),
        target_sdk,
        &image,
        image_installed,
        jdk_ok,
    ));
    out
}

#[allow(clippy::too_many_arguments)]
fn package_requirement(
    probe: &Probe<'_>,
    sdk: &AndroidSdk,
    installer: Option<&Path>,
    key: &str,
    present: Option<PathBuf>,
    package: &str,
    what: &str,
) -> Requirement {
    if let Some(dir) = present {
        return Requirement::new(
            key,
            PLATFORM,
            Check::pass(
                CheckId::EnvAndroidPackageMissing,
                format!("{package}: {}", dir.display()),
            ),
        );
    }
    let check = Check::fail(
        CheckId::EnvAndroidPackageMissing,
        format!(
            "{what} is missing from the SDK at {} (sdkmanager package `{package}`)",
            sdk.root.display()
        ),
    );
    match installer {
        Some(sdkmanager) => Requirement::new(key, PLATFORM, check).with_fixes(
            vec![Fix::SdkManager {
                sdkmanager: sdkmanager.to_path_buf(),
                sdk_root: sdk.root.clone(),
                packages: vec![package.to_string()],
            }],
            probe.env,
        ),
        None => {
            let mut check = check;
            check.error.detail.push_str(
                "; sdkmanager cannot install it until the licences are accepted and the command-line tools exist",
            );
            check.error.fix.by = By::Owner;
            Requirement::new(key, PLATFORM, check)
        }
    }
}

fn platform_dir(sdk: &AndroidSdk, api: u32) -> Option<PathBuf> {
    [format!("android-{api}"), format!("android-{api}.0")]
        .into_iter()
        .map(|name| sdk.root.join("platforms").join(name))
        .find(|dir| dir.join("android.jar").is_file())
}

/// The installed system image for an API level and ABI, any tag.
fn system_image_dir(sdk: &AndroidSdk, api: u32, abi: &str) -> Option<PathBuf> {
    for level in [format!("android-{api}"), format!("android-{api}.0")] {
        for tag in [
            managed::SYSTEM_IMAGE_TAG,
            "google_apis_playstore",
            "default",
            "google_atd",
            "aosp_atd",
        ] {
            let dir = sdk
                .root
                .join("system-images")
                .join(&level)
                .join(tag)
                .join(abi);
            if dir.is_dir() {
                return Some(dir);
            }
        }
    }
    None
}

fn avd(
    probe: &Probe<'_>,
    avdmanager: Option<&Path>,
    target_sdk: u32,
    image: &str,
    image_installed: bool,
    jdk_ok: bool,
) -> Requirement {
    let key = "android.avd";

    if let Some(device) = probe
        .host
        .android
        .device
        .as_deref()
        .filter(|d| !d.trim().is_empty())
    {
        return Requirement::new(
            key,
            PLATFORM,
            Check::skip(
                CheckId::EnvAvdMissing,
                format!("host.toml pins device {device}; no emulator is needed"),
            ),
        );
    }

    if let Some(name) = probe
        .host
        .android
        .avd
        .as_deref()
        .filter(|a| !a.trim().is_empty())
    {
        let check = if managed::avd_exists(probe.env, name) {
            Check::pass(
                CheckId::EnvAvdMissing,
                format!("host.toml names AVD {name}, which exists"),
            )
        } else {
            let mut check = Check::fail(
                CheckId::EnvAvdMissing,
                format!("host.toml [android] avd names {name}, which does not exist"),
            );
            check.error = check
                .error
                .fix(
                    "Set [android] avd in host.toml to an existing AVD (`icm devices android` lists them), or remove it to use icm's managed one.",
                    &["icm devices android --json -q"],
                )
                .by(By::Agent);
            check
        };
        return Requirement::new(key, PLATFORM, check);
    }

    let name = managed::avd_name(target_sdk);
    if managed::avd_exists(probe.env, &name) {
        let home = managed::avd_home(probe.env).unwrap_or_default();
        return Requirement::new(
            key,
            PLATFORM,
            Check::pass(
                CheckId::EnvAvdMissing,
                format!("the managed AVD {name} exists in {}", home.display()),
            ),
        );
    }

    let mut check = Check::fail(
        CheckId::EnvAvdMissing,
        format!("the managed AVD {name} does not exist yet"),
    );
    match avdmanager {
        Some(avdmanager) if image_installed && jdk_ok => Requirement::new(key, PLATFORM, check)
            .with_fixes(
                vec![Fix::AvdCreate {
                    avdmanager: avdmanager.to_path_buf(),
                    name,
                    image: image.to_string(),
                    device: managed::EMULATOR_DEVICE.to_string(),
                }],
                probe.env,
            ),
        _ => {
            check.error.detail.push_str(&format!(
                "; it needs the JDK, avdmanager and the system image {image} first"
            ));
            check.error.fix.commands = vec!["icm doctor android --fix --yes".to_string()];
            Requirement::new(key, PLATFORM, check)
        }
    }
}
