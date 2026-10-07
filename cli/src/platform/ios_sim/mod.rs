//! The iOS Simulator (`ios-sim`): `icm build|run|logs|shot|stop ios-sim`
//! (design §10.3, §13.1, §13.3; Appendix C items 6, 22, 25, 26).
//!
//! `run`:
//! 1. Prelude: macOS host, Xcode (exit 9 when missing), the project's Rust
//!    target (exit 4), the simulator runtime and device (exit 4 / 7).
//! 2. Build: `IPHONEOS_DEPLOYMENT_TARGET=<min_os> cargo build --bin <bin>
//!    --target aarch64-apple-ios-sim` (a changed `min_os` relinks the app,
//!    Appendix C item 6), then the Mach-O gates.
//! 3. Bundle: Info.plist with the scene manifest, actool AppIcon,
//!    PrivacyInfo, resources, plist gates, ad-hoc codesign ([`bundle`]).
//! 4. Simulator: icm's managed `icm-<type>-ios-<version>` (created when
//!    missing; `--runtime min` tests the lowest runtime), booted while
//!    cargo builds; install.
//! 5. Launch with `SIMCTL_CHILD_ICM_EVENTS=1`, after starting a detached
//!    `log stream` collector for `icm logs` (Appendix C item 26). Ready is
//!    `ICM_EVENT ready` in the app's stderr; without events, `launchctl
//!    list` showing the app on three polls plus a non-blank screenshot.
//!    A dead app gets its panic, crash report and system log as evidence
//!    (exit 10).
//! 6. Screenshot, preview (long edge ≤ 1024) and `screen{px, pt, preview,
//!    scale}`; a snapshot of the logs; the session file; the result. The
//!    app keeps running.
//!
//! `logs` re-reads the live sources ([`logs`]); `shot` captures the running
//! simulator; `stop` terminates the app and the collector (`--shutdown`
//! also shuts icm's simulator down; a `--fresh` one is deleted).

pub mod bundle;
pub mod image;
pub mod input;
pub mod logs;
pub mod macho;
pub mod plist;
pub mod session;
pub mod simctl;

use crate::cargo::{Invocation, Select};
use crate::catalogue::CheckId;
use crate::cli::{BuildArgs, LogSource, LogsArgs, RunArgs, ShotArgs, StopArgs};
use crate::context::{Ctx, Project};
use crate::error::{Check, Evidence, IcmError, Result};
use crate::plan::{Plan, Step};
use crate::process::{self, Cmd};
use crate::screen::Screen;
use crate::tools::Xcode;
use bundle::Bundle;
use serde_json::{Map, Value, json};
use session::{Session, SessionDevice, SessionLogs};
use simctl::{Device, RuntimeChoice};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The platform's name.
pub const PLATFORM: &str = "ios-sim";

/// How long the app may stay silent before icm stops waiting for
/// `ICM_EVENT start` and probes instead (apps on a framework without
/// events, release builds that did not opt in).
const PROBE_AFTER: Duration = Duration::from_secs(5);

/// How many consecutive `launchctl list` polls must show the app.
const PROBE_POLLS: u32 = 3;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

fn simctl(xcode: &Xcode) -> Cmd {
    xcode.xcrun().arg("simctl")
}

fn internal(what: impl Into<String>) -> IcmError {
    IcmError::new(CheckId::InternalBug, what)
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

// ---- prelude -------------------------------------------------------------------------------

/// The host, the project and Xcode.
fn prelude(ctx: &mut Ctx) -> Result<(Project, Xcode)> {
    if !cfg!(target_os = "macos") {
        return Err(IcmError::new(
            CheckId::EnvUnsupportedHost,
            "the iOS Simulator needs a macOS host with Xcode",
        ));
    }
    let project = ctx.project()?.clone();
    let xcode = crate::tools::xcode(&ctx.env)?;
    if xcode.beta {
        ctx.rep.check(Check::warn(
            CheckId::EnvXcodeBeta,
            format!(
                "Xcode {} at {} looks like a beta",
                xcode.display(),
                xcode.developer_dir.display()
            ),
        ));
    }
    // A placeholder `[app] id` is fine for development: `icm check` and
    // `icm new` report it, the dev platforms' commands do not.
    Ok((project, xcode))
}

fn profile_name(release: bool) -> &'static str {
    if release { "release" } else { "dev" }
}

fn set_tools(ctx: &Ctx, xcode: &Xcode, rustc: Option<&str>, runtime: Option<String>) {
    ctx.rep.set(
        "tools",
        json!({
            "xcode": xcode.display(),
            "rustc": rustc,
            "simulator_runtime": runtime,
        }),
    );
}

// ---- build ---------------------------------------------------------------------------------

/// What `build_app` produced.
struct Built {
    bundle: Bundle,
    bin: String,
    /// `1.98.0`, when this run built.
    rustc: Option<String>,
}

/// `cargo build` for the simulator, the Mach-O gates, the bundle.
fn build_app(ctx: &mut Ctx, project: &Project, xcode: &Xcode, release: bool) -> Result<Built> {
    let profile = profile_name(release);
    let package = project.package_for(PLATFORM)?.clone();
    let bin = project.bin_for(PLATFORM)?;
    let triple = crate::toolchain::ios_sim_triple();
    let min_os = project.config.config.ios.min_os.clone();

    let toolchain = crate::toolchain::active(package.dir())?;
    for check in crate::toolchain::check_targets(&toolchain, &[triple.to_string()]) {
        if check.failed() {
            return Err(check.into_error());
        }
        ctx.rep.check(check);
    }
    let rustc = toolchain.rustc_version().to_string();
    set_tools(ctx, xcode, Some(&rustc), None);

    let prepared = ctx.deployment_target(project, &package.name, Some(triple), profile, &min_os)?;
    let mut env = Vec::new();
    let mut stamp = None;
    if let Some((pair, deployment)) = prepared {
        env.push(pair);
        stamp = Some(deployment);
    }

    let mut invocation = Invocation::new("build", &package.manifest_path, &package.name);
    invocation.select = Select::Bin(bin.clone());
    invocation.triple = Some(triple.to_string());
    invocation.profile = profile.to_string();
    let output = ctx.cargo("cargo.build", &invocation, &env)?;
    if let Some(stamp) = stamp {
        let _ = stamp.write();
    }

    let exe = output
        .executable(&bin)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| {
            crate::cargo::artifacts_dir(&project.target_dir, Some(triple), profile).join(&bin)
        });
    if !exe.is_file() {
        return Err(internal(format!(
            "cargo reported success but {} does not exist",
            crate::paths::display(&exe)
        )));
    }
    macho_gates(ctx, &exe, &min_os)?;

    let bundle = bundle::assemble(ctx, project, xcode, &exe, &bin, profile)?;
    ctx.rep.artifact("bundle", &bundle.app);
    Ok(Built {
        bundle,
        bin,
        rustc: Some(rustc),
    })
}

/// `ios.macho.platform` (blocking) and `ios.macho.minos`.
fn macho_gates(ctx: &Ctx, exe: &Path, min_os: &str) -> Result<()> {
    let versions = macho::build_versions(exe).map_err(|error| {
        IcmError::new(CheckId::BuildWrongPlatform, error).evidence(Evidence::file(exe))
    })?;
    let Some(sim) = versions
        .iter()
        .find(|v| v.platform == macho::PLATFORM_IOSSIMULATOR)
    else {
        let found: Vec<String> = versions
            .iter()
            .map(|v| format!("{} {}", v.arch, macho::platform_name(v.platform)))
            .collect();
        return Err(IcmError::new(
            CheckId::IosMachoPlatform,
            format!(
                "{} is built for {}, not IOSSIMULATOR",
                crate::paths::display(exe),
                if found.is_empty() {
                    "no Apple platform".to_string()
                } else {
                    found.join(", ")
                }
            ),
        )
        .evidence(Evidence::file(exe)));
    };
    ctx.rep.check(Check::pass(
        CheckId::IosMachoPlatform,
        format!(
            "IOSSIMULATOR {} minos {} sdk {}",
            sim.arch,
            sim.minos_string(),
            sim.sdk_string()
        ),
    ));
    if macho::minos_matches(sim.minos, min_os) {
        ctx.rep.check(Check::pass(
            CheckId::IosMachoMinos,
            format!("minos {} = [ios] min_os", sim.minos_string()),
        ));
    } else {
        ctx.rep.check(
            Check::fail(
                CheckId::IosMachoMinos,
                format!(
                    "the executable's minos is {}, but [ios] min_os is {min_os}",
                    sim.minos_string()
                ),
            )
            .evidence(Evidence::file(exe))
            .fix(
                "Rerun; icm relinks the app when the deployment target changes. If it persists, run the cargo clean in the fix.",
                &["cargo clean -p <package> --target aarch64-apple-ios-sim"],
            ),
        );
    }
    Ok(())
}

/// The bundle a previous build left, for `run --no-build`.
fn previous_bundle(project: &Project, release: bool) -> Result<Built> {
    let profile = profile_name(release);
    let bin = project.bin_for(PLATFORM)?;
    let app = project
        .build_dir(PLATFORM, profile)
        .join(bundle::bundle_name(&project.config.config.app.name));
    let executable = app.join(&bin);
    if !executable.is_file() {
        return Err(IcmError::new(
            CheckId::UsageBadArgs,
            format!(
                "--no-build: there is no previous {profile} build at {}",
                crate::paths::display(&app)
            ),
        )
        .fix("Drop --no-build.", &["icm run ios-sim"]));
    }
    Ok(Built {
        bundle: Bundle {
            app,
            executable,
            info: Map::new(),
        },
        bin,
        rustc: None,
    })
}

/// `icm build ios-sim`.
pub fn build(ctx: &mut Ctx, args: &BuildArgs) -> Result<()> {
    let (project, xcode) = prelude(ctx)?;
    ctx.rep.set(
        "profile",
        json!(crate::cargo::profile_dir(profile_name(args.release))),
    );
    if ctx.dry_run() {
        plan(&project, &xcode, None, false).report(ctx);
        return Ok(());
    }
    let _lock = ctx.lock_platform(PLATFORM)?;
    let built = build_app(ctx, &project, &xcode, args.release)?;
    ctx.rep.summary(format!(
        "built {} for the iOS Simulator",
        crate::paths::display(&built.bundle.app)
    ));
    ctx.rep.next(
        "icm run ios-sim --no-build --json -q",
        "install and launch this build",
    );
    Ok(())
}

// ---- devices -------------------------------------------------------------------------------

/// The simulator a run uses.
#[derive(Clone, Debug)]
struct Target {
    device: Device,
    os: String,
    runtime_build: String,
    device_type: String,
    type_bundle: Option<PathBuf>,
    fresh: bool,
}

impl Target {
    fn json(&self) -> Value {
        json!({
            "kind": "simulator",
            "udid": self.device.udid,
            "name": self.device.name,
            "os": self.os,
            "type": self.device_type,
            "managed": self.device.is_managed(),
            "fresh": self.fresh,
        })
    }
}

fn list_json(ctx: &Ctx, xcode: &Xcode, what: &[&str]) -> Result<String> {
    let outcome = ctx.probe(
        &simctl(xcode)
            .args(["list", "-j"])
            .args(what)
            .timeout(Duration::from_secs(90)),
    )?;
    if !outcome.success() {
        return Err(IcmError::new(
            CheckId::ToolFailed,
            format!(
                "xcrun simctl list -j {} failed: {}",
                what.join(" "),
                outcome.stderr_tail(4)
            ),
        ));
    }
    Ok(outcome.stdout_text())
}

fn parse_failure(error: String) -> IcmError {
    IcmError::new(CheckId::ToolFailed, error)
}

fn devices(ctx: &Ctx, xcode: &Xcode) -> Result<Vec<Device>> {
    simctl::parse_devices(&list_json(ctx, xcode, &["devices"])?).map_err(parse_failure)
}

/// Picks (and when needed creates) the simulator.
fn choose_target(
    ctx: &mut Ctx,
    project: &Project,
    xcode: &Xcode,
    args: &RunArgs,
) -> Result<Target> {
    let min_os = project.config.config.ios.min_os.clone();
    let host = ctx.host()?.clone();
    let runtimes = simctl::parse_runtimes(&list_json(ctx, xcode, &["runtimes", "available"])?)
        .map_err(parse_failure)?;
    let types = simctl::parse_device_types(&list_json(ctx, xcode, &["devicetypes"])?)
        .map_err(parse_failure)?;
    let mut all = devices(ctx, xcode)?;

    let type_of = |device: &Device| {
        let identifier = device.device_type.clone().unwrap_or_default();
        types
            .iter()
            .find(|t| t.identifier == identifier)
            .map(|t| (t.name.clone(), t.bundle_path.clone()))
            .unwrap_or((identifier, None))
    };

    let selector = args
        .sim
        .clone()
        .or_else(|| args.device.clone())
        .or_else(|| host.ios.simulator_udid.clone())
        .filter(|s| !s.trim().is_empty());

    if let Some(selector) = selector {
        let device = simctl::find_device(&all, &selector).cloned().ok_or_else(|| {
            IcmError::new(
                CheckId::IosSimNotFound,
                format!("no simulator is named or has the UDID `{selector}`"),
            )
            .fix(
                "Pick one from `xcrun simctl list devices`, or drop --sim/--device (and host.toml simulator_udid) to use icm's managed simulator.",
                &["xcrun simctl list devices available"],
            )
        })?;
        if !device.available {
            return Err(IcmError::new(
                CheckId::IosSimNotFound,
                format!(
                    "the simulator {} ({}) is unavailable (its runtime is missing)",
                    device.name, device.udid
                ),
            ));
        }
        let runtime = runtimes.iter().find(|r| r.identifier == device.runtime);
        let os = runtime.map(|r| r.version.clone()).unwrap_or_default();
        if let Some(runtime) = runtime
            && !simctl::at_least(&runtime.version, &min_os)
        {
            return Err(IcmError::new(
                CheckId::EnvIosRuntimeMissing,
                format!(
                    "the simulator {} runs {}, below [ios] min_os {min_os}",
                    device.name, runtime.name
                ),
            ));
        }
        let (device_type, type_bundle) = type_of(&device);
        return Ok(Target {
            runtime_build: runtime.map(|r| r.build.clone()).unwrap_or_default(),
            device,
            os,
            device_type,
            type_bundle,
            fresh: false,
        });
    }

    let choice = RuntimeChoice::parse(args.runtime.as_deref())
        .map_err(|detail| IcmError::new(CheckId::UsageBadArgs, detail))?;
    let runtime = simctl::choose_runtime(&runtimes, &min_os, &choice).map_err(|detail| {
        IcmError::new(CheckId::EnvIosRuntimeMissing, detail).fix_commands([
            "icm doctor ios-sim --fix --yes   # xcodebuild -downloadPlatform iOS, about 8 GB",
        ])
    })?;
    let device_type = simctl::choose_device_type(runtime, host.ios.simulator_type.as_deref())
        .map_err(|detail| IcmError::new(CheckId::IosSimNotFound, detail))?;

    let existing = if args.fresh {
        None
    } else {
        simctl::find_managed(&all, device_type, runtime).cloned()
    };
    let device = match existing {
        Some(device) => device,
        None => {
            let name = if args.fresh {
                format!(
                    "{}fresh-{}-ios-{}-{}",
                    simctl::MANAGED_PREFIX,
                    simctl::slug(&device_type.name),
                    runtime.version,
                    &ctx.rep.run_id()[ctx.rep.run_id().len().saturating_sub(4)..]
                )
            } else {
                simctl::managed_name(device_type, runtime)
            };
            let outcome = ctx.step(
                "simctl.create",
                &simctl(xcode)
                    .arg("create")
                    .arg(&name)
                    .arg(&device_type.identifier)
                    .arg(&runtime.identifier)
                    .timeout(Duration::from_secs(120)),
            )?;
            if !outcome.success() {
                return Err(ctx.step_failure("simctl.create", CheckId::IosSimBootFailed, &outcome));
            }
            let udid = outcome.stdout_text().trim().to_string();
            all = devices(ctx, xcode)?;
            simctl::find_device(&all, &udid)
                .cloned()
                .ok_or_else(|| internal(format!("simctl created {udid} but does not list it")))?
        }
    };

    Ok(Target {
        device,
        os: runtime.version.clone(),
        runtime_build: runtime.build.clone(),
        device_type: device_type.name.clone(),
        type_bundle: device_type.bundle_path.clone(),
        fresh: args.fresh,
    })
}

/// `simctl boot`, unless it is already booted. Returns at once; the
/// simulator finishes booting while cargo builds.
fn start_boot(ctx: &Ctx, xcode: &Xcode, device: &Device) -> Result<()> {
    if device.is_booted() {
        return Ok(());
    }
    let outcome = ctx.step(
        "simctl.boot",
        &simctl(xcode)
            .arg("boot")
            .arg(&device.udid)
            .timeout(Duration::from_secs(180)),
    )?;
    if !outcome.success() && !outcome.stderr_text().contains("current state: Booted") {
        return Err(ctx.step_failure("simctl.boot", CheckId::IosSimBootFailed, &outcome));
    }
    Ok(())
}

/// `simctl bootstatus -b`: waits until the simulator has booted.
fn finish_boot(ctx: &Ctx, xcode: &Xcode, device: &Device) -> Result<()> {
    let outcome = ctx.step(
        "simctl.bootstatus",
        &simctl(xcode)
            .args(["bootstatus"])
            .arg(&device.udid)
            .arg("-b")
            .timeout(Duration::from_secs(300)),
    )?;
    if !outcome.success() {
        return Err(ctx.step_failure("simctl.bootstatus", CheckId::IosSimBootFailed, &outcome));
    }
    Ok(())
}

// ---- run -----------------------------------------------------------------------------------

/// The plan `--dry-run` prints.
fn plan(project: &Project, xcode: &Xcode, udid: Option<&str>, launch: bool) -> Plan {
    let config = &project.config.config;
    let triple = crate::toolchain::ios_sim_triple();
    let udid = udid.unwrap_or("<udid>");
    let mut plan = Plan::new();
    let package = project.package_for(PLATFORM).ok();
    if let (Some(package), Ok(bin)) = (package, project.bin_for(PLATFORM)) {
        let mut invocation = Invocation::new("build", &package.manifest_path, &package.name);
        invocation.select = Select::Bin(bin);
        invocation.triple = Some(triple.to_string());
        plan.push(
            Step::exec(
                "cargo.build",
                invocation
                    .cmd()
                    .env("IPHONEOS_DEPLOYMENT_TARGET", &config.ios.min_os),
            )
            .gate(CheckId::IosMachoPlatform)
            .gate(CheckId::IosMachoMinos)
            .on_fail(CheckId::BuildCompileError),
        );
    }
    plan.push(Step::internal(
        "ios.generate",
        "write Info.plist (scene manifest), PrivacyInfo.xcprivacy and Assets.xcassets",
    ));
    plan.push(
        Step::exec(
            "ios.actool",
            xcode.xcrun().args([
                "actool",
                "Assets.xcassets",
                "--compile",
                "actool-out",
                "--platform",
                "iphonesimulator",
                "--minimum-deployment-target",
                &config.ios.min_os,
                "--app-icon",
                plist::APP_ICON,
                "--target-device",
                "iphone",
            ]),
        )
        .on_fail(CheckId::IosActoolFailed),
    );
    plan.push(
        Step::exec(
            "ios.codesign",
            Cmd::tool("codesign").args(["--force", "--sign", "-", "--timestamp=none", "<App>.app"]),
        )
        .gate(CheckId::IosPlistLint)
        .gate(CheckId::IosPlistSceneManifest)
        .gate(CheckId::IosPrivacyPresent),
    );
    if launch {
        plan.push(
            Step::exec("simctl.boot", simctl(xcode).args(["boot", udid]))
                .on_fail(CheckId::IosSimBootFailed),
        );
        plan.push(
            Step::exec(
                "simctl.install",
                simctl(xcode).args(["install", udid, "<App>.app"]),
            )
            .on_fail(CheckId::IosSimInstallFailed),
        );
        plan.push(
            Step::exec(
                "simctl.launch",
                simctl(xcode)
                    .args([
                        "launch",
                        "--terminate-running-process",
                        "--stdout=<session>/app.stdout",
                        "--stderr=<session>/app.stderr",
                        udid,
                        &config.app.id,
                    ])
                    .env("SIMCTL_CHILD_ICM_EVENTS", "1")
                    .env("SIMCTL_CHILD_RUST_BACKTRACE", "1"),
            )
            .gate(CheckId::RunReady),
        );
        plan.push(
            Step::exec(
                "simctl.screenshot",
                simctl(xcode).args(["io", udid, "screenshot", "--type=png", "screen.png"]),
            )
            .gate(CheckId::RunScreenBlank),
        );
    }
    plan
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

/// Ends what the previous session left running: the collector, and the app
/// when it ran on another simulator.
fn end_previous(ctx: &Ctx, xcode: &Xcode, sessions_dir: &Path, udid: &str) {
    let Some(mut previous) = Session::read(sessions_dir) else {
        return;
    };
    stop_collector(&mut previous);
    if previous.device.udid != udid && previous.app_alive() {
        let _ = ctx.probe(
            &simctl(xcode)
                .args(["terminate", &previous.device.udid, &previous.app_id])
                .timeout(Duration::from_secs(30)),
        );
    }
}

/// The unified-log predicate of the collector: the app's own records
/// (iced's subsystem) and its errors and faults from any subsystem.
fn collector_predicate(exe: &str) -> String {
    format!(
        "process == \"{exe}\" AND (subsystem == \"{}\" OR messageType == error OR messageType == fault)",
        logs::ICED_SUBSYSTEM
    )
}

/// Starts the detached `log stream` collector and waits until it is
/// attached (its header line arrives), so early records are not lost.
fn start_collector(xcode: &Xcode, udid: &str, exe: &str, out: &Path) -> Option<i32> {
    let cmd = simctl(xcode)
        .args([
            "spawn", udid, "log", "stream", "--level", "debug", "--style", "ndjson",
        ])
        .arg("--predicate")
        .arg(collector_predicate(exe));
    let pid = process::spawn_detached(&cmd, out, &out.with_extension("err")).ok()?;
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if std::fs::metadata(out).is_ok_and(|m| m.len() > 0) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    i32::try_from(pid).ok()
}

/// What the readiness wait found.
enum Readiness {
    /// The first frame (or the probe) arrived.
    Ready {
        source: &'static str,
        ms: u64,
        window: Option<Value>,
        panic: Option<logs::Panic>,
    },
    /// The app died.
    Died { panic: Option<logs::Panic> },
    /// Alive, but no first frame in time: `waited` that long, and
    /// `overall` when what was left of `--timeout` ran out before
    /// `--wait-ready`.
    NotReady {
        saw_start: bool,
        waited: Duration,
        overall: bool,
    },
}

/// Whether `launchctl list` in the simulator shows the app with a pid.
fn launchctl_shows(ctx: &Ctx, xcode: &Xcode, udid: &str, app_id: &str) -> bool {
    let Ok(outcome) = ctx.probe(
        &simctl(xcode)
            .args(["spawn", udid, "launchctl", "list"])
            .timeout(Duration::from_secs(20)),
    ) else {
        return false;
    };
    let needle = format!("UIKitApplication:{app_id}[");
    outcome.stdout_text().lines().any(|line| {
        line.contains(&needle)
            && line
                .split_whitespace()
                .next()
                .is_some_and(|pid| pid.parse::<u32>().is_ok())
    })
}

/// Waits for `ICM_EVENT ready`, or the probe, or death.
fn wait_ready(
    ctx: &Ctx,
    xcode: &Xcode,
    session: &Session,
    pid: i32,
    launched_at: Instant,
    limit: Duration,
    probe_shot: &Path,
) -> Result<Readiness> {
    let started = launched_at;
    let own = started + limit;
    let (deadline, overall) = match ctx.deadline() {
        Some(overall) if overall < own => (overall, true),
        _ => (own, false),
    };
    let mut saw_start = false;
    let mut panic = None;
    let mut polls = 0u32;
    let mut last_poll = Instant::now() - Duration::from_secs(10);

    loop {
        if let Some(signal) = crate::signals::pending() {
            return Err(crate::output::interrupted(signal));
        }

        let stderr = std::fs::read_to_string(&session.logs.stderr).unwrap_or_default();
        for line in stderr.lines() {
            let Some(event) = logs::parse_event(line) else {
                continue;
            };
            match event.get("kind").and_then(Value::as_str) {
                Some("start") => saw_start = true,
                Some("ready") => {
                    return Ok(Readiness::Ready {
                        source: "icm_event",
                        ms: started.elapsed().as_millis() as u64,
                        window: event.get("window").cloned(),
                        panic: logs::find_panic(&stderr),
                    });
                }
                Some("panic") => panic = logs::find_panic(&stderr),
                _ => {}
            }
        }

        if !crate::signals::alive(pid) {
            // Let the last writes land.
            std::thread::sleep(Duration::from_millis(300));
            let stderr = std::fs::read_to_string(&session.logs.stderr).unwrap_or_default();
            return Ok(Readiness::Died {
                panic: logs::find_panic(&stderr).or(panic),
            });
        }

        if !saw_start
            && started.elapsed() >= PROBE_AFTER
            && last_poll.elapsed() >= Duration::from_secs(1)
        {
            last_poll = Instant::now();
            if launchctl_shows(ctx, xcode, &session.device.udid, &session.app_id) {
                polls += 1;
            } else {
                polls = 0;
            }
            if polls >= PROBE_POLLS
                && capture(ctx, xcode, &session.device.udid, probe_shot, None).is_ok()
                && image::read_png(probe_shot)
                    .is_ok_and(|shot| !image::blank_stats(&shot).is_blank())
            {
                return Ok(Readiness::Ready {
                    source: "probe",
                    ms: started.elapsed().as_millis() as u64,
                    window: None,
                    panic,
                });
            }
        }

        if Instant::now() >= deadline {
            return Ok(Readiness::NotReady {
                saw_start,
                waited: started.elapsed(),
                overall,
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `simctl io <udid> screenshot` into `path` (a quick probe when `step` is
/// `None`).
fn capture(ctx: &Ctx, xcode: &Xcode, udid: &str, path: &Path, step: Option<&str>) -> Result<()> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let cmd = simctl(xcode)
        .args(["io", udid, "screenshot", "--type=png"])
        .arg(path)
        .timeout(Duration::from_secs(60));
    let outcome = match step {
        Some(name) => ctx.step(name, &cmd)?,
        None => ctx.probe(&cmd)?,
    };
    if !outcome.success() || !path.is_file() {
        let text = format!("{}{}", outcome.stdout_text(), outcome.stderr_text());
        let id = if text.contains("No devices are booted") || text.contains("state: Shutdown") {
            CheckId::RunNoSession
        } else {
            CheckId::ToolFailed
        };
        return Err(IcmError::new(
            id,
            format!("simctl io screenshot failed: {}", outcome.stderr_tail(3)),
        ));
    }
    Ok(())
}

/// The device's points-per-pixel scale from its device type's
/// capabilities (`main-screen-scale`).
fn type_scale(ctx: &Ctx, bundle: Option<&Path>) -> Option<f64> {
    let path = bundle?
        .join("Contents")
        .join("Resources")
        .join("capabilities.plist");
    if !path.is_file() {
        return None;
    }
    let plist = bundle::read_plist(ctx, &path).ok()?;
    plist
        .get("capabilities")
        .and_then(|c| c.get("ScreenDimensionsCapability"))
        .and_then(|s| s.get("main-screen-scale"))
        .and_then(Value::as_f64)
}

/// Takes the screenshot `<dir>/<name>.png`, writes the preview, reports
/// both and the blank check; returns the `screen` object.
fn screenshot(
    ctx: &Ctx,
    xcode: &Xcode,
    udid: &str,
    dir: &Path,
    name: &str,
    scale: Option<f64>,
    expect_content: bool,
) -> Result<Screen> {
    let path = dir.join(format!("{name}.png"));
    let preview_path = dir.join(format!("{name}.preview.png"));
    capture(ctx, xcode, udid, &path, Some("simctl.screenshot"))?;
    let preview = image::preview(&path, &preview_path).map_err(|error| {
        IcmError::new(
            CheckId::ToolFailed,
            format!("cannot read the screenshot: {error}"),
        )
        .evidence(Evidence::file(&path))
    })?;

    let scale = scale.unwrap_or(if preview.px.0 >= 1_000 { 3.0 } else { 2.0 });
    let screen = Screen::new(preview.px, scale);
    let mut extra = Map::new();
    let _ = extra.insert("blank".into(), json!(preview.blank.is_blank()));
    let _ = extra.insert(
        "bytes".into(),
        json!(std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)),
    );
    ctx.rep.artifact_with("screenshot", &path, extra);
    ctx.rep.artifact("preview", &preview_path);
    ctx.rep.set("screen", screen.to_json());

    let detail = preview.blank.describe();
    if preview.blank.is_blank() {
        let check = if expect_content {
            Check::fail(CheckId::RunScreenBlank, detail)
        } else {
            Check::warn(CheckId::RunScreenBlank, detail)
        };
        ctx.rep.check(check.evidence(Evidence::file(&path)).fix(
            "Compare with a headless render; check fonts and theme; read the logs.",
            &[
                "icm logs ios-sim --level warn",
                "icm shot --headless",
                "icm explain run.screen_blank",
            ],
        ));
    } else {
        ctx.rep.check(Check::pass(
            CheckId::RunScreenBlank,
            format!("the screen has content ({detail})"),
        ));
    }
    Ok(screen)
}

/// A session's records, by source.
struct Parts {
    stderr: Vec<logs::Record>,
    stdout: Vec<logs::Record>,
    /// The collector's and the crash reports' (and the system log's).
    timed: Vec<logs::Record>,
    notes: Vec<String>,
}

/// Every record from a session's sources, merged.
fn collect_records(
    ctx: &Ctx,
    xcode: Option<&Xcode>,
    session: &Session,
    system: bool,
    raw: bool,
) -> (Vec<logs::Record>, Vec<String>) {
    let parts = collect_parts(ctx, xcode, session, system, raw);
    let mut stdio = parts.stderr;
    stdio.extend(parts.stdout);
    (logs::merge(stdio, parts.timed), parts.notes)
}

/// The crash reports of this session's app (`~/Library/Logs/DiagnosticReports`).
fn session_crashes(session: &Session) -> Vec<PathBuf> {
    let Some(home) = crate::paths::home() else {
        return Vec::new();
    };
    let dir = home.join("Library").join("Logs").join("DiagnosticReports");
    logs::crash_reports(&dir, &session.exe, session.launch_unix_ms)
        .into_iter()
        .filter(|report| {
            logs::crash_belongs(report, session.pid, &session.device.udid, &session.app_id)
        })
        .collect()
}

/// Every record from a session's sources.
fn collect_parts(
    ctx: &Ctx,
    xcode: Option<&Xcode>,
    session: &Session,
    system: bool,
    raw: bool,
) -> Parts {
    let mut notes = Vec::new();
    let read = |path: &Path| std::fs::read_to_string(path).unwrap_or_default();

    let stderr = logs::stdio_records("stderr", &read(&session.logs.stderr));
    let stdout = logs::stdio_records("stdout", &read(&session.logs.stdout));

    let mut timed = if raw {
        read(&session.logs.oslog)
            .lines()
            .filter(|line| line.starts_with('{'))
            .filter_map(|line| {
                logs::oslog_record("oslog", line).map(|mut record| {
                    record.msg = line.to_string();
                    record
                })
            })
            .collect()
    } else {
        logs::oslog_records("oslog", &read(&session.logs.oslog))
    };

    for report in session_crashes(session) {
        if let Some(record) = logs::crash_record(&report) {
            timed.push(record);
        }
    }

    if system && let Some(xcode) = xcode {
        match system_log(ctx, xcode, session, false) {
            Ok(text) => timed.extend(logs::oslog_records("system", &text)),
            Err(note) => notes.push(note),
        }
    }
    // The collector and the system query can both hold an app error.
    let mut seen = std::collections::HashSet::new();
    timed.retain(|record| seen.insert((record.unix_ms, record.pid, record.msg.clone())));
    timed.sort_by_key(|record| record.unix_ms.unwrap_or(i64::MIN));

    Parts {
        stderr,
        stdout,
        timed,
        notes,
    }
}

/// The `log show` predicate for the app's system messages. Normally only
/// errors and faults: the app's from other subsystems than iced's (the
/// collector has those) and other processes' about the app (launch
/// failures, terminations). `wide` (used when the app died) takes every
/// message of the app's process and every message naming it.
fn system_predicate(exe: &str, id: &str, wide: bool) -> String {
    if wide {
        format!("process == \"{exe}\" OR eventMessage CONTAINS \"{id}\"")
    } else {
        format!(
            "(messageType == error OR messageType == fault) AND ((process == \"{exe}\" AND subsystem != \"{}\") OR (process != \"{exe}\" AND eventMessage CONTAINS \"{id}\"))",
            logs::ICED_SUBSYSTEM
        )
    }
}

/// The live `log show` query for the app's system messages
/// ([`system_predicate`]).
fn system_log(
    ctx: &Ctx,
    xcode: &Xcode,
    session: &Session,
    wide: bool,
) -> std::result::Result<String, String> {
    let predicate = system_predicate(&session.exe, &session.app_id, wide);
    let outcome = ctx
        .probe(
            &simctl(xcode)
                .args([
                    "spawn",
                    &session.device.udid,
                    "log",
                    "show",
                    "--style",
                    "ndjson",
                ])
                .arg("--start")
                .arg(logs::log_show_start(session.launch_unix_ms - 1_000))
                .arg("--predicate")
                .arg(predicate)
                .timeout(Duration::from_secs(90)),
        )
        .map_err(|error| error.detail)?;
    if outcome.success() {
        Ok(outcome.stdout_text())
    } else {
        Err(format!(
            "the system log could not be read (is the simulator booted?): {}",
            outcome.stderr_tail(2)
        ))
    }
}

/// Writes `app.log` (readable) and `logs.ndjson` into a directory.
fn write_records(dir: &Path, records: &[logs::Record]) -> (PathBuf, PathBuf) {
    let app_log = dir.join("app.log");
    let ndjson = dir.join("logs.ndjson");
    let mut text = String::new();
    let mut lines = String::new();
    for record in records {
        text.push_str(&record.line());
        text.push('\n');
        lines.push_str(&serde_json::to_string(record).unwrap_or_default());
        lines.push('\n');
    }
    let _ = std::fs::write(&app_log, text);
    let _ = std::fs::write(&ndjson, lines);
    (app_log, ndjson)
}

/// A snapshot of the session's logs into the run directory.
fn snapshot_logs(ctx: &Ctx, session: &Session, run_dir: &Path) {
    for (from, name) in [
        (&session.logs.stdout, "app.stdout"),
        (&session.logs.stderr, "app.stderr"),
    ] {
        let _ = std::fs::copy(from, run_dir.join(name));
    }
    let (records, _) = collect_records(ctx, None, session, false, false);
    let (app_log, ndjson) = write_records(run_dir, &records);
    ctx.rep.artifact("app_log", &app_log);
    ctx.rep.artifact("logs", &ndjson);
}

/// Known failure signatures in the evidence (design §13.4).
fn signatures(text: &str) -> Vec<String> {
    let mut causes = Vec::new();
    let lower = text.to_lowercase();
    if lower.contains("has no uiapplicationscenemanifest")
        || ((lower.contains("failed to launch")
            || lower.contains("fbsopenapplicationserviceerrordomain"))
            && lower.contains("scene"))
    {
        causes.push(
            "the scene manifest (ios.plist.scene_manifest): iOS 27 kills apps without UISceneConfigurations"
                .to_string(),
        );
    }
    if lower.contains("codesigning") || lower.contains("code signature") {
        causes.push("the bundle's signature: rerun `icm run ios-sim` (it re-signs)".to_string());
    }
    if lower.contains("failed to find an appropriate adapter") || lower.contains("surface creation")
    {
        causes.push("no GPU adapter: retry with `--env ICED_BACKEND=tiny-skia`".to_string());
    }
    if lower.contains("dyld") && lower.contains("library not loaded") {
        causes.push("a dynamic library is missing from the bundle".to_string());
    }
    causes
}

/// Gathers the evidence for a dead app and builds the error (exit 10).
fn died(
    ctx: &Ctx,
    xcode: &Xcode,
    session: &Session,
    run_dir: &Path,
    panic: Option<logs::Panic>,
    after: Duration,
    launch_error: Option<String>,
) -> IcmError {
    // ReportCrash writes the report up to ~10 s after the process ends. A
    // panic already names its cause, so wait long only without one;
    // `icm logs ios-sim --source crash` finds a late report.
    let mut reports = Vec::new();
    let until = Instant::now() + Duration::from_secs(if panic.is_some() { 2 } else { 15 });
    while Instant::now() < until && launch_error.is_none() {
        reports = session_crashes(session);
        if !reports.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let crash_dir = run_dir.join("crash");
    let mut copied = Vec::new();
    for report in &reports {
        let _ = std::fs::create_dir_all(&crash_dir);
        if let Some(name) = report.file_name() {
            let target = crash_dir.join(name);
            if std::fs::copy(report, &target).is_ok() {
                copied.push(target);
            }
        }
    }
    if let Some(first) = copied.first() {
        ctx.rep.artifact("crash", first);
    }
    ctx.rep.set(
        "crash_reports",
        json!(
            copied
                .iter()
                .map(|p| crate::paths::display(p))
                .collect::<Vec<_>>()
        ),
    );

    let system_path = run_dir.join("system.ndjson");
    let system = system_log(ctx, xcode, session, true).unwrap_or_default();
    let _ = std::fs::write(&system_path, &system);
    if !system.is_empty() {
        ctx.rep.artifact("system_log", &system_path);
    }
    snapshot_logs(ctx, session, run_dir);

    let stderr_copy = run_dir.join("app.stderr");
    let secs = after.as_secs_f64();
    let mut error = match &panic {
        Some(panic) => {
            let location = panic.location.as_deref().unwrap_or("an unknown location");
            IcmError::new(
                CheckId::RunAppPanicked,
                format!("panicked at {location}: {}", panic.message),
            )
            .evidence(Evidence::line(
                &stderr_copy,
                panic.line,
                format!("panicked at {location}"),
            ))
            .cause(format!("a bug at {location}"))
        }
        None => {
            let detail = match &launch_error {
                Some(text) => format!("simctl could not launch the app: {text}"),
                None => format!("the app exited {secs:.1}s after launch"),
            };
            IcmError::new(CheckId::RunAppDied, detail).evidence(Evidence::file(&stderr_copy))
        }
    };
    for report in &copied {
        let summary = logs::crash_record(report)
            .map(|r| r.msg)
            .unwrap_or_default();
        error = error.evidence(Evidence::file(report).with_excerpt(summary));
    }
    if !system.is_empty() {
        error = error.evidence(Evidence::file(&system_path));
    }
    let haystack = format!(
        "{}\n{}\n{}",
        launch_error.unwrap_or_default(),
        std::fs::read_to_string(&session.logs.stderr).unwrap_or_default(),
        system
    );
    for cause in signatures(&haystack) {
        error = error.cause(cause);
    }
    error.fix_commands([
        "icm logs ios-sim --level warn --json",
        "icm logs ios-sim --source crash --json   # a crash report can take ~10 s to appear",
        "icm run ios-sim --json -q",
    ])
}

/// `icm run ios-sim`.
pub fn run(ctx: &mut Ctx, args: &RunArgs) -> Result<()> {
    let (project, xcode) = prelude(ctx)?;
    ctx.rep.latest(PLATFORM);
    ctx.rep.set(
        "profile",
        json!(crate::cargo::profile_dir(profile_name(args.release))),
    );
    let extra_env = app_env(&args.env)?;
    let config = project.config.config.clone();

    if ctx.dry_run() {
        plan(
            &project,
            &xcode,
            args.sim.as_deref().or(args.device.as_deref()),
            true,
        )
        .report(ctx);
        return Ok(());
    }

    let lock = ctx.lock_platform(PLATFORM)?;
    let target = choose_target(ctx, &project, &xcode, args)?;
    ctx.rep.set("device", target.json());
    // Boot while cargo builds.
    start_boot(ctx, &xcode, &target.device)?;
    if args.show {
        let _ = ctx.probe(
            &Cmd::tool("open")
                .args(["-a", "Simulator", "--args", "-CurrentDeviceUDID"])
                .arg(&target.device.udid)
                .timeout(Duration::from_secs(30)),
        );
    }

    let built = if args.no_build {
        previous_bundle(&project, args.release)?
    } else {
        build_app(ctx, &project, &xcode, args.release)?
    };
    set_tools(
        ctx,
        &xcode,
        built.rustc.as_deref(),
        Some(format!("iOS {} ({})", target.os, target.runtime_build)),
    );

    finish_boot(ctx, &xcode, &target.device)?;
    ctx.rep.check(Check::pass(
        CheckId::IosSimBootFailed,
        format!("{} (iOS {}) is booted", target.device.name, target.os),
    ));

    if args.reinstall {
        let _ = ctx.step(
            "simctl.uninstall",
            &simctl(&xcode)
                .args(["uninstall", &target.device.udid, &config.app.id])
                .timeout(Duration::from_secs(120)),
        )?;
    }
    let outcome = ctx.step(
        "simctl.install",
        &simctl(&xcode)
            .arg("install")
            .arg(&target.device.udid)
            .arg(&built.bundle.app)
            .timeout(Duration::from_secs(300)),
    )?;
    if !outcome.success() {
        return Err(ctx.step_failure("simctl.install", CheckId::IosSimInstallFailed, &outcome));
    }

    // The session: live files outside the run directory.
    let sessions_dir = project.sessions_dir();
    let run_id = ctx.rep.run_id();
    let run_dir = ctx
        .rep
        .run_dir()
        .ok_or_else(|| internal("the run directory is not attached"))?;
    end_previous(ctx, &xcode, &sessions_dir, &target.device.udid);
    let files = session::files_dir(&sessions_dir, &run_id);
    std::fs::create_dir_all(&files)
        .map_err(|error| internal(format!("cannot create {}: {error}", files.display())))?;
    session::prune_files(&sessions_dir, &run_id);
    let requested_stdout = files.join("app.stdout");
    let requested_stderr = files.join("app.stderr");
    let data_path = target.device.data_path.as_deref();
    let oslog = files.join("oslog.ndjson");
    let exe_name = built.bin.clone();
    let collector_pid = start_collector(&xcode, &target.device.udid, &exe_name, &oslog);

    let mut session = Session {
        schema: session::SCHEMA.to_string(),
        platform: PLATFORM.to_string(),
        run: run_id.clone(),
        run_dir: Some(run_dir.clone()),
        state: "running".to_string(),
        device: SessionDevice {
            udid: target.device.udid.clone(),
            name: target.device.name.clone(),
            os: target.os.clone(),
            device_type: target.device_type.clone(),
            managed: target.device.is_managed(),
            fresh: target.fresh,
            data_path: target.device.data_path.clone(),
        },
        app_id: config.app.id.clone(),
        exe: exe_name.clone(),
        bundle: built.bundle.app.clone(),
        pid: None,
        launch_unix_ms: now_ms(),
        logs: SessionLogs {
            stdout: simctl::sim_visible_path(&requested_stdout, data_path),
            stderr: simctl::sim_visible_path(&requested_stderr, data_path),
            oslog,
        },
        collector_pid,
        screen: None,
    };
    let session_path = session::path(&sessions_dir);
    let write_session = |session: &Session| {
        session
            .write(&sessions_dir)
            .map_err(|error| internal(format!("cannot write the session file: {error}")))
    };
    write_session(&session)?;
    ctx.rep
        .set("session", json!(crate::paths::display(&session_path)));

    // Launch.
    let mut launch = simctl(&xcode)
        .args(["launch", "--terminate-running-process"])
        .arg(format!("--stdout={}", requested_stdout.display()))
        .arg(format!("--stderr={}", requested_stderr.display()))
        .arg(&target.device.udid)
        .arg(&config.app.id)
        .env("SIMCTL_CHILD_RUST_BACKTRACE", "1")
        .env("SIMCTL_CHILD_ICM_EVENTS", "1")
        .env("SIMCTL_CHILD_ICM_RUN_ID", &run_id)
        .timeout(Duration::from_secs(120));
    if !extra_env.iter().any(|(key, _)| key == "RUST_LOG") {
        launch = launch.env("SIMCTL_CHILD_RUST_LOG", "info");
    }
    for (key, value) in &extra_env {
        launch = launch.env(format!("SIMCTL_CHILD_{key}"), value);
    }
    session.launch_unix_ms = now_ms();
    let launched_at = Instant::now();
    let outcome = ctx.step("simctl.launch", &launch)?;
    if !outcome.success() {
        let text = format!("{}{}", outcome.stdout_text(), outcome.stderr_text());
        session.state = "exited".to_string();
        let _ = write_session(&session);
        ctx.rep.set(
            "process",
            json!({"pid": null, "alive": false, "ready": {"source": "none", "ms": null}}),
        );
        let error = died(
            ctx,
            &xcode,
            &session,
            &run_dir,
            None,
            launched_at.elapsed(),
            Some(text.trim().lines().last().unwrap_or("").to_string()),
        );
        stop_collector(&mut session);
        let _ = write_session(&session);
        return Err(error);
    }
    let pid = outcome
        .stdout_text()
        .lines()
        .find_map(|line| {
            line.strip_prefix(&format!("{}:", config.app.id))
                .and_then(|pid| pid.trim().parse::<i32>().ok())
        })
        .ok_or_else(|| {
            IcmError::new(
                CheckId::ToolFailed,
                format!(
                    "cannot read the pid from simctl launch: {}",
                    outcome.stdout_text().trim()
                ),
            )
        })?;
    session.pid = Some(i64::from(pid));
    write_session(&session)?;

    let readiness = wait_ready(
        ctx,
        &xcode,
        &session,
        pid,
        launched_at,
        args.wait_ready,
        &run_dir.join("probe.png"),
    )?;
    let process = |alive: bool, source: &str, ms: Option<u64>| json!({"pid": pid, "alive": alive, "ready": {"source": source, "ms": ms}});

    let (source, ms, window, late_panic) = match readiness {
        Readiness::Ready {
            source,
            ms,
            window,
            panic,
        } => (source, ms, window, panic),
        Readiness::Died { panic } => {
            ctx.rep.set("process", process(false, "none", None));
            let error = died(
                ctx,
                &xcode,
                &session,
                &run_dir,
                panic,
                launched_at.elapsed(),
                None,
            );
            stop_collector(&mut session);
            session.state = "exited".to_string();
            let _ = write_session(&session);
            ctx.rep.next("icm logs ios-sim --json", "the app's output");
            return Err(error);
        }
        Readiness::NotReady {
            saw_start,
            waited,
            overall,
        } => {
            ctx.rep.set("process", process(true, "none", None));
            let _ = screenshot(
                ctx,
                &xcode,
                &session.device.udid,
                &run_dir,
                "screen",
                None,
                args.expect_content,
            );
            snapshot_logs(ctx, &session, &run_dir);
            let waited = crate::time::format_duration(waited);
            ctx.rep.next("icm stop ios-sim", "terminate the app");
            if overall {
                // The app may be fine: icm ran out of time, not the app.
                return Err(IcmError::new(
                    CheckId::StepTimeout,
                    format!(
                        "the overall --timeout ran out after {waited} while waiting for the app's first frame"
                    ),
                )
                .evidence(Evidence::file(run_dir.join("app.stderr"))));
            }
            let detail = if saw_start {
                format!("the app started (ICM_EVENT start) but drew no first frame within {waited}")
            } else {
                format!(
                    "the app is alive but sent no ICM_EVENT and the probe saw no content within {waited}"
                )
            };
            ctx.rep.next(
                "icm logs ios-sim --level warn --json",
                "why it did not draw",
            );
            return Err(IcmError::new(CheckId::RunNotReady, detail)
                .evidence(Evidence::file(run_dir.join("app.stderr"))));
        }
    };

    let scale = window
        .as_ref()
        .and_then(|w| w.get("scale"))
        .and_then(Value::as_f64)
        .or_else(|| type_scale(ctx, target.type_bundle.as_deref()));
    let size = window
        .as_ref()
        .and_then(|w| w.get("size"))
        .and_then(Value::as_array)
        .map(|size| {
            size.iter()
                .filter_map(Value::as_f64)
                .map(|v| format!("{v}"))
                .collect::<Vec<_>>()
                .join("x")
        });
    let mut ready = json!({
        "session": crate::paths::display(&session_path),
        "source": source,
        "ms_since_launch": ms,
    });
    if let Some(window) = &window {
        ready["window"] = json!({"size": window.get("size"), "scale": window.get("scale")});
    }
    ctx.rep.ready(ready);
    ctx.rep.check(Check::pass(
        CheckId::RunReady,
        format!(
            "first frame {}after {} (source: {source})",
            match (&size, scale) {
                (Some(size), Some(scale)) => format!("{size}@{scale} "),
                _ => String::new(),
            },
            crate::time::format_duration(Duration::from_millis(ms))
        ),
    ));
    if let Some(panic) = late_panic {
        ctx.rep.check(Check::fail(
            CheckId::RunAppPanicked,
            format!(
                "a thread panicked at {}: {} (the app kept running)",
                panic.location.as_deref().unwrap_or("an unknown location"),
                panic.message
            ),
        ));
    }

    if !args.no_shot {
        sleep_checked(args.settle)?;
        let screen = screenshot(
            ctx,
            &xcode,
            &session.device.udid,
            &run_dir,
            "screen",
            scale,
            args.expect_content,
        )?;
        session.screen = Some(screen.to_json());
    } else if let Some(scale) = scale {
        session.screen = Some(json!({"scale": scale}));
    }

    let alive = crate::signals::alive(pid);
    ctx.rep.set("process", process(alive, source, Some(ms)));
    snapshot_logs(ctx, &session, &run_dir);
    write_session(&session)?;
    if !alive {
        let error = died(
            ctx,
            &xcode,
            &session,
            &run_dir,
            None,
            launched_at.elapsed(),
            None,
        );
        stop_collector(&mut session);
        session.state = "exited".to_string();
        let _ = write_session(&session);
        return Err(error);
    }
    ctx.rep.check(Check::pass(
        CheckId::RunAlive,
        format!("pid {pid} is running"),
    ));

    // The project's `[checks] ios-sim` scripts (design §13.6).
    if crate::hooks::configured(&project, PLATFORM) {
        let sim_data = ctx
            .probe(
                &simctl(&xcode)
                    .args([
                        "get_app_container",
                        &session.device.udid,
                        &session.app_id,
                        "data",
                    ])
                    .timeout(Duration::from_secs(30)),
            )
            .ok()
            .filter(|outcome| outcome.success())
            .map(|outcome| PathBuf::from(outcome.stdout_text().trim()));
        crate::hooks::run_for(
            ctx,
            &project,
            &crate::hooks::HookContext {
                platform: PLATFORM.to_string(),
                pid: u32::try_from(pid).ok(),
                device: Some(session.device.udid.clone()),
                bin: Some(session.bundle.clone()),
                app_stderr: Some(session.logs.stderr.clone()),
                logs: Some(run_dir.join("logs.ndjson")),
                log_mark: Some(session.launch_unix_ms.to_string()),
                sim_data,
                ..crate::hooks::HookContext::default()
            },
        )?;
    }

    ctx.rep.summary(format!(
        "{} ({}) is running on {} (iOS {}); first frame after {} (source: {source})",
        config.app.name,
        config.app.id,
        target.device.name,
        target.os,
        crate::time::format_duration(Duration::from_millis(ms))
    ));
    ctx.rep.next(
        "icm logs ios-sim --level warn --json",
        "read the app's output",
    );
    ctx.rep
        .next("icm shot ios-sim --json -q", "a new screenshot");
    ctx.rep.next("icm stop ios-sim", "terminate the app");

    if args.attach {
        // The run's work is done: `icm stop ios-sim` (which takes this
        // lock) must work while --attach streams.
        drop(lock);
        if follow(ctx, &session, &logs::Filter::default(), true)? == Followed::AppGone {
            // `icm stop ios-sim` marks the session stopped before the app
            // goes: that stop was asked for. Any other exit is the app's.
            let stopped = Session::read(&sessions_dir)
                .is_some_and(|now| now.run == session.run && now.state == "stopped");
            if stopped {
                ctx.rep.summary("the app was stopped (icm stop ios-sim)");
                return Ok(());
            }
            let stderr = std::fs::read_to_string(&session.logs.stderr).unwrap_or_default();
            let panic = logs::find_panic(&stderr);
            stop_collector(&mut session);
            session.state = "exited".to_string();
            let _ = write_session(&session);
            ctx.rep.set("process", process(false, source, Some(ms)));
            ctx.rep.clear_summary();
            let mut error = died(
                ctx,
                &xcode,
                &session,
                &run_dir,
                panic,
                launched_at.elapsed(),
                None,
            );
            error.detail = format!("{} (while attached)", error.detail);
            return Err(error);
        }
    }
    Ok(())
}

/// How [`follow`] ended.
#[derive(Debug, PartialEq, Eq)]
enum Followed {
    /// `--timeout`, or not following until the app exits.
    Ended,
    /// The app's process is gone.
    AppGone,
}

/// Stops the session's `log stream` collector (its process group), but
/// only while the recorded pid still runs that collector: once it has died
/// (the simulator shut down, a reboot) the pid may belong to anything.
fn stop_collector(session: &mut Session) {
    if let Some(pid) = session.collector_pid.take()
        && crate::sessions::command_line(pid)
            .is_some_and(|line| is_collector(&line, &session.device.udid))
    {
        crate::signals::kill_group(pid, libc::SIGTERM);
    }
}

/// Whether a command line is the collector [`start_collector`] starts for
/// the simulator `udid` (`… simctl spawn <udid> log stream …`).
fn is_collector(command_line: &str, udid: &str) -> bool {
    command_line.contains(&format!("spawn {udid} log stream"))
}

// ---- logs ----------------------------------------------------------------------------------

fn no_session() -> IcmError {
    IcmError::new(
        CheckId::RunNoSession,
        "no ios-sim session: nothing was launched with `icm run ios-sim` in this project",
    )
    .fix("Start the app first.", &["icm run ios-sim --json -q"])
}

fn read_session(ctx: &mut Ctx) -> Result<(Project, Session)> {
    let project = ctx.project()?.clone();
    let session = Session::read(&project.sessions_dir()).ok_or_else(no_session)?;
    Ok((project, session))
}

fn session_device(session: &Session) -> Value {
    json!({
        "kind": "simulator",
        "udid": session.device.udid,
        "name": session.device.name,
        "os": session.device.os,
        "type": session.device.device_type,
        "managed": session.device.managed,
        "fresh": session.device.fresh,
    })
}

/// The `Filter` for `icm logs`' flags.
fn filter_for(args: &LogsArgs) -> Result<logs::Filter> {
    let since_unix_ms = match args.since.trim() {
        "launch" | "" => None,
        other => {
            let duration = crate::time::parse_duration(other).map_err(|detail| {
                IcmError::new(
                    CheckId::UsageBadArgs,
                    format!("--since: {detail} (or `launch`)"),
                )
            })?;
            Some(now_ms() - duration.as_millis() as i64)
        }
    };
    Ok(logs::Filter {
        level: args.level,
        sources: match args.source {
            Some(LogSource::App) => &["app"],
            Some(LogSource::System) => &["system"],
            Some(LogSource::Crash) => &["crash"],
            Some(LogSource::All) => &[],
            // The system log is mostly other processes' errors about the
            // app (runningboard, FrontBoard, preferences): noise unless
            // asked for.
            None => &["app", "crash"],
        },
        grep: args.grep.clone(),
        since_unix_ms,
        tail: Some(args.tail),
    })
}

fn emit_record(ctx: &Ctx, record: &logs::Record) {
    let mut event = serde_json::to_value(record).unwrap_or(Value::Null);
    event["type"] = json!("log");
    ctx.rep.emit(event);
}

/// Streams new records until Ctrl-C, the `--timeout`, or (with
/// `stop_on_exit`) the app's death.
fn follow(
    ctx: &Ctx,
    session: &Session,
    filter: &logs::Filter,
    stop_on_exit: bool,
) -> Result<Followed> {
    let mut seen = [0usize; 3];
    let filter = logs::Filter {
        tail: None,
        ..filter.clone()
    };
    loop {
        // Each source only grows: what lies past the last count is new.
        let parts = collect_parts(ctx, None, session, false, false);
        for (index, records) in [parts.stderr, parts.stdout, parts.timed]
            .into_iter()
            .enumerate()
        {
            if records.len() > seen[index] {
                for record in filter.apply(records[seen[index]..].to_vec()) {
                    emit_record(ctx, &record);
                }
                seen[index] = records.len();
            }
        }
        if let Some(signal) = crate::signals::pending() {
            return Err(crate::output::interrupted(signal));
        }
        if ctx.remaining().is_some_and(|left| left.is_zero()) {
            return Ok(Followed::Ended);
        }
        if stop_on_exit && !session.app_alive() {
            return Ok(Followed::AppGone);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// `icm logs ios-sim`.
pub fn logs(ctx: &mut Ctx, args: &LogsArgs) -> Result<()> {
    if ctx.dry_run() {
        let _ = args;
        return dry_run(
            ctx,
            &[(
                "ios-sim.logs",
                "read the session's stdout and stderr, its log stream collector and crash reports"
                    .to_string(),
            )],
        );
    }
    let (_project, session) = read_session(ctx)?;
    let filter = filter_for(args)?;
    ctx.rep.set("device", session_device(&session));
    let alive = session.app_alive();
    ctx.rep.set(
        "process",
        json!({"pid": session.pid, "alive": alive, "ready": null}),
    );
    ctx.rep.set(
        "sources",
        json!({
            "stdout": crate::paths::display(&session.logs.stdout),
            "stderr": crate::paths::display(&session.logs.stderr),
            "oslog": crate::paths::display(&session.logs.oslog),
            "launch": logs::rfc3339_ms(session.launch_unix_ms),
        }),
    );

    if args.follow {
        return follow(ctx, &session, &filter, false).map(|_| ());
    }

    let wants_system = matches!(args.source, Some(LogSource::System | LogSource::All));
    let xcode = if wants_system {
        crate::tools::xcode(&ctx.env).ok()
    } else {
        None
    };
    let (records, notes) = collect_records(ctx, xcode.as_ref(), &session, wants_system, args.raw);
    for note in &notes {
        ctx.rep.progress(note);
    }
    ctx.rep.set("notes", json!(notes));
    let total = records.len();
    let kept = filter.apply(records);
    crate::grep::warn_unmatched(ctx, args.grep.as_deref(), kept.len(), total);
    for record in &kept {
        emit_record(ctx, record);
    }
    if let Some(run_dir) = ctx.rep.run_dir() {
        let (app_log, ndjson) = write_records(&run_dir, &kept);
        ctx.rep.artifact("app_log", &app_log);
        ctx.rep.artifact("logs", &ndjson);
    }
    ctx.rep.set("count", json!(kept.len()));
    ctx.rep.set("records", json!(kept));
    ctx.rep.summary(format!(
        "{} of {total} record(s) since {} from {} (app {})",
        kept.len(),
        logs::rfc3339_ms(session.launch_unix_ms),
        session.device.name,
        if alive { "running" } else { "not running" }
    ));
    Ok(())
}

// ---- shot ----------------------------------------------------------------------------------

/// `icm shot ios-sim`.
pub fn shot(ctx: &mut Ctx, args: &ShotArgs) -> Result<()> {
    if ctx.dry_run() {
        return dry_run(
            ctx,
            &[(
                "ios-sim.screenshot",
                format!(
                    "xcrun simctl io <the session's simulator> screenshot into {}",
                    args.out
                        .as_deref()
                        .map(crate::paths::display)
                        .unwrap_or_else(|| "the run directory".to_string())
                ),
            )],
        );
    }
    let (project, mut session) = read_session(ctx)?;
    let xcode = crate::tools::xcode(&ctx.env)?;
    ctx.rep.latest(PLATFORM);
    ctx.rep.set("device", session_device(&session));
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
    let scale = session
        .screen
        .as_ref()
        .and_then(|s| s.get("scale"))
        .and_then(Value::as_f64);
    let screen = screenshot(
        ctx,
        &xcode,
        &session.device.udid,
        &run_dir,
        &name,
        scale,
        false,
    )
    .map_err(|error| {
        if error.id == CheckId::RunNoSession.id() {
            no_session().fix(
                "The simulator is not booted; start the app again.",
                &["icm run ios-sim --json -q"],
            )
        } else {
            error
        }
    })?;
    if let Some(out) = &args.out {
        let from = run_dir.join(format!("{name}.png"));
        if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::copy(&from, out).map_err(|error| {
            IcmError::new(
                CheckId::UsageBadArgs,
                format!("cannot write --out {}: {error}", out.display()),
            )
        })?;
        ctx.rep.artifact("out", out);
    }
    let alive = session.app_alive();
    ctx.rep.set(
        "process",
        json!({"pid": session.pid, "alive": alive, "ready": null}),
    );
    if !alive {
        ctx.rep.check(Check::warn(
            CheckId::RunAppDied,
            "the app is not running; the screenshot shows the simulator",
        ));
    }
    session.screen = Some(screen.to_json());
    let _ = session.write(&project.sessions_dir());
    ctx.rep.summary(format!(
        "screenshot of {} ({}x{} px, preview {}x{})",
        session.device.name, screen.px.0, screen.px.1, screen.preview.0, screen.preview.1
    ));
    Ok(())
}

// ---- stop ----------------------------------------------------------------------------------

/// `icm stop ios-sim`.
pub fn stop(ctx: &mut Ctx, args: &StopArgs) -> Result<()> {
    if ctx.dry_run() {
        let project = ctx.project()?.clone();
        let mut steps = vec![(
            "ios-sim.stop",
            format!(
                "xcrun simctl terminate the app on the session's simulator ({}) and stop its log collector",
                crate::paths::display(&project.sessions_dir().join("ios-sim.json"))
            ),
        )];
        if args.shutdown {
            steps.push((
                "ios-sim.shutdown",
                "xcrun simctl shutdown the icm-managed simulator (icm-* names only, never icm-test-*)"
                    .to_string(),
            ));
        }
        return dry_run(ctx, &steps);
    }
    stop_session(ctx, args.shutdown).map(|_| ())
}

/// `--dry-run` for the commands that act on the running session: their
/// steps, as a plan; nothing runs.
pub fn dry_run(ctx: &Ctx, steps: &[(&str, String)]) -> Result<()> {
    let mut plan = Plan::new();
    for (name, description) in steps {
        plan.push(Step::internal(name, description));
    }
    plan.report(ctx);
    ctx.rep
        .summary("the plan (--dry-run: nothing ran on the simulator)");
    Ok(())
}

/// Stops this project's ios-sim session, if any (for `icm stop --all` too):
/// terminates the app and the log collector; with `shutdown`, shuts icm's
/// own simulator down. A `--fresh` simulator is deleted. Returns what it
/// stopped (`{platform, app, pid, device, stopped, did}`), or `None`
/// without a session.
pub fn stop_session(ctx: &mut Ctx, shutdown: bool) -> Result<Option<Value>> {
    let project = ctx.project()?.clone();
    let sessions_dir = project.sessions_dir();
    let Some(mut session) = Session::read(&sessions_dir) else {
        ctx.rep.check(Check::skip(
            CheckId::RunNoSession,
            "no ios-sim session to stop",
        ));
        ctx.rep.summary("no ios-sim session to stop");
        return Ok(None);
    };
    let xcode = crate::tools::xcode(&ctx.env)?;
    let _lock = ctx.lock_platform(PLATFORM)?;
    ctx.rep.set("device", session_device(&session));

    let was_alive = session.app_alive();
    if was_alive {
        let outcome = ctx.step(
            "simctl.terminate",
            &simctl(&xcode)
                .args(["terminate", &session.device.udid, &session.app_id])
                .timeout(Duration::from_secs(60)),
        )?;
        if !outcome.success() && session.app_alive() {
            return Err(ctx.step_failure("simctl.terminate", CheckId::ToolFailed, &outcome));
        }
    }
    stop_collector(&mut session);

    let managed = crate::managed::is_managed(&session.device.name);
    let mut did = vec![if was_alive {
        format!("terminated {}", session.app_id)
    } else {
        format!("{} was not running", session.app_id)
    }];
    if managed && (shutdown || session.device.fresh) {
        let outcome = ctx.step(
            "simctl.shutdown",
            &simctl(&xcode)
                .args(["shutdown", &session.device.udid])
                .timeout(Duration::from_secs(120)),
        )?;
        if outcome.success() {
            did.push(format!("shut down {}", session.device.name));
        }
        if session.device.fresh {
            let outcome = ctx.step(
                "simctl.delete",
                &simctl(&xcode)
                    .args(["delete", &session.device.udid])
                    .timeout(Duration::from_secs(120)),
            )?;
            if outcome.success() {
                did.push(format!("deleted {}", session.device.name));
            }
        }
    } else if shutdown && !managed {
        ctx.rep.check(Check::info(
            CheckId::RunNoSession,
            format!(
                "left {} running: icm shuts down only the simulators it created (icm-*, never icm-test-*)",
                session.device.name
            ),
        ));
    }

    session.state = "stopped".to_string();
    session
        .write(&sessions_dir)
        .map_err(|error| internal(format!("cannot write the session file: {error}")))?;
    ctx.rep.set(
        "process",
        json!({"pid": session.pid, "alive": session.app_alive(), "ready": null}),
    );
    ctx.rep.summary(did.join("; "));
    Ok(Some(json!({
        "platform": PLATFORM,
        "app": session.app_id,
        "pid": session.pid,
        "device": session.device.name,
        "stopped": if was_alive { "terminated" } else { "was not running" },
        "did": did,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    #[test]
    fn env_values_parse() {
        assert_eq!(
            app_env(&["A=1".into(), "B=x=y".into()]).unwrap(),
            vec![("A".into(), "1".into()), ("B".into(), "x=y".into())]
        );
        assert!(app_env(&["nope".into()]).is_err());
        assert!(app_env(&["=1".into()]).is_err());
    }

    #[test]
    fn a_reused_collector_pid_is_never_signalled() {
        let udid = "00000000-AAAA-BBBB-CCCC-000000000000";
        assert!(is_collector(
            &format!(
                "/usr/bin/xcrun simctl spawn {udid} log stream --level debug --style ndjson --predicate x"
            ),
            udid
        ));
        assert!(!is_collector("/bin/zsh -l", udid));
        assert!(!is_collector(
            "xcrun simctl spawn OTHER log stream --level debug",
            udid
        ));

        // A process group leader that is not the collector survives.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let mut session: Session = serde_json::from_value(json!({
            "schema": session::SCHEMA, "platform": "ios-sim", "run": "r", "run_dir": null,
            "state": "running",
            "device": {"udid": udid, "name": "icm-x", "os": "27.0", "type": "iPhone 17",
                       "managed": true, "fresh": false, "data_path": null},
            "app_id": "com.x", "exe": "app", "bundle": "/b/App.app", "pid": null,
            "launch_unix_ms": 1,
            "logs": {"stdout": "/o", "stderr": "/e", "oslog": "/l"},
            "collector_pid": null
        }))
        .unwrap();
        session.collector_pid = Some(child.id() as i32);
        stop_collector(&mut session);
        assert_eq!(session.collector_pid, None);
        std::thread::sleep(Duration::from_millis(200));
        assert!(child.try_wait().unwrap().is_none(), "sleep was signalled");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn predicates_quote_the_executable() {
        assert_eq!(
            collector_predicate("app"),
            "process == \"app\" AND (subsystem == \"iced\" OR messageType == error OR messageType == fault)"
        );
    }

    #[test]
    fn signatures_name_likely_causes() {
        let causes = signatures(
            "The request to open \"com.x\" failed. FBSOpenApplicationServiceErrorDomain: scene creation failed",
        );
        assert!(causes[0].contains("ios.plist.scene_manifest"));
        assert!(
            signatures("iced: the app's Info.plist has no UIApplicationSceneManifest.")[0]
                .contains("scene_manifest")
        );
        // A normal launch mentions requests to open the app.
        assert!(signatures("Sending request to open \"com.x\"; scene created").is_empty());
        assert!(signatures("all quiet").is_empty());
        assert!(signatures("Failed to find an appropriate adapter")[0].contains("tiny-skia"));
    }
}
