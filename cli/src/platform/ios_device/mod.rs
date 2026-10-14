//! Physical iOS devices (`ios-device`; design §10.5, §13.1): `icm build|run|
//! shot|logs|devices ios-device` through `xcrun devicectl`. `icm stop
//! ios-device` and `icm stop --all` end the session by what its record
//! says (`commands/stop.rs`), and `icm input ios-device` is unsupported (no
//! bridge on physical iOS in v1, §13.7).
//!
//! `run`:
//! 1. Prelude: macOS and Xcode; the device (`--device`, else the single
//!    connected, paired physical device; `ios.device.not_found`,
//!    `.ambiguous`, `.developer_mode_off`); the Apple Development identity
//!    and a development profile that lists the device (`[ios.signing]
//!    development`, owner items, exit 9). All before the build.
//! 2. Build `aarch64-apple-ios` (`IPHONEOS_DEPLOYMENT_TARGET` stamped), the
//!    Mach-O platform gate, the device bundle with the `DT*` keys and
//!    `embedded.mobileprovision` ([`crate::ios::bundle`]), the development
//!    entitlements checked against the profile, `codesign` under the
//!    keychain watchdog, `codesign --verify`.
//! 3. `devicectl device install app`, then `devicectl device process launch
//!    --terminate-existing --console <id>` as a detached child whose output
//!    goes to the session's `console.log`, with `DEVICECTL_CHILD_ICM_EVENTS=1`.
//!    Ready is `ICM_EVENT ready` there; without a `start` event after 5 s,
//!    three `devicectl device info processes` polls showing the app.
//! 4. `devicectl device capture screenshot`, the preview, the session.
//!
//! The console in `target/icm/sessions/ios-device/<run>/` is the app's own
//! output, unredacted. The run directory's `console.log` (which the
//! evidence names), `app.log`, `logs.ndjson` and devicectl's `install.json`
//! have the secret values icm knows redacted. The session keeps the values
//! of the app's secret-named `--env` in `secrets.json` next to the console
//! ([`process::keep_secrets`]) for later commands.
//!
//! Nothing here ever touches a simulator: devicectl's simulator entries are
//! left out of the device list.

pub mod devicectl;

use crate::cargo::{Invocation, Select};
use crate::catalogue::CheckId;
use crate::cli::{BuildArgs, InputArgs, LogsArgs, RunArgs, ShotArgs};
use crate::context::{Ctx, Project};
use crate::error::{Check, Evidence, IcmError, Result};
use crate::ios::identity::{Identity, Role};
use crate::ios::profile::{Kind, Profile, Want};
use crate::ios::{bundle, codesign, dt, entitlements, identity, profile};
use crate::plan::{Plan, Step};
use crate::platform::desktop::logs as records;
use crate::platform::ios_sim::macho::{self, PLATFORM_IOS};
use crate::process::{self, Cmd};
use crate::session::{self, Session, SessionDevice};
use crate::tools::Xcode;
use devicectl::Device;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// The platform's name.
pub const PLATFORM: &str = "ios-device";

/// The device triple.
pub const TRIPLE: &str = "aarch64-apple-ios";

/// How long the app may stay silent before icm probes instead.
const PROBE_AFTER: Duration = Duration::from_secs(5);

/// How many consecutive process polls must show the app.
const PROBE_POLLS: u32 = 3;

fn internal(what: impl Into<String>) -> IcmError {
    IcmError::new(CheckId::InternalBug, what)
}

fn io(what: &str, path: &Path, error: impl std::fmt::Display) -> IcmError {
    internal(format!(
        "cannot {what} {}: {error}",
        crate::paths::display(path)
    ))
}

fn sleep_checked(duration: Duration) -> Result<()> {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        if let Some(signal) = crate::signals::pending() {
            return Err(crate::output::interrupted(signal));
        }
        std::thread::sleep(Duration::from_millis(50).min(until - Instant::now()));
    }
    Ok(())
}

fn devicectl(xcode: &Xcode) -> Cmd {
    xcode.xcrun().arg("devicectl")
}

fn profile_name(release: bool) -> &'static str {
    if release { "release" } else { "dev" }
}

/// The host and Xcode.
fn host_xcode(ctx: &Ctx) -> Result<Xcode> {
    if !cfg!(target_os = "macos") {
        return Err(IcmError::new(
            CheckId::EnvUnsupportedHost,
            "iOS devices need a macOS host with Xcode",
        ));
    }
    crate::tools::xcode(&ctx.env)
}

// ---- devices -------------------------------------------------------------------------------

/// A scratch file for devicectl's `--json-output`.
fn json_file(what: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!(
        "icm-devicectl-{what}-{}-{nanos}.json",
        std::process::id()
    ))
}

/// Runs a devicectl command that writes `--json-output` and returns the
/// JSON text.
fn devicectl_json(ctx: &Ctx, xcode: &Xcode, args: &[&str], what: &str) -> Result<String> {
    let file = json_file(what);
    let outcome = ctx.probe(
        &devicectl(xcode)
            .args(args)
            .arg("--json-output")
            .arg(&file)
            .timeout(Duration::from_secs(60)),
    )?;
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let _ = std::fs::remove_file(&file);
    if !outcome.success() && text.is_empty() {
        return Err(IcmError::new(
            CheckId::ToolFailed,
            format!(
                "xcrun devicectl {} failed: {}",
                args.join(" "),
                outcome.reason(4)
            ),
        ));
    }
    Ok(text)
}

/// The physical devices CoreDevice knows.
pub fn list(ctx: &Ctx, xcode: &Xcode) -> Result<Vec<Device>> {
    let text = devicectl_json(ctx, xcode, &["list", "devices"], "devices")?;
    devicectl::parse_devices(&text).map_err(|e| IcmError::new(CheckId::ToolFailed, e))
}

/// The device a command uses: `--device`, else the single connected,
/// paired device.
fn choose(devices: &[Device], selector: Option<&str>) -> Result<Device> {
    let fix = "Connect the device with a cable, unlock it and trust this Mac, or pass --device <udid|name>.";
    if let Some(selector) = selector.filter(|s| !s.trim().is_empty()) {
        return devices
            .iter()
            .find(|d| d.matches(selector))
            .cloned()
            .ok_or_else(|| {
                IcmError::new(
                    CheckId::IosDeviceNotFound,
                    format!("no physical iOS device is named or has the UDID `{selector}`"),
                )
                .fix(
                    fix,
                    &["icm devices ios-device", "xcrun devicectl list devices"],
                )
            });
    }
    let ready: Vec<&Device> = devices.iter().filter(|d| d.connected && d.paired).collect();
    match ready.as_slice() {
        [device] => Ok((*device).clone()),
        [] => {
            let known: Vec<String> = devices.iter().map(Device::label).collect();
            Err(IcmError::new(
                CheckId::IosDeviceNotFound,
                if known.is_empty() {
                    "no physical iOS device is paired with this Mac".to_string()
                } else {
                    format!(
                        "no paired iOS device is connected (known: {})",
                        known.join("; ")
                    )
                },
            )
            .fix(fix, &["icm devices ios-device"]))
        }
        several => Err(IcmError::new(
            CheckId::IosDeviceAmbiguous,
            format!(
                "{} iOS devices are connected: {}",
                several.len(),
                several
                    .iter()
                    .map(|d| d.label())
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        )
        .fix(
            "Pass --device with one of the UDIDs.",
            &["icm run ios-device --device <udid>"],
        )),
    }
}

/// Picks the device and checks Developer Mode.
fn device(ctx: &Ctx, xcode: &Xcode, selector: Option<&str>) -> Result<Device> {
    let device = choose(&list(ctx, xcode)?, selector)?;
    if device.developer_mode == Some(false) {
        return Err(IcmError::new(
            CheckId::IosDeviceDeveloperModeOff,
            format!("Developer Mode is off on {}", device.label()),
        ));
    }
    ctx.rep.check(Check::pass(
        CheckId::IosDeviceNotFound,
        format!("device {}", device.label()),
    ));
    Ok(device)
}

/// `icm devices ios-device` and the ios-device part of `icm devices`:
/// (text lines, JSON, count).
pub fn listing(ctx: &mut Ctx) -> Result<(String, Value, String)> {
    let xcode = host_xcode(ctx)?;
    let devices = list(ctx, &xcode)?;
    let lines: Vec<String> = devices
        .iter()
        .map(|d| {
            format!(
                "{}{}",
                d.label(),
                if d.connected { "" } else { " (not connected)" }
            )
        })
        .collect();
    let connected = devices.iter().filter(|d| d.connected).count();
    Ok((
        if lines.is_empty() {
            "no physical device is paired with this Mac".to_string()
        } else {
            lines.join("\n")
        },
        json!({"devices": devices.iter().map(Device::to_json).collect::<Vec<_>>()}),
        format!("{connected} connected of {}", devices.len()),
    ))
}

// ---- signing -------------------------------------------------------------------------------

/// The development identity and profile.
#[derive(Clone, Debug)]
struct Signing {
    team: String,
    identity: Identity,
    profile: Profile,
    keychain: Option<PathBuf>,
}

/// The development identity and a development profile for the app (and
/// the device, when known). Everything missing is the owner's (exit 9).
fn signing(ctx: &mut Ctx, project: &Project, device: Option<&Device>) -> Result<Signing> {
    let config = project.config.config.clone();
    let Some(team) = config.ios.team_id.clone() else {
        return Err(IcmError::new(
            CheckId::ConfigOwnerDecision,
            format!(
                "{}: [ios] team_id is unset; signing for a device needs the Apple team",
                project.config.source.location_for("ios.team_id")
            ),
        )
        .evidence(project.config.evidence("ios"))
        .fix(
            "The owner sets [ios] team_id = \"<10-character team id>\" (Apple Developer > Membership details).",
            &[],
        ));
    };
    let host = ctx.host()?.clone();
    let keychain = host.signing_keychain(&ctx.env);
    let reference = config.ios.signing.development.clone();
    let listed = identity::list(ctx, keychain.as_deref())?;
    let candidates =
        identity::candidates(&reference.identity, &listed, Role::Development, Some(&team))
            .map_err(|e| e.evidence(project.config.evidence("ios.signing.development")))?;
    let certificates: Vec<String> = candidates.iter().map(|i| i.sha1.clone()).collect();
    let profile_ref = {
        let reference = reference.profile.trim();
        let uuid = reference.len() == 36 && reference.matches('-').count() == 4;
        if reference == "auto" || uuid {
            reference.to_string()
        } else {
            crate::release::resolve_path(project, reference)
                .display()
                .to_string()
        }
    };
    let want = Want {
        kind: Kind::Development,
        team: &team,
        bundle_id: &config.app.id,
        certificates: &certificates,
        device: device.map(|d| d.udid.as_str()),
        now: crate::ios::now_unix(),
    };
    let chosen = profile::choose(&profile_ref, &profile::search_dirs(&ctx.env), &want)
        .map_err(|e| e.evidence(project.config.evidence("ios.signing.development")))?
        .profile;
    let identity = candidates
        .into_iter()
        .find(|i| {
            chosen
                .certificates
                .iter()
                .any(|c| c.eq_ignore_ascii_case(&i.sha1))
        })
        .ok_or_else(|| internal("the chosen profile names none of the candidate identities"))?;
    ctx.rep.check(match &identity.problem {
        // Named explicitly and taken as it is; the device refuses an app
        // whose signature it cannot trust, at install.
        Some(problem) => Check::warn(
            CheckId::IosSignNoIdentity,
            format!(
                "{} is not a valid identity ({problem}); the device will refuse the app",
                identity.label()
            ),
        ),
        None => Check::pass(
            CheckId::IosSignNoIdentity,
            format!("signing identity {}", identity.label()),
        ),
    });
    ctx.rep.check(Check::pass(
        CheckId::IosSignNoProfile,
        format!(
            "development profile {}, expires {}",
            chosen.label(),
            chosen.expires.as_deref().unwrap_or("?")
        ),
    ));
    Ok(Signing {
        team,
        identity,
        profile: chosen,
        keychain,
    })
}

// ---- build ---------------------------------------------------------------------------------

/// A signed device bundle.
struct Built {
    app: PathBuf,
    bin: String,
}

fn build_app(
    ctx: &mut Ctx,
    project: &Project,
    xcode: &Xcode,
    release: bool,
    signing: &Signing,
) -> Result<Built> {
    let profile = profile_name(release);
    let package = project.package_for(PLATFORM)?.clone();
    let bin = project.bin_for(PLATFORM)?;
    let config = project.config.config.clone();

    let toolchain = crate::toolchain::active(ctx, package.dir())?;
    for check in crate::toolchain::check_targets(&toolchain, &[TRIPLE.to_string()]) {
        if check.failed() {
            return Err(check.into_error());
        }
        ctx.rep.check(check);
    }
    ctx.rep.set(
        "tools",
        json!({"xcode": xcode.display(), "rustc": toolchain.rustc_version()}),
    );

    let prepared = ctx.deployment_target(
        project,
        &package.name,
        Some(TRIPLE),
        profile,
        &config.ios.min_os,
    )?;
    let mut env = Vec::new();
    let mut stamp = None;
    if let Some((pair, deployment)) = prepared {
        env.push(pair);
        stamp = Some(deployment);
    }
    let mut invocation = Invocation::new("build", &package.manifest_path, &package.name);
    invocation.select = Select::Bin(bin.clone());
    invocation.triple = Some(TRIPLE.to_string());
    invocation.profile = profile.to_string();
    let output = ctx.cargo("cargo.build", &invocation, &env)?;
    if let Some(stamp) = stamp {
        let _ = stamp.write();
    }
    let exe = output
        .executable(&bin)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| {
            crate::cargo::artifacts_dir(&project.target_dir, Some(TRIPLE), profile).join(&bin)
        });
    if !exe.is_file() {
        return Err(internal(format!(
            "cargo reported success but {} does not exist",
            crate::paths::display(&exe)
        )));
    }
    let versions = macho::build_versions(&exe).map_err(|e| {
        IcmError::new(CheckId::BuildWrongPlatform, e).evidence(Evidence::file(&exe))
    })?;
    if versions.is_empty() || versions.iter().any(|v| v.platform != PLATFORM_IOS) {
        return Err(IcmError::new(
            CheckId::IosMachoPlatform,
            format!(
                "{} is not built for IOS (devices)",
                crate::paths::display(&exe)
            ),
        )
        .evidence(Evidence::file(&exe)));
    }
    ctx.rep.check(Check::pass(
        CheckId::IosMachoPlatform,
        format!(
            "IOS {} minos {} sdk {}",
            versions[0].arch,
            versions[0].minos_string(),
            versions[0].sdk_string()
        ),
    ));

    let dt = dt::read(ctx, xcode)?;
    let gen_dir = project.gen_dir(PLATFORM, profile);
    let out_dir = project.build_dir(PLATFORM, profile);
    let device = bundle::assemble(
        ctx,
        project,
        xcode,
        &bundle::Inputs {
            gen_dir: &gen_dir,
            out_dir: &out_dir,
            exe: &exe,
            bin: &bin,
            cargo_version: &package.version,
            dt: &dt,
            release: false,
            extra: vec![(
                signing.profile.path.clone(),
                "embedded.mobileprovision".to_string(),
            )],
        },
    )?;
    let app = device.app.clone();
    let lint = bundle::lint(ctx, &app)?;
    let checks = std::iter::once(lint).chain(bundle::plist_checks(&app, &device.info));
    for check in checks {
        if check.failed() {
            return Err(check.into_error());
        }
        ctx.rep.check(check);
    }

    let expected = entitlements::development(&config, &signing.team);
    let missing = entitlements::not_in_profile(&expected, &signing.profile.entitlements);
    let entitlements_path = gen_dir.join("entitlements.plist");
    std::fs::write(
        &entitlements_path,
        crate::platform::ios_sim::plist::to_xml(&Value::Object(expected)),
    )
    .map_err(|e| io("write", &entitlements_path, e))?;
    if !missing.is_empty() {
        return Err(IcmError::new(
            CheckId::IosEntitlementsNotInProfile,
            format!(
                "{} does not allow: {}",
                signing.profile.label(),
                missing.join("; ")
            ),
        )
        .evidence(Evidence::file(&entitlements_path)));
    }
    ctx.rep.check(Check::pass(
        CheckId::IosEntitlementsNotInProfile,
        "the development profile allows every entitlement",
    ));

    codesign::clear_xattrs(ctx, &app)?;
    codesign::sign(
        ctx,
        &app,
        &signing.identity.sha1,
        Some(&entitlements_path),
        signing.keychain.as_deref(),
    )?;
    let outcome = codesign::verify(ctx, "ios.codesign.verify", &app)?;
    if !outcome.success() {
        return Err(ctx.step_failure("ios.codesign.verify", CheckId::IosSignVerify, &outcome));
    }
    ctx.rep.check(Check::pass(
        CheckId::IosSignVerify,
        format!("signed with {}", signing.identity.label()),
    ));
    ctx.rep.artifact("bundle", &app);
    Ok(Built { app, bin })
}

// ---- plans ---------------------------------------------------------------------------------

/// What `--dry-run` prints: the steps, with the device and identity
/// described (a dry run lists no device and reads no keychain).
fn plan(project: &Project, release: bool, selector: Option<&str>, launch: bool) -> Plan {
    let config = &project.config.config;
    let profile = profile_name(release);
    let device = selector.unwrap_or("<the single connected device>");
    let xcrun = || Cmd::tool("xcrun");
    let mut plan = Plan::new();
    plan.push(
        Step::exec(
            "devicectl.list",
            xcrun().args(["devicectl", "list", "devices", "--json-output", "<tmp>"]),
        )
        .gate(CheckId::IosDeviceNotFound)
        .gate(CheckId::IosDeviceDeveloperModeOff),
    );
    plan.push(
        Step::exec("security.find_identity", identity::list_cmd(None))
            .gate(CheckId::IosSignNoIdentity)
            .gate(CheckId::IosSignNoProfile),
    );
    if let (Ok(package), Ok(bin)) = (project.package_for(PLATFORM), project.bin_for(PLATFORM)) {
        let mut invocation = Invocation::new("build", &package.manifest_path, &package.name);
        invocation.select = Select::Bin(bin);
        invocation.triple = Some(TRIPLE.to_string());
        invocation.profile = profile.to_string();
        plan.push(
            Step::exec(
                "cargo.build",
                invocation
                    .cmd()
                    .env("IPHONEOS_DEPLOYMENT_TARGET", &config.ios.min_os),
            )
            .gate(CheckId::IosMachoPlatform)
            .on_fail(CheckId::BuildCompileError),
        );
    }
    plan.push(
        Step::exec(
            "ios.actool",
            xcrun().args([
                "actool",
                "Assets.xcassets",
                "--compile",
                "actool-out",
                "--platform",
                "iphoneos",
                "--minimum-deployment-target",
                &config.ios.min_os,
            ]),
        )
        .on_fail(CheckId::IosActoolFailed),
    );
    plan.push(Step::internal(
        "ios.generate",
        "Info.plist with the DT* keys, PrivacyInfo.xcprivacy, the development entitlements (checked against the profile), embedded.mobileprovision",
    ));
    let app = crate::platform::ios_sim::bundle::bundle_name(&config.app.name);
    plan.push(
        Step::exec(
            "ios.codesign",
            codesign::sign_cmd(
                Path::new(&app),
                "<Apple Development identity>",
                Some(Path::new("entitlements.plist")),
                None,
            ),
        )
        .gate(CheckId::IosSignVerify)
        .on_fail(CheckId::IosSignKeychainPrompt),
    );
    if launch {
        plan.push(
            Step::exec(
                "devicectl.install",
                xcrun()
                    .args(["devicectl", "device", "install", "app", "--device", device])
                    .arg(&app),
            )
            .on_fail(CheckId::IosDeviceInstallFailed),
        );
        plan.push(
            Step::exec(
                "devicectl.launch",
                xcrun()
                    .args([
                        "devicectl",
                        "device",
                        "process",
                        "launch",
                        "--device",
                        device,
                        "--terminate-existing",
                        "--console",
                        &config.app.id,
                    ])
                    .env("DEVICECTL_CHILD_ICM_EVENTS", "1")
                    .env("DEVICECTL_CHILD_RUST_BACKTRACE", "1"),
            )
            .gate(CheckId::RunReady)
            .on_fail(CheckId::IosDeviceLaunchFailed),
        );
        plan.push(
            Step::exec(
                "devicectl.screenshot",
                xcrun().args([
                    "devicectl",
                    "device",
                    "capture",
                    "screenshot",
                    "--device",
                    device,
                    "--destination",
                    "screen.png",
                ]),
            )
            .gate(CheckId::RunScreenBlank),
        );
    }
    plan
}

/// `--dry-run` for commands that act on the session.
fn dry_run(ctx: &Ctx, steps: &[(&str, String)]) -> Result<()> {
    let mut plan = Plan::new();
    for (name, description) in steps {
        plan.push(Step::internal(name, description));
    }
    plan.report(ctx);
    ctx.rep
        .summary("the plan (--dry-run: nothing ran on the device)");
    Ok(())
}

// ---- build / run ---------------------------------------------------------------------------

/// `icm build ios-device`.
pub fn build(ctx: &mut Ctx, args: &BuildArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    ctx.rep.set(
        "profile",
        json!(crate::cargo::profile_dir(profile_name(args.release))),
    );
    if ctx.dry_run() {
        plan(&project, args.release, args.device.as_deref(), false).report(ctx);
        ctx.rep
            .summary("the plan for the device build (--dry-run: nothing was built)");
        return Ok(());
    }
    let xcode = host_xcode(ctx)?;
    let _lock = ctx.lock_platform(PLATFORM)?;
    let device = match &args.device {
        Some(selector) => Some(device(ctx, &xcode, Some(selector))?),
        None => None,
    };
    let signing = signing(ctx, &project, device.as_ref())?;
    let built = build_app(ctx, &project, &xcode, args.release, &signing)?;
    ctx.rep.summary(format!(
        "built and signed {} for iOS devices",
        crate::paths::display(&built.app)
    ));
    ctx.rep.next(
        "icm run ios-device --json -q",
        "install and launch it on the connected device",
    );
    Ok(())
}

/// The `ICM_EVENT` lines in a text.
fn events(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("ICM_EVENT "))
        .filter_map(|json| serde_json::from_str(json).ok())
        .collect()
}

fn of_kind<'a>(events: &'a [Value], kind: &str) -> Option<&'a Value> {
    events.iter().find(|e| e["kind"] == kind)
}

/// The console text and the file it is in.
fn console(files: &Path) -> String {
    let mut text = std::fs::read_to_string(files.join("console.log")).unwrap_or_default();
    let stderr = std::fs::read_to_string(files.join("console.stderr")).unwrap_or_default();
    if !stderr.trim().is_empty() {
        text.push('\n');
        text.push_str(&stderr);
    }
    text
}

/// How the app came up.
enum Ready {
    /// `ICM_EVENT ready`.
    Event(Value, Duration),
    /// The process probe.
    Probe(Duration),
}

/// The first line of a panic in the console, with its line number.
fn panic_line(text: &str) -> Option<(usize, String)> {
    text.lines()
        .enumerate()
        .find(|(_, line)| line.contains("panicked at"))
        .map(|(index, line)| (index + 1, line.trim().to_string()))
}

#[allow(clippy::too_many_arguments)]
fn wait_ready(
    ctx: &Ctx,
    xcode: &Xcode,
    device: &Device,
    console_pid: i32,
    files: &Path,
    app: &str,
    bin: &str,
    wait: Duration,
) -> Result<Ready> {
    let started = Instant::now();
    let console_log = files.join("console.log");
    let mut polls = 0;
    loop {
        let text = console(files);
        let found = events(&text);
        if of_kind(&found, "panic").is_some() || text.contains("panicked at") {
            sleep_checked(Duration::from_millis(500))?;
            let text = console(files);
            let (line, excerpt) = panic_line(&text).unwrap_or((0, "the app panicked".into()));
            let mut evidence = Evidence::file(&console_log);
            if line > 0 {
                evidence = Evidence::line(&console_log, line as u32, excerpt.clone());
            }
            return Err(IcmError::new(
                CheckId::RunAppPanicked,
                format!("the app panicked on {}: {excerpt}", device.name),
            )
            .evidence(evidence));
        }
        if let Some(ready) = of_kind(&found, "ready") {
            return Ok(Ready::Event(ready.clone(), started.elapsed()));
        }
        crate::sessions::reap(console_pid);
        if !crate::signals::alive(console_pid) {
            let tail: Vec<&str> = text.lines().rev().take(5).collect();
            return Err(IcmError::new(
                CheckId::RunAppDied,
                format!(
                    "the app ended on {} before its first frame (devicectl's console exited){}",
                    device.name,
                    if tail.is_empty() {
                        String::new()
                    } else {
                        format!(
                            ": {}",
                            tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
                        )
                    }
                ),
            )
            .evidence(Evidence::file(&console_log)));
        }
        if of_kind(&found, "start").is_none() && started.elapsed() >= PROBE_AFTER {
            let processes = devicectl_json(
                ctx,
                xcode,
                &["device", "info", "processes", "--device", &device.udid],
                "processes",
            )
            .unwrap_or_default();
            if devicectl::app_pids(&processes, app, bin).is_empty() {
                polls = 0;
            } else {
                polls += 1;
                if polls >= PROBE_POLLS {
                    return Ok(Ready::Probe(started.elapsed()));
                }
            }
        }
        if started.elapsed() >= wait {
            return Err(IcmError::new(
                CheckId::RunNotReady,
                format!(
                    "the app did not report its first frame on {} within {}",
                    device.name,
                    crate::time::format_duration(wait)
                ),
            )
            .evidence(Evidence::file(&console_log))
            .fix(
                "Read the console log; an app on a framework without ICM_EVENT is found by the process probe after 5 s.",
                &["icm logs ios-device --json -q"],
            ));
        }
        sleep_checked(Duration::from_millis(500))?;
    }
}

/// The pid the device lists for the installed app, for a run whose app
/// reported no `start` event (the process probe found it, or only `ready`
/// came). It is the one listed process whose executable is `<install>/<bin>`,
/// the path `stop` compares before it terminates the pid
/// ([`app_process`]), so the pid is one `stop` can confirm. None when the
/// listing cannot be had (`devicectl::running_processes` fails it for a
/// process whose pid is not an integer and that may be the app, and for an
/// unreadable answer), names no such process, or names more than one: the
/// record then names no pid and no terminate command, and `stop` removes it
/// without asking the device, as DESIGN.md's ios-device session says. A
/// process of another program with a non-integer pid does not fail it.
fn listed_app_pid(
    ctx: &Ctx,
    xcode: &Xcode,
    device: &Device,
    install: Option<&str>,
    binary: &str,
) -> Option<i64> {
    let install = install?;
    let text = devicectl_json(
        ctx,
        xcode,
        &["device", "info", "processes", "--device", &device.udid],
        "processes",
    )
    .ok()?;
    let mut pids = devicectl::running_processes(&text, Some(install), binary)
        .ok()?
        .into_iter()
        .filter(|process| {
            process.executable.as_deref().is_some_and(|path| {
                devicectl::match_installed_executable(path, install, binary)
                    == devicectl::Match::Same
            })
        })
        .map(|process| process.pid);
    let pid = pids.next()?;
    pids.next().is_none().then_some(pid)
}

/// Parses `--env K=V` values.
fn app_env(values: &[String]) -> Result<Vec<(String, String)>> {
    values
        .iter()
        .map(|value| {
            value
                .split_once('=')
                .filter(|(key, _)| !key.is_empty())
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .ok_or_else(|| {
                    IcmError::new(
                        CheckId::UsageBadArgs,
                        format!("--env `{value}` is not KEY=VALUE"),
                    )
                })
        })
        .collect()
}

/// The command that terminates the app's process on the device, which `run`
/// stores in the session's `stop`. `stop` runs it only when [`app_process`]
/// finds the device listing that pid as the app.
fn terminate_argv(udid: &str, pid: i64) -> Vec<String> {
    [
        "xcrun",
        "devicectl",
        "device",
        "process",
        "terminate",
        "--device",
        udid,
        "--pid",
        &pid.to_string(),
    ]
    .map(str::to_string)
    .to_vec()
}

/// What the device says about the app process a session recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppProcess {
    /// The device lists the recorded pid running the app's executable, so
    /// the recorded stop command ends the app icm launched. (devicectl
    /// lists no start time to tell a later process with the same pid and
    /// executable apart; the pid and executable together are what there is.)
    Running,
    /// The device answered, and the recorded pid is not the app: no process
    /// has it, or another one does (what is said). Nothing to terminate.
    NotRunning(String),
    /// icm could not tell: the device or devicectl would not answer, the
    /// record does not say what to look for, the listing does not read the
    /// recorded pid's executable as the app's or another process's (why), or
    /// the listing names no usable executable for the pid (why). The recorded
    /// pid may name any process on the phone, so nothing is terminated.
    Unconfirmed(String),
}

/// Whether the recorded app pid, listed by the device, is the app: the pid
/// must run `<install>/<binary>`, the app's executable in the directory it was
/// installed in, container included. A copy of the app installed elsewhere is
/// another process, and a process of the app under another pid is not what
/// icm launched, so neither is the one to end. A listed pid whose executable
/// icm cannot read is unconfirmed, not another process: a missing answer is no
/// proof that the app is gone. Without the install's directory nothing can be
/// the app, so a listed pid is unconfirmed.
fn judge_app_process(
    listing: &[devicectl::Listed],
    install: Option<&str>,
    binary: &str,
    pid: i64,
) -> AppProcess {
    // A process with no executable named does not run the app's binary.
    let compare = |process: &devicectl::Listed| match (process.executable.as_deref(), install) {
        (None, _) => devicectl::Match::Different,
        (Some(path), Some(dir)) => devicectl::match_installed_executable(path, dir, binary),
        (Some(_), None) => devicectl::Match::Unknown,
    };
    match listing.iter().find(|process| process.pid == pid) {
        Some(process) if process.usable_program().is_none() => AppProcess::Unconfirmed(format!(
            "the device lists pid {pid} {}, so icm cannot tell whether that process is the app",
            process.why_no_usable_path()
        )),
        Some(process) if install.is_none() => AppProcess::Unconfirmed(format!(
            "the record does not say where the app is installed, so icm cannot tell whether pid {pid} ({}) is the app",
            process.program()
        )),
        Some(process) => match compare(process) {
            devicectl::Match::Same => AppProcess::Running,
            devicectl::Match::Different => AppProcess::NotRunning(format!(
                "pid {pid} is {} on the device now, not the app",
                process.program()
            )),
            // Its executable does not read as a path icm can compare, so
            // the pid may still be the app: nothing may be terminated.
            devicectl::Match::Unknown => AppProcess::Unconfirmed(format!(
                "pid {pid} is {} on the device now, and icm cannot tell from that path whether it is the app",
                process.program()
            )),
        },
        None => {
            let others: Vec<String> = listing
                .iter()
                .filter(|process| compare(process) == devicectl::Match::Same)
                .map(|process| process.pid.to_string())
                .collect();
            AppProcess::NotRunning(if others.is_empty() {
                format!("no process has pid {pid} on the device")
            } else {
                format!(
                    "no process has pid {pid} on the device; the app runs as pid {}, which icm did not launch",
                    others.join(", ")
                )
            })
        }
    }
}

/// Asks the device whether the app process `session` recorded is still the
/// app (`devicectl device info processes`), before `stop` runs the recorded
/// terminate command: a pid on the phone is reused like any other, and the
/// command would end whatever process has it now. Anything that stops the
/// question from being answered, the record from saying what to look for,
/// or the listing from saying what runs under the pid, is
/// [`AppProcess::Unconfirmed`], never a reason to terminate.
pub fn app_process(ctx: &Ctx, session: &Session) -> AppProcess {
    let unconfirmed = |why: String| AppProcess::Unconfirmed(why);
    let Some(device) = session.device.as_ref().filter(|d| !d.id.is_empty()) else {
        return unconfirmed("the record names no device".into());
    };
    let Some(pid) = session.extra.get("app_pid").and_then(Value::as_i64) else {
        return unconfirmed("the record names no app pid".into());
    };
    let app = |key: &str| {
        session
            .app
            .as_ref()
            .and_then(|app| app.get(key))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    };
    let Some(executable) = app("bin") else {
        return unconfirmed("the record does not name the app's executable".into());
    };
    // Where devicectl installed the app (`install_url`, from the install
    // output): the listed executable must run from there.
    let install = app("install_url");
    // The stored command is the one for this device and pid: a record that
    // says otherwise would end a process the check below did not look at.
    let expected = terminate_argv(&device.id, pid);
    if let Some(other) = session.stop.iter().find(|argv| **argv != expected) {
        return unconfirmed(format!(
            "the record's stop command (`{}`) is not the one for pid {pid} on {}",
            other.join(" "),
            device.id
        ));
    }
    let xcode = match host_xcode(ctx) {
        Ok(xcode) => xcode,
        Err(error) => return unconfirmed(error.detail),
    };
    let text = match devicectl_json(
        ctx,
        &xcode,
        &["device", "info", "processes", "--device", &device.id],
        "processes",
    ) {
        Ok(text) => text,
        Err(error) => return unconfirmed(error.detail),
    };
    match devicectl::running_processes(&text, install, executable) {
        Ok(listing) => judge_app_process(&listing, install, executable, pid),
        Err(why) => unconfirmed(why),
    }
}

/// Ends this project's previous device session (its console process; the
/// app itself is replaced by `--terminate-existing`). A console process
/// that icm cannot tell from another one (its identity is unavailable, or
/// the OS will not describe it now) is not signalled, and a WARN says it is
/// left running, as `stop` does.
fn end_previous(ctx: &Ctx, sessions_dir: &Path) {
    let path = session::path(sessions_dir, PLATFORM);
    if let Ok(previous) = session::read(&path) {
        let written = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        for pid in previous.all_pids() {
            if previous.is_ours(pid, written) {
                let _ = session::terminate(pid, Duration::from_secs(3));
            }
        }
        for (pid, why) in previous.unverified_pids() {
            ctx.rep.check(session::unverified_check(
                "the previous run's console process",
                pid,
                &why,
            ));
        }
        let _ = std::fs::remove_file(&path);
    }
}

/// Captures a screenshot into `png`.
fn capture(ctx: &Ctx, xcode: &Xcode, udid: &str, png: &Path) -> Result<()> {
    let outcome = ctx.step(
        "devicectl.screenshot",
        &devicectl(xcode)
            .args([
                "device",
                "capture",
                "screenshot",
                "--device",
                udid,
                "--destination",
            ])
            .arg(png)
            .timeout(Duration::from_secs(60)),
    )?;
    if !outcome.success() || !png.is_file() {
        return Err(ctx.step_failure("devicectl.screenshot", CheckId::ToolFailed, &outcome));
    }
    Ok(())
}

/// Writes `app.log` and `logs.ndjson` for the console into `dir`, redacted.
fn write_logs(files: &Path, dir: &Path, launched: &str, pid: Option<i32>) -> Vec<records::Record> {
    let mut parser = records::Parser::new(true, launched);
    let records = parser.parse(&console(files));
    let log: String = records
        .iter()
        .map(|r| format!("{}\n", r.to_line()))
        .collect();
    let ndjson: String = records
        .iter()
        .map(|r| format!("{}\n", record_json(r, pid)))
        .collect();
    let _ = process::write_redacted(&dir.join("app.log"), &log);
    let _ = process::write_redacted(&dir.join("logs.ndjson"), &ndjson);
    records
}

fn record_json(record: &records::Record, pid: Option<i32>) -> Value {
    let mut value = record.to_json(pid);
    value["platform"] = json!(PLATFORM);
    value
}

/// `icm run ios-device`.
pub fn run(ctx: &mut Ctx, args: &RunArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    ctx.rep.latest(PLATFORM);
    ctx.rep.set(
        "profile",
        json!(crate::cargo::profile_dir(profile_name(args.release))),
    );
    let extra_env = app_env(&args.env)?;
    if ctx.dry_run() {
        plan(&project, args.release, args.device.as_deref(), true).report(ctx);
        ctx.rep.summary(
            "the plan for a device run (--dry-run: no device was listed, nothing was built or installed)",
        );
        return Ok(());
    }
    let xcode = host_xcode(ctx)?;
    let _lock = ctx.lock_platform(PLATFORM)?;
    let config = project.config.config.clone();

    // Device and signing before the long build.
    let device = device(ctx, &xcode, args.device.as_deref())?;
    ctx.rep.set("device", device.to_json());
    let signing = signing(ctx, &project, Some(&device))?;
    let built = if args.no_build {
        let app = project
            .build_dir(PLATFORM, profile_name(args.release))
            .join(crate::platform::ios_sim::bundle::bundle_name(
                &config.app.name,
            ));
        if !app.join("_CodeSignature").is_dir() {
            return Err(IcmError::new(
                CheckId::UsageBadArgs,
                format!(
                    "--no-build: there is no signed build at {}",
                    crate::paths::display(&app)
                ),
            )
            .fix("Drop --no-build.", &["icm run ios-device"]));
        }
        Built {
            app,
            bin: project.bin_for(PLATFORM)?,
        }
    } else {
        build_app(ctx, &project, &xcode, args.release, &signing)?
    };

    // Install.
    let run_dir = ctx
        .rep
        .run_dir()
        .ok_or_else(|| internal("the run directory is not attached"))?;
    let outcome = ctx.step(
        "devicectl.install",
        &devicectl(&xcode)
            .args(["device", "install", "app", "--device", &device.udid])
            .arg(&built.app)
            .arg("--json-output")
            .arg(run_dir.join("install.json"))
            .timeout(Duration::from_secs(600)),
    )?;
    process::redact_in_place(&run_dir.join("install.json"));
    if !outcome.success() {
        return Err(ctx.step_failure(
            "devicectl.install",
            CheckId::IosDeviceInstallFailed,
            &outcome,
        ));
    }
    // Where devicectl installed the app: the directory its executable runs
    // from, which `stop` compares a listed executable with. Without it, `stop`
    // cannot tell the app's process from another's, so it terminates nothing.
    let install_url = std::fs::read_to_string(run_dir.join("install.json"))
        .ok()
        .and_then(|text| devicectl::installation_url(&text, &config.app.id));

    // Launch with the console attached, detached from icm.
    let sessions_dir = project.sessions_dir();
    end_previous(ctx, &sessions_dir);
    let run_id = ctx.rep.run_id();
    let files = sessions_dir.join(PLATFORM).join(&run_id);
    std::fs::create_dir_all(&files).map_err(|e| io("create", &files, e))?;
    let launched = records::timestamp(SystemTime::now());
    let mut launch = devicectl(&xcode)
        .args([
            "device",
            "process",
            "launch",
            "--device",
            &device.udid,
            "--terminate-existing",
            "--console",
            &config.app.id,
        ])
        .env("DEVICECTL_CHILD_ICM_EVENTS", "1")
        .env("DEVICECTL_CHILD_ICM_RUN_ID", &run_id)
        .env("DEVICECTL_CHILD_RUST_BACKTRACE", "1");
    for (key, value) in &extra_env {
        launch = launch.env(format!("DEVICECTL_CHILD_{key}"), value);
    }
    ctx.rep.progress(format!("launching: {}", launch.display()));
    let console_pid = process::spawn_detached(
        &launch,
        &files.join("console.log"),
        &files.join("console.stderr"),
    )
    .map_err(|error| IcmError::new(CheckId::IosDeviceLaunchFailed, error.to_string()))?
        as i32;
    // What the console process is now, for the record `stop` checks its pid
    // against (a pid is reused once the process ends).
    let console_identity = Some(crate::procid::capture(console_pid));
    if let Some(check) = crate::session::unavailable_check(
        "the `devicectl` console process",
        console_pid,
        console_identity.as_ref(),
    ) {
        ctx.rep.check(check);
    }
    // The secret values the app was handed (its `--env`) stay known to
    // later commands that read its console.
    let _ = process::keep_secrets(&files, &process::handed_secrets(&launch));

    let app_name = crate::platform::ios_sim::bundle::bundle_name(&config.app.name);
    let ready = wait_ready(
        ctx,
        &xcode,
        &device,
        console_pid,
        &files,
        &app_name,
        &built.bin,
        args.wait_ready,
    );
    let text = console(&files);
    let found = events(&text);
    let app_pid = of_kind(&found, "start")
        .and_then(|start| start["pid"].as_i64())
        .map(|pid| pid as i32);
    let records = write_logs(&files, &run_dir, &launched, app_pid);
    let console_copy = run_dir.join("console.log");
    let _ = process::copy_redacted(&files.join("console.log"), &console_copy);
    ctx.rep.artifact("app_log", &run_dir.join("app.log"));
    ctx.rep.artifact("logs", &run_dir.join("logs.ndjson"));

    let ready = match ready {
        Ok(ready) => ready,
        Err(mut error) => {
            // The evidence names the redacted copy, not the live console.
            let live = crate::paths::display(&files.join("console.log"));
            for evidence in &mut error.evidence {
                if evidence.path == live {
                    evidence.path = crate::paths::display(&console_copy);
                }
            }
            let _ = session::terminate(console_pid, Duration::from_secs(3));
            ctx.rep.set(
                "process",
                json!({"pid": app_pid, "alive": false, "ready": {"source": "none", "ms": null}}),
            );
            return Err(error);
        }
    };
    // An app that reported no `start` event has no pid from it: the record
    // then takes the pid the device lists for the installed app, so `stop`
    // can ask the device about that pid and terminate it.
    let app_pid = app_pid.or_else(|| {
        listed_app_pid(ctx, &xcode, &device, install_url.as_deref(), &built.bin)
            .map(|pid| pid as i32)
    });
    let (source, ms, scale) = match &ready {
        Ready::Event(event, elapsed) => (
            "icm_event",
            event["ms"].as_u64().unwrap_or(elapsed.as_millis() as u64),
            event["window"]["scale"].as_f64().unwrap_or(3.0),
        ),
        Ready::Probe(elapsed) => ("probe", elapsed.as_millis() as u64, 3.0),
    };
    ctx.rep.check(Check::pass(
        CheckId::RunReady,
        format!(
            "first frame on {} after {ms} ms (source: {source})",
            device.name
        ),
    ));
    ctx.rep.set(
        "process",
        json!({"pid": app_pid, "alive": true, "ready": {"source": source, "ms": ms}}),
    );

    // The screenshot.
    let mut screen = Value::Null;
    if !args.no_shot {
        sleep_checked(args.settle)?;
        let png = run_dir.join("screen.png");
        capture(ctx, &xcode, &device.udid, &png)?;
        let shot = crate::preview::finish(
            &ctx.rep,
            &png,
            scale,
            args.expect_content,
            &["icm logs ios-device --json -q"],
        )
        .map_err(internal)?;
        screen = shot.screen.to_json();
    }

    // The session, for logs, shot, stop and ps.
    let mut stop = Vec::new();
    if let Some(pid) = app_pid {
        stop.push(terminate_argv(&device.udid, i64::from(pid)));
    }
    let mut record = Session {
        v: 1,
        platform: PLATFORM.to_string(),
        run: Some(run_id.clone()),
        started: Some(crate::time::Utc::now().rfc3339()),
        pid: Some(console_pid),
        identity: console_identity,
        app: Some(json!({
            "id": config.app.id,
            "bin": built.bin,
            "bundle": app_name,
            "install_url": install_url,
        })),
        device: Some(SessionDevice {
            kind: "device".into(),
            id: device.udid.clone(),
            name: device.name.clone(),
            managed: false,
        }),
        stop,
        ..Session::default()
    };
    let _ = record
        .extra
        .insert("console".into(), json!(files.display().to_string()));
    let _ = record.extra.insert("launched".into(), json!(launched));
    let _ = record.extra.insert("app_pid".into(), json!(app_pid));
    let _ = record.extra.insert("screen".into(), screen);
    let path = session::write(&sessions_dir, &record).map_err(|e| io("write", &sessions_dir, e))?;
    ctx.rep.set("session", json!(crate::paths::display(&path)));

    ctx.rep.summary(format!(
        "{} is running on {}; first frame after {ms} ms (source: {source}); {} log record(s)",
        config.app.id,
        device.label(),
        records.len()
    ));
    ctx.rep
        .next("icm logs ios-device --json -q", "the app's console output");
    ctx.rep
        .next("icm shot ios-device --json -q", "a new screenshot");
    ctx.rep
        .next("icm stop ios-device --json -q", "terminate the app");
    Ok(())
}

// ---- session commands ----------------------------------------------------------------------

fn no_session() -> IcmError {
    IcmError::new(
        CheckId::RunNoSession,
        "no ios-device session: run the app first",
    )
    .fix(
        "Start the app on the device.",
        &["icm run ios-device --json -q"],
    )
}

fn read_session(ctx: &mut Ctx) -> Result<(Project, Session)> {
    let project = ctx.project()?.clone();
    let path = session::path(&project.sessions_dir(), PLATFORM);
    let record = session::read(&path).map_err(|_| no_session())?;
    Ok((project, record))
}

/// `icm shot ios-device`.
pub fn shot(ctx: &mut Ctx, args: &ShotArgs) -> Result<()> {
    if ctx.dry_run() {
        return dry_run(
            ctx,
            &[(
                "devicectl.screenshot",
                "xcrun devicectl device capture screenshot on the session's device".to_string(),
            )],
        );
    }
    let (_, record) = read_session(ctx)?;
    let xcode = host_xcode(ctx)?;
    ctx.rep.latest(PLATFORM);
    let device = record.device.clone().ok_or_else(no_session)?;
    let run_dir = ctx
        .rep
        .run_dir()
        .ok_or_else(|| internal("the run directory is not attached"))?;
    let name = args.name.clone().unwrap_or_else(|| "screen".to_string());
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return Err(IcmError::new(
            CheckId::UsageBadArgs,
            format!("--name `{name}` must be a plain file name"),
        ));
    }
    let png = run_dir.join(format!("{name}.png"));
    capture(ctx, &xcode, &device.id, &png)?;
    let scale = record
        .extra
        .get("screen")
        .and_then(|s| s.get("scale"))
        .and_then(Value::as_f64)
        .unwrap_or(3.0);
    let shot = crate::preview::finish(&ctx.rep, &png, scale, false, &[]).map_err(internal)?;
    if let Some(out) = &args.out {
        if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::copy(&png, out).map_err(|error| {
            IcmError::new(
                CheckId::UsageBadArgs,
                format!("cannot write --out {}: {error}", out.display()),
            )
        })?;
        ctx.rep.artifact("out", out);
    }
    ctx.rep.set(
        "device",
        json!({"kind": "device", "udid": device.id, "name": device.name}),
    );
    ctx.rep.summary(format!(
        "screenshot of {} ({}x{} px)",
        device.name, shot.screen.px.0, shot.screen.px.1
    ));
    Ok(())
}

/// `icm logs ios-device`: the console output of the session's app.
pub fn logs(ctx: &mut Ctx, args: &LogsArgs) -> Result<()> {
    if ctx.dry_run() {
        return dry_run(
            ctx,
            &[(
                "ios-device.logs",
                "read the session's devicectl console log".to_string(),
            )],
        );
    }
    let (_, record) = read_session(ctx)?;
    let files = record
        .extra
        .get("console")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(no_session)?;
    if args.raw {
        let text = console(&files);
        let lines: Vec<&str> = text.lines().collect();
        let tail = lines[lines.len().saturating_sub(args.tail)..].join("\n");
        ctx.rep.content(format!("{tail}\n"));
        ctx.rep.summary("the device console's last lines");
        return Ok(());
    }
    let launched = record
        .extra
        .get("launched")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let filter = records::Filter::new(args.level, &args.since, args.grep.as_deref())
        .map_err(|e| IcmError::new(CheckId::UsageBadArgs, e))?;
    let mut parser = records::Parser::new(true, &launched);
    let all = parser.parse(&console(&files));
    let pid = record
        .extra
        .get("app_pid")
        .and_then(Value::as_i64)
        .map(|p| p as i32);
    let matched: Vec<&records::Record> = all.iter().filter(|r| filter.keeps(r)).collect();
    let shown = &matched[matched.len().saturating_sub(args.tail)..];
    for record in shown {
        let mut event = record_json(record, pid);
        event["type"] = json!("log");
        ctx.rep.emit(event);
    }
    ctx.rep.set(
        "records",
        Value::Array(shown.iter().map(|r| record_json(r, pid)).collect()),
    );
    ctx.rep.set(
        "counts",
        json!({"total": all.len(), "matched": matched.len(), "shown": shown.len()}),
    );
    ctx.rep.summary(format!(
        "{} of {} console record(s) from the device",
        shown.len(),
        all.len()
    ));
    Ok(())
}

/// `icm input ios-device`: no input path on physical devices in v1.
pub fn input(_ctx: &mut Ctx, _args: &InputArgs) -> Result<()> {
    Err(IcmError::new(
        CheckId::InputUnsupported,
        "input to an app on a physical iOS device is not supported (no agent bridge on devices in v1); drive the UI on the simulator or headlessly",
    )
    .fix(
        "Use the simulator or the headless harness.",
        &["icm input ios-sim --help", "icm ui --headless tree --json"],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, connected: bool) -> Device {
        let value = devicectl::fixture_device(name, &format!("UDID-{name}"), connected, true);
        devicectl::parse_devices(&json!({"result": {"devices": [value]}}).to_string()).unwrap()[0]
            .clone()
    }

    #[test]
    fn the_device_is_chosen_or_the_error_says_why() {
        let one = vec![device("A", true), device("B", false)];
        assert_eq!(choose(&one, None).unwrap().name, "A");
        assert_eq!(choose(&one, Some("UDID-B")).unwrap().name, "B");
        assert_eq!(
            choose(&one, Some("Z")).unwrap_err().id,
            "ios.device.not_found"
        );
        let two = vec![device("A", true), device("B", true)];
        assert_eq!(choose(&two, None).unwrap_err().id, "ios.device.ambiguous");
        let none = vec![device("B", false)];
        let error = choose(&none, None).unwrap_err();
        assert_eq!(error.id, "ios.device.not_found");
        assert!(error.detail.contains("\"B\""), "{}", error.detail);
        assert!(
            choose(&[], None)
                .unwrap_err()
                .detail
                .contains("no physical")
        );
    }

    fn listed(pid: i64, executable: Option<&str>) -> devicectl::Listed {
        devicectl::Listed {
            pid,
            executable: executable.map(str::to_string),
        }
    }

    /// Only the recorded pid, running the app's executable, is the app.
    #[test]
    fn the_recorded_pid_is_the_app_only_while_it_runs_the_apps_executable() {
        let app = "file:///private/var/containers/Bundle/Application/X/Fixture.app/fixture-app";
        let install = Some("file:///private/var/containers/Bundle/Application/X/Fixture.app/");
        let judge = |listing: &[devicectl::Listed]| {
            judge_app_process(listing, install, "fixture-app", 4242)
        };
        assert_eq!(
            judge(&[
                listed(50, Some("file:///usr/libexec/backboardd")),
                listed(4242, Some(app))
            ]),
            AppProcess::Running
        );
        // Another process has the pid.
        let AppProcess::NotRunning(why) =
            judge(&[listed(4242, Some("file:///usr/libexec/backboardd"))])
        else {
            panic!("another process under the pid was taken for the app");
        };
        assert!(why.contains("/usr/libexec/backboardd"), "{why}");
        // The app runs under another pid: not the one icm launched.
        let AppProcess::NotRunning(why) = judge(&[listed(5000, Some(app))]) else {
            panic!("the app under another pid was taken for the recorded one");
        };
        assert!(why.contains("the app runs as pid 5000"), "{why}");
        // Nothing runs.
        assert!(
            matches!(judge(&[]), AppProcess::NotRunning(why) if why.contains("no process has pid 4242"))
        );
        // A copy of the app with the same name, installed in another
        // container, is another process: its path ends the same way.
        let copy = "file:///private/var/containers/Bundle/Application/Y/Fixture.app/fixture-app";
        let AppProcess::NotRunning(why) = judge(&[listed(4242, Some(copy))]) else {
            panic!("a copy of the app in another container was taken for the app");
        };
        assert!(
            why.contains("pid 4242 is /private/var/containers/Bundle/Application/Y/Fixture.app/fixture-app on the device now"),
            "{why}"
        );
        // Nor is the app's executable the executable of another app's install.
        let other = Some("file:///private/var/containers/Bundle/Application/X/Other.app/");
        assert!(matches!(
            judge_app_process(&[listed(4242, Some(app))], other, "fixture-app", 4242),
            AppProcess::NotRunning(_)
        ));
        // Without the install's directory, nothing listed under the pid can be
        // the app, but a pid nothing lists is still not running.
        assert!(matches!(
            judge_app_process(&[listed(4242, Some(app))], None, "fixture-app", 4242),
            AppProcess::Unconfirmed(why) if why.contains("does not say where the app is installed")
        ));
        assert!(matches!(
            judge_app_process(&[listed(50, Some(app))], None, "fixture-app", 4242),
            AppProcess::NotRunning(why) if why.contains("no process has pid 4242")
        ));
    }

    /// An executable that does not read as a path icm can compare (here a
    /// malformed escape) leaves the recorded pid unconfirmed: stop then
    /// terminates nothing and keeps the record. Only a listing with no such
    /// process under the pid is a definite "not running".
    #[test]
    fn an_executable_that_does_not_read_leaves_the_pid_unconfirmed() {
        let malformed =
            "file:///private/var/containers/Bundle/Application/X/Fixture%G1.app/fixture-app";
        let AppProcess::Unconfirmed(why) = judge_app_process(
            &[listed(4242, Some(malformed))],
            Some("file:///private/var/containers/Bundle/Application/X/Fixture%G1.app/"),
            "fixture-app",
            4242,
        ) else {
            panic!(
                "an executable that does not decode was taken for the app or for another process"
            );
        };
        assert!(why.contains("pid 4242"), "{why}");
        assert!(why.contains("cannot tell"), "{why}");
        assert!(matches!(
            judge_app_process(
                &[listed(5000, Some(malformed))],
                Some("file:///private/var/containers/Bundle/Application/X/Fixture.app/"),
                "fixture-app",
                4242
            ),
            AppProcess::NotRunning(why) if why.contains("no process has pid 4242")
        ));
    }

    /// A listed recorded pid whose executable icm cannot read is unconfirmed,
    /// not another process: nothing says what runs under it, so the record
    /// stays and nothing is terminated. Missing, empty, blank and undecodable
    /// executables all count.
    #[test]
    fn a_listed_pid_without_a_usable_executable_is_unconfirmed() {
        let install = Some("file:///private/var/containers/Bundle/Application/X/Fixture.app/");
        let judge = |listing: &[devicectl::Listed]| {
            judge_app_process(listing, install, "fixture-app", 4242)
        };
        // Missing: the entry names no executable at all.
        let AppProcess::Unconfirmed(why) = judge(&[listed(4242, None)]) else {
            panic!("a listed pid with no executable was taken for another process");
        };
        assert!(
            why.contains("pid 4242 without a usable executable"),
            "{why}"
        );
        // Empty, blank, a bare `file://`, and an escape that does not decode.
        let undecodable = "file:///private/var/containers/Bundle/Application/X/%ZZ/fixture-app";
        for executable in ["", "   ", "file://", undecodable] {
            assert!(
                matches!(
                    judge(&[listed(4242, Some(executable))]),
                    AppProcess::Unconfirmed(_)
                ),
                "{executable:?} was taken for another process"
            );
        }
        // A path that does not decode is named as such, not as a missing one:
        // the device did list an executable, and icm cannot read it.
        let AppProcess::Unconfirmed(why) = judge(&[listed(4242, Some(undecodable))]) else {
            panic!("an undecodable executable was taken for another process");
        };
        assert!(
            why.contains("pid 4242 with an executable path that does not decode")
                && why.contains("/%ZZ/fixture-app"),
            "{why}"
        );
        assert!(!why.contains("without a usable executable path"), "{why}");
        // An unusable entry under another pid says nothing about the one icm
        // recorded, which no process lists: that is still not running.
        assert!(matches!(
            judge(&[listed(5000, None)]),
            AppProcess::NotRunning(why) if why.contains("no process has pid 4242")
        ));
    }

    /// A readable executable that another program runs is not the app, and
    /// neither is a pid that nothing lists: both are NotRunning, with the
    /// reason.
    #[test]
    fn a_readable_other_executable_or_an_unlisted_pid_is_not_running() {
        let install = Some("file:///private/var/containers/Bundle/Application/X/Fixture.app/");
        let judge = |listing: &[devicectl::Listed]| {
            judge_app_process(listing, install, "fixture-app", 4242)
        };
        let backboardd = "file:///usr/libexec/backboardd";
        let AppProcess::NotRunning(why) = judge(&[listed(4242, Some(backboardd))]) else {
            panic!("a readable other executable under the pid was taken for the app");
        };
        assert!(why.contains("pid 4242 is /usr/libexec/backboardd"), "{why}");
        let AppProcess::NotRunning(why) = judge(&[listed(50, Some(backboardd))]) else {
            panic!("an unlisted pid was taken for the app");
        };
        assert!(why.contains("no process has pid 4242"), "{why}");
    }

    /// devicectl lists an executable as a file URL with its path
    /// percent-encoded, so the app `My App` runs in `My%20App.app`. The
    /// record names the bundle as the directory is called. Matching them
    /// raw took the app for another process: `stop` terminated nothing,
    /// said the app was not running and removed the record.
    #[test]
    fn an_app_whose_bundle_name_is_escaped_in_the_listing_is_the_app() {
        for (bundle, listed) in [
            ("My App.app", "My%20App.app"),
            ("Caf\u{e9}.app", "Caf%C3%A9.app"),
            ("100%.app", "100%25.app"),
        ] {
            let app =
                format!("file:///private/var/containers/Bundle/Application/X/{listed}/my-app");
            // The app is installed in the directory its name is listed as.
            let install = format!("file:///private/var/containers/Bundle/Application/X/{listed}/");
            let judge = |listing: &[devicectl::Listed], dir: &str| {
                judge_app_process(listing, Some(dir), "my-app", 4242)
            };
            assert_eq!(
                judge(&[listed_process(4242, &app)], &install),
                AppProcess::Running,
                "{bundle} listed as {listed}"
            );
            // Not for another app, whatever it is called.
            let other = "file:///private/var/containers/Bundle/Application/X/Other.app/";
            let AppProcess::NotRunning(why) = judge(&[listed_process(4242, &app)], other) else {
                panic!("{listed} was taken for Other.app");
            };
            // The reason names the path as it is, not as it is escaped.
            assert!(why.contains(&format!("/{bundle}/my-app")), "{why}");
            // The app under another pid: not the one icm launched, and
            // found all the same.
            let AppProcess::NotRunning(why) = judge(&[listed_process(5000, &app)], &install) else {
                panic!("{bundle}: the app under another pid was taken for the recorded one");
            };
            assert!(why.contains("the app runs as pid 5000"), "{why}");
        }
    }

    fn listed_process(pid: i64, executable: &str) -> devicectl::Listed {
        listed(pid, Some(executable))
    }

    #[test]
    fn the_stop_command_is_the_one_run_stores() {
        assert_eq!(
            terminate_argv("UDID-1", 812).join(" "),
            "xcrun devicectl device process terminate --device UDID-1 --pid 812"
        );
    }

    #[test]
    fn console_events_and_panics_are_read() {
        let text = "hello\nICM_EVENT {\"v\":1,\"kind\":\"start\",\"pid\":812}\nICM_EVENT {\"v\":1,\"kind\":\"ready\",\"ms\":40,\"window\":{\"scale\":3}}\n";
        let found = events(text);
        assert_eq!(of_kind(&found, "start").unwrap()["pid"], 812);
        assert_eq!(of_kind(&found, "ready").unwrap()["ms"], 40);
        assert!(of_kind(&found, "panic").is_none());
        assert_eq!(
            panic_line("a\nthread 'main' panicked at src/lib.rs:7:5:\nboom"),
            Some((2, "thread 'main' panicked at src/lib.rs:7:5:".to_string()))
        );
        assert_eq!(
            app_env(&["A=1".into(), "B=x=y".into()]).unwrap(),
            [("A".into(), "1".into()), ("B".into(), "x=y".into())]
        );
        assert!(app_env(&["=1".into()]).is_err());
    }
}
