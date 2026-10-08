//! The Android commands (design §10.4, §13):
//!
//! - `build`: the signed dev APK for one ABI;
//! - `run`: device (booting the managed emulator while the build runs),
//!   build, install, `setprop debug.icm.events 1`, launch, readiness
//!   (`ICM_EVENT ready`, else a probe), screenshot, logs, session;
//! - `shot`, `logs`, `input`, `stop`, `devices` on the session's device;

use super::Toolset;
use super::adb::{self, Adb};
use super::apk;
use super::avd;
use super::device::{self, Chosen};
use super::image;
use super::logcat::{self, Record};
use super::manifest::{ACTIVITY, Axis};
use super::session::{self, Geometry, Session};
use crate::catalogue::CheckId;
use crate::cli::{
    BuildArgs, InputAction, InputArgs, Key, LogSource, LogsArgs, Rotation, RunArgs, ShotArgs,
    StopArgs, Theme,
};
use crate::config::Abi;
use crate::context::{Ctx, Project};
use crate::error::{Check, Evidence, IcmError, Result, Status};
use crate::host::HostConfig;
use crate::managed::Owner;
use crate::screen::{Screen, Space, preview_size};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn setup(ctx: &mut Ctx) -> Result<(Project, HostConfig, Toolset)> {
    let project = ctx.project()?.clone();
    let host = ctx.host()?.clone();
    let tools = Toolset::discover(&host, &ctx.env)?;
    Ok((project, host, tools))
}

fn run_dir(ctx: &Ctx, project: &Project) -> PathBuf {
    ctx.rep
        .run_dir()
        .unwrap_or_else(|| project.runs_dir().join(ctx.rep.run_id()))
}

fn internal(what: &str, error: impl std::fmt::Display) -> IcmError {
    IcmError::new(CheckId::InternalBug, format!("{what}: {error}"))
}

// ---- build ---------------------------------------------------------------------

/// `icm build android`: the signed dev APK for `--abi`, the `--device`'s
/// ABI, or the host's emulator ABI.
pub fn build(ctx: &mut Ctx, args: &BuildArgs) -> Result<()> {
    if ctx.dry_run() {
        return super::plan::build(ctx, args);
    }
    let (project, _host, tools) = setup(ctx)?;
    let _lock = ctx.lock_platform("android")?;
    let ctx: &Ctx = ctx;

    let abi = match (&args.abi, &args.device) {
        (Some(name), _) => Abi::from_name(name).ok_or_else(|| {
            IcmError::new(
                CheckId::UsageBadArgs,
                format!("unknown ABI `{name}`; use arm64-v8a, x86_64, armeabi-v7a or x86"),
            )
        })?,
        (None, Some(serial)) => device::device_abi(&Adb::new(&tools, serial)?)?,
        (None, None) => avd::host_abi(),
    };
    ctx.rep.set(
        "profile",
        json!(crate::cargo::profile_dir(if args.release {
            "release"
        } else {
            "dev"
        })),
    );
    let built = apk::build(ctx, &project, &tools, abi, args.release)?;
    ctx.rep.artifact("apk", &built.apk);
    ctx.rep.summary(format!(
        "built {} ({})",
        crate::paths::display(&built.apk),
        abi.as_str()
    ));
    ctx.rep.next(
        "icm run android --no-build --json -q",
        "install and launch this build",
    );
    Ok(())
}

// ---- run -----------------------------------------------------------------------

/// `(name, value)` system properties.
type Props = Vec<(String, String)>;

/// System properties standing in for `--env` (Android apps get no
/// environment), and the variables that have no stand-in.
fn props_from_env(env: &[String]) -> Result<(Props, Vec<String>)> {
    let mut props = Vec::new();
    let mut ignored = Vec::new();
    for pair in env {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(IcmError::new(
                CheckId::UsageBadArgs,
                format!("--env {pair}: expected K=V"),
            ));
        };
        match key {
            "ICED_BACKEND" => props.push(("debug.iced.backend".to_string(), value.to_string())),
            "ICM_EVENTS" => {}
            _ => ignored.push(key.to_string()),
        }
    }
    Ok((props, ignored))
}

/// `icm run android`.
pub fn run(ctx: &mut Ctx, args: &RunArgs) -> Result<()> {
    if ctx.dry_run() {
        return super::plan::run(ctx, args);
    }
    let Launched {
        project,
        tools,
        adb,
        app_id,
        dir,
        mark,
        pids,
        session,
        apk: apk_path,
        lock: _lock,
        ..
    } = launch_app(ctx, args)?;
    let ctx: &Ctx = ctx;

    // The project's `[checks] android` scripts (design §13.6), with the
    // JDK and SDK environment (Appendix C item 2).
    crate::hooks::run_for(
        ctx,
        &project,
        &crate::hooks::HookContext {
            platform: "android".to_string(),
            pid: session.pid,
            device: Some(adb.serial.clone()),
            adb: Some(format!(
                "{} -s {}",
                crate::process::shell_quote(&tools.sdk.adb(&ctx.env).display().to_string()),
                adb.serial
            )),
            bin: Some(apk_path.clone()),
            logs: Some(dir.join("logs.ndjson")),
            log_mark: Some(mark.clone()),
            env: tools.child_env().to_vec(),
            ..crate::hooks::HookContext::default()
        },
    )?;

    ctx.rep.next(
        "icm logs android --level warn --json",
        "read the app's warnings and errors",
    );
    ctx.rep.next(
        "icm input android tap <x> <y> --json -q",
        "tap in screen.preview.png pixels",
    );
    ctx.rep.next("icm stop android --json -q", "stop the app");

    if args.attach {
        match follow(ctx, &adb, &app_id, &mark, pids.clone(), true, &|_, _| true)? {
            Followed::Gone { stopped: true } => {
                ctx.rep
                    .summary(format!("{app_id} was stopped (am force-stop)"));
            }
            Followed::Gone { stopped: false } => {
                // The app's own exit decides the result, as on the other
                // platforms: a panic or crash is the run's failure.
                let mut error = IcmError::new(
                    CheckId::RunAppDied,
                    format!("{app_id} exited during --attach, after its first frame"),
                );
                let logs = collect_logs(ctx, &adb, &dir, &app_id, &mark, &pids);
                attach_evidence(&mut error, &logs, &project);
                ctx.rep
                    .set("process", json!({"pid": session.pid, "alive": false}));
                ctx.rep.clear_summary();
                return Err(error);
            }
            Followed::Destroyed => {
                // As closing the last window on the desktop: the app ended
                // normally.
                ctx.rep.set(
                    "process",
                    json!({"pid": session.pid, "alive": true, "activity": false}),
                );
                ctx.rep.summary(format!(
                    "{app_id} ended with its activity, which Android destroyed (Back at the app's root, say); its process lives on, cached, with no window"
                ));
            }
            Followed::Ended => {}
        }
    }
    Ok(())
}

/// What [`launch_app`] started: the app running on its device, ready and
/// (unless `--no-shot`) screenshotted, with its session written.
pub(crate) struct Launched {
    /// The project.
    pub project: Project,
    /// The SDK, JDK and NDK.
    pub tools: Toolset,
    /// The device.
    pub adb: Adb,
    /// `[app] id`.
    pub app_id: String,
    /// The run directory.
    pub dir: PathBuf,
    /// The launch mark (device epoch seconds).
    pub mark: String,
    /// The app's pids seen so far.
    pub pids: BTreeSet<u32>,
    /// The session record, as written.
    pub session: Session,
    /// The installed APK (or the `.aab` of `--from-aab`).
    pub apk: PathBuf,
    /// "emulator-5580 (icm-api36, API 36)".
    pub device_name: String,
    /// The platform lock, held while this lives.
    pub lock: crate::locks::Lock,
}

/// The `.aab` of the newest Android release (`dist/latest/android`), for
/// `icm run android --from-aab`.
fn release_bundle(project: &Project) -> Result<PathBuf> {
    let not_found = |detail: String| {
        IcmError::new(CheckId::ReleaseNotFound, detail).fix(
            "Make a release first (an unsigned one is enough: the install re-signs it with icm's debug key).",
            &["icm release android --sign none --allow-dirty --json -q"],
        )
    };
    let latest = crate::release::dist::latest(project, "android");
    let Some(dir) = crate::release::dist::resolve_latest(project, "android") else {
        return Err(not_found(format!(
            "--from-aab: there is no Android release in {}",
            crate::paths::display(&latest)
        )));
    };
    let manifest =
        crate::release::manifest::Manifest::read(&dir.join(crate::release::manifest::FILE))?;
    manifest
        .uploads()
        .find(|file| file.kind == "aab")
        .map(|file| file.absolute(&dir))
        .filter(|path| path.is_file())
        .ok_or_else(|| {
            not_found(format!(
                "--from-aab: {} lists no .aab",
                crate::paths::display(&dir.join(crate::release::manifest::FILE))
            ))
        })
}

/// `icm run android` up to the session: device, build (or the release's
/// bundle with `--from-aab`), install, launch, readiness, screenshot,
/// logs. `icm test --on android --lifecycle` starts the same way.
pub(crate) fn launch_app(ctx: &mut Ctx, args: &RunArgs) -> Result<Launched> {
    let (props, ignored_env) = props_from_env(&args.env)?;
    let (project, host, tools) = setup(ctx)?;
    ctx.rep.latest("android");
    let lock = ctx.lock_platform("android")?;
    let ctx: &Ctx = ctx;
    let bundle = if args.from_aab {
        Some(release_bundle(&project)?)
    } else {
        None
    };

    let config = &project.config.config;
    let app_id = config.app.id.clone();
    let profile = if args.release { "release" } else { "dev" };
    ctx.rep
        .set("profile", json!(crate::cargo::profile_dir(profile)));
    for key in &ignored_env {
        ctx.rep.check(Check::warn(
            CheckId::UsageBadArgs,
            format!(
                "--env {key}: Android apps get no environment variables; only ICED_BACKEND has a stand-in (the sysprop debug.iced.backend)"
            ),
        ));
    }
    let dir = run_dir(ctx, &project);

    // 1. The device; an emulator boots while the app builds.
    let request = device::Request {
        serial: args.device.clone(),
        avd: args.avd.clone(),
        show: args.show,
        wipe: args.fresh,
        target_sdk: config.android.target_sdk,
    };
    let chosen = device::choose(
        ctx,
        &tools,
        &host,
        &ctx.env,
        &request,
        true,
        &dir.join("emulator.log"),
    )?;
    ctx.rep
        .progress(format!("device: {} ({})", chosen.serial, chosen.reason));
    // An emulator an earlier run booted is still icm's to shut down: a
    // rerun on it must not forget that.
    let booted = match &chosen.booting {
        Some(booting) => {
            let booted = session::Booted::of(booting);
            session::write_booted(&project, &booted);
            Some(booted)
        }
        None => {
            // The record, else (files from before it) the last session's.
            // Only an emulator whose recorded process is verified to
            // still run is the one an earlier run booted: a pid that
            // another process has taken, or that has no identity to
            // check, is no proof that the emulator on this serial is
            // this project's, and a rerun must not claim another's.
            let previous = session::read(&project).and_then(|previous| previous.booted());
            session::booted(&project)
                .into_iter()
                .chain(previous)
                .filter(session::Booted::verified)
                .find(|booted| {
                    booted.serial == chosen.serial
                        && chosen.avd.as_deref().is_none_or(|avd| avd == booted.avd)
                })
                .inspect(|booted| session::write_booted(&project, booted))
        }
    };
    let mut session = Session {
        schema: session::SCHEMA.to_string(),
        run: ctx.rep.run_id(),
        run_dir: Some(dir.clone()),
        serial: chosen.serial.clone(),
        kind: chosen.kind().to_string(),
        avd: chosen.avd.clone(),
        abi: chosen.abi.as_str().to_string(),
        app_id: app_id.clone(),
        started: crate::time::Utc::now().rfc3339(),
        ..Session::default()
    };
    if let Some(booted) = &booted {
        session.run_on(booted);
    }
    if let Some(booting) = &chosen.booting {
        // Recorded now, so `icm stop android --shutdown` finds the emulator
        // even when the build fails.
        let _ = session::write(&project, &session);
        ctx.rep.artifact("emulator_log", &booting.log);
    }

    // 2. The APK, or the release's bundle.
    let apk_path = if let Some(aab) = &bundle {
        let names = crate::release::notices::zip_names(aab).unwrap_or_default();
        if !super::bundle::libraries(&names)
            .iter()
            .any(|(abi, _)| abi == chosen.abi.as_str())
        {
            return Err(IcmError::new(
                CheckId::AndroidSoAbis,
                format!(
                    "--from-aab: {} has no library for {}, the device's ABI",
                    crate::paths::display(aab),
                    chosen.abi.as_str()
                ),
            )
            .fix(
                format!(
                    "Add \"{}\" to [android] abis in icm.toml and release again.",
                    chosen.abi.as_str()
                ),
                &["icm release android --sign none --allow-dirty --json -q"],
            ));
        }
        aab.clone()
    } else if args.no_build {
        let package = project.package_for("android")?;
        let path = apk::apk_path(&project, &package.name, profile);
        if !path.is_file() {
            return Err(IcmError::new(
                CheckId::UsageBadArgs,
                format!(
                    "--no-build: there is no APK at {} from an earlier build",
                    crate::paths::display(&path)
                ),
            )
            .fix("Build it first.", &["icm build android", "icm run android"]));
        }
        let names = super::zip::names(&path).unwrap_or_default();
        if !names
            .iter()
            .any(|name| name.starts_with(&format!("lib/{}/", chosen.abi.as_str())))
        {
            return Err(IcmError::new(
                CheckId::AndroidSoAbis,
                format!(
                    "--no-build: {} has no library for {}, the device's ABI",
                    crate::paths::display(&path),
                    chosen.abi.as_str()
                ),
            )
            .fix_commands([format!("icm build android --abi {}", chosen.abi.as_str())]));
        }
        path
    } else {
        apk::build(ctx, &project, &tools, chosen.abi, args.release)?.apk
    };
    ctx.rep
        .artifact(if bundle.is_some() { "aab" } else { "apk" }, &apk_path);
    session.apk = Some(apk_path.clone());

    // 3. Boot, install, launch.
    if let Some(booting) = &chosen.booting {
        ctx.rep
            .progress(format!("waiting for {} to boot", booting.serial));
        let took = avd::wait_booted(ctx, &tools, booting)?;
        ctx.rep.progress(format!(
            "{} booted in {}",
            booting.serial,
            crate::time::format_duration(took)
        ));
    }
    let adb = Adb::new(&tools, &chosen.serial)?;
    if chosen.managed() {
        avd::prepare(&adb);
    }
    // An emulator icm booted for this project, now or in an earlier run
    // (whose build may have failed before the claim), says so.
    let own = session::owner_tag(&project);
    if session.booted_by_icm {
        claim(ctx, &adb, &own);
    } else if let Owner::Project(owner) = emulator_owner(&adb)
        && owner != own
    {
        ctx.rep.check(Check::info(
            CheckId::AndroidEmulatorShared,
            format!(
                "{} ({}) was booted by icm for another project (debug.icm.booted_by {owner}), which may still use it: its app and this one share the screen, and `icm stop --shutdown` here leaves the emulator running",
                chosen.serial,
                chosen.avd.as_deref().unwrap_or("unknown AVD")
            ),
        ));
    }
    let device_json = device::to_json(&chosen, &adb);
    ctx.rep.set("device", device_json.clone());
    // "emulator-5580 (icm-api36, API 36)" for the summary.
    let device_name = {
        let mut about: Vec<String> = Vec::new();
        if let Some(name) = chosen
            .avd
            .clone()
            .or_else(|| device_json["model"].as_str().map(str::to_string))
        {
            about.push(name);
        }
        if let Some(api) = device_json["api"].as_u64() {
            about.push(format!("API {api}"));
        }
        if about.is_empty() {
            chosen.serial.clone()
        } else {
            format!("{} ({})", chosen.serial, about.join(", "))
        }
    };

    match &bundle {
        Some(aab) => {
            if args.reinstall {
                uninstall(ctx, &adb, &app_id, args.wipe_data)?;
            }
            let (bundletool, _) = tools.bundletool(ctx)?;
            let work = project.gen_dir("android", "release").join("bundle");
            install_bundle(ctx, &tools, &adb, &bundletool, aab, &app_id, &work)?;
        }
        None => install(
            ctx,
            &adb,
            &apk_path,
            &app_id,
            args.reinstall,
            args.wipe_data,
        )?,
    }

    let mark = adb.epoch().ok_or_else(|| {
        IcmError::new(
            CheckId::AndroidDeviceNone,
            format!("{} does not answer `date`", adb.serial),
        )
    })?;
    session.log_mark = Some(mark.clone());
    set_props(ctx, &adb, &props)?;

    let launched = Instant::now();
    launch(ctx, &adb, &app_id)?;

    // 4. Ready, screenshot, logs.
    let outcome = wait_ready(ctx, &adb, &app_id, &mark, launched, args.wait_ready);
    let pids: BTreeSet<u32> = match &outcome {
        Ok(ready) => ready.pids.clone(),
        Err(_) => adb.pids(&app_id).into_iter().collect(),
    };
    session.pid = match &outcome {
        Ok(ready) => ready.pid.or_else(|| pids.iter().next().copied()),
        Err(_) => pids.iter().next().copied(),
    };

    let mut result = outcome.and_then(|ready| {
        let readiness = report_ready(ctx, &project, &ready, launched);
        ctx.rep.summary(format!(
            "{} ({app_id}) is running on {device_name}; {readiness}",
            config.app.name
        ));
        if !args.no_shot {
            std::thread::sleep(args.settle);
            let shot = capture(ctx, &adb, &dir, "screen", &app_id, args.expect_content)?;
            session.screen = Some(Geometry::from(&shot));
            // Dying between the first frame and the screenshot is dying.
            if adb.pids(&app_id).is_empty() {
                return Err(IcmError::new(
                    CheckId::RunAppDied,
                    format!("{app_id} exited after its first frame"),
                ));
            }
        }
        ctx.rep.set(
            "process",
            json!({
                "pid": ready.pid,
                "alive": true,
                "ready": {"source": ready.source, "ms": ready.ms},
            }),
        );
        Ok(())
    });

    let logs = collect_logs(ctx, &adb, &dir, &app_id, &mark, &pids);
    let recreated = recreation(ctx, &adb, &dir, &project, &mark, &pids);
    if let Err(error) = &mut result {
        attach_evidence(error, &logs, &project);
        if let Some(recreated) = &recreated {
            // The relaunch is why the app never drew, or stopped answering.
            error.likely_causes.insert(0, recreated.cause.clone());
            if [CheckId::RunNotReady.id(), CheckId::RunAnr.id()].contains(&&*error.id) {
                error.fix.summary = recreated.fix.clone();
                error.fix.commands = recreated.commands.clone();
                if let Some(evidence) = &recreated.evidence {
                    error.evidence.insert(0, evidence.clone());
                }
            }
        }
        ctx.rep.set(
            "process",
            json!({
                "pid": session.pid,
                "alive": !adb.pids(&app_id).is_empty(),
                "ready": {"source": "none", "ms": null},
            }),
        );
    }

    if let Ok(path) = session::write(&project, &session) {
        ctx.rep.set("session", json!(crate::paths::display(&path)));
    }
    result?;
    Ok(Launched {
        project,
        tools,
        adb,
        app_id,
        dir,
        mark,
        pids,
        session,
        apk: apk_path,
        device_name,
        lock,
    })
}

pub(super) fn set_props(ctx: &Ctx, adb: &Adb, props: &[(String, String)]) -> Result<()> {
    // Events are opt-in for every build (Appendix C item 27).
    let mut lines = vec!["setprop debug.icm.events 1".to_string()];
    let backend = props
        .iter()
        .find(|(key, _)| key == "debug.iced.backend")
        .map(|(_, value)| value.clone());
    match backend {
        Some(value) => lines.push(format!("setprop debug.iced.backend {}", adb::quote(&value))),
        // A backend an earlier run chose must not leak into this one.
        None => lines.push("setprop debug.iced.backend ''".to_string()),
    }
    let cmd = adb
        .shell(&lines.join(" && "))
        .timeout(Duration::from_secs(30));
    let outcome = ctx.step("adb.setprop", &cmd)?;
    if outcome.success() {
        Ok(())
    } else {
        Err(ctx.step_failure("adb.setprop", CheckId::ToolFailed, &outcome))
    }
}

/// `pm uninstall` (`-k`: keeping the app's data, unless `wipe`).
fn uninstall(ctx: &Ctx, adb: &Adb, app_id: &str, wipe: bool) -> Result<()> {
    let line = if wipe {
        format!("pm uninstall {}", adb::quote(app_id))
    } else {
        format!("pm uninstall -k {}", adb::quote(app_id))
    };
    // Not installed is fine.
    let _ = ctx.step(
        "adb.uninstall",
        &adb.shell(&line).timeout(Duration::from_secs(120)),
    )?;
    Ok(())
}

fn install(
    ctx: &Ctx,
    adb: &Adb,
    apk: &Path,
    app_id: &str,
    reinstall: bool,
    wipe: bool,
) -> Result<()> {
    if reinstall {
        uninstall(ctx, adb, app_id, wipe)?;
    }

    let cmd = adb
        .cmd(["install", "-r", "-d"])
        .arg(apk)
        .timeout(Duration::from_secs(300));
    let outcome = ctx.step("adb.install", &cmd)?;
    let text = format!("{}\n{}", outcome.stdout_text(), outcome.stderr_text());
    if outcome.success() && text.contains("Success") {
        return Ok(());
    }
    Err(install_error(adb, apk, app_id, &text, &outcome))
}

/// The error of a failed install, from its output.
fn install_error(
    adb: &Adb,
    apk: &Path,
    app_id: &str,
    text: &str,
    outcome: &crate::process::Outcome,
) -> IcmError {
    let reason = adb::install_failure(text).unwrap_or_else(|| outcome.describe());
    let mut error = match reason.as_str() {
        "INSTALL_FAILED_UPDATE_INCOMPATIBLE" => IcmError::new(
            CheckId::AndroidInstallSignatureMismatch,
            format!(
                "{app_id} is installed on {} with a different signing key; reinstalling wipes its data",
                adb.serial
            ),
        )
        .fix_commands(["icm run android --reinstall --wipe-data"]),
        "INSTALL_FAILED_NO_MATCHING_ABIS" => IcmError::new(
            CheckId::AndroidSoAbis,
            format!("{} has no library for {}'s ABI", apk.display(), adb.serial),
        ),
        _ => IcmError::new(
            CheckId::AndroidInstallFailed,
            format!("adb install failed on {}: {reason}", adb.serial),
        ),
    };
    if let Some(log) = &outcome.log {
        error = error.evidence(Evidence::file(log).with_excerpt(reason));
    }
    error
}

/// Installs an App Bundle the way Google Play would for this device:
/// `bundletool build-apks --connected-device` into `<work>/device.apks`,
/// signed with icm's debug key like dev installs (so neither replaces the
/// other's signature; never `~/.android/debug.keystore`), then
/// `bundletool install-apks --allow-downgrade` (as `adb install -d`).
pub(crate) fn install_bundle(
    ctx: &Ctx,
    tools: &Toolset,
    adb: &Adb,
    bundletool: &crate::process::Cmd,
    aab: &Path,
    app_id: &str,
    work: &Path,
) -> Result<()> {
    let keystore = apk::ensure_debug_keystore(ctx, tools)?;
    std::fs::create_dir_all(work).map_err(|e| internal("cannot create the bundle directory", e))?;
    let apks = work.join("device.apks");
    let _ = std::fs::remove_file(&apks);
    let adb_path = tools.sdk.adb(&ctx.env);
    let build = bundletool
        .clone()
        .arg("build-apks")
        .arg(format!("--bundle={}", aab.display()))
        .arg(format!("--output={}", apks.display()))
        .arg("--connected-device")
        .arg(format!("--device-id={}", adb.serial))
        .arg(format!("--adb={}", adb_path.display()))
        .arg(format!("--ks={}", keystore.display()))
        .arg(format!("--ks-pass=pass:{}", super::DEBUG_KEYSTORE_PASS))
        .arg(format!("--ks-key-alias={}", super::DEBUG_KEY_ALIAS))
        .arg(format!("--key-pass=pass:{}", super::DEBUG_KEYSTORE_PASS))
        .timeout(Duration::from_secs(600));
    apk::run_tool(
        ctx,
        "bundletool.build_apks",
        &build,
        CheckId::AndroidBundletoolFailed,
    )?;
    let install = bundletool
        .clone()
        .arg("install-apks")
        .arg(format!("--apks={}", apks.display()))
        .arg(format!("--device-id={}", adb.serial))
        .arg(format!("--adb={}", adb_path.display()))
        .arg("--allow-downgrade")
        .timeout(Duration::from_secs(300));
    let outcome = ctx.step("bundletool.install_apks", &install)?;
    if outcome.success() {
        return Ok(());
    }
    let text = format!("{}\n{}", outcome.stdout_text(), outcome.stderr_text());
    Err(install_error(adb, aab, app_id, &text, &outcome))
}

/// Launches an app installed by [`install_bundle`] and waits for its first
/// frame and a screenshot (`<stem>.png` in `dir`), as `icm run` does: the
/// release's smoke test. Returns how it went, for a person.
pub(crate) fn launch_installed(
    ctx: &Ctx,
    project: &Project,
    adb: &Adb,
    dir: &Path,
    stem: &str,
) -> Result<String> {
    let app_id = project.config.config.app.id.clone();
    let mark = adb.epoch().ok_or_else(|| {
        IcmError::new(
            CheckId::AndroidDeviceNone,
            format!("{} does not answer `date`", adb.serial),
        )
    })?;
    set_props(ctx, adb, &[])?;
    let launched = Instant::now();
    launch(ctx, adb, &app_id)?;
    let fail = |mut error: IcmError| {
        let pids: BTreeSet<u32> = adb.pids(&app_id).into_iter().collect();
        let logs = collect_logs(ctx, adb, dir, &app_id, &mark, &pids);
        attach_evidence(&mut error, &logs, project);
        error
    };
    let ready =
        wait_ready(ctx, adb, &app_id, &mark, launched, Duration::from_secs(30)).map_err(&fail)?;
    std::thread::sleep(Duration::from_millis(1500));
    let shot = capture(ctx, adb, dir, stem, &app_id, false)?;
    if adb.pids(&app_id).is_empty() {
        return Err(fail(IcmError::new(
            CheckId::RunAppDied,
            format!("{app_id} exited after its first frame"),
        )));
    }
    let ms = ready
        .ms
        .unwrap_or_else(|| launched.elapsed().as_millis() as u64);
    Ok(format!(
        "{app_id} drew its first frame after {} (source: {}); screenshot {}x{}",
        crate::time::format_duration(Duration::from_millis(ms)),
        ready.source,
        shot.px.0,
        shot.px.1
    ))
}

pub(super) fn launch(ctx: &Ctx, adb: &Adb, app_id: &str) -> Result<adb::Started> {
    let component = format!("{app_id}/{ACTIVITY}");
    let cmd = adb
        .shell(&format!("am start -W -S -n {}", adb::quote(&component)))
        .timeout(Duration::from_secs(90));
    let outcome = ctx.step("adb.launch", &cmd)?;
    let text = format!("{}\n{}", outcome.stdout_text(), outcome.stderr_text());
    let started = adb::parse_am_start(&text);
    if !outcome.success() || started.error.is_some() {
        let mut error = IcmError::new(
            CheckId::AndroidLaunchFailed,
            format!(
                "am start {component} failed: {}",
                started
                    .error
                    .clone()
                    .unwrap_or_else(|| outcome.stderr_tail(3))
            ),
        );
        if let Some(log) = &outcome.log {
            error = error.evidence(Evidence::file(log));
        }
        return Err(error);
    }
    Ok(started)
}

/// How the app became ready.
#[derive(Clone, Debug)]
pub(super) struct Ready {
    pub(super) source: &'static str,
    pub(super) ms: Option<u64>,
    pub(super) window: Option<Value>,
    /// The process that drew: the `ready` event's, or the probed one.
    pub(super) pid: Option<u32>,
    /// Every pid the app had since the launch.
    pub(super) pids: BTreeSet<u32>,
}

/// How long a relaunched activity gets to draw before the run gives up on
/// it. The relaunch ends the application, and the new activity starts it
/// again in the same process, which takes about as long as a warm start and
/// sends its own `ICM_EVENT start` and `ready`. An app that stays silent
/// this long after a relaunch will not draw (a framework from before the
/// Android lifecycle fix freezes there: winit 0.30.13 does not end its
/// event loop when the activity is destroyed).
const RELAUNCH_GRACE: Duration = Duration::from_secs(10);

/// Waits for `ICM_EVENT ready` in logcat (the framework emits it after
/// the first presented frame) from one of the app's processes
/// ([`processes`]): every iced_mobile app on the device writes these
/// events, so another one's never counts. An app that never speaks the
/// protocol (no `start` event) is ready by probe: alive and the top resumed
/// activity on three polls in a row. A death or panic fails at once; so
/// does an activity Android relaunched that does not draw within
/// [`RELAUNCH_GRACE`], which the probe cannot tell from a live one.
pub(super) fn wait_ready(
    ctx: &Ctx,
    adb: &Adb,
    app_id: &str,
    mark: &str,
    launched: Instant,
    wait: Duration,
) -> Result<Ready> {
    let own = launched + wait;
    let (deadline, overall) = match ctx.deadline() {
        Some(overall) if overall < own => (overall, true),
        _ => (own, false),
    };
    let mut pids: BTreeSet<u32> = BTreeSet::new();
    let mut start_seen = false;
    let mut probes = 0;
    // Whether the resumed-activity probe has looked, and seen the app on
    // top: what a timeout may say about it.
    let mut probed = false;
    let mut resumed_seen = false;
    let mut gone_polls = 0;
    let mut relaunched: Option<Instant> = None;

    loop {
        if let Some(signal) = crate::signals::pending() {
            return Err(crate::output::interrupted(signal));
        }
        // The processes first: a process that starts after this look
        // writes events this poll skips and the next one reads.
        let processes = processes(adb, app_id, &pids);
        let alive = processes.now.clone();
        match hear(&events_since(adb, mark), &processes, &mut pids) {
            Heard::Ready(ready) => return Ok(ready),
            Heard::Panicked(error) => return Err(error),
            Heard::Started => start_seen = true,
            Heard::Silent => {}
        }

        if alive.is_empty() {
            gone_polls += 1;
            // Give a fresh launch a moment to appear; a vanished process
            // (or one that never came) is dead.
            if !pids.is_empty() || gone_polls >= 6 {
                return Err(IcmError::new(
                    CheckId::RunAppDied,
                    format!(
                        "{app_id} exited {} after launch, before its first frame",
                        crate::time::format_duration(launched.elapsed())
                    ),
                ));
            }
        } else {
            gone_polls = 0;
            pids.extend(alive.iter().copied());
        }

        if relaunched.is_none() && !relaunches_since(adb, mark, app_id).is_empty() {
            relaunched = Some(Instant::now());
        }
        if let Some(seen) = relaunched
            && seen.elapsed() >= RELAUNCH_GRACE
        {
            return Err(IcmError::new(
                CheckId::RunNotReady,
                format!(
                    "{app_id} is alive but sent no ICM_EVENT ready in the {} after Android relaunched its activity",
                    crate::time::format_duration(seen.elapsed())
                ),
            ));
        }

        if !start_seen
            && relaunched.is_none()
            && !pids.is_empty()
            && launched.elapsed() >= Duration::from_secs(6)
        {
            probed = true;
            if top_resumed(adb, app_id) {
                resumed_seen = true;
                probes += 1;
                if probes >= 3 {
                    return Ok(Ready {
                        source: "probe",
                        ms: None,
                        window: None,
                        pid: alive.first().copied(),
                        pids,
                    });
                }
            } else {
                probes = 0;
            }
        }

        if Instant::now() >= deadline {
            let waited = crate::time::format_duration(launched.elapsed());
            if overall {
                // The app may be fine: icm ran out of time, not the app.
                return Err(IcmError::new(
                    CheckId::StepTimeout,
                    format!(
                        "the overall --timeout ran out after {waited} while waiting for {app_id}'s first frame"
                    ),
                ));
            }
            // Say only what the polls saw. No process now means none was
            // ever seen (one that vanished failed above as died): the app
            // may still be starting, or may have exited at once. A
            // relaunch keeps the probe off, whenever the wait ends.
            let detail = if alive.is_empty() {
                format!(
                    "{app_id} was not ready within {waited}: icm saw no process of it, so it may still be starting or may have exited"
                )
            } else if let Some(seen) = relaunched {
                format!(
                    "{app_id} is alive but sent no ICM_EVENT{} within {waited}, and Android had relaunched its activity, which icm saw {} before the wait ran out (run.activity_recreated): after a relaunch only the app's ready event counts, as the resumed-activity probe cannot tell an activity that froze from one that draws",
                    if start_seen { " ready" } else { "" },
                    crate::time::format_duration(seen.elapsed())
                )
            } else if start_seen {
                format!("{app_id} is alive but sent no ICM_EVENT ready within {waited}")
            } else if !probed {
                format!(
                    "{app_id} is alive but sent no ICM_EVENT within {waited}, before the resumed-activity probe starts (6s after launch)"
                )
            } else if resumed_seen {
                format!(
                    "{app_id} is alive but sent no ICM_EVENT, and was not the resumed activity on three looks in a row, within {waited}"
                )
            } else {
                format!("{app_id} is alive but never became the resumed activity within {waited}")
            };
            return Err(IcmError::new(CheckId::RunNotReady, detail)
                .fix_commands(["icm logs android --level warn --json".to_string()]));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// What the app's `ICM_EVENT` records say so far.
#[derive(Debug)]
enum Heard {
    /// No event of the app's.
    Silent,
    /// A `start`, and neither `ready` nor `panic` yet.
    Started,
    /// The first `ready`.
    Ready(Ready),
    /// A `panic` before any `ready`.
    Panicked(IcmError),
}

/// Reads the events in `records` that the app's processes wrote, adding
/// their pids to `pids`, up to the first `ready` or `panic`. Other
/// processes' events are skipped, however they look.
fn hear(records: &[Record], processes: &logcat::Processes, pids: &mut BTreeSet<u32>) -> Heard {
    let mut started = false;
    for record in records.iter().filter(|record| processes.owns(record)) {
        let Some(event) = logcat::event(record) else {
            continue;
        };
        let _ = pids.insert(record.pid);
        match logcat::kind(&event) {
            "start" => started = true,
            "ready" => {
                return Heard::Ready(Ready {
                    source: "icm_event",
                    ms: event.get("ms").and_then(Value::as_u64),
                    window: event.get("window").cloned(),
                    pid: Some(record.pid),
                    pids: pids.clone(),
                });
            }
            "panic" => {
                let message = event.get("message").and_then(Value::as_str).unwrap_or("");
                let location = event
                    .get("location")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown location");
                return Heard::Panicked(IcmError::new(
                    CheckId::RunAppPanicked,
                    format!("panicked at {location}: {message}"),
                ));
            }
            _ => {}
        }
    }
    if started {
        Heard::Started
    } else {
        Heard::Silent
    }
}

pub(super) fn events_since(adb: &Adb, mark: &str) -> Vec<Record> {
    let cmd = adb.cmd([
        "logcat",
        "-d",
        "-v",
        "threadtime,epoch",
        "-T",
        mark,
        "-s",
        "ICM_EVENT:I",
    ]);
    adb::quick(cmd, Duration::from_secs(15))
        .map(|outcome| logcat::parse(&outcome.stdout_text()))
        .unwrap_or_default()
}

/// The app's processes: its pids now (`pidof`), the starts and deaths of
/// processes the events buffer still holds, and `known`, pids seen as the
/// app's earlier. Records are the app's only when one of these wrote them
/// ([`logcat::Processes::owns`]).
pub(super) fn processes(adb: &Adb, app_id: &str, known: &BTreeSet<u32>) -> logcat::Processes {
    let now = adb.pids(app_id);
    let mut args: Vec<String> = [
        "logcat",
        "-d",
        "-b",
        "events",
        "-v",
        "threadtime,epoch",
        "-s",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    args.extend(logcat::PROCESS_TAGS.iter().map(|tag| format!("{tag}:I")));
    let events = adb::quick(adb.cmd(&args), Duration::from_secs(15))
        .filter(|outcome| outcome.success())
        .map(|outcome| logcat::parse(&outcome.stdout_text()))
        .unwrap_or_default();
    logcat::Processes::read(app_id, now, &events).knowing(known.iter().copied())
}

/// The events buffer since `mark`, as logcat prints it (`tags` empty:
/// every tag).
pub(super) fn events_buffer(adb: &Adb, mark: &str, tags: &[&str]) -> Option<String> {
    let mut args: Vec<String> = [
        "logcat",
        "-d",
        "-b",
        "events",
        "-v",
        "threadtime,epoch",
        "-T",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    args.push(mark.to_string());
    if !tags.is_empty() {
        args.push("-s".to_string());
        args.extend(tags.iter().map(|tag| format!("{tag}:I")));
    }
    let outcome = adb::quick(adb.cmd(&args), Duration::from_secs(15))?;
    outcome.success().then(|| outcome.stdout_text())
}

/// The relaunches of the app's activity since `mark`.
fn relaunches_since(adb: &Adb, mark: &str, app_id: &str) -> Vec<logcat::Relaunch> {
    events_buffer(adb, mark, logcat::RELAUNCH_TAGS)
        .map(|text| logcat::relaunches(&logcat::parse(&text), app_id))
        .unwrap_or_default()
}

/// The app's activities, from `dumpsys activity activities` (`None` when
/// the device does not answer, or lists no activity at all).
fn activities(adb: &Adb, app_id: &str) -> Option<adb::Activities> {
    adb.shell_text("dumpsys activity activities", Duration::from_secs(15))
        .and_then(|text| adb::parse_activities(&text, app_id))
}

fn top_resumed(adb: &Adb, app_id: &str) -> bool {
    activities(adb, app_id).is_some_and(|activities| activities.top)
}

/// Where the app is on the device. A live process alone does not mean the
/// app is on screen: the framework ends the application with its activity,
/// and after Back at the app's root Android destroys the activity but keeps
/// the process, cached, with no window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Presence {
    /// No process.
    Gone,
    /// A process without an activity: Android destroyed it, and the app
    /// ended.
    NoActivity { pid: u32 },
    /// An activity, with another one in front (after Home, say).
    Behind { pid: u32 },
    /// The top resumed activity. Also what a device whose `dumpsys` says
    /// nothing about activities gets, from its live process.
    Front { pid: u32 },
}

impl Presence {
    /// The result's `process` object.
    fn to_json(self) -> Value {
        match self {
            Presence::Gone => json!({"pid": null, "alive": false, "activity": false}),
            Presence::NoActivity { pid } => {
                json!({"pid": pid, "alive": true, "activity": false})
            }
            Presence::Behind { pid } => {
                json!({"pid": pid, "alive": true, "activity": true, "front": false})
            }
            Presence::Front { pid } => {
                json!({"pid": pid, "alive": true, "activity": true, "front": true})
            }
        }
    }
}

pub(super) fn presence(adb: &Adb, app_id: &str) -> Presence {
    let Some(&pid) = adb.pids(app_id).first() else {
        return Presence::Gone;
    };
    match activities(adb, app_id) {
        Some(adb::Activities { any: false, .. }) => Presence::NoActivity { pid },
        Some(adb::Activities { top: false, .. }) => Presence::Behind { pid },
        _ => Presence::Front { pid },
    }
}

/// The detail for an app whose process outlived its activity.
fn no_activity(app_id: &str, serial: &str, pid: u32) -> String {
    format!(
        "{app_id} has no activity on {serial}: Android destroyed it (Back at the app's root, say) and the app ended, while its process (pid {pid}) lives on, cached, with no window"
    )
}

/// The `adb shell` line that marks an emulator as booted for the project
/// tagged `own` ([`session::OWNER_PROP`]).
fn claim_line(own: &str) -> String {
    format!("setprop {} {}", session::OWNER_PROP, adb::quote(own))
}

/// Records on an emulator icm booted that it did so for the project tagged
/// `own` ([`session::OWNER_PROP`]). When that fails, the emulator looks
/// booted outside icm, and another project's `stop --shutdown` would shut
/// it down under this run's app: a WARN `android.emulator.owner_unknown`
/// says so.
fn claim(ctx: &Ctx, adb: &Adb, own: &str) {
    let why = match adb::quick(adb.shell(&claim_line(own)), Duration::from_secs(15)) {
        Some(outcome) if outcome.success() => return,
        Some(outcome) => crate::managed::failure(&outcome),
        None => "adb could not be started".to_string(),
    };
    ctx.rep.check(
        Check::warn(
            CheckId::AndroidEmulatorOwnerUnknown,
            format!(
                "could not mark {} as this project's ({}, `adb shell setprop` {why}): another project's `icm stop --shutdown` may shut it down while this app runs",
                adb.serial,
                session::OWNER_PROP
            ),
        )
        .fix(
            "Mark it once the emulator answers (this project's next run on it tries again), or give this project its own emulator (`--avd <name>`, or host.toml android.avd):",
            &[&format!("adb -s {} shell {}", adb.serial, claim_line(own))],
        ),
    );
}

/// The project an icm booted the emulator for ([`session::OWNER_PROP`]):
/// nobody when the property is unset, unknown when adb cannot read it.
fn emulator_owner(adb: &Adb) -> Owner {
    let line = format!("getprop {}", adb::quote(session::OWNER_PROP));
    match adb::quick(adb.shell(&line), Duration::from_secs(15)) {
        Some(outcome) => Owner::from_query(&outcome),
        None => Owner::Unknown("adb could not be started".to_string()),
    }
}

/// Reports `ready` and `run.ready`; returns how it got ready ("first frame
/// 411x914@2.625 after 0.9 s (source: icm_event)").
fn report_ready(ctx: &Ctx, project: &Project, ready: &Ready, launched: Instant) -> String {
    let window = ready.window.clone().unwrap_or(Value::Null);
    // Logical sizes are fractional on Android (1080 px / 2.625); one
    // decimal is plenty for a person.
    let number = |v: f64| {
        if (v - v.round()).abs() < 0.05 {
            format!("{}", v.round())
        } else {
            format!("{v:.1}")
        }
    };
    let size = window.get("size").and_then(Value::as_array).map(|size| {
        size.iter()
            .filter_map(Value::as_f64)
            .map(number)
            .collect::<Vec<_>>()
            .join("x")
    });
    let scale = window.get("scale").and_then(Value::as_f64);
    let ms = ready
        .ms
        .unwrap_or_else(|| launched.elapsed().as_millis() as u64);
    ctx.rep.ready(json!({
        "session": crate::paths::display(&session::path(project)),
        "source": ready.source,
        "ms_since_launch": launched.elapsed().as_millis() as u64,
        "window": window,
    }));
    let what = match (size, scale) {
        (Some(size), Some(scale)) => format!("first frame {size}@{scale}"),
        _ => "the app is up".to_string(),
    };
    let detail = format!(
        "{what} after {} (source: {})",
        crate::time::format_duration(Duration::from_millis(ms)),
        ready.source
    );
    ctx.rep
        .check(Check::pass(CheckId::RunReady, detail.clone()));
    detail
}

// ---- screenshots ---------------------------------------------------------------------

/// A screenshot on disk and what [`image::write_preview`] found in it.
pub(super) struct Grabbed {
    /// `<stem>.png`.
    pub png: PathBuf,
    /// `<stem>.preview.png`.
    pub preview: PathBuf,
    /// Size, preview size, blank detection.
    pub stats: image::Stats,
    /// The PNG's size in bytes.
    pub bytes: usize,
}

/// `adb exec-out screencap -p` into `<dir>/<stem>.png` and its preview,
/// without reporting anything.
pub(super) fn grab(ctx: &Ctx, adb: &Adb, dir: &Path, stem: &str) -> Result<Grabbed> {
    std::fs::create_dir_all(dir).map_err(|e| internal("cannot create the run directory", e))?;
    let cmd = adb
        .cmd(["exec-out", "screencap", "-p"])
        .timeout(Duration::from_secs(60));
    let outcome = ctx.step("adb.screencap", &cmd)?;
    if !outcome.success() || image::png_size(&outcome.stdout).is_none() {
        let mut error = ctx.step_failure("adb.screencap", CheckId::ToolFailed, &outcome);
        if outcome.success() {
            error.detail = format!(
                "screencap returned {} bytes that are not a PNG",
                outcome.stdout.len()
            );
        }
        return Err(error);
    }
    let png = dir.join(format!("{stem}.png"));
    let preview = dir.join(format!("{stem}.preview.png"));
    std::fs::write(&png, &outcome.stdout)
        .map_err(|e| internal("cannot write the screenshot", e))?;
    let stats = image::write_preview(&outcome.stdout, &preview)
        .map_err(|e| internal("cannot write the preview", e))?;
    Ok(Grabbed {
        png,
        preview,
        stats,
        bytes: outcome.stdout.len(),
    })
}

/// Whether the app's focused window has FLAG_SECURE (its screenshots are
/// black).
pub(super) fn window_is_secure(adb: &Adb, app_id: &str) -> bool {
    adb.shell_text("dumpsys window windows", Duration::from_secs(20))
        .is_some_and(|text| adb::focused_window_is_secure(&text, app_id))
}

/// Captures `<stem>.png` and `<stem>.preview.png` in `dir`, reports them,
/// the `screen` geometry and blank detection (`run.screen_blank`, or INFO
/// `android.screen.secure` for a FLAG_SECURE window).
fn capture(
    ctx: &Ctx,
    adb: &Adb,
    dir: &Path,
    stem: &str,
    app_id: &str,
    expect_content: bool,
) -> Result<Screen> {
    let Grabbed {
        png: png_path,
        preview: preview_path,
        stats,
        bytes,
    } = grab(ctx, adb, dir, stem)?;
    let outcome_len = bytes;

    let scale = adb
        .shell_text("wm density", Duration::from_secs(15))
        .and_then(|text| adb::parse_density(&text))
        .unwrap_or(1.0);
    let mut screen = Screen::new(stats.px, scale);
    screen.preview = stats.preview;

    let mut extra = serde_json::Map::new();
    let _ = extra.insert("bytes".into(), json!(outcome_len));
    let _ = extra.insert("blank".into(), json!(stats.blank));
    ctx.rep.artifact_with("screenshot", &png_path, extra);
    ctx.rep.artifact("preview", &preview_path);
    ctx.rep.set("screen", screen.to_json());

    if stats.blank {
        if window_is_secure(adb, app_id) {
            ctx.rep.check(Check::info(
                CheckId::AndroidScreenSecure,
                format!("{app_id}'s window has FLAG_SECURE, so the screenshot is black"),
            ));
        } else {
            let detail = format!(
                "{:.1}% of pixels are {}",
                stats.dominant_share * 100.0,
                stats.dominant
            );
            let status = if expect_content {
                Status::Fail
            } else {
                Status::Warn
            };
            ctx.rep.check(
                Check::new(CheckId::RunScreenBlank, status, detail)
                    .evidence(Evidence::file(&png_path))
                    .fix(
                        "Compare with a headless render; read the logs.",
                        &[
                            "icm shot --headless --json -q",
                            "icm logs android --level warn --json",
                        ],
                    ),
            );
        }
    }
    Ok(screen)
}

/// `icm shot android`: a screenshot of the device the session runs on.
pub fn shot(ctx: &mut Ctx, args: &ShotArgs) -> Result<()> {
    if ctx.dry_run() {
        return super::plan::shot(ctx, args);
    }
    let (project, host, tools) = setup(ctx)?;
    let ctx: &Ctx = ctx;
    let (adb, mut session) = session_device(ctx, &project, &host, &tools)?;
    let dir = run_dir(ctx, &project);
    let stem = args.name.clone().unwrap_or_else(|| "screen".to_string());
    if stem.contains('/') || stem.is_empty() {
        return Err(IcmError::new(
            CheckId::UsageBadArgs,
            format!("--name `{stem}` must be a plain file name"),
        ));
    }
    let app_id = project.config.config.app.id.clone();
    let screen = capture(ctx, &adb, &dir, &stem, &app_id, false)?;
    let presence = presence(&adb, &app_id);
    ctx.rep.set("process", presence.to_json());
    let gone = match presence {
        Presence::Gone => Some(format!("{app_id} is not running on {}", adb.serial)),
        Presence::NoActivity { pid } => Some(no_activity(&app_id, &adb.serial, pid)),
        Presence::Behind { .. } | Presence::Front { .. } => None,
    };
    if let Some(gone) = gone {
        ctx.rep.check(
            Check::warn(
                CheckId::RunAppDied,
                format!(
                    "{gone}; the screenshot shows whatever is on screen instead (the launcher)"
                ),
            )
            .fix(
                "Start the app, then take the screenshot again.",
                &["icm run android --json -q"],
            ),
        );
    }
    if let Some(out) = &args.out {
        if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| internal("cannot create --out's directory", e))?;
        }
        std::fs::copy(dir.join(format!("{stem}.png")), out)
            .map_err(|e| internal("cannot write --out", e))?;
        ctx.rep.artifact("out", out);
    }
    if let Some(session) = session.as_mut() {
        session.screen = Some(Geometry::from(&screen));
        let _ = session::write(&project, session);
    }
    ctx.rep.set("device", json!({"serial": adb.serial}));
    ctx.rep.next(
        "icm input android tap <x> <y> --json -q",
        "act in screen.preview.png pixels",
    );
    Ok(())
}

/// The session's device when it is online, else one chosen without
/// booting.
fn session_device(
    ctx: &Ctx,
    project: &Project,
    host: &HostConfig,
    tools: &Toolset,
) -> Result<(Adb, Option<Session>)> {
    let session = session::read(project);
    let listed = adb::devices(tools)?;
    if let Some(session) = &session
        && listed
            .iter()
            .any(|device| device.serial == session.serial && device.online())
    {
        return Ok((Adb::new(tools, &session.serial)?, Some(session.clone())));
    }
    let request = device::Request {
        target_sdk: project.config.config.android.target_sdk,
        ..device::Request::default()
    };
    let chosen: Chosen = device::choose(
        ctx,
        tools,
        host,
        &ctx.env,
        &request,
        false,
        Path::new("/dev/null"),
    )?;
    Ok((Adb::new(tools, &chosen.serial)?, None))
}

// ---- logs ----------------------------------------------------------------------

/// What [`collect_logs`] found.
#[derive(Clone, Debug, Default)]
pub(super) struct Collected {
    records: Vec<Record>,
    selected: Vec<(String, Record)>,
    logs: Option<PathBuf>,
    /// The app's processes the records were selected with.
    processes: logcat::Processes,
}

pub(super) fn query(adb: &Adb, mark: &str, buffers: &[&str]) -> Option<String> {
    let mut args: Vec<String> = vec!["logcat".into(), "-d".into()];
    for buffer in buffers {
        args.push("-b".into());
        args.push((*buffer).to_string());
    }
    args.extend([
        "-v".into(),
        "threadtime,epoch".into(),
        "-T".into(),
        mark.to_string(),
    ]);
    // Once in a while a dump fails while the next one, a moment later,
    // does not (seen right after a launch); the logs are a failed run's
    // evidence, so try twice.
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(500));
        }
        if let Some(outcome) = adb::quick(adb.cmd(&args), Duration::from_secs(60))
            && outcome.success()
        {
            return Some(outcome.stdout_text());
        }
    }
    None
}

/// Writes `logcat.txt` (raw), `logs.ndjson` and `app.log` (the records of
/// the app's processes: `pids`, and those [`processes`] finds) into the run
/// directory, with the secret values icm knows redacted as on stdout.
pub(super) fn collect_logs(
    ctx: &Ctx,
    adb: &Adb,
    dir: &Path,
    app_id: &str,
    mark: &str,
    pids: &BTreeSet<u32>,
) -> Collected {
    let Some(text) = query(adb, mark, &["main", "system", "crash"]) else {
        return Collected::default();
    };
    let raw = dir.join("logcat.txt");
    let _ = crate::process::write_redacted(&raw, &text);
    let records = logcat::parse(&text);
    let processes = processes(adb, app_id, pids);
    let selected: Vec<(String, Record)> = logcat::select(&records, app_id, &processes)
        .into_iter()
        .map(|(source, record)| (source.to_string(), record.clone()))
        .collect();
    let (logs, app_log) = write_records(dir, &selected);
    if let Some(path) = &logs {
        ctx.rep.artifact("logs", path);
    }
    if let Some(path) = &app_log {
        ctx.rep.artifact("app_log", path);
    }
    ctx.rep.artifact("logcat", &raw);
    Collected {
        records,
        selected,
        logs,
        processes,
    }
}

/// Writes `logs.ndjson` and `app.log`, redacted.
fn write_records(dir: &Path, selected: &[(String, Record)]) -> (Option<PathBuf>, Option<PathBuf>) {
    // Each record's strings are redacted before JSON escapes them.
    let ndjson: String = selected
        .iter()
        .map(|(source, record)| {
            let mut json = record.to_json(source);
            crate::output::redact(&mut json);
            format!("{json}\n")
        })
        .collect();
    let readable: String = selected
        .iter()
        .map(|(_, record)| format!("{}\n", record.line()))
        .collect();
    let logs = dir.join("logs.ndjson");
    let app_log = dir.join("app.log");
    (
        crate::process::write_redacted(&logs, &ndjson)
            .ok()
            .map(|()| logs),
        crate::process::write_redacted(&app_log, &readable)
            .ok()
            .map(|()| app_log),
    )
}

/// What a relaunch of the app's activity means for the run.
#[derive(Clone, Debug)]
struct Recreated {
    /// The likely cause, for a failed run.
    cause: String,
    /// What to do about it.
    fix: String,
    /// The commands that do it.
    commands: Vec<String>,
    /// The relaunch in `events.txt`.
    evidence: Option<Evidence>,
}

/// Whether the app started over after Android relaunched its activity.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Restart {
    /// The new activity sent its own `ICM_EVENT start`.
    Seen,
    /// No `start` followed the last relaunch within [`RELAUNCH_GRACE`] (or
    /// the app sends none at all): the app did not start over.
    Missing,
    /// As `Missing`, and the lock's winit is not the one iced brings
    /// ([`crate::deps::winit_outside_iced`] names it): a framework from
    /// before the Android lifecycle fix, which freezes on a relaunch.
    OldFramework(String),
}

/// `run.activity_recreated`: Android relaunched the app's activity since
/// the launch mark. A WARN when the app started over in the new activity
/// (it lost what it kept in memory); a FAIL when it did not, as a
/// framework from before the Android lifecycle fix freezes there. Writes
/// the events buffer since the mark to `events.txt` (design §10.4 step 12)
/// and reports the first relaunch with the configuration changes that
/// caused it ([`describe_recreation`]).
fn recreation(
    ctx: &Ctx,
    adb: &Adb,
    dir: &Path,
    project: &Project,
    mark: &str,
    pids: &BTreeSet<u32>,
) -> Option<Recreated> {
    let text = events_buffer(adb, mark, &[])?;
    let path = dir.join("events.txt");
    if crate::process::write_redacted(&path, &text).is_ok() {
        ctx.rep.artifact("events", &path);
    }
    let config = &project.config.config;
    let found = logcat::relaunches(&logcat::parse(&text), &config.app.id);
    let first = found.first()?;
    let last = found.last()?;
    let old_winit = project
        .lock()
        .ok()
        .flatten()
        .and_then(|lock| crate::deps::winit_outside_iced(&lock));
    // A framework from before the fix sends no `start` after a relaunch:
    // there is nothing to wait for.
    let restart = if restarted(
        adb,
        mark,
        last.record.seconds(),
        &config.app.id,
        pids,
        old_winit.is_none(),
    ) {
        Restart::Seen
    } else {
        old_winit.map_or(Restart::Missing, Restart::OldFramework)
    };
    let (detail, recreated) = describe_recreation(
        &found,
        mark.parse().unwrap_or(0.0),
        config.android.target_sdk,
        &restart,
    )?;
    let line = text
        .lines()
        .position(|line| line.contains(&first.record.ts) && line.contains(&first.record.tag))
        .map_or(1, |index| index as u32 + 1);
    let evidence = Evidence::line(&path, line, first.record.line());
    let commands: Vec<&str> = recreated.commands.iter().map(String::as_str).collect();
    let status = if restart == Restart::Seen {
        Status::Warn
    } else {
        Status::Fail
    };
    ctx.rep.check(
        Check::new(CheckId::RunActivityRecreated, status, detail)
            .evidence(evidence.clone())
            .fix(recreated.fix.clone(), &commands),
    );
    Some(Recreated {
        evidence: Some(evidence),
        ..recreated
    })
}

/// Whether the app sent an `ICM_EVENT start` after `after` (epoch seconds:
/// the last relaunch), from one of its processes ([`processes`], knowing
/// `pids`). With `wait`, waits for it until [`RELAUNCH_GRACE`] after the
/// relaunch, unless the app sent no `start` at all since the mark (it
/// speaks no `ICM_EVENT`).
fn restarted(
    adb: &Adb,
    mark: &str,
    after: f64,
    app_id: &str,
    pids: &BTreeSet<u32>,
    wait: bool,
) -> bool {
    loop {
        let processes = processes(adb, app_id, pids);
        let starts: Vec<f64> = events_since(adb, mark)
            .iter()
            .filter(|record| {
                processes.owns(record)
                    && logcat::event(record).is_some_and(|event| logcat::kind(&event) == "start")
            })
            .map(Record::seconds)
            .collect();
        if starts.iter().any(|&at| at > after) {
            return true;
        }
        if !wait || starts.is_empty() || crate::signals::pending().is_some() {
            return false;
        }
        let Some(now) = adb.epoch().and_then(|epoch| epoch.parse::<f64>().ok()) else {
            return false;
        };
        if now - after >= RELAUNCH_GRACE.as_secs_f64() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// The `run.activity_recreated` detail and what it means, for the
/// relaunches found since `mark` (epoch seconds), naming the changes as
/// `android:configChanges` does and comparing them with the manifest this
/// icm generates for `target_sdk`.
fn describe_recreation(
    found: &[logcat::Relaunch],
    mark: f64,
    target_sdk: u32,
    restart: &Restart,
) -> Option<(String, Recreated)> {
    let first = found.first()?;
    let after = crate::time::format_duration(Duration::from_secs_f64(
        (first.record.seconds() - mark).max(0.0),
    ));
    let generated = super::manifest::config_changes(target_sdk);
    let generated: Vec<&str> = generated.split('|').collect();
    let names = first
        .mask
        .map(super::manifest::config_names)
        .unwrap_or_default();
    let unlisted: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|name| !generated.contains(name))
        .collect();

    let (why, fix) = match unlisted.as_slice() {
        [] if names.is_empty() => (
            "the event log does not name the change".to_string(),
            "Rerun; if the relaunch repeats, report it with events.txt.",
        ),
        [] => (
            format!(
                "the installed APK's android:configChanges lacks {}, which the manifest this icm generates lists, so an earlier icm built it (or `--no-build` reused it)",
                names.join("|")
            ),
            "Rebuild and reinstall the APK: run without --no-build.",
        ),
        ["assetsPaths"] => (
            format!(
                "a runtime resource overlay changed (assetsPaths; SystemUI applies its theme palette during an emulator's first boots), and android:configChanges can name assetsPaths only from [android] target_sdk = 36 (this app has {target_sdk})"
            ),
            "Raise [android] target_sdk to 36 in icm.toml, or rerun once the emulator has settled (its overlays change only on its first boots).",
        ),
        unlisted => (
            format!(
                "a configuration change android:configChanges does not list for target_sdk {target_sdk} ({})",
                unlisted.join("|")
            ),
            "Rerun; if the relaunch repeats, report it with events.txt (icm's manifest should list the change).",
        ),
    };

    let mut detail = format!(
        "Android relaunched {} {after} after launch ({})",
        first.component,
        if names.is_empty() {
            "no change named".to_string()
        } else {
            names.join("|")
        }
    );
    if found.len() > 1 {
        detail.push_str(&format!(", {} relaunches", found.len()));
    }
    let grace = crate::time::format_duration(RELAUNCH_GRACE);
    let (outcome, consequence, fix) = match restart {
        Restart::Seen => (
            "The app ended with its activity and started over in the new one, losing what it kept in memory".to_string(),
            "which ends the app and starts it over in the new activity".to_string(),
            fix.to_string(),
        ),
        Restart::Missing => (
            format!(
                "No ICM_EVENT start followed within {grace}, so the app did not start over in the new activity: it stopped drawing and answering input (a framework from before the Android lifecycle fix freezes there)"
            ),
            "and the app did not start over in the new activity (it stopped drawing and answering input)".to_string(),
            format!(
                "{fix} Then read the app's logs for why it did not start again; an iced_mobile pin from before the Android lifecycle fix freezes on every relaunch, so update it."
            ),
        ),
        Restart::OldFramework(winit) => (
            format!(
                "The app's framework is from before the Android lifecycle fix (its Cargo.lock has {winit}, not the winit iced brings), and it freezes when its activity is recreated: it stops drawing and answering input"
            ),
            format!(
                "and an iced app on a framework from before the Android lifecycle fix ({winit} in Cargo.lock) freezes when its activity is recreated (it stops drawing and answering input)"
            ),
            format!(
                "{fix} Update the app's iced_mobile pin to one with the Android lifecycle fix (winit from iced's own source); until then never let Android recreate the activity."
            ),
        ),
    };
    detail.push_str(&format!(": {why}. {outcome}"));
    let recreated = Recreated {
        cause: format!(
            "Android relaunched the activity {after} after launch, {consequence}: {why} (run.activity_recreated)"
        ),
        fix,
        commands: vec!["icm run android --json -q".to_string()],
        evidence: None,
    };
    Some((detail, recreated))
}

/// Adds the logs, a panic's location and the failure signatures to a run
/// failure.
pub(super) fn attach_evidence(error: &mut IcmError, collected: &Collected, project: &Project) {
    let app_records: Vec<Record> = collected
        .selected
        .iter()
        .map(|(_, record)| record.clone())
        .collect();

    let panic = logcat::panic_of(&collected.records, &collected.processes);
    let app_id = &project.config.config.app.id;
    if error.id == CheckId::RunNotReady.id()
        && let Some(anr) = app_records
            .iter()
            .find(|record| record.msg.starts_with(&format!("ANR in {app_id}")))
    {
        let mut anr_error = IcmError::new(
            CheckId::RunAnr,
            format!("{}: {}", error.detail, anr.msg.trim()),
        );
        anr_error.evidence = std::mem::take(&mut error.evidence);
        *error = anr_error;
    }
    if error.id == CheckId::RunAppDied.id()
        && let Some((message, location)) = &panic
    {
        let mut panicked = IcmError::new(
            CheckId::RunAppPanicked,
            match location {
                Some(location) if !message.contains(location.as_str()) => {
                    format!("panicked at {location}: {message}")
                }
                _ => message.clone(),
            },
        );
        panicked.evidence = std::mem::take(&mut error.evidence);
        *error = panicked;
    }

    if let Some(logs) = &collected.logs {
        // The panic line when there is one (what follows it is the abort),
        // else the last error.
        let panic_line = app_records.iter().find(|record| {
            record.msg.contains("panicked at")
                || logcat::event(record).is_some_and(|event| logcat::kind(&event) == "panic")
        });
        let excerpt = panic_line
            .or_else(|| {
                app_records
                    .iter()
                    .rev()
                    .find(|record| record.priority == 'E' || record.priority == 'F')
            })
            .or(app_records.last())
            .map(Record::line)
            .unwrap_or_default();
        error
            .evidence
            .push(Evidence::file(logs).with_excerpt(excerpt));
    }
    let lib = project.lib_name().unwrap_or_default();
    for cause in logcat::likely_causes(&app_records, &lib) {
        // A panic aborts the process: the SIGABRT is its consequence.
        if panic.is_some() && cause.contains("Fatal signal 6") {
            continue;
        }
        error.likely_causes.push(cause);
    }
    if error.fix.commands.is_empty() {
        error
            .fix
            .commands
            .push("icm logs android --level warn --json".to_string());
    }
}

fn source_matches(filter: LogSource, source: &str) -> bool {
    match filter {
        LogSource::All => true,
        LogSource::App => source == "app",
        LogSource::System => source == "system",
        LogSource::Crash => source == "crash",
    }
}

/// `icm logs android`: re-queries logcat from the launch mark (or
/// `--since <dur>` ago) on the session's device.
pub fn logs(ctx: &mut Ctx, args: &LogsArgs) -> Result<()> {
    if ctx.dry_run() {
        return super::plan::logs(ctx, args);
    }
    let (project, host, tools) = setup(ctx)?;
    let ctx: &Ctx = ctx;
    let (adb, session) = session_device(ctx, &project, &host, &tools)?;
    let app_id = project.config.config.app.id.clone();

    let mark = if args.since == "launch" {
        session
            .as_ref()
            .filter(|session| session.serial == adb.serial)
            .and_then(|session| session.log_mark.clone())
            .ok_or_else(|| {
                IcmError::new(
                    CheckId::RunNoSession,
                    format!(
                        "no `icm run android` session on {}, so there is no launch mark",
                        adb.serial
                    ),
                )
                .fix_commands([
                    "icm run android --json -q",
                    "icm logs android --since 10m --json",
                ])
            })?
    } else {
        let ago = crate::time::parse_duration(&args.since).map_err(|error| {
            IcmError::new(
                CheckId::UsageBadArgs,
                format!(
                    "--since {}: {error} (use `launch` or a duration like 10m)",
                    args.since
                ),
            )
        })?;
        let now: f64 = adb
            .epoch()
            .and_then(|epoch| epoch.parse().ok())
            .ok_or_else(|| {
                IcmError::new(
                    CheckId::AndroidDeviceNone,
                    format!("{} does not answer `date`", adb.serial),
                )
            })?;
        format!("{:.3}", (now - ago.as_secs_f64()).max(0.0))
    };
    ctx.rep.set("device", json!({"serial": adb.serial}));
    ctx.rep.set("since", json!(mark));

    // The session's pid, when the session ran on this device.
    let pids: BTreeSet<u32> = session
        .as_ref()
        .filter(|session| session.serial == adb.serial)
        .and_then(|session| session.pid)
        .into_iter()
        .collect();

    let dir = run_dir(ctx, &project);
    let text = query(&adb, &mark, &["main", "system", "crash"]).ok_or_else(|| {
        IcmError::new(
            CheckId::ToolFailed,
            format!("adb logcat failed on {}", adb.serial),
        )
    })?;
    let raw = dir.join("logcat.txt");
    let _ = std::fs::create_dir_all(&dir);
    let _ = crate::process::write_redacted(&raw, &text);
    ctx.rep.artifact("logcat", &raw);

    if args.raw {
        ctx.rep.content(&text);
        ctx.rep.summary(format!(
            "{} lines of logcat since {mark}",
            text.lines().count()
        ));
        return Ok(());
    }

    let records = logcat::parse(&text);
    let processes = processes(&adb, &app_id, &pids);
    let grep = crate::grep::Grep::new(args.grep.as_deref());
    let wanted = |source: &str, record: &Record| {
        source_matches(args.source.unwrap_or(LogSource::All), source)
            && logcat::at_least(record, args.level)
            && grep
                .as_ref()
                .is_none_or(|grep| grep.matches(&[&record.tag, &record.msg]))
    };
    let app_records = logcat::select(&records, &app_id, &processes);
    let considered = app_records.len();
    let mut selected: Vec<(String, Record)> = app_records
        .into_iter()
        .filter(|(source, record)| wanted(source, record))
        .map(|(source, record)| (source.to_string(), record.clone()))
        .collect();
    let total = selected.len();
    crate::grep::warn_unmatched(ctx, args.grep.as_deref(), total, considered);
    if selected.len() > args.tail {
        selected.drain(..selected.len() - args.tail);
    }
    let (logs, app_log) = write_records(&dir, &selected);
    if let Some(path) = logs {
        ctx.rep.artifact("logs", &path);
    }
    if let Some(path) = app_log {
        ctx.rep.artifact("app_log", &path);
    }
    ctx.rep.set(
        "records",
        Value::Array(
            selected
                .iter()
                .map(|(source, record)| record.to_json(source))
                .collect(),
        ),
    );
    let text: String = selected
        .iter()
        .map(|(_, record)| format!("{}\n", record.line()))
        .collect();
    if !text.is_empty() {
        ctx.rep.content(text);
    }
    ctx.rep.summary(format!(
        "{} of {total} record(s) from {} since {mark}",
        selected.len(),
        adb.serial
    ));

    if args.follow {
        let _ = follow(ctx, &adb, &app_id, &mark, pids, false, &wanted)?;
    }
    Ok(())
}

/// How [`follow`] ended.
enum Followed {
    /// Ctrl-C or `--timeout`.
    Ended,
    /// The app's process is gone (`until_exit`); `stopped` when it was
    /// `am force-stop` (`icm stop android`, or someone at the device).
    Gone { stopped: bool },
    /// The app ended with its activity, which Android destroyed (Back at
    /// its root), and no new activity started it again; the process lives
    /// on, cached (`until_exit`).
    Destroyed,
}

/// How long after an `ICM_EVENT exit` with `destroyed: true` [`follow`]
/// waits for a new activity's `start` before it takes the app as ended: a
/// relaunch (a configuration change the manifest does not list) starts it
/// again within a second.
const DESTROYED_GRACE: Duration = Duration::from_secs(3);

/// Streams the app's new records until the app exits (`until_exit`),
/// Ctrl-C or `--timeout`. In JSON mode each record is a `log` event.
fn follow(
    ctx: &Ctx,
    adb: &Adb,
    app_id: &str,
    mark: &str,
    mut pids: BTreeSet<u32>,
    until_exit: bool,
    wanted: &dyn Fn(&str, &Record) -> bool,
) -> Result<Followed> {
    type Key = (String, u32, u32, String);
    let key = |record: &Record| -> Key {
        (
            record.ts.clone(),
            record.pid,
            record.tid,
            record.msg.clone(),
        )
    };
    let mut since = mark.to_string();
    let mut seen: BTreeSet<Key> = BTreeSet::new();
    // Skip what was already shown.
    if let Some(text) = query(adb, &since, &["main", "system", "crash"]) {
        for record in logcat::parse(&text) {
            since = record.ts.clone();
            let _ = seen.insert(key(&record));
        }
    }
    let deadline = ctx.deadline();
    let force_stop = format!("Force stopping {app_id} ");
    let mut stopped = false;
    // When the app last ended with a destroyed activity, unless a new
    // activity has started it since.
    let mut destroyed: Option<Instant> = None;
    loop {
        if crate::signals::pending().is_some() || deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(Followed::Ended);
        }
        std::thread::sleep(Duration::from_millis(1000));
        let text = query(adb, &since, &["main", "system", "crash"]);
        // The processes after the records: every process that wrote one
        // has started by now, and is in `pidof` or the events buffer.
        let processes = processes(adb, app_id, &pids);
        let alive = processes.now.clone();
        pids.extend(alive.iter().copied());
        if let Some(text) = text {
            let records = logcat::parse(&text);
            // `-T <since>` repeats the records at `since`; older keys are
            // no longer needed.
            let floor: f64 = since.parse().unwrap_or(0.0);
            seen.retain(|(ts, ..)| ts.parse::<f64>().unwrap_or(0.0) >= floor);
            // ActivityManager logs every `am force-stop`.
            stopped |= records
                .iter()
                .any(|record| record.msg.starts_with(&force_stop));
            for (source, record) in logcat::select(&records, app_id, &processes) {
                if !seen.insert(key(record)) {
                    continue;
                }
                if let Some(event) = logcat::event(record).filter(|_| source == "app") {
                    match logcat::kind(&event) {
                        "exit" if event.get("destroyed").and_then(Value::as_bool) == Some(true) => {
                            destroyed = Some(Instant::now());
                        }
                        "start" => destroyed = None,
                        _ => {}
                    }
                }
                if !wanted(source, record) {
                    continue;
                }
                let mut event = record.to_json(source);
                event["type"] = json!("log");
                if ctx.rep.mode().json {
                    ctx.rep.emit(event);
                } else {
                    ctx.rep.content(record.line());
                }
            }
            if let Some(last) = records.last() {
                since = last.ts.clone();
            }
        }
        if until_exit && alive.is_empty() {
            ctx.rep.progress(format!("{app_id} exited"));
            return Ok(Followed::Gone { stopped });
        }
        // The process outlives a destroyed activity, so its pid says
        // nothing then: the app has ended once no activity of it is left.
        if until_exit && destroyed.is_some_and(|at| at.elapsed() >= DESTROYED_GRACE) {
            if matches!(presence(adb, app_id), Presence::NoActivity { .. }) {
                ctx.rep
                    .progress(format!("{app_id} ended with its activity"));
                return Ok(Followed::Destroyed);
            }
            destroyed = None;
        }
    }
}

// ---- input ---------------------------------------------------------------------

/// The geometry input coordinates refer to: the last screenshot's (what
/// the agent looked at), else the device's current display.
fn geometry(adb: &Adb, session: Option<&Session>) -> Result<Screen> {
    if let Some(geometry) = session.and_then(|session| session.screen) {
        return Ok(geometry.screen());
    }
    let displays = adb
        .shell_text("dumpsys window displays", Duration::from_secs(20))
        .unwrap_or_default();
    let wm_size = adb
        .shell_text("wm size", Duration::from_secs(15))
        .unwrap_or_default();
    let px = adb::parse_display_size(&displays, &wm_size).ok_or_else(|| {
        IcmError::new(
            CheckId::ToolFailed,
            format!("cannot read {}'s display size", adb.serial),
        )
    })?;
    let scale = adb
        .shell_text("wm density", Duration::from_secs(15))
        .and_then(|text| adb::parse_density(&text))
        .unwrap_or(1.0);
    let mut screen = Screen::new(px, scale);
    screen.preview = preview_size(px);
    Ok(screen)
}

/// WARN `android.orientation_locked`: the app is locked to `locked` by
/// `orientations`, so a rotation to the other axis does not turn it on a
/// phone. `what` says what happened instead ("… <axis>").
pub(super) fn orientation_locked(
    app_id: &str,
    orientations: &[crate::config::Orientation],
    locked: Axis,
    what: &str,
) -> Check {
    let names: Vec<String> = orientations
        .iter()
        .filter_map(|orientation| serde_json::to_value(orientation).ok())
        .filter_map(|value| value.as_str().map(|name| format!("\"{name}\"")))
        .collect();
    Check::warn(
        CheckId::AndroidOrientationLocked,
        format!(
            "{app_id} is locked to {} ([app] orientations = [{}]): {what} {} (large screens from API 36 ignore the lock)",
            locked.name(),
            names.join(", "),
            locked.name()
        ),
    )
}

fn keycode(key: Key) -> &'static str {
    match key {
        Key::Back => "KEYCODE_BACK",
        Key::Home => "KEYCODE_HOME",
        Key::Enter => "KEYCODE_ENTER",
        Key::Tab => "KEYCODE_TAB",
        Key::Escape => "KEYCODE_ESCAPE",
    }
}

fn space_name(space: Space) -> &'static str {
    match space {
        Space::Preview => "preview",
        Space::Px => "px",
        Space::Pt => "pt",
    }
}

/// `icm input android …`: taps and swipes in one coordinate space
/// (preview pixels by default, Appendix C item 25), text, keys and the
/// device-state helpers.
pub fn input(ctx: &mut Ctx, args: &InputArgs) -> Result<()> {
    if ctx.dry_run() {
        return super::plan::input(ctx, args);
    }
    let (project, host, tools) = setup(ctx)?;
    let ctx: &Ctx = ctx;
    let (adb, session) = session_device(ctx, &project, &host, &tools)?;
    let app_id = project.config.config.app.id.clone();
    ctx.rep.set("device", json!({"serial": adb.serial}));

    // Touches and keys go to whatever is on screen: without the app they
    // would drive the launcher. That includes a process that outlived its
    // activity (Back at the app's root), which has no window to send to.
    if matches!(
        args.action,
        InputAction::Tap { .. }
            | InputAction::Swipe { .. }
            | InputAction::Text { .. }
            | InputAction::Key { .. }
    ) {
        let refused = match presence(&adb, &app_id) {
            Presence::Gone if session.is_some() => Some((
                CheckId::RunAppDied,
                format!(
                    "{app_id} is not running on {} (it exited or crashed); nothing was sent",
                    adb.serial
                ),
            )),
            Presence::Gone => Some((
                CheckId::RunNoSession,
                format!(
                    "{app_id} is not running on {}; nothing was sent",
                    adb.serial
                ),
            )),
            Presence::NoActivity { pid } => Some((
                CheckId::RunAppDied,
                format!(
                    "{}; nothing was sent",
                    no_activity(&app_id, &adb.serial, pid)
                ),
            )),
            Presence::Behind { .. } | Presence::Front { .. } => None,
        };
        if let Some((id, detail)) = refused {
            return Err(IcmError::new(id, detail).fix(
                "Start the app (and read why it stopped), then send the input again.",
                &[
                    "icm logs android --level warn --json -q",
                    "icm run android --json -q",
                ],
            ));
        }
    }

    let point = |screen: &Screen, x: f64, y: f64| -> Result<(i64, i64)> {
        if !screen.contains(x, y, args.space) {
            let (w, h) = match args.space {
                Space::Preview => (f64::from(screen.preview.0), f64::from(screen.preview.1)),
                Space::Px => (f64::from(screen.px.0), f64::from(screen.px.1)),
                Space::Pt => screen.pt(),
            };
            return Err(IcmError::new(
                CheckId::UsageBadArgs,
                format!(
                    "({x}, {y}) is outside the screen, which is {w}x{h} in {} space",
                    space_name(args.space)
                ),
            ));
        }
        let (px, py) = screen.to_px(x, y, args.space);
        Ok((px.round() as i64, py.round() as i64))
    };

    let (line, what): (String, Value) = match &args.action {
        InputAction::Tap { x, y } => {
            let screen = geometry(&adb, session.as_ref())?;
            let (px, py) = point(&screen, *x, *y)?;
            ctx.rep.set("screen", screen.to_json());
            (
                format!("input tap {px} {py}"),
                json!({"action": "tap", "space": space_name(args.space), "at": [x, y], "px": [px, py]}),
            )
        }
        InputAction::Swipe { x1, y1, x2, y2, ms } => {
            let screen = geometry(&adb, session.as_ref())?;
            let (ax, ay) = point(&screen, *x1, *y1)?;
            let (bx, by) = point(&screen, *x2, *y2)?;
            let ms = ms.unwrap_or(300);
            ctx.rep.set("screen", screen.to_json());
            (
                format!("input swipe {ax} {ay} {bx} {by} {ms}"),
                json!({"action": "swipe", "space": space_name(args.space), "from": [x1, y1], "to": [x2, y2], "px": [[ax, ay], [bx, by]], "ms": ms}),
            )
        }
        InputAction::Text { text } => {
            if !text.is_ascii() || text.chars().any(char::is_control) {
                return Err(IcmError::new(
                    CheckId::UsageBadArgs,
                    "adb can type printable ASCII only",
                ));
            }
            (
                format!("input text {}", adb::input_text_arg(text)),
                json!({"action": "text", "text": text}),
            )
        }
        InputAction::Key { key } => (
            format!("input keyevent {}", keycode(*key)),
            json!({"action": "key", "key": keycode(*key)}),
        ),
        InputAction::Appearance { mode } => {
            let night = if *mode == Theme::Dark { "yes" } else { "no" };
            (
                format!("cmd uimode night {night}"),
                json!({"action": "appearance", "night": night}),
            )
        }
        InputAction::Rotate { orientation } => {
            let rotation = if *orientation == Rotation::Landscape {
                1
            } else {
                0
            };
            let wanted = if rotation == 1 {
                Axis::Landscape
            } else {
                Axis::Portrait
            };
            let orientations = &project.config.config.app.orientations;
            if let Some(locked) = super::manifest::locked_axis(orientations)
                && locked != wanted
            {
                ctx.rep.check(orientation_locked(
                    &app_id,
                    orientations,
                    locked,
                    "the device turns, but Android keeps the app",
                ));
            }
            (
                format!(
                    "settings put system accelerometer_rotation 0 && settings put system user_rotation {rotation}"
                ),
                json!({"action": "rotate", "user_rotation": rotation}),
            )
        }
        InputAction::FontScale { scale } => {
            if !(0.5..=3.0).contains(scale) {
                return Err(IcmError::new(
                    CheckId::UsageBadArgs,
                    format!("font scale {scale} is outside 0.5 to 3.0"),
                ));
            }
            (
                format!("settings put system font_scale {scale}"),
                json!({"action": "font-scale", "scale": scale}),
            )
        }
        InputAction::Background => (
            "input keyevent KEYCODE_HOME".to_string(),
            json!({"action": "background"}),
        ),
        InputAction::Foreground => (
            format!(
                "am start -n {}",
                adb::quote(&format!("{app_id}/{ACTIVITY}"))
            ),
            json!({"action": "foreground"}),
        ),
    };

    let outcome = ctx.step(
        "adb.input",
        &adb.shell(&line).timeout(Duration::from_secs(30)),
    )?;
    let text = format!("{}{}", outcome.stdout_text(), outcome.stderr_text());
    if !outcome.success() || text.contains("Exception") || text.contains("Error:") {
        let mut error = ctx.step_failure("adb.input", CheckId::ToolFailed, &outcome);
        if outcome.success() {
            error.detail = format!("`{line}` failed: {}", text.trim());
        }
        return Err(error);
    }
    ctx.rep.set("input", what);
    ctx.rep.summary(format!("{line} on {}", adb.serial));
    ctx.rep.next("icm shot android --json -q", "see the result");
    Ok(())
}

// ---- stop ----------------------------------------------------------------------

/// `icm stop android [--shutdown]`: force-stops the app; with
/// `--shutdown`, also the emulator icm booted for this project, and a
/// running one of icm's own AVDs that no other project claims (never an
/// AVD outside icm's `icm-` names, never one icm booted for another
/// project).
pub fn stop(ctx: &mut Ctx, args: &StopArgs) -> Result<()> {
    if ctx.dry_run() {
        return super::plan::stop(ctx, args);
    }
    let (project, host, tools) = setup(ctx)?;
    let ctx: &Ctx = ctx;
    let Stopped {
        stopped,
        still_running,
    } = stop_session(ctx, &project, &host, &tools, args.shutdown)?;
    ctx.rep
        .summary(match (stopped.len(), still_running.is_empty()) {
            (0, true) => "nothing to stop on Android".to_string(),
            (n, true) => format!("stopped {n} on Android"),
            (0, false) => still_running_note(&still_running),
            (n, false) => format!(
                "stopped {n} on Android; {}",
                still_running_note(&still_running)
            ),
        });
    ctx.rep.set("stopped", Value::Array(stopped));
    ctx.rep.set("still_running", json!(still_running));
    Ok(())
}

/// What a stop did.
#[derive(Debug, Default)]
pub struct Stopped {
    /// What it stopped: the app (`platform`, `app`, `serial`) and each
    /// emulator it shut down (`platform`, `emulator`).
    pub stopped: Vec<Value>,
    /// The emulators `--shutdown` tried to shut down that are still
    /// running ([`avd::Shutdown::Lingers`]).
    pub still_running: Vec<String>,
}

/// The summary's words for the emulators a stop could not shut down.
pub fn still_running_note(serials: &[String]) -> String {
    format!("{} did not shut down (still running)", serials.join(", "))
}

/// Why `stop` does not act on the emulator on a serial.
#[derive(Debug, PartialEq, Eq)]
enum Leave {
    /// Its owner property names another project.
    Shared(String),
    /// Adb could not read its owner property (why).
    Unknown(String),
    /// Its owner property is unset, and nothing proves it is this project's.
    Unproven,
}

/// Whether `stop` may act on an emulator (force-stop the app, shut it
/// down), from its owner property as the device reads it and from what
/// proves it is this project's. A project's records live in its own
/// directory, so an emulator is another project's, or somebody's, whatever
/// they say: the owner is read from the device before every destructive
/// step, never skipped because a record looks live, and a failed read is
/// not an unset one. `proven` is that the emulator process icm started is
/// verified to run on the serial ([`session::Process::Verified`]), or that
/// it runs one of icm's managed AVDs, which are shut down unless another
/// project claimed them. Without it, only a device that names this
/// project (`own`) is acted on.
fn may_act(owner: &Owner, own: &str, proven: bool) -> std::result::Result<(), Leave> {
    match owner {
        Owner::Project(tag) if tag == own => Ok(()),
        Owner::Project(tag) => Err(Leave::Shared(tag.clone())),
        Owner::Unknown(why) => Err(Leave::Unknown(why.clone())),
        Owner::Nobody if proven => Ok(()),
        Owner::Nobody => Err(Leave::Unproven),
    }
}

/// What a record says of the emulator process it names, as part of a message
/// about an emulator that nothing else shows to be this project's. `None`:
/// no record names the emulator.
fn cannot_confirm(process: Option<&session::Process>) -> &'static str {
    match process {
        Some(session::Process::Gone) => "the recorded emulator process has ended or was replaced",
        Some(session::Process::Unverified) => {
            "the recorded emulator process cannot be confirmed (the record has no process identity, icm could not read it when it started the emulator, or the OS would not describe the process)"
        }
        _ => "icm has no record that it booted it",
    }
}

/// What `stop` has read from the devices: each serial's owner, once.
struct Owners<'a> {
    tools: &'a Toolset,
    read: std::cell::RefCell<std::collections::BTreeMap<String, Owner>>,
}

impl Owners<'_> {
    /// The project an icm booted the emulator for, as the device says.
    fn of(&self, serial: &str) -> Owner {
        self.read
            .borrow_mut()
            .entry(serial.to_string())
            .or_insert_with(|| match Adb::new(self.tools, serial) {
                Ok(adb) => emulator_owner(&adb),
                Err(error) => Owner::Unknown(error.detail),
            })
            .clone()
    }
}

/// The WARN for an emulator `stop` leaves running because adb could not
/// read which project booted it (`what` says what was left).
fn owner_unknown(ctx: &Ctx, serial: &str, why: &str, what: &str) {
    ctx.rep.check(
        Check::warn(
            CheckId::AndroidEmulatorOwnerUnknown,
            format!(
                "{serial} {what}: icm could not read which project booted it ({}, `adb shell getprop` {why}), and another may still use it",
                session::OWNER_PROP
            ),
        )
        .fix(
            "Rerun `icm stop android --shutdown` once the emulator answers; to shut down this emulator anyway, and stop any app on it:",
            &[&format!("adb -s {serial} emu kill")],
        ),
    );
}

/// The INFO for an emulator `stop` leaves running because another project
/// booted it (`what` says what was left). With `app`, the app that was not
/// force-stopped there, the fix stops that app by hand.
fn emulator_shared(ctx: &Ctx, serial: &str, owner: &str, what: &str, app: Option<&str>) {
    let check = Check::info(
        CheckId::AndroidEmulatorShared,
        format!(
            "{serial} {what}: icm booted it for another project ({} {owner}), which may still use it; `icm stop --shutdown` there shuts it down",
            session::OWNER_PROP
        ),
    );
    ctx.rep.check(match app {
        Some(app) => check.fix(
            "To stop the app without touching the emulator:",
            &[&format!("adb -s {serial} shell am force-stop {app}")],
        ),
        None => check,
    });
}

/// One emulator `--shutdown` may shut down.
struct Target {
    serial: String,
    /// The emulator process icm started, when it is verified to run: its
    /// pid and the identity that was recorded for it.
    process: Option<(u32, crate::procid::Identity)>,
    /// What the first record that names the emulator says of its process
    /// when no record's is the verified one (it ended or was replaced, or it
    /// cannot be told); for the message about an emulator left running.
    unconfirmed: Option<session::Process>,
}

/// Stops this project's app on its device (`am force-stop`) and, with
/// `shutdown`, the icm-managed emulators it or the project's default AVD
/// runs on; removes the session. Returns what it stopped, and the emulators
/// that ignored the shutdown and still run (for `icm stop --all` too).
///
/// A recorded pid is no proof of anything: once an emulator has exited,
/// another process can have its pid and another emulator its serial. A
/// record counts as the emulator on its serial only while its process is
/// verified against the identity icm recorded when it started it
/// ([`session::Process`]); an older record without one proves nothing, and
/// neither does one whose process has ended or was replaced. Such a record
/// still names a serial that is online, and icm then decides as the device's
/// owner property says, never skipping the emulator because of the record.
/// So whatever the records say, the emulator's owner property is read before
/// the app is force-stopped or the emulator shut down ([`may_act`]), and the
/// emulator's process is signalled only while it is that verified one.
pub fn stop_session(
    ctx: &Ctx,
    project: &Project,
    host: &HostConfig,
    tools: &Toolset,
    shutdown: bool,
) -> Result<Stopped> {
    let app_id = project.config.config.app.id.clone();
    let session = session::read(project);
    let listed = adb::devices(tools)?;
    let online = |serial: &str| {
        listed
            .iter()
            .any(|device| device.serial == serial && device.online())
    };
    let own = session::owner_tag(project);
    let owners = Owners {
        tools,
        read: Default::default(),
    };
    // The AVD each emulator runs now, which a session's record may no
    // longer name: asked of the emulators once, when a decision needs it.
    let running_now = std::cell::OnceCell::new();
    let avd_on = |serial: &str| {
        running_now
            .get_or_init(|| device::running_emulators(tools, &listed))
            .iter()
            .find(|(s, _)| s == serial)
            .and_then(|(_, name)| name.clone())
    };
    let managed_on =
        |serial: &str| avd_on(serial).is_some_and(|avd| crate::managed::is_managed(&avd));
    let mut stopped: Vec<Value> = Vec::new();
    let mut still_running: Vec<String> = Vec::new();
    // The serials already reported as left running for their owner.
    let mut told: BTreeSet<String> = BTreeSet::new();

    // The app is force-stopped on a device icm did not boot (the one the
    // run was told to use) as before. On an emulator icm booted, only while
    // that emulator is this project's, which the device's owner property
    // says: the record's process, verified to run, or one of icm's AVDs,
    // stands in for an unset property, and another project's tag or a
    // property that cannot be read leaves the app running. A record whose
    // process has ended or was replaced is no different from one with no
    // identity: the serial is online, and icm cannot tell from the record
    // whether it holds the emulator its run booted, so the owner decides.
    if let Some(session) = &session
        && online(&session.serial)
    {
        let proceed = if !session.booted_by_icm {
            true
        } else {
            let process = session.emulator_process();
            let proven =
                matches!(process, session::Process::Verified { .. }) || managed_on(&session.serial);
            match may_act(&owners.of(&session.serial), &own, proven) {
                Ok(()) => true,
                Err(Leave::Unknown(why)) => {
                    owner_unknown(
                        ctx,
                        &session.serial,
                        &why,
                        &format!("kept running with {app_id} on it"),
                    );
                    let _ = told.insert(session.serial.clone());
                    false
                }
                Err(Leave::Unproven) => {
                    ctx.rep.check(Check::info(
                        CheckId::RunNoSession,
                        format!(
                            "{app_id} was not force-stopped on {}: {}; the emulator runs no AVD icm manages and {} is unset, so nothing shows it is this project's",
                            session.serial,
                            cannot_confirm(Some(&process)),
                            session::OWNER_PROP
                        ),
                    ));
                    false
                }
                // Another project's: said once, here or by
                // `--shutdown` below, whichever meets it first.
                Err(Leave::Shared(owner)) => {
                    if told.insert(session.serial.clone()) {
                        emulator_shared(
                            ctx,
                            &session.serial,
                            &owner,
                            &format!("kept running with {app_id} on it"),
                            Some(&app_id),
                        );
                    }
                    false
                }
            }
        };
        if proceed {
            let adb = Adb::new(tools, &session.serial)?;
            let outcome = ctx.step(
                "adb.force_stop",
                &adb.shell(&format!("am force-stop {}", adb::quote(&app_id)))
                    .timeout(Duration::from_secs(30)),
            )?;
            if !outcome.success() {
                return Err(ctx.step_failure("adb.force_stop", CheckId::ToolFailed, &outcome));
            }
            stopped.push(json!({"platform": "android", "app": app_id, "serial": session.serial}));
        }
    }

    // `--shutdown` stops the emulators icm booted for this project, and
    // otherwise only icm's own AVDs: never an `icm-test-` one a test run
    // made and owns (crate::managed::is_managed), never anyone else's, and
    // never one icm booted for another project (session::OWNER_PROP).
    if shutdown {
        let running = running_now.get_or_init(|| device::running_emulators(tools, &listed));
        let mut targets: Vec<Target> = Vec::new();
        // A record's verdict on its emulator process, or `None` for an
        // emulator no record names (a managed AVD).
        let mut add = |serial: &str, record: Option<session::Process>| {
            let at = match targets.iter().position(|target| target.serial == serial) {
                Some(at) => at,
                None => {
                    targets.push(Target {
                        serial: serial.to_string(),
                        process: None,
                        unconfirmed: None,
                    });
                    targets.len() - 1
                }
            };
            let target = &mut targets[at];
            match record {
                Some(session::Process::Verified { pid, identity }) => {
                    target.process = target.process.take().or(Some((pid, identity)));
                }
                Some(unconfirmed) => {
                    target.unconfirmed = target.unconfirmed.take().or(Some(unconfirmed));
                }
                None => {}
            }
        };
        if let Some(session) = &session
            && online(&session.serial)
        {
            if session.booted_by_icm {
                // Recorded, whether or not the record proves it: the device
                // must say the emulator is ours, so a record whose process
                // ended or was replaced is no reason to skip it.
                add(&session.serial, Some(session.emulator_process()));
            } else if managed_on(&session.serial) {
                // Whose it is, the owner check below reads from the device.
                add(&session.serial, None);
            }
        }
        let default = device::default_avd(host, project.config.config.android.target_sdk);
        if crate::managed::is_managed(&default) {
            for (serial, name) in running {
                if name.as_deref() == Some(default.as_str()) {
                    add(serial, None);
                }
            }
        }
        // Every emulator icm booted for this project, even after a plain
        // `icm stop android` removed the session that named it.
        for booted in session::booted(project) {
            if online(&booted.serial) {
                add(&booted.serial, Some(booted.process()));
            }
        }
        if let Some(session) = &session
            && online(&session.serial)
            && session.kind == "emulator"
            && !session.booted_by_icm
            && !targets.iter().any(|target| target.serial == session.serial)
        {
            let avd = avd_on(&session.serial);
            ctx.rep.check(Check::info(
                CheckId::RunNoSession,
                format!(
                    "{} ({}) left running: icm did not boot it and shuts down only its own AVDs (icm-*, never icm-test-*)",
                    session.serial,
                    avd.as_deref().unwrap_or("unknown AVD")
                ),
            ));
        }
        for Target {
            serial,
            process,
            unconfirmed,
        } in targets
        {
            // The owner is read for every emulator, the ones a record says
            // icm booted too: a record can outlive its emulator, and
            // whoever runs on the serial now may be another project.
            let proven = process.is_some() || managed_on(&serial);
            match may_act(&owners.of(&serial), &own, proven) {
                Ok(()) => {}
                Err(Leave::Shared(owner)) => {
                    // Reported already when the app was left running.
                    if told.insert(serial.clone()) {
                        emulator_shared(ctx, &serial, &owner, "left running", None);
                    }
                    continue;
                }
                Err(Leave::Unknown(why)) => {
                    // Reported already when the app was left running.
                    if told.insert(serial.clone()) {
                        owner_unknown(ctx, &serial, &why, "left running");
                    }
                    continue;
                }
                Err(Leave::Unproven) => {
                    ctx.rep.check(Check::info(
                        CheckId::RunNoSession,
                        format!(
                            "{serial} ({}) left running: {}; it runs no AVD icm manages and {} is unset, so nothing shows icm booted it for this project",
                            avd_on(&serial).as_deref().unwrap_or("unknown AVD"),
                            cannot_confirm(unconfirmed.as_ref()),
                            session::OWNER_PROP
                        ),
                    ));
                    continue;
                }
            }
            ctx.rep.progress(format!("shutting down {serial}"));
            let ended = avd::shutdown(
                ctx,
                tools,
                &serial,
                process.as_ref().map(|(pid, identity)| (*pid, identity)),
            )?;
            match ended {
                avd::Shutdown::Done => {
                    session::remove_booted(project, &serial);
                    stopped.push(json!({"platform": "android", "emulator": serial}));
                }
                // It ignored `emu kill` (and SIGTERM, when icm may send
                // one): it is not stopped, and its record stays, so that
                // the next `--shutdown` can still verify its process.
                avd::Shutdown::Lingers => {
                    let why = match &process {
                        Some((pid, _)) => {
                            format!("`adb emu kill` and SIGTERM to its process (pid {pid})")
                        }
                        None => "`adb emu kill`, and icm has no verified process of it to signal"
                            .to_string(),
                    };
                    ctx.rep.check(
                        Check::warn(
                            CheckId::AndroidEmulatorShutdownFailed,
                            format!("{serial} is still running: it ignored {why}"),
                        )
                        .fix(
                            "Run it again, or quit the emulator yourself (its window, or the process that listens on its console port); `icm stop android --shutdown` tries again:",
                            &[&format!("adb -s {serial} emu kill")],
                        ),
                    );
                    still_running.push(serial);
                }
            }
        }
    }

    session::remove(project);
    Ok(Stopped {
        stopped,
        still_running,
    })
}

/// Whether this project's app runs on its session's device, for `icm ps`
/// (an Android record has no host process to probe).
pub fn app_state(ctx: &mut Ctx, project: &Project) -> crate::session::AppState {
    use crate::session::AppState;
    let Some(session) = session::read(project) else {
        return AppState::Unknown;
    };
    let Ok(host) = ctx.host().cloned() else {
        return AppState::Unknown;
    };
    let Ok(tools) = Toolset::discover(&host, &ctx.env) else {
        return AppState::Unknown;
    };
    let Ok(listed) = adb::devices(&tools) else {
        return AppState::Unknown;
    };
    if !listed
        .iter()
        .any(|device| device.serial == session.serial && device.online())
    {
        return AppState::Gone("the device is offline".to_string());
    }
    let Ok(adb) = Adb::new(&tools, &session.serial) else {
        return AppState::Unknown;
    };
    match presence(&adb, &session.app_id) {
        Presence::Gone => AppState::Gone("the app is not running".to_string()),
        Presence::NoActivity { .. } => AppState::Gone(
            "the app has no activity (Android destroyed it, and the app ended; its process lives on, cached)"
                .to_string(),
        ),
        Presence::Behind { .. } | Presence::Front { .. } => AppState::Running,
    }
}

// ---- devices -------------------------------------------------------------------

/// `icm devices android`: online devices and the AVDs.
pub fn devices(ctx: &mut Ctx) -> Result<()> {
    if ctx.dry_run() {
        return super::plan::devices(ctx);
    }
    let (lines, listing) = device_listing(ctx)?;
    for key in ["devices", "avds", "default_avd"] {
        ctx.rep.set(key, listing[key].clone());
    }
    ctx.rep.summary(format!(
        "android: {} device(s) online, {} AVD(s)",
        listing["devices"].as_array().map_or(0, |devices| devices
            .iter()
            .filter(|d| d["state"] == "device")
            .count()),
        listing["avds"].as_array().map_or(0, Vec::len)
    ));
    ctx.rep.content(lines);
    Ok(())
}

/// The devices adb lists and the AVDs, as text lines and as
/// `{devices, avds, default_avd}` (`icm devices [android]`).
pub fn device_listing(ctx: &mut Ctx) -> Result<(String, Value)> {
    let host = ctx.host()?.clone();
    let tools = Toolset::discover(&host, &ctx.env)?;
    let target_sdk = ctx
        .try_project()
        .map_or(36, |project| project.config.config.android.target_sdk);
    let ctx: &Ctx = ctx;
    let listed = adb::devices(&tools)?;
    let emulators = device::running_emulators(&tools, &listed);
    let mut lines = String::new();

    let devices: Vec<Value> = listed
        .iter()
        .map(|device| {
            let avd = emulators
                .iter()
                .find(|(serial, _)| *serial == device.serial)
                .and_then(|(_, avd)| avd.clone());
            let abi = if device.online() {
                Adb::new(&tools, &device.serial)
                    .ok()
                    .and_then(|adb| adb.getprop("ro.product.cpu.abi"))
            } else {
                None
            };
            lines.push_str(&format!(
                "{:16} {:12} {:12} {}\n",
                device.serial,
                device.state,
                abi.clone().unwrap_or_default(),
                avd.clone().unwrap_or_default()
            ));
            json!({
                "serial": device.serial,
                "state": device.state,
                "kind": if device.is_emulator() { "emulator" } else { "device" },
                "avd": avd,
                "abi": abi,
                "model": device.props.get("model"),
            })
        })
        .collect();

    let default = device::default_avd(&host, target_sdk);
    let avds: Vec<Value> = avd::list(&ctx.env)
        .into_iter()
        .map(|name| {
            let running = emulators
                .iter()
                .find(|(_, avd)| avd.as_deref() == Some(name.as_str()))
                .map(|(serial, _)| serial.clone());
            lines.push_str(&format!(
                "avd {name}{}{}\n",
                if name == default { " (icm's)" } else { "" },
                running
                    .as_ref()
                    .map(|serial| format!(" running as {serial}"))
                    .unwrap_or_default()
            ));
            json!({
                "name": name,
                "abi": avd::abi(&ctx.env, &name).map(Abi::as_str),
                "managed": avd::is_managed(&name),
                "default": name == default,
                "running": running,
            })
        })
        .collect();

    Ok((
        lines,
        json!({"devices": devices, "avds": avds, "default_avd": default}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A context whose output goes nowhere.
    fn quiet_ctx() -> Ctx {
        recording_ctx().0
    }

    /// The NDJSON lines a test context wrote.
    #[derive(Clone, Default)]
    struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        /// The check events written so far with the id `id`.
        fn checks(&self, id: &str) -> Vec<Value> {
            String::from_utf8(self.0.lock().unwrap().clone())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .filter(|event| event["type"] == "check" && event["id"] == id)
                .collect()
        }
    }

    /// A `run android` context whose events go to the returned sink.
    fn recording_ctx() -> (Ctx, Sink) {
        let sink = Sink::default();
        let rep = crate::output::Reporter::with_writers(
            crate::output::Mode {
                json: true,
                ..crate::output::Mode::default()
            },
            crate::output::RunInfo {
                run: "20261007T000000Z-run-android-0000".into(),
                command: "run".into(),
                target: Some("android".into()),
                argv: vec![],
                save: false,
            },
            Box::new(sink.clone()),
            Box::new(std::io::sink()),
        );
        (
            Ctx::new(crate::cli::GlobalArgs::default(), rep, vec![]),
            sink,
        )
    }

    /// A fake adb in `dir` for `emulator-5580`, running the shell `body`
    /// with `-s <serial>` shifted off (`$1` is adb's command, `$2` the
    /// line of `adb shell`).
    fn scripted_adb(dir: &Path, body: &str) -> Adb {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("adb");
        std::fs::write(
            &path,
            format!("#!/bin/sh\n[ \"$1\" = -s ] && shift 2\n{body}"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Adb::from_program(&path, "emulator-5580")
    }

    /// A fake adb in `dir`: `pidof` prints `pids`, and every other command
    /// prints nothing and exits 0 (no events, no activities).
    fn fake_adb(dir: &Path, pids: &str) -> Adb {
        scripted_adb(
            dir,
            &format!("case \"$2\" in\npidof*) echo '{pids}' ;;\nesac\nexit 0\n"),
        )
    }

    /// The owner is read before anything is done to an emulator, whatever
    /// its record says, and decides with what proves the emulator is this
    /// project's: another project's and an unreadable owner are left,
    /// whether or not the record looks live; an unclaimed emulator is acted
    /// on only with proof (its verified process, or a managed AVD); one
    /// the device marks as this project's, with or without.
    #[test]
    fn the_owner_decides_with_what_proves_the_emulator_is_ours() {
        let own = "0123456789abcdef";
        let other = Owner::Project("fedcba9876543210".to_string());
        let unknown = Owner::Unknown("exit 1: error: device offline".to_string());
        for proven in [false, true] {
            assert_eq!(
                may_act(&Owner::Project(own.to_string()), own, proven),
                Ok(())
            );
            assert_eq!(
                may_act(&other, own, proven),
                Err(Leave::Shared("fedcba9876543210".to_string()))
            );
            assert_eq!(
                may_act(&unknown, own, proven),
                Err(Leave::Unknown("exit 1: error: device offline".to_string()))
            );
        }
        assert_eq!(may_act(&Owner::Nobody, own, true), Ok(()));
        assert_eq!(may_act(&Owner::Nobody, own, false), Err(Leave::Unproven));
    }

    /// The owner property reads as nobody, a project or unknown, and a
    /// claim that adb cannot write is reported with a fix that writes it.
    #[test]
    fn emulator_owners_and_failed_claims() {
        let dir = tempfile::tempdir().unwrap();
        // `getprop` prints what `setprop` stored; with the file `fails`,
        // both fail as adb does for a device it lost.
        let adb = scripted_adb(
            dir.path(),
            r#"state=$(dirname "$0")
if [ -f "$state/fails" ]; then echo "error: device offline" >&2; exit 1; fi
case "$2" in
"getprop debug.icm.booted_by") cat "$state/tag" 2>/dev/null; echo ;;
"setprop debug.icm.booted_by "*) echo "${2##* }" > "$state/tag" ;;
esac
exit 0
"#,
        );
        let tag = "0123456789abcdef";
        let (ctx, sink) = recording_ctx();
        assert_eq!(emulator_owner(&adb), Owner::Nobody);
        claim(&ctx, &adb, tag);
        assert_eq!(emulator_owner(&adb), Owner::Project(tag.to_string()));
        assert!(sink.checks("android.emulator.owner_unknown").is_empty());

        std::fs::write(dir.path().join("fails"), "").unwrap();
        let Owner::Unknown(why) = emulator_owner(&adb) else {
            panic!("a failed getprop read as an answer");
        };
        assert_eq!(why, "exit 1: error: device offline");
        claim(&ctx, &adb, tag);
        let unclaimed = sink.checks("android.emulator.owner_unknown");
        assert_eq!(unclaimed.len(), 1, "{unclaimed:?}");
        assert_eq!(unclaimed[0]["status"], "warn");
        let detail = unclaimed[0]["detail"].as_str().unwrap();
        assert!(
            detail.starts_with("could not mark emulator-5580 as this project's")
                && detail.contains("exit 1: error: device offline"),
            "{detail}"
        );
        assert_eq!(
            unclaimed[0]["fix"]["commands"],
            json!(["adb -s emulator-5580 shell setprop debug.icm.booted_by 0123456789abcdef"])
        );
    }

    /// A `--wait-ready` that ends before the app's process shows up says
    /// that readiness was not established, never that the app is alive;
    /// one that ends before the resumed-activity probe starts does not say
    /// the activity never resumed.
    #[test]
    fn a_short_wait_says_only_what_it_saw() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = quiet_ctx();
        let wait = |adb: &Adb| {
            wait_ready(
                &ctx,
                adb,
                "com.example.app",
                "1791334000.123456789",
                Instant::now(),
                Duration::from_secs(1),
            )
            .expect_err("ready without a frame")
        };

        let absent = wait(&fake_adb(dir.path(), ""));
        assert_eq!(absent.id, CheckId::RunNotReady.id(), "{}", absent.detail);
        assert!(!absent.detail.contains("alive"), "{}", absent.detail);
        assert!(
            absent
                .detail
                .starts_with("com.example.app was not ready within ")
                && absent.detail.contains("no process of it"),
            "{}",
            absent.detail
        );

        let running = wait(&fake_adb(dir.path(), "4321"));
        assert_eq!(running.id, CheckId::RunNotReady.id(), "{}", running.detail);
        assert!(
            running.detail.starts_with("com.example.app is alive but "),
            "{}",
            running.detail
        );
        assert!(
            !running.detail.contains("never became the resumed activity"),
            "{}",
            running.detail
        );
        assert!(
            running
                .detail
                .contains("before the resumed-activity probe starts"),
            "{}",
            running.detail
        );

        // Android relaunched the activity, which keeps the probe off: a
        // wait that ran past the probe's start says so, not that the
        // probe had yet to start.
        let relaunched = scripted_adb(
            dir.path(),
            "case \"$1\" in
shell) case \"$2\" in pidof*) echo 4321 ;; esac ;;
logcat) case \"$*\" in *wm_relaunch_resume_activity:I*)
  echo '1791334001.000   600   610 I wm_relaunch_resume_activity: [0,175822296,8,com.example.app/android.app.NativeActivity,80000000]' ;;
esac ;;
esac
exit 0
",
        );
        let late = wait_ready(
            &ctx,
            &relaunched,
            "com.example.app",
            "1791334000.123456789",
            Instant::now() - Duration::from_secs(7),
            Duration::from_secs(8),
        )
        .expect_err("ready without a frame");
        assert_eq!(late.id, CheckId::RunNotReady.id(), "{}", late.detail);
        assert!(
            late.detail.starts_with("com.example.app is alive but ")
                && late.detail.contains("Android had relaunched its activity")
                && late.detail.contains("run.activity_recreated")
                && !late
                    .detail
                    .contains("before the resumed-activity probe starts"),
            "{}",
            late.detail
        );
    }

    /// Readiness comes from the app's own processes: another iced_mobile
    /// app's `ready` or `panic` (every one writes `ICM_EVENT` once the
    /// system property is set) is skipped, also before any pid of the app
    /// is known.
    #[test]
    fn only_the_apps_events_make_it_ready() {
        let other = logcat::parse(
            "1791333711.000  7777  7777 I ICM_EVENT: {\"v\":1,\"kind\":\"start\",\"protocol\":1}
1791333711.100  7777  7777 I ICM_EVENT: {\"v\":1,\"kind\":\"ready\",\"ms\":90}
1791333711.200  7777  7777 I ICM_EVENT: {\"v\":1,\"kind\":\"panic\",\"message\":\"not ours\",\"location\":\"x.rs:1:1\"}
",
        );
        let mut pids = BTreeSet::new();
        let nobody = logcat::Processes::default();
        assert!(matches!(hear(&other, &nobody, &mut pids), Heard::Silent));
        assert!(pids.is_empty());

        // The app's process started (in the events buffer) and is drawing.
        let events = logcat::parse(
            "1791333710.000   600   610 I am_proc_start: [0,4321,10123,com.example.app,next-top-activity,{com.example.app/android.app.NativeActivity}]
1791333710.500   600   610 I am_proc_start: [0,7777,10124,com.other.iced,activity,{com.other.iced/android.app.NativeActivity}]
",
        );
        let processes = logcat::Processes::read("com.example.app", Vec::new(), &events);
        let mut records = logcat::parse(
            "1791333710.900  4321  4321 I ICM_EVENT: {\"v\":1,\"kind\":\"start\",\"protocol\":1}\n",
        );
        records.extend(other.iter().cloned());
        assert!(matches!(
            hear(&records, &processes, &mut pids),
            Heard::Started
        ));
        assert_eq!(pids, [4321].into_iter().collect());

        records.extend(logcat::parse(
            "1791333712.000  4321  4330 I ICM_EVENT: {\"v\":1,\"kind\":\"ready\",\"ms\":1100}\n",
        ));
        let Heard::Ready(ready) = hear(&records, &processes, &mut pids) else {
            panic!("not ready");
        };
        assert_eq!(ready.pid, Some(4321));
        assert_eq!(ready.ms, Some(1100));
        assert_eq!(ready.pids, [4321].into_iter().collect());

        // The app's own panic fails the wait.
        let panicked = logcat::parse(
            "1791333711.500  4321  4330 I ICM_EVENT: {\"v\":1,\"kind\":\"panic\",\"message\":\"boom\",\"location\":\"src/lib.rs:3:5\"}\n",
        );
        let Heard::Panicked(error) = hear(&panicked, &processes, &mut pids) else {
            panic!("no panic");
        };
        assert_eq!(error.detail, "panicked at src/lib.rs:3:5: boom");
    }

    fn relaunch(at: &str, mask: Option<u32>) -> logcat::Relaunch {
        logcat::Relaunch {
            record: logcat::parse_line(&format!(
                "{at}   660   683 I wm_relaunch_resume_activity: [0,1,8,com.example.app/android.app.NativeActivity,{}]",
                mask.map(|m| format!("{m:x}")).unwrap_or_default()
            ))
            .unwrap(),
            component: "com.example.app/android.app.NativeActivity".to_string(),
            mask,
        }
    }

    #[test]
    fn recreations_name_their_cause() {
        assert!(describe_recreation(&[], 0.0, 36, &Restart::Seen).is_none());

        // The fresh-emulator case: SystemUI's overlays, with a manifest
        // linked below API 36, which cannot list assetsPaths.
        let overlays = [relaunch("100.400", Some(0x8000_0000))];
        let (detail, recreated) =
            describe_recreation(&overlays, 100.0, 35, &Restart::Seen).unwrap();
        assert!(
            detail.starts_with(
                "Android relaunched com.example.app/android.app.NativeActivity 400ms after launch (assetsPaths)"
            ),
            "{detail}"
        );
        assert!(detail.contains("started over"), "{detail}");
        assert!(recreated.cause.contains("runtime resource overlay"));
        assert!(recreated.cause.ends_with("(run.activity_recreated)"));
        assert!(recreated.fix.contains("target_sdk to 36"));

        // From API 36 icm's manifest lists it: the installed APK is older.
        let (_, stale) = describe_recreation(&overlays, 100.0, 36, &Restart::Seen).unwrap();
        assert!(
            stale.cause.contains("an earlier icm built it"),
            "{}",
            stale.cause
        );
        assert!(stale.fix.contains("without --no-build"));

        // A change no configChanges name covers, twice.
        let unknown = [
            relaunch("101.5", Some(0x0100_0000)),
            relaunch("102", Some(0x0100_0000)),
        ];
        let (detail, recreated) = describe_recreation(&unknown, 100.0, 36, &Restart::Seen).unwrap();
        assert!(detail.contains("(0x1000000), 2 relaunches"), "{detail}");
        assert!(
            recreated
                .cause
                .contains("does not list for target_sdk 36 (0x1000000)")
        );

        // Older releases log no mask.
        let (detail, _) =
            describe_recreation(&[relaunch("100", None)], 100.0, 36, &Restart::Seen).unwrap();
        assert!(detail.contains("(no change named)"), "{detail}");
    }

    #[test]
    fn a_relaunch_the_app_did_not_survive_says_so() {
        let unlisted = [relaunch("101", Some(0x200))];
        let (detail, missing) =
            describe_recreation(&unlisted, 100.0, 36, &Restart::Missing).unwrap();
        assert!(detail.contains("(uiMode)"), "{detail}");
        assert!(
            detail.contains("No ICM_EVENT start followed within 10.0s"),
            "{detail}"
        );
        assert!(!detail.contains("started over in the new one"), "{detail}");
        assert!(
            missing.cause.contains("did not start over"),
            "{}",
            missing.cause
        );
        assert!(missing.fix.contains("update it"), "{}", missing.fix);

        let (detail, old) = describe_recreation(
            &unlisted,
            100.0,
            36,
            &Restart::OldFramework("winit 0.30.13 from crates.io".to_string()),
        )
        .unwrap();
        assert!(
            detail.contains("its Cargo.lock has winit 0.30.13 from crates.io"),
            "{detail}"
        );
        assert!(detail.contains("freezes"), "{detail}");
        assert!(old.cause.contains("freezes"), "{}", old.cause);
        assert!(old.fix.contains("iced_mobile pin"), "{}", old.fix);
    }
}
