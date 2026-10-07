//! `icm devices [<platform>]` (design §6): what each platform can run on.
//! Without a platform it lists the desktop, the web's browser, the iOS
//! simulators (on macOS) and Android's devices and AVDs; a platform that
//! cannot be listed there (no SDK, no Xcode) is a WARN, not a failure.

use crate::catalogue::CheckId;
use crate::cli::{DevicesArgs, Platform};
use crate::context::Ctx;
use crate::error::{Check, IcmError, Result, Status};
use serde_json::{Map, Value, json};
use std::time::Duration;

/// Runs `icm devices`.
pub fn run(ctx: &mut Ctx, args: &DevicesArgs) -> Result<()> {
    let platforms = match args.platform {
        Some(Platform::Android) => return crate::android::devices(ctx),
        Some(platform) => vec![platform],
        None => {
            let mut all = vec![Platform::Desktop, Platform::Web];
            if cfg!(target_os = "macos") {
                all.push(Platform::IosSim);
                all.push(Platform::IosDevice);
            }
            all.push(Platform::Android);
            all
        }
    };
    let single = platforms.len() == 1;

    let mut text = String::new();
    let mut listed = Map::new();
    let mut counts = Vec::new();
    for platform in platforms {
        let name = platform.as_str();
        let found = match platform {
            Platform::Desktop => Ok(desktop()),
            Platform::Web => web(ctx),
            Platform::IosSim => ios_sim(ctx),
            Platform::Android => crate::android::device_listing(ctx).map(|(lines, value)| {
                let online = value["devices"]
                    .as_array()
                    .map_or(0, |d| d.iter().filter(|d| d["state"] == "device").count());
                let avds = value["avds"].as_array().map_or(0, Vec::len);
                (lines, value, format!("{online} online, {avds} AVD(s)"))
            }),
            Platform::IosDevice => crate::platform::ios_device::listing(ctx),
        };
        match found {
            Ok((lines, value, count)) => {
                text.push_str(&format!("{name}:\n"));
                for line in lines.lines() {
                    text.push_str(&format!("  {line}\n"));
                }
                let _ = listed.insert(name.to_string(), value);
                counts.push(format!("{name} {count}"));
            }
            Err(error) if single => return Err(error),
            Err(error) => {
                text.push_str(&format!("{name}: cannot list ({})\n", error.detail));
                let _ = listed.insert(name.to_string(), Value::Null);
                counts.push(format!("{name} unavailable"));
                ctx.rep.check(Check::from_error(error, Status::Warn));
            }
        }
    }

    ctx.rep.set("platforms", Value::Object(listed));
    ctx.rep.summary(counts.join("; "));
    ctx.rep.content(text);
    Ok(())
}

/// The desktop: this machine.
fn desktop() -> (String, Value, String) {
    let device = crate::platform::desktop::device_json();
    let line = format!(
        "this machine ({}, {})",
        device["os"].as_str().unwrap_or(""),
        device["arch"].as_str().unwrap_or("")
    );
    (
        line,
        json!({"devices": [device]}),
        "this machine".to_string(),
    )
}

/// The web: the Chrome the session drives.
fn web(ctx: &mut Ctx) -> Result<(String, Value, String)> {
    let host = ctx.host()?.clone();
    let chrome = crate::tools::chrome(&host, &ctx.env)?;
    let line = format!(
        "headless Chrome {}({})",
        chrome
            .version
            .as_deref()
            .map(|v| format!("{v} "))
            .unwrap_or_default(),
        crate::paths::display(&chrome.path)
    );
    Ok((
        line,
        json!({"browser": chrome}),
        "headless Chrome".to_string(),
    ))
}

/// The iOS simulators `simctl` lists as available, booted ones first;
/// icm's own (`icm-*`) and host.toml's pinned one are marked.
fn ios_sim(ctx: &mut Ctx) -> Result<(String, Value, String)> {
    let host = ctx.host()?.clone();
    let xcode = crate::tools::xcode(&ctx.env)?;
    let outcome = ctx.probe(
        &xcode
            .xcrun()
            .args(["simctl", "list", "-j", "devices", "available"])
            .timeout(Duration::from_secs(60)),
    )?;
    if !outcome.success() {
        return Err(IcmError::new(
            CheckId::ToolFailed,
            format!("simctl list failed: {}", outcome.stderr_tail(3)),
        ));
    }
    let mut devices = crate::simctl::parse_devices(&outcome.stdout_text())
        .map_err(|error| IcmError::new(CheckId::ToolFailed, error))?;
    devices.retain(|device| device.runtime.contains("iOS"));
    devices.sort_by_key(|device| {
        (
            device.state != "Booted",
            !crate::managed::is_managed(&device.name) && !device.name.starts_with("icm-"),
            std::cmp::Reverse(runtime_version(&device.runtime)),
            device.name.clone(),
        )
    });
    let pinned = host.ios.simulator_udid.as_deref().filter(|u| !u.is_empty());

    let mut lines = String::new();
    let mut listed = Vec::new();
    for device in &devices {
        let version = runtime_version(&device.runtime);
        let os = format!("iOS {}.{}", version.0, version.1);
        let mut marks = Vec::new();
        if device.name.starts_with("icm-") {
            marks.push("icm's");
        }
        if pinned.is_some_and(|udid| udid.eq_ignore_ascii_case(&device.udid)) {
            marks.push("host.toml");
        }
        lines.push_str(&format!(
            "{:36} {:8} {:9} {}{}\n",
            device.udid,
            device.state,
            os,
            device.name,
            if marks.is_empty() {
                String::new()
            } else {
                format!(" ({})", marks.join(", "))
            }
        ));
        listed.push(json!({
            "udid": device.udid,
            "name": device.name,
            "state": device.state,
            "os": format!("{}.{}", version.0, version.1),
            "managed": crate::managed::is_managed(&device.name),
            "pinned": marks.contains(&"host.toml"),
        }));
    }
    let booted = devices.iter().filter(|d| d.state == "Booted").count();
    Ok((
        lines,
        json!({"devices": listed}),
        format!("{} simulator(s), {booted} booted", devices.len()),
    ))
}

/// `com.apple.CoreSimulator.SimRuntime.iOS-27-0` gives `(27, 0)`.
fn runtime_version(runtime: &str) -> (u32, u32) {
    let tail = runtime.rsplit('.').next().unwrap_or("");
    let mut parts = tail
        .trim_start_matches("iOS-")
        .split('-')
        .map(|part| part.parse::<u32>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_versions_come_from_the_identifier() {
        assert_eq!(
            runtime_version("com.apple.CoreSimulator.SimRuntime.iOS-27-0"),
            (27, 0)
        );
        assert_eq!(
            runtime_version("com.apple.CoreSimulator.SimRuntime.iOS-18-3"),
            (18, 3)
        );
        assert_eq!(runtime_version("weird"), (0, 0));
    }
}
