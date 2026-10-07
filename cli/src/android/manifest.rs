//! The generated `AndroidManifest.xml` and value resources (design §9.4).
//!
//! Every managed attribute comes from icm.toml; `[android.manifest]` adds
//! attributes and raw XML. `aapt2 link --debug-mode` adds
//! `android:debuggable` to dev builds; the manifest itself never has it.

use crate::catalogue::CheckId;
use crate::config::{IcmToml, Orientation};
use crate::error::IcmError;

/// The activity every icm app runs in (`[android] activity = "native"`).
pub const ACTIVITY: &str = "android.app.NativeActivity";

/// The theme the generated `values/themes.xml` defines.
pub const THEME: &str = "IcmTheme";

/// `android:configChanges` values and the API level that introduced each.
/// The activity handles all of them itself, so Android never destroys and
/// recreates it (which freezes an iced app); policy data keyed by the API
/// level the manifest is linked against.
pub const CONFIG_CHANGES: &[(&str, u32)] = &[
    ("mcc", 1),
    ("mnc", 1),
    ("locale", 1),
    ("touchscreen", 1),
    ("keyboard", 1),
    ("keyboardHidden", 1),
    ("navigation", 1),
    ("screenLayout", 3),
    ("fontScale", 1),
    ("uiMode", 8),
    ("orientation", 1),
    ("density", 17),
    ("screenSize", 13),
    ("smallestScreenSize", 13),
    ("layoutDirection", 17),
    ("colorMode", 26),
    ("fontWeightAdjustment", 31),
    ("grammaticalGender", 34),
];

/// The `android:configChanges` value for a manifest linked against `api`.
pub fn config_changes(api: u32) -> String {
    CONFIG_CHANGES
        .iter()
        .filter(|(_, since)| *since <= api)
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join("|")
}

/// What the manifest is generated from, besides icm.toml.
#[derive(Clone, Debug)]
pub struct Inputs<'a> {
    /// The resolved icm.toml.
    pub config: &'a IcmToml,
    /// Cargo.toml's `version` (versionName).
    pub version: &'a str,
    /// The library name (`android.app.lib_name`).
    pub lib: &'a str,
}

/// Escapes text for an XML attribute value.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\n' => out.push_str("&#10;"),
            other => out.push(other),
        }
    }
    out
}

/// `android:screenOrientation` for the configured orientations: set only
/// when the app is locked to one axis (Android ignores it on large screens
/// from API 36).
pub fn screen_orientation(orientations: &[Orientation]) -> Option<&'static str> {
    let has = |o: Orientation| orientations.contains(&o);
    let portrait = has(Orientation::Portrait);
    let upside_down = has(Orientation::PortraitUpsideDown);
    // UIKit's landscape-right (home button on the right) is Android's
    // "landscape" (rotated 90 degrees counter-clockwise).
    let right = has(Orientation::LandscapeRight);
    let left = has(Orientation::LandscapeLeft);
    let portraits = portrait || upside_down;
    let landscapes = left || right;
    match (portraits, landscapes) {
        (true, false) if portrait && upside_down => Some("sensorPortrait"),
        (true, false) if portrait => Some("portrait"),
        (true, false) => Some("reversePortrait"),
        (false, true) if left && right => Some("sensorLandscape"),
        (false, true) if right => Some("landscape"),
        (false, true) => Some("reverseLandscape"),
        _ => None,
    }
}

/// The Android permissions `[app.permissions]` and
/// `[android] extra_permissions` ask for (design §7.5).
pub fn permissions(config: &IcmToml) -> Vec<String> {
    let p = &config.app.permissions;
    let mut out: Vec<String> = Vec::new();
    let mut add = |name: &str| {
        let full = if name.contains('.') {
            name.to_string()
        } else {
            format!("android.permission.{name}")
        };
        if !out.contains(&full) {
            out.push(full);
        }
    };
    if p.internet {
        add("INTERNET");
    }
    if p.camera.is_some() {
        add("CAMERA");
    }
    if p.microphone.is_some() {
        add("RECORD_AUDIO");
    }
    if p.face_id.is_some() {
        add("USE_BIOMETRIC");
    }
    if p.photos.is_some() {
        add("READ_MEDIA_IMAGES");
    }
    if p.location.is_some() {
        add("ACCESS_FINE_LOCATION");
    }
    if p.notifications {
        add("POST_NOTIFICATIONS");
    }
    for extra in &config.android.extra_permissions {
        add(extra.trim());
    }
    out
}

/// What raw XML in `[android.manifest]` may not contain, and the key that
/// sets it instead.
const RAW_FORBIDDEN: &[(&str, &str)] = &[
    ("<uses-sdk", "[android] min_sdk / target_sdk"),
    ("<application", "[android.manifest] application"),
    ("<manifest", "(generated)"),
    ("android.app.NativeActivity", "(generated: the activity)"),
    ("android:versionCode", "[app] build"),
    ("android:versionName", "Cargo.toml version"),
    ("android:debuggable", "(generated: dev builds only)"),
    ("package=", "[app] id"),
];

/// Rejects raw XML that sets something icm manages
/// (`config.raw_xml_forbidden`, exit 3).
pub fn check_raw_xml(key: &str, xml: &str) -> Result<(), IcmError> {
    for (needle, owner) in RAW_FORBIDDEN {
        if xml.contains(needle) {
            return Err(IcmError::new(
                CheckId::ConfigRawXmlForbidden,
                format!(
                    "`android.manifest.{key}` contains `{}`, which icm generates; set {owner} instead",
                    needle.trim_start_matches('<').trim_end_matches('=')
                ),
            ));
        }
    }
    let opens = xml.matches('<').count();
    let closes = xml.matches('>').count();
    if opens != closes {
        return Err(IcmError::new(
            CheckId::ConfigInvalid,
            format!("`android.manifest.{key}` is not well-formed XML (unbalanced < and >)"),
        ));
    }
    Ok(())
}

fn overlay_attributes(table: &toml::Table, indent: &str) -> String {
    let mut out = String::new();
    for (key, value) in table {
        let text = match value {
            toml::Value::String(s) => s.clone(),
            toml::Value::Boolean(b) => b.to_string(),
            toml::Value::Integer(i) => i.to_string(),
            toml::Value::Float(f) => f.to_string(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };
        out.push_str(&format!("\n{indent}{key}=\"{}\"", escape(&text)));
    }
    out
}

fn indent_block(xml: &str, indent: &str) -> String {
    xml.trim()
        .lines()
        .map(|line| format!("{indent}{}\n", line.trim_end()))
        .collect()
}

/// Generates the manifest.
pub fn manifest(inputs: &Inputs<'_>) -> Result<String, IcmError> {
    let config = inputs.config;
    let android = &config.android;
    let api = android.target_sdk;
    check_raw_xml("extra_manifest_xml", &android.manifest.extra_manifest_xml)?;
    check_raw_xml(
        "extra_application_xml",
        &android.manifest.extra_application_xml,
    )?;

    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    xml.push_str(&format!(
        "<manifest xmlns:android=\"http://schemas.android.com/apk/res/android\"\n    package=\"{}\"\n    android:versionCode=\"{}\"\n    android:versionName=\"{}\">\n",
        escape(&config.app.id),
        config.app.build,
        escape(inputs.version)
    ));
    xml.push_str(&format!(
        "    <uses-sdk android:minSdkVersion=\"{}\" android:targetSdkVersion=\"{}\"/>\n",
        android.min_sdk, android.target_sdk
    ));
    for permission in permissions(config) {
        xml.push_str(&format!(
            "    <uses-permission android:name=\"{}\"/>\n",
            escape(&permission)
        ));
    }
    if !android.manifest.extra_manifest_xml.trim().is_empty() {
        xml.push_str(&indent_block(&android.manifest.extra_manifest_xml, "    "));
    }

    let has_code = android.activity != "native";
    xml.push_str("    <application\n");
    xml.push_str(&format!(
        "        android:label=\"{}\"\n",
        escape(&config.app.name)
    ));
    xml.push_str("        android:icon=\"@mipmap/ic_launcher\"\n");
    if api >= 25 {
        xml.push_str("        android:roundIcon=\"@mipmap/ic_launcher_round\"\n");
    }
    xml.push_str(&format!("        android:hasCode=\"{has_code}\"\n"));
    xml.push_str("        android:extractNativeLibs=\"false\"\n");
    xml.push_str(&format!(
        "        android:allowBackup=\"{}\"\n",
        android.allow_backup
    ));
    if android.back == "key" && api >= 33 {
        xml.push_str("        android:enableOnBackInvokedCallback=\"false\"\n");
    }
    xml.push_str(&format!("        android:theme=\"@style/{THEME}\""));
    xml.push_str(&overlay_attributes(
        &android.manifest.application,
        "        ",
    ));
    xml.push_str(">\n");

    xml.push_str(&format!(
        "        <activity\n            android:name=\"{ACTIVITY}\"\n"
    ));
    xml.push_str("            android:exported=\"true\"\n");
    xml.push_str("            android:launchMode=\"singleTask\"\n");
    xml.push_str("            android:windowSoftInputMode=\"adjustResize|stateHidden\"\n");
    if let Some(orientation) = screen_orientation(&config.app.orientations) {
        xml.push_str(&format!(
            "            android:screenOrientation=\"{orientation}\"\n"
        ));
    }
    xml.push_str(&format!(
        "            android:configChanges=\"{}\"",
        config_changes(api)
    ));
    xml.push_str(&overlay_attributes(
        &android.manifest.activity,
        "            ",
    ));
    xml.push_str(">\n");
    xml.push_str(&format!(
        "            <meta-data android:name=\"android.app.lib_name\" android:value=\"{}\"/>\n",
        escape(inputs.lib)
    ));
    xml.push_str("            <intent-filter>\n");
    xml.push_str("                <action android:name=\"android.intent.action.MAIN\"/>\n");
    xml.push_str("                <category android:name=\"android.intent.category.LAUNCHER\"/>\n");
    xml.push_str("            </intent-filter>\n");
    xml.push_str("        </activity>\n");
    if !android.manifest.extra_application_xml.trim().is_empty() {
        xml.push_str(&indent_block(
            &android.manifest.extra_application_xml,
            "        ",
        ));
    }
    xml.push_str("    </application>\n");
    xml.push_str("</manifest>\n");
    Ok(xml)
}

/// `values/themes.xml`: a theme without an action bar whose window
/// background is `[app] background`, so the first frame does not flash.
pub fn themes_xml() -> String {
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<resources>\n");
    xml.push_str(&format!(
        "    <style name=\"{THEME}\" parent=\"@android:style/Theme.Material.NoActionBar\">\n"
    ));
    xml.push_str("        <item name=\"android:windowBackground\">@color/icm_background</item>\n");
    xml.push_str("        <item name=\"android:colorBackground\">@color/icm_background</item>\n");
    xml.push_str("    </style>\n</resources>\n");
    xml
}

/// `values/colors.xml`.
pub fn colors_xml(background: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<resources>\n    <color name=\"icm_background\">{}</color>\n</resources>\n",
        escape(background)
    )
}

/// `mipmap-anydpi-v26/ic_launcher.xml` (and `_round`): an adaptive icon
/// with the background colour and the padded foreground.
pub fn adaptive_icon_xml() -> String {
    "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<adaptive-icon xmlns:android=\"http://schemas.android.com/apk/res/android\">\n    <background android:drawable=\"@color/icm_background\"/>\n    <foreground android:drawable=\"@mipmap/ic_launcher_foreground\"/>\n</adaptive-icon>\n".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template() -> IcmToml {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/app/icm.toml"),
        )
        .unwrap();
        toml::from_str(&text).unwrap()
    }

    #[test]
    fn the_template_manifest() {
        let config = template();
        let xml = manifest(&Inputs {
            config: &config,
            version: "0.1.0",
            lib: "app",
        })
        .unwrap();
        for expected in [
            "package=\"com.example.app\"",
            "android:versionCode=\"1\"",
            "android:versionName=\"0.1.0\"",
            "<uses-sdk android:minSdkVersion=\"26\" android:targetSdkVersion=\"36\"/>",
            "<uses-permission android:name=\"android.permission.INTERNET\"/>",
            "android:hasCode=\"false\"",
            "android:extractNativeLibs=\"false\"",
            "android:enableOnBackInvokedCallback=\"false\"",
            "android:roundIcon=\"@mipmap/ic_launcher_round\"",
            "android:name=\"android.app.NativeActivity\"",
            "android:screenOrientation=\"portrait\"",
            "android:configChanges=\"mcc|mnc|locale|touchscreen|keyboard|keyboardHidden|navigation|screenLayout|fontScale|uiMode|orientation|density|screenSize|smallestScreenSize|layoutDirection|colorMode|fontWeightAdjustment|grammaticalGender\"",
            "<meta-data android:name=\"android.app.lib_name\" android:value=\"app\"/>",
            "<action android:name=\"android.intent.action.MAIN\"/>",
            "<category android:name=\"android.intent.category.LAUNCHER\"/>",
        ] {
            assert!(xml.contains(expected), "missing {expected} in\n{xml}");
        }
        assert!(!xml.contains("debuggable"));
    }

    #[test]
    fn config_changes_follow_the_api_level() {
        assert!(config_changes(36).ends_with("fontWeightAdjustment|grammaticalGender"));
        assert!(!config_changes(33).contains("grammaticalGender"));
        assert!(config_changes(33).contains("fontWeightAdjustment"));
        assert!(!config_changes(30).contains("fontWeightAdjustment"));
        assert!(config_changes(30).contains("colorMode"));
    }

    #[test]
    fn overlays_and_escaping() {
        let mut config = template();
        config.app.name = "Tom & \"Jerry\"".into();
        let _ = config.android.manifest.application.insert(
            "android:dataExtractionRules".into(),
            toml::Value::String("@xml/rules".into()),
        );
        config.android.manifest.extra_application_xml =
            "<meta-data android:name=\"x\" android:value=\"y\"/>".into();
        config.android.extra_permissions = vec!["VIBRATE".into(), "com.x.PERM".into()];
        config.app.orientations = vec![Orientation::LandscapeLeft, Orientation::LandscapeRight];
        config.android.back = "system".into();
        let xml = manifest(&Inputs {
            config: &config,
            version: "1.2.3",
            lib: "my_lib",
        })
        .unwrap();
        assert!(
            xml.contains("android:label=\"Tom &amp; &quot;Jerry&quot;\""),
            "{xml}"
        );
        assert!(xml.contains("android:dataExtractionRules=\"@xml/rules\""));
        assert!(xml.contains("        <meta-data android:name=\"x\" android:value=\"y\"/>\n"));
        assert!(xml.contains("android.permission.VIBRATE"));
        assert!(xml.contains("\"com.x.PERM\""));
        assert!(xml.contains("sensorLandscape"));
        assert!(!xml.contains("enableOnBackInvokedCallback"));

        config.android.manifest.extra_manifest_xml =
            "<uses-sdk android:minSdkVersion=\"1\"/>".into();
        let error = manifest(&Inputs {
            config: &config,
            version: "1",
            lib: "x",
        })
        .unwrap_err();
        assert_eq!(error.id, "config.raw_xml_forbidden");
    }

    #[test]
    fn orientations() {
        use Orientation::*;
        assert_eq!(screen_orientation(&[Portrait]), Some("portrait"));
        assert_eq!(
            screen_orientation(&[Portrait, PortraitUpsideDown]),
            Some("sensorPortrait")
        );
        assert_eq!(screen_orientation(&[LandscapeRight]), Some("landscape"));
        assert_eq!(
            screen_orientation(&[LandscapeLeft]),
            Some("reverseLandscape")
        );
        assert_eq!(screen_orientation(&[Portrait, LandscapeLeft]), None);
        assert_eq!(screen_orientation(&[]), None);
    }

    #[test]
    fn resources() {
        assert!(themes_xml().contains("Theme.Material.NoActionBar"));
        assert!(colors_xml("#FFFFFF").contains(">#FFFFFF<"));
        assert!(adaptive_icon_xml().contains("@mipmap/ic_launcher_foreground"));
    }
}
