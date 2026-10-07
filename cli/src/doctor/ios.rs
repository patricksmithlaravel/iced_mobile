//! What the iOS Simulator (and, for building only, iOS devices) needs:
//! macOS, a release Xcode ≥ 26 with its licence accepted, a simulator
//! runtime at or above `[ios] min_os`, and icm's managed simulator.

use super::{Fix, Probe, Requirement, unsupported};
use crate::catalogue::{By, CheckId};
use crate::cli::Platform;
use crate::error::{Check, Status};
use crate::managed;
use crate::simctl;
use crate::tools::{self, Xcode};
use std::time::Duration;

/// The oldest Xcode icm supports.
pub const MIN_XCODE_MAJOR: u32 = 26;

pub(super) fn gather(probe: &Probe<'_>, platform: Platform) -> Vec<Requirement> {
    let mut out = Vec::new();
    if !cfg!(target_os = "macos") {
        out.push(unsupported(
            probe,
            platform,
            &format!("{} needs a macOS host with Xcode", platform.as_str()),
        ));
        return out;
    }

    let xcode = match tools::xcode(probe.env) {
        Ok(xcode) => xcode,
        Err(error) => {
            out.push(Requirement::new(
                "ios.xcode",
                Some(platform),
                Check::from_error(error, Status::Fail),
            ));
            return out;
        }
    };

    if xcode.major() < MIN_XCODE_MAJOR {
        out.push(Requirement::new(
            "ios.xcode",
            Some(platform),
            Check::fail(
                CheckId::EnvXcodeTooOld,
                format!(
                    "Xcode {} at {} is older than Xcode {MIN_XCODE_MAJOR}",
                    xcode.display(),
                    xcode.developer_dir.display()
                ),
            ),
        ));
        return out;
    }
    out.push(Requirement::new(
        "ios.xcode",
        Some(platform),
        Check::pass(
            CheckId::EnvXcodeMissing,
            format!(
                "Xcode {} at {} ({})",
                xcode.display(),
                xcode.developer_dir.display(),
                xcode.source
            ),
        ),
    ));
    if xcode.beta {
        out.push(Requirement::new(
            "ios.xcode_beta",
            Some(platform),
            Check::warn(
                CheckId::EnvXcodeBeta,
                format!("Xcode {} looks like a beta", xcode.display()),
            ),
        ));
    }

    if platform == Platform::IosSim {
        out.extend(simulator(probe, &xcode));
    }
    out
}

fn min_os(probe: &Probe<'_>) -> String {
    probe
        .project
        .map(|p| p.config.config.ios.min_os.clone())
        .unwrap_or_else(|| "16.0".to_string())
}

fn simctl_json(probe: &Probe<'_>, xcode: &Xcode, args: &[&str]) -> Result<String, String> {
    let cmd = xcode
        .xcrun()
        .args(["simctl", "list", "-j"])
        .args(args)
        .timeout(Duration::from_secs(60));
    match probe.ctx.probe(&cmd) {
        Ok(outcome) if outcome.success() => Ok(outcome.stdout_text()),
        Ok(outcome) => Err(format!(
            "`{}` failed ({}): {}",
            cmd.display(),
            outcome.describe(),
            outcome.stderr_tail(3)
        )),
        Err(error) => Err(error.detail),
    }
}

fn simulator(probe: &Probe<'_>, xcode: &Xcode) -> Vec<Requirement> {
    let platform = Some(Platform::IosSim);
    let min_os = min_os(probe);
    let mut out = Vec::new();

    let runtimes = match simctl_json(probe, xcode, &["runtimes", "available"])
        .and_then(|json| simctl::parse_runtimes(&json))
    {
        Ok(runtimes) => runtimes,
        Err(detail) => {
            out.push(Requirement::new(
                "ios.runtime",
                platform,
                Check::fail(CheckId::ToolFailed, detail),
            ));
            return out;
        }
    };

    let Some(runtime) = simctl::newest_runtime(&runtimes, &min_os) else {
        let installed: Vec<String> = runtimes
            .iter()
            .filter(|r| r.is_ios())
            .map(|r| r.name.clone())
            .collect();
        let check = Check::fail(
            CheckId::EnvIosRuntimeMissing,
            format!(
                "no iOS simulator runtime at or above [ios] min_os {min_os} (installed: {}); the download is about 8 GB",
                if installed.is_empty() {
                    "none".to_string()
                } else {
                    installed.join(", ")
                }
            ),
        );
        out.push(Requirement::new("ios.runtime", platform, check).with_fixes(
            vec![Fix::DownloadIosPlatform {
                developer_dir: xcode.developer_dir.clone(),
            }],
            probe.env,
        ));
        out.push(Requirement::new(
            "ios.simulator",
            platform,
            Check::skip(
                CheckId::EnvSimulatorMissing,
                "the managed simulator needs an iOS runtime first",
            ),
        ));
        return out;
    };
    out.push(Requirement::new(
        "ios.runtime",
        platform,
        Check::pass(
            CheckId::EnvIosRuntimeMissing,
            format!(
                "{} ({}) satisfies [ios] min_os {min_os}",
                runtime.name, runtime.buildversion
            ),
        ),
    ));

    let devices = match simctl_json(probe, xcode, &["devices"])
        .and_then(|json| simctl::parse_devices(&json))
    {
        Ok(devices) => devices,
        Err(detail) => {
            out.push(Requirement::new(
                "ios.simulator",
                platform,
                Check::fail(CheckId::ToolFailed, detail),
            ));
            return out;
        }
    };

    // A simulator pinned in host.toml replaces the managed one.
    if let Some(udid) = probe
        .host
        .ios
        .simulator_udid
        .as_deref()
        .filter(|u| !u.trim().is_empty())
    {
        let check = match devices.iter().find(|d| d.udid == udid) {
            Some(device) => Check::pass(
                CheckId::EnvSimulatorMissing,
                format!(
                    "host.toml pins simulator {} ({udid}, {})",
                    device.name, device.state
                ),
            ),
            None => {
                let mut check = Check::fail(
                    CheckId::EnvSimulatorMissing,
                    format!("host.toml [ios] simulator_udid {udid} names no simulator"),
                );
                check.error = check
                    .error
                    .fix(
                        "Set [ios] simulator_udid in host.toml to an existing simulator (`xcrun simctl list devices`), or remove it to use icm's managed one.",
                        &[],
                    )
                    .by(By::Agent);
                check
            }
        };
        out.push(Requirement::new("ios.simulator", platform, check));
        return out;
    }

    let wanted = probe.host.ios.simulator_type.as_deref();
    let Some(device_type) = simctl::choose_device_type(runtime, wanted) else {
        let mut check = Check::fail(
            CheckId::EnvSimulatorMissing,
            match wanted {
                Some(wanted) => format!(
                    "{} does not run the device type host.toml [ios] simulator_type names ({wanted})",
                    runtime.name
                ),
                None => format!("{} offers no iPhone device type", runtime.name),
            },
        );
        check.error = check.error.by(By::Owner);
        out.push(Requirement::new("ios.simulator", platform, check));
        return out;
    };

    let name = managed::simulator_name(&device_type.name, &runtime.version);
    match devices
        .iter()
        .find(|d| d.name == name && d.runtime == runtime.identifier && d.is_available)
    {
        Some(device) => out.push(Requirement::new(
            "ios.simulator",
            platform,
            Check::pass(
                CheckId::EnvSimulatorMissing,
                format!("{name} ({}, {})", device.udid, device.state),
            ),
        )),
        None => out.push(
            Requirement::new(
                "ios.simulator",
                platform,
                Check::fail(
                    CheckId::EnvSimulatorMissing,
                    format!("the managed simulator {name} does not exist yet"),
                ),
            )
            .with_fixes(
                vec![Fix::SimCreate {
                    developer_dir: xcode.developer_dir.clone(),
                    name,
                    device_type: device_type.identifier.clone(),
                    runtime: runtime.identifier.clone(),
                }],
                probe.env,
            ),
        ),
    }
    out
}
