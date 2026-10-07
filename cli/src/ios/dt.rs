//! The `DT*` provenance keys of a device or App Store Info.plist (design
//! §9.1), read from the selected Xcode at build time, never hard-coded:
//! App Store Connect checks them against the SDK the binary was linked
//! with (ITMS-90534 and friends).
//!
//! | Key | Read from |
//! |---|---|
//! | `DTSDKBuild`, `DTPlatformBuild` | `xcodebuild -version -sdk iphoneos` `ProductBuildVersion` |
//! | `DTPlatformVersion`, `DTSDKName` | its `PlatformVersion` / `SDKVersion` (`iphoneos<SDKVersion>`) |
//! | `DTPlatformName` | `iphoneos` |
//! | `DTXcode` | `<developer dir>/../Info.plist` `DTXcode` |
//! | `DTXcodeBuild` | `xcodebuild -version` `Build version` |
//! | `DTCompiler` | `<developer dir>/Platforms/iPhoneOS.platform/Info.plist` `DefaultProperties.DEFAULT_COMPILER` |
//! | `BuildMachineOSBuild` | `sw_vers -buildVersion` |

use crate::catalogue::CheckId;
use crate::context::Ctx;
use crate::error::{Evidence, IcmError, Result};
use crate::process::Cmd;
use crate::tools::Xcode;
use serde_json::{Map, Value, json};
use std::time::Duration;

/// The keys, as the plist gets them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DtKeys {
    /// `DTSDKBuild` (`24A430`).
    pub sdk_build: String,
    /// `DTPlatformBuild`.
    pub platform_build: String,
    /// `DTPlatformVersion` (`27.0`).
    pub platform_version: String,
    /// `DTSDKName` (`iphoneos27.0`).
    pub sdk_name: String,
    /// `DTXcode` (`2700`).
    pub xcode: String,
    /// `DTXcodeBuild` (`27A266a`).
    pub xcode_build: String,
    /// `DTCompiler`.
    pub compiler: String,
    /// `BuildMachineOSBuild`.
    pub machine_os_build: String,
}

/// The names of the keys, in the order the gates list them.
pub const KEYS: &[&str] = &[
    "BuildMachineOSBuild",
    "DTCompiler",
    "DTPlatformBuild",
    "DTPlatformName",
    "DTPlatformVersion",
    "DTSDKBuild",
    "DTSDKName",
    "DTXcode",
    "DTXcodeBuild",
];

impl DtKeys {
    /// The keys as plist entries.
    pub fn to_map(&self) -> Map<String, Value> {
        let mut map = Map::new();
        for (key, value) in [
            ("BuildMachineOSBuild", &self.machine_os_build),
            ("DTCompiler", &self.compiler),
            ("DTPlatformBuild", &self.platform_build),
            ("DTPlatformName", &"iphoneos".to_string()),
            ("DTPlatformVersion", &self.platform_version),
            ("DTSDKBuild", &self.sdk_build),
            ("DTSDKName", &self.sdk_name),
            ("DTXcode", &self.xcode),
            ("DTXcodeBuild", &self.xcode_build),
        ] {
            let _ = map.insert(key.to_string(), json!(value));
        }
        map
    }

    /// The SDK version (`27.0`), from `DTSDKName`.
    pub fn sdk_version(&self) -> &str {
        self.sdk_name.trim_start_matches("iphoneos")
    }
}

/// The SDK version a plist's `DTSDKName` names (`iphoneos27.0` → `27.0`).
pub fn sdk_version_of(plist: &Map<String, Value>) -> Option<&str> {
    plist
        .get("DTSDKName")
        .and_then(Value::as_str)
        .map(|name| name.trim_start_matches("iphoneos"))
}

/// The keys a plist lacks, and those that differ from `expected` (`key:
/// plist value, expected value`).
pub fn compare(plist: &Map<String, Value>, expected: &DtKeys) -> (Vec<String>, Vec<String>) {
    let wanted = expected.to_map();
    let mut missing = Vec::new();
    let mut differ = Vec::new();
    for key in KEYS {
        match (plist.get(*key), wanted.get(*key)) {
            (None, _) => missing.push((*key).to_string()),
            (Some(have), Some(want)) if have != want => {
                differ.push(format!("{key} is {have}, this Xcode gives {want}"));
            }
            _ => {}
        }
    }
    (missing, differ)
}

/// `Key: value` lines of `xcodebuild -version -sdk iphoneos`.
pub fn parse_sdk_info(text: &str) -> Map<String, Value> {
    let mut map = Map::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(": ") {
            let key = key.trim();
            if !key.is_empty() && !key.contains(' ') {
                let _ = map.insert(key.to_string(), json!(value.trim()));
            }
        }
    }
    map
}

fn xcode_error(xcode: &Xcode, detail: String) -> IcmError {
    IcmError::new(CheckId::EnvXcodeMissing, detail)
        .evidence(Evidence::file(&xcode.developer_dir))
        .fix(
            "The owner reinstalls or reselects Xcode (`sudo xcode-select -s /Applications/Xcode.app`); its iOS platform must be installed (Xcode > Settings > Components).",
            &[],
        )
}

/// Reads the keys from the selected Xcode.
pub fn read(ctx: &Ctx, xcode: &Xcode) -> Result<DtKeys> {
    let outcome = ctx.probe(
        &Cmd::tool("xcodebuild")
            .args(["-version", "-sdk", "iphoneos"])
            .env("DEVELOPER_DIR", &xcode.developer_dir)
            .timeout(Duration::from_secs(60)),
    )?;
    let info = parse_sdk_info(&outcome.stdout_text());
    let field = |key: &str| info.get(key).and_then(Value::as_str).map(str::to_string);
    let (Some(sdk_version), Some(build)) = (field("SDKVersion"), field("ProductBuildVersion"))
    else {
        return Err(xcode_error(
            xcode,
            format!(
                "`xcodebuild -version -sdk iphoneos` did not name the iOS SDK ({}): is the iOS platform installed?",
                outcome.stderr_tail(2).trim()
            ),
        ));
    };
    let platform_version = field("PlatformVersion").unwrap_or_else(|| sdk_version.clone());

    let xcode_plist = crate::platform::ios_sim::bundle::read_plist(ctx, &xcode.info_plist())
        .map_err(|error| xcode_error(xcode, error.detail))?;
    let dt_xcode = xcode_plist
        .get("DTXcode")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            xcode_error(
                xcode,
                format!(
                    "{} has no DTXcode",
                    crate::paths::display(&xcode.info_plist())
                ),
            )
        })?;

    let platform = xcode
        .developer_dir
        .join("Platforms/iPhoneOS.platform/Info.plist");
    let compiler = crate::platform::ios_sim::bundle::read_plist(ctx, &platform)
        .ok()
        .and_then(|plist| {
            plist
                .get("DefaultProperties")?
                .get("DEFAULT_COMPILER")?
                .as_str()
                .map(str::to_string)
        })
        .ok_or_else(|| {
            xcode_error(
                xcode,
                format!(
                    "{} has no DefaultProperties.DEFAULT_COMPILER",
                    crate::paths::display(&platform)
                ),
            )
        })?;

    let sw_vers = ctx.probe(
        &Cmd::tool("sw_vers")
            .arg("-buildVersion")
            .timeout(Duration::from_secs(20)),
    )?;
    let machine_os_build = sw_vers.stdout_text().trim().to_string();
    if machine_os_build.is_empty() {
        return Err(IcmError::new(
            CheckId::EnvUnsupportedHost,
            "sw_vers -buildVersion printed nothing",
        ));
    }

    Ok(DtKeys {
        sdk_build: build.clone(),
        platform_build: build,
        platform_version,
        sdk_name: format!("iphoneos{sdk_version}"),
        xcode: dt_xcode,
        xcode_build: xcode.build.clone(),
        compiler,
        machine_os_build,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DtKeys {
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
    fn sdk_info_is_parsed() {
        let text = "iPhoneOS27.0.sdk - iOS 27.0 (iphoneos27.0)\nSDKVersion: 27.0\nPath: /A/iPhoneOS27.0.sdk\nPlatformVersion: 27.0\nProductBuildVersion: 24A430\nProductName: iPhone OS\n";
        let info = parse_sdk_info(text);
        assert_eq!(info["SDKVersion"], "27.0");
        assert_eq!(info["ProductBuildVersion"], "24A430");
        assert_eq!(info.len(), 5, "the heading line is not a key: {info:?}");
    }

    #[test]
    fn keys_are_compared() {
        let keys = sample();
        let mut plist = keys.to_map();
        assert_eq!(compare(&plist, &keys), (vec![], vec![]));
        assert_eq!(sdk_version_of(&plist), Some("27.0"));
        assert_eq!(keys.sdk_version(), "27.0");
        let _ = plist.remove("DTXcode");
        let _ = plist.insert("DTXcodeBuild".into(), json!("26A100"));
        let (missing, differ) = compare(&plist, &keys);
        assert_eq!(missing, ["DTXcode"]);
        assert_eq!(differ.len(), 1);
        assert!(differ[0].starts_with("DTXcodeBuild is \"26A100\""));
    }
}
