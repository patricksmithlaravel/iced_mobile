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

/// How a listed executable compares with an app's `<bundle>/<binary>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Match {
    /// The executable is the app's binary, inside its bundle.
    Same,
    /// It is another program, or this binary in another bundle or install.
    Different,
    /// The text does not settle it (see [`match_app_executable`]), so
    /// nothing may be concluded from it.
    Unknown,
}

/// How `executable`, as devicectl lists it, compares with `binary` inside
/// the bundle directory `bundle`. devicectl writes the executable as a file
/// URL, and Foundation encodes a URL's path with percent escapes: an app
/// named `My App` is listed in `My%20App.app/my-app`, and `Café` as
/// `Caf%C3%A9`.
///
/// The path of a file URL is decoded once, and only the decoded path is
/// compared: `100%25.app` is listed as `100%2525.app`, and the listed text
/// `100%25.app` denotes the app `100%.app`, not `100%25.app`. A plain path,
/// with no scheme, is compared as it stands. Anything else is
/// [`Match::Unknown`]: a URL with another scheme, a file URL with another
/// host or no path, a raw `?` or `#` (Foundation escapes both in a path, so
/// a URL that has one does not say where its path ends), or an escape that
/// is not two hex digits, or bytes that are not UTF-8 once decoded.
///
/// Only the last two components are compared, so a copy of the app in another
/// container matches as well. That is good enough to find the app's processes
/// for the readiness probe, which ends nothing. What `stop` ends is decided by
/// [`match_installed_executable`], which compares the whole path.
pub fn match_app_executable(executable: &str, bundle: &str, binary: &str) -> Match {
    match executable_path(executable) {
        Some(path) if path.ends_with(&format!("/{bundle}/{binary}")) => Match::Same,
        Some(_) => Match::Different,
        None => Match::Unknown,
    }
}

/// How `executable`, as devicectl lists it, compares with `binary` in the app
/// installed at `install`: the bundle directory's file URL, as devicectl's
/// install output gives it (`installationURL`, with a trailing slash or
/// without). Both are read as [`match_app_executable`] reads a listed
/// executable, and the whole path is compared, container directory included:
/// a copy of the same app installed in another container is
/// [`Match::Different`], however its path ends. Anything that does not read
/// as a path, and an install that names no absolute directory, is
/// [`Match::Unknown`].
pub fn match_installed_executable(executable: &str, install: &str, binary: &str) -> Match {
    let (Some(path), Some(dir)) = (executable_path(executable), executable_path(install)) else {
        return Match::Unknown;
    };
    let dir = dir.trim_end_matches('/');
    if !dir.starts_with('/') {
        return Match::Unknown;
    }
    if path == format!("{dir}/{binary}") {
        Match::Same
    } else {
        Match::Different
    }
}

/// The bundle directory `devicectl device install app` installed `bundle_id`
/// in, as its output names it (`installationURL`, a file URL), or `None` when
/// the output names no such app or gives it no such field.
pub fn installation_url(text: &str, bundle_id: &str) -> Option<String> {
    let json: Value = serde_json::from_str(text).ok()?;
    json.pointer("/result/installedApplications")?
        .as_array()?
        .iter()
        .find(|app| app.get("bundleID").and_then(Value::as_str) == Some(bundle_id))?
        .get("installationURL")?
        .as_str()
        .map(str::to_string)
}

/// The path a listed executable or an install's URL names: a `file://` URL's
/// decoded path, or a plain path as it stands. `None` for another scheme, or
/// a file URL [`file_url_path`] cannot read.
fn executable_path(text: &str) -> Option<String> {
    if text.starts_with("file://") {
        file_url_path(text)
    } else if has_scheme(text) {
        None
    } else {
        Some(text.to_string())
    }
}

/// The decoded path of a `file://` URL whose host is empty or `localhost`,
/// or `None` when [`executable_path`] cannot read one.
fn file_url_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    if rest.contains(['?', '#']) {
        return None;
    }
    let (host, path) = rest.split_at(rest.find('/')?);
    if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
        return None;
    }
    decode_once(path)
}

/// Whether `text` starts with a URL scheme (`name:`). A plain path does
/// not: a colon after a `/` or a space is part of the path.
fn has_scheme(text: &str) -> bool {
    let Some((scheme, _)) = text.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// `text` with each `%XX` escape decoded, once; `None` when an escape is
/// not two hex digits or the decoded bytes are not UTF-8. Stricter than
/// `web::server::percent_decode`, which also reads a sign after the `%`.
fn decode_once(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_value(*bytes.get(index + 1)?)?;
            let low = hex_value(*bytes.get(index + 2)?)?;
            out.push(high * 16 + low);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    char::from(byte).to_digit(16).map(|digit| digit as u8)
}

/// The pids of the running processes whose executable is `<bundle>/<executable>`
/// (`devicectl device info processes`), for the readiness probe. Only a
/// [`Match::Same`] executable counts: one that does not read as the app's is
/// not taken for it. The probe ends nothing, so the tail match is enough here;
/// `stop` uses [`match_installed_executable`].
pub fn app_pids(text: &str, bundle: &str, executable: &str) -> Vec<i64> {
    let Ok(json) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    json.pointer("/result/runningProcesses")
        .and_then(Value::as_array)
        .map(|processes| {
            processes
                .iter()
                .filter(|p| {
                    p.get("executable")
                        .and_then(Value::as_str)
                        .is_some_and(|path| {
                            match_app_executable(path, bundle, executable) == Match::Same
                        })
                })
                .filter_map(|p| p.get("processIdentifier").and_then(Value::as_i64))
                .collect()
        })
        .unwrap_or_default()
}

/// One process of `devicectl device info processes`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// Its pid on the device.
    pub pid: i64,
    /// Its executable as devicectl writes it (a `file://` URL), when it
    /// names one.
    pub executable: Option<String>,
}

impl Listed {
    /// The executable's path for a message: without its `file://`, and with
    /// the URL's percent escapes decoded when they decode, or `?`.
    pub fn program(&self) -> String {
        let path = self.executable.as_deref().unwrap_or("?");
        let path = path.strip_prefix("file://").unwrap_or(path);
        decode_once(path).unwrap_or_else(|| path.to_string())
    }

    /// The executable's path, as [`Listed::program`] shows it, when the entry
    /// names one icm can read: a path that is not blank once `file://` is
    /// removed and its escapes decoded. None when the entry names no
    /// executable, an empty or blank one, or one that does not decode. Such
    /// an entry says nothing about what the process is.
    pub fn usable_program(&self) -> Option<String> {
        let path = self.executable.as_deref()?;
        let path = decode_once(path.strip_prefix("file://").unwrap_or(path))?;
        (!path.trim().is_empty()).then_some(path)
    }

    /// Why [`Listed::usable_program`] names no path, for a message. An
    /// executable whose escapes do not decode is named with its path, since
    /// the device did list one and icm cannot read it; a missing, empty or
    /// blank one is without a usable path.
    pub fn why_no_usable_path(&self) -> String {
        match self.executable.as_deref() {
            Some(path) if decode_once(path.strip_prefix("file://").unwrap_or(path)).is_none() => {
                format!(
                    "with an executable path that does not decode ({})",
                    self.program()
                )
            }
            _ => "without a usable executable path".to_string(),
        }
    }
}

/// Every running process in `devicectl device info processes
/// --json-output` JSON, or why the text is not such a listing. A failed
/// devicectl writes JSON too (`info.outcome` `failed`, an `error`), so
/// that, or any text without `result.runningProcesses`, is an error, never
/// an empty device: a listing that could not be had says nothing about
/// what runs.
pub fn running_processes(text: &str) -> Result<Vec<Listed>, String> {
    let json: Value = serde_json::from_str(text)
        .map_err(|error| format!("devicectl's process list is not JSON: {error}"))?;
    if let Some(outcome) = json.pointer("/info/outcome").and_then(Value::as_str)
        && outcome != "success"
    {
        let said = json
            .pointer("/error/localizedDescription")
            .and_then(Value::as_str)
            .unwrap_or("no reason given");
        return Err(format!("devicectl's outcome was {outcome}: {said}"));
    }
    let processes = json
        .pointer("/result/runningProcesses")
        .and_then(Value::as_array)
        .ok_or("devicectl's process list has no result.runningProcesses")?;
    Ok(processes
        .iter()
        .filter_map(|process| {
            Some(Listed {
                pid: process.get("processIdentifier").and_then(Value::as_i64)?,
                executable: process
                    .get("executable")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect())
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
    fn a_process_list_that_could_not_be_had_is_no_empty_device() {
        let text = json!({"info": {"outcome": "success"}, "result": {"runningProcesses": [
            {"executable": "file:///private/var/containers/Bundle/Application/X/App.app/app", "processIdentifier": 812},
            {"processIdentifier": 1}
        ]}})
        .to_string();
        let listed = running_processes(&text).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].pid, 812);
        assert_eq!(
            listed[0].program(),
            "/private/var/containers/Bundle/Application/X/App.app/app"
        );
        assert_eq!(listed[1].executable, None);
        assert_eq!(listed[1].program(), "?");
        // A device that runs nothing answers with an empty list.
        assert!(
            running_processes(r#"{"result":{"runningProcesses":[]}}"#)
                .unwrap()
                .is_empty()
        );
        // What a failed devicectl writes, and what is no listing at all.
        let failed = json!({"info": {"outcome": "failed"},
            "error": {"localizedDescription": "The device is not connected."}})
        .to_string();
        let error = running_processes(&failed).unwrap_err();
        assert!(error.contains("not connected"), "{error}");
        assert!(running_processes(r#"{"result":{}}"#).is_err());
        assert!(running_processes("").is_err());
        assert!(running_processes("not json").is_err());
    }

    /// An executable icm can read is a path that is not blank once decoded.
    /// An entry with none, an empty or blank one, or escapes that do not
    /// decode has no usable path, and says nothing about its process.
    #[test]
    fn a_listed_executable_is_usable_only_as_a_path_that_decodes() {
        let usable = |executable: Option<&str>| {
            Listed {
                pid: 812,
                executable: executable.map(str::to_string),
            }
            .usable_program()
        };
        assert_eq!(
            usable(Some(
                "file:///private/var/containers/Bundle/Application/X/My%20App.app/app"
            )),
            Some("/private/var/containers/Bundle/Application/X/My App.app/app".to_string())
        );
        assert_eq!(
            usable(Some("/usr/libexec/backboardd")),
            Some("/usr/libexec/backboardd".to_string())
        );
        assert_eq!(usable(None), None);
        assert_eq!(usable(Some("")), None);
        assert_eq!(usable(Some("file://")), None);
        assert_eq!(usable(Some(" \t ")), None);
        assert_eq!(usable(Some("file:///var/%ZZ/app")), None);
        assert_eq!(usable(Some("file:///var/%FF/app")), None);
    }

    /// The reason an entry has no usable path says what it does name: an
    /// executable that does not decode is named with its path, and a missing,
    /// empty or blank one is without a usable path.
    #[test]
    fn an_entry_without_a_usable_path_says_why() {
        let why = |executable: Option<&str>| {
            Listed {
                pid: 812,
                executable: executable.map(str::to_string),
            }
            .why_no_usable_path()
        };
        for executable in [None, Some(""), Some("file://"), Some(" \t ")] {
            assert_eq!(
                why(executable),
                "without a usable executable path",
                "{executable:?}"
            );
        }
        assert_eq!(
            why(Some("file:///var/%ZZ/app")),
            "with an executable path that does not decode (/var/%ZZ/app)"
        );
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

    /// The file URL devicectl lists for a binary in the app's container.
    fn at(dir: &str, binary: &str) -> String {
        format!("file:///private/var/containers/Bundle/Application/X/{dir}/{binary}")
    }

    /// devicectl writes an executable as a file URL, and Foundation
    /// percent-encodes a URL's path: an app whose name has a space, a
    /// non-ASCII character or a `%` is not listed under its bundle's own
    /// spelling, and was taken for another app.
    #[test]
    fn an_escaped_executable_is_the_apps_executable() {
        for (bundle, listed) in [
            ("My App.app", "My%20App.app"),
            ("Caf\u{e9}.app", "Caf%C3%A9.app"),
            ("100%.app", "100%25.app"),
            ("A #1 (beta).app", "A%20%231%20(beta).app"),
            // A plain name needs no escape, and a devicectl that writes
            // the path as it stands matches too.
            ("Plain.app", "Plain.app"),
            ("My App.app", "My App.app"),
        ] {
            assert_eq!(
                match_app_executable(&at(listed, "my-app"), bundle, "my-app"),
                Match::Same,
                "{bundle} was not found as {listed}"
            );
        }
        // Another bundle's, another binary's, and a directory that only
        // ends the same way are not this app's.
        let spaced = at("My%20App.app", "my-app");
        assert_eq!(
            match_app_executable(&spaced, "App.app", "my-app"),
            Match::Different
        );
        assert_eq!(
            match_app_executable(&spaced, "My.app", "my-app"),
            Match::Different
        );
        assert_eq!(
            match_app_executable(&spaced, "My App.app", "other"),
            Match::Different
        );
        assert_eq!(
            match_app_executable(&at("My%20App.app.old", "my-app"), "My App.app", "my-app"),
            Match::Different
        );

        let text = json!({"result": {"runningProcesses": [
            {"executable": at("My%20App.app", "my-app"), "processIdentifier": 812},
            {"executable": at("Caf%C3%A9.app", "my-app"), "processIdentifier": 813},
            {"executable": "file:///usr/libexec/backboardd", "processIdentifier": 50}
        ]}})
        .to_string();
        assert_eq!(app_pids(&text, "My App.app", "my-app"), [812]);
        assert_eq!(app_pids(&text, "Caf\u{e9}.app", "my-app"), [813]);
        assert!(app_pids(&text, "My%20App.app.x", "my-app").is_empty());

        // A message names the path as it is, not as it is escaped.
        let listed = Listed {
            pid: 812,
            executable: Some(at("My%20App.app", "my-app")),
        };
        assert_eq!(
            listed.program(),
            "/private/var/containers/Bundle/Application/X/My App.app/my-app"
        );
    }

    /// A file URL's path is decoded once: a bundle named `100%25.app` is
    /// listed as `100%2525.app`, and the listed `100%25.app` is the app
    /// `100%.app`, not the one named `100%25.app`.
    #[test]
    fn an_escape_is_decoded_once() {
        let listed = at("100%25.app", "my-app");
        assert_eq!(
            match_app_executable(&listed, "100%25.app", "my-app"),
            Match::Different
        );
        assert_eq!(
            match_app_executable(&listed, "100%.app", "my-app"),
            Match::Same
        );
        let escaped = at("100%2525.app", "my-app");
        assert_eq!(
            match_app_executable(&escaped, "100%25.app", "my-app"),
            Match::Same
        );
        assert_eq!(
            match_app_executable(&escaped, "100%.app", "my-app"),
            Match::Different
        );
    }

    #[test]
    fn an_encoded_space_is_a_space() {
        let listed = at("My%20App.app", "my-app");
        assert_eq!(
            match_app_executable(&listed, "My App.app", "my-app"),
            Match::Same
        );
        assert_eq!(
            match_app_executable(&listed, "My%20App.app", "my-app"),
            Match::Different
        );
    }

    #[test]
    fn encoded_unicode_is_the_letter_it_spells() {
        let listed = at("Caf%C3%A9.app", "my-app");
        assert_eq!(
            match_app_executable(&listed, "Caf\u{e9}.app", "my-app"),
            Match::Same
        );
        assert_eq!(
            match_app_executable(&listed, "Cafe.app", "my-app"),
            Match::Different
        );
    }

    /// An escape that is not two hex digits, or bytes that are not UTF-8
    /// once decoded, settles nothing: the listing is neither a match nor a
    /// mismatch for any bundle, and no app counts as running under it.
    #[test]
    fn a_malformed_escape_is_unknown() {
        for dir in ["%G1.app", "50%.app", "%+1.app", "%FF.app", "a%2"] {
            assert_eq!(
                match_app_executable(&at(dir, "my-app"), dir, "my-app"),
                Match::Unknown,
                "{dir}"
            );
            assert_eq!(
                match_app_executable(&at(dir, "my-app"), "50.app", "my-app"),
                Match::Unknown,
                "{dir}"
            );
        }
        let text = json!({"result": {"runningProcesses": [
            {"executable": at("50%.app", "my-app"), "processIdentifier": 77}
        ]}})
        .to_string();
        assert!(app_pids(&text, "50%.app", "my-app").is_empty());
    }

    /// The other shapes of executable that settle nothing: another scheme,
    /// another host, no path, and a `?` or `#` that a file URL would escape.
    #[test]
    fn an_executable_of_another_shape_is_unknown() {
        for url in [
            "https://example.com/private/My%20App.app/my-app",
            "FILE:///private/My%20App.app/my-app",
            "file:/private/My%20App.app/my-app",
            "file://other-host/private/My%20App.app/my-app",
            "file://",
            "file:///private/My%20App.app/my-app?x",
            "file:///private/My%20App.app/my-app#x",
        ] {
            assert_eq!(
                match_app_executable(url, "My App.app", "my-app"),
                Match::Unknown,
                "{url}"
            );
        }
        // localhost is this machine, and its path is read.
        assert_eq!(
            match_app_executable(
                "file://localhost/private/My%20App.app/my-app",
                "My App.app",
                "my-app"
            ),
            Match::Same
        );
    }

    /// A plain path, with no scheme, is compared as it stands: its escapes
    /// are not decoded, and a colon inside it is no scheme.
    #[test]
    fn a_plain_path_is_compared_as_it_stands() {
        assert_eq!(
            match_app_executable("/private/x/50%.app/my-app", "50%.app", "my-app"),
            Match::Same
        );
        assert_eq!(
            match_app_executable("/private/x/My%20App.app/my-app", "My App.app", "my-app"),
            Match::Different
        );
        assert_eq!(
            match_app_executable("/private/a:b/My App.app/my-app", "My App.app", "my-app"),
            Match::Same
        );
    }

    /// The app icm installed is in container `X`. A copy of an app with the
    /// same bundle name in container `OTHER-UUID` ends in the same
    /// `Fixture.app/release-app`: the whole path decides, so it is another
    /// process, and the readiness probe's tail match does not decide it.
    #[test]
    fn an_installed_app_is_the_whole_path_and_no_copy_in_another_container() {
        let install = "file:///private/var/containers/Bundle/Application/X/Fixture.app/";
        assert_eq!(
            match_installed_executable(&at("Fixture.app", "release-app"), install, "release-app"),
            Match::Same
        );
        let other =
            "file:///private/var/containers/Bundle/Application/OTHER-UUID/Fixture.app/release-app";
        assert_eq!(
            match_installed_executable(other, install, "release-app"),
            Match::Different
        );
        // The same container with another binary, or another bundle, is not it.
        assert_eq!(
            match_installed_executable(&at("Fixture.app", "other"), install, "release-app"),
            Match::Different
        );
        assert_eq!(
            match_installed_executable(&at("Other.app", "release-app"), install, "release-app"),
            Match::Different
        );
        // A directory that only ends the same way is not the install either.
        assert_eq!(
            match_installed_executable(
                &at("OLD-Fixture.app", "release-app"),
                install,
                "release-app"
            ),
            Match::Different
        );
        // The install's trailing slash is optional.
        assert_eq!(
            match_installed_executable(
                &at("Fixture.app", "release-app"),
                install.trim_end_matches('/'),
                "release-app"
            ),
            Match::Same
        );
    }

    /// The install is compared as a listed executable is: its escapes are
    /// decoded once, so the app named `My App` installed as `My%20App.app`
    /// runs `My App.app/my-app`, and the app named `100%25` (installed as
    /// `100%2525.app`) is not the app `100%.app` listed as `100%25.app`.
    #[test]
    fn an_installed_app_is_compared_decoded_once() {
        let spaced = "file:///private/var/containers/Bundle/Application/X/My%20App.app/";
        assert_eq!(
            match_installed_executable(&at("My%20App.app", "my-app"), spaced, "my-app"),
            Match::Same
        );
        // A URL with a raw space names the same directory.
        let raw = spaced.replace("%20", " ");
        assert_eq!(
            match_installed_executable(&at("My%20App.app", "my-app"), &raw, "my-app"),
            Match::Same
        );
        let percent = "file:///private/var/containers/Bundle/Application/X/100%2525.app/";
        assert_eq!(
            match_installed_executable(&at("100%25.app", "my-app"), percent, "my-app"),
            Match::Different
        );
        assert_eq!(
            match_installed_executable(&at("100%2525.app", "my-app"), percent, "my-app"),
            Match::Same
        );
        // A listing that spells `100%.app` has a bad escape, so it says nothing.
        assert_eq!(
            match_installed_executable(&at("100%.app", "my-app"), percent, "my-app"),
            Match::Unknown
        );
    }

    /// Where the listing or the install does not say what path it names,
    /// nothing is concluded: a malformed escape, another scheme, a `?` or `#`
    /// in the listing, or an install that names no absolute directory.
    #[test]
    fn an_installed_app_compared_with_what_does_not_read_is_unknown() {
        let install = "file:///private/var/containers/Bundle/Application/X/Fixture.app/";
        for executable in [
            "https://example.com/private/Fixture.app/release-app",
            "file:///private/var/containers/Bundle/Application/X/Fixture%G1.app/release-app",
            "file:///private/var/containers/Bundle/Application/X/Fixture.app/release-app?x",
            "file:///private/var/containers/Bundle/Application/X/Fixture.app/release-app#x",
        ] {
            assert_eq!(
                match_installed_executable(executable, install, "release-app"),
                Match::Unknown,
                "{executable}"
            );
        }
        for install in [
            "https://example.com/Fixture.app/",
            "file://other-host/private/Fixture.app/",
            "file:///",
            "file:///%G1/",
            "Fixture.app/",
            "",
        ] {
            assert_eq!(
                match_installed_executable(
                    &at("Fixture.app", "release-app"),
                    install,
                    "release-app"
                ),
                Match::Unknown,
                "{install}"
            );
        }
    }

    /// The bundle directory is read from the install output's entry for the
    /// app's bundle id, and only when that entry names one.
    #[test]
    fn the_install_output_names_the_installed_app_directory() {
        let text = json!({"info": {"outcome": "success"}, "result": {"installedApplications": [
            {"bundleID": "com.example.other", "installationURL": "file:///private/var/containers/Bundle/Application/Y/Other.app/"},
            {"bundleID": "com.acme.fixture", "installationURL": "file:///private/var/containers/Bundle/Application/X/Fixture.app/"}
        ]}})
        .to_string();
        assert_eq!(
            installation_url(&text, "com.acme.fixture").as_deref(),
            Some("file:///private/var/containers/Bundle/Application/X/Fixture.app/")
        );
        assert_eq!(installation_url(&text, "com.acme.missing"), None);
        // An entry with no installation URL, and an output with no entries,
        // name no directory.
        assert_eq!(
            installation_url(
                r#"{"result":{"installedApplications":[{"bundleID":"com.acme.fixture"}]}}"#,
                "com.acme.fixture"
            ),
            None
        );
        assert_eq!(
            installation_url(r#"{"result":{}}"#, "com.acme.fixture"),
            None
        );
        assert_eq!(installation_url("not json", "com.acme.fixture"), None);
    }
}
