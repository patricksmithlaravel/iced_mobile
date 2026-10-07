//! icm, the iced_mobile app tool. See `docs/icm/DESIGN.md` in the fork
//! (Appendix C overrides earlier sections) and `cli/README.md`.
//!
//! The CLI links no iced crate: it drives cargo, Xcode's and the Android
//! SDK's tools, and talks to apps through versioned protocols.

// `IcmError` carries evidence and a fix by value; errors are the cold path
// and end the command, so their size does not matter (the framework's
// workspace allows this lint too).
#![allow(clippy::result_large_err)]

#[cfg(not(unix))]
compile_error!("icm supports macOS and Linux hosts in this phase (Windows hosts come later)");

pub mod android;
pub mod buildinfo;
pub mod cargo;
pub mod catalogue;
pub mod cli;
pub mod commands;
pub mod config;
pub mod context;
pub mod deps;
pub mod doctor;
pub mod error;
pub mod exit;
pub mod gitinfo;
pub mod harness;
pub mod hash;
pub mod hooks;
pub mod host;
pub mod image;
pub mod locks;
pub mod managed;
pub mod output;
pub mod paths;
pub mod plan;
pub mod platform;
pub mod preview;
pub mod process;
pub mod raster;
pub mod screen;
pub mod session;
pub mod sessions;
pub mod signals;
pub mod signatures;
pub mod simctl;
pub mod template;
pub mod time;
pub mod toolchain;
pub mod tools;
pub mod version;
pub mod web;

use catalogue::CheckId;
use clap::Parser;
use cli::Cli;
use context::Ctx;
use error::IcmError;
use exit::Exit;
use output::{Mode, Reporter, RunInfo, rundir};
use std::ffi::OsString;
use std::panic::AssertUnwindSafe;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static FINISHED: AtomicBool = AtomicBool::new(false);
static PANIC: Mutex<Option<String>> = Mutex::new(None);

/// icm's entry point; returns the process exit code.
pub fn main() -> std::process::ExitCode {
    signals::install();
    let argv: Vec<String> = std::env::args_os()
        .map(|arg: OsString| arg.to_string_lossy().into_owned())
        .collect();

    let exit = match Cli::try_parse_from(&argv) {
        Ok(cli) => run(cli, argv),
        Err(error) => usage_error(&error, &argv),
    };
    std::process::ExitCode::from(exit.code())
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}

fn run(cli: Cli, argv: Vec<String>) -> Exit {
    let (command, target) = cli.command.name_and_target();
    let detaching =
        cli.global.detach && !cli.command.is_view() && !env_flag(commands::detach::DETACHED_ENV);
    // An unknown or later-phase command, and the command after `print
    // plan`, swallow their flags unparsed.
    let swallowed: &[String] = match &cli.command {
        cli::Command::External(args) => args,
        cli::Command::Print(cli::PrintArgs {
            what: cli::PrintWhat::Plan { command },
        }) => command,
        _ => &[],
    };
    let (external_json, external_quiet) = (
        swallowed.iter().any(|arg| arg == "--json"),
        swallowed.iter().any(|arg| is_quiet_flag(arg)),
    );
    let json = cli.global.json || env_flag("ICM_JSON") || external_json;

    let inherited = std::env::var("ICM_RUN_ID")
        .ok()
        .filter(|id| rundir::is_run_id(id) && env_flag(commands::detach::DETACHED_ENV));
    let run_id = match (&cli.command, inherited) {
        (cli::Command::Wait(args), _) => args.run.clone(),
        (_, Some(id)) => id,
        _ => rundir::new_run_id(&command, target.as_deref()),
    };

    let mode = Mode {
        json,
        quiet: cli.global.quiet || external_quiet,
        verbose: cli.global.verbose,
        strict: cli.global.strict,
        content: cli.command.is_content(),
    };
    let rep = Reporter::new(
        mode,
        RunInfo {
            run: run_id,
            command,
            target,
            argv: argv.clone(),
            save: !cli.command.is_view() && !detaching,
        },
    );
    rep.make_active();

    // A detached child continues the run directory its parent created.
    if let (Ok(dir), Ok(root)) = (std::env::var("ICM_RUN_DIR"), std::env::var("ICM_RUN_ROOT"))
        && env_flag(commands::detach::DETACHED_ENV)
    {
        let _ = rep.attach_dir(std::path::Path::new(&root), std::path::Path::new(&dir));
    }

    if !matches!(cli.command, cli::Command::Wait(_)) {
        rep.start();
    }

    start_watchdog();
    install_panic_hook();

    let mut global = cli.global.clone();
    global.json = json;
    let mut ctx = Ctx::new(global, rep.clone(), argv);
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        if detaching {
            commands::detach::run(&mut ctx)
        } else {
            commands::dispatch(&mut ctx, cli.command)
        }
    }));

    let exit = match outcome {
        Ok(result) => rep.finish(result),
        Err(_) => {
            let message = PANIC
                .lock()
                .ok()
                .and_then(|message| message.clone())
                .unwrap_or_else(|| "unknown panic".to_string());
            rep.finish_panic(&message)
        }
    };
    FINISHED.store(true, Ordering::SeqCst);
    exit
}

/// Records panic messages for the exit-70 result; still prints them to
/// stderr (stdout carries only protocol lines or NDJSON).
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(mut slot) = PANIC.lock()
            && slot.is_none()
        {
            *slot = Some(info.to_string().replace('\n', " "));
        }
        previous(info);
    }));
}

/// The backstop for SIGINT/SIGTERM/SIGHUP when the main thread does not
/// notice the signal in time: stop the child groups, write the result,
/// exit 130.
fn start_watchdog() {
    let _ = std::thread::Builder::new()
        .name("icm-signal-watchdog".into())
        .spawn(|| {
            loop {
                std::thread::sleep(Duration::from_millis(100));
                if FINISHED.load(Ordering::SeqCst) {
                    return;
                }
                let Some(signal) = signals::pending() else {
                    continue;
                };

                // The runner normally handles it within one poll; step in
                // only when the main thread is busy elsewhere.
                let finished_within = |limit: Duration| {
                    let until = Instant::now() + limit;
                    while Instant::now() < until {
                        if FINISHED.load(Ordering::SeqCst) {
                            return true;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    false
                };
                if finished_within(Duration::from_secs(2)) {
                    return;
                }
                signals::kill_registered(libc::SIGTERM);
                if finished_within(Duration::from_secs(3)) {
                    return;
                }

                signals::kill_registered(libc::SIGKILL);
                let exit = Reporter::active()
                    .and_then(|rep| rep.try_finish_interrupted(signal, Duration::from_secs(2)))
                    .unwrap_or(Exit::Interrupted);
                std::process::exit(i32::from(exit.code()));
            }
        });
}

/// `-q`, `--quiet`, or a cluster of short flags containing `q` (`-qv`).
fn is_quiet_flag(arg: &str) -> bool {
    arg == "--quiet"
        || (arg.starts_with('-')
            && !arg.starts_with("--")
            && arg.len() > 1
            && arg[1..].chars().all(|c| c.is_ascii_alphabetic())
            && arg.contains('q'))
}

/// The `detail` of a usage error: clap's message without its usage and
/// help footer.
pub fn usage_detail(rendered: &str) -> String {
    let lines: Vec<&str> = rendered
        .lines()
        .map(str::trim)
        .take_while(|line| !line.starts_with("Usage:") && !line.starts_with("For more information"))
        .filter(|line| !line.trim().is_empty())
        .collect();
    let text = lines.join(" ").replace("  ", " ");
    text.trim().trim_start_matches("error: ").to_string()
}

/// A parse failure: help and version print and exit 0; anything else is a
/// usage error with a result object (exit 2).
fn usage_error(error: &clap::Error, argv: &[String]) -> Exit {
    use clap::error::ErrorKind;
    if matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        let _ = error.print();
        return Exit::Ok;
    }

    let args = &argv[1.min(argv.len())..];
    let json = args.iter().any(|a| a == "--json") || env_flag("ICM_JSON");
    let quiet = args.iter().any(|a| is_quiet_flag(a));
    let command = args
        .iter()
        .find(|arg| !arg.starts_with('-'))
        .cloned()
        .unwrap_or_else(|| "icm".to_string());

    let rep = Reporter::new(
        Mode {
            json,
            quiet,
            ..Mode::default()
        },
        RunInfo {
            run: rundir::new_run_id("usage", None),
            command,
            target: None,
            argv: argv.to_vec(),
            save: false,
        },
    );
    rep.start();

    if !json {
        let _ = error.print();
    }

    let rendered = error.render().to_string();
    let detail = if error.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand {
        "a command is required; see `icm --help`".to_string()
    } else {
        usage_detail(&rendered)
    };
    rep.finish(Err(IcmError::new(CheckId::UsageBadArgs, detail)
        .fix("Read the command's help.", &["icm --help"])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_details_drop_the_footer() {
        let rendered = "error: invalid value 'nowhere' for '<PLATFORM>'\n  [possible values: desktop, web, ios-sim, ios-device, android]\n\nFor more information, try '--help'.\n";
        assert_eq!(
            usage_detail(rendered),
            "invalid value 'nowhere' for '<PLATFORM>' [possible values: desktop, web, ios-sim, ios-device, android]"
        );
    }
}
