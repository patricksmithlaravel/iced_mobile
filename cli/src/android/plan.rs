//! What `--dry-run` (and `icm print plan`) shows for the Android commands:
//! the steps each would run, and nothing more. A dry run touches no device
//! (adb is not even started), takes no lock and writes nothing; it only
//! reads the SDK and NDK from disk to show the real build command.

use super::Toolset;
use super::apk;
use crate::catalogue::CheckId;
use crate::cli::{BuildArgs, InputArgs, LogsArgs, RunArgs, ShotArgs, StopArgs};
use crate::config::Abi;
use crate::context::{Ctx, Project};
use crate::error::{IcmError, Result};
use crate::plan::{Plan, Step};
use crate::time::format_duration;
use serde_json::json;

/// The project and, when the SDK is found, the toolset.
fn setup(ctx: &mut Ctx) -> Result<(Project, Option<Toolset>)> {
    let project = ctx.project()?.clone();
    let host = ctx.host()?.clone();
    Ok((project, Toolset::discover(&host, &ctx.env).ok()))
}

fn profile(release: bool) -> &'static str {
    if release { "release" } else { "dev" }
}

/// The steps of a dev APK build for one ABI.
fn build_steps(
    ctx: &Ctx,
    project: &Project,
    tools: Option<&Toolset>,
    abi: Abi,
    release: bool,
    plan: &mut Plan,
) -> Result<()> {
    let config = &project.config.config;
    let package = project.package_for("android")?.clone();
    let lib = project.lib_name()?;
    let triple = abi.triple();
    let profile = profile(release);

    let mut env: Vec<(String, String)> = tools.map(|t| t.child_env().to_vec()).unwrap_or_default();
    match tools.and_then(|t| t.ndk.as_ref().ok()) {
        Some(ndk) => env.extend(crate::tools::ndk_env(ndk, triple, config.android.min_sdk)),
        None => plan.push(Step::internal(
            "android.ndk",
            "no NDK r28+ was found: the build would fail with env.ndk_too_old (`icm doctor android --fix --yes`)",
        )),
    }
    plan.push(Step::internal(
        "android.tools",
        &format!(
            "check the {triple} target on the project's toolchain, a JDK 17+, build-tools 35+ and platforms/android-{}/android.jar",
            config.android.target_sdk
        ),
    ));
    plan.push(
        Step::exec(
            "cargo.rustc",
            apk::cdylib_cmd(ctx.global.offline, &package, triple, profile, &env),
        )
        .on_fail(CheckId::BuildCompileError),
    );
    plan.push(
        Step::internal(
            "android.so",
            &format!(
                "ELF gates on lib{lib}.so: ANativeActivity_onCreate exported, {} machine, PT_LOAD aligned to 16 KB",
                abi.as_str()
            ),
        )
        .gate(CheckId::AndroidSoExport)
        .gate(CheckId::AndroidSoAlign16k),
    );
    let gen_dir = project.gen_dir("android", profile);
    plan.push(Step::internal(
        "llvm-strip",
        &format!(
            "llvm-strip --strip-debug into {}",
            crate::paths::display(&gen_dir.join("lib").join(abi.as_str()))
        ),
    ));
    plan.push(Step::internal(
        "aapt2",
        &format!(
            "write AndroidManifest.xml and res/ in {}, then aapt2 compile and aapt2 link -I android-{}.jar --version-code {} --version-name {}{}",
            crate::paths::display(&gen_dir),
            config.android.target_sdk,
            config.app.build,
            package.version,
            if release { "" } else { " --debug-mode" }
        ),
    ));
    plan.push(
        Step::internal(
            "android.apk",
            &format!(
                "pack {} (stored), zipalign -P 16 4, apksigner sign with the debug keystore {}, apksigner verify, zipalign -c -P 16 4",
                crate::paths::display(&apk::apk_path(project, &package.name, profile)),
                crate::paths::display(&super::debug_keystore())
            ),
        )
        .gate(CheckId::AndroidApkSignature)
        .gate(CheckId::AndroidApkZipalign),
    );
    Ok(())
}

/// `icm build android --dry-run`.
pub fn build(ctx: &mut Ctx, args: &BuildArgs) -> Result<()> {
    let (project, tools) = setup(ctx)?;
    let abi = match &args.abi {
        Some(name) => Abi::from_name(name).ok_or_else(|| {
            IcmError::new(
                CheckId::UsageBadArgs,
                format!("unknown ABI `{name}`; use arm64-v8a, x86_64, armeabi-v7a or x86"),
            )
        })?,
        None => super::avd::host_abi(),
    };
    let mut plan = Plan::new();
    if let Some(serial) = &args.device {
        plan.push(Step::internal(
            "android.abi",
            &format!(
                "read ro.product.cpu.abi of {serial} (planned below for {})",
                abi.as_str()
            ),
        ));
    }
    build_steps(ctx, &project, tools.as_ref(), abi, args.release, &mut plan)?;
    finish(ctx, plan, "build android");
    Ok(())
}

/// `icm run android --dry-run`.
pub fn run(ctx: &mut Ctx, args: &RunArgs) -> Result<()> {
    let (project, tools) = setup(ctx)?;
    let config = &project.config.config;
    let managed = super::avd::managed_name(config.android.target_sdk);
    let device = match (&args.device, &args.avd) {
        (Some(serial), _) => format!("use device {serial}"),
        (None, Some(avd)) => format!("boot or reuse the AVD {avd}"),
        (None, None) => format!(
            "choose the device: $ANDROID_SERIAL, host.toml android.device, a running {managed}, the single online device, else boot {managed}"
        ),
    };
    let mut plan = Plan::new();
    plan.push(Step::internal("android.device", &device));
    let uninstall = if args.reinstall {
        " (after pm uninstall, keeping data unless --wipe-data)"
    } else {
        ""
    };
    if args.from_aab {
        plan.push(Step::internal(
            "android.aab",
            &format!(
                "the .aab of the newest release ({}), which `icm release android` made; nothing is built",
                crate::paths::display(&crate::release::dist::latest(&project, "android"))
            ),
        ));
        plan.push(Step::internal(
            "bundletool.install",
            &format!(
                "bundletool build-apks --connected-device --device-id=<serial> with icm's debug keystore {}, then bundletool install-apks --allow-downgrade{uninstall}",
                crate::paths::display(&super::debug_keystore())
            ),
        ));
    } else {
        if !args.no_build {
            build_steps(
                ctx,
                &project,
                tools.as_ref(),
                super::avd::host_abi(),
                args.release,
                &mut plan,
            )?;
        }
        let profile = profile(args.release);
        let package = project.package_for("android")?;
        plan.push(Step::internal(
            "adb.install",
            &format!(
                "adb install -r -d {}{uninstall}",
                crate::paths::display(&apk::apk_path(&project, &package.name, profile)),
            ),
        ));
    }
    plan.push(Step::internal(
        "android.props",
        "setprop debug.icm.events 1, and debug.iced.backend from --env ICED_BACKEND",
    ));
    plan.push(Step::internal(
        "android.launch",
        &format!(
            "am start -W -S -n {}/{}",
            config.app.id,
            super::manifest::ACTIVITY
        ),
    ));
    plan.push(
        Step::internal(
            "android.ready",
            &format!(
                "wait up to {} for ICM_EVENT ready in logcat",
                format_duration(args.wait_ready)
            ),
        )
        .gate(CheckId::RunReady)
        .on_fail(CheckId::RunNotReady),
    );
    if !args.no_shot {
        plan.push(
            Step::internal(
                "android.screenshot",
                "adb exec-out screencap -p into the run directory, then the preview and blank check",
            )
            .gate(CheckId::RunScreenBlank),
        );
    }
    plan.push(Step::internal(
        "hooks",
        "run the [checks] android scripts, if any",
    ));
    finish(ctx, plan, "run android");
    Ok(())
}

/// `icm stop android --dry-run`.
pub fn stop(ctx: &mut Ctx, args: &StopArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    let mut plan = Plan::new();
    plan.push(Step::internal(
        "android.stop",
        &format!(
            "am force-stop {} on the session's device ({})",
            project.config.config.app.id,
            crate::paths::display(&project.sessions_dir().join("android.json"))
        ),
    ));
    if args.shutdown {
        plan.push(Step::internal(
            "android.shutdown",
            "adb emu kill for the emulator icm booted for this project, and for a running icm-* AVD no other project claims",
        ));
    }
    finish(ctx, plan, "stop android");
    Ok(())
}

/// `icm shot android --dry-run`.
pub fn shot(ctx: &mut Ctx, args: &ShotArgs) -> Result<()> {
    let _ = ctx.project()?;
    let mut plan = Plan::new();
    plan.push(Step::internal(
        "android.screenshot",
        &format!(
            "adb exec-out screencap -p on the session's device into {}",
            args.out
                .as_deref()
                .map(crate::paths::display)
                .unwrap_or_else(|| "the run directory".to_string())
        ),
    ));
    finish(ctx, plan, "shot android");
    Ok(())
}

/// `icm logs android --dry-run`.
pub fn logs(ctx: &mut Ctx, args: &LogsArgs) -> Result<()> {
    let _ = ctx.project()?;
    let mut plan = Plan::new();
    plan.push(Step::internal(
        "logcat",
        &format!(
            "adb logcat -d -v threadtime,epoch from {} on the session's device (main, system, crash buffers), the last {} records{}",
            if args.since == "launch" {
                "the launch mark".to_string()
            } else {
                args.since.clone()
            },
            args.tail,
            if args.follow { ", then follow" } else { "" }
        ),
    ));
    finish(ctx, plan, "logs android");
    Ok(())
}

/// `icm input android --dry-run`.
pub fn input(ctx: &mut Ctx, args: &InputArgs) -> Result<()> {
    let _ = ctx.project()?;
    let mut plan = Plan::new();
    plan.push(Step::internal(
        "android.input",
        &format!(
            "send {:?} to the session's device with adb shell input ({} coordinates)",
            args.action,
            match args.space {
                crate::screen::Space::Preview => "preview",
                crate::screen::Space::Px => "px",
                crate::screen::Space::Pt => "pt",
            }
        ),
    ));
    finish(ctx, plan, "input android");
    Ok(())
}

/// `icm devices android --dry-run`.
pub fn devices(ctx: &mut Ctx) -> Result<()> {
    let mut plan = Plan::new();
    plan.push(Step::internal(
        "android.devices",
        "adb devices -l, and the AVDs in ANDROID_AVD_HOME",
    ));
    finish(ctx, plan, "devices android");
    Ok(())
}

fn finish(ctx: &Ctx, plan: Plan, what: &str) {
    plan.report(ctx);
    ctx.rep.set("platform", json!("android"));
    ctx.rep
        .summary(format!("the plan of icm {what} (--dry-run: nothing ran)"));
}
