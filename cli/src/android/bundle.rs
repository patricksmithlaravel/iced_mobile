//! The Android App Bundle (design §11.2, §12.3): what `icm release
//! android` puts into `base.zip`, what it reads back from the tools it
//! runs (keytool, jarsigner, `bundletool dump`), and the store gates on
//! the linked bundle. Everything here is pure: the pipeline
//! (`release/android.rs`) runs the tools and reports the checks.

use super::elf;
use super::manifest;
use super::zip;
use crate::catalogue::CheckId;
use crate::config::Abi;
use crate::error::{Check, Evidence};
use serde_json::Value;
use std::io;
use std::path::{Path, PathBuf};

/// Where the notices live inside `base.zip` (the bundle has them at
/// `base/<this>`, an APK at `<this>`).
pub const NOTICES_ENTRY: &str = "assets/THIRD_PARTY_NOTICES.txt";

/// `BundleConfig.json`: native libraries stay uncompressed and aligned to
/// 16 KB pages in the APKs Google Play serves. bundletool's default is
/// 4 KB, so the setting is mandatory (`android.bundle.alignment`).
pub const BUNDLE_CONFIG: &str = "{\"optimizations\":{\"uncompressNativeLibraries\":{\"enabled\":true,\"alignment\":\"PAGE_ALIGNMENT_16K\"}}}\n";

/// The highest versionCode Google Play accepts.
pub const MAX_VERSION_CODE: u64 = 2_100_000_000;

/// The marker the agent bridge (phase 6) compiles into a library; a store
/// build must not contain it (`store.no_agent_bridge`).
pub const AGENT_MARKER: &[u8] = b"ICM_AGENT_BRIDGE_V1";

/// The library NativeActivity calls.
pub const ENTRY_POINT: &str = "ANativeActivity_onCreate";

/// Permissions with the `dangerous` protection level: each is a runtime
/// prompt, and Google Play's Data safety form must account for what they
/// collect (`android.permissions.review`).
pub const DANGEROUS_PERMISSIONS: &[&str] = &[
    "ACCEPT_HANDOVER",
    "ACCESS_BACKGROUND_LOCATION",
    "ACCESS_COARSE_LOCATION",
    "ACCESS_FINE_LOCATION",
    "ACCESS_MEDIA_LOCATION",
    "ACTIVITY_RECOGNITION",
    "ADD_VOICEMAIL",
    "ANSWER_PHONE_CALLS",
    "BLUETOOTH_ADVERTISE",
    "BLUETOOTH_CONNECT",
    "BLUETOOTH_SCAN",
    "BODY_SENSORS",
    "BODY_SENSORS_BACKGROUND",
    "CALL_PHONE",
    "CAMERA",
    "GET_ACCOUNTS",
    "NEARBY_WIFI_DEVICES",
    "POST_NOTIFICATIONS",
    "READ_CALENDAR",
    "READ_CALL_LOG",
    "READ_CONTACTS",
    "READ_EXTERNAL_STORAGE",
    "READ_MEDIA_AUDIO",
    "READ_MEDIA_IMAGES",
    "READ_MEDIA_VIDEO",
    "READ_MEDIA_VISUAL_USER_SELECTED",
    "READ_PHONE_NUMBERS",
    "READ_PHONE_STATE",
    "READ_SMS",
    "RECEIVE_MMS",
    "RECEIVE_SMS",
    "RECEIVE_WAP_PUSH",
    "RECORD_AUDIO",
    "SEND_SMS",
    "USE_SIP",
    "UWB_RANGING",
    "WRITE_CALENDAR",
    "WRITE_CALL_LOG",
    "WRITE_CONTACTS",
    "WRITE_EXTERNAL_STORAGE",
];

// ---- base.zip ---------------------------------------------------------------------

/// The entries of `base.zip` (design §11.2 step 3), in a fixed order:
/// `manifest/AndroidManifest.xml` (aapt2 writes it at the root of its
/// proto output; the bundle wants it under `manifest/`), `resources.pb`,
/// `res/**`, `lib/<abi>/lib<lib>.so` and `assets/**`. There is no `dex/`:
/// the app has no code (`hasCode="false"`). An asset whose path repeats
/// an earlier one (the notices) is left out.
pub fn base_entries(
    proto: &Path,
    libs: &[(Abi, PathBuf)],
    lib: &str,
    assets: &[(PathBuf, String)],
) -> io::Result<Vec<zip::Entry>> {
    let manifest = proto.join("AndroidManifest.xml");
    let resources = proto.join("resources.pb");
    for required in [&manifest, &resources] {
        if !required.is_file() {
            return Err(io::Error::other(format!(
                "aapt2 --proto-format left no {}",
                required.display()
            )));
        }
    }
    let mut entries = vec![
        zip::Entry {
            name: "manifest/AndroidManifest.xml".to_string(),
            source: zip::Source::File(manifest),
        },
        zip::Entry {
            name: "resources.pb".to_string(),
            source: zip::Source::File(resources),
        },
    ];
    let res = proto.join("res");
    if res.is_dir() {
        let mut files = Vec::new();
        walk(&res, &res, &mut files)?;
        files.sort_by(|a, b| a.1.cmp(&b.1));
        entries.extend(files.into_iter().map(|(path, relative)| zip::Entry {
            name: format!("res/{relative}"),
            source: zip::Source::File(path),
        }));
    }
    let mut libs: Vec<&(Abi, PathBuf)> = libs.iter().collect();
    libs.sort_by_key(|(abi, _)| abi.as_str());
    for (abi, path) in libs {
        entries.push(zip::Entry {
            name: format!("lib/{}/lib{lib}.so", abi.as_str()),
            source: zip::Source::File(path.clone()),
        });
    }
    for (path, relative) in assets {
        let name = format!("assets/{relative}");
        if entries.iter().any(|entry| entry.name == name) {
            continue;
        }
        entries.push(zip::Entry {
            name,
            source: zip::Source::File(path.clone()),
        });
    }
    Ok(entries)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, String)>) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            walk(root, &path, out)?;
        } else if let Ok(relative) = path.strip_prefix(root) {
            let name = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.push((path, name));
        }
    }
    Ok(())
}

/// The native libraries in a bundle's (or APK's) entry names:
/// `(abi, entry)` for `[base/]lib/<abi>/<file>.so`.
pub fn libraries(names: &[String]) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = names
        .iter()
        .filter_map(|name| {
            let rest = name.strip_prefix("base/").unwrap_or(name);
            let rest = rest.strip_prefix("lib/")?;
            let (abi, file) = rest.split_once('/')?;
            (file.ends_with(".so") && !file.contains('/')).then(|| (abi.to_string(), name.clone()))
        })
        .collect();
    found.sort();
    found
}

// ---- the upload key ---------------------------------------------------------------

/// The key algorithm of an upload key, which picks jarsigner's `-sigalg`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAlgorithm {
    /// RSA: `SHA256withRSA`.
    Rsa,
    /// An elliptic-curve key: `SHA256withECDSA`.
    Ec,
    /// DSA: `SHA256withDSA` (Google Play accepts it; keytool no longer
    /// makes them by default).
    Dsa,
}

impl KeyAlgorithm {
    /// jarsigner's `-sigalg`.
    pub fn sigalg(self) -> &'static str {
        match self {
            KeyAlgorithm::Rsa => "SHA256withRSA",
            KeyAlgorithm::Ec => "SHA256withECDSA",
            KeyAlgorithm::Dsa => "SHA256withDSA",
        }
    }
}

/// What `keytool -list -v -alias <alias>` says about the upload key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UploadKey {
    /// Its algorithm, from `Subject Public Key Algorithm:`.
    pub algorithm: Option<KeyAlgorithm>,
    /// Its certificate's SHA-256 fingerprint (`AB:CD:…`, upper case).
    pub sha256: Option<String>,
    /// The certificate's owner (`CN=…`).
    pub owner: Option<String>,
}

/// The first `SHA256:` fingerprint in keytool output, upper case.
fn sha256_line(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let value = line.trim().strip_prefix("SHA256:")?.trim();
        (value.len() >= 95 && value.chars().all(|c| c.is_ascii_hexdigit() || c == ':'))
            .then(|| value.to_ascii_uppercase())
    })
}

/// Parses `keytool -J-Duser.language=en -list -v -alias <alias>`.
pub fn parse_keytool_list(text: &str) -> UploadKey {
    let mut key = UploadKey {
        sha256: sha256_line(text),
        ..UploadKey::default()
    };
    for line in text.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("Subject Public Key Algorithm:") {
            let value = value.to_ascii_uppercase();
            key.algorithm = if value.contains("RSA") {
                Some(KeyAlgorithm::Rsa)
            } else if value.contains(" EC") || value.contains("ECDSA") {
                Some(KeyAlgorithm::Ec)
            } else if value.contains("DSA") {
                Some(KeyAlgorithm::Dsa)
            } else {
                None
            };
        } else if key.owner.is_none()
            && let Some(value) = line.strip_prefix("Owner:")
        {
            key.owner = Some(value.trim().to_string());
        }
    }
    key
}

/// Why keytool or jarsigner could not use the upload key, from their
/// output (`None`: not a key problem).
pub fn key_failure(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let reason = if lower.contains("password was incorrect")
        || lower.contains("cannot recover key")
        || lower.contains("unrecoverablekeyexception")
        || lower.contains("given final block not properly padded")
    {
        "a password is wrong (the store password, or the key password when the key has its own)"
            .to_string()
    } else if lower.contains("does not exist") && lower.contains("alias") {
        "the alias is not in the keystore".to_string()
    } else if lower.contains("cannot find environment variable") {
        "a password variable is not set in icm's environment".to_string()
    } else if lower.contains("keystore file does not exist")
        || lower.contains("filenotfoundexception")
    {
        "the keystore file does not exist".to_string()
    } else if lower.contains("unrecognized keystore format")
        || lower.contains("invalid keystore format")
        || lower.contains("not a keystore")
    {
        "the file is not a keystore keytool can read (PKCS12 or JKS)".to_string()
    } else if lower.contains("not a private key entry") || lower.contains("no private key") {
        "the alias holds a certificate, not a private key".to_string()
    } else {
        return None;
    };
    Some(reason)
}

/// What `jarsigner -verify -verbose -certs` says about a bundle. It is run
/// without `-strict`, which fails every self-signed upload key, and its
/// exit code is not trusted: an unsigned jar exits 0.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Verified {
    /// `jar verified.`, and not `jar is unsigned.`.
    pub verified: bool,
    /// `jar is unsigned.`
    pub unsigned: bool,
    /// The signatures carry no timestamp (INFO: Google Play does not need
    /// one).
    pub no_timestamp: bool,
    /// Signers (`- Signed by "CN=…"`).
    pub signers: Vec<String>,
}

/// Parses `jarsigner -J-Duser.language=en -verify -verbose -certs <aab>`.
pub fn parse_jarsigner_verify(text: &str) -> Verified {
    let unsigned = text.contains("jar is unsigned");
    let signers = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- Signed by \""))
        .map(|rest| rest.trim_end_matches('"').to_string())
        .collect();
    Verified {
        verified: text.contains("jar verified.") && !unsigned,
        unsigned,
        no_timestamp: text.contains("do not include a timestamp")
            || text.contains("signed without a timestamp"),
        signers,
    }
}

/// The signer certificate's SHA-256 from `keytool -printcert -jarfile`
/// (`None`: "Not a signed jar file").
pub fn parse_printcert(text: &str) -> Option<String> {
    if text.contains("Not a signed jar file") {
        return None;
    }
    sha256_line(text)
}

// ---- bundletool dump --------------------------------------------------------------

/// What `bundletool dump manifest` shows of the linked manifest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dumped {
    /// `package`.
    pub package: Option<String>,
    /// `android:versionCode`.
    pub version_code: Option<u64>,
    /// `android:versionName`.
    pub version_name: Option<String>,
    /// `<uses-sdk android:minSdkVersion>`.
    pub min_sdk: Option<u32>,
    /// `<uses-sdk android:targetSdkVersion>`.
    pub target_sdk: Option<u32>,
    /// `<application android:debuggable>`.
    pub debuggable: Option<bool>,
    /// `<application android:hasCode>`.
    pub has_code: Option<bool>,
    /// `<application android:enableOnBackInvokedCallback>`.
    pub back_callback: Option<bool>,
    /// The NativeActivity's `android:configChanges`, as the bit mask
    /// (`ActivityInfo.CONFIG_*`).
    pub config_changes: Option<u32>,
    /// The `android.app.lib_name` meta-data value.
    pub lib_name: Option<String>,
    /// `<uses-permission android:name>` values.
    pub permissions: Vec<String>,
    /// Whether the NativeActivity was found.
    pub native_activity: bool,
}

/// One start tag: its name and attributes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Tag {
    name: String,
    attributes: Vec<(String, String)>,
    /// `<…/>`: no children.
    empty: bool,
    /// `</…>`.
    end: bool,
}

impl Tag {
    fn get(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&#10;", "\n")
        .replace("&amp;", "&")
}

/// The tags of well-formed XML (comments, declarations and text skipped).
fn tags(xml: &str) -> Vec<Tag> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find('<') {
        rest = &rest[start + 1..];
        if rest.starts_with("!--") {
            rest = rest.find("-->").map_or("", |end| &rest[end + 3..]);
            continue;
        }
        if rest.starts_with('?') || rest.starts_with('!') {
            rest = rest.find('>').map_or("", |end| &rest[end + 1..]);
            continue;
        }
        // The tag runs to the first `>` outside quotes.
        let mut quote = None;
        let mut end = None;
        for (index, c) in rest.char_indices() {
            match (quote, c) {
                (None, '"' | '\'') => quote = Some(c),
                (Some(open), c) if c == open => quote = None,
                (None, '>') => {
                    end = Some(index);
                    break;
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        let body = &rest[..end];
        rest = &rest[end + 1..];
        let (closing, body) = match body.strip_prefix('/') {
            Some(body) => (true, body),
            None => (false, body),
        };
        let (empty, body) = match body.trim_end().strip_suffix('/') {
            Some(body) => (true, body),
            None => (false, body),
        };
        let name_end = body.find(char::is_whitespace).unwrap_or(body.len());
        let name = body[..name_end].to_string();
        let mut attributes = Vec::new();
        let mut text = &body[name_end..];
        while let Some(eq) = text.find('=') {
            let key = text[..eq].trim().to_string();
            let after = text[eq + 1..].trim_start();
            let Some(open) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                break;
            };
            let value_text = &after[1..];
            let Some(close) = value_text.find(open) else {
                break;
            };
            attributes.push((key, unescape(&value_text[..close])));
            text = &value_text[close + 1..];
        }
        out.push(Tag {
            name,
            attributes,
            empty,
            end: closing,
        });
    }
    out
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim() {
        "true" | "-1" | "0xffffffff" | "1" => Some(true),
        "false" | "0" | "0x0" | "0x00000000" => Some(false),
        _ => None,
    }
}

/// `android:configChanges` as printed: a hex mask (`0xd000ffff`), a
/// decimal one, or the names.
pub fn parse_config_changes(value: &str) -> Option<u32> {
    let value = value.trim();
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        return u32::from_str_radix(hex, 16).ok();
    }
    if let Ok(number) = value.parse::<u32>() {
        return Some(number);
    }
    let mut mask = 0;
    for name in value.split('|').map(str::trim) {
        let (_, _, bit) = manifest::CONFIG_CHANGES
            .iter()
            .find(|(known, _, _)| *known == name)?;
        mask |= bit;
    }
    Some(mask)
}

/// The mask of the `configChanges` list icm generates for `api`.
pub fn config_changes_mask(api: u32) -> u32 {
    manifest::CONFIG_CHANGES
        .iter()
        .filter(|(_, since, _)| *since <= api)
        .fold(0, |mask, (_, _, bit)| mask | bit)
}

/// Parses `bundletool dump manifest --bundle=<aab>` (or `aapt2 dump
/// xmltree`-like XML with the same attribute names).
pub fn parse_manifest(xml: &str) -> Dumped {
    let mut dumped = Dumped::default();
    let mut in_native = false;
    for tag in tags(xml) {
        if tag.end {
            if tag.name == "activity" {
                in_native = false;
            }
            continue;
        }
        match tag.name.as_str() {
            "manifest" => {
                dumped.package = tag.get("package").map(str::to_string);
                dumped.version_code = tag
                    .get("android:versionCode")
                    .and_then(|v| v.trim().parse().ok());
                dumped.version_name = tag.get("android:versionName").map(str::to_string);
            }
            "uses-sdk" => {
                dumped.min_sdk = tag
                    .get("android:minSdkVersion")
                    .and_then(|v| v.trim().parse().ok());
                dumped.target_sdk = tag
                    .get("android:targetSdkVersion")
                    .and_then(|v| v.trim().parse().ok());
            }
            "uses-permission" => {
                if let Some(name) = tag.get("android:name") {
                    dumped.permissions.push(name.to_string());
                }
            }
            "application" => {
                dumped.debuggable = tag.get("android:debuggable").and_then(parse_bool);
                dumped.has_code = tag.get("android:hasCode").and_then(parse_bool);
                dumped.back_callback = tag
                    .get("android:enableOnBackInvokedCallback")
                    .and_then(parse_bool);
            }
            "activity" => {
                if tag.get("android:name") == Some(manifest::ACTIVITY) {
                    dumped.native_activity = true;
                    dumped.config_changes = tag
                        .get("android:configChanges")
                        .and_then(parse_config_changes);
                    in_native = !tag.empty;
                }
            }
            "meta-data" if in_native && tag.get("android:name") == Some("android.app.lib_name") => {
                dumped.lib_name = tag.get("android:value").map(str::to_string);
            }
            _ => {}
        }
    }
    dumped
}

/// `optimizations.uncompressNativeLibraries` in `bundletool dump config`:
/// `(enabled, alignment)`.
pub fn parse_config(json: &str) -> Option<(bool, Option<String>)> {
    let value: Value = serde_json::from_str(json.trim()).ok()?;
    let native = value
        .get("optimizations")?
        .get("uncompressNativeLibraries")?;
    Some((
        native
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        native
            .get("alignment")
            .and_then(Value::as_str)
            .map(str::to_string),
    ))
}

// ---- gates ------------------------------------------------------------------------

/// What the gates compare the bundle with. A field left `None` (or empty)
/// is not checked: `icm verify` on a bundle built elsewhere knows only the
/// store's floors.
#[derive(Clone, Debug, Default)]
pub struct Expect {
    /// Cargo's version (versionName).
    pub version: Option<String>,
    /// `[app] build` (versionCode).
    pub build: Option<u64>,
    /// The library name (`lib<lib>.so`).
    pub lib: Option<String>,
    /// `[android] abis`.
    pub abis: Vec<Abi>,
    /// Google Play's targetSdk floor (`play.target_sdk`).
    pub target_sdk_floor: Option<u32>,
    /// `[android] activity = "native"`: no code.
    pub native: bool,
}

/// The §12.3 gates on the linked manifest and the bundle's entries.
/// `dump` is the saved `bundletool dump manifest` output (the evidence).
pub fn manifest_checks(
    dumped: &Dumped,
    names: &[String],
    expect: &Expect,
    dump: &Path,
) -> Vec<Check> {
    let evidence = || Evidence::file(dump);
    let mut checks = Vec::new();

    // targetSdk.
    checks.push(match (dumped.target_sdk, expect.target_sdk_floor) {
        (Some(target), Some(floor)) if target >= floor => Check::pass(
            CheckId::AndroidManifestTargetSdk,
            format!("targetSdkVersion {target} (Google Play's floor is {floor})"),
        ),
        (Some(target), Some(floor)) => Check::fail(
            CheckId::AndroidManifestTargetSdk,
            format!("targetSdkVersion is {target}; Google Play takes new apps and updates only from {floor}"),
        )
        .evidence(evidence())
        .fix(format!("Set [android] target_sdk = {floor} in icm.toml."), &[]),
        (Some(target), None) => Check::pass(
            CheckId::AndroidManifestTargetSdk,
            format!("targetSdkVersion {target}"),
        ),
        (None, _) => Check::fail(
            CheckId::AndroidManifestTargetSdk,
            "the linked manifest has no targetSdkVersion",
        )
        .evidence(evidence()),
    });

    // configChanges: every value icm generates for the target API.
    let api = dumped.target_sdk.unwrap_or(0);
    let wanted = config_changes_mask(api);
    checks.push(match dumped.config_changes {
        _ if !dumped.native_activity => Check::fail(
            CheckId::AndroidManifestConfigChanges,
            format!("the linked manifest has no {} activity", manifest::ACTIVITY),
        )
        .evidence(evidence()),
        Some(mask) if mask & wanted == wanted => Check::pass(
            CheckId::AndroidManifestConfigChanges,
            format!("android:configChanges lists every change for API {api} ({mask:#x})"),
        ),
        found => {
            let missing = manifest::config_names(wanted & !found.unwrap_or(0));
            Check::fail(
                CheckId::AndroidManifestConfigChanges,
                format!(
                    "android:configChanges lacks {}: Android would destroy and recreate the activity for those changes, and the app would start over",
                    missing.join("|")
                ),
            )
            .evidence(evidence())
        }
    });

    // hasCode ⇔ dex/.
    let dex = names
        .iter()
        .any(|name| name.starts_with("base/dex/") || name.starts_with("dex/"));
    let has_code = dumped.has_code.unwrap_or(true);
    checks.push(match (has_code, dex) {
        (false, false) => Check::pass(
            CheckId::AndroidManifestHasCode,
            "android:hasCode=\"false\" and the bundle has no dex/",
        ),
        (true, true) => Check::pass(
            CheckId::AndroidManifestHasCode,
            "android:hasCode is true and the bundle has dex/",
        ),
        (false, true) => Check::fail(
            CheckId::AndroidManifestHasCode,
            "android:hasCode=\"false\" but the bundle carries dex/: Android would never load that code",
        )
        .evidence(evidence()),
        (true, false) => Check::fail(
            CheckId::AndroidManifestHasCode,
            "android:hasCode is true (or unset) but the bundle has no dex/: Android would look for classes that are not there",
        )
        .evidence(evidence()),
    });
    if expect.native && dex {
        checks.push(
            Check::fail(
                CheckId::AndroidManifestHasCode,
                "[android] activity = \"native\" needs no code, yet the bundle has dex/",
            )
            .evidence(evidence()),
        );
    }

    // debuggable.
    checks.push(match dumped.debuggable {
        Some(true) => Check::fail(
            CheckId::AndroidManifestDebuggable,
            "the release manifest has android:debuggable=\"true\"; Google Play refuses debuggable uploads",
        )
        .evidence(evidence()),
        _ => Check::pass(
            CheckId::AndroidManifestDebuggable,
            "the manifest is not debuggable",
        ),
    });

    // lib_name: the meta-data names the library of every ABI.
    let libraries = libraries(names);
    checks.push(match &dumped.lib_name {
        None => Check::fail(
            CheckId::AndroidManifestLibName,
            "the NativeActivity has no android.app.lib_name meta-data",
        )
        .evidence(evidence()),
        Some(name) => {
            let file = format!("lib{name}.so");
            let mut problems: Vec<String> = Vec::new();
            if let Some(lib) = &expect.lib
                && lib != name
            {
                problems.push(format!("the app's library is lib{lib}.so"));
            }
            let abis: std::collections::BTreeSet<&str> =
                libraries.iter().map(|(abi, _)| abi.as_str()).collect();
            for abi in &abis {
                if !libraries
                    .iter()
                    .any(|(a, entry)| a == abi && entry.ends_with(&format!("/{file}")))
                {
                    problems.push(format!("{abi} has no {file}"));
                }
            }
            if problems.is_empty() {
                Check::pass(
                    CheckId::AndroidManifestLibName,
                    format!(
                        "android.app.lib_name is {name}, and every ABI ({}) has {file}",
                        abis.iter().copied().collect::<Vec<_>>().join(", ")
                    ),
                )
            } else {
                Check::fail(
                    CheckId::AndroidManifestLibName,
                    format!(
                        "android.app.lib_name is {name}, but {}: Android would fail to load the library (dlopen)",
                        problems.join("; ")
                    ),
                )
                .evidence(evidence())
            }
        }
    });

    // versionCode and versionName.
    let mut problems: Vec<String> = Vec::new();
    match dumped.version_code {
        None => problems.push("no versionCode".to_string()),
        Some(code) => {
            if code == 0 || code > MAX_VERSION_CODE {
                problems.push(format!(
                    "versionCode {code} is outside 1 to {MAX_VERSION_CODE}, Google Play's range"
                ));
            }
            if let Some(build) = expect.build
                && build != code
            {
                problems.push(format!("versionCode is {code}, [app] build is {build}"));
            }
        }
    }
    match (&dumped.version_name, &expect.version) {
        (None, _) => problems.push("no versionName".to_string()),
        (Some(name), Some(version)) if name != version => problems.push(format!(
            "versionName is {name}, the Cargo version is {version}"
        )),
        _ => {}
    }
    checks.push(if problems.is_empty() {
        Check::pass(
            CheckId::AndroidManifestVersion,
            format!(
                "versionCode {} and versionName {}",
                dumped.version_code.unwrap_or(0),
                dumped.version_name.as_deref().unwrap_or("")
            ),
        )
    } else {
        Check::fail(CheckId::AndroidManifestVersion, problems.join("; ")).evidence(evidence())
    });

    // Back: the predictive-back opt-out goes away at API 37.
    checks.push(if dumped.back_callback == Some(false) {
        Check::warn(
            CheckId::AndroidManifestBackOptout,
            "the app opts out of predictive back (android:enableOnBackInvokedCallback=\"false\", [android] back = \"key\"); Android ignores the opt-out for apps that target API 37",
        )
        .evidence(evidence())
    } else {
        Check::pass(
            CheckId::AndroidManifestBackOptout,
            "the app keeps Android's predictive back",
        )
    });

    // Dangerous permissions: the Data safety form must account for them.
    let dangerous: Vec<&str> = dumped
        .permissions
        .iter()
        .map(String::as_str)
        .filter(|name| {
            name.strip_prefix("android.permission.")
                .is_some_and(|short| DANGEROUS_PERMISSIONS.contains(&short))
        })
        .collect();
    checks.push(if dangerous.is_empty() {
        Check::pass(
            CheckId::AndroidPermissionsReview,
            format!(
                "no dangerous permissions ({})",
                if dumped.permissions.is_empty() {
                    "none declared".to_string()
                } else {
                    dumped.permissions.join(", ")
                }
            ),
        )
    } else {
        Check::warn(
            CheckId::AndroidPermissionsReview,
            format!(
                "the app asks for {}: the Play Console's Data safety form must declare what they collect",
                dangerous.join(", ")
            ),
        )
        .evidence(evidence())
    });

    checks.extend(abi_checks(&libraries, expect));
    checks
}

/// `android.so.abis`: arm64-v8a is there (Google Play requires 64-bit
/// ARM), every declared ABI is there, and x86_64 (emulators,
/// Chromebooks) is a WARN when missing.
pub fn abi_checks(libraries: &[(String, String)], expect: &Expect) -> Vec<Check> {
    let found: std::collections::BTreeSet<&str> =
        libraries.iter().map(|(abi, _)| abi.as_str()).collect();
    let listed = found.iter().copied().collect::<Vec<_>>().join(", ");
    let mut checks = Vec::new();
    let missing: Vec<&str> = expect
        .abis
        .iter()
        .map(|abi| abi.as_str())
        .filter(|abi| !found.contains(abi))
        .collect();
    if !found.contains("arm64-v8a") {
        checks.push(
            Check::fail(
                CheckId::AndroidSoAbis,
                format!(
                    "the bundle has no arm64-v8a library ({}); Google Play requires 64-bit ARM",
                    if listed.is_empty() {
                        "no libraries at all".to_string()
                    } else {
                        listed.clone()
                    }
                ),
            )
            .fix("Keep arm64-v8a in [android] abis.", &[]),
        );
    } else if !missing.is_empty() {
        checks.push(Check::fail(
            CheckId::AndroidSoAbis,
            format!(
                "[android] abis lists {} but the bundle has no library for it ({listed})",
                missing.join(", ")
            ),
        ));
    } else if !found.contains("x86_64") {
        checks.push(
            Check::warn(
                CheckId::AndroidSoAbis,
                format!("the bundle has {listed} but no x86_64: emulators on Intel hosts and Chromebooks cannot install it"),
            )
            .fix("Add \"x86_64\" to [android] abis.", &[]),
        );
    } else {
        checks.push(Check::pass(
            CheckId::AndroidSoAbis,
            format!("libraries for {listed}"),
        ));
    }
    checks
}

/// The ELF gates on one library extracted from the bundle: the machine
/// matches its ABI directory, NativeActivity's entry point is exported,
/// every `PT_LOAD` segment is aligned to 16 KB, and the agent bridge is
/// not in it.
pub fn library_checks(abi: &str, path: &Path, shown: &str) -> Vec<Check> {
    let facts = match elf::read(path) {
        Ok(facts) => facts,
        Err(error) => {
            return vec![
                Check::fail(
                    CheckId::AndroidSoAbis,
                    format!("{shown} is not a readable ELF library: {error}"),
                )
                .evidence(Evidence::file(path)),
            ];
        }
    };
    let mut checks = Vec::new();
    match elf::machine_for_abi(abi) {
        Some(machine) if machine == facts.machine => {}
        expected => checks.push(
            Check::fail(
                CheckId::AndroidSoAbis,
                match expected {
                    Some(machine) => format!(
                        "{shown} is {}, but lib/{abi}/ needs {}",
                        elf::machine_name(facts.machine),
                        elf::machine_name(machine)
                    ),
                    None => format!("{shown} sits under lib/{abi}/, an ABI icm does not know"),
                },
            )
            .evidence(Evidence::file(path)),
        ),
    }
    checks.push(if facts.exports(ENTRY_POINT) {
        Check::pass(
            CheckId::AndroidSoExport,
            format!(
                "{shown} exports {ENTRY_POINT} ({})",
                elf::machine_name(facts.machine)
            ),
        )
    } else {
        Check::fail(
            CheckId::AndroidSoExport,
            format!("{shown} does not export {ENTRY_POINT}, so NativeActivity cannot start it"),
        )
        .evidence(Evidence::file(path))
    });
    let align = facts.min_load_align();
    checks.push(if align >= 0x4000 {
        Check::pass(
            CheckId::AndroidSoAlign16k,
            format!("{shown}: every PT_LOAD segment is aligned to {align:#x}"),
        )
    } else {
        Check::fail(
            CheckId::AndroidSoAlign16k,
            format!("{shown}: a PT_LOAD segment is aligned to {align:#x}, below the 16 KB pages Google Play requires"),
        )
        .evidence(Evidence::file(path))
        .fix("Build with NDK r28 or newer (`icm doctor android --fix --yes`).", &[])
    });
    checks.push(match contains(path, AGENT_MARKER) {
        Ok(false) => Check::pass(
            CheckId::StoreNoAgentBridge,
            format!("{shown} has no agent bridge"),
        ),
        Ok(true) => Check::fail(
            CheckId::StoreNoAgentBridge,
            format!("{shown} contains the agent bridge (ICM_AGENT_BRIDGE_V1)"),
        )
        .evidence(Evidence::file(path)),
        Err(error) => Check::fail(
            CheckId::StoreNoAgentBridge,
            format!("cannot read {shown}: {error}"),
        ),
    });
    checks
}

/// Whether a file contains `needle`, read in blocks.
pub fn contains(path: &Path, needle: &[u8]) -> io::Result<bool> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0u8; 1 << 20];
    let mut carry: Vec<u8> = Vec::new();
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(false);
        }
        let mut window = std::mem::take(&mut carry);
        window.extend_from_slice(&buffer[..read]);
        if window.windows(needle.len()).any(|w| w == needle) {
            return Ok(true);
        }
        let keep = needle.len().saturating_sub(1).min(window.len());
        carry = window[window.len() - keep..].to_vec();
    }
}

/// `android.bundle.alignment` from `bundletool dump config`.
pub fn config_check(json: &str, dump: &Path) -> Check {
    match parse_config(json) {
        Some((true, Some(alignment))) if alignment == "PAGE_ALIGNMENT_16K" => Check::pass(
            CheckId::AndroidBundleAlignment,
            "uncompressed native libraries, aligned to 16 KB pages (PAGE_ALIGNMENT_16K)",
        ),
        Some((enabled, alignment)) => Check::fail(
            CheckId::AndroidBundleAlignment,
            format!(
                "the bundle's native libraries are {} with {} alignment; Google Play needs PAGE_ALIGNMENT_16K",
                if enabled { "uncompressed" } else { "compressed" },
                alignment.as_deref().unwrap_or("the default (4 KB)")
            ),
        )
        .evidence(Evidence::file(dump)),
        None => Check::fail(
            CheckId::AndroidBundleAlignment,
            "bundletool dump config shows no uncompressNativeLibraries setting (the default is 4 KB pages)",
        )
        .evidence(Evidence::file(dump)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `bundletool dump manifest` of the template, as bundletool 1.18.3
    /// prints it.
    const DUMPED: &str = r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" android:compileSdkVersion="36" android:compileSdkVersionCodename="16" android:versionCode="3" android:versionName="0.1.0" package="dev.accept.demo" platformBuildVersionCode="36" platformBuildVersionName="16">
  <uses-sdk android:minSdkVersion="26" android:targetSdkVersion="36"/>
  <uses-permission android:name="android.permission.INTERNET"/>
  <application android:allowBackup="true" android:extractNativeLibs="false" android:hasCode="false" android:label="Demo">
    <activity android:configChanges="0xd000ffff" android:exported="true" android:launchMode="2" android:name="android.app.NativeActivity">
      <meta-data android:name="android.app.lib_name" android:value="demo"/>
      <intent-filter>
        <action android:name="android.intent.action.MAIN"/>
        <category android:name="android.intent.category.LAUNCHER"/>
      </intent-filter>
    </activity>
  </application>
</manifest>
"#;

    const KEYTOOL_RSA: &str = "Alias name: upload\nCreation date: Oct 7, 2026\nEntry type: PrivateKeyEntry\nCertificate chain length: 1\nCertificate[1]:\nOwner: CN=test\nIssuer: CN=test\nSerial number: 9a9d04501a5e0b64\nCertificate fingerprints:\n\t SHA1: A6:F5:1E:1C:A0:F7:A0:9A:89:02:A0:C8:E7:16:FD:0D:AC:07:A4:BC\n\t SHA256: 09:05:5F:DE:21:70:CC:1F:3C:61:7B:50:A5:80:00:87:D3:23:08:82:2D:8F:B4:DB:34:64:16:F3:ED:92:44:69\nSignature algorithm name: SHA384withRSA\nSubject Public Key Algorithm: 2048-bit RSA key\nVersion: 3\n";

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    fn expect() -> Expect {
        Expect {
            version: Some("0.1.0".into()),
            build: Some(3),
            lib: Some("demo".into()),
            abis: vec![Abi::Arm64V8a, Abi::X86_64],
            target_sdk_floor: Some(36),
            native: true,
        }
    }

    fn bundle_names() -> Vec<String> {
        names(&[
            "BundleConfig.pb",
            "base/lib/arm64-v8a/libdemo.so",
            "base/lib/x86_64/libdemo.so",
            "base/assets/THIRD_PARTY_NOTICES.txt",
            "base/manifest/AndroidManifest.xml",
            "base/resources.pb",
        ])
    }

    fn statuses(checks: &[Check]) -> Vec<(String, crate::error::Status)> {
        checks
            .iter()
            .map(|check| (check.id().to_string(), check.status))
            .collect()
    }

    #[test]
    fn the_dumped_manifest_is_read() {
        let dumped = parse_manifest(DUMPED);
        assert_eq!(dumped.package.as_deref(), Some("dev.accept.demo"));
        assert_eq!(dumped.version_code, Some(3));
        assert_eq!(dumped.version_name.as_deref(), Some("0.1.0"));
        assert_eq!((dumped.min_sdk, dumped.target_sdk), (Some(26), Some(36)));
        assert_eq!(dumped.debuggable, None);
        assert_eq!(dumped.has_code, Some(false));
        assert_eq!(dumped.config_changes, Some(0xd000_ffff));
        assert_eq!(dumped.lib_name.as_deref(), Some("demo"));
        assert_eq!(dumped.permissions, ["android.permission.INTERNET"]);
        assert!(dumped.native_activity);
        // The full list for API 36 is exactly that mask.
        assert_eq!(config_changes_mask(36), 0xd000_ffff);
        assert_eq!(
            parse_config_changes("mcc|mnc|assetsPaths"),
            Some(0x8000_0003)
        );
        assert_eq!(parse_config_changes("1234"), Some(1234));
        assert_eq!(parse_config_changes("bogus"), None);
    }

    #[test]
    fn a_good_bundle_passes_every_gate() {
        let dumped = parse_manifest(DUMPED);
        let checks = manifest_checks(&dumped, &bundle_names(), &expect(), Path::new("m.xml"));
        let failed: Vec<_> = checks
            .iter()
            .filter(|check| check.status != crate::error::Status::Pass)
            .map(|check| format!("{}: {}", check.id(), check.error.detail))
            .collect();
        assert!(failed.is_empty(), "{failed:#?}");
        for id in [
            "android.manifest.target_sdk",
            "android.manifest.config_changes",
            "android.manifest.has_code",
            "android.manifest.debuggable",
            "android.manifest.lib_name",
            "android.manifest.version",
            "android.manifest.back_optout",
            "android.permissions.review",
            "android.so.abis",
        ] {
            assert!(checks.iter().any(|check| check.id() == id), "{id}");
        }
    }

    #[test]
    fn bad_manifests_fail_their_gates() {
        let xml = DUMPED
            .replace(
                "android:targetSdkVersion=\"36\"",
                "android:targetSdkVersion=\"34\"",
            )
            .replace("0xd000ffff", "0x40000fff")
            .replace(
                "android:hasCode=\"false\"",
                "android:debuggable=\"true\" android:enableOnBackInvokedCallback=\"false\"",
            )
            .replace("android:value=\"demo\"", "android:value=\"other\"")
            .replace("android:versionCode=\"3\"", "android:versionCode=\"4\"")
            .replace(
                "<uses-permission android:name=\"android.permission.INTERNET\"/>",
                "<uses-permission android:name=\"android.permission.CAMERA\"/>",
            );
        let dumped = parse_manifest(&xml);
        let checks = manifest_checks(&dumped, &bundle_names(), &expect(), Path::new("m.xml"));
        use crate::error::Status::{Fail, Pass, Warn};
        let got = statuses(&checks);
        for (id, status) in [
            ("android.manifest.target_sdk", Fail),
            ("android.manifest.config_changes", Fail),
            ("android.manifest.has_code", Fail),
            ("android.manifest.debuggable", Fail),
            ("android.manifest.lib_name", Fail),
            ("android.manifest.version", Fail),
            ("android.manifest.back_optout", Warn),
            ("android.permissions.review", Warn),
            ("android.so.abis", Pass),
        ] {
            assert!(
                got.contains(&(id.to_string(), status)),
                "{id} {status:?} not in {got:?}"
            );
        }
        let config = checks
            .iter()
            .find(|check| check.id() == "android.manifest.config_changes")
            .unwrap();
        assert!(
            config.error.detail.contains("grammaticalGender"),
            "{}",
            config.error.detail
        );
    }

    #[test]
    fn abis_need_arm64_and_warn_without_x86_64() {
        let only_x86 = libraries(&names(&["base/lib/x86_64/libdemo.so"]));
        assert_eq!(
            abi_checks(&only_x86, &Expect::default())[0].status,
            crate::error::Status::Fail
        );
        let only_arm = libraries(&names(&["base/lib/arm64-v8a/libdemo.so"]));
        assert_eq!(
            abi_checks(&only_arm, &Expect::default())[0].status,
            crate::error::Status::Warn
        );
        let declared = Expect {
            abis: vec![Abi::Arm64V8a, Abi::X86_64],
            ..Expect::default()
        };
        assert_eq!(
            abi_checks(&only_arm, &declared)[0].status,
            crate::error::Status::Fail
        );
        // APK entries have no base/ prefix; other files are not libraries.
        assert_eq!(
            libraries(&names(&[
                "lib/arm64-v8a/libapp.so",
                "lib/arm64-v8a/sub/x.so",
                "assets/lib/x86/readme.txt"
            ])),
            vec![(
                "arm64-v8a".to_string(),
                "lib/arm64-v8a/libapp.so".to_string()
            )]
        );
    }

    #[test]
    fn upload_keys_are_read() {
        let key = parse_keytool_list(KEYTOOL_RSA);
        assert_eq!(key.algorithm, Some(KeyAlgorithm::Rsa));
        assert_eq!(key.algorithm.unwrap().sigalg(), "SHA256withRSA");
        assert_eq!(
            key.sha256.as_deref(),
            Some(
                "09:05:5F:DE:21:70:CC:1F:3C:61:7B:50:A5:80:00:87:D3:23:08:82:2D:8F:B4:DB:34:64:16:F3:ED:92:44:69"
            )
        );
        assert_eq!(key.owner.as_deref(), Some("CN=test"));
        let ec = parse_keytool_list(
            "Signature algorithm name: SHA384withECDSA\nSubject Public Key Algorithm: 256-bit EC (secp256r1) key\n",
        );
        assert_eq!(ec.algorithm.unwrap().sigalg(), "SHA256withECDSA");

        assert!(
            key_failure("keytool error: java.io.IOException: keystore password was incorrect")
                .unwrap()
                .contains("password")
        );
        assert!(
            key_failure("keytool error: java.lang.Exception: Alias <nope> does not exist")
                .unwrap()
                .contains("alias")
        );
        assert!(key_failure("Cannot find environment variable: X").is_some());
        assert_eq!(key_failure("jar signed."), None);
    }

    #[test]
    fn signatures_are_read_from_jarsigner_and_keytool() {
        let signed = "sm        86 Fri Jan 01 00:00:00 CST 2010 base/resources.pb\n\n- Signed by \"CN=test\"\n    Digest algorithm: SHA-256\n\njar verified.\n\nWarning: \nThis jar contains entries whose signer certificate is self-signed.\nThis jar contains signatures that do not include a timestamp. Without a timestamp, users may not be able to validate this jar after any of the signer certificates expire (as early as 2027-10-07).\n";
        let verified = parse_jarsigner_verify(signed);
        assert!(verified.verified && !verified.unsigned && verified.no_timestamp);
        assert_eq!(verified.signers, ["CN=test"]);
        // An unsigned jar exits 0: only the text tells.
        let unsigned = parse_jarsigner_verify("\nno manifest.\n\njar is unsigned.\n");
        assert!(!unsigned.verified && unsigned.unsigned);

        assert_eq!(
            parse_printcert(KEYTOOL_RSA),
            parse_keytool_list(KEYTOOL_RSA).sha256
        );
        assert_eq!(parse_printcert("Not a signed jar file\n"), None);
    }

    #[test]
    fn the_bundle_config_is_checked() {
        let good = "{\n  \"bundletool\": {\"version\": \"1.18.3\"},\n  \"optimizations\": {\"uncompressNativeLibraries\": {\"enabled\": true, \"alignment\": \"PAGE_ALIGNMENT_16K\"}}\n}";
        assert_eq!(
            config_check(good, Path::new("c.json")).status,
            crate::error::Status::Pass
        );
        let default = "{\"optimizations\": {\"uncompressNativeLibraries\": {\"enabled\": true}}}";
        let check = config_check(default, Path::new("c.json"));
        assert_eq!(check.status, crate::error::Status::Fail);
        assert!(check.error.detail.contains("4 KB"));
        assert_eq!(
            config_check("{}", Path::new("c.json")).status,
            crate::error::Status::Fail
        );
        // What icm writes is what it checks for.
        assert_eq!(
            parse_config(BUNDLE_CONFIG),
            Some((true, Some("PAGE_ALIGNMENT_16K".to_string())))
        );
    }

    #[test]
    fn libraries_are_gated() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.so");
        std::fs::write(
            &good,
            elf::synthetic(elf::EM_AARCH64, 0x4000, &[ENTRY_POINT]),
        )
        .unwrap();
        let checks = library_checks("arm64-v8a", &good, "lib/arm64-v8a/libdemo.so");
        assert!(
            checks
                .iter()
                .all(|check| check.status == crate::error::Status::Pass),
            "{:?}",
            statuses(&checks)
        );

        let bad = dir.path().join("bad.so");
        let mut bytes = elf::synthetic(elf::EM_X86_64, 0x1000, &[]);
        bytes.extend_from_slice(AGENT_MARKER);
        std::fs::write(&bad, bytes).unwrap();
        let got = statuses(&library_checks("arm64-v8a", &bad, "bad"));
        use crate::error::Status::Fail;
        for id in [
            "android.so.abis",
            "android.so.export",
            "android.so.align16k",
            "store.no_agent_bridge",
        ] {
            assert!(got.contains(&(id.to_string(), Fail)), "{id}: {got:?}");
        }
    }

    #[test]
    fn markers_are_found_across_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big");
        let mut bytes = vec![0u8; (1 << 20) - 5];
        bytes.extend_from_slice(AGENT_MARKER);
        std::fs::write(&path, &bytes).unwrap();
        assert!(contains(&path, AGENT_MARKER).unwrap());
        std::fs::write(&path, vec![7u8; 3 << 20]).unwrap();
        assert!(!contains(&path, AGENT_MARKER).unwrap());
    }

    #[test]
    fn base_zip_has_the_bundle_layout() {
        let dir = tempfile::tempdir().unwrap();
        let proto = dir.path().join("proto");
        std::fs::create_dir_all(proto.join("res/mipmap-hdpi")).unwrap();
        std::fs::write(proto.join("AndroidManifest.xml"), b"m").unwrap();
        std::fs::write(proto.join("resources.pb"), b"r").unwrap();
        std::fs::write(proto.join("res/mipmap-hdpi/ic_launcher.png"), b"p").unwrap();
        let so = dir.path().join("lib.so");
        std::fs::write(&so, b"elf").unwrap();
        let notices = dir.path().join("n.txt");
        std::fs::write(&notices, b"n").unwrap();
        let entries = base_entries(
            &proto,
            &[(Abi::X86_64, so.clone()), (Abi::Arm64V8a, so)],
            "demo",
            &[
                (notices.clone(), "THIRD_PARTY_NOTICES.txt".to_string()),
                (notices, "THIRD_PARTY_NOTICES.txt".to_string()),
            ],
        )
        .unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "manifest/AndroidManifest.xml",
                "resources.pb",
                "res/mipmap-hdpi/ic_launcher.png",
                "lib/arm64-v8a/libdemo.so",
                "lib/x86_64/libdemo.so",
                NOTICES_ENTRY,
            ]
        );
        std::fs::remove_file(proto.join("resources.pb")).unwrap();
        assert!(base_entries(&proto, &[], "demo", &[]).is_err());
    }
}
