//! Device `.app` bundles (design §9.1-§9.2, §10.5 step 4, §11.1 steps
//! 5-6), for `icm run ios-device` and `icm release ios`: actool for
//! `iphoneos` (the icon flattened onto `[app] background` into an opaque
//! 1024 px RGB PNG), the device Info.plist with the `DT*` keys from the
//! host Xcode, PrivacyInfo.xcprivacy, the executable, `[app] resources`,
//! `platform/ios/resources/`, and extra root files (THIRD_PARTY_NOTICES,
//! `embedded.mobileprovision`). Signing is the caller's last step.

use super::dt::DtKeys;
use crate::catalogue::CheckId;
use crate::config::IcmToml;
use crate::context::{Ctx, Project};
use crate::error::{Check, Evidence, IcmError, Result};
use crate::platform::ios_sim::bundle::{
    bundle_name, compile_assets_for, copy_tree, resource_files,
};
use crate::platform::ios_sim::plist::{self, APP_ICON, InfoInputs};
use crate::process::Cmd;
use crate::tools::Xcode;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The keys every device Info.plist must hold (`ios.plist.required_keys`),
/// besides the `DT*` keys (`ios.plist.dt_keys`).
pub const REQUIRED_KEYS: &[&str] = &[
    "CFBundleDisplayName",
    "CFBundleExecutable",
    "CFBundleIdentifier",
    "CFBundleInfoDictionaryVersion",
    "CFBundleName",
    "CFBundlePackageType",
    "CFBundleShortVersionString",
    "CFBundleSupportedPlatforms",
    "CFBundleVersion",
    "LSRequiresIPhoneOS",
    "MinimumOSVersion",
    "UIApplicationSceneManifest",
    "UIDeviceFamily",
    "UILaunchScreen",
    "UIRequiredDeviceCapabilities",
    "UISupportedInterfaceOrientations",
];

/// What a device Info.plist is made of besides icm.toml.
pub struct InfoParts<'a> {
    /// `CFBundleExecutable`.
    pub executable: &'a str,
    /// The Cargo version.
    pub cargo_version: &'a str,
    /// actool's partial plist.
    pub actool: Option<&'a Map<String, Value>>,
    /// The host Xcode's `DT*` keys.
    pub dt: &'a DtKeys,
    /// An App Store build: the export-compliance keys.
    pub release: bool,
}

/// The device Info.plist (design §9.1): the simulator's managed keys with
/// `CFBundleSupportedPlatforms = [iPhoneOS]`, the `DT*` keys and, for the
/// App Store, `ITSAppUsesNonExemptEncryption` (when the owner answered)
/// and `ITSEncryptionExportComplianceCode`.
pub fn info_plist(config: &IcmToml, parts: &InfoParts<'_>) -> Map<String, Value> {
    let mut info = plist::info_plist(
        config,
        &InfoInputs {
            executable: parts.executable,
            cargo_version: parts.cargo_version,
            platform: "iPhoneOS",
            actool: parts.actool,
        },
    );
    for (key, value) in parts.dt.to_map() {
        let _ = info.insert(key, value);
    }
    if parts.release {
        if let Some(answer) = config.ios.uses_non_exempt_encryption {
            let _ = info.insert("ITSAppUsesNonExemptEncryption".into(), json!(answer));
        }
        if let Some(code) = &config.ios.export_compliance_code {
            let _ = info.insert("ITSEncryptionExportComplianceCode".into(), json!(code));
        }
    }
    info
}

/// The required keys a device plist lacks.
pub fn missing_required(info: &Map<String, Value>) -> Vec<&'static str> {
    REQUIRED_KEYS
        .iter()
        .copied()
        .filter(|key| !info.contains_key(*key))
        .collect()
}

/// A finished (unsigned) bundle.
#[derive(Clone, Debug)]
pub struct DeviceBundle {
    /// `<Name>.app`.
    pub app: PathBuf,
    /// The executable inside it.
    pub executable: PathBuf,
    /// The Info.plist's contents.
    pub info: Map<String, Value>,
    /// The generated Info.plist (outside the bundle).
    pub info_path: PathBuf,
    /// The generated PrivacyInfo.xcprivacy.
    pub privacy_path: PathBuf,
}

/// Where and from what a bundle is assembled.
pub struct Inputs<'a> {
    /// Generated inputs (`Assets.xcassets`, actool output, plists).
    pub gen_dir: &'a Path,
    /// The directory the `<Name>.app` goes into.
    pub out_dir: &'a Path,
    /// The executable to copy in (already stripped for the App Store).
    pub exe: &'a Path,
    /// Its name in the bundle.
    pub bin: &'a str,
    /// The Cargo version.
    pub cargo_version: &'a str,
    /// The host Xcode's `DT*` keys.
    pub dt: &'a DtKeys,
    /// An App Store build.
    pub release: bool,
    /// Files for the bundle root: (source, name).
    pub extra: Vec<(PathBuf, String)>,
}

fn io(what: &str, path: &Path, error: impl std::fmt::Display) -> IcmError {
    IcmError::new(
        CheckId::InternalBug,
        format!("cannot {what} {}: {error}", crate::paths::display(path)),
    )
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io("create", parent, e))?;
    }
    std::fs::write(path, bytes).map_err(|e| io("write", path, e))
}

/// Assembles the bundle (not signed).
pub fn assemble(
    ctx: &Ctx,
    project: &Project,
    xcode: &Xcode,
    inputs: &Inputs<'_>,
) -> Result<DeviceBundle> {
    let config = &project.config.config;
    std::fs::create_dir_all(inputs.gen_dir).map_err(|e| io("create", inputs.gen_dir, e))?;
    let (actool_out, actool_plist) =
        compile_assets_for(ctx, project, xcode, inputs.gen_dir, "iphoneos")?;

    let info = info_plist(
        config,
        &InfoParts {
            executable: inputs.bin,
            cargo_version: inputs.cargo_version,
            actool: actool_plist.as_ref(),
            dt: inputs.dt,
            release: inputs.release,
        },
    );
    let info_path = inputs.gen_dir.join("Info.plist");
    write(
        &info_path,
        plist::to_xml(&Value::Object(info.clone())).as_bytes(),
    )?;
    let privacy_path = inputs.gen_dir.join("PrivacyInfo.xcprivacy");
    write(
        &privacy_path,
        plist::to_xml(&Value::Object(plist::privacy_info(config))).as_bytes(),
    )?;

    let app = inputs.out_dir.join(bundle_name(&config.app.name));
    let _ = std::fs::remove_dir_all(&app);
    std::fs::create_dir_all(&app).map_err(|e| io("create", &app, e))?;
    let executable = app.join(inputs.bin);
    let _ = std::fs::copy(inputs.exe, &executable).map_err(|e| io("copy", inputs.exe, e))?;
    copy_tree(&actool_out, &app).map_err(|e| io("copy", &actool_out, e))?;
    let _ =
        std::fs::copy(&info_path, app.join("Info.plist")).map_err(|e| io("copy", &info_path, e))?;
    let _ = std::fs::copy(&privacy_path, app.join("PrivacyInfo.xcprivacy"))
        .map_err(|e| io("copy", &privacy_path, e))?;
    for relative in resource_files(project.dir(), &config.app.resources) {
        copy_tree(&project.dir().join(&relative), &app.join(&relative))
            .map_err(|e| io("copy", &relative, e))?;
    }
    let overrides = project.dir().join("platform").join("ios").join("resources");
    if overrides.is_dir() {
        copy_tree(&overrides, &app).map_err(|e| io("copy", &overrides, e))?;
    }
    for (source, name) in &inputs.extra {
        let _ = std::fs::copy(source, app.join(name)).map_err(|e| io("copy", source, e))?;
    }
    Ok(DeviceBundle {
        app,
        executable,
        info,
        info_path,
        privacy_path,
    })
}

/// `plutil -lint` on the bundle's plists (`ios.plist.lint`).
pub fn lint(ctx: &Ctx, app: &Path) -> Result<Check> {
    let info = app.join("Info.plist");
    let outcome = ctx.step(
        "ios.plist.lint",
        &Cmd::tool("plutil")
            .arg("-lint")
            .arg(&info)
            .arg(app.join("PrivacyInfo.xcprivacy"))
            .timeout(Duration::from_secs(30)),
    )?;
    Ok(if outcome.success() {
        Check::pass(
            CheckId::IosPlistLint,
            "Info.plist and PrivacyInfo.xcprivacy are valid",
        )
    } else {
        let text = format!("{}{}", outcome.stdout_text(), outcome.stderr_text());
        Check::fail(
            CheckId::IosPlistLint,
            format!("plutil -lint rejected the bundle's plists: {}", text.trim()),
        )
        .evidence(Evidence::file(&info))
    })
}

/// The plist gates that need no tool: `ios.plist.required_keys`,
/// `.scene_manifest`, `.ipad_orientations`, `ios.privacy.present`.
pub fn plist_checks(app: &Path, info: &Map<String, Value>) -> Vec<Check> {
    let info_path = app.join("Info.plist");
    let mut checks = Vec::new();
    let missing = missing_required(info);
    checks.push(if missing.is_empty() {
        Check::pass(
            CheckId::IosPlistRequiredKeys,
            "the managed keys are present",
        )
    } else {
        Check::fail(
            CheckId::IosPlistRequiredKeys,
            format!("Info.plist lacks {}", missing.join(", ")),
        )
        .evidence(Evidence::file(&info_path))
    });
    checks.push(if plist::has_scene_manifest(info) {
        Check::pass(
            CheckId::IosPlistSceneManifest,
            "UIApplicationSceneManifest present",
        )
    } else {
        Check::fail(
            CheckId::IosPlistSceneManifest,
            "Info.plist has no UISceneConfigurations; iOS 27 kills such an app at launch",
        )
        .evidence(Evidence::file(&info_path))
    });
    checks.push(ipad_orientations(info, &info_path));
    checks.push(if app.join("PrivacyInfo.xcprivacy").is_file() {
        Check::pass(
            CheckId::IosPrivacyPresent,
            "PrivacyInfo.xcprivacy at the bundle root",
        )
    } else {
        Check::fail(
            CheckId::IosPrivacyPresent,
            "PrivacyInfo.xcprivacy is missing from the bundle root",
        )
        .evidence(Evidence::file(app))
    });
    checks
}

/// `ios.plist.ipad_orientations`: an app that runs on iPad (family 2)
/// must support all four orientations (ITMS-90474), unless it requires
/// full screen.
pub fn ipad_orientations(info: &Map<String, Value>, info_path: &Path) -> Check {
    let families: Vec<i64> = info
        .get("UIDeviceFamily")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    if !families.contains(&2) {
        return Check::pass(
            CheckId::IosPlistIpadOrientations,
            format!("UIDeviceFamily {families:?}: iPhone only"),
        );
    }
    let full_screen = info
        .get("UIRequiresFullScreen")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let orientations = info
        .get("UISupportedInterfaceOrientations~ipad")
        .or_else(|| info.get("UISupportedInterfaceOrientations"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if full_screen || orientations >= 4 {
        Check::pass(
            CheckId::IosPlistIpadOrientations,
            "an iPad app with all four orientations (or full screen)",
        )
    } else {
        Check::fail(
            CheckId::IosPlistIpadOrientations,
            format!("the app runs on iPad but supports {orientations} orientation(s); iPad multitasking needs all four"),
        )
        .evidence(Evidence::file(info_path))
    }
}

/// The sensitive classes whose use needs a usage description, and the key.
pub const SENSITIVE_CLASSES: &[(&str, &str)] = &[
    ("AVCaptureDevice", "NSCameraUsageDescription"),
    ("AVAudioRecorder", "NSMicrophoneUsageDescription"),
    ("LAContext", "NSFaceIDUsageDescription"),
    ("PHPhotoLibrary", "NSPhotoLibraryUsageDescription"),
    ("CLLocationManager", "NSLocationWhenInUseUsageDescription"),
];

/// `ios.plist.usage_descriptions`: every sensitive class the executable
/// names has its NS*UsageDescription (ITMS-90683).
pub fn usage_descriptions(info: &Map<String, Value>, exe_bytes: &[u8], info_path: &Path) -> Check {
    let missing: Vec<String> = SENSITIVE_CLASSES
        .iter()
        .filter(|(class, key)| {
            super::macho::contains(exe_bytes, class.as_bytes())
                && info
                    .get(*key)
                    .and_then(Value::as_str)
                    .is_none_or(|text| text.trim().is_empty())
        })
        .map(|(class, key)| format!("{key} (the executable uses {class})"))
        .collect();
    if missing.is_empty() {
        Check::pass(
            CheckId::IosPlistUsageDescriptions,
            "every sensitive API the executable names has its usage description",
        )
    } else {
        Check::fail(
            CheckId::IosPlistUsageDescriptions,
            format!("Info.plist lacks {}", missing.join(", ")),
        )
        .evidence(Evidence::file(info_path))
        .fix(
            "Set the matching [app.permissions] reason (camera, microphone, face_id, photos, location) in icm.toml.",
            &[],
        )
    }
}

/// `ios.icon.opaque_1024` from `xcrun assetutil --info <Assets.car>`: the
/// App Store icon is 1024 px and opaque (ITMS-90713, 90717).
pub fn icon_check(
    ctx: &Ctx,
    xcode: &Xcode,
    app: &Path,
    info: &Map<String, Value>,
) -> Result<Check> {
    let car = app.join("Assets.car");
    if info.get("CFBundleIconName").and_then(Value::as_str) != Some(APP_ICON) || !car.is_file() {
        return Ok(Check::fail(
            CheckId::IosIconOpaque1024,
            "the bundle has no compiled AppIcon (CFBundleIconName and Assets.car); set [app] icon",
        )
        .evidence(Evidence::file(app)));
    }
    let outcome = ctx.probe(
        &xcode
            .xcrun()
            .args(["assetutil", "--info"])
            .arg(&car)
            .timeout(Duration::from_secs(60)),
    )?;
    if !outcome.success() {
        return Err(IcmError::new(
            CheckId::ToolFailed,
            format!("assetutil --info failed: {}", outcome.stderr_tail(3)),
        )
        .evidence(Evidence::file(&car)));
    }
    Ok(judge_icon(&outcome.stdout_text(), &car))
}

/// Judges assetutil's JSON.
pub fn judge_icon(text: &str, car: &Path) -> Check {
    let start = text.find('[').unwrap_or(0);
    let entries: Vec<Value> = serde_json::from_str(&text[start..]).unwrap_or_default();
    let icons: Vec<&Value> = entries
        .iter()
        .filter(|e| {
            e.get("AssetType").and_then(Value::as_str) == Some("Icon Image")
                && e.get("Name").and_then(Value::as_str) == Some(APP_ICON)
        })
        .collect();
    let large = icons.iter().find(|e| {
        e.get("PixelHeight").and_then(Value::as_u64) == Some(1024)
            && e.get("PixelWidth").and_then(Value::as_u64) == Some(1024)
    });
    match large {
        Some(icon) if icon.get("Opaque").and_then(Value::as_bool) == Some(true) => Check::pass(
            CheckId::IosIconOpaque1024,
            "AppIcon 1024x1024 is opaque in Assets.car",
        ),
        Some(_) => Check::fail(
            CheckId::IosIconOpaque1024,
            "AppIcon 1024x1024 in Assets.car has transparency",
        )
        .evidence(Evidence::file(car)),
        None => Check::fail(
            CheckId::IosIconOpaque1024,
            format!(
                "Assets.car has no 1024x1024 AppIcon ({} AppIcon image(s))",
                icons.len()
            ),
        )
        .evidence(Evidence::file(car)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template() -> IcmToml {
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/app/icm.toml"),
        )
        .unwrap();
        crate::config::parse(Path::new("/x/icm.toml"), &text)
            .unwrap()
            .config
    }

    fn dt() -> DtKeys {
        DtKeys {
            sdk_build: "24A430".into(),
            platform_build: "24A430".into(),
            platform_version: "27.0".into(),
            sdk_name: "iphoneos27.0".into(),
            xcode: "2700".into(),
            xcode_build: "27A266a".into(),
            compiler: "com.apple.compilers.llvm.clang.1_0".into(),
            machine_os_build: "26A434".into(),
        }
    }

    #[test]
    fn the_device_plist_has_the_store_keys() {
        let mut config = template();
        config.ios.uses_non_exempt_encryption = Some(true);
        config.ios.export_compliance_code = Some("ABC123".into());
        let actool: Map<String, Value> = serde_json::from_value(json!({
            "CFBundleIcons": {"CFBundlePrimaryIcon": {"CFBundleIconFiles": ["AppIcon60x60"], "CFBundleIconName": "AppIcon"}}
        }))
        .unwrap();
        let keys = dt();
        let info = info_plist(
            &config,
            &InfoParts {
                executable: "app",
                cargo_version: "1.2.0",
                actool: Some(&actool),
                dt: &keys,
                release: true,
            },
        );
        assert!(missing_required(&info).is_empty());
        assert_eq!(info["CFBundleSupportedPlatforms"], json!(["iPhoneOS"]));
        assert_eq!(info["UIDeviceFamily"], json!([1]));
        assert_eq!(info["MinimumOSVersion"], "16.0");
        assert_eq!(info["UIRequiredDeviceCapabilities"], json!(["arm64"]));
        assert_eq!(info["DTXcodeBuild"], "27A266a");
        assert_eq!(info["DTSDKName"], "iphoneos27.0");
        assert_eq!(info["DTPlatformName"], "iphoneos");
        assert_eq!(info["BuildMachineOSBuild"], "26A434");
        assert_eq!(info["CFBundleIconName"], "AppIcon");
        assert_eq!(info["ITSAppUsesNonExemptEncryption"], true);
        assert_eq!(info["ITSEncryptionExportComplianceCode"], "ABC123");
        assert_eq!(super::super::dt::compare(&info, &keys), (vec![], vec![]));

        // A device run has no export-compliance keys; an unanswered
        // question leaves the key out.
        config.ios.uses_non_exempt_encryption = None;
        let dev = info_plist(
            &config,
            &InfoParts {
                executable: "app",
                cargo_version: "1.2.0",
                actool: None,
                dt: &keys,
                release: false,
            },
        );
        assert!(!dev.contains_key("ITSAppUsesNonExemptEncryption"));
        assert!(!dev.contains_key("ITSEncryptionExportComplianceCode"));
    }

    #[test]
    fn ipad_and_usage_gates() {
        let path = Path::new("Info.plist");
        let mut info: Map<String, Value> = serde_json::from_value(json!({
            "UIDeviceFamily": [1, 2],
            "UISupportedInterfaceOrientations": ["UIInterfaceOrientationPortrait"],
        }))
        .unwrap();
        assert!(ipad_orientations(&info, path).failed());
        let _ = info.insert("UIRequiresFullScreen".into(), json!(true));
        assert!(!ipad_orientations(&info, path).failed());

        let bytes = b"...AVCaptureDevice...LAContext...";
        let check = usage_descriptions(&info, bytes, path);
        assert!(check.failed());
        assert!(check.error.detail.contains("NSCameraUsageDescription"));
        assert!(check.error.detail.contains("NSFaceIDUsageDescription"));
        let _ = info.insert("NSCameraUsageDescription".into(), json!("Scan codes"));
        let _ = info.insert("NSFaceIDUsageDescription".into(), json!("Unlock"));
        assert!(!usage_descriptions(&info, bytes, path).failed());
    }

    #[test]
    fn assetutil_output_is_judged() {
        let car = Path::new("Assets.car");
        let opaque = r#"[{"AssetStorageVersion":"x"},{"AssetType":"Icon Image","Name":"AppIcon","PixelHeight":1024,"PixelWidth":1024,"Opaque":true}]"#;
        assert!(!judge_icon(opaque, car).failed());
        let clear = opaque.replace("\"Opaque\":true", "\"Opaque\":false");
        assert!(judge_icon(&clear, car).failed());
        let small = opaque.replace("1024", "180");
        assert!(
            judge_icon(&small, car)
                .error
                .detail
                .contains("no 1024x1024")
        );
    }
}
