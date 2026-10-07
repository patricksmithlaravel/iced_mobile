//! The macOS `.app` (design §9.6, §11.4 step 2): `Contents/Info.plist`,
//! `Contents/PkgInfo`, `Contents/MacOS/<bin>`, and in `Contents/Resources`
//! `AppIcon.icns`, `THIRD_PARTY_NOTICES.txt` and `[app] resources`; plus
//! the entitlements the hardened runtime needs for `[app.permissions]`.
//!
//! Plists are written with the iOS bundle's XML writer
//! ([`crate::platform::ios_sim::plist::to_xml`]), keys sorted.

use crate::config::{IcmToml, Permissions};
use crate::platform::ios_sim::plist::short_version;
use serde_json::{Map, Value, json};

/// The icon's file name inside `Contents/Resources` (without `.icns`).
pub const ICON: &str = "AppIcon";

/// What the Info.plist is made of besides icm.toml.
#[derive(Clone, Debug)]
pub struct Inputs<'a> {
    /// The executable's file name.
    pub executable: &'a str,
    /// The Cargo version.
    pub version: &'a str,
    /// Whether `AppIcon.icns` is in the bundle.
    pub icon: bool,
}

/// The NS*UsageDescription keys macOS reads for `[app.permissions]`
/// (design §7.5: camera, microphone and location; Face ID and the photo
/// library have none on macOS).
pub fn usage_descriptions(permissions: &Permissions) -> Vec<(&'static str, String)> {
    let mut keys = Vec::new();
    if let Some(why) = &permissions.camera {
        keys.push(("NSCameraUsageDescription", why.clone()));
    }
    if let Some(why) = &permissions.microphone {
        keys.push(("NSMicrophoneUsageDescription", why.clone()));
    }
    if let Some(why) = &permissions.location {
        keys.push(("NSLocationUsageDescription", why.clone()));
        keys.push(("NSLocationWhenInUseUsageDescription", why.clone()));
    }
    keys
}

/// `Contents/Info.plist`.
pub fn info_plist(config: &IcmToml, inputs: &Inputs<'_>) -> Map<String, Value> {
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
        json!(short_version(inputs.version)),
    );
    set("CFBundleSupportedPlatforms", json!(["MacOSX"]));
    set("CFBundleVersion", json!(app.build.to_string()));
    set("LSMinimumSystemVersion", json!(config.desktop.macos.min_os));
    set("NSHighResolutionCapable", json!(true));
    set("NSSupportsAutomaticGraphicsSwitching", json!(true));
    if inputs.icon {
        set("CFBundleIconFile", json!(ICON));
    }
    if let Some(category) = app
        .category
        .as_deref()
        .and_then(crate::release::desktop::macos_category)
    {
        set("LSApplicationCategoryType", json!(category));
    }
    if let Some(copyright) = &app.copyright {
        set("NSHumanReadableCopyright", json!(copyright));
    }
    for (key, why) in usage_descriptions(&app.permissions) {
        set(key, json!(why));
    }
    plist
}

/// The keys every app's Info.plist must have.
pub const REQUIRED: &[&str] = &[
    "CFBundleExecutable",
    "CFBundleIdentifier",
    "CFBundleName",
    "CFBundlePackageType",
    "CFBundleShortVersionString",
    "CFBundleVersion",
    "LSMinimumSystemVersion",
];

/// The required keys a plist lacks (or holds empty).
pub fn missing_required(plist: &Map<String, Value>) -> Vec<&'static str> {
    REQUIRED
        .iter()
        .copied()
        .filter(|key| match plist.get(*key) {
            Some(Value::String(text)) => text.is_empty(),
            Some(_) => false,
            None => true,
        })
        .collect()
}

/// The entitlements the hardened runtime needs for `[app.permissions]`:
/// without them macOS denies the camera, the microphone and location to a
/// hardened app even when the user allows it.
pub fn entitlements(permissions: &Permissions) -> Map<String, Value> {
    let mut map = Map::new();
    if permissions.camera.is_some() {
        let _ = map.insert("com.apple.security.device.camera".into(), json!(true));
    }
    if permissions.microphone.is_some() {
        let _ = map.insert("com.apple.security.device.audio-input".into(), json!(true));
    }
    if permissions.location.is_some() {
        let _ = map.insert(
            "com.apple.security.personal-information.location".into(),
            json!(true),
        );
    }
    map
}

/// `Contents/PkgInfo`.
pub const PKG_INFO: &[u8] = b"APPL????";

#[cfg(test)]
mod tests {
    use super::*;

    fn config(extra: &str) -> IcmToml {
        crate::config::parse(
            std::path::Path::new("/p/icm.toml"),
            &format!(
                "schema = 1\n[app]\nname = \"Notes\"\nid = \"com.acme.notes\"\nbuild = 12\ncategory = \"productivity\"\ncopyright = \"© 2026 Acme\"\n{extra}"
            ),
        )
        .unwrap_or_else(|errors| panic!("{errors:?}"))
        .config
    }

    #[test]
    fn the_info_plist_carries_identity_version_and_floor() {
        let config = config(
            "[app.permissions]\ncamera = \"to scan notes\"\n[desktop.macos]\nmin_os = \"13.0\"\n",
        );
        let plist = info_plist(
            &config,
            &Inputs {
                executable: "notes",
                version: "1.2.3",
                icon: true,
            },
        );
        assert_eq!(plist["CFBundleIdentifier"], "com.acme.notes");
        assert_eq!(plist["CFBundleExecutable"], "notes");
        assert_eq!(plist["CFBundleShortVersionString"], "1.2.3");
        assert_eq!(plist["CFBundleVersion"], "12");
        assert_eq!(plist["LSMinimumSystemVersion"], "13.0");
        assert_eq!(plist["CFBundleIconFile"], "AppIcon");
        assert_eq!(
            plist["LSApplicationCategoryType"],
            "public.app-category.productivity"
        );
        assert_eq!(plist["NSHumanReadableCopyright"], "© 2026 Acme");
        assert_eq!(plist["NSCameraUsageDescription"], "to scan notes");
        assert!(missing_required(&plist).is_empty());
        let xml = crate::platform::ios_sim::plist::to_xml(&Value::Object(plist));
        assert!(xml.contains("<key>NSHighResolutionCapable</key>"));

        let ents = entitlements(&config.app.permissions);
        assert_eq!(ents.len(), 1);
        assert_eq!(ents["com.apple.security.device.camera"], true);
        assert!(entitlements(&Permissions::default()).is_empty());
    }

    #[test]
    fn missing_keys_are_named() {
        let mut plist = Map::new();
        let _ = plist.insert("CFBundleExecutable".into(), json!(""));
        let missing = missing_required(&plist);
        assert!(missing.contains(&"CFBundleExecutable"));
        assert!(missing.contains(&"LSMinimumSystemVersion"));
    }
}
