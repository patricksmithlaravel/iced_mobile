//! `icm.toml` (design §7): types, discovery, loading with `file:line`
//! findings, and validation.
//!
//! Unknown keys are rejected (`config.unknown_key`), type errors are
//! `config.invalid`, both with the offending line. The minimum icm version
//! is compared with semver *ordering* against a plain version (Appendix C
//! item 5): write `min_icm = "0.14.1-mobile.1"`. The older form
//! `icm = ">=0.14.1-mobile.1"` is still read, as the same minimum.
//!
//! `schema` and the minimum are read first, leniently (Appendix D item 1): a
//! file for a newer icm is `config.too_new` (exit 4) even when it holds keys
//! this icm does not know, and the strict parse never runs on it.

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
    /// `[store]`: listing metadata and upload credentials, by reference.
    #[serde(default)]
    pub store: StoreConfig,
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
    /// The environment variables `sign_command` reads its credentials from
    /// (names only); a release checks they are set.
    #[serde(default)]
    pub sign_env: Vec<String>,
}

impl Default for WindowsConfig {
    fn default() -> Self {
        WindowsConfig {
            formats: default_windows_formats(),
            sign_command: None,
            sign_env: Vec::new(),
        }
    }
}

/// `[store]`: what the store listings and the printed upload commands need
/// that no binary carries. URLs are public; credentials appear only as the
/// names of the environment variables that hold them.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    /// The privacy policy URL (App Store Connect and Google Play require one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy_policy_url: Option<String>,
    /// The support URL (App Store Connect requires one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_url: Option<String>,
    /// The marketing URL (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marketing_url: Option<String>,
    /// The variable holding the App Store Connect API key id (altool,
    /// notarytool), for the printed commands.
    #[serde(default = "default_asc_key_id_env")]
    pub asc_key_id_env: String,
    /// The variable holding the App Store Connect API issuer id.
    #[serde(default = "default_asc_issuer_id_env")]
    pub asc_issuer_id_env: String,
}

impl Default for StoreConfig {
    fn default() -> Self {
        StoreConfig {
            privacy_policy_url: None,
            support_url: None,
            marketing_url: None,
            asc_key_id_env: default_asc_key_id_env(),
            asc_issuer_id_env: default_asc_issuer_id_env(),
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
fn default_asc_key_id_env() -> String {
    "ASC_KEY_ID".to_string()
}
fn default_asc_issuer_id_env() -> String {
    "ASC_ISSUER_ID".to_string()
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

    read(path, text)
}

/// Parses an icm.toml from text (tests and tools that already read it).
pub fn parse(path: &Path, text: &str) -> Result<Loaded, Vec<IcmError>> {
    read(path.to_path_buf(), text.to_string())
}

/// Checks that this icm may read the file, then parses it strictly and
/// validates it.
fn read(path: PathBuf, text: String) -> Result<Loaded, Vec<IcmError>> {
    let source = Source::new(&path, text);

    // A file for a newer icm may hold keys this one does not know: stop
    // before the strict parse would report the first of them.
    let compatibility = compatibility(&source);
    if !compatibility.too_new.is_empty() {
        return Err(compatibility.too_new);
    }
    let min_icm = compatibility.min_icm.as_ref();

    let config: IcmToml = toml::from_str(&source.text)
        .map_err(|error| vec![newer_key_fix(toml_error(&source, &error), min_icm)])?;

    let loaded = Loaded {
        dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
        path,
        config,
        source,
    };

    let problems: Vec<IcmError> = validate(&loaded)
        .into_iter()
        .map(|problem| newer_key_fix(problem, min_icm))
        .collect();
    if problems.is_empty() {
        Ok(loaded)
    } else {
        Err(problems)
    }
}

/// The keys that say whether this icm may read a file. Every other key is
/// ignored, and a value of another type is left to the strict parse.
#[derive(Debug, Deserialize)]
struct VersionKeys {
    #[serde(default)]
    schema: Option<toml::Value>,
    #[serde(default)]
    min_icm: Option<toml::Value>,
    #[serde(default)]
    icm: Option<toml::Value>,
}

/// What a file's version keys say about this icm.
#[derive(Debug, Default)]
struct Compatibility {
    /// The minimum icm the file names, when it is a version.
    min_icm: Option<semver::Version>,
    /// `config.too_new`: the file needs a newer icm.
    too_new: Vec<IcmError>,
}

/// Reads `schema` and `min_icm` (or the older `icm`) before the strict
/// parse. A syntax error leaves the file to the strict parse, which
/// reports it.
fn compatibility(source: &Source) -> Compatibility {
    let Ok(keys) = toml::from_str::<VersionKeys>(&source.text) else {
        return Compatibility::default();
    };
    let mut too_new = Vec::new();

    if let Some(schema) = keys.schema.as_ref().and_then(toml::Value::as_integer)
        && schema > i64::from(SCHEMA)
    {
        too_new.push(
            IcmError::new(
                CheckId::ConfigTooNew,
                format!(
                    "{}: schema {schema} is newer than this icm reads (schema {SCHEMA})",
                    source.location_for("schema"),
                ),
            )
            .evidence(source.evidence_for("schema"))
            .fix_commands([crate::version::install_command(None)]),
        );
    }

    // `min_icm` wins over `icm`, as in `IcmToml::min_icm`.
    let named = match (&keys.min_icm, &keys.icm) {
        (Some(raw), _) => Some(("min_icm", raw)),
        (None, raw) => raw.as_ref().map(|raw| ("icm", raw)),
    };
    let min = named.and_then(|(key, raw)| {
        let min = crate::version::parse_min(raw.as_str()?).ok()?;
        Some((key, min))
    });
    let current = crate::buildinfo::version();
    if let Some((key, min)) = &min
        && !crate::version::meets(&current, min)
    {
        too_new.push(
            IcmError::new(
                CheckId::ConfigTooNew,
                format!(
                    "{}: this project needs icm {min} or newer; this is icm {current}",
                    source.location_for(key)
                ),
            )
            .evidence(source.evidence_for(key))
            .fix_commands([crate::version::install_command(Some(min))]),
        );
    }

    Compatibility {
        min_icm: min.map(|(_, min)| min),
        too_new,
    }
}

/// A key this icm does not know may be one a newer icm added, and the file
/// did not say it needs that icm: its `min_icm` is absent (or not a
/// version), or this icm meets it. The fix of a `config.unknown_key` says
/// so.
fn newer_key_fix(mut problem: IcmError, min_icm: Option<&semver::Version>) -> IcmError {
    if problem.check_id() != Some(CheckId::ConfigUnknownKey) {
        return problem;
    }
    let newer = match min_icm {
        None => "The file names no `min_icm` this icm can read, so the key may come from a \
                 newer icm: if it does, install that icm and set `min_icm` to its version."
            .to_string(),
        Some(min) => format!(
            "If the key comes from an icm newer than `min_icm` ({min}), install that icm and \
             raise `min_icm` to its version."
        ),
    };
    problem.fix.summary = format!("{} {newer}", problem.fix.summary);
    problem
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

/// Semantic validation of a file `compatibility` let through. Returns every
/// problem.
fn validate(loaded: &Loaded) -> Vec<IcmError> {
    let config = &loaded.config;
    let mut problems = Vec::new();

    // schema: a newer one was `config.too_new` before the strict parse.
    if config.schema != SCHEMA {
        problems.push(invalid(loaded, "schema", format!("must be {SCHEMA}")));
    }

    // min_icm / icm: one this icm does not meet was `config.too_new`
    // before the strict parse.
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
        if let Err(message) = crate::version::parse_min(raw) {
            problems.push(invalid(loaded, key, message));
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

    // Signing references and store metadata (design §1 principle 5).
    problems.extend(validate_release_keys(loaded));

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

    unquote_secret_lines(&mut problems);
    problems
}

/// A line that holds a secret is never quoted, whichever key's problem
/// points at it: an inline table (`upload = { keystore = "", …,
/// store_pass_env = "<secret>" }`) or `sign_command` keeps several values
/// on one line, and only the secret's own finding knew to hide it.
fn unquote_secret_lines(problems: &mut [IcmError]) {
    let lines: Vec<(String, u32)> = problems
        .iter()
        .filter(|problem| problem.fix.summary == SECRET_FIX)
        .flat_map(|problem| problem.evidence.iter())
        .filter_map(|evidence| evidence.line.map(|line| (evidence.path.clone(), line)))
        .collect();
    for evidence in problems
        .iter_mut()
        .flat_map(|problem| problem.evidence.iter_mut())
    {
        if let Some(line) = evidence.line
            && lines.contains(&(evidence.path.clone(), line))
        {
            evidence.excerpt = None;
        }
    }
}

/// Whether `name` is an environment variable name in the form icm requires
/// for every `*_env` key: upper-case letters, digits and underscores, not
/// starting with a digit. A password pasted where its variable's name
/// belongs almost never passes.
pub fn is_env_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// The fix of a finding about a value that may be a secret.
const SECRET_FIX: &str = "Keep secrets out of icm.toml: put the value in an environment variable (or the keychain) and name the variable here.";

/// A finding about a value that may be a secret: it points at the line but
/// never quotes it (nor does any other finding on that line,
/// [`unquote_secret_lines`]).
fn invalid_secret(loaded: &Loaded, key: &str, message: &str) -> IcmError {
    invalid_quiet(loaded, key, message).fix(SECRET_FIX, &[])
}

/// [`invalid`] without the line's text, for the keys that sit next to
/// secrets or name them (`[android.signing]`, `sign_command`, `sign_env`).
fn invalid_quiet(loaded: &Loaded, key: &str, message: impl AsRef<str>) -> IcmError {
    let mut error = invalid(loaded, key, message);
    for evidence in &mut error.evidence {
        evidence.excerpt = None;
    }
    error
}

/// Short flags that take a secret: AzureSignTool's Key Vault client
/// secret, access token and password.
const SECRET_SHORT_FLAGS: &[&str] = &["kvs", "kvt", "kvp"];

/// A Windows-style switch (`/p`, `/fd`): a slash and letters or digits,
/// never a path.
fn is_slash_switch(token: &str) -> bool {
    token
        .strip_prefix('/')
        .is_some_and(|name| !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// The flag of a command line that takes a secret as a literal (not `$VAR`,
/// `${VAR}`, `%VAR%` or `env:VAR`), if any. Flags start with `-`, or `/`
/// as Windows tools write them. A flag takes a secret when its name has
/// `pass`, `password`, `secret` or `token` in it, is one of AzureSignTool's
/// `-kvs`/`-kvt`/`-kvp`, or is signtool's password `/p` (`-p` when the
/// command runs signtool).
pub fn literal_secret_flag(command: &str) -> Option<String> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let signtool = tokens.iter().any(|token| {
        let program = token
            .trim_matches(['"', '\''])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        program == "signtool" || program == "signtool.exe"
    });
    let secret_flag = |flag: &str| {
        let name = if flag.starts_with('-') {
            flag.trim_start_matches('-').to_ascii_lowercase()
        } else if is_slash_switch(flag) {
            flag[1..].to_ascii_lowercase()
        } else {
            return false;
        };
        ["pass", "password", "secret", "token"]
            .iter()
            .any(|word| name.contains(word))
            || SECRET_SHORT_FLAGS.contains(&name.as_str())
            || (name == "p" && (flag.starts_with('/') || signtool))
    };
    let indirect = |value: &str| {
        let value = value.trim_matches(['"', '\'']);
        value.starts_with('$') || value.starts_with('%') || value.starts_with("env:")
    };
    for (index, token) in tokens.iter().enumerate() {
        if let Some((flag, value)) = token.split_once('=') {
            if secret_flag(flag) && !indirect(value) {
                return Some(flag.to_string());
            }
        } else if secret_flag(token)
            && let Some(value) = tokens.get(index + 1)
            && !value.starts_with('-')
            && !is_slash_switch(value)
            && !indirect(value)
        {
            return Some(token.to_string());
        }
    }
    None
}

fn is_https_url(url: &str) -> bool {
    url.strip_prefix("https://").is_some_and(|rest| {
        !rest.is_empty()
            && !rest.starts_with('/')
            && !rest.chars().any(|c| c.is_whitespace() || c.is_control())
    })
}

/// A signing identity: `auto`, a certificate SHA-1 or its common name.
fn is_identity_ref(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && !value.chars().any(char::is_control)
}

/// A provisioning profile: `auto`, a UUID or a `.mobileprovision` path.
fn is_profile_ref(value: &str) -> bool {
    let uuid = |v: &str| {
        let groups: Vec<&str> = v.split('-').collect();
        groups.len() == 5
            && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, len)| {
                group.len() == len && group.chars().all(|c| c.is_ascii_hexdigit())
            })
    };
    value == "auto" || uuid(value) || value.ends_with(".mobileprovision")
}

/// The release keys: signing references, upload credentials (names
/// only) and store metadata.
fn validate_release_keys(loaded: &Loaded) -> Vec<IcmError> {
    let config = &loaded.config;
    let mut problems = Vec::new();
    let env_name = |key: &str, value: &str, problems: &mut Vec<IcmError>| {
        if !is_env_name(value) {
            problems.push(invalid_secret(
                loaded,
                key,
                "must be the name of an environment variable (A-Z, 0-9 and _), never the secret itself",
            ));
        }
    };

    // [ios]
    let ios = &config.ios;
    if let Some(team) = &ios.team_id
        && !(team.len() == 10
            && team
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()))
    {
        problems.push(invalid(
            loaded,
            "ios.team_id",
            "must be the 10-character Apple team id (upper-case letters and digits, e.g. \"ABCDE12345\")",
        ));
    }
    if let Some(id) = &ios.asc_app_id
        && !(!id.is_empty() && id.len() <= 12 && id.chars().all(|c| c.is_ascii_digit()))
    {
        problems.push(invalid(
            loaded,
            "ios.asc_app_id",
            "must be App Store Connect's numeric app id (App Information > Apple ID)",
        ));
    }
    if ios.export_compliance_code.is_some() && ios.uses_non_exempt_encryption != Some(true) {
        problems.push(invalid(
            loaded,
            "ios.export_compliance_code",
            "applies only when `ios.uses_non_exempt_encryption = true`; remove it or answer true",
        ));
    }
    for (kind, signing) in [
        ("development", &ios.signing.development),
        ("distribution", &ios.signing.distribution),
    ] {
        if !is_identity_ref(&signing.identity) {
            problems.push(invalid(
                loaded,
                &format!("ios.signing.{kind}.identity"),
                "must be \"auto\", a certificate SHA-1 or its common name",
            ));
        }
        if !is_profile_ref(&signing.profile) {
            problems.push(invalid(
                loaded,
                &format!("ios.signing.{kind}.profile"),
                "must be \"auto\", a profile UUID or the path of a .mobileprovision file",
            ));
        }
    }

    // [android.signing], [android.play]
    let android = &config.android;
    if let Some(upload) = android.signing.as_ref().and_then(|s| s.upload.as_ref()) {
        if upload.keystore.trim().is_empty() {
            problems.push(invalid_quiet(
                loaded,
                "android.signing.upload.keystore",
                "must be the keystore's path",
            ));
        }
        if upload.alias.trim().is_empty() {
            problems.push(invalid_quiet(
                loaded,
                "android.signing.upload.alias",
                "must be the key's alias",
            ));
        }
        env_name(
            "android.signing.upload.store_pass_env",
            &upload.store_pass_env,
            &mut problems,
        );
        if let Some(key_pass) = &upload.key_pass_env {
            env_name(
                "android.signing.upload.key_pass_env",
                key_pass,
                &mut problems,
            );
        }
    }
    env_name(
        "android.play.service_account_json_env",
        &android.play.service_account_json_env,
        &mut problems,
    );
    if android.play.track.trim().is_empty() {
        problems.push(invalid(
            loaded,
            "android.play.track",
            "must name a Play track (internal, alpha, beta, production or a closed-testing track)",
        ));
    }

    // [desktop.macos], [desktop.windows], [desktop.linux]
    let desktop = &config.desktop;
    if !is_identity_ref(&desktop.macos.identity) {
        problems.push(invalid(
            loaded,
            "desktop.macos.identity",
            "must be \"auto\", a certificate SHA-1 or its common name",
        ));
    }
    if let Some(profile) = &desktop.macos.notary_profile
        && (profile.trim().is_empty() || profile.chars().any(char::is_control))
    {
        problems.push(invalid(
            loaded,
            "desktop.macos.notary_profile",
            "must name the keychain profile `xcrun notarytool store-credentials` created",
        ));
    }
    for (index, format) in desktop.windows.formats.iter().enumerate() {
        if !["msi", "nsis"].contains(&format.as_str()) {
            problems.push(invalid(
                loaded,
                &format!("desktop.windows.formats[{index}]"),
                "must be \"msi\" or \"nsis\"",
            ));
        }
    }
    if let Some(command) = &desktop.windows.sign_command {
        if !command.contains("{file}") {
            problems.push(invalid_quiet(
                loaded,
                "desktop.windows.sign_command",
                "must contain `{file}`, which icm replaces with the file to sign",
            ));
        }
        if let Some(flag) = literal_secret_flag(command) {
            problems.push(invalid_secret(
                loaded,
                "desktop.windows.sign_command",
                &format!(
                    "passes a literal value to `{flag}`; pass secrets as $VARIABLES and list their names in `sign_env`"
                ),
            ));
        }
    }
    for (index, name) in desktop.windows.sign_env.iter().enumerate() {
        env_name(
            &format!("desktop.windows.sign_env[{index}]"),
            name,
            &mut problems,
        );
    }
    for (index, format) in desktop.linux.formats.iter().enumerate() {
        if !["deb", "appimage"].contains(&format.as_str()) {
            problems.push(invalid(
                loaded,
                &format!("desktop.linux.formats[{index}]"),
                "must be \"deb\" or \"appimage\"",
            ));
        }
    }
    if parse_os_version(&desktop.linux.glibc_floor).is_none() {
        problems.push(invalid(
            loaded,
            "desktop.linux.glibc_floor",
            "must look like \"2.35\"",
        ));
    }
    if let Some(maintainer) = &desktop.linux.maintainer {
        let well_formed = maintainer.split_once('<').is_some_and(|(name, rest)| {
            !name.trim().is_empty() && rest.ends_with('>') && rest.contains('@')
        });
        if !well_formed {
            problems.push(invalid(
                loaded,
                "desktop.linux.maintainer",
                "must look like \"Example Ltd <dev@example.com>\"",
            ));
        }
    }

    // [store]
    let store = &config.store;
    for (key, value) in [
        ("store.privacy_policy_url", &store.privacy_policy_url),
        ("store.support_url", &store.support_url),
        ("store.marketing_url", &store.marketing_url),
    ] {
        if let Some(url) = value
            && !is_https_url(url)
        {
            problems.push(invalid(
                loaded,
                key,
                "must be an https:// URL the stores can open",
            ));
        }
    }
    env_name("store.asc_key_id_env", &store.asc_key_id_env, &mut problems);
    env_name(
        "store.asc_issuer_id_env",
        &store.asc_issuer_id_env,
        &mut problems,
    );

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

    /// A version newer than this icm.
    fn newer_than_this() -> semver::Version {
        let current = crate::buildinfo::version();
        semver::Version::parse(&format!(
            "{}.{}.0-mobile.1",
            current.major,
            current.minor + 1
        ))
        .unwrap()
    }

    #[test]
    fn a_file_for_a_newer_icm_is_too_new_whatever_else_it_holds() {
        let newer = newer_than_this();
        // Keys a newer icm might add: at the top, in a known table and as a
        // new table, and a known key with a type this icm does not take.
        let extra = "colour = \"red\"\nbuild = \"one\"\n[future]\nkey = true\n";

        for (line, key) in [
            (format!("min_icm = \"{newer}\""), "min_icm"),
            (format!("icm = \">={newer}\""), "icm"),
        ] {
            let errors = parse_text(&format!("{line}\n{MINIMAL}{extra}")).unwrap_err();
            assert_eq!(errors.len(), 1, "{key}: {errors:?}");
            let error = &errors[0];
            assert_eq!(error.id, "config.too_new", "{key}");
            assert_eq!(error.exit, crate::exit::Exit::Environment);
            assert_eq!(error.exit.code(), 4);
            assert_eq!(error.evidence[0].line, Some(1), "{key}");
            assert!(
                error.detail.starts_with(&format!(
                    "/proj/icm.toml:1: this project needs icm {newer} or newer"
                )),
                "{}",
                error.detail
            );
            assert_eq!(
                error.fix.commands,
                vec![crate::version::install_command(Some(&newer))],
                "{key}"
            );
        }

        // A newer schema, with no minimum to name.
        let schema = format!("{}{extra}", MINIMAL.replace("schema = 1", "schema = 2"));
        let errors = parse_text(&schema).unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].id, "config.too_new");
        assert_eq!(
            errors[0].fix.commands,
            vec![crate::version::install_command(None)]
        );

        // Both at once: each is said.
        let both = format!(
            "min_icm = \"{newer}\"\n{}{extra}",
            MINIMAL.replace("schema = 1", "schema = 2")
        );
        let ids: Vec<String> = parse_text(&both)
            .unwrap_err()
            .iter()
            .map(|e| e.id.to_string())
            .collect();
        assert_eq!(ids, ["config.too_new", "config.too_new"]);

        // A syntax error is still the strict parse's to report.
        let broken = first_error(&format!("min_icm = \"{newer}\"\n{MINIMAL}[app\n"));
        assert_eq!(broken.id, "config.invalid");
    }

    #[test]
    fn unknown_keys_under_a_met_minimum_stay_unknown() {
        let current = crate::buildinfo::version();
        let older = semver::Version::parse("0.14.1-mobile.1").unwrap();
        assert!(older <= current);

        for min in [current.clone(), older.clone()] {
            for (text, line) in [
                (
                    format!("min_icm = \"{min}\"\n{MINIMAL}colour = \"red\"\n"),
                    6,
                ),
                (
                    format!("icm = \">={min}\"\n{MINIMAL}[future]\nkey = 1\n"),
                    6,
                ),
                (
                    format!("min_icm = \"{min}\"\n{MINIMAL}[checks]\ntv = []\n"),
                    7,
                ),
            ] {
                let error = first_error(&text);
                assert_eq!(error.id, "config.unknown_key", "{text}");
                assert_eq!(error.exit, crate::exit::Exit::Config);
                assert_eq!(error.exit.code(), 3);
                assert_eq!(error.evidence[0].line, Some(line), "{text}");
                assert!(
                    error.fix.summary.starts_with("Remove or rename the key")
                        && error.fix.summary.contains(&format!(
                            "an icm newer than `min_icm` ({min}), install that icm and raise \
                             `min_icm`"
                        )),
                    "{}",
                    error.fix.summary
                );
                assert!(error.fix.commands.is_empty());
            }
        }

        // Without a minimum, the key may be a newer icm's too.
        let error = first_error(&format!("{MINIMAL}colour = \"red\"\n"));
        assert_eq!(error.id, "config.unknown_key");
        assert!(
            error.fix.summary.contains(
                "names no `min_icm` this icm can read, so the key may come from a newer icm: if \
                 it does, install that icm and set `min_icm` to its version"
            ),
            "{}",
            error.fix.summary
        );

        // A minimum that is not a version is no minimum to the key's fix,
        // and `config.invalid` once the key is gone.
        let bad = format!("min_icm = \"^0.14\"\n{MINIMAL}colour = \"red\"\n");
        let error = first_error(&bad);
        assert_eq!(error.id, "config.unknown_key");
        assert!(
            error
                .fix
                .summary
                .contains("names no `min_icm` this icm can read")
        );
        let error = first_error(&bad.replace("colour = \"red\"\n", ""));
        assert_eq!(error.id, "config.invalid");

        // Other findings keep their own fix.
        let error = first_error(&format!("min_icm = \"{older}\"\n{MINIMAL}build = 0\n"));
        assert_eq!(error.id, "config.invalid");
        assert!(
            !error.fix.summary.contains("min_icm"),
            "{}",
            error.fix.summary
        );
    }

    /// The template's comment on `min_icm` (`icm explain config.min_icm`)
    /// says what an older icm does with the file.
    #[test]
    fn the_template_says_what_an_older_icm_does() {
        let keys = crate::template::config_keys();
        let min_icm = keys
            .iter()
            .find(|k| k.key == "min_icm")
            .expect("the template documents min_icm");
        let comment = &min_icm.comment;

        // From this icm on, an older icm than the file asks for exits 4,
        // even when the file holds a key or table it does not know.
        assert!(
            comment.contains("an older icm exits 4 (config.too_new)"),
            "{comment}"
        );
        let template = std::str::from_utf8(crate::template::file("icm.toml").unwrap()).unwrap();
        assert!(parse_text(template).is_ok());
        let newer = newer_than_this();
        let text = format!(
            "{}\n[future]\nkey = true\n",
            template.replace(&min_icm.line, &format!("min_icm = \"{newer}\""))
        );
        let errors = parse_text(&text).unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].id, "config.too_new");
        assert_eq!(errors[0].exit.code(), 4);

        // The one release that parsed strictly first is named, with what it
        // says instead.
        assert!(
            comment.contains(
                "except 0.14.1-mobile.1, which stops first at a key it does not know \
                 (config.unknown_key, exit 3)"
            ),
            "{comment}"
        );
        assert!(
            semver::Version::parse("0.14.1-mobile.1").unwrap() < crate::buildinfo::version(),
            "the release that parsed strictly first is older than this one"
        );
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
    fn release_keys_are_references_that_validate() {
        let good = format!(
            "{MINIMAL}[ios]\nteam_id = \"ABCDE12345\"\nasc_app_id = \"1234567890\"\nuses_non_exempt_encryption = true\nexport_compliance_code = \"abc\"\n[ios.signing]\ndistribution = {{ identity = \"0123456789abcdef0123456789abcdef01234567\", profile = \"01234567-89ab-cdef-0123-456789abcdef\" }}\n[android.signing]\nupload = {{ keystore = \"~/.icm/keys/up.jks\", alias = \"upload\", store_pass_env = \"ICM_ANDROID_STORE_PASS\", key_pass_env = \"ICM_ANDROID_KEY_PASS\" }}\n[desktop.windows]\nsign_command = \"jsign --storetype TRUSTEDSIGNING --storepass $AZURE_TOKEN {{file}}\"\nsign_env = [\"AZURE_TOKEN\"]\n[desktop.linux]\nmaintainer = \"Acme <dev@acme.com>\"\n[store]\nprivacy_policy_url = \"https://acme.com/privacy\"\nsupport_url = \"https://acme.com/help\"\n"
        );
        let loaded = parse_text(&good).unwrap_or_else(|errors| panic!("{errors:?}"));
        assert_eq!(loaded.config.store.asc_key_id_env, "ASC_KEY_ID");
        assert_eq!(loaded.config.desktop.windows.sign_env, ["AZURE_TOKEN"]);

        let errors = |text: String| -> Vec<IcmError> { parse_text(&text).unwrap_err() };
        let details = |errors: &[IcmError]| -> String {
            errors
                .iter()
                .map(|e| e.detail.clone())
                .collect::<Vec<_>>()
                .join("\n")
        };

        // A password where its variable's name belongs is refused, and
        // neither the detail nor the evidence quotes it.
        let pasted = errors(format!(
            "{MINIMAL}[android.signing]\nupload = {{ keystore = \"k.jks\", alias = \"upload\", store_pass_env = \"hunter2!\" }}\n"
        ));
        assert_eq!(pasted.len(), 1, "{}", details(&pasted));
        assert_eq!(pasted[0].id, "config.invalid");
        assert!(
            pasted[0]
                .detail
                .contains("android.signing.upload.store_pass_env")
        );
        assert!(!pasted[0].detail.contains("hunter2"));
        assert_eq!(pasted[0].evidence[0].line, Some(6));
        assert_eq!(pasted[0].evidence[0].excerpt, None);

        let bad = errors(format!(
            "{MINIMAL}[ios]\nteam_id = \"abc\"\nasc_app_id = \"com.acme\"\nexport_compliance_code = \"x\"\n[ios.signing]\ndistribution = {{ identity = \"auto\", profile = \"my profile\" }}\n[desktop.windows]\nsign_command = \"signtool sign /f cert.pfx --password s3cret\"\nsign_env = [\"azure_token\"]\nformats = [\"zip\"]\n[desktop.linux]\nmaintainer = \"nobody\"\n[store]\nprivacy_policy_url = \"http://acme.com/privacy\"\n"
        ));
        let text = details(&bad);
        for key in [
            "ios.team_id",
            "ios.asc_app_id",
            "ios.export_compliance_code",
            "ios.signing.distribution.profile",
            "desktop.windows.sign_command` must contain `{file}`",
            "passes a literal value to `--password`",
            "desktop.windows.sign_env[0]",
            "desktop.windows.formats[0]",
            "desktop.linux.maintainer",
            "store.privacy_policy_url",
        ] {
            assert!(text.contains(key), "{key} not in:\n{text}");
        }
        assert!(!text.contains("s3cret"), "{text}");
    }

    #[test]
    fn literal_secrets_in_commands_are_found() {
        assert_eq!(
            literal_secret_flag("jsign --storepass hunter2 {file}"),
            Some("--storepass".to_string())
        );
        assert_eq!(
            literal_secret_flag("tool --password=hunter2 {file}"),
            Some("--password".to_string())
        );
        assert_eq!(literal_secret_flag("jsign --storepass $PASS {file}"), None);
        assert_eq!(
            literal_secret_flag("jsign --storepass \"${PASS}\" {file}"),
            None
        );
        assert_eq!(literal_secret_flag("signtool sign /p %PASS% {file}"), None);
        assert_eq!(literal_secret_flag("tool --token env:TOKEN {file}"), None);
        assert_eq!(
            literal_secret_flag("jsign --keystore k.p12 --alias a {file}"),
            None
        );
        // signtool's password is /p (or -p), Windows tools' switches start
        // with a slash, and AzureSignTool's secrets are short flags.
        assert_eq!(
            literal_secret_flag(
                "signtool sign /fd SHA256 /f C:/certs/code.pfx /p Hunter2Secret! {file}"
            ),
            Some("/p".to_string())
        );
        assert_eq!(
            literal_secret_flag(
                "\"C:/Program Files (x86)/Windows Kits/10/bin/x64/signtool.exe\" sign -f x.pfx -p hunter2 {file}"
            ),
            Some("-p".to_string())
        );
        assert_eq!(
            literal_secret_flag("tool /Password hunter2 {file}"),
            Some("/Password".to_string())
        );
        for flag in ["-kvs", "-kvt", "-kvp"] {
            assert_eq!(
                literal_secret_flag(&format!(
                    "AzureSignTool sign -kvu https://v.example {flag} hunter2 {{file}}"
                )),
                Some(flag.to_string())
            );
        }
        assert_eq!(
            literal_secret_flag("signtool sign /f x.pfx /p $PASS {file}"),
            None
        );
        assert_eq!(
            literal_secret_flag("AzureSignTool sign -kvs %AZURE_SECRET% {file}"),
            None
        );
        // `-p` of another tool, and paths that merely contain a word, are
        // not secrets.
        assert_eq!(literal_secret_flag("my-sign -p production {file}"), None);
        assert_eq!(
            literal_secret_flag("/opt/tokens/sign --in {file} /usr/share/password-free"),
            None
        );
        assert_eq!(
            literal_secret_flag("signtool sign /p /fd SHA256 {file}"),
            None
        );
    }

    #[test]
    fn secrets_are_quoted_by_no_finding_on_their_line() {
        let errors = |text: String| -> Vec<IcmError> { parse_text(&text).unwrap_err() };
        let quoted = |errors: &[IcmError]| -> String {
            serde_json::to_string(
                &errors
                    .iter()
                    .map(|e| (e.detail.clone(), e.evidence.clone()))
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        };
        // A missing {file} next to a literal password.
        let sign = errors(format!(
            "{MINIMAL}[desktop.windows]\nsign_command = \"jsign --storepass Hunter2Secret! app.exe\"\n"
        ));
        assert_eq!(sign.len(), 2, "{}", quoted(&sign));
        assert!(sign.iter().all(|e| e.evidence[0].line == Some(6)));
        assert!(
            !quoted(&sign).contains("Hunter2Secret!"),
            "{}",
            quoted(&sign)
        );
        // signtool's /p, same.
        let signtool = errors(format!(
            "{MINIMAL}[desktop.windows]\nsign_command = \"signtool sign /fd SHA256 /f C:/certs/code.pfx /p Hunter2Secret! {{file}}\"\n"
        ));
        assert_eq!(signtool.len(), 1, "{}", quoted(&signtool));
        assert!(
            signtool[0].detail.contains("`/p`"),
            "{}",
            signtool[0].detail
        );
        assert!(!quoted(&signtool).contains("Hunter2Secret!"));
        // An empty keystore next to a pasted password in one inline table.
        let upload = errors(format!(
            "{MINIMAL}[android.signing]\nupload = {{ keystore = \"\", alias = \"upload\", store_pass_env = \"Hunter2Secret!\" }}\n"
        ));
        assert_eq!(upload.len(), 2, "{}", quoted(&upload));
        assert!(
            !quoted(&upload).contains("Hunter2Secret!"),
            "{}",
            quoted(&upload)
        );
        // Other findings keep their excerpt.
        let other = errors(format!(
            "{MINIMAL}[desktop.linux]\nmaintainer = \"nobody\"\n"
        ));
        assert_eq!(
            other[0].evidence[0].excerpt.as_deref(),
            Some("maintainer = \"nobody\"")
        );
    }

    #[test]
    fn env_names_are_upper_case() {
        assert!(is_env_name("ICM_ANDROID_STORE_PASS"));
        assert!(is_env_name("_X1"));
        assert!(!is_env_name("icm_pass"));
        assert!(!is_env_name("1PASS"));
        assert!(!is_env_name(""));
        assert!(!is_env_name("PASS WORD"));
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
