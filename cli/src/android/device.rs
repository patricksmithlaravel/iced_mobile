//! Which Android device a command uses (design §6):
//!
//! 1. `--device <serial>` (`run`, `build`), else `$ANDROID_SERIAL` (adb's
//!    own variable; the way to pick a device for `shot`, `logs`, `input`)
//! 2. host.toml `[android] device`
//! 3. `--avd <name>`: that AVD's running emulator, else boot it
//! 4. a running emulator of the AVD icm boots (host.toml `[android] avd`,
//!    else `icm-api<target_sdk>`)
//! 5. the single online device (several: exit 7 `android.device.ambiguous`)
//! 6. boot that AVD, creating it first when icm owns the name
//!
//! Booting starts the emulator and returns; the caller builds while it
//! boots and then waits ([`super::avd::wait_booted`]).

use super::Toolset;
use super::adb::{self, Adb, Listed};
use super::avd::{self, Booting, StartOptions};
use crate::catalogue::CheckId;
use crate::config::Abi;
use crate::context::Ctx;
use crate::error::{IcmError, Result};
use crate::host::HostConfig;
use crate::tools::Env;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

/// What the command asked for.
#[derive(Clone, Debug, Default)]
pub struct Request {
    /// `--device`.
    pub serial: Option<String>,
    /// `--avd`.
    pub avd: Option<String>,
    /// `--show`.
    pub show: bool,
    /// `--fresh`: boot with `-wipe-data`.
    pub wipe: bool,
    /// `[android] target_sdk` (names the managed AVD).
    pub target_sdk: u32,
}

/// The chosen device.
#[derive(Clone, Debug)]
pub struct Chosen {
    /// The serial.
    pub serial: String,
    /// The emulator's AVD, when known.
    pub avd: Option<String>,
    /// The ABI to build for.
    pub abi: Abi,
    /// Set when icm started the emulator for this command.
    pub booting: Option<Booting>,
    /// Why this device was chosen.
    pub reason: String,
}

impl Chosen {
    /// Whether icm owns the emulator (may prepare it and shut it down).
    pub fn managed(&self) -> bool {
        self.avd.as_deref().is_some_and(avd::is_managed)
    }

    /// `emulator` or `device`.
    pub fn kind(&self) -> &'static str {
        if self.serial.starts_with("emulator-") {
            "emulator"
        } else {
            "device"
        }
    }
}

/// The AVD icm boots when nothing else is chosen.
pub fn default_avd(host: &HostConfig, target_sdk: u32) -> String {
    host.android
        .avd
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| avd::managed_name(target_sdk))
}

/// The ABI of an online device (`ro.product.cpu.abi`).
pub fn device_abi(adb: &Adb) -> Result<Abi> {
    let Some(value) = adb
        .getprop("ro.product.cpu.abi")
        .filter(|value| !value.is_empty())
    else {
        return Err(IcmError::new(
            CheckId::AndroidDeviceNone,
            format!(
                "{} is listed online but does not answer `adb shell` (a hung or overloaded emulator)",
                adb.serial
            ),
        )
        .fix(
            "Restart the device; for icm's emulator, `icm stop android --shutdown` and rerun.",
            &["icm stop android --shutdown", "icm run android"],
        ));
    };
    Abi::from_name(&value).ok_or_else(|| {
        IcmError::new(
            CheckId::AndroidDeviceNone,
            format!(
                "{} reports the ABI `{value}`, which icm cannot build for",
                adb.serial
            ),
        )
    })
}

fn not_online(serial: &str, listed: &[Listed], source: &str) -> IcmError {
    let state = listed
        .iter()
        .find(|device| device.serial == serial)
        .map(|device| device.state.clone());
    let detail = match state.as_deref() {
        Some("unauthorized") => format!(
            "{serial} ({source}) is unauthorized: accept the USB debugging prompt on the device"
        ),
        Some(state) => format!("{serial} ({source}) is {state}, not online"),
        None => format!("{serial} ({source}) is not connected"),
    };
    IcmError::new(CheckId::AndroidDeviceNone, detail).fix_commands(["icm devices android"])
}

fn online<'a>(listed: &'a [Listed], serial: &str) -> Option<&'a Listed> {
    listed
        .iter()
        .find(|device| device.serial == serial && device.online())
}

/// The running emulators and their AVD names.
pub fn running_emulators(tools: &Toolset, listed: &[Listed]) -> Vec<(String, Option<String>)> {
    listed
        .iter()
        .filter(|device| device.online() && device.is_emulator())
        .map(|device| {
            let name = Adb::new(tools, &device.serial)
                .ok()
                .and_then(|adb| adb.avd_name());
            (device.serial.clone(), name)
        })
        .collect()
}

/// Chooses (and if needed boots) a device. `boot` is false for commands
/// that must not start an emulator (`shot`, `input`, `logs`).
pub fn choose(
    ctx: &Ctx,
    tools: &Toolset,
    host: &HostConfig,
    env: &Env,
    request: &Request,
    boot: bool,
    emulator_log: &Path,
) -> Result<Chosen> {
    let listed = adb::devices(tools)?;
    let booter = Boot {
        ctx,
        tools,
        env,
        listed: &listed,
        host,
        request,
        emulator_log,
    };
    let online_device = |serial: &str, reason: &str| -> Result<Chosen> {
        if online(&listed, serial).is_none() {
            return Err(not_online(serial, &listed, reason));
        }
        let adb = Adb::new(tools, serial)?;
        Ok(Chosen {
            serial: serial.to_string(),
            avd: adb.avd_name(),
            abi: device_abi(&adb)?,
            booting: None,
            reason: reason.to_string(),
        })
    };

    if let Some(serial) = &request.serial {
        return online_device(serial, "--device");
    }
    if let Some(serial) = env.var("ANDROID_SERIAL") {
        return online_device(serial, "$ANDROID_SERIAL");
    }
    if let Some(serial) = host.android.device.as_deref().filter(|s| !s.is_empty()) {
        return online_device(serial, "host.toml android.device");
    }

    let emulators = running_emulators(tools, &listed);
    let running = |name: &str| {
        emulators
            .iter()
            .find(|(_, avd)| avd.as_deref() == Some(name))
            .map(|(serial, _)| serial.clone())
    };

    if let Some(name) = &request.avd {
        if let Some(serial) = running(name) {
            return online_device(&serial, "--avd (running)");
        }
        if !boot {
            return Err(IcmError::new(
                CheckId::AndroidDeviceNone,
                format!("the AVD `{name}` is not running"),
            )
            .fix_commands([format!("icm run android --avd {name}")]));
        }
        return start(&booter, name, "--avd");
    }

    let default = default_avd(host, request.target_sdk);
    if let Some(serial) = running(&default) {
        return online_device(&serial, "icm's emulator (running)");
    }

    let all_online: Vec<&Listed> = listed.iter().filter(|device| device.online()).collect();
    match all_online.as_slice() {
        [single] => return online_device(&single.serial, "the only online device"),
        [] => {}
        several => {
            let serials: Vec<String> = several
                .iter()
                .map(|device| {
                    let avd = emulators
                        .iter()
                        .find(|(serial, _)| *serial == device.serial)
                        .and_then(|(_, avd)| avd.clone());
                    match avd {
                        Some(avd) => format!("{} ({avd})", device.serial),
                        None => device.serial.clone(),
                    }
                })
                .collect();
            return Err(IcmError::new(
                CheckId::AndroidDeviceAmbiguous,
                format!(
                    "{} devices are online ({}) and none was chosen",
                    serials.len(),
                    serials.join(", ")
                ),
            )
            .fix_commands([
                format!("icm run android --device {}", several[0].serial),
                format!("icm run android --avd {default}"),
            ]));
        }
    }

    if !boot {
        return Err(IcmError::new(
            CheckId::AndroidDeviceNone,
            "no Android device is online and no app session exists",
        )
        .fix_commands(["icm run android"]));
    }
    start(&booter, &default, "icm's emulator")
}

/// Where [`choose`] boots an emulator.
struct Boot<'a> {
    ctx: &'a Ctx,
    tools: &'a Toolset,
    env: &'a Env,
    listed: &'a [Listed],
    host: &'a HostConfig,
    request: &'a Request,
    emulator_log: &'a Path,
}

fn start(boot: &Boot<'_>, name: &str, reason: &str) -> Result<Chosen> {
    let Boot {
        ctx,
        tools,
        env,
        listed,
        host,
        request,
        emulator_log,
    } = *boot;
    // Create the AVD when icm owns the name and it does not exist.
    if !avd::list(env).iter().any(|avd| avd == name) {
        let abi = avd::host_abi();
        let image = avd::find_image(&tools.sdk, request.target_sdk, abi).ok_or_else(|| {
            let package = avd::image_package(request.target_sdk, abi);
            IcmError::new(
                CheckId::EnvAndroidPackageMissing,
                format!(
                    "the system image for android-{} ({}) is not installed, so icm cannot create the AVD {name}",
                    request.target_sdk,
                    abi.as_str()
                ),
            )
            .fix_commands([
                "icm doctor android --fix --yes".to_string(),
                format!("sdkmanager --install \"{package}\""),
            ])
        })?;
        avd::create(ctx, tools, name, &image)?;
        ctx.rep
            .progress(format!("created the AVD {name} from {}", image.package));
    }

    let ports = host.emulator_ports();
    let port = avd::free_port(&ports, listed).ok_or_else(|| {
        IcmError::new(
            CheckId::AndroidEmulatorPortsBusy,
            format!(
                "the emulator ports {} are all in use",
                ports
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
        .fix_commands(["icm stop android --shutdown"])
    })?;

    let booting = avd::start(
        ctx,
        tools,
        name,
        port,
        &StartOptions {
            show: request.show,
            wipe: request.wipe,
            gpu: host.android.emulator_gpu.clone(),
        },
        emulator_log,
    )?;
    let abi = avd::abi(env, name).unwrap_or_else(avd::host_abi);
    Ok(Chosen {
        serial: booting.serial.clone(),
        avd: Some(name.to_string()),
        abi,
        booting: Some(booting),
        reason: format!("{reason} (booted)"),
    })
}

/// The result's `device` object.
pub fn to_json(chosen: &Chosen, adb: &Adb) -> Value {
    let get = |name: &str| adb.getprop(name).filter(|v| !v.is_empty());
    json!({
        "kind": chosen.kind(),
        "serial": chosen.serial,
        "avd": chosen.avd,
        "abi": chosen.abi.as_str(),
        "model": get("ro.product.model"),
        "os": get("ro.build.version.release"),
        "api": get("ro.build.version.sdk").and_then(|v| v.parse::<u32>().ok()),
        "managed": chosen.managed(),
        "booted": chosen.booting.is_some(),
        "chosen_by": chosen.reason,
    })
}

/// Waits for a device adb lists to come online (after an install of a
/// fresh emulator the transport can flap).
pub fn wait_online(tools: &Toolset, serial: &str, limit: Duration) -> bool {
    let deadline = std::time::Instant::now() + limit;
    loop {
        if adb::devices(tools)
            .map(|listed| online(&listed, serial).is_some())
            .unwrap_or(false)
        {
            return true;
        }
        if std::time::Instant::now() >= deadline || crate::signals::pending().is_some() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
