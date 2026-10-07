//! The simulator inventory (`xcrun simctl list -j ...`) and how icm picks
//! a runtime, a device type and a simulator (design §10.3 step 7,
//! Appendix C item 22).
//!
//! - Runtime: the newest available iOS runtime at or above `[ios] min_os`;
//!   `--runtime min` takes the lowest one (to test the minimum OS),
//!   `--runtime 18.3` a specific one.
//! - Device type: host.toml `[ios] simulator_type`, else the newest plain
//!   `iPhone <n>` the runtime supports (iPhone 17 with iOS 27).
//! - Simulator: `--sim`/`--device` (a name or UDID), else host.toml
//!   `simulator_udid`, else icm's managed one, `icm-<type>-ios-<version>`
//!   (e.g. `icm-iphone-17-ios-27.0`), created when missing. `--fresh`
//!   creates `icm-fresh-...`, deleted by `icm stop`. icm never creates,
//!   shuts down or deletes a simulator whose name does not start with
//!   `icm-` unless it was told to use it (and then only boots it).

use serde_json::Value;
use std::path::PathBuf;

/// The prefix of every simulator icm creates.
pub const MANAGED_PREFIX: &str = "icm-";

/// A simulator device type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceType {
    /// `iPhone 17`.
    pub name: String,
    /// `com.apple.CoreSimulator.SimDeviceType.iPhone-17`.
    pub identifier: String,
    /// `iPhone`, `iPad`, ...
    pub product_family: String,
    /// The `.simdevicetype` bundle.
    pub bundle_path: Option<PathBuf>,
}

/// A simulator runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Runtime {
    /// `com.apple.CoreSimulator.SimRuntime.iOS-27-0`.
    pub identifier: String,
    /// `27.0`.
    pub version: String,
    /// `iOS 27.0`.
    pub name: String,
    /// `iOS`, `tvOS`, ...
    pub platform: String,
    /// The runtime build, e.g. `24A434`.
    pub build: String,
    /// Whether it can be used.
    pub available: bool,
    /// The device types it supports.
    pub device_types: Vec<DeviceType>,
}

/// A simulator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// Its UDID.
    pub udid: String,
    /// Its name.
    pub name: String,
    /// `Booted`, `Shutdown`, `Booting`, ...
    pub state: String,
    /// Whether it can be used.
    pub available: bool,
    /// Its device type's identifier.
    pub device_type: Option<String>,
    /// Its runtime's identifier.
    pub runtime: String,
    /// The host directory that holds its data (and its `/tmp`).
    pub data_path: Option<PathBuf>,
}

impl Device {
    /// Whether icm created it (and may shut it down or delete it).
    pub fn is_managed(&self) -> bool {
        self.name.starts_with(MANAGED_PREFIX)
    }

    /// Whether it is booted.
    pub fn is_booted(&self) -> bool {
        self.state == "Booted"
    }
}

fn str_of(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn device_type_of(value: &Value) -> DeviceType {
    DeviceType {
        name: str_of(value, "name"),
        identifier: str_of(value, "identifier"),
        product_family: str_of(value, "productFamily"),
        bundle_path: value
            .get("bundlePath")
            .and_then(Value::as_str)
            .map(PathBuf::from),
    }
}

/// Parses `simctl list -j runtimes`.
pub fn parse_runtimes(json: &str) -> Result<Vec<Runtime>, String> {
    let value: Value =
        serde_json::from_str(json).map_err(|error| format!("simctl runtimes: {error}"))?;
    let runtimes = value
        .get("runtimes")
        .and_then(Value::as_array)
        .ok_or("simctl runtimes: no `runtimes` array")?;
    Ok(runtimes
        .iter()
        .map(|runtime| {
            let identifier = str_of(runtime, "identifier");
            let platform = match str_of(runtime, "platform") {
                platform if !platform.is_empty() => platform,
                _ if identifier.contains(".iOS-") => "iOS".to_string(),
                _ => String::new(),
            };
            Runtime {
                version: str_of(runtime, "version"),
                name: str_of(runtime, "name"),
                build: str_of(runtime, "buildversion"),
                available: runtime
                    .get("isAvailable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                device_types: runtime
                    .get("supportedDeviceTypes")
                    .and_then(Value::as_array)
                    .map(|types| types.iter().map(device_type_of).collect())
                    .unwrap_or_default(),
                platform,
                identifier,
            }
        })
        .collect())
}

/// Parses `simctl list -j devicetypes`.
pub fn parse_device_types(json: &str) -> Result<Vec<DeviceType>, String> {
    let value: Value =
        serde_json::from_str(json).map_err(|error| format!("simctl devicetypes: {error}"))?;
    Ok(value
        .get("devicetypes")
        .and_then(Value::as_array)
        .ok_or("simctl devicetypes: no `devicetypes` array")?
        .iter()
        .map(device_type_of)
        .collect())
}

/// Parses `simctl list -j devices`.
pub fn parse_devices(json: &str) -> Result<Vec<Device>, String> {
    let value: Value =
        serde_json::from_str(json).map_err(|error| format!("simctl devices: {error}"))?;
    let by_runtime = value
        .get("devices")
        .and_then(Value::as_object)
        .ok_or("simctl devices: no `devices` object")?;
    let mut devices = Vec::new();
    for (runtime, list) in by_runtime {
        for device in list.as_array().into_iter().flatten() {
            devices.push(Device {
                udid: str_of(device, "udid"),
                name: str_of(device, "name"),
                state: str_of(device, "state"),
                available: device
                    .get("isAvailable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                device_type: device
                    .get("deviceTypeIdentifier")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                runtime: runtime.clone(),
                data_path: device
                    .get("dataPath")
                    .and_then(Value::as_str)
                    .map(PathBuf::from),
            });
        }
    }
    Ok(devices)
}

/// The numeric parts of a version (`18.3.1` gives `[18, 3, 1]`).
pub fn version_parts(version: &str) -> Vec<u32> {
    version
        .split('.')
        .map_while(|part| part.trim().parse().ok())
        .collect()
}

/// Whether `version` is at or above `floor` (`18.3` ≥ `16.0`).
pub fn at_least(version: &str, floor: &str) -> bool {
    let mut a = version_parts(version);
    let mut b = version_parts(floor);
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    a >= b
}

/// Which runtime `--runtime` asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeChoice {
    /// The newest one (the default).
    Newest,
    /// The lowest one that satisfies `[ios] min_os`.
    Min,
    /// A version, e.g. `18.3`.
    Version(String),
}

impl RuntimeChoice {
    /// Parses `newest`, `min` or a version.
    pub fn parse(text: Option<&str>) -> Result<RuntimeChoice, String> {
        match text.map(str::trim) {
            None | Some("") | Some("newest") | Some("latest") => Ok(RuntimeChoice::Newest),
            Some("min") | Some("minimum") | Some("lowest") => Ok(RuntimeChoice::Min),
            Some(version) if !version_parts(version).is_empty() => {
                Ok(RuntimeChoice::Version(version.to_string()))
            }
            Some(other) => Err(format!(
                "--runtime `{other}` is not `newest`, `min` or a version such as 18.3"
            )),
        }
    }
}

/// Picks the iOS runtime. The error lists what is installed.
pub fn choose_runtime<'a>(
    runtimes: &'a [Runtime],
    min_os: &str,
    choice: &RuntimeChoice,
) -> Result<&'a Runtime, String> {
    let mut candidates: Vec<&Runtime> = runtimes
        .iter()
        .filter(|runtime| runtime.available && runtime.platform == "iOS")
        .filter(|runtime| at_least(&runtime.version, min_os))
        .collect();
    candidates.sort_by_key(|runtime| version_parts(&runtime.version));

    let installed = || {
        let names: Vec<&str> = runtimes
            .iter()
            .filter(|runtime| runtime.available && runtime.platform == "iOS")
            .map(|runtime| runtime.name.as_str())
            .collect();
        if names.is_empty() {
            "no iOS simulator runtime is installed".to_string()
        } else {
            format!("installed: {}", names.join(", "))
        }
    };

    let chosen = match choice {
        RuntimeChoice::Newest => candidates.last().copied(),
        RuntimeChoice::Min => candidates.first().copied(),
        RuntimeChoice::Version(version) => {
            let wanted = version_parts(version);
            candidates
                .iter()
                .rev()
                .find(|runtime| version_parts(&runtime.version).starts_with(&wanted))
                .copied()
        }
    };

    chosen.ok_or_else(|| match choice {
        RuntimeChoice::Version(version) => format!(
            "no available iOS {version} simulator runtime at or above [ios] min_os {min_os} ({})",
            installed()
        ),
        _ => format!(
            "no available iOS simulator runtime at or above [ios] min_os {min_os} ({})",
            installed()
        ),
    })
}

/// The number in a plain `iPhone <n>` name.
fn plain_iphone_number(name: &str) -> Option<u32> {
    name.strip_prefix("iPhone ")?.trim().parse().ok()
}

/// Picks the device type: `preferred` (a name or identifier) when the
/// runtime supports it, else the newest plain `iPhone <n>`, else the last
/// iPhone the runtime lists.
pub fn choose_device_type<'a>(
    runtime: &'a Runtime,
    preferred: Option<&str>,
) -> Result<&'a DeviceType, String> {
    if let Some(preferred) = preferred.filter(|p| !p.trim().is_empty()) {
        return runtime
            .device_types
            .iter()
            .find(|t| t.name == preferred || t.identifier == preferred)
            .ok_or_else(|| {
                format!(
                    "{} does not support the device type `{preferred}` (host.toml [ios] simulator_type)",
                    runtime.name
                )
            });
    }

    let iphones: Vec<&DeviceType> = runtime
        .device_types
        .iter()
        .filter(|t| t.product_family == "iPhone" || t.name.starts_with("iPhone"))
        .collect();
    iphones
        .iter()
        .filter_map(|t| plain_iphone_number(&t.name).map(|n| (n, *t)))
        .max_by_key(|(n, _)| *n)
        .map(|(_, t)| t)
        .or_else(|| iphones.last().copied())
        .ok_or_else(|| format!("{} supports no iPhone device type", runtime.name))
}

/// `iPhone 17` gives `iphone-17`.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// The managed simulator's name, e.g. `icm-iphone-17-ios-27.0`.
pub fn managed_name(device_type: &DeviceType, runtime: &Runtime) -> String {
    format!(
        "{MANAGED_PREFIX}{}-ios-{}",
        slug(&device_type.name),
        runtime.version
    )
}

/// A simulator by UDID or exact name (an available one first).
pub fn find_device<'a>(devices: &'a [Device], selector: &str) -> Option<&'a Device> {
    let selector = selector.trim();
    devices
        .iter()
        .find(|d| d.udid.eq_ignore_ascii_case(selector))
        .or_else(|| {
            devices
                .iter()
                .filter(|d| d.name == selector)
                .max_by_key(|d| (d.available, d.is_booted()))
        })
}

/// The managed simulator for a type and runtime, if it exists.
pub fn find_managed<'a>(
    devices: &'a [Device],
    device_type: &DeviceType,
    runtime: &Runtime,
) -> Option<&'a Device> {
    let name = managed_name(device_type, runtime);
    devices
        .iter()
        .filter(|d| d.name == name && d.runtime == runtime.identifier && d.available)
        .max_by_key(|d| d.is_booted())
}

/// Where a file the app writes at `path` really lands: the simulator maps
/// `/tmp` and `/private/tmp` to its own `<data>/tmp`, so `simctl launch
/// --stdout=/tmp/x` writes `<data>/tmp/x` on the host.
pub fn sim_visible_path(path: &std::path::Path, data_path: Option<&std::path::Path>) -> PathBuf {
    let Some(data) = data_path else {
        return path.to_path_buf();
    };
    let canonical = path
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .and_then(|parent| path.file_name().map(|name| parent.join(name)))
        .unwrap_or_else(|| path.to_path_buf());
    for prefix in ["/private/tmp", "/tmp"] {
        if let Ok(rest) = canonical.strip_prefix(prefix) {
            return data.join("tmp").join(rest);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn runtimes() -> Vec<Runtime> {
        let iphone = |name: &str| {
            json!({"name": name, "identifier": format!("com.apple.CoreSimulator.SimDeviceType.{}", name.replace(' ', "-")),
                   "productFamily": "iPhone", "bundlePath": format!("/Library/{name}.simdevicetype")})
        };
        let json = json!({"runtimes": [
            {"isAvailable": true, "version": "18.1", "buildversion": "22B81", "platform": "iOS",
             "identifier": "com.apple.CoreSimulator.SimRuntime.iOS-18-1", "name": "iOS 18.1",
             "supportedDeviceTypes": [iphone("iPhone 16 Pro"), iphone("iPhone 16"), iphone("iPhone SE (3rd generation)")]},
            {"isAvailable": true, "version": "27.0", "buildversion": "24A434", "platform": "iOS",
             "identifier": "com.apple.CoreSimulator.SimRuntime.iOS-27-0", "name": "iOS 27.0",
             "supportedDeviceTypes": [iphone("iPhone 18 Pro"), iphone("iPhone 17e"), iphone("iPhone Air"), iphone("iPhone 17"), iphone("iPhone 16")]},
            {"isAvailable": true, "version": "18.3", "buildversion": "22D8075", "platform": "iOS",
             "identifier": "com.apple.CoreSimulator.SimRuntime.iOS-18-3", "name": "iOS 18.3",
             "supportedDeviceTypes": [iphone("iPhone 16")]},
            {"isAvailable": true, "version": "27.0", "platform": "watchOS",
             "identifier": "com.apple.CoreSimulator.SimRuntime.watchOS-27-0", "name": "watchOS 27.0",
             "supportedDeviceTypes": []},
            {"isAvailable": false, "version": "28.0", "platform": "iOS",
             "identifier": "com.apple.CoreSimulator.SimRuntime.iOS-28-0", "name": "iOS 28.0",
             "supportedDeviceTypes": []}
        ]});
        parse_runtimes(&json.to_string()).unwrap()
    }

    #[test]
    fn runtimes_are_chosen_by_min_os() {
        let runtimes = runtimes();
        let newest = choose_runtime(&runtimes, "16.0", &RuntimeChoice::Newest).unwrap();
        assert_eq!(newest.version, "27.0");
        let min = choose_runtime(&runtimes, "16.0", &RuntimeChoice::Min).unwrap();
        assert_eq!(min.version, "18.1");
        let min = choose_runtime(&runtimes, "18.2", &RuntimeChoice::Min).unwrap();
        assert_eq!(min.version, "18.3");
        let exact = choose_runtime(&runtimes, "16.0", &RuntimeChoice::Version("18.3".into()));
        assert_eq!(exact.unwrap().version, "18.3");
        let error =
            choose_runtime(&runtimes, "16.0", &RuntimeChoice::Version("19".into())).unwrap_err();
        assert!(
            error.contains("installed: iOS 18.1, iOS 27.0, iOS 18.3"),
            "{error}"
        );
        assert!(choose_runtime(&runtimes, "30.0", &RuntimeChoice::Newest).is_err());

        assert_eq!(RuntimeChoice::parse(None), Ok(RuntimeChoice::Newest));
        assert_eq!(RuntimeChoice::parse(Some("min")), Ok(RuntimeChoice::Min));
        assert_eq!(
            RuntimeChoice::parse(Some("18.3")),
            Ok(RuntimeChoice::Version("18.3".into()))
        );
        assert!(RuntimeChoice::parse(Some("oldest-ish")).is_err());
    }

    #[test]
    fn device_types_prefer_the_newest_plain_iphone() {
        let runtimes = runtimes();
        let ios27 = choose_runtime(&runtimes, "16.0", &RuntimeChoice::Newest).unwrap();
        assert_eq!(choose_device_type(ios27, None).unwrap().name, "iPhone 17");
        let ios18 = choose_runtime(&runtimes, "16.0", &RuntimeChoice::Min).unwrap();
        assert_eq!(choose_device_type(ios18, None).unwrap().name, "iPhone 16");
        assert_eq!(
            choose_device_type(ios27, Some("iPhone Air")).unwrap().name,
            "iPhone Air"
        );
        assert!(choose_device_type(ios27, Some("iPhone 3G")).is_err());

        let device_type = choose_device_type(ios27, None).unwrap();
        assert_eq!(managed_name(device_type, ios27), "icm-iphone-17-ios-27.0");
        assert_eq!(
            slug("iPhone SE (3rd generation)"),
            "iphone-se-3rd-generation"
        );
    }

    #[test]
    fn devices_are_found_by_udid_or_name() {
        let json = json!({"devices": {
            "com.apple.CoreSimulator.SimRuntime.iOS-27-0": [
                {"udid": "AAA", "name": "icm-iphone-17-ios-27.0", "state": "Shutdown", "isAvailable": true,
                 "deviceTypeIdentifier": "com.apple.CoreSimulator.SimDeviceType.iPhone-17", "dataPath": "/d/AAA/data"},
                {"udid": "BBB", "name": "iPhone 17", "state": "Booted", "isAvailable": true}
            ],
            "com.apple.CoreSimulator.SimRuntime.iOS-18-1": [
                {"udid": "CCC", "name": "iPhone 16", "state": "Shutdown", "isAvailable": true}
            ]
        }});
        let devices = parse_devices(&json.to_string()).unwrap();
        assert_eq!(devices.len(), 3);
        assert_eq!(find_device(&devices, "bbb").unwrap().name, "iPhone 17");
        assert!(find_device(&devices, "iPhone 17").unwrap().is_booted());
        assert!(find_device(&devices, "nope").is_none());

        let runtimes = runtimes();
        let ios27 = choose_runtime(&runtimes, "16.0", &RuntimeChoice::Newest).unwrap();
        let device_type = choose_device_type(ios27, None).unwrap();
        let managed = find_managed(&devices, device_type, ios27).unwrap();
        assert_eq!(managed.udid, "AAA");
        assert!(managed.is_managed());
        assert_eq!(
            managed.data_path.as_deref(),
            Some(std::path::Path::new("/d/AAA/data"))
        );
        assert!(!find_device(&devices, "BBB").unwrap().is_managed());
    }

    #[test]
    fn tmp_paths_land_in_the_simulator_data() {
        let data = std::path::Path::new("/d/AAA/data");
        assert_eq!(
            sim_visible_path(std::path::Path::new("/tmp/x/app.stderr"), Some(data)),
            PathBuf::from("/d/AAA/data/tmp/x/app.stderr")
        );
        assert_eq!(
            sim_visible_path(
                std::path::Path::new("/private/tmp/x/app.stderr"),
                Some(data)
            ),
            PathBuf::from("/d/AAA/data/tmp/x/app.stderr")
        );
        let home = std::path::Path::new("/Users/someone/app/target/icm/app.stderr");
        assert_eq!(sim_visible_path(home, Some(data)), home);
        assert_eq!(
            sim_visible_path(std::path::Path::new("/tmp/y"), None),
            PathBuf::from("/tmp/y")
        );
    }
}
