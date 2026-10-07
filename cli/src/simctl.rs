//! Reading `xcrun simctl list -j` (runtimes, device types, devices) and
//! choosing the managed simulator's device type and runtime (design §10.3
//! step 7: the newest "iPhone <n>" and the newest runtime at or above
//! `[ios] min_os`).

use serde::Deserialize;
use std::collections::BTreeMap;

/// A simulator runtime.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Runtime {
    /// e.g. `com.apple.CoreSimulator.SimRuntime.iOS-27-0`.
    pub identifier: String,
    /// e.g. `27.0`.
    pub version: String,
    /// e.g. `iOS 27.0`.
    #[serde(default)]
    pub name: String,
    /// e.g. `iOS` (missing in older Xcodes).
    #[serde(default)]
    pub platform: Option<String>,
    /// e.g. `24A434`.
    #[serde(default)]
    pub buildversion: String,
    /// Whether it can be used.
    #[serde(default)]
    pub is_available: bool,
    /// The device types it runs.
    #[serde(default)]
    pub supported_device_types: Vec<DeviceType>,
}

impl Runtime {
    /// Whether it is an iOS runtime.
    pub fn is_ios(&self) -> bool {
        match &self.platform {
            Some(platform) => platform == "iOS",
            None => self.identifier.contains(".SimRuntime.iOS-"),
        }
    }
}

/// A simulator device type.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceType {
    /// e.g. `iPhone 17`.
    pub name: String,
    /// e.g. `com.apple.CoreSimulator.SimDeviceType.iPhone-17`.
    pub identifier: String,
    /// e.g. `iPhone`.
    #[serde(default)]
    pub product_family: Option<String>,
}

/// A simulator.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    /// Its UDID.
    pub udid: String,
    /// Its name.
    pub name: String,
    /// `Booted`, `Shutdown`, ...
    #[serde(default)]
    pub state: String,
    /// Whether it can be used.
    #[serde(default)]
    pub is_available: bool,
    /// Its device type.
    #[serde(default)]
    pub device_type_identifier: Option<String>,
    /// The runtime it belongs to (filled in from the listing's key).
    #[serde(skip)]
    pub runtime: String,
}

#[derive(Deserialize)]
struct Runtimes {
    runtimes: Vec<Runtime>,
}

#[derive(Deserialize)]
struct Devices {
    devices: BTreeMap<String, Vec<Device>>,
}

/// Parses `simctl list -j runtimes`.
pub fn parse_runtimes(json: &str) -> Result<Vec<Runtime>, String> {
    serde_json::from_str::<Runtimes>(json)
        .map(|r| r.runtimes)
        .map_err(|error| format!("cannot read simctl's runtimes: {error}"))
}

/// Parses `simctl list -j devices`.
pub fn parse_devices(json: &str) -> Result<Vec<Device>, String> {
    let listing: Devices = serde_json::from_str(json)
        .map_err(|error| format!("cannot read simctl's devices: {error}"))?;
    Ok(listing
        .devices
        .into_iter()
        .flat_map(|(runtime, devices)| {
            devices.into_iter().map(move |mut device| {
                device.runtime = runtime.clone();
                device
            })
        })
        .collect())
}

/// `16.0` → (16, 0); `18.3.1` → (18, 3).
pub fn version_key(version: &str) -> (u32, u32) {
    let mut parts = version
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

/// The newest available iOS runtime at or above `min_os`.
pub fn newest_runtime<'a>(runtimes: &'a [Runtime], min_os: &str) -> Option<&'a Runtime> {
    let floor = version_key(min_os);
    runtimes
        .iter()
        .filter(|runtime| runtime.is_ios() && runtime.is_available)
        .filter(|runtime| version_key(&runtime.version) >= floor)
        .max_by_key(|runtime| version_key(&runtime.version))
}

/// The managed simulator's device type: `wanted` (host.toml
/// `simulator_type`) when the runtime supports it, else the newest plain
/// `iPhone <n>`, else any iPhone.
pub fn choose_device_type<'a>(
    runtime: &'a Runtime,
    wanted: Option<&str>,
) -> Option<&'a DeviceType> {
    let types = &runtime.supported_device_types;
    if let Some(wanted) = wanted.filter(|w| !w.trim().is_empty()) {
        return types
            .iter()
            .find(|t| t.name == wanted || t.identifier == wanted);
    }
    let number =
        |t: &DeviceType| -> Option<u32> { t.name.strip_prefix("iPhone ")?.parse::<u32>().ok() };
    types
        .iter()
        .filter_map(|t| number(t).map(|n| (n, t)))
        .max_by_key(|(n, _)| *n)
        .map(|(_, t)| t)
        .or_else(|| types.iter().find(|t| t.name.starts_with("iPhone")))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const RUNTIMES: &str = r#"{"runtimes":[
      {"identifier":"com.apple.CoreSimulator.SimRuntime.iOS-18-1","version":"18.1","name":"iOS 18.1","platform":"iOS","buildversion":"22B81","isAvailable":true,
       "supportedDeviceTypes":[{"name":"iPhone 16","identifier":"com.apple.CoreSimulator.SimDeviceType.iPhone-16","productFamily":"iPhone"}]},
      {"identifier":"com.apple.CoreSimulator.SimRuntime.iOS-27-0","version":"27.0","name":"iOS 27.0","platform":"iOS","buildversion":"24A434","isAvailable":true,
       "supportedDeviceTypes":[
         {"name":"iPhone 18 Pro","identifier":"com.apple.CoreSimulator.SimDeviceType.iPhone-18-Pro","productFamily":"iPhone"},
         {"name":"iPhone 17e","identifier":"com.apple.CoreSimulator.SimDeviceType.iPhone-17e","productFamily":"iPhone"},
         {"name":"iPhone 17","identifier":"com.apple.CoreSimulator.SimDeviceType.iPhone-17","productFamily":"iPhone"},
         {"name":"iPhone 16","identifier":"com.apple.CoreSimulator.SimDeviceType.iPhone-16","productFamily":"iPhone"},
         {"name":"iPad (A16)","identifier":"com.apple.CoreSimulator.SimDeviceType.iPad-A16","productFamily":"iPad"}]},
      {"identifier":"com.apple.CoreSimulator.SimRuntime.watchOS-27-0","version":"27.0","name":"watchOS 27.0","platform":"watchOS","isAvailable":true,"supportedDeviceTypes":[]}
    ]}"#;

    #[test]
    fn the_newest_runtime_and_plain_iphone_win() {
        let runtimes = parse_runtimes(RUNTIMES).unwrap();
        let runtime = newest_runtime(&runtimes, "16.0").unwrap();
        assert_eq!(runtime.version, "27.0");
        assert!(runtime.is_ios());
        assert_eq!(choose_device_type(runtime, None).unwrap().name, "iPhone 17");
        assert_eq!(
            choose_device_type(runtime, Some("iPhone 18 Pro"))
                .unwrap()
                .identifier,
            "com.apple.CoreSimulator.SimDeviceType.iPhone-18-Pro"
        );
        assert!(choose_device_type(runtime, Some("iPhone 99")).is_none());
        assert!(newest_runtime(&runtimes, "28.0").is_none());
        assert_eq!(newest_runtime(&runtimes, "18.0").unwrap().version, "27.0");
        assert_eq!(version_key("18.3.1"), (18, 3));
    }

    #[test]
    fn devices_carry_their_runtime() {
        let json = r#"{"devices":{
          "com.apple.CoreSimulator.SimRuntime.iOS-27-0":[
            {"udid":"A","name":"icm-iphone-17-ios-27.0","state":"Booted","isAvailable":true,"deviceTypeIdentifier":"x"},
            {"udid":"B","name":"iPhone 17","state":"Shutdown","isAvailable":true}],
          "com.apple.CoreSimulator.SimRuntime.iOS-18-1":[]}}"#;
        let devices = parse_devices(json).unwrap();
        assert_eq!(devices.len(), 2);
        assert_eq!(
            devices[0].runtime,
            "com.apple.CoreSimulator.SimRuntime.iOS-27-0"
        );
        assert_eq!(devices[0].state, "Booted");
        assert!(parse_devices("{}").is_err());
    }
}
