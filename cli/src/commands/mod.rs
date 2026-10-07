//! Command dispatch. Commands this build does not implement yet fail with
//! `usage.not_implemented` (exit 2) and say so.

pub mod build;
pub mod check;
pub mod detach;
pub mod devices;
pub mod doctor;
pub mod explain;
pub mod headless;
pub mod new;
pub mod print;
pub mod selftest;
pub mod stop;
pub mod test;
pub mod wait;

use crate::catalogue::CheckId;
use crate::cli::{Command, LATER_COMMANDS, Platform};
use crate::context::Ctx;
use crate::error::{IcmError, Result};
use crate::platform::{desktop, ios_device, ios_sim};

/// Runs a command.
pub fn dispatch(ctx: &mut Ctx, command: Command) -> Result<()> {
    // ios-sim and android match their own signatures in the system log and
    // logcat; the desktop and web pipelines leave it to this.
    let run_platform = match &command {
        Command::Run(args) if matches!(args.platform, Platform::Desktop | Platform::Web) => {
            Some(args.platform)
        }
        _ => None,
    };
    let result = route(ctx, command);
    match run_platform {
        Some(platform) => result.map_err(|error| with_signatures(error, platform)),
        None => result,
    }
}

/// A failed `icm run desktop|web`: the known failure signatures
/// (design §13.4, [`crate::signatures`]) in the text files its evidence
/// names (the app's stderr, logcat, the console, crash reports) become
/// likely causes. The panic itself is left to the platform, which already
/// names its location.
fn with_signatures(mut error: IcmError, platform: Platform) -> IcmError {
    let facts = crate::signatures::Facts {
        platform: Some(platform.as_str()),
        panicked: error.id == CheckId::RunAppPanicked.id(),
        ..crate::signatures::Facts::default()
    };
    let mut seen: Vec<std::path::PathBuf> = Vec::new();
    for evidence in &error.evidence {
        let path = std::path::PathBuf::from(&evidence.path);
        let text_like = !path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext, "png" | "jpg" | "jpeg" | "apk" | "so" | "wasm"));
        if text_like && path.is_file() && !seen.contains(&path) {
            seen.push(path);
        }
    }
    for path in seen {
        for found in crate::signatures::scan_file(&path, &facts) {
            if found.name != "panic" && !error.likely_causes.contains(&found.cause) {
                error.likely_causes.push(found.cause);
            }
        }
    }
    error
}

/// The commands that have no plan: they build and run the app's code
/// (`check`, host `test`, `ui`) or read and run a release (`verify`). The
/// lifecycle suite plans (`test --on android --lifecycle`), and `test --on`
/// anything else is refused by `test` itself.
fn planless(command: &Command) -> Option<&'static str> {
    match command {
        Command::Check(_) => Some("check"),
        Command::Test(args) if args.device().is_none() && !args.lifecycle => Some("test"),
        Command::Ui(_) => Some("ui"),
        Command::Verify(_) => Some("verify"),
        _ => None,
    }
}

/// `--dry-run` promises a plan and no change (`icm --help`): a command
/// without a plan refuses it before it starts, rather than run for real.
/// The fix is the same command line without `--dry-run` (or `print plan`).
fn refuse_dry_run(argv: &[String], name: &str) -> IcmError {
    let mut args: Vec<&str> = argv
        .iter()
        .skip(1)
        .map(String::as_str)
        .filter(|arg| *arg != "--dry-run")
        .collect();
    if args.starts_with(&["print", "plan"]) {
        let _ = args.drain(..2);
    }
    let again = std::iter::once("icm".to_string())
        .chain(args.iter().map(|arg| crate::process::shell_quote(arg)))
        .collect::<Vec<_>>()
        .join(" ");
    IcmError::new(
        CheckId::UsageBadArgs,
        format!(
            "`icm {name}` has no plan to print, so it refuses --dry-run (and `icm print plan`) instead of running for real; nothing ran"
        ),
    )
    .fix(
        format!("Run `icm {name}` without --dry-run; it touches no device."),
        &[again.as_str()],
    )
}

fn route(ctx: &mut Ctx, command: Command) -> Result<()> {
    if ctx.dry_run()
        && let Some(name) = planless(&command)
    {
        return Err(refuse_dry_run(&ctx.argv, name));
    }
    let android = Some(Platform::Android);
    match command {
        Command::Explain(args) => explain::run(ctx, &args),
        Command::Wait(args) => wait::run(ctx, &args),
        Command::Print(args) => print::run(ctx, args),
        Command::SelfTest(args) => selftest::run(ctx, args),
        Command::External(args) => external(&args),
        Command::New(args) => new::run(ctx, &args),
        Command::Doctor(args) => doctor::run(ctx, &args),
        Command::Check(args) => check::run(ctx, &args),
        // desktop (design §10.1)
        Command::Build(args) if !args.all && args.platform == Some(Platform::Desktop) => {
            desktop::build(ctx, &args)
        }
        Command::Run(args) if args.platform == Platform::Desktop => desktop::run(ctx, &args),
        Command::Stop(args) if !args.all && args.platform == Some(Platform::Desktop) => {
            desktop::stop(ctx, &args)
        }
        Command::Shot(args) if !args.headless && args.platform == Some(Platform::Desktop) => {
            desktop::shot(ctx, &args)
        }
        Command::Logs(args) if args.platform == Platform::Desktop => desktop::logs(ctx, &args),
        Command::Input(args) if args.platform == Platform::Desktop => desktop::input(ctx, &args),
        // ios-sim (design §10.3)
        Command::Build(args) if args.platform == Some(Platform::IosSim) && !args.all => {
            ios_sim::build(ctx, &args)
        }
        Command::Run(args) if args.platform == Platform::IosSim => ios_sim::run(ctx, &args),
        Command::Logs(args) if args.platform == Platform::IosSim => ios_sim::logs(ctx, &args),
        Command::Shot(args) if !args.headless && args.platform == Some(Platform::IosSim) => {
            ios_sim::shot(ctx, &args)
        }
        Command::Stop(args) if args.platform == Some(Platform::IosSim) && !args.all => {
            ios_sim::stop(ctx, &args)
        }
        Command::Input(args) if args.platform == Platform::IosSim => {
            ios_sim::input::input(ctx, &args)
        }
        // ios-device (design §10.5); its sessions stop by their record
        Command::Build(args) if args.platform == Some(Platform::IosDevice) && !args.all => {
            ios_device::build(ctx, &args)
        }
        Command::Run(args) if args.platform == Platform::IosDevice => ios_device::run(ctx, &args),
        Command::Logs(args) if args.platform == Platform::IosDevice => ios_device::logs(ctx, &args),
        Command::Shot(args) if !args.headless && args.platform == Some(Platform::IosDevice) => {
            ios_device::shot(ctx, &args)
        }
        Command::Input(args) if args.platform == Platform::IosDevice => {
            ios_device::input(ctx, &args)
        }
        // android (design §10.4); `icm doctor android` is the generic doctor
        Command::Build(args) if args.platform == android && !args.all => {
            crate::android::build(ctx, &args)
        }
        Command::Run(args) if args.platform == Platform::Android => crate::android::run(ctx, &args),
        Command::Stop(args) if args.platform == android && !args.all => {
            crate::android::stop(ctx, &args)
        }
        Command::Devices(args) if args.platform == android => crate::android::devices(ctx),
        Command::Shot(args) if args.platform == android && !args.headless => {
            crate::android::shot(ctx, &args)
        }
        Command::Logs(args) if args.platform == Platform::Android => {
            crate::android::logs(ctx, &args)
        }
        Command::Input(args) if args.platform == Platform::Android => {
            crate::android::input(ctx, &args)
        }
        Command::Test(args) if args.lifecycle && args.device() == android => {
            crate::android::lifecycle::run(ctx, &args)
        }
        // web (design §10.2)
        Command::Run(args) if args.platform == Platform::Web => crate::web::run(ctx, &args),
        Command::Build(args) if args.platform == Some(Platform::Web) && !args.all => {
            crate::web::build_command(ctx, &args)
        }
        Command::Shot(args) if !args.headless && args.platform == Some(Platform::Web) => {
            crate::web::shot(ctx, &args)
        }
        Command::Logs(args) if args.platform == Platform::Web => crate::web::logs(ctx, &args),
        Command::Input(args) if args.platform == Platform::Web => crate::web::input(ctx, &args),
        Command::Session(args) => crate::web::host::main(ctx, &args.args),
        Command::Build(args) if args.all || args.platform.is_none() => build::all(ctx, &args),
        Command::Build(_) => not_implemented("build"),
        Command::Run(_) => not_implemented("run"),
        Command::Stop(args) => stop::stop(ctx, &args),
        Command::Ps => stop::ps(ctx),
        Command::Devices(args) => devices::run(ctx, &args),
        Command::Shot(args) if args.headless => headless::shot(ctx, &args),
        Command::Shot(args) if args.platform.is_none() => Err(IcmError::new(
            CheckId::UsageBadArgs,
            "name the platform whose running app to capture (`icm shot android`), or pass --headless",
        )
        .fix(
            "Capture the app `icm run` started on a platform, or render it without a device.",
            &["icm ps --json -q", "icm shot --headless --json -q"],
        )),
        Command::Shot(_) => not_implemented("shot"),
        Command::Logs(_) => not_implemented("logs"),
        Command::Input(_) => not_implemented("input"),
        Command::Ui(args) => headless::ui(ctx, &args),
        Command::Test(args) => test::run(ctx, &args),
        Command::Clean(_) => not_implemented("clean"),
        // releases (design §11, §12)
        Command::Release(args) => crate::release::run(ctx, &args),
        Command::Verify(args) => crate::release::verify::run(ctx, &args),
        Command::UploadCommands(args) => crate::release::upload_commands(ctx, args.target),
        Command::Ledger(args) => crate::release::ledger::run(ctx, &args),
        Command::Diagnose(args) => crate::release::diagnose::run(ctx, &args),
    }
}

/// The error for a phase-1 command this build does not implement yet.
pub fn not_implemented(command: &str) -> Result<()> {
    Err(IcmError::new(
        CheckId::UsageNotImplemented,
        format!(
            "`icm {command}` is not implemented in this build of icm ({})",
            crate::buildinfo::VERSION_LINE
        ),
    )
    .fix(
        "Use a command this build implements (`icm --help`), or install a newer icm.",
        &["icm --help"],
    ))
}

fn external(args: &[String]) -> Result<()> {
    let name = args.first().map(String::as_str).unwrap_or("");
    if LATER_COMMANDS.contains(&name) {
        return Err(IcmError::new(
            CheckId::UsageNotImplemented,
            format!(
                "`icm {name}` belongs to a later phase and is not implemented in this build of icm"
            ),
        )
        .fix("Use a command this build implements.", &["icm --help"]));
    }

    let mut detail = format!("unknown command `{name}`");
    let known = crate::cli::Platform::ALL;
    if known.iter().any(|platform| platform.as_str() == name) {
        detail.push_str(&format!("; did you mean `icm run {name}`?"));
    }
    Err(IcmError::new(CheckId::UsageBadArgs, detail)
        .fix("Run `icm --help` for the commands.", &["icm --help"]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Evidence;

    #[test]
    fn failed_runs_get_the_signatures_in_their_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.stderr");
        std::fs::write(
            &log,
            "thread 'main' panicked at src/main.rs:3:5:\nNo Unix display server backend is available\n",
        )
        .unwrap();
        let error = IcmError::new(CheckId::RunAppPanicked, "the app panicked")
            .evidence(Evidence::file(&log))
            .evidence(Evidence::file(dir.path().join("missing.txt")));
        let error = with_signatures(error, Platform::Desktop);
        // The display-server cause, and not a second panic cause.
        assert_eq!(error.likely_causes.len(), 1, "{:?}", error.likely_causes);
        assert!(error.likely_causes[0].contains("x11 or wayland"));

        // Nothing known: nothing added.
        std::fs::write(&log, "all quiet\n").unwrap();
        let quiet = with_signatures(
            IcmError::new(CheckId::RunAppDied, "x").evidence(Evidence::file(&log)),
            Platform::Web,
        );
        assert!(quiet.likely_causes.is_empty());
    }
}
