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
pub mod grep;
pub mod harness;
pub mod hash;
pub mod hooks;
pub mod host;
pub mod image;
pub mod ios;
pub mod locks;
pub mod managed;
pub mod output;
pub mod paths;
pub mod pinned;
pub mod plan;
pub mod platform;
pub mod policy;
pub mod preview;
pub mod process;
pub mod raster;
pub mod release;
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

/// How long the watchdog waits for a cleanup in progress
/// ([`signals::cleanup`]): longer than any stop grace plus its SIGKILL wait.
const CLEANUP_PATIENCE: Duration = Duration::from_secs(20);
static PANIC: Mutex<Option<String>> = Mutex::new(None);

/// icm's entry point; returns the process exit code.
pub fn main() -> std::process::ExitCode {
    signals::install();
    let argv: Vec<String> = std::env::args_os()
        .map(|arg: OsString| arg.to_string_lossy().into_owned())
        .collect();

    let exit = match Cli::try_parse_from(&argv) {
        Ok(cli) => match planned_help(&cli, &argv) {
            Some(exit) => exit,
            None => run(cli, argv),
        },
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
    let detached_child = env_flag(commands::detach::DETACHED_ENV);
    let detaching = cli.global.detach && !cli.command.is_view() && !detached_child;
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

    // A detached child continues the run directory its parent created
    // (`ICM_RUN_ID`, `ICM_RUN_DIR`, `ICM_RUN_ROOT`, `ICM_DETACHED`). Read
    // the plumbing once and take it out of the environment, so no child
    // (a hook, an app, a session host) inherits it: an icm started by a
    // hook would otherwise take over this run's directory and its result.
    let plumbing = take_run_plumbing();
    let inherited = plumbing.as_ref().map(|plumbing| plumbing.id.clone());
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

    if let Some(plumbing) = &plumbing {
        let _ = rep.attach_dir(&plumbing.root, &plumbing.dir);
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
        // A command that failed after a signal failed because of it (a
        // child icm killed looks like a failed child): exit 130 whatever
        // the command made of it. A command that ended well after one (a
        // `--follow` ended with Ctrl-C) keeps its result.
        Ok(result) => match (result, signals::pending()) {
            (Err(error), Some(signal)) => rep.finish_interrupted(signal, error),
            (result, _) => rep.finish(result),
        },
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

/// The run a detached child continues.
struct RunPlumbing {
    id: String,
    dir: std::path::PathBuf,
    root: std::path::PathBuf,
}

/// The variables a detached parent hands its child.
const RUN_PLUMBING: [&str; 4] = [
    "ICM_RUN_ID",
    "ICM_RUN_DIR",
    "ICM_RUN_ROOT",
    commands::detach::DETACHED_ENV,
];

/// Reads the detached run's plumbing and removes it from icm's
/// environment. Only a detached child (`ICM_DETACHED=1` with a valid id
/// and both directories) continues a run, and never one that already has
/// its `result.json`: that run has ended, so this icm starts its own.
fn take_run_plumbing() -> Option<RunPlumbing> {
    let detached = env_flag(commands::detach::DETACHED_ENV);
    let id = std::env::var("ICM_RUN_ID").ok();
    let dir = std::env::var_os("ICM_RUN_DIR").map(std::path::PathBuf::from);
    let root = std::env::var_os("ICM_RUN_ROOT").map(std::path::PathBuf::from);
    for name in RUN_PLUMBING {
        // SAFETY: called from `run` on the main thread before icm starts
        // any thread (the watchdog, the runner's readers), so nothing reads
        // the environment concurrently.
        unsafe { std::env::remove_var(name) };
    }

    let (id, dir, root) = (id?, dir?, root?);
    (detached && rundir::is_run_id(&id) && !dir.join("result.json").exists())
        .then_some(RunPlumbing { id, dir, root })
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
                // only when the main thread is busy elsewhere. While the
                // main thread is stopping what it started (an app gets its
                // SIGTERM grace, then SIGKILL, then its session file is
                // removed: `signals::cleanup`), wait for it rather than
                // exit in the middle, up to CLEANUP_PATIENCE in all.
                let patience = Instant::now() + CLEANUP_PATIENCE;
                let finished_within = |limit: Duration| {
                    let until = Instant::now() + limit;
                    loop {
                        if FINISHED.load(Ordering::SeqCst) {
                            return true;
                        }
                        let now = Instant::now();
                        if now >= until && !(signals::cleaning() && now < patience) {
                            return false;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
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

/// The command `argv` names, for a command line clap refused: the first
/// word that is neither an option nor an option's value. Which options take
/// a value comes from clap's definition, so in `icm --config x.toml run`
/// the command is `run`, not `x.toml`. An option clap does not define (a
/// misspelled `--conf`) may take one too, so the word after it is the
/// command only when it names one: `icm --conf x.toml run` names `run`. An
/// alias gives the command's name; another word that names no command (an
/// external one) is returned as written.
fn named_subcommand(argv: &[String]) -> Option<String> {
    use clap::CommandFactory;
    fn long<'a>(icm: &'a clap::Command, name: &str) -> Option<&'a clap::Arg> {
        icm.get_arguments().find(|option| {
            option.get_long() == Some(name)
                || option
                    .get_all_aliases()
                    .is_some_and(|aliases| aliases.contains(&name))
        })
    }
    fn short(icm: &clap::Command, name: char) -> Option<&clap::Arg> {
        icm.get_arguments()
            .find(|option| option.get_short() == Some(name))
    }
    let takes_value = |option: &clap::Arg| option.get_action().takes_values();
    let mut icm = Cli::command();
    icm.build();
    let mut words = argv.iter().skip(1);
    // Whether the word before was an option clap does not define.
    let mut after_unknown = false;
    while let Some(word) = words.next() {
        let maybe_value = std::mem::take(&mut after_unknown);
        if word == "--" {
            return words.next().cloned();
        }
        if let Some(name) = word.strip_prefix("--") {
            // `--name=value` carries its value.
            if !name.contains('=') {
                match long(&icm, name) {
                    Some(option) if takes_value(option) => {
                        let _ = words.next();
                    }
                    Some(_) => {}
                    None => after_unknown = true,
                }
            }
            continue;
        }
        if let Some(shorts) = word.strip_prefix('-').filter(|shorts| !shorts.is_empty()) {
            // A cluster (`-qv`): the first short option that takes a value
            // takes the rest of the word, or else the next word.
            for (at, name) in shorts.char_indices() {
                match short(&icm, name) {
                    Some(option) if takes_value(option) => {
                        if at + name.len_utf8() == shorts.len() {
                            let _ = words.next();
                        }
                        break;
                    }
                    Some(_) => {}
                    None => {
                        after_unknown = true;
                        break;
                    }
                }
            }
            continue;
        }
        match icm.find_subcommand(word) {
            Some(command) => return Some(command.get_name().to_string()),
            // The value the unknown option before it may take.
            None if maybe_value => {}
            None => return Some(word.clone()),
        }
    }
    None
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

/// Whether `argv` asks for JSON (`--json` anywhere, or `ICM_JSON`) and for
/// quiet output, read before clap has parsed it.
fn json_and_quiet(argv: &[String]) -> (bool, bool) {
    let args = &argv[1.min(argv.len())..];
    (
        args.iter().any(|a| a == "--json") || env_flag("ICM_JSON"),
        args.iter().any(|a| is_quiet_flag(a)),
    )
}

/// Whether a parse failure is clap answering `--help` or `--version`.
fn is_help_or_version(error: &clap::Error) -> bool {
    use clap::error::ErrorKind;
    matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    )
}

/// `icm print plan <cmd…> --help`: clap hands the command after `print
/// plan` over unparsed, and `print plan` parses it only when it runs, where
/// its help or version would come back as a usage error. Answered here
/// instead, as the help of that command (exit 0, [`answer_help`]).
fn planned_help(cli: &Cli, argv: &[String]) -> Option<Exit> {
    let cli::Command::Print(cli::PrintArgs {
        what: cli::PrintWhat::Plan { command },
    }) = &cli.command
    else {
        return None;
    };
    // As `print plan` parses it (commands::print).
    let planned: Vec<String> = std::iter::once("icm".to_string())
        .chain(command.iter().cloned())
        .chain(std::iter::once("--dry-run".to_string()))
        .collect();
    let error = Cli::try_parse_from(&planned).err()?;
    is_help_or_version(&error).then(|| answer_help(&error, argv, named_subcommand(&planned)))
}

/// Help or version: clap's text, exit 0; under `--json` a result object
/// ([`help_or_version`]). `subcommand` is the command the help is of.
fn answer_help(error: &clap::Error, argv: &[String], subcommand: Option<String>) -> Exit {
    let (json, quiet) = json_and_quiet(argv);
    if !json {
        let _ = error.print();
        return Exit::Ok;
    }
    help_or_version(error, argv, subcommand, quiet)
}

/// A parse failure: help and version exit 0 (under `--json` as a result
/// object, [`help_or_version`]); anything else is a usage error with a
/// result object (exit 2).
fn usage_error(error: &clap::Error, argv: &[String]) -> Exit {
    use clap::error::ErrorKind;
    let (json, quiet) = json_and_quiet(argv);
    let subcommand = named_subcommand(argv);

    if is_help_or_version(error) {
        return answer_help(error, argv, subcommand);
    }

    let command = subcommand.unwrap_or_else(|| "icm".to_string());

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

/// `--help` or `--version` under `--json`: stdout stays NDJSON whose last
/// line is the result (design §4), and the text clap would have printed is
/// the result's `help` or `version` (exit 0). `command` is `help` or
/// `version`; a subcommand's help names it in `target`.
fn help_or_version(
    error: &clap::Error,
    argv: &[String],
    subcommand: Option<String>,
    quiet: bool,
) -> Exit {
    let version = error.kind() == clap::error::ErrorKind::DisplayVersion;
    let (command, target) = if version {
        ("version", None)
    } else {
        ("help", subcommand)
    };
    let rep = Reporter::new(
        Mode {
            json: true,
            quiet,
            ..Mode::default()
        },
        RunInfo {
            run: rundir::new_run_id(command, target.as_deref()),
            command: command.to_string(),
            target: target.clone(),
            argv: argv.to_vec(),
            save: false,
        },
    );
    rep.start();
    // `Display` drops clap's styling.
    let text = error.render().to_string().trim_end().to_string();
    if version {
        rep.set("version", serde_json::json!(text));
        rep.summary(text);
    } else {
        rep.set("help", serde_json::json!(text));
        rep.summary(match target {
            Some(target) => format!("the help of `icm {target}`"),
            None => "icm's help".to_string(),
        });
    }
    rep.finish(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A global option's value is not the command, however the options are
    /// written and ordered.
    #[test]
    fn the_command_skips_option_values() {
        let named = |args: &[&str]| {
            let argv: Vec<String> = std::iter::once("icm")
                .chain(args.iter().copied())
                .map(str::to_string)
                .collect();
            named_subcommand(&argv)
        };
        let run = Some("run".to_string());
        assert_eq!(named(&["run", "--help"]), run);
        assert_eq!(named(&["--config", "x.toml", "run", "--help"]), run);
        assert_eq!(named(&["--config=x.toml", "run"]), run);
        assert_eq!(named(&["--timeout", "90s", "-q", "run"]), run);
        assert_eq!(named(&["--color", "never", "--json", "run"]), run);
        assert_eq!(named(&["--wait-lock", "5s", "-qv", "run"]), run);
        // `--config run`: `run` is the path, `stop` the command.
        assert_eq!(
            named(&["--config", "run", "stop", "-h"]),
            Some("stop".to_string())
        );
        assert_eq!(named(&["--", "run"]), run);
        // An option clap does not define (a misspelled `--config`) may take
        // a value: the word after it is not the command unless it names
        // one.
        assert_eq!(named(&["--conf", "x.toml", "run", "--help"]), run);
        assert_eq!(named(&["--timout", "90s", "-q", "run"]), run);
        assert_eq!(named(&["-Z", "x.toml", "run"]), run);
        assert_eq!(named(&["-qZ", "x.toml", "run"]), run);
        assert_eq!(named(&["--conf", "run", "--help"]), run);
        assert_eq!(named(&["--conf=x.toml", "run"]), run);
        assert_eq!(named(&["--conf", "x.toml"]), None);
        // An unknown word is still what was asked for.
        assert_eq!(
            named(&["--timeout", "1m", "clean"]),
            Some("clean".to_string())
        );
        assert_eq!(named(&["--timeout", "90s", "--json"]), None);
        assert_eq!(named(&[]), None);
    }

    #[test]
    fn usage_details_drop_the_footer() {
        let rendered = "error: invalid value 'nowhere' for '<PLATFORM>'\n  [possible values: desktop, web, ios-sim, ios-device, android]\n\nFor more information, try '--help'.\n";
        assert_eq!(
            usage_detail(rendered),
            "invalid value 'nowhere' for '<PLATFORM>' [possible values: desktop, web, ios-sim, ios-device, android]"
        );
    }
}
