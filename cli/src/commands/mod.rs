//! Command dispatch. Commands this build does not implement yet fail with
//! `usage.not_implemented` (exit 2) and say so.

pub mod check;
pub mod detach;
pub mod doctor;
pub mod explain;
pub mod new;
pub mod print;
pub mod selftest;
pub mod stop;
pub mod wait;

use crate::catalogue::CheckId;
use crate::cli::{Command, LATER_COMMANDS, Platform};
use crate::context::Ctx;
use crate::error::{IcmError, Result};
use crate::platform::{desktop, ios_sim};

/// Runs a command.
pub fn dispatch(ctx: &mut Ctx, command: Command) -> Result<()> {
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
        Command::Build(_) => not_implemented("build"),
        Command::Run(_) => not_implemented("run"),
        Command::Stop(args) => stop::stop(ctx, &args),
        Command::Ps => stop::ps(ctx),
        Command::Devices(_) => not_implemented("devices"),
        Command::Shot(_) => not_implemented("shot"),
        Command::Logs(_) => not_implemented("logs"),
        Command::Input(_) => not_implemented("input"),
        Command::Ui(_) => not_implemented("ui"),
        Command::Test(_) => not_implemented("test"),
        Command::Clean(_) => not_implemented("clean"),
        Command::Session(_) => not_implemented("__session"),
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
