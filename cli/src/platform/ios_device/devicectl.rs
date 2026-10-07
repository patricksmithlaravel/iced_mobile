//! `xcrun devicectl` JSON (`--json-output`): the devices CoreDevice knows
//! and the processes running on one.
//!
//! devicectl 642 (Xcode 27) lists simulators too (`reality: "simulated"`);
//! icm keeps the physical devices. It reads the `properties` dictionary and
//! falls back to the deprecated `hardwareProperties`, `deviceProperties` and
//! `connectionProperties` that older devicectl versions write.

use serde_json::Value;

/// One physical device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// Its UDID (what `--device` and profiles name).
    pub udid: String,
    /// Its CoreDevice identifier.
    pub identifier: String,
    /// The name the owner gave it.
    pub name: String,
    /// `iPhone 17 Pro`, ...
    pub model: String,
    /// `iOS`, `iPadOS`, ...
    pub platform: String,
    /// The OS version.
    pub os: String,
    /// Whether it is paired with this Mac.
    pub paired: bool,
    /// Whether it is reachable now.
    pub connected: bool,
    /// `wired`, `localNetwork`, ...
    pub transport: Option<String>,
    /// `Some(false)` when Developer Mode is off.
    pub developer_mode: Option<bool>,
}

impl Device {
    /// The result's `device` object.
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "kind": "device",
            "udid": self.udid,
            "name": self.name,
            "model": self.model,
            "os": self.os,
            "connected": self.connected,
            "transport": self.transport,
            "developer_mode": self.developer_mode,
            "managed": false,
        })
    }

    /// `"Jo's iPhone" (iPhone 17 Pro, iOS 27.0, 00008150-…)`.
    pub fn label(&self) -> String {
        format!(
            "\"{}\" ({}, {} {}, {})",
            self.name, self.model, self.platform, self.os, self.udid
        )
    }

    /// Whether a `--device` value names it.
    pub fn matches(&self, selector: &str) -> bool {
        let selector = selector.trim();
        [&self.udid, &self.identifier, &self.name]
            .iter()
            .any(|value| !value.is_empty() && value.eq_ignore_ascii_case(selector))
    }
}

fn path<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().try_fold(value, |value, key| value.get(*key))
}

fn field(device: &Value, new: &[&str], old: &[&str]) -> Option<String> {
    path(device, new)
        .or_else(|| path(device, old))
        .and_then(|value| match value {
            Value::String(text) => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
}

fn find_key<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => map
            .get(key)
            .or_else(|| map.values().find_map(|v| find_key(v, key))),
        Value::Array(items) => items.iter().find_map(|v| find_key(v, key)),
        _ => None,
    }
}

/// The physical devices in `devicectl list devices --json-output` JSON.
pub fn parse_devices(text: &str) -> Result<Vec<Device>, String> {
    let json: Value = serde_json::from_str(text)
        .map_err(|error| format!("devicectl's device list is not JSON: {error}"))?;
    let devices = json
        .pointer("/result/devices")
        .and_then(Value::as_array)
        .ok_or("devicectl's device list has no result.devices")?;
    let mut out = Vec::new();
    for device in devices {
        let reality = field(
            device,
            &["properties", "hardware", "reality"],
            &["hardwareProperties", "reality"],
        );
        let visibility = field(
            device,
            &["properties", "state", "visibilityClass"],
            &["visibilityClass"],
        );
        let simulated =
            reality.as_deref() == Some("simulated") || visibility.as_deref() == Some("simulators");
        if simulated {
            continue;
        }
        let state = field(
            device,
            &["properties", "connection", "state"],
            &["connectionProperties", "tunnelState"],
        )
        .unwrap_or_default();
        let pairing = field(
            device,
            &["properties", "connection", "pairingState"],
            &["connectionProperties", "pairingState"],
        )
        .unwrap_or_default();
        let developer_mode = find_key(device, "developerModeStatus")
            .and_then(Value::as_str)
            .map(|status| status != "disabled");
        out.push(Device {
            udid: field(
                device,
                &["properties", "hardware", "udid"],
                &["hardwareProperties", "udid"],
            )
            .unwrap_or_default(),
            identifier: field(device, &["identifier"], &["identifier"]).unwrap_or_default(),
            name: field(
                device,
                &["properties", "state", "name"],
                &["deviceProperties", "name"],
            )
            .unwrap_or_default(),
            model: field(
                device,
                &["properties", "hardware", "marketingName"],
                &["hardwareProperties", "marketingName"],
            )
            .unwrap_or_default(),
            platform: field(
                device,
                &["properties", "hardware", "platform"],
                &["hardwareProperties", "platform"],
            )
            .unwrap_or_default(),
            os: field(
                device,
                &["properties", "software", "osVersionNumber", "stringValue"],
                &["deviceProperties", "osVersionNumber"],
            )
            .unwrap_or_default(),
            paired: pairing == "paired",
            connected: matches!(state.as_str(), "connected" | "available" | "tunneled"),
            transport: field(
                device,
                &["properties", "connection", "transportType"],
                &["connectionProperties", "transportType"],
            ),
            developer_mode,
        });
    }
    Ok(out)
}

/// The pids of the running processes whose executable lies in
/// `<bundle>/<executable>` (`devicectl device info processes`).
pub fn app_pids(text: &str, bundle: &str, executable: &str) -> Vec<i64> {
    let Ok(json) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let suffix = format!("/{bundle}/{executable}");
    json.pointer("/result/runningProcesses")
        .and_then(Value::as_array)
        .map(|processes| {
            processes
                .iter()
                .filter(|p| {
                    p.get("executable")
                        .and_then(Value::as_str)
                        .is_some_and(|path| path.ends_with(&suffix))
                })
                .filter_map(|p| p.get("processIdentifier").and_then(Value::as_i64))
                .collect()
        })
        .unwrap_or_default()
}

/// A physical device entry as devicectl 642 writes it (icm's tests and
/// fake tools).
pub fn fixture_device(name: &str, udid: &str, connected: bool, developer_mode: bool) -> Value {
    serde_json::json!({
        "identifier": format!("{udid}-coredevice"),
        "deviceProperties": {"developerModeStatus": if developer_mode { "enabled" } else { "disabled" }},
        "properties": {
            "connection": {
                "pairingState": "paired",
                "state": if connected { "connected" } else { "disconnected" },
                "transportType": "wired"
            },
            "hardware": {
                "deviceType": "iPhone", "marketingName": "iPhone 17 Pro", "platform": "iOS",
                "productType": "iPhone18,1", "reality": "physical", "udid": udid
            },
            "software": {"osVersionNumber": {"stringValue": "27.0"}},
            "state": {"bootState": "booted", "name": name}
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn simulators_are_left_out_and_physical_devices_read() {
        let list = json!({"info": {"outcome": "success"}, "result": {"devices": [
            {"identifier": "SIM", "properties": {
                "connection": {"pairingState": "paired", "state": "disconnected", "transportType": "sameMachine"},
                "hardware": {"marketingName": "iPhone 17", "platform": "iOS", "reality": "simulated", "udid": "FC93DA9A"},
                "state": {"name": "icm-iphone-17-ios-27.0", "visibilityClass": "simulators"}}},
            fixture_device("Jo's iPhone", "00008150-001A2B3C4D5E6F70", true, true),
            fixture_device("Old iPad", "00008020-0011223344556677", false, false),
            {"identifier": "LEGACY", "hardwareProperties": {"udid": "00008110-000AAAA", "reality": "physical", "marketingName": "iPhone 15", "platform": "iOS"},
             "deviceProperties": {"name": "Legacy", "osVersionNumber": "18.3", "developerModeStatus": "enabled"},
             "connectionProperties": {"pairingState": "paired", "tunnelState": "connected", "transportType": "localNetwork"}}
        ]}});
        let devices = parse_devices(&list.to_string()).unwrap();
        assert_eq!(devices.len(), 3);
        let jo = &devices[0];
        assert_eq!(jo.udid, "00008150-001A2B3C4D5E6F70");
        assert_eq!(jo.name, "Jo's iPhone");
        assert_eq!(jo.os, "27.0");
        assert!(jo.connected && jo.paired);
        assert_eq!(jo.developer_mode, Some(true));
        assert!(jo.matches("jo's iphone"));
        assert!(jo.matches("00008150-001A2B3C4D5E6F70-coredevice"));
        assert!(!devices[1].connected);
        assert_eq!(devices[1].developer_mode, Some(false));
        assert_eq!(devices[2].name, "Legacy");
        assert_eq!(devices[2].os, "18.3");
        assert!(devices[2].connected);
        assert!(parse_devices("{}").is_err());
    }

    #[test]
    fn running_apps_are_found_by_executable() {
        let text = json!({"result": {"runningProcesses": [
            {"executable": "file:///private/var/containers/Bundle/Application/X/App.app/app", "processIdentifier": 812},
            {"executable": "file:///usr/libexec/backboardd", "processIdentifier": 50}
        ]}})
        .to_string();
        assert_eq!(app_pids(&text, "App.app", "app"), [812]);
        assert!(app_pids(&text, "Other.app", "app").is_empty());
        assert!(app_pids("not json", "App.app", "app").is_empty());
    }
}
