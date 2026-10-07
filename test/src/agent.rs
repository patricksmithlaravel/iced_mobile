//! The harness that the `icm` tool, or any agent, drives to see and test an
//! application with no window, device or GPU: headless screenshots, the
//! widget tree, and `.ice` flows.
//!
//! An application declares a test target without libtest's harness,
//!
//! ```toml
//! [[test]]
//! name = "icm"
//! path = "tests/icm.rs"
//! harness = false
//! ```
//!
//! and hands its program to [`main`] in `tests/icm.rs`:
//!
//! ```rust,ignore
//! fn main() -> std::process::ExitCode {
//!     iced_test::agent::main(my_app::application(), env!("CARGO_MANIFEST_DIR"))
//! }
//! ```
//!
//! Plain `cargo test` then runs every flow in `tests/flows/*.ice`, and
//! `cargo test --test icm -- <command> [options]` runs one command:
//!
//! | Command | Does |
//! |---|---|
//! | `icm-shot [--viewport V] [--scale F] [--theme light\|dark] [--preset NAME] [--wait-ms N] --out PATH` | writes a PNG of the view |
//! | `icm-tree [--viewport V] [--preset NAME] [--wait-ms N] [--out PATH]` | writes the widget tree as JSON |
//! | `icm-ice FILE [--report PATH] [--timeout-ms N]` | runs one `.ice` flow, and writes a result for each instruction |
//!
//! - `V` is `WIDTHxHEIGHT` in logical pixels (`402x874`), or a device
//!   preset: `iphone-17` (402×874 at scale 3, the default), `iphone-se`
//!   (375×667 at 2), `pixel-9` (412×915 at 2.625), `web-mobile` (390×844 at
//!   3) or `desktop` (1024×768 at 1). `--scale` defaults to the preset's, or
//!   to 1.
//! - `--theme` is the system's light or dark mode, which the program sees
//!   when it does not choose a theme itself (default `light`).
//! - A viewport the size of a device preset gets the safe area that
//!   device's shell reports, through `iced::mobile::safe_area()` (the
//!   runtime's `safe_area` module), before the program boots: `iphone-17`
//!   62 top and 34 bottom, `iphone-se` 20 top, `pixel-9` 54.1 top and 24
//!   bottom (icm's `pixel_9` emulator), and zero at `web-mobile` and
//!   `desktop`, as the web and the desktop report. A viewport of any other
//!   size gets none, and the program keeps whatever padding it uses until
//!   one arrives. `.ice` flows follow the same rule with their `viewport:`
//!   line.
//! - `--preset` boots the program in one of its
//!   [`Preset`](crate::program::Preset)s instead of its usual state.
//! - `--wait-ms` lets the tasks the program starts at boot run for that
//!   long before the screenshot or the tree is taken (default 500).
//! - Relative paths are relative to the package's directory, where Cargo
//!   runs tests.
//!
//! # Output (protocol 1)
//!
//! The first line on stdout is `ICM_HARNESS {"protocol":1}`. A command's last
//! line is `ICM_HARNESS_RESULT <json>`, one JSON object with `"protocol":1`,
//! `"kind"` (`shot`, `tree`, `ice` or `flows`) and `"ok"`. The file
//! `icm-ice --report` writes holds the same object, and so does the one
//! `icm-tree --out` writes, with the widgets.
//!
//! - `shot`: `out`, `size` (in physical pixels), `viewport`, `scale`,
//!   `theme` (the mode of the theme drawn), `backend`.
//! - `tree`: `viewport`, `widgets`: every widget a selector can see, in
//!   depth-first order, each with `kind` (`container`, `focusable`,
//!   `scrollable`, `text_input`, `text` or `custom`), `id` (the name given
//!   with `.id("...")`, or `null`), `text`, `bounds` and `visible` (`[x, y,
//!   width, height]` in logical pixels, `visible` being where it is on screen
//!   or `null` when it is scrolled or clipped away), and `focused` for text
//!   inputs and focusables. With `--out`, the line has `out` and the number
//!   of widgets instead of the widgets.
//! - `ice`: `name`, `file`, `passed`, `ms`, `error` (why the flow could not
//!   run, or `null`) and `steps`: for each instruction, `line`,
//!   `instruction`, `status` (`passed`, `failed` or `skipped`) and `ms`; a
//!   failed one adds `reason` and `texts`, the texts visible when it failed.
//! - `flows`: `passed` and `failed` counts, and `flows`, one `ice` result
//!   per flow.
//!
//! The exit code is 0 when everything passed, 1 when a flow failed, and 2
//! for a usage error or a file that cannot be read or written.
//!
//! # Determinism
//!
//! The harness draws with `tiny-skia`, on the CPU, unless
//! `ICED_TEST_BACKEND` names another backend, so a screenshot is the same on
//! every machine and needs no GPU. The default font (`Font::DEFAULT`) is
//! Fira Sans, as on Android, iOS and the web with iced's default features;
//! text the embedded fonts lack still falls back to the host's fonts.
//!
//! Flows run as [`run`](crate::run) runs them, in an [`Emulator`]: tasks
//! and subscriptions are real, so side effects happen. A flow that does not
//! finish within `--timeout-ms` (default 30000) fails at the instruction it
//! was running.
use crate::Ice;
use crate::core::theme;
use crate::core::widget;
use crate::core::window;
use crate::core::{Padding, Rectangle, Size, Vector};
use crate::emulator::{self, Emulator};
use crate::futures::futures::channel::mpsc;
use crate::instruction::{self, Expectation, Instruction};
use crate::program::Program;
use crate::runtime::safe_area::{self, SafeArea};
use crate::selector::Candidate;

use std::env;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

/// The version of the harness protocol, printed first as
/// `ICM_HARNESS {"protocol":1}`.
pub const PROTOCOL: u32 = 1;

/// The device presets `--viewport` accepts: name, logical size and scale.
pub const VIEWPORTS: &[(&str, Size, f32)] = &[
    ("iphone-17", Size::new(402.0, 874.0), 3.0),
    ("iphone-se", Size::new(375.0, 667.0), 2.0),
    ("pixel-9", Size::new(412.0, 915.0), 2.625),
    ("web-mobile", Size::new(390.0, 844.0), 3.0),
    ("desktop", Size::new(1024.0, 768.0), 1.0),
];

/// The safe area each device preset of [`VIEWPORTS`] reports, in logical
/// pixels: the status bar, the notch, Dynamic Island or display cutout, and
/// the home indicator or navigation bar. The web and the desktop report
/// zero.
const SAFE_AREAS: &[(&str, Padding)] = &[
    // The iPhone 17 simulator (iOS 27): Dynamic Island, home indicator.
    (
        "iphone-17",
        Padding {
            top: 62.0,
            right: 0.0,
            bottom: 34.0,
            left: 0.0,
        },
    ),
    // The status bar; a home button, so nothing at the bottom.
    (
        "iphone-se",
        Padding {
            top: 20.0,
            right: 0.0,
            bottom: 0.0,
            left: 0.0,
        },
    ),
    // icm's `pixel_9` emulator (API 36, gesture navigation): a 142 px status
    // bar and a 63 px navigation bar at 2.625.
    (
        "pixel-9",
        Padding {
            top: 142.0 / 2.625,
            right: 0.0,
            bottom: 63.0 / 2.625,
            left: 0.0,
        },
    ),
    ("web-mobile", Padding::ZERO),
    ("desktop", Padding::ZERO),
];

/// The safe area of the device preset of the same size as `viewport`, if
/// there is one.
fn device_safe_area(viewport: Size) -> Option<SafeArea> {
    let (name, _, _) =
        VIEWPORTS.iter().find(|(_, size, _)| *size == viewport)?;

    SAFE_AREAS
        .iter()
        .find(|(device, _)| device == name)
        .map(|(_, insets)| SafeArea::new(*insets))
}

/// Gives the program the safe area of the device `viewport` stands for, as
/// that device's shell would, before it boots, so that its subscriptions
/// start with it. A viewport of another size gets none.
fn publish_safe_area(viewport: Size) {
    safe_area::reset();

    if let Some(area) = device_safe_area(viewport) {
        safe_area::publish(area);
    }
}

const DEFAULT_WAIT: Duration = Duration::from_millis(500);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Runs the harness for the `program`, with the arguments of the process;
/// see the [module documentation](self).
///
/// `manifest_dir` is the application package's directory
/// (`env!("CARGO_MANIFEST_DIR")`), where the flows are in `tests/flows`.
pub fn main<P>(program: P, manifest_dir: impl AsRef<Path>) -> ExitCode
where
    P: Program + 'static,
{
    let args: Vec<String> = env::args().skip(1).collect();

    run(&program, manifest_dir, &args)
}

/// Runs the harness for the `program` like [`main`], with the given
/// arguments instead of those of the process: `args` holds what follows
/// `--` in `cargo test --test icm -- <args>`.
pub fn run<P>(
    program: &P,
    manifest_dir: impl AsRef<Path>,
    args: &[String],
) -> ExitCode
where
    P: Program + 'static,
{
    let flows_dir = manifest_dir.as_ref().join("tests").join("flows");

    // `cargo nextest` lists the tests of a binary before running them, and
    // parses that list: nothing else may be printed.
    if args.iter().any(|arg| arg == "--list") {
        let filter = Filter::parse(args);

        for name in flow_files(&flows_dir)
            .unwrap_or_default()
            .iter()
            .map(|file| flow_name(file))
            .filter(|name| filter.matches(name))
        {
            println!("{name}: test");
        }

        return ExitCode::SUCCESS;
    }

    println!("ICM_HARNESS {{\"protocol\":{PROTOCOL}}}");

    prepare_fonts();

    let backend = env::var("ICED_TEST_BACKEND")
        .ok()
        .filter(|backend| !backend.trim().is_empty())
        .unwrap_or_else(|| String::from("tiny-skia"));

    let result = match args.first().map(String::as_str) {
        Some("icm-shot") => shot(program, &args[1..], &backend),
        Some("icm-tree") => tree(program, &args[1..], &backend),
        Some("icm-ice") => ice(program, &args[1..], &backend),
        Some(command) if command.starts_with("icm-") => Err(format!(
            "unknown command {command:?}; the commands are icm-shot, \
            icm-tree and icm-ice"
        )),
        _ => Ok(flows(program, &flows_dir, args, &backend)),
    };

    match result {
        Ok(Output { json, passed }) => {
            println!("ICM_HARNESS_RESULT {json}");

            if passed {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(error) => {
            eprintln!("error: {error}");

            let mut json = Json::object();
            let _ = json
                .number("protocol", PROTOCOL)
                .bool("ok", false)
                .string("error", &error);

            println!("ICM_HARNESS_RESULT {}", json.finish());

            ExitCode::from(2)
        }
    }
}

/// What a command prints last.
struct Output {
    json: String,
    passed: bool,
}

/// `icm-shot`
fn shot<P>(
    program: &P,
    args: &[String],
    backend: &str,
) -> Result<Output, String>
where
    P: Program + 'static,
{
    let args = Args::parse(
        args,
        &["viewport", "scale", "theme", "preset", "wait-ms", "out"],
    )?;

    let (viewport, preset_scale) = viewport(args.get("viewport"))?;

    let scale = match args.get("scale") {
        Some(scale) => scale
            .parse::<f32>()
            .ok()
            .filter(|scale| scale.is_finite() && *scale > 0.0)
            .ok_or_else(|| format!("invalid --scale {scale:?}"))?,
        None => preset_scale,
    };

    let mode = match args.get("theme").unwrap_or("light") {
        "light" => theme::Mode::Light,
        "dark" => theme::Mode::Dark,
        other => {
            return Err(format!(
                "invalid --theme {other:?}; it is light or dark"
            ));
        }
    };

    let out = PathBuf::from(
        args.get("out")
            .ok_or("icm-shot needs --out PATH, the PNG to write")?,
    );

    let wait = args.millis("wait-ms")?.unwrap_or(DEFAULT_WAIT);

    let mut session =
        Session::boot(program, viewport, args.get("preset"), backend)?;
    session.settle(program, wait);

    let theme = session
        .emulator
        .theme(program)
        .unwrap_or_else(|| <P::Theme as theme::Base>::default(mode));

    let screenshot = session.emulator.screenshot(program, &theme, scale);

    write_png(&out, &screenshot)
        .map_err(|error| format!("cannot write {}: {error}", out.display()))?;

    let mut json = Json::object();
    let _ = json
        .number("protocol", PROTOCOL)
        .string("kind", "shot")
        .bool("ok", true)
        .string("out", &out.display().to_string())
        .raw(
            "size",
            &format!("[{},{}]", screenshot.size.width, screenshot.size.height),
        )
        .raw("viewport", &size(viewport))
        .float("scale", scale)
        .string(
            "theme",
            match theme::Base::mode(&theme) {
                theme::Mode::Dark => "dark",
                theme::Mode::Light | theme::Mode::None => "light",
            },
        )
        .string("backend", backend);

    Ok(Output {
        json: json.finish(),
        passed: true,
    })
}

/// `icm-tree`
fn tree<P>(
    program: &P,
    args: &[String],
    backend: &str,
) -> Result<Output, String>
where
    P: Program + 'static,
{
    let args = Args::parse(args, &["viewport", "preset", "wait-ms", "out"])?;
    let (viewport, _scale) = viewport(args.get("viewport"))?;
    let wait = args.millis("wait-ms")?.unwrap_or(DEFAULT_WAIT);

    let mut session =
        Session::boot(program, viewport, args.get("preset"), backend)?;
    session.settle(program, wait);

    let nodes = widgets(&mut session.emulator, program);

    let mut widgets = String::from("[");

    for (i, node) in nodes.iter().enumerate() {
        if i > 0 {
            widgets.push(',');
        }

        widgets.push_str(&node.to_json());
    }

    widgets.push(']');

    let mut json = Json::object();
    let _ = json
        .number("protocol", PROTOCOL)
        .string("kind", "tree")
        .bool("ok", true)
        .raw("viewport", &size(viewport));

    match args.get("out") {
        Some(out) => {
            let out = PathBuf::from(out);

            let mut file = json.clone();
            let _ = file.raw("widgets", &widgets);

            write_file(&out, &(file.finish() + "\n")).map_err(|error| {
                format!("cannot write {}: {error}", out.display())
            })?;

            let _ = json
                .string("out", &out.display().to_string())
                .number("widgets", nodes.len());
        }
        None => {
            let _ = json.raw("widgets", &widgets);
        }
    }

    Ok(Output {
        json: json.finish(),
        passed: true,
    })
}

/// `icm-ice`
fn ice<P>(program: &P, args: &[String], backend: &str) -> Result<Output, String>
where
    P: Program + 'static,
{
    let args = Args::parse(args, &["report", "timeout-ms"])?;

    let [file] = args.positional.as_slice() else {
        return Err(String::from(
            "icm-ice needs exactly one .ice file: icm-ice FILE [--report \
            PATH] [--timeout-ms N]",
        ));
    };

    let file = PathBuf::from(file);

    if !file.is_file() {
        return Err(format!("no flow at {}", file.display()));
    }

    let timeout = args.millis("timeout-ms")?.unwrap_or(DEFAULT_TIMEOUT);
    let report = run_flow(program, &file, backend, timeout);

    let mut json = Json::object();
    let _ = json
        .number("protocol", PROTOCOL)
        .string("kind", "ice")
        .bool("ok", report.passed());
    let json = report.to_json(json);

    if let Some(path) = args.get("report") {
        let path = Path::new(path);

        write_file(path, &format!("{json}\n")).map_err(|error| {
            format!("cannot write {}: {error}", path.display())
        })?;
    }

    Ok(Output {
        passed: report.passed(),
        json,
    })
}

/// Plain `cargo test`: every flow in `tests/flows`.
fn flows<P>(
    program: &P,
    directory: &Path,
    args: &[String],
    backend: &str,
) -> Output
where
    P: Program + 'static,
{
    let started = Instant::now();
    let filter = Filter::parse(args);

    let timeout = args
        .iter()
        .position(|arg| arg == "--timeout-ms")
        .and_then(|i| args.get(i + 1))
        .and_then(|value| value.parse().ok())
        .map_or(DEFAULT_TIMEOUT, Duration::from_millis);

    let files: Vec<PathBuf> = flow_files(directory)
        .unwrap_or_default()
        .into_iter()
        .filter(|file| filter.matches(&flow_name(file)))
        .collect();

    println!();
    println!(
        "running {} flow{} from {}",
        files.len(),
        if files.len() == 1 { "" } else { "s" },
        directory.display()
    );

    let mut reports = Vec::new();

    for file in &files {
        let report = run_flow(program, file, backend, timeout);

        println!(
            "flow {} ... {} ({} ms)",
            flow_name(file),
            if report.passed() { "ok" } else { "FAILED" },
            report.ms
        );

        reports.push(report);
    }

    let failed: Vec<&Report> =
        reports.iter().filter(|report| !report.passed()).collect();

    if !failed.is_empty() {
        println!();
        println!("failures:");

        for report in &failed {
            println!();
            println!("---- {} ({}) ----", report.name, report.file.display());

            if let Some(error) = &report.error {
                println!("{error}");
            }

            for step in &report.steps {
                if let Status::Failed = step.status {
                    println!(
                        "line {}: {} failed: {}",
                        step.line,
                        step.instruction,
                        step.reason.as_deref().unwrap_or("unknown reason")
                    );

                    let texts: Vec<String> = step
                        .texts
                        .iter()
                        .map(|text| format!("{text:?}"))
                        .collect();

                    println!("visible texts: {}", texts.join(", "));
                }
            }
        }
    }

    let passed = reports.len() - failed.len();

    println!();
    println!(
        "test result: {}. {passed} passed; {} failed; finished in {:.2}s",
        if failed.is_empty() { "ok" } else { "FAILED" },
        failed.len(),
        started.elapsed().as_secs_f32()
    );
    println!();

    let mut flows = String::from("[");

    for (i, report) in reports.iter().enumerate() {
        if i > 0 {
            flows.push(',');
        }

        flows.push_str(&report.to_json(Json::object()));
    }

    flows.push(']');

    let mut json = Json::object();
    let _ = json
        .number("protocol", PROTOCOL)
        .string("kind", "flows")
        .bool("ok", failed.is_empty())
        .number("passed", passed)
        .number("failed", failed.len())
        .raw("flows", &flows);

    Output {
        json: json.finish(),
        passed: failed.is_empty(),
    }
}

/// An [`Emulator`] and the receiving end of its events.
struct Session<P: Program> {
    emulator: Emulator<P>,
    receiver: mpsc::Receiver<emulator::Event<P>>,
}

impl<P> Session<P>
where
    P: Program + 'static,
{
    fn boot(
        program: &P,
        viewport: Size,
        preset: Option<&str>,
        backend: &str,
    ) -> Result<Self, String> {
        let preset = crate::preset(program, preset)
            .map_err(|error| error.to_string())?;

        let (sender, receiver) = mpsc::channel(100);

        publish_safe_area(viewport);

        let emulator = Emulator::with_backend(
            sender,
            program,
            emulator::Mode::Immediate,
            viewport,
            preset,
            Some(backend),
        );

        Ok(Self { emulator, receiver })
    }

    /// Performs what the program asks for, for `duration`.
    fn settle(&mut self, program: &P, duration: Duration) {
        let deadline = Instant::now() + duration;

        loop {
            while let Ok(Some(event)) = crate::try_next(&mut self.receiver) {
                if let emulator::Event::Action(action) = event {
                    self.emulator.perform(program, action);
                }
            }

            if Instant::now() >= deadline {
                break;
            }

            thread::sleep(Duration::from_millis(1));
        }
    }
}

/// The outcome of a flow.
struct Report {
    name: String,
    file: PathBuf,
    error: Option<String>,
    steps: Vec<Step>,
    ms: u128,
}

impl Report {
    fn passed(&self) -> bool {
        self.error.is_none()
            && self
                .steps
                .iter()
                .all(|step| matches!(step.status, Status::Passed))
    }

    fn to_json(&self, mut json: Json) -> String {
        let mut steps = String::from("[");

        for (i, step) in self.steps.iter().enumerate() {
            if i > 0 {
                steps.push(',');
            }

            let mut object = Json::object();
            let _ = object
                .number("line", step.line)
                .string("instruction", &step.instruction)
                .string(
                    "status",
                    match step.status {
                        Status::Passed => "passed",
                        Status::Failed => "failed",
                        Status::Skipped => "skipped",
                    },
                )
                .number("ms", step.ms);

            if let Status::Failed = step.status {
                let _ = object
                    .optional_string("reason", step.reason.as_deref())
                    .raw("texts", &strings(&step.texts));
            }

            steps.push_str(&object.finish());
        }

        steps.push(']');

        let _ = json
            .string("name", &self.name)
            .string("file", &self.file.display().to_string())
            .bool("passed", self.passed())
            .number("ms", self.ms)
            .optional_string("error", self.error.as_deref())
            .raw("steps", &steps);

        json.finish()
    }
}

struct Step {
    line: usize,
    instruction: String,
    status: Status,
    ms: u128,
    reason: Option<String>,
    texts: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Passed,
    Failed,
    Skipped,
}

/// Runs the flow in `file`, step by step.
fn run_flow<P>(
    program: &P,
    file: &Path,
    backend: &str,
    timeout: Duration,
) -> Report
where
    P: Program + 'static,
{
    let started = Instant::now();

    let mut report = Report {
        name: flow_name(file),
        file: file.to_path_buf(),
        error: None,
        steps: Vec::new(),
        ms: 0,
    };

    let content = match fs::read_to_string(file) {
        Ok(content) => content,
        Err(error) => {
            report.error = Some(format!("cannot read the flow: {error}"));
            return report;
        }
    };

    let ice = match Ice::parse(&content) {
        Ok(ice) => ice,
        Err(error) => {
            report.error = Some(format!("invalid flow: {error}"));
            return report;
        }
    };

    let preset = match crate::preset(program, ice.preset.as_deref()) {
        Ok(preset) => preset,
        Err(error) => {
            report.error = Some(error.to_string());
            return report;
        }
    };

    let first_line = first_instruction_line(&content);

    report.steps = ice
        .instructions
        .iter()
        .enumerate()
        .map(|(i, instruction)| Step {
            line: first_line + i,
            instruction: instruction.to_string(),
            status: Status::Skipped,
            ms: 0,
            reason: None,
            texts: Vec::new(),
        })
        .collect();

    let (sender, mut receiver) = mpsc::channel(100);

    publish_safe_area(ice.viewport);

    let mut emulator = Emulator::with_backend(
        sender,
        program,
        ice.mode,
        ice.viewport,
        preset,
        Some(backend),
    );

    let deadline = started + timeout;
    let mut current: Option<(usize, Instant)> = None;
    let mut next = 0;

    loop {
        let event = match crate::try_next(&mut receiver) {
            Ok(Some(event)) => event,
            Ok(None) => {
                report.error = Some(String::from("the emulator stopped"));
                break;
            }
            Err(_) => {
                if Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(1));
                    continue;
                }

                let reason =
                    format!("timed out after {} ms", timeout.as_millis());

                match current {
                    Some((i, at)) => {
                        let texts = visible_texts(&mut emulator, program);
                        let step = &mut report.steps[i];

                        step.status = Status::Failed;
                        step.ms = at.elapsed().as_millis();
                        step.reason = Some(reason);
                        step.texts = texts;
                    }
                    None => {
                        report.error =
                            Some(format!("the program did not boot: {reason}"));
                    }
                }

                break;
            }
        };

        match event {
            emulator::Event::Action(action) => {
                emulator.perform(program, action);
            }
            emulator::Event::Ready => {
                if let Some((i, at)) = current.take() {
                    let step = &mut report.steps[i];

                    step.status = Status::Passed;
                    step.ms = at.elapsed().as_millis();
                }

                let Some(instruction) = ice.instructions.get(next) else {
                    break;
                };

                current = Some((next, Instant::now()));
                next += 1;

                emulator.run(program, instruction.clone());
            }
            emulator::Event::Failed(instruction) => {
                let texts = visible_texts(&mut emulator, program);

                if let Some((i, at)) = current.take() {
                    let step = &mut report.steps[i];

                    step.status = Status::Failed;
                    step.ms = at.elapsed().as_millis();
                    step.reason = Some(failure(&instruction));
                    step.texts = texts;
                } else {
                    report.error = Some(failure(&instruction));
                }

                break;
            }
        }
    }

    report.ms = started.elapsed().as_millis();
    report
}

/// Why an instruction fails: the emulator finds no target for it.
fn failure(instruction: &Instruction) -> String {
    match instruction {
        Instruction::Expect(Expectation::Text(text)) => {
            format!("no widget shows the text {text:?}")
        }
        Instruction::Interact(instruction::Interaction::Mouse(_)) => {
            String::from(
                "its target is not in the view: no widget shows that text",
            )
        }
        Instruction::Interact(instruction::Interaction::Keyboard(_)) => {
            String::from("the keyboard interaction failed")
        }
    }
}

/// The texts of the view that are on screen, in depth-first order.
fn visible_texts<P>(emulator: &mut Emulator<P>, program: &P) -> Vec<String>
where
    P: Program + 'static,
{
    emulator.find_all(program, |candidate: Candidate<'_>| match candidate {
        Candidate::Text {
            content,
            visible_bounds: Some(_),
            ..
        } => Some(content.to_owned()),
        Candidate::TextInput {
            state,
            visible_bounds: Some(_),
            ..
        } => Some(state.text().to_owned()),
        _ => None,
    })
}

/// A widget of the tree.
#[derive(Debug, Clone, PartialEq)]
struct Node {
    kind: &'static str,
    id: Option<String>,
    text: Option<String>,
    focused: Option<bool>,
    bounds: Rectangle,
    visible: Option<Rectangle>,
    content_bounds: Option<Rectangle>,
    translation: Option<Vector>,
}

impl Node {
    fn new(candidate: Candidate<'_>) -> Self {
        let mut node = Self {
            kind: "custom",
            id: candidate.id().and_then(widget::Id::name).map(str::to_owned),
            text: None,
            focused: None,
            bounds: candidate.bounds(),
            visible: candidate.visible_bounds(),
            content_bounds: None,
            translation: None,
        };

        match candidate {
            Candidate::Container { .. } => {
                node.kind = "container";
            }
            Candidate::Focusable { state, .. } => {
                node.kind = "focusable";
                node.focused = Some(state.is_focused());
            }
            Candidate::Scrollable {
                content_bounds,
                translation,
                ..
            } => {
                node.kind = "scrollable";
                node.content_bounds = Some(content_bounds);
                node.translation = Some(translation);
            }
            Candidate::TextInput { state, .. } => {
                node.kind = "text_input";
                node.text = Some(state.text().to_owned());
            }
            Candidate::Text { content, .. } => {
                node.kind = "text";
                node.text = Some(content.to_owned());
            }
            Candidate::Custom { .. } => {}
        }

        node
    }

    fn to_json(&self) -> String {
        let mut json = Json::object();

        let _ = json
            .string("kind", self.kind)
            .optional_string("id", self.id.as_deref());

        if let Some(text) = &self.text {
            let _ = json.string("text", text);
        }

        if let Some(focused) = self.focused {
            let _ = json.bool("focused", focused);
        }

        let _ = json.raw("bounds", &rectangle(self.bounds)).raw(
            "visible",
            &self.visible.map_or(String::from("null"), rectangle),
        );

        if let Some(content_bounds) = self.content_bounds {
            let _ = json.raw("content_bounds", &rectangle(content_bounds));
        }

        if let Some(translation) = self.translation {
            let _ = json.raw(
                "translation",
                &format!("[{},{}]", float(translation.x), float(translation.y)),
            );
        }

        json.finish()
    }
}

/// Every widget a selector can see, in depth-first order.
///
/// A text input reports itself twice, as a text input and as a focusable
/// with the same bounds; the two become one node.
fn widgets<P>(emulator: &mut Emulator<P>, program: &P) -> Vec<Node>
where
    P: Program + 'static,
{
    let all = emulator.find_all(program, |candidate: Candidate<'_>| {
        Some(Node::new(candidate))
    });

    let mut nodes: Vec<Node> = Vec::with_capacity(all.len());

    for node in all {
        if node.kind == "focusable"
            && let Some(last) = nodes.last_mut()
            && last.kind == "text_input"
            && last.id == node.id
            && last.bounds == node.bounds
        {
            last.focused = node.focused;
            continue;
        }

        nodes.push(node);
    }

    nodes
}

/// Makes `Font::DEFAULT` draw with the embedded Fira Sans, as on Android,
/// iOS and the web, rather than with whatever the host has.
fn prepare_fonts() {
    crate::renderer::graphics::text::font_system()
        .write()
        .expect("Write to font system")
        .raw()
        .db_mut()
        .set_sans_serif_family("Fira Sans");
}

/// The `.ice` files of a directory, sorted by name.
fn flow_files(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(OsStr::to_str) == Some("ice"))
        .collect();

    files.sort();

    Ok(files)
}

/// `flows::<file stem>`, the name a flow has as a test.
fn flow_name(file: &Path) -> String {
    format!(
        "flows::{}",
        file.file_stem()
            .map(|stem| stem.to_string_lossy())
            .unwrap_or_default()
    )
}

/// The line (from 1) of the first instruction of an `.ice` file: the one
/// after the line of the first `-`, where [`Ice::parse`] ends the metadata.
fn first_instruction_line(content: &str) -> usize {
    let separator = content.find('-').unwrap_or_default();

    content[..separator].matches('\n').count() + 2
}

/// The test filters libtest takes, as `cargo test` passes them to every
/// test binary: names to run (`cargo test smoke`), `--skip NAME`, `--exact`
/// and `--ignored` (no flow is ignored). Other libtest flags are accepted
/// and ignored.
struct Filter {
    names: Vec<String>,
    skip: Vec<String>,
    exact: bool,
    ignored: bool,
}

impl Filter {
    fn parse(args: &[String]) -> Self {
        // libtest flags that take a value as the next argument.
        const WITH_VALUE: &[&str] = &[
            "--test-threads",
            "--color",
            "--format",
            "--logfile",
            "--report-time",
            "--timeout-ms",
            "-Z",
        ];

        let mut filter = Self {
            names: Vec::new(),
            skip: Vec::new(),
            exact: false,
            ignored: false,
        };

        let mut args = args.iter();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--skip" => filter.skip.extend(args.next().cloned()),
                "--exact" => filter.exact = true,
                "--ignored" => filter.ignored = true,
                arg if WITH_VALUE.contains(&arg) => {
                    let _ = args.next();
                }
                arg if arg.starts_with('-') => {}
                name => filter.names.push(name.to_owned()),
            }
        }

        filter
    }

    fn matches(&self, name: &str) -> bool {
        let matches = |filter: &String| {
            if self.exact {
                name == filter
            } else {
                name.contains(filter.as_str())
            }
        };

        !self.ignored
            && (self.names.is_empty() || self.names.iter().any(matches))
            && !self.skip.iter().any(matches)
    }
}

/// The options of a command: `--name value` or `--name=value`, and
/// positional arguments.
struct Args {
    options: Vec<(String, String)>,
    positional: Vec<String>,
}

impl Args {
    fn parse(args: &[String], known: &[&str]) -> Result<Self, String> {
        let mut options = Vec::new();
        let mut positional = Vec::new();
        let mut args = args.iter();

        while let Some(arg) = args.next() {
            let Some(option) = arg.strip_prefix("--") else {
                positional.push(arg.clone());
                continue;
            };

            let (name, value) = match option.split_once('=') {
                Some((name, value)) => (name, value.to_owned()),
                None => (
                    option,
                    args.next()
                        .cloned()
                        .ok_or_else(|| format!("--{option} needs a value"))?,
                ),
            };

            if !known.contains(&name) {
                return Err(format!(
                    "unknown option --{name}; this command takes {}",
                    known
                        .iter()
                        .map(|name| format!("--{name}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }

            options.push((name.to_owned(), value));
        }

        Ok(Self {
            options,
            positional,
        })
    }

    /// The last value given for an option.
    fn get(&self, name: &str) -> Option<&str> {
        self.options
            .iter()
            .rev()
            .find(|(option, _)| option == name)
            .map(|(_, value)| value.as_str())
    }

    fn millis(&self, name: &str) -> Result<Option<Duration>, String> {
        self.get(name)
            .map(|value| {
                value
                    .parse()
                    .map(Duration::from_millis)
                    .map_err(|_| format!("invalid --{name} {value:?}"))
            })
            .transpose()
    }
}

/// A `--viewport`: a device preset, or `WIDTHxHEIGHT` at scale 1.
fn viewport(value: Option<&str>) -> Result<(Size, f32), String> {
    let value = value.unwrap_or("iphone-17");

    if let Some((_, size, scale)) =
        VIEWPORTS.iter().find(|(name, _, _)| *name == value)
    {
        return Ok((*size, *scale));
    }

    value
        .split_once('x')
        .and_then(|(width, height)| {
            let width: f32 = width.trim().parse().ok()?;
            let height: f32 = height.trim().parse().ok()?;

            (width.is_finite()
                && height.is_finite()
                && width >= 1.0
                && height >= 1.0)
                .then(|| (Size::new(width, height), 1.0))
        })
        .ok_or_else(|| {
            format!(
                "invalid --viewport {value:?}; it is WIDTHxHEIGHT (402x874) \
                or one of {}",
                VIEWPORTS
                    .iter()
                    .map(|(name, _, _)| *name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

fn write_png(path: &Path, screenshot: &window::Screenshot) -> io::Result<()> {
    create_parent(path)?;

    let file = io::BufWriter::new(fs::File::create(path)?);

    let mut encoder =
        png::Encoder::new(file, screenshot.size.width, screenshot.size.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);

    let mut writer = encoder.write_header().map_err(io::Error::other)?;

    writer
        .write_image_data(&screenshot.rgba)
        .map_err(io::Error::other)?;

    writer.finish().map_err(io::Error::other)
}

fn write_file(path: &Path, content: &str) -> io::Result<()> {
    create_parent(path)?;

    fs::write(path, content)
}

fn create_parent(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            fs::create_dir_all(parent)
        }
        _ => Ok(()),
    }
}

/// A JSON object under construction, written on one line.
#[derive(Clone)]
struct Json(String);

impl Json {
    fn object() -> Self {
        Self(String::from("{"))
    }

    fn key(&mut self, key: &str) -> &mut Self {
        if self.0.len() > 1 {
            self.0.push(',');
        }

        self.0.push_str(&string(key));
        self.0.push(':');
        self
    }

    fn string(&mut self, key: &str, value: &str) -> &mut Self {
        let value = string(value);

        self.raw(key, &value)
    }

    fn optional_string(&mut self, key: &str, value: Option<&str>) -> &mut Self {
        match value {
            Some(value) => self.string(key, value),
            None => self.raw(key, "null"),
        }
    }

    fn number(
        &mut self,
        key: &str,
        value: impl std::fmt::Display,
    ) -> &mut Self {
        let value = value.to_string();

        self.raw(key, &value)
    }

    fn float(&mut self, key: &str, value: f32) -> &mut Self {
        self.raw(key, &float(value))
    }

    fn bool(&mut self, key: &str, value: bool) -> &mut Self {
        self.raw(key, if value { "true" } else { "false" })
    }

    /// `json` must be a complete JSON value.
    fn raw(&mut self, key: &str, json: &str) -> &mut Self {
        let _ = self.key(key);
        self.0.push_str(json);
        self
    }

    fn finish(mut self) -> String {
        self.0.push('}');
        self.0
    }
}

/// `value` as a JSON string.
fn string(value: &str) -> String {
    let mut json = String::with_capacity(value.len() + 2);

    json.push('"');

    for c in value.chars() {
        match c {
            '"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            '\n' => json.push_str("\\n"),
            '\r' => json.push_str("\\r"),
            '\t' => json.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(json, "\\u{:04x}", u32::from(c));
            }
            '\u{2028}' => json.push_str("\\u2028"),
            '\u{2029}' => json.push_str("\\u2029"),
            c => json.push(c),
        }
    }

    json.push('"');
    json
}

/// A JSON array of strings.
fn strings(values: &[String]) -> String {
    let values: Vec<String> =
        values.iter().map(|value| string(value)).collect();

    format!("[{}]", values.join(","))
}

/// A number, or `null` when it is not finite.
/// Layout leaves noise in the last digits (`30.799995`), so it is rounded to
/// a thousandth of a logical pixel.
fn float(value: f32) -> String {
    if value.is_finite() {
        // `+ 0.0` turns -0 into 0.
        ((value * 1000.0).round() / 1000.0 + 0.0).to_string()
    } else {
        String::from("null")
    }
}

fn size(size: Size) -> String {
    format!("[{},{}]", float(size.width), float(size.height))
}

fn rectangle(rectangle: Rectangle) -> String {
    format!(
        "[{},{},{},{}]",
        float(rectangle.x),
        float(rectangle.y),
        float(rectangle.width),
        float(rectangle.height)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn options_parse_in_both_forms() {
        let parsed = Args::parse(
            &args(&["--viewport", "402x874", "--scale=3", "file.ice"]),
            &["viewport", "scale"],
        )
        .unwrap();

        assert_eq!(parsed.get("viewport"), Some("402x874"));
        assert_eq!(parsed.get("scale"), Some("3"));
        assert_eq!(parsed.positional, args(&["file.ice"]));

        assert!(Args::parse(&args(&["--sclae", "3"]), &["scale"]).is_err());
        assert!(Args::parse(&args(&["--scale"]), &["scale"]).is_err());
    }

    #[test]
    fn viewports_are_presets_or_sizes() {
        assert_eq!(viewport(None).unwrap(), (Size::new(402.0, 874.0), 3.0));
        assert_eq!(
            viewport(Some("pixel-9")).unwrap(),
            (Size::new(412.0, 915.0), 2.625)
        );
        assert_eq!(
            viewport(Some("1280x720")).unwrap(),
            (Size::new(1280.0, 720.0), 1.0)
        );
        assert!(viewport(Some("0x720")).is_err());
        assert!(viewport(Some("big")).is_err());
    }

    #[test]
    fn device_viewports_have_their_safe_area() {
        let insets =
            |viewport| device_safe_area(viewport).map(|area| area.insets);

        assert_eq!(
            insets(Size::new(402.0, 874.0)),
            Some(Padding::ZERO.top(62.0).bottom(34.0))
        );
        assert_eq!(
            insets(Size::new(375.0, 667.0)),
            Some(Padding::ZERO.top(20.0))
        );

        let pixel = insets(Size::new(412.0, 915.0)).unwrap();
        assert!((pixel.top - 54.1).abs() < 0.01, "{pixel:?}");
        assert_eq!(pixel.bottom, 24.0);

        assert_eq!(insets(Size::new(390.0, 844.0)), Some(Padding::ZERO));
        assert_eq!(insets(Size::new(1024.0, 768.0)), Some(Padding::ZERO));
        assert_eq!(insets(Size::new(400.0, 800.0)), None);

        // Every preset has one.
        for (name, size, _) in VIEWPORTS {
            assert!(device_safe_area(*size).is_some(), "{name}");
        }
    }

    #[test]
    fn filters_follow_libtest() {
        let all = Filter::parse(&args(&["--nocapture", "--test-threads", "1"]));
        assert!(all.matches("flows::smoke"));

        let some = Filter::parse(&args(&["smoke", "--skip", "slow"]));
        assert!(some.matches("flows::smoke"));
        assert!(!some.matches("flows::smoke_slow"));
        assert!(!some.matches("flows::login"));

        let exact = Filter::parse(&args(&["--exact", "flows::smoke"]));
        assert!(exact.matches("flows::smoke"));
        assert!(!exact.matches("flows::smoke_2"));

        let ignored = Filter::parse(&args(&["--ignored"]));
        assert!(!ignored.matches("flows::smoke"));
    }

    #[test]
    fn instruction_lines_count_from_one() {
        let content =
            "viewport: 402x874\nmode: Immediate\n-----\nclick \"A\"\n";

        assert_eq!(first_instruction_line(content), 4);
    }

    #[test]
    fn json_is_escaped_and_on_one_line() {
        let mut json = Json::object();
        let _ = json
            .string("text", "a \"b\"\n\\ \u{1}")
            .optional_string("id", None)
            .float("scale", 2.625)
            .float("nan", f32::NAN)
            .bool("ok", true)
            .raw("texts", &strings(&[String::from("x")]));

        assert_eq!(
            json.finish(),
            r#"{"text":"a \"b\"\n\\ \u0001","id":null,"scale":2.625,"nan":null,"ok":true,"texts":["x"]}"#
        );
    }
}
