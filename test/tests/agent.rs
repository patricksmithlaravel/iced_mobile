//! The agent harness, end to end, on a small counter, as an application's
//! `tests/icm.rs` uses it.
//!
//! Plain `cargo test` runs `tests/flows/*.ice`, then checks what `icm-shot`,
//! `icm-tree` and `icm-ice` write. With a command
//! (`cargo test -p iced_test --test agent -- icm-shot --out shot.png`), it is
//! the harness alone.
use iced_test::agent;
use iced_test::core::window;
use iced_test::core::{Element, Font, Settings, Theme};
use iced_test::program::Program;
use iced_test::runtime::Task;
use iced_widget::{button, column, container, text, text_input};

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

struct Counter;

#[derive(Default)]
struct State {
    count: i64,
    name: String,
}

#[derive(Debug, Clone)]
enum Message {
    Increment,
    NameChanged(String),
}

impl Program for Counter {
    type State = State;
    type Message = Message;
    type Theme = Theme;
    type Renderer = iced_test::renderer::Renderer;
    type Executor = iced_test::futures::backend::default::Executor;

    fn name() -> &'static str {
        "counter"
    }

    fn settings(&self) -> Settings {
        Settings {
            default_font: Font::with_name("Fira Sans"),
            ..Settings::default()
        }
    }

    fn window(&self) -> Option<window::Settings> {
        Some(window::Settings::default())
    }

    fn boot(&self) -> (State, Task<Message>) {
        (State::default(), Task::none())
    }

    fn update(&self, state: &mut State, message: Message) -> Task<Message> {
        match message {
            Message::Increment => state.count += 1,
            Message::NameChanged(name) => state.name = name,
        }

        Task::none()
    }

    fn view<'a>(
        &self,
        state: &'a State,
        _window: window::Id,
    ) -> Element<'a, Message, Theme, Self::Renderer> {
        container(
            column![
                text(format!("Count: {}", state.count)).size(32),
                button("Increment").on_press(Message::Increment),
                text_input("Your name", &state.name)
                    .id("name")
                    .on_input(Message::NameChanged),
            ]
            .spacing(16),
        )
        .padding(48)
        .into()
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    // A command, or `cargo nextest` listing the tests: the harness alone.
    if args
        .iter()
        .any(|arg| arg.starts_with("icm-") || arg == "--list")
    {
        return agent::main(Counter, manifest_dir);
    }

    let flows = agent::run(&Counter, manifest_dir, &args);

    if flows != ExitCode::SUCCESS {
        return flows;
    }

    // `cargo test` passes its filters to every test binary; check the
    // commands only when nothing was filtered out.
    if args.iter().any(|arg| !arg.starts_with('-')) {
        return ExitCode::SUCCESS;
    }

    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("agent");
    let _ = fs::remove_dir_all(&scratch);

    commands_write_what_they_report(&scratch);

    println!("agent commands ... ok");

    ExitCode::SUCCESS
}

fn commands_write_what_they_report(scratch: &Path) {
    let run = |args: &[&str]| {
        let args: Vec<String> =
            args.iter().map(|arg| (*arg).to_owned()).collect();

        agent::run(&Counter, env!("CARGO_MANIFEST_DIR"), &args)
    };

    // Screenshots: physical size, and the same pixels every time.
    let light = scratch.join("light.png");
    let again = scratch.join("again.png");
    let dark = scratch.join("dark.png");

    for (out, theme) in [(&light, "light"), (&again, "light"), (&dark, "dark")]
    {
        let code = run(&[
            "icm-shot",
            "--viewport",
            "iphone-17",
            "--theme",
            theme,
            "--wait-ms",
            "50",
            "--out",
            out.to_str().unwrap(),
        ]);

        assert_eq!(code, ExitCode::SUCCESS, "icm-shot {theme}");
    }

    assert_eq!(png_size(&light), (1206, 2622));
    assert_eq!(
        fs::read(&light).unwrap(),
        fs::read(&again).unwrap(),
        "two shots of the same view differ"
    );
    assert_ne!(
        fs::read(&light).unwrap(),
        fs::read(&dark).unwrap(),
        "the dark shot is the light one"
    );

    // The tree: texts, and the text input under its id.
    let tree = scratch.join("tree.json");

    assert_eq!(
        run(&[
            "icm-tree",
            "--viewport=390x844",
            "--wait-ms=0",
            "--out",
            tree.to_str().unwrap(),
        ]),
        ExitCode::SUCCESS
    );

    let tree = fs::read_to_string(tree).unwrap();

    assert!(tree.starts_with(r#"{"protocol":1,"kind":"tree","ok":true,"viewport":[390,844],"widgets":["#));
    assert!(tree.contains(
        r#"{"kind":"text","id":null,"text":"Count: 0","bounds":[48,48,"#
    ));
    assert!(tree.contains(r#""text":"Increment""#));
    assert!(tree.contains(
        r#"{"kind":"text_input","id":"name","text":"Your name","focused":false,"#
    ));
    assert!(!tree.contains(r#""kind":"focusable""#));

    // A failing flow: the step, why, and what was on screen.
    let flow = scratch.join("fails.ice");
    let report = scratch.join("fails.json");

    fs::write(
        &flow,
        "viewport: 402x874\nmode: Immediate\n-----\nclick \"Increment\"\nexpect \"Count: 2\"\nclick \"Increment\"\n",
    )
    .unwrap();

    assert_eq!(
        run(&[
            "icm-ice",
            flow.to_str().unwrap(),
            "--report",
            report.to_str().unwrap(),
        ]),
        ExitCode::from(1)
    );

    let report = fs::read_to_string(report).unwrap();

    assert!(report.contains(r#""passed":false"#), "{report}");
    assert!(report.contains(
        r#"{"line":4,"instruction":"click \"Increment\"","status":"passed","#
    ));
    assert!(report.contains(
        r#"{"line":5,"instruction":"expect \"Count: 2\"","status":"failed","#
    ));
    assert!(
        report.contains(r#""reason":"no widget shows the text \"Count: 2\"""#)
    );
    assert!(report.contains(r#""texts":["Count: 1","Increment","Your name"]"#));
    assert!(report.contains(
        r#"{"line":6,"instruction":"click \"Increment\"","status":"skipped","#
    ));

    // Usage errors.
    assert_eq!(run(&["icm-shot"]), ExitCode::from(2));
    assert_eq!(run(&["icm-tree", "--viewport", "huge"]), ExitCode::from(2));
    assert_eq!(run(&["icm-launch"]), ExitCode::from(2));
}

/// The width and height in a PNG's header.
fn png_size(path: &Path) -> (u32, u32) {
    let bytes = fs::read(path).unwrap();

    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");

    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());

    (width, height)
}
