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

/// `android:configChanges` values, the API level that introduced each, and
/// its bit (`ActivityInfo.CONFIG_*`, the mask Android logs when it
/// relaunches an activity). The activity handles all of them itself, so
/// Android never destroys and recreates it for them: a recreated activity
/// ends the iced application, which starts over in the new one and loses
/// its state. Policy data keyed by the API level the manifest is linked
/// against.
///
/// `assetsPaths` is a change of the app's resource overlays: on an
/// emulator's first boots SystemUI applies its theme overlays (the
/// `com.android.systemui-*.frro` palette), and Android relaunches every
/// running activity that does not list it (`wm_relaunch_resume_activity
/// … 80000000`), the app's included when that happens mid-launch. aapt2
/// knows the name from API 36's android.jar (`ActivityInfo.
/// CONFIG_ASSETS_PATHS` became public API there).
pub const CONFIG_CHANGES: &[(&str, u32, u32)] = &[
    ("mcc", 1, 0x0001),
    ("mnc", 1, 0x0002),
    ("locale", 1, 0x0004),
    ("touchscreen", 1, 0x0008),
    ("keyboard", 1, 0x0010),
    ("keyboardHidden", 1, 0x0020),
    ("navigation", 1, 0x0040),
    ("screenLayout", 3, 0x0100),
    ("fontScale", 1, 0x4000_0000),
    ("uiMode", 8, 0x0200),
    ("orientation", 1, 0x0080),
    ("density", 17, 0x1000),
    ("screenSize", 13, 0x0400),
    ("smallestScreenSize", 13, 0x0800),
    ("layoutDirection", 17, 0x2000),
    ("colorMode", 26, 0x4000),
    ("fontWeightAdjustment", 31, 0x1000_0000),
    ("grammaticalGender", 34, 0x8000),
    ("assetsPaths", 36, 0x8000_0000),
];

/// The `android:configChanges` value for a manifest linked against `api`.
pub fn config_changes(api: u32) -> String {
    CONFIG_CHANGES
        .iter()
        .filter(|(_, since, _)| *since <= api)
        .map(|(name, _, _)| *name)
        .collect::<Vec<_>>()
        .join("|")
}

/// The `configChanges` names in a configuration change mask, in the order
/// of [`CONFIG_CHANGES`]; bits without a name come last, in hex.
pub fn config_names(mask: u32) -> Vec<String> {
    let mut names: Vec<String> = CONFIG_CHANGES
        .iter()
        .filter(|(_, _, bit)| mask & bit != 0)
        .map(|(name, _, _)| (*name).to_string())
        .collect();
    let known = CONFIG_CHANGES.iter().fold(0, |all, (_, _, bit)| all | bit);
    if mask & !known != 0 {
        names.push(format!("0x{:x}", mask & !known));
    }
    names
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
    // `[android] back = "key"`: Back reaches the app as a key press
    // (`Key::Named(Named::BrowserBack)`) instead of finishing the activity.
    // Only for an app that handles it; the attribute exists from API 33.
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
/// background is `@color/icm_window_background`, so the first frame does
/// not flash, and whose bar icons are dark when `@bool/icm_light_bars` is
/// true.
///
/// From targetSdk 35 the app draws behind transparent system bars, so the
/// bars' icons sit on the app's own background. The parent is a dark
/// theme, whose icons are white: on a light background they would vanish.
/// The navigation bar's flag is API 27, and older devices ignore it.
///
/// Both values are resources, so that `values-night/` can give them other
/// values in dark mode ([`window_xml`]), while a style named `IcmTheme` in
/// `[android] res` still replaces this one in both modes.
pub fn themes_xml() -> String {
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<resources>\n");
    xml.push_str(&format!(
        "    <style name=\"{THEME}\" parent=\"@android:style/Theme.Material.NoActionBar\">\n"
    ));
    for item in [
        "android:windowBackground\">@color/icm_window_background",
        "android:colorBackground\">@color/icm_window_background",
        "android:windowLightStatusBar\">@bool/icm_light_bars",
        "android:windowLightNavigationBar\">@bool/icm_light_bars",
    ] {
        xml.push_str(&format!("        <item name=\"{item}</item>\n"));
    }
    xml.push_str("    </style>\n</resources>\n");
    xml
}

/// The window's background in dark mode when `[app] background` is light:
/// the background of iced's built-in dark theme (`Theme::Dark`), which an
/// app without a theme of its own draws in dark mode.
pub const NIGHT_BACKGROUND: [u8; 3] = [0x2B, 0x2D, 0x31];

/// The window's background in dark mode: `[app] background` when it is
/// dark already, else [`NIGHT_BACKGROUND`].
pub fn night_background(background: [u8; 3]) -> [u8; 3] {
    if is_light(background) {
        NIGHT_BACKGROUND
    } else {
        background
    }
}

/// `values/window.xml`, and `values-night/window.xml` for
/// [`night_background`]: the window's background (`icm_window_background`)
/// and whether the bars' icons are dark on it (`icm_light_bars`,
/// [`is_light`]), which the generated theme reads.
pub fn window_xml(background: [u8; 3]) -> String {
    let [red, green, blue] = background;
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<resources>\n    <color name=\"icm_window_background\">#{red:02X}{green:02X}{blue:02X}</color>\n    <bool name=\"icm_light_bars\">{}</bool>\n</resources>\n",
        is_light(background)
    )
}

/// Whether dark icons read better than white ones on a colour: its WCAG
/// contrast with black beats its contrast with white (relative luminance
/// above about 0.18).
pub fn is_light([r, g, b]: [u8; 3]) -> bool {
    let linear = |channel: u8| {
        let c = f64::from(channel) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
    (luminance + 0.05) / 0.05 > 1.05 / (luminance + 0.05)
}

/// `values/colors.xml`: `icm_background`, the adaptive icon's background,
/// the same in both modes.
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
            "android:roundIcon=\"@mipmap/ic_launcher_round\"",
            "android:name=\"android.app.NativeActivity\"",
            "android:screenOrientation=\"portrait\"",
            "android:configChanges=\"mcc|mnc|locale|touchscreen|keyboard|keyboardHidden|navigation|screenLayout|fontScale|uiMode|orientation|density|screenSize|smallestScreenSize|layoutDirection|colorMode|fontWeightAdjustment|grammaticalGender|assetsPaths\"",
            "<meta-data android:name=\"android.app.lib_name\" android:value=\"app\"/>",
            "<action android:name=\"android.intent.action.MAIN\"/>",
            "<category android:name=\"android.intent.category.LAUNCHER\"/>",
        ] {
            assert!(xml.contains(expected), "missing {expected} in\n{xml}");
        }
        assert!(!xml.contains("debuggable"));
        // The template leaves Back to Android (`back = "system"`).
        assert!(!xml.contains("enableOnBackInvokedCallback"));
    }

    #[test]
    fn config_masks_name_their_changes() {
        // wm_relaunch_resume_activity's mask when an overlay changes.
        assert_eq!(config_names(0x8000_0000), vec!["assetsPaths"]);
        assert_eq!(config_names(0x0280), vec!["uiMode", "orientation"]);
        assert_eq!(config_names(0x2000_0004), vec!["locale", "0x20000000"]);
        assert!(config_names(0).is_empty());
        let mut bits: Vec<u32> = CONFIG_CHANGES.iter().map(|(_, _, bit)| *bit).collect();
        bits.sort_unstable();
        bits.dedup();
        assert_eq!(bits.len(), CONFIG_CHANGES.len(), "one bit per name");
    }

    #[test]
    fn config_changes_follow_the_api_level() {
        assert!(config_changes(36).ends_with("fontWeightAdjustment|grammaticalGender|assetsPaths"));
        assert!(config_changes(35).ends_with("fontWeightAdjustment|grammaticalGender"));
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
        config.android.back = "key".into();
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
        assert!(xml.contains("android:enableOnBackInvokedCallback=\"false\""));

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
        let theme = themes_xml();
        assert!(theme.contains("Theme.Material.NoActionBar"));
        for item in [
            "<item name=\"android:windowBackground\">@color/icm_window_background</item>",
            "<item name=\"android:colorBackground\">@color/icm_window_background</item>",
            "<item name=\"android:windowLightStatusBar\">@bool/icm_light_bars</item>",
            "<item name=\"android:windowLightNavigationBar\">@bool/icm_light_bars</item>",
        ] {
            assert!(theme.contains(item), "{item}");
        }
        let white = window_xml([0xFF, 0xFF, 0xFF]);
        assert!(white.contains("<color name=\"icm_window_background\">#FFFFFF</color>"));
        assert!(white.contains("<bool name=\"icm_light_bars\">true</bool>"));
        let black = window_xml([0x00, 0x00, 0x00]);
        assert!(black.contains("<bool name=\"icm_light_bars\">false</bool>"));
        assert!(colors_xml("#FFFFFF").contains(">#FFFFFF<"));
        assert!(adaptive_icon_xml().contains("@mipmap/ic_launcher_foreground"));
    }

    #[test]
    fn dark_mode_darkens_a_light_window_only() {
        assert_eq!(night_background([0xFF; 3]), NIGHT_BACKGROUND);
        assert_eq!(night_background([0xFF, 0xD6, 0x0A]), NIGHT_BACKGROUND);
        assert_eq!(night_background([0x1C, 0x1C, 0x1E]), [0x1C, 0x1C, 0x1E]);
        assert!(!is_light(NIGHT_BACKGROUND));
        let night = window_xml(night_background([0xFF; 3]));
        assert!(night.contains("<color name=\"icm_window_background\">#2B2D31</color>"));
        assert!(night.contains("<bool name=\"icm_light_bars\">false</bool>"));
    }

    #[test]
    fn light_backgrounds_get_dark_bar_icons() {
        for light in [[0xFF; 3], [0xF2, 0xF2, 0xF7], [0xFF, 0xD6, 0x0A], [0x80; 3]] {
            assert!(is_light(light), "{light:?}");
        }
        for dark in [[0x00; 3], [0x1C, 0x1C, 0x1E], [0x00, 0x33, 0x99], [0x60; 3]] {
            assert!(!is_light(dark), "{dark:?}");
        }
    }
}
