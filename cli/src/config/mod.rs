//! `icm.toml` (design §7): types, discovery, loading with `file:line`
//! findings, and validation.
//!
//! Unknown keys are rejected (`config.unknown_key`), type errors are
//! `config.invalid`, both with the offending line. The minimum icm version
//! is compared with semver *ordering* against a plain version (Appendix C
//! item 5): write `min_icm = "0.14.1-mobile.1"`. The older form
//! `icm = ">=0.14.1-mobile.1"` is still read, as the same minimum.

pub mod source;

use crate::catalogue::CheckId;
use crate::error::{Evidence, IcmError};
use serde::{Deserialize, Serialize};
use source::Source;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The file name.
pub const FILE_NAME: &str = "icm.toml";

/// The schema version this icm reads.
pub const SCHEMA: u32 = 1;

/// A platform an app ships on (`[app] platforms`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppPlatform {
    /// macOS, Windows, Linux.
    Desktop,
    /// The browser.
    Web,
    /// iPhone.
    Ios,
    /// Android phones.
    Android,
}

/// A supported interface orientation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Orientation {
    /// Upright.
    Portrait,
    /// Upside down.
    PortraitUpsideDown,
    /// Rotated left.
    LandscapeLeft,
    /// Rotated right.
    LandscapeRight,
}

/// An Android ABI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Abi {
    /// 64-bit ARM.
    #[serde(rename = "arm64-v8a")]
    Arm64V8a,
    /// 64-bit x86 (emulators on Intel hosts, Chromebooks).
    #[serde(rename = "x86_64")]
    X86_64,
    /// 32-bit ARM.
    #[serde(rename = "armeabi-v7a")]
    ArmeabiV7a,
    /// 32-bit x86.
    #[serde(rename = "x86")]
    X86,
}

impl Abi {
    /// The ABI's name.
    pub fn as_str(self) -> &'static str {
        match self {
            Abi::Arm64V8a => "arm64-v8a",
            Abi::X86_64 => "x86_64",
            Abi::ArmeabiV7a => "armeabi-v7a",
            Abi::X86 => "x86",
        }
    }

    /// The Rust target triple.
    pub fn triple(self) -> &'static str {
        match self {
            Abi::Arm64V8a => "aarch64-linux-android",
            Abi::X86_64 => "x86_64-linux-android",
            Abi::ArmeabiV7a => "armv7-linux-androideabi",
            Abi::X86 => "i686-linux-android",
        }
    }

    /// The ABI for an `ro.product.cpu.abi` value.
    pub fn from_name(name: &str) -> Option<Abi> {
        [Abi::Arm64V8a, Abi::X86_64, Abi::ArmeabiV7a, Abi::X86]
            .into_iter()
            .find(|abi| abi.as_str() == name.trim())
    }
}

/// The whole file.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IcmToml {
    /// The schema version (1).
    pub schema: u32,
    /// The oldest icm that may read this file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_icm: Option<String>,
    /// The older spelling of `min_icm`: `">=X"` or `"X"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icm: Option<String>,
    /// `[app]`.
    pub app: AppConfig,
    /// `[ios]`.
    #[serde(default)]
    pub ios: IosConfig,
    /// `[android]`.
    #[serde(default)]
    pub android: AndroidConfig,
    /// `[web]`.
    #[serde(default)]
    pub web: WebConfig,
    /// `[desktop]`.
    #[serde(default)]
    pub desktop: DesktopConfig,
    /// `[test]`.
    #[serde(default)]
    pub test: TestConfig,
    /// `[checks]`: hook scripts per dev platform.
    #[serde(default)]
    pub checks: BTreeMap<String, Vec<String>>,
    /// `[review]`.
    #[serde(default)]
    pub review: ReviewConfig,
}

/// `[app]`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// The display name.
    pub name: String,
    /// The reverse-DNS identifier; permanent after the first store upload.
    pub id: String,
    /// The store build number.
    #[serde(default = "default_build")]
    pub build: u64,
    /// The platforms the app ships on.
    #[serde(default = "default_platforms")]
    pub platforms: Vec<AppPlatform>,
    /// The cargo package (default: the one next to icm.toml).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// The library target (Android's `lib<lib>.so`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lib: Option<String>,
    /// The binary target (iOS, desktop, web).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin: Option<String>,
    /// A square PNG of at least 1024x1024.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// `#RRGGBB`: launch colour, icon flattening, web theme.
    #[serde(default = "default_background")]
    pub background: String,
    /// Supported orientations.
    #[serde(default = "default_orientations")]
    pub orientations: Vec<Orientation>,
    /// The publisher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    /// The copyright line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copyright: Option<String>,
    /// A one-line description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The store category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Globs bundled with the app.
    #[serde(default)]
    pub resources: Vec<String>,
    /// Compile the debug-only agent bridge into `icm run` builds (phase 6).
    #[serde(default)]
    pub agent: bool,
    /// `[app.permissions]`.
    #[serde(default)]
    pub permissions: Permissions,
}

/// `[app.permissions]` (design §7.5).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    /// Network access.
    #[serde(default)]
    pub internet: bool,
    /// The camera, with the reason shown to the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<String>,
    /// The microphone, with the reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub microphone: Option<String>,
    /// Face ID / biometrics, with the reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_id: Option<String>,
    /// The photo library, with the reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub photos: Option<String>,
    /// Location while in use, with the reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Notifications.
    #[serde(default)]
    pub notifications: bool,
}

/// `[ios]`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IosConfig {
    /// The minimum iOS version.
    #[serde(default = "default_ios_min_os")]
    pub min_os: String,
    /// Device families (schema 1: `iphone` only).
    #[serde(default = "default_ios_devices")]
    pub devices: Vec<String>,
    /// The Apple team id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    /// The export-compliance answer; required for release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uses_non_exempt_encryption: Option<bool>,
    /// ITSEncryptionExportComplianceCode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_compliance_code: Option<String>,
    /// The numeric App Store Connect id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asc_app_id: Option<String>,
    /// Package override for iOS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Binary override for iOS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin: Option<String>,
    /// `[ios.signing]`.
    #[serde(default)]
    pub signing: IosSigning,
    /// `[ios.privacy]`.
    #[serde(default)]
    pub privacy: IosPrivacy,
    /// `[ios.entitlements]`: added to the minimal set.
    #[serde(default)]
    pub entitlements: toml::Table,
    /// `[ios.info_plist]`: extra keys; managed keys are refused.
    #[serde(default)]
    pub info_plist: toml::Table,
}

impl Default for IosConfig {
    fn default() -> Self {
        IosConfig {
            min_os: default_ios_min_os(),
            devices: default_ios_devices(),
            team_id: None,
            uses_non_exempt_encryption: None,
            export_compliance_code: None,
            asc_app_id: None,
            package: None,
            bin: None,
            signing: IosSigning::default(),
            privacy: IosPrivacy::default(),
            entitlements: toml::Table::new(),
            info_plist: toml::Table::new(),
        }
    }
}

/// `[ios.signing]`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IosSigning {
    /// Development signing.
    #[serde(default)]
    pub development: SigningRef,
    /// Distribution signing.
    #[serde(default)]
    pub distribution: SigningRef,
}

/// An identity and a profile: `"auto"`, a SHA-1, a name, a UUID or a path.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SigningRef {
    /// The signing identity.
    #[serde(default = "auto")]
    pub identity: String,
    /// The provisioning profile.
    #[serde(default = "auto")]
    pub profile: String,
}

impl Default for SigningRef {
    fn default() -> Self {
        SigningRef {
            identity: auto(),
            profile: auto(),
        }
    }
}

/// `[ios.privacy]`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IosPrivacy {
    /// NSPrivacyTracking.
    #[serde(default)]
    pub tracking: bool,
    /// NSPrivacyTrackingDomains.
    #[serde(default)]
    pub tracking_domains: Vec<String>,
    /// NSPrivacyCollectedDataTypes entries.
    #[serde(default)]
    pub collected_data: Vec<toml::Value>,
    /// Required-reason API categories → reason codes.
    #[serde(default)]
    pub api_reasons: BTreeMap<String, Vec<String>>,
}

/// `[android]`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AndroidConfig {
    /// minSdk.
    #[serde(default = "default_min_sdk")]
    pub min_sdk: u32,
    /// targetSdk.
    #[serde(default = "default_target_sdk")]
    pub target_sdk: u32,
    /// Release ABIs; dev builds only the device's.
    #[serde(default = "default_abis")]
    pub abis: Vec<Abi>,
    /// `native` (NativeActivity); `game` is reserved.
    #[serde(default = "default_activity")]
    pub activity: String,
    /// `system`: Android handles Back (predictive back) and finishes the
    /// activity at the app's root. `key`: Back reaches the app as a key
    /// (`enableOnBackInvokedCallback="false"`), for an app that handles it.
    #[serde(default = "default_back")]
    pub back: String,
    /// android:allowBackup.
    #[serde(default = "default_true")]
    pub allow_backup: bool,
    /// User resources layered over the generated ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub res: Option<String>,
    /// Extra `<uses-permission>` names.
    #[serde(default)]
    pub extra_permissions: Vec<String>,
    /// Package override for Android.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Library override for Android.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lib: Option<String>,
    /// `[android.manifest]`.
    #[serde(default)]
    pub manifest: AndroidManifestOverlay,
    /// `[android.signing]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing: Option<AndroidSigning>,
    /// `[android.play]`.
    #[serde(default)]
    pub play: AndroidPlay,
}

impl Default for AndroidConfig {
    fn default() -> Self {
        AndroidConfig {
            min_sdk: default_min_sdk(),
            target_sdk: default_target_sdk(),
            abis: default_abis(),
            activity: default_activity(),
            back: default_back(),
            allow_backup: true,
            res: None,
            extra_permissions: Vec::new(),
            package: None,
            lib: None,
            manifest: AndroidManifestOverlay::default(),
            signing: None,
            play: AndroidPlay::default(),
        }
    }
}

/// `[android.manifest]`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AndroidManifestOverlay {
    /// Extra `<application>` attributes.
    #[serde(default)]
    pub application: toml::Table,
    /// Extra `<activity>` attributes.
    #[serde(default)]
    pub activity: toml::Table,
    /// Raw XML inside `<manifest>`.
    #[serde(default)]
    pub extra_manifest_xml: String,
    /// Raw XML inside `<application>`.
    #[serde(default)]
    pub extra_application_xml: String,
}

/// `[android.signing]`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AndroidSigning {
    /// The upload key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload: Option<KeystoreRef>,
}

/// A keystore reference; passwords only by env-var name.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeystoreRef {
    /// The keystore path.
    pub keystore: String,
    /// The key alias.
    pub alias: String,
    /// The env var holding the store password.
    pub store_pass_env: String,
    /// The env var holding the key password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_pass_env: Option<String>,
}

/// `[android.play]`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AndroidPlay {
    /// The Play track for printed commands.
    #[serde(default = "default_track")]
    pub track: String,
    /// The env var holding the service-account JSON (printed commands only).
    #[serde(default = "default_service_account_env")]
    pub service_account_json_env: String,
}

impl Default for AndroidPlay {
    fn default() -> Self {
        AndroidPlay {
            track: default_track(),
            service_account_json_env: default_service_account_env(),
        }
    }
}

/// A static host for `[web] host`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WebHost {
    /// Any static host.
    Generic,
    /// Cloudflare Pages.
    CloudflarePages,
    /// Netlify.
    Netlify,
    /// GitHub Pages.
    GithubPages,
    /// Amazon S3.
    S3,
}

/// `[web]`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    /// The URL path the site is served under.
    #[serde(default = "default_public_url")]
    pub public_url: String,
    /// The host, for printed commands.
    #[serde(default = "default_web_host")]
    pub host: WebHost,
    /// The host project or bucket.
    #[serde(default)]
    pub project: String,
    /// The gzip size budget of the .wasm.
    #[serde(default = "default_size_budget")]
    pub size_budget_kb: u64,
    /// Extra rustflags for wasm32.
    #[serde(default)]
    pub rustflags: Vec<String>,
    /// Package override for the web.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Binary override for the web.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin: Option<String>,
}

impl Default for WebConfig {
    fn default() -> Self {
        WebConfig {
            public_url: default_public_url(),
            host: default_web_host(),
            project: String::new(),
            size_budget_kb: default_size_budget(),
            rustflags: Vec::new(),
            package: None,
            bin: None,
        }
    }
}

/// `[desktop]`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopConfig {
    /// Package override for desktop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Binary override for desktop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin: Option<String>,
    /// `[desktop.macos]`.
    #[serde(default)]
    pub macos: MacosConfig,
    /// `[desktop.windows]`.
    #[serde(default)]
    pub windows: WindowsConfig,
    /// `[desktop.linux]`.
    #[serde(default)]
    pub linux: LinuxConfig,
}

/// `[desktop.macos]`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MacosConfig {
    /// MACOSX_DEPLOYMENT_TARGET and LSMinimumSystemVersion.
    #[serde(default = "default_macos_min_os")]
    pub min_os: String,
    /// Build a universal binary.
    #[serde(default)]
    pub universal: bool,
    /// The Developer ID Application identity.
    #[serde(default = "auto")]
    pub identity: String,
    /// The notarytool keychain profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notary_profile: Option<String>,
}

impl Default for MacosConfig {
    fn default() -> Self {
        MacosConfig {
            min_os: default_macos_min_os(),
            universal: false,
            identity: auto(),
            notary_profile: None,
        }
    }
}

/// `[desktop.windows]`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowsConfig {
    /// Installer formats.
    #[serde(default = "default_windows_formats")]
    pub formats: Vec<String>,
    /// The signing command (credentials only via env).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sign_command: Option<String>,
}

impl Default for WindowsConfig {
    fn default() -> Self {
        WindowsConfig {
            formats: default_windows_formats(),
            sign_command: None,
        }
    }
}

/// `[desktop.linux]`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxConfig {
    /// Package formats.
    #[serde(default = "default_linux_formats")]
    pub formats: Vec<String>,
    /// The oldest glibc the binary may need.
    #[serde(default = "default_glibc_floor")]
    pub glibc_floor: String,
    /// The .deb maintainer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maintainer: Option<String>,
    /// The .deb package name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deb_package: Option<String>,
    /// .deb Depends.
    #[serde(default)]
    pub deb_depends: Vec<String>,
    /// .deb Recommends.
    #[serde(default)]
    pub deb_recommends: Vec<String>,
}

impl Default for LinuxConfig {
    fn default() -> Self {
        LinuxConfig {
            formats: default_linux_formats(),
            glibc_floor: default_glibc_floor(),
            maintainer: None,
            deb_package: None,
            deb_depends: Vec::new(),
            deb_recommends: Vec::new(),
        }
    }
}

/// `[test]`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestConfig {
    /// The directory of `.ice` flows.
    #[serde(default = "default_flows")]
    pub flows: String,
    /// Viewport presets (or `WxH[@scale]`) for headless shots.
    #[serde(default = "default_viewports")]
    pub viewports: Vec<String>,
}

impl Default for TestConfig {
    fn default() -> Self {
        TestConfig {
            flows: default_flows(),
            viewports: default_viewports(),
        }
    }
}

/// `[review]`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewConfig {
    /// Keep review snapshots of generated files (later phase).
    #[serde(default)]
    pub snapshot: bool,
}

fn auto() -> String {
    "auto".to_string()
}
fn default_true() -> bool {
    true
}
fn default_build() -> u64 {
    1
}
fn default_platforms() -> Vec<AppPlatform> {
    vec![
        AppPlatform::Desktop,
        AppPlatform::Web,
        AppPlatform::Ios,
        AppPlatform::Android,
    ]
}
fn default_background() -> String {
    "#FFFFFF".to_string()
}
fn default_orientations() -> Vec<Orientation> {
    vec![Orientation::Portrait]
}
fn default_ios_min_os() -> String {
    "16.0".to_string()
}
fn default_ios_devices() -> Vec<String> {
    vec!["iphone".to_string()]
}
fn default_min_sdk() -> u32 {
    26
}
fn default_target_sdk() -> u32 {
    36
}
fn default_abis() -> Vec<Abi> {
    vec![Abi::Arm64V8a, Abi::X86_64]
}
fn default_activity() -> String {
    "native".to_string()
}
fn default_back() -> String {
    "system".to_string()
}
fn default_track() -> String {
    "internal".to_string()
}
fn default_service_account_env() -> String {
    "PLAY_SERVICE_ACCOUNT_JSON".to_string()
}
fn default_public_url() -> String {
    "/".to_string()
}
fn default_web_host() -> WebHost {
    WebHost::Generic
}
fn default_size_budget() -> u64 {
    4096
}
fn default_macos_min_os() -> String {
    "12.0".to_string()
}
fn default_windows_formats() -> Vec<String> {
    vec!["msi".to_string(), "nsis".to_string()]
}
fn default_linux_formats() -> Vec<String> {
    vec!["deb".to_string(), "appimage".to_string()]
}
fn default_glibc_floor() -> String {
    "2.35".to_string()
}
fn default_flows() -> String {
    "tests/flows".to_string()
}
fn default_viewports() -> Vec<String> {
    ["iphone-17", "pixel-9", "web-mobile", "desktop"]
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// The dev platforms `[checks]` may name.
pub const DEV_PLATFORMS: &[&str] = &["desktop", "web", "ios-sim", "ios-device", "android"];

/// The viewport presets (design §13.1).
pub const VIEWPORT_PRESETS: &[&str] =
    &["iphone-17", "iphone-se", "pixel-9", "web-mobile", "desktop"];

/// Info.plist keys icm generates, and the icm.toml key that sets each.
pub const MANAGED_PLIST_KEYS: &[(&str, &str)] = &[
    ("CFBundleIdentifier", "[app] id"),
    ("CFBundleExecutable", "[app] bin"),
    ("CFBundleName", "[app] name"),
    ("CFBundleDisplayName", "[app] name"),
    ("CFBundlePackageType", "(generated)"),
    ("CFBundleShortVersionString", "Cargo.toml version"),
    ("CFBundleVersion", "[app] build"),
    ("CFBundleSupportedPlatforms", "(generated)"),
    ("CFBundleInfoDictionaryVersion", "(generated)"),
    ("MinimumOSVersion", "[ios] min_os"),
    ("LSRequiresIPhoneOS", "(generated)"),
    ("UIDeviceFamily", "[ios] devices"),
    ("UIRequiredDeviceCapabilities", "(generated)"),
    ("UISupportedInterfaceOrientations", "[app] orientations"),
    ("UIApplicationSceneManifest", "(generated)"),
    ("UILaunchScreen", "[app] background"),
    ("CFBundleIcons", "[app] icon"),
    ("CFBundleIconName", "[app] icon"),
    (
        "ITSAppUsesNonExemptEncryption",
        "[ios] uses_non_exempt_encryption",
    ),
    (
        "ITSEncryptionExportComplianceCode",
        "[ios] export_compliance_code",
    ),
    ("BuildMachineOSBuild", "(generated)"),
    ("NSCameraUsageDescription", "[app.permissions] camera"),
    (
        "NSMicrophoneUsageDescription",
        "[app.permissions] microphone",
    ),
    ("NSFaceIDUsageDescription", "[app.permissions] face_id"),
    ("NSPhotoLibraryUsageDescription", "[app.permissions] photos"),
    (
        "NSLocationWhenInUseUsageDescription",
        "[app.permissions] location",
    ),
];

/// Manifest attributes icm generates, the icm.toml key that sets each, and
/// the `[android.manifest]` table that may not set it (`None`: neither).
/// An overlay's attributes are written after the generated ones, so one of
/// these would be a duplicate attribute, which aapt2 rejects late in the
/// build.
pub const MANAGED_MANIFEST_ATTRIBUTES: &[(&str, &str, Option<&str>)] = &[
    ("package", "[app] id", None),
    ("android:versionCode", "[app] build", None),
    ("android:versionName", "Cargo.toml version", None),
    ("android:hasCode", "[android] activity", None),
    ("android:extractNativeLibs", "(generated)", None),
    ("android:debuggable", "(generated: dev builds only)", None),
    ("android:name", "(generated: the activity)", None),
    ("android:icon", "[app] icon", None),
    ("android:roundIcon", "[app] icon", None),
    ("android:label", "[app] name", None),
    ("android:allowBackup", "[android] allow_backup", None),
    ("android:screenOrientation", "[app] orientations", None),
    (
        "android:enableOnBackInvokedCallback",
        "[android] back",
        None,
    ),
    (
        "android:theme",
        "[app] background, or a style named IcmTheme in platform/android/res,",
        Some("application"),
    ),
    (
        "android:exported",
        "(generated: the launcher starts the activity)",
        Some("activity"),
    ),
    (
        "android:launchMode",
        "(generated: singleTask, so a second activity never runs beside the first)",
        Some("activity"),
    ),
    (
        "android:windowSoftInputMode",
        "(generated: adjustResize|stateHidden)",
        Some("activity"),
    ),
    (
        "android:configChanges",
        "(generated: every change [android] target_sdk can name, so the app keeps its state through them; a value cannot be removed)",
        Some("activity"),
    ),
];

/// The end of a `config.managed_key` detail: the icm.toml key to set
/// instead, or, for a value icm alone writes (an owner in parentheses,
/// "(generated: why)"), that an overlay cannot replace it.
fn managed_instead(owner: &str) -> String {
    match owner.strip_prefix("(generated") {
        Some(rest) => {
            let why = rest.trim_end_matches(')').trim_start_matches(':').trim();
            if why.is_empty() {
                "an overlay cannot replace it".to_string()
            } else {
                format!("an overlay cannot replace it ({why})")
            }
        }
        None => format!("set {owner} instead"),
    }
}

/// A loaded, validated icm.toml.
#[derive(Clone, Debug)]
pub struct Loaded {
    /// The file.
    pub path: PathBuf,
    /// Its directory: the project directory.
    pub dir: PathBuf,
    /// The configuration.
    pub config: IcmToml,
    /// Its text and spans.
    pub source: Source,
}

impl Loaded {
    /// `file:line` evidence for a dotted key.
    pub fn evidence(&self, key: &str) -> Evidence {
        self.source.evidence_for(key)
    }

    /// The sha256 of the file, for `inputs.icm_toml_sha256`.
    pub fn sha256(&self) -> String {
        crate::hash::sha256_hex(self.source.text.as_bytes())
    }
}

impl IcmToml {
    /// The minimum icm version, from `min_icm` or the older `icm` key.
    pub fn min_icm(&self) -> Option<&str> {
        self.min_icm.as_deref().or(self.icm.as_deref())
    }

    /// Whether `[app] id` is still a placeholder.
    pub fn id_is_placeholder(&self) -> bool {
        self.app.id.starts_with("com.example.") || self.app.id == "com.example"
    }

    /// Whether the app ships on a platform.
    pub fn ships_on(&self, platform: AppPlatform) -> bool {
        self.app.platforms.contains(&platform)
    }
}

/// Finds icm.toml: `explicit` (a file or a directory), else the nearest
/// one walking up from `start`.
pub fn locate(explicit: Option<&Path>, start: &Path) -> Result<PathBuf, IcmError> {
    if let Some(path) = explicit {
        let path = if path.is_dir() {
            path.join(FILE_NAME)
        } else {
            path.to_path_buf()
        };
        if path.is_file() {
            return Ok(path);
        }
        return Err(IcmError::new(
            CheckId::ConfigNotFound,
            format!("{} does not exist", crate::paths::display(&path)),
        )
        .fix(
            "Pass the path of an existing icm.toml (or its directory) to --config.",
            &[],
        ));
    }

    let mut dir = Some(start);
    while let Some(current) = dir {
        let candidate = current.join(FILE_NAME);
        if candidate.is_file() {
            return Ok(candidate);
        }
        dir = current.parent();
    }

    Err(IcmError::new(
        CheckId::ConfigNotFound,
        format!(
            "no icm.toml in {} or any parent directory",
            crate::paths::display(start)
        ),
    )
    .fix_commands(["icm new <dir>"]))
}

/// Reads, parses and validates an icm.toml. On failure every problem found
/// is returned, the first one first.
pub fn load(path: &Path) -> Result<Loaded, Vec<IcmError>> {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let text = std::fs::read_to_string(&path).map_err(|error| {
        vec![IcmError::new(
            CheckId::ConfigNotFound,
            format!("cannot read {}: {error}", crate::paths::display(&path)),
        )]
    })?;

    let source = Source::new(&path, text);
    let config: IcmToml =
        toml::from_str(&source.text).map_err(|error| vec![toml_error(&source, &error)])?;

    let loaded = Loaded {
        dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
        path,
        config,
        source,
    };

    let problems = validate(&loaded);
    if problems.is_empty() {
        Ok(loaded)
    } else {
        Err(problems)
    }
}

/// Parses an icm.toml from text (tests and tools that already read it).
pub fn parse(path: &Path, text: &str) -> Result<Loaded, Vec<IcmError>> {
    let source = Source::new(path, text.to_string());
    let config: IcmToml =
        toml::from_str(&source.text).map_err(|error| vec![toml_error(&source, &error)])?;
    let loaded = Loaded {
        dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
        path: path.to_path_buf(),
        config,
        source,
    };
    let problems = validate(&loaded);
    if problems.is_empty() {
        Ok(loaded)
    } else {
        Err(problems)
    }
}

/// Maps a TOML parse or type error to `config.unknown_key` or
/// `config.invalid`, at its `file:line:col`.
pub fn toml_error(source: &Source, error: &toml::de::Error) -> IcmError {
    let message = error.message().trim().to_string();
    let id = if message.starts_with("unknown field") {
        CheckId::ConfigUnknownKey
    } else {
        CheckId::ConfigInvalid
    };

    let (location, evidence) = match error.span() {
        Some(span) => (source.location(&span), source.evidence(&span)),
        None => (
            crate::paths::display(&source.path),
            Evidence::file(&source.path),
        ),
    };

    let mut detail = format!("{location}: {message}");
    if id == CheckId::ConfigUnknownKey
        && let Some(span) = error.span()
    {
        let table = enclosing_table(source, span.start);
        if !table.is_empty() {
            detail.push_str(&format!(" (in [{table}])"));
        }
    }

    IcmError::new(id, detail).evidence(evidence)
}

/// The `[table]` header governing an offset.
fn enclosing_table(source: &Source, offset: usize) -> String {
    let before = source.text.get(..offset).unwrap_or("");
    for line in before.lines().rev() {
        let line = line.trim();
        if line.starts_with('[') {
            return line
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim_matches(['[', ']'])
                .trim()
                .to_string();
        }
    }
    String::new()
}

fn invalid(loaded: &Loaded, key: &str, message: impl AsRef<str>) -> IcmError {
    let evidence = loaded.evidence(key);
    let location = match evidence.line {
        Some(line) => format!("{}:{line}", evidence.path),
        None => evidence.path.clone(),
    };
    IcmError::new(
        CheckId::ConfigInvalid,
        format!("{location}: `{key}` {}", message.as_ref()),
    )
    .evidence(evidence)
}

/// Semantic validation. Returns every problem.
pub fn validate(loaded: &Loaded) -> Vec<IcmError> {
    let config = &loaded.config;
    let mut problems = Vec::new();

    // schema
    if config.schema > SCHEMA {
        let evidence = loaded.evidence("schema");
        problems.push(
            IcmError::new(
                CheckId::ConfigTooNew,
                format!(
                    "{}: schema {} is newer than this icm reads (schema {SCHEMA})",
                    loaded.source.location_for("schema"),
                    config.schema
                ),
            )
            .evidence(evidence)
            .fix_commands([crate::version::install_command(None)]),
        );
    } else if config.schema != SCHEMA {
        problems.push(invalid(loaded, "schema", format!("must be {SCHEMA}")));
    }

    // min_icm / icm
    if config.min_icm.is_some() && config.icm.is_some() {
        problems.push(invalid(
            loaded,
            "icm",
            "duplicates `min_icm`; keep only `min_icm = \"<version>\"`",
        ));
    }
    if let Some(raw) = config.min_icm() {
        let key = if config.min_icm.is_some() {
            "min_icm"
        } else {
            "icm"
        };
        match crate::version::parse_min(raw) {
            Ok(min) => {
                let current = crate::buildinfo::version();
                if !crate::version::meets(&current, &min) {
                    problems.push(
                        IcmError::new(
                            CheckId::ConfigTooNew,
                            format!(
                                "{}: this project needs icm {min} or newer; this is icm {current}",
                                loaded.source.location_for(key)
                            ),
                        )
                        .evidence(loaded.evidence(key))
                        .fix_commands([crate::version::install_command(Some(&min))]),
                    );
                }
            }
            Err(message) => problems.push(invalid(loaded, key, message)),
        }
    }

    // [app]
    let app = &config.app;
    if app.name.trim().is_empty() {
        problems.push(invalid(loaded, "app.name", "must not be empty"));
    } else if app.name.chars().any(char::is_control) {
        problems.push(invalid(
            loaded,
            "app.name",
            "must not contain control characters",
        ));
    }

    if let Err(message) = validate_app_id(&app.id) {
        let evidence = loaded.evidence("app.id");
        problems.push(
            IcmError::new(
                CheckId::ConfigIdInvalid,
                format!(
                    "{}: `app.id` {message}",
                    loaded.source.location_for("app.id")
                ),
            )
            .evidence(evidence),
        );
    }

    if app.build == 0 || app.build > 2_100_000_000 {
        problems.push(invalid(
            loaded,
            "app.build",
            "must be between 1 and 2100000000 (Android versionCode limit)",
        ));
    }

    if !is_hex_colour(&app.background) {
        problems.push(invalid(
            loaded,
            "app.background",
            "must be a colour like \"#FFFFFF\"",
        ));
    }

    if app.platforms.is_empty() {
        problems.push(invalid(
            loaded,
            "app.platforms",
            "must name at least one platform",
        ));
    }
    if has_duplicates(&app.platforms) {
        problems.push(invalid(loaded, "app.platforms", "lists a platform twice"));
    }
    if app.orientations.is_empty() {
        problems.push(invalid(
            loaded,
            "app.orientations",
            "must name at least one orientation",
        ));
    }

    for (key, value) in [
        ("app.package", &app.package),
        ("app.lib", &app.lib),
        ("app.bin", &app.bin),
        ("ios.package", &config.ios.package),
        ("ios.bin", &config.ios.bin),
        ("android.package", &config.android.package),
        ("android.lib", &config.android.lib),
        ("web.package", &config.web.package),
        ("web.bin", &config.web.bin),
        ("desktop.package", &config.desktop.package),
        ("desktop.bin", &config.desktop.bin),
    ] {
        if value.as_deref().is_some_and(|v| v.trim().is_empty()) {
            problems.push(invalid(
                loaded,
                key,
                "must not be empty (remove it for the default)",
            ));
        }
    }

    // [ios]
    let store_floor = crate::policy::get()
        .text("app_store.min_deployment")
        .unwrap_or("13.0");
    match parse_os_version(&config.ios.min_os) {
        Some(version) if parse_os_version(store_floor).is_some_and(|floor| version < floor) => {
            problems.push(invalid(
                loaded,
                "ios.min_os",
                format!(
                    "is below iOS {store_floor}, the App Store's minimum deployment target (`icm print policy`)"
                ),
            ))
        }
        Some(_) => {}
        None => problems.push(invalid(loaded, "ios.min_os", "must look like \"16.0\"")),
    }
    if config.ios.devices != ["iphone"] {
        problems.push(invalid(
            loaded,
            "ios.devices",
            "must be [\"iphone\"] in schema 1 (iPad is not supported yet)",
        ));
    }
    for key in config.ios.info_plist.keys() {
        let managed = MANAGED_PLIST_KEYS
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, owner)| *owner)
            .or_else(|| {
                key.starts_with("DT")
                    .then_some("(generated from the Xcode in use)")
            });
        if let Some(owner) = managed {
            let path = format!("ios.info_plist.{key}");
            problems.push(
                IcmError::new(
                    CheckId::ConfigManagedKey,
                    format!(
                        "{}: Info.plist key `{key}` is generated by icm; {}",
                        loaded.source.location_for(&path),
                        managed_instead(owner)
                    ),
                )
                .evidence(loaded.evidence(&path)),
            );
        }
    }

    // [android]
    let android = &config.android;
    if android.min_sdk < 21 {
        problems.push(invalid(loaded, "android.min_sdk", "must be at least 21"));
    }
    if android.min_sdk > android.target_sdk {
        problems.push(invalid(
            loaded,
            "android.min_sdk",
            format!(
                "({}) is above `android.target_sdk` ({})",
                android.min_sdk, android.target_sdk
            ),
        ));
    }
    if android.abis.is_empty() {
        problems.push(invalid(
            loaded,
            "android.abis",
            "must name at least one ABI",
        ));
    }
    if has_duplicates(&android.abis) {
        problems.push(invalid(loaded, "android.abis", "lists an ABI twice"));
    }
    if !["native", "game"].contains(&android.activity.as_str()) {
        problems.push(invalid(
            loaded,
            "android.activity",
            "must be \"native\" (\"game\" is reserved)",
        ));
    }
    if !["key", "system"].contains(&android.back.as_str()) {
        problems.push(invalid(
            loaded,
            "android.back",
            "must be \"key\" or \"system\"",
        ));
    }
    for (table, attributes) in [
        ("application", &android.manifest.application),
        ("activity", &android.manifest.activity),
    ] {
        for key in attributes.keys() {
            if let Some((_, owner, _)) = MANAGED_MANIFEST_ATTRIBUTES
                .iter()
                .find(|(name, _, only)| name == key && only.is_none_or(|only| only == table))
            {
                let path = format!("android.manifest.{table}.{key}");
                problems.push(
                    IcmError::new(
                        CheckId::ConfigManagedKey,
                        format!(
                            "{}: manifest attribute `{key}` is generated by icm; {}",
                            loaded.source.location_for(&path),
                            managed_instead(owner)
                        ),
                    )
                    .evidence(loaded.evidence(&path)),
                );
            }
        }
    }

    // [web]
    if !(config.web.public_url.starts_with('/') || config.web.public_url.starts_with("https://")) {
        problems.push(invalid(
            loaded,
            "web.public_url",
            "must start with \"/\" or \"https://\"",
        ));
    }

    // [desktop]
    if parse_os_version(&config.desktop.macos.min_os).is_none() {
        problems.push(invalid(
            loaded,
            "desktop.macos.min_os",
            "must look like \"12.0\"",
        ));
    }

    // [test]
    for (index, viewport) in config.test.viewports.iter().enumerate() {
        if !VIEWPORT_PRESETS.contains(&viewport.as_str()) && parse_viewport(viewport).is_none() {
            problems.push(invalid(
                loaded,
                &format!("test.viewports[{index}]"),
                format!(
                    "`{viewport}` is neither a preset ({}) nor WxH[@scale]",
                    VIEWPORT_PRESETS.join(", ")
                ),
            ));
        }
    }

    // [checks]
    for (platform, scripts) in &config.checks {
        let path = format!("checks.{platform}");
        if !DEV_PLATFORMS.contains(&platform.as_str()) {
            let span = loaded.source.key_span(&path);
            let evidence = span
                .as_ref()
                .map(|s| loaded.source.evidence(s))
                .unwrap_or_else(|| loaded.evidence("checks"));
            problems.push(
                IcmError::new(
                    CheckId::ConfigUnknownKey,
                    format!(
                        "{}: unknown platform `{platform}` in [checks]; expected one of {}",
                        loaded.source.location_for(&path),
                        DEV_PLATFORMS.join(", ")
                    ),
                )
                .evidence(evidence),
            );
        }
        if scripts.iter().any(|script| script.trim().is_empty()) {
            problems.push(invalid(loaded, &path, "has an empty script path"));
        }
    }

    problems
}

/// Checks a reverse-DNS app id: at least two segments, each starting with a
/// letter and holding only letters, digits and underscores (Android's rule;
/// stricter than iOS's).
pub fn validate_app_id(id: &str) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err("must not be empty".to_string());
    }
    let segments: Vec<&str> = id.split('.').collect();
    if segments.len() < 2 {
        return Err(format!(
            "`{id}` needs at least two dot-separated segments, e.g. com.example.notes"
        ));
    }
    for segment in segments {
        let Some(first) = segment.chars().next() else {
            return Err(format!("`{id}` has an empty segment"));
        };
        if !first.is_ascii_alphabetic() {
            return Err(format!(
                "segment `{segment}` of `{id}` must start with a letter"
            ));
        }
        if !segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(format!(
                "segment `{segment}` of `{id}` may hold only letters, digits and underscores"
            ));
        }
    }
    if id.len() > 155 {
        return Err("is longer than 155 characters".to_string());
    }
    Ok(())
}

fn is_hex_colour(value: &str) -> bool {
    value.len() == 7 && value.starts_with('#') && value[1..].chars().all(|c| c.is_ascii_hexdigit())
}

fn has_duplicates<T: PartialEq>(items: &[T]) -> bool {
    items
        .iter()
        .enumerate()
        .any(|(i, item)| items[..i].contains(item))
}

/// Parses `16`, `16.0` or `16.0.1` into (major, minor).
pub fn parse_os_version(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = match parts.next() {
        Some(minor) => minor.parse().ok()?,
        None => 0,
    };
    if let Some(patch) = parts.next() {
        let _: u32 = patch.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor))
}

/// Parses `WxH` or `WxH@scale`.
pub fn parse_viewport(viewport: &str) -> Option<(u32, u32, f32)> {
    let (size, scale) = match viewport.split_once('@') {
        Some((size, scale)) => (size, scale.parse().ok()?),
        None => (viewport, 1.0),
    };
    let (width, height) = size.split_once('x')?;
    let width = width.parse().ok()?;
    let height = height.parse().ok()?;
    (width > 0 && height > 0 && scale > 0.0).then_some((width, height, scale))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The template's file (design §7.2), with the corrected `min_icm`.
    pub const TEMPLATE: &str = r##"#:schema ./.icm/icm.schema.json
schema = 1
min_icm = "0.14.1-mobile.1"

[app]
name = "App"
id = "com.example.app"
build = 1
platforms = ["desktop", "web", "ios", "android"]
package = "app"
lib = "app"
bin = "app"
icon = "assets/icon.png"
background = "#FFFFFF"
orientations = ["portrait"]
publisher = "Example Ltd"
copyright = "© 2026 Example Ltd"
description = "A starter app."
category = "utilities"
resources = []
agent = true

[app.permissions]
internet = true

[ios]
min_os = "16.0"
devices = ["iphone"]
[ios.signing]
development = { identity = "auto", profile = "auto" }
distribution = { identity = "auto", profile = "auto" }
[ios.privacy]
tracking = false
tracking_domains = []
collected_data = []
api_reasons = { FileTimestamp = ["C617.1"], SystemBootTime = ["35F9.1"] }
[ios.entitlements]
[ios.info_plist]

[android]
min_sdk = 26
target_sdk = 36
abis = ["arm64-v8a", "x86_64"]
activity = "native"
back = "system"
allow_backup = true
res = "platform/android/res"
extra_permissions = []
[android.manifest]
application = {}
activity = {}
extra_manifest_xml = ""
extra_application_xml = ""
[android.play]
track = "internal"
service_account_json_env = "PLAY_SERVICE_ACCOUNT_JSON"

[web]
public_url = "/"
host = "generic"
project = ""
size_budget_kb = 4096
rustflags = []

[desktop.macos]
min_os = "12.0"
universal = false
identity = "auto"
[desktop.windows]
formats = ["msi", "nsis"]
[desktop.linux]
formats = ["deb", "appimage"]
glibc_floor = "2.35"
deb_depends = []
deb_recommends = []

[test]
flows = "tests/flows"
viewports = ["iphone-17", "pixel-9", "web-mobile", "desktop"]

[checks]

[review]
snapshot = false
"##;

    fn parse_text(text: &str) -> Result<Loaded, Vec<IcmError>> {
        parse(Path::new("/proj/icm.toml"), text)
    }

    fn first_error(text: &str) -> IcmError {
        parse_text(text).expect_err("should fail").remove(0)
    }

    const MINIMAL: &str = "schema = 1\n[app]\nname = \"Notes\"\nid = \"com.acme.notes\"\n";

    #[test]
    fn the_template_parses() {
        let loaded = parse_text(TEMPLATE).unwrap_or_else(|errors| panic!("{errors:?}"));
        let config = &loaded.config;
        assert_eq!(config.app.name, "App");
        assert!(config.id_is_placeholder());
        assert_eq!(config.android.abis, vec![Abi::Arm64V8a, Abi::X86_64]);
        assert_eq!(
            config.ios.privacy.api_reasons["FileTimestamp"],
            vec!["C617.1"]
        );
        assert_eq!(config.min_icm(), Some("0.14.1-mobile.1"));
    }

    #[test]
    fn a_minimal_file_gets_the_defaults() {
        let config = parse_text(MINIMAL).unwrap().config;
        assert_eq!(config.app.build, 1);
        assert_eq!(config.app.platforms.len(), 4);
        assert_eq!(config.ios.min_os, "16.0");
        assert_eq!(config.android.min_sdk, 26);
        assert_eq!(config.android.target_sdk, 36);
        assert!(!config.id_is_placeholder());
        assert!(config.ships_on(AppPlatform::Android));
    }

    #[test]
    fn unknown_keys_point_at_their_line() {
        let error = first_error(&format!("{MINIMAL}colour = \"red\"\n"));
        assert_eq!(error.id, "config.unknown_key");
        assert_eq!(error.evidence[0].line, Some(5));
        assert_eq!(
            error.evidence[0].excerpt.as_deref(),
            Some("colour = \"red\"")
        );
        assert!(
            error
                .detail
                .starts_with("/proj/icm.toml:5:1: unknown field `colour`"),
            "{}",
            error.detail
        );
        assert!(error.detail.contains("(in [app])"), "{}", error.detail);
    }

    #[test]
    fn unknown_tables_are_rejected() {
        let error = first_error(&format!("{MINIMAL}[andriod]\nmin_sdk = 26\n"));
        assert_eq!(error.id, "config.unknown_key");
        assert_eq!(error.evidence[0].line, Some(5));
    }

    #[test]
    fn type_errors_are_invalid() {
        let error =
            first_error("schema = 1\n[app]\nname = \"N\"\nid = \"com.a.b\"\nbuild = \"one\"\n");
        assert_eq!(error.id, "config.invalid");
        assert_eq!(error.exit, crate::exit::Exit::Config);
        assert_eq!(error.evidence[0].line, Some(5));

        let error = first_error(
            "schema = 1\n[app]\nname = \"N\"\nid = \"com.a.b\"\nplatforms = [\"tv\"]\n",
        );
        assert_eq!(error.id, "config.invalid");
        assert!(
            error.detail.contains("unknown variant `tv`"),
            "{}",
            error.detail
        );
    }

    #[test]
    fn missing_required_keys_are_invalid() {
        let error = first_error("schema = 1\n[app]\nname = \"N\"\n");
        assert_eq!(error.id, "config.invalid");
        assert!(
            error.detail.contains("missing field `id`"),
            "{}",
            error.detail
        );
    }

    #[test]
    fn syntax_errors_are_invalid() {
        let error = first_error("schema = 1\n[app\n");
        assert_eq!(error.id, "config.invalid");
        assert_eq!(error.evidence[0].line, Some(2));
    }

    #[test]
    fn bad_ids_are_reported() {
        for id in [
            "notes",
            "com.1acme.notes",
            "com.acme-corp.notes",
            "com..notes",
        ] {
            let error = first_error(&MINIMAL.replace("com.acme.notes", id));
            assert_eq!(error.id, "config.id_invalid", "{id}");
            assert_eq!(error.evidence[0].line, Some(4), "{id}");
        }
    }

    #[test]
    fn every_problem_is_reported() {
        let text = "schema = 1\n[app]\nname = \"N\"\nid = \"x\"\nbuild = 0\nbackground = \"white\"\n[android]\nmin_sdk = 40\ntarget_sdk = 36\n";
        let errors = parse_text(text).unwrap_err();
        let ids: Vec<&str> = errors.iter().map(|e| e.id.as_ref()).collect();
        assert_eq!(
            ids,
            vec![
                "config.id_invalid",
                "config.invalid",
                "config.invalid",
                "config.invalid"
            ]
        );
        assert!(
            errors[3].detail.contains("android.min_sdk"),
            "{}",
            errors[3].detail
        );
        assert_eq!(errors[3].evidence[0].line, Some(8));
    }

    #[test]
    fn min_icm_uses_version_ordering() {
        let current = crate::buildinfo::version();
        let ok = format!("min_icm = \"{current}\"\n{MINIMAL}");
        assert!(parse_text(&ok).is_ok());

        let legacy = format!("icm = \">={current}\"\n{MINIMAL}");
        assert!(parse_text(&legacy).is_ok());

        let newer = format!(
            "min_icm = \"{}.{}.{}-mobile.1\"\n{MINIMAL}",
            current.major,
            current.minor + 1,
            0
        );
        let error = first_error(&newer);
        assert_eq!(error.id, "config.too_new");
        assert_eq!(error.exit, crate::exit::Exit::Environment);
        assert_eq!(error.evidence[0].line, Some(1));
        assert!(error.fix.commands[0].starts_with("cargo install --locked"));

        let bad = first_error(&format!("min_icm = \"^0.14\"\n{MINIMAL}"));
        assert_eq!(bad.id, "config.invalid");
    }

    #[test]
    fn newer_schemas_need_a_newer_icm() {
        let error = first_error(&MINIMAL.replace("schema = 1", "schema = 2"));
        assert_eq!(error.id, "config.too_new");
    }

    #[test]
    fn managed_keys_are_refused() {
        let text = format!(
            "{MINIMAL}[ios.info_plist]\nCFBundleIdentifier = \"x.y\"\nNSBluetoothAlwaysUsageDescription = \"ok\"\nDTXcode = \"1\"\n[android.manifest.application]\n\"android:label\" = \"X\"\n\"android:dataExtractionRules\" = \"@xml/rules\"\n"
        );
        let errors = parse_text(&text).unwrap_err();
        let details: Vec<&str> = errors.iter().map(|e| e.detail.as_str()).collect();
        assert_eq!(errors.len(), 3, "{details:?}");
        assert!(errors.iter().all(|e| e.id == "config.managed_key"));
        assert!(details[0].contains("CFBundleIdentifier") && details[0].contains("[app] id"));
        assert_eq!(errors[0].evidence[0].line, Some(6));
        assert!(details[2].contains("android:label"));
    }

    #[test]
    fn generated_activity_attributes_are_refused() {
        // Each of these is in the generated manifest, so an overlay would
        // be a duplicate attribute that only aapt2 reports.
        let text = format!(
            "{MINIMAL}[android.manifest.activity]\n\"android:configChanges\" = \"keyboardHidden\"\n\"android:launchMode\" = \"standard\"\n\"android:exported\" = true\n\"android:windowSoftInputMode\" = \"adjustPan\"\n\"android:enableOnBackInvokedCallback\" = false\n\"android:theme\" = \"@style/Mine\"\n[android.manifest.application]\n\"android:theme\" = \"@style/Mine\"\n"
        );
        let errors = parse_text(&text).unwrap_err();
        assert!(errors.iter().all(|e| e.id == "config.managed_key"));
        let details: Vec<&str> = errors.iter().map(|e| e.detail.as_str()).collect();
        let about = |key: &str| -> &str {
            details
                .iter()
                .find(|detail| detail.contains(key))
                .copied()
                .unwrap_or_else(|| panic!("nothing about {key} in {details:?}"))
        };
        // The activity's own theme is not generated; the application's is.
        assert_eq!(errors.len(), 6, "{details:?}");
        assert!(about("android:configChanges").contains("a value cannot be removed"));
        assert!(
            about("android:configChanges")
                .contains("is generated by icm; an overlay cannot replace it (every change"),
            "{details:?}"
        );
        assert!(about("android:launchMode").contains("(singleTask, so"));
        assert!(about("android:exported").contains("an overlay cannot replace it"));
        assert!(about("android:windowSoftInputMode").contains("adjustResize"));
        assert!(
            about("android:enableOnBackInvokedCallback").contains("set [android] back instead")
        );
        // Line 11 is the activity's theme, line 13 the application's.
        assert!(
            about("icm.toml:13: manifest attribute `android:theme`")
                .contains("set [app] background")
        );
        assert!(!details.iter().any(|detail| detail.contains("icm.toml:11:")));
        let config_changes = errors
            .iter()
            .find(|error| error.detail.contains("android:configChanges"))
            .unwrap();
        assert_eq!(config_changes.evidence[0].line, Some(6));
    }

    #[test]
    fn checks_name_dev_platforms() {
        let error = first_error(&format!("{MINIMAL}[checks]\nios = [\"x.sh\"]\n"));
        assert_eq!(error.id, "config.unknown_key");
        assert_eq!(error.evidence[0].line, Some(6));
        assert!(parse_text(&format!("{MINIMAL}[checks]\nios-sim = [\"x.sh\"]\n")).is_ok());
    }

    #[test]
    fn viewports_are_presets_or_sizes() {
        assert!(
            parse_text(&format!(
                "{MINIMAL}[test]\nviewports = [\"402x874@3\", \"desktop\"]\n"
            ))
            .is_ok()
        );
        let error = first_error(&format!("{MINIMAL}[test]\nviewports = [\"huge\"]\n"));
        assert_eq!(error.id, "config.invalid");
        assert_eq!(error.evidence[0].line, Some(6));
    }

    #[test]
    fn locate_walks_up() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.path().join("icm.toml"), MINIMAL).unwrap();
        assert_eq!(locate(None, &nested).unwrap(), dir.path().join("icm.toml"));
        assert_eq!(
            locate(Some(dir.path()), &nested).unwrap(),
            dir.path().join("icm.toml")
        );

        let empty = tempfile::tempdir().unwrap();
        let error = locate(None, empty.path()).unwrap_err();
        assert_eq!(error.id, "config.not_found");
        let error = locate(Some(&empty.path().join("nope.toml")), empty.path()).unwrap_err();
        assert_eq!(error.id, "config.not_found");
    }

    #[test]
    fn os_versions_and_viewports_parse() {
        assert_eq!(parse_os_version("16.0"), Some((16, 0)));
        assert_eq!(parse_os_version("17"), Some((17, 0)));
        assert_eq!(parse_os_version("12.3.1"), Some((12, 3)));
        assert_eq!(parse_os_version("16.x"), None);
        assert_eq!(parse_viewport("402x874@3"), Some((402, 874, 3.0)));
        assert_eq!(parse_viewport("1024x768"), Some((1024, 768, 1.0)));
        assert_eq!(parse_viewport("0x5"), None);
    }
}
