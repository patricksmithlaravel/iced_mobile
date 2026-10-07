//! Property lists (design §9.1, §9.2): a small XML writer over
//! `serde_json::Value`, the simulator `Info.plist` and
//! `PrivacyInfo.xcprivacy`.
//!
//! icm reads plists that tools write (actool's partial plist, device-type
//! capabilities) with `plutil -convert json`, so it needs no plist crate.
//! Dictionaries are written with sorted keys, like Xcode does.

use crate::config::{IcmToml, Orientation, Permissions};
use serde_json::{Map, Value, json};

/// The XML document for a plist value.
pub fn to_xml(root: &Value) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n",
    );
    write_value(&mut out, root, 0);
    out.push_str("</plist>\n");
    out
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn write_value(out: &mut String, value: &Value, depth: usize) {
    let indent = "\t".repeat(depth);
    match value {
        // Plists have no null; callers leave such keys out.
        Value::Null => {}
        Value::Bool(true) => out.push_str(&format!("{indent}<true/>\n")),
        Value::Bool(false) => out.push_str(&format!("{indent}<false/>\n")),
        Value::Number(number) if number.is_f64() => {
            out.push_str(&format!("{indent}<real>{number}</real>\n"));
        }
        Value::Number(number) => out.push_str(&format!("{indent}<integer>{number}</integer>\n")),
        Value::String(text) => {
            out.push_str(&format!("{indent}<string>{}</string>\n", escape(text)));
        }
        Value::Array(items) => {
            let items: Vec<&Value> = items.iter().filter(|item| !item.is_null()).collect();
            if items.is_empty() {
                out.push_str(&format!("{indent}<array/>\n"));
            } else {
                out.push_str(&format!("{indent}<array>\n"));
                for item in items {
                    write_value(out, item, depth + 1);
                }
                out.push_str(&format!("{indent}</array>\n"));
            }
        }
        Value::Object(map) => {
            let entries: Vec<(&String, &Value)> =
                map.iter().filter(|(_, value)| !value.is_null()).collect();
            if entries.is_empty() {
                out.push_str(&format!("{indent}<dict/>\n"));
            } else {
                out.push_str(&format!("{indent}<dict>\n"));
                for (key, value) in entries {
                    out.push_str(&format!("{indent}\t<key>{}</key>\n", escape(key)));
                    write_value(out, value, depth + 1);
                }
                out.push_str(&format!("{indent}</dict>\n"));
            }
        }
    }
}

/// A TOML value as a plist value (datetimes become strings).
pub fn from_toml(value: &toml::Value) -> Value {
    match value {
        toml::Value::String(text) => Value::String(text.clone()),
        toml::Value::Integer(number) => json!(number),
        toml::Value::Float(number) => json!(number),
        toml::Value::Boolean(flag) => Value::Bool(*flag),
        toml::Value::Datetime(datetime) => Value::String(datetime.to_string()),
        toml::Value::Array(items) => Value::Array(items.iter().map(from_toml).collect()),
        toml::Value::Table(table) => Value::Object(
            table
                .iter()
                .map(|(key, value)| (key.clone(), from_toml(value)))
                .collect(),
        ),
    }
}

/// `UIInterfaceOrientationPortrait`, ...
pub fn orientation_key(orientation: Orientation) -> &'static str {
    match orientation {
        Orientation::Portrait => "UIInterfaceOrientationPortrait",
        Orientation::PortraitUpsideDown => "UIInterfaceOrientationPortraitUpsideDown",
        Orientation::LandscapeLeft => "UIInterfaceOrientationLandscapeLeft",
        Orientation::LandscapeRight => "UIInterfaceOrientationLandscapeRight",
    }
}

/// The NS*UsageDescription keys `[app.permissions]` maps to (design §7.5).
pub fn usage_descriptions(permissions: &Permissions) -> Vec<(&'static str, String)> {
    [
        ("NSCameraUsageDescription", &permissions.camera),
        ("NSMicrophoneUsageDescription", &permissions.microphone),
        ("NSFaceIDUsageDescription", &permissions.face_id),
        ("NSPhotoLibraryUsageDescription", &permissions.photos),
        ("NSLocationWhenInUseUsageDescription", &permissions.location),
    ]
    .into_iter()
    .filter_map(|(key, why)| why.as_ref().map(|why| (key, why.clone())))
    .collect()
}

/// The marketing version as the bundle wants it: the numeric `X.Y.Z` part
/// of the Cargo version (`0.1.0-beta.1` gives `0.1.0`).
pub fn short_version(cargo_version: &str) -> String {
    cargo_version
        .split(['-', '+'])
        .next()
        .unwrap_or(cargo_version)
        .to_string()
}

/// The asset-catalog name of the app icon.
pub const APP_ICON: &str = "AppIcon";

/// The asset-catalog name of the launch-screen colour.
pub const LAUNCH_COLOR: &str = "LaunchBackground";

/// What the Info.plist is generated from besides icm.toml.
#[derive(Clone, Debug)]
pub struct InfoInputs<'a> {
    /// The executable's file name (`CFBundleExecutable`).
    pub executable: &'a str,
    /// The Cargo package version.
    pub cargo_version: &'a str,
    /// `iPhoneSimulator` or `iPhoneOS`.
    pub platform: &'a str,
    /// actool's partial plist (icon keys), when an icon was compiled.
    pub actool: Option<&'a Map<String, Value>>,
}

/// The Info.plist for the simulator (design §9.1): the managed keys, the
/// scene manifest (mandatory with the iOS 27 SDK), the launch screen, the
/// icon keys from actool, then the `[ios.info_plist]` overlay (whose
/// managed keys config validation already refused). No DT* keys.
pub fn info_plist(config: &IcmToml, inputs: &InfoInputs<'_>) -> Map<String, Value> {
    let app = &config.app;
    let mut plist = Map::new();
    let mut set = |key: &str, value: Value| {
        let _ = plist.insert(key.to_string(), value);
    };

    set("CFBundleDevelopmentRegion", json!("en"));
    set("CFBundleDisplayName", json!(app.name));
    set("CFBundleExecutable", json!(inputs.executable));
    set("CFBundleIdentifier", json!(app.id));
    set("CFBundleInfoDictionaryVersion", json!("6.0"));
    set("CFBundleName", json!(app.name));
    set("CFBundlePackageType", json!("APPL"));
    set(
        "CFBundleShortVersionString",
        json!(short_version(inputs.cargo_version)),
    );
    set("CFBundleSupportedPlatforms", json!([inputs.platform]));
    set("CFBundleVersion", json!(app.build.to_string()));
    set("LSRequiresIPhoneOS", json!(true));
    set("MinimumOSVersion", json!(config.ios.min_os));
    set(
        "UIApplicationSceneManifest",
        json!({
            "UIApplicationSupportsMultipleScenes": false,
            "UISceneConfigurations": {
                "UIWindowSceneSessionRoleApplication": [
                    {"UISceneConfigurationName": "Default"}
                ]
            }
        }),
    );
    set("UIDeviceFamily", json!([1]));
    set("UILaunchScreen", json!({"UIColorName": LAUNCH_COLOR}));
    set("UIRequiredDeviceCapabilities", json!(["arm64"]));
    set(
        "UISupportedInterfaceOrientations",
        Value::Array(
            app.orientations
                .iter()
                .map(|o| json!(orientation_key(*o)))
                .collect(),
        ),
    );
    for (key, why) in usage_descriptions(&app.permissions) {
        set(key, json!(why));
    }

    if let Some(actool) = inputs.actool {
        for (key, value) in actool {
            set(key, value.clone());
        }
        set("CFBundleIconName", json!(APP_ICON));
    }

    for (key, value) in &config.ios.info_plist {
        set(key, from_toml(value));
    }

    plist
}

/// `PrivacyInfo.xcprivacy` from `[ios.privacy]` (design §9.2).
pub fn privacy_info(config: &IcmToml) -> Map<String, Value> {
    let privacy = &config.ios.privacy;
    let api_types: Vec<Value> = privacy
        .api_reasons
        .iter()
        .map(|(category, reasons)| {
            let category = if category.starts_with("NSPrivacyAccessedAPICategory") {
                category.clone()
            } else {
                format!("NSPrivacyAccessedAPICategory{category}")
            };
            json!({
                "NSPrivacyAccessedAPIType": category,
                "NSPrivacyAccessedAPITypeReasons": reasons,
            })
        })
        .collect();

    let mut plist = Map::new();
    let _ = plist.insert("NSPrivacyTracking".into(), json!(privacy.tracking));
    let _ = plist.insert(
        "NSPrivacyTrackingDomains".into(),
        json!(privacy.tracking_domains),
    );
    let _ = plist.insert(
        "NSPrivacyCollectedDataTypes".into(),
        Value::Array(privacy.collected_data.iter().map(from_toml).collect()),
    );
    let _ = plist.insert("NSPrivacyAccessedAPITypes".into(), Value::Array(api_types));
    plist
}

/// The keys every simulator Info.plist must hold (`ios.plist.required_keys`).
pub const REQUIRED_KEYS: &[&str] = &[
    "CFBundleIdentifier",
    "CFBundleExecutable",
    "CFBundleName",
    "CFBundlePackageType",
    "CFBundleShortVersionString",
    "CFBundleVersion",
    "CFBundleSupportedPlatforms",
    "MinimumOSVersion",
    "UIApplicationSceneManifest",
    "UILaunchScreen",
];

/// The required keys a plist lacks.
pub fn missing_required(plist: &Map<String, Value>) -> Vec<&'static str> {
    REQUIRED_KEYS
        .iter()
        .copied()
        .filter(|key| !plist.contains_key(*key))
        .collect()
}

/// Whether a plist declares a window scene configuration
/// (`ios.plist.scene_manifest`; without one, an app linked with the iOS 27
/// SDK is killed at launch).
pub fn has_scene_manifest(plist: &Map<String, Value>) -> bool {
    plist
        .get("UIApplicationSceneManifest")
        .and_then(|manifest| manifest.get("UISceneConfigurations"))
        .and_then(|configurations| configurations.get("UIWindowSceneSessionRoleApplication"))
        .and_then(Value::as_array)
        .is_some_and(|roles| !roles.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn template() -> IcmToml {
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/app/icm.toml"),
        )
        .unwrap();
        crate::config::parse(Path::new("/x/icm.toml"), &text)
            .unwrap()
            .config
    }

    #[test]
    fn xml_is_well_formed_and_escaped() {
        let value = json!({
            "b": true, "a": "x < y & z", "n": 3, "r": 1.5, "e": [], "d": {}, "skip": null,
            "list": ["one", {"k": false}]
        });
        let xml = to_xml(&value);
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist"));
        assert!(xml.contains("<key>a</key>\n\t<string>x &lt; y &amp; z</string>"));
        assert!(xml.contains("<integer>3</integer>"));
        assert!(xml.contains("<real>1.5</real>"));
        assert!(xml.contains("<key>e</key>\n\t<array/>"));
        assert!(xml.contains("<key>d</key>\n\t<dict/>"));
        assert!(!xml.contains("skip"));
        // Keys are sorted.
        assert!(xml.find("<key>a</key>").unwrap() < xml.find("<key>b</key>").unwrap());
        assert!(xml.ends_with("</dict>\n</plist>\n"));
    }

    #[test]
    fn the_simulator_info_plist_has_the_managed_keys() {
        let mut config = template();
        config.app.permissions.camera = Some("Scan codes".into());
        let _ = config.ios.info_plist.insert(
            "ITSAppUsesNonExemptEncryption".into(),
            toml::Value::Boolean(false),
        );
        let actool: Map<String, Value> = serde_json::from_value(json!({
            "CFBundleIcons": {"CFBundlePrimaryIcon": {"CFBundleIconFiles": ["AppIcon60x60"], "CFBundleIconName": "AppIcon"}}
        }))
        .unwrap();

        let plist = info_plist(
            &config,
            &InfoInputs {
                executable: "app",
                cargo_version: "0.1.0-beta.2",
                platform: "iPhoneSimulator",
                actool: Some(&actool),
            },
        );
        assert!(missing_required(&plist).is_empty());
        assert!(has_scene_manifest(&plist));
        assert_eq!(plist["CFBundleIdentifier"], "com.example.app");
        assert_eq!(plist["CFBundleExecutable"], "app");
        assert_eq!(plist["CFBundleShortVersionString"], "0.1.0");
        assert_eq!(plist["CFBundleVersion"], "1");
        assert_eq!(plist["MinimumOSVersion"], "16.0");
        assert_eq!(
            plist["CFBundleSupportedPlatforms"],
            json!(["iPhoneSimulator"])
        );
        assert_eq!(
            plist["UISupportedInterfaceOrientations"],
            json!(["UIInterfaceOrientationPortrait"])
        );
        assert_eq!(plist["UILaunchScreen"]["UIColorName"], LAUNCH_COLOR);
        assert_eq!(plist["NSCameraUsageDescription"], "Scan codes");
        assert_eq!(plist["CFBundleIconName"], APP_ICON);
        assert!(plist.contains_key("CFBundleIcons"));
        assert_eq!(plist["ITSAppUsesNonExemptEncryption"], false);
        assert!(!plist.keys().any(|key| key.starts_with("DT")));

        // Without an icon, no icon keys.
        let bare = info_plist(
            &config,
            &InfoInputs {
                executable: "app",
                cargo_version: "1.2.3",
                platform: "iPhoneSimulator",
                actool: None,
            },
        );
        assert!(!bare.contains_key("CFBundleIconName"));
    }

    #[test]
    fn privacy_info_maps_the_reasons() {
        let plist = privacy_info(&template());
        assert_eq!(plist["NSPrivacyTracking"], false);
        assert_eq!(
            plist["NSPrivacyAccessedAPITypes"],
            json!([
                {"NSPrivacyAccessedAPIType": "NSPrivacyAccessedAPICategoryFileTimestamp",
                 "NSPrivacyAccessedAPITypeReasons": ["C617.1"]},
                {"NSPrivacyAccessedAPIType": "NSPrivacyAccessedAPICategorySystemBootTime",
                 "NSPrivacyAccessedAPITypeReasons": ["35F9.1"]}
            ])
        );
        let xml = to_xml(&Value::Object(plist));
        assert!(xml.contains("<key>NSPrivacyCollectedDataTypes</key>\n\t<array/>"));
    }

    #[test]
    fn scene_manifests_are_detected() {
        let without: Map<String, Value> =
            serde_json::from_value(json!({"UIApplicationSceneManifest": {}})).unwrap();
        assert!(!has_scene_manifest(&without));
        assert_eq!(short_version("2.0.1+build.5"), "2.0.1");
    }
}
