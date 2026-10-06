//! The app's headless harness. `cargo test` runs the `.ice` flows in
//! `tests/flows`; `icm test`, `icm shot --headless` and `icm ui --headless`
//! run this binary with arguments. Leave it as it is.
use std::process::ExitCode;

fn main() -> ExitCode {
    agent::main(app::application(), env!("CARGO_MANIFEST_DIR"))
}

/// Stands in for `iced_test::agent::main`, the harness of icm's protocol 1,
/// until iced_test provides it. It runs the flows; the `icm-*` subcommands
/// need the real harness. Once iced_test has it, `main` calls
/// `iced_test::agent::main` with the same arguments and this module goes.
mod agent {
    use iced_test::program::Program;

    use std::path::Path;
    use std::process::ExitCode;

    /// libtest's options that take a value.
    const VALUE_OPTIONS: &[&str] = &[
        "--color",
        "--format",
        "--logfile",
        "--skip",
        "--test-threads",
        "-Z",
    ];

    pub fn main(
        program: impl Program + 'static,
        manifest_dir: &str,
    ) -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();

        if let Some(command) =
            args.first().filter(|arg| arg.starts_with("icm-"))
        {
            eprintln!(
                "{command} needs iced_test::agent (harness protocol 1), which \
                this iced_test does not have yet; only the flows run"
            );

            return ExitCode::from(2);
        }

        // `cargo test` hands its arguments after `--` to every test binary,
        // so behave like libtest with a single test named `flows`.
        if args.iter().any(|arg| arg == "--list") {
            println!("flows: test");

            return ExitCode::SUCCESS;
        }

        let only_ignored = args.iter().any(|arg| arg == "--ignored")
            && !args.iter().any(|arg| arg == "--include-ignored");

        let mut filters = Vec::new();
        let mut skips = Vec::new();
        let mut args = args.iter();

        while let Some(arg) = args.next() {
            if VALUE_OPTIONS.contains(&arg.as_str()) {
                let value = args.next();

                if arg == "--skip" {
                    skips.extend(value.map(String::as_str));
                }
            } else if let Some(skip) = arg.strip_prefix("--skip=") {
                skips.push(skip);
            } else if !arg.starts_with('-') {
                filters.push(arg.as_str());
            }
        }

        let selected = (filters.is_empty()
            || filters.iter().any(|filter| "flows".contains(filter)))
            && !skips.iter().any(|skip| "flows".contains(skip));

        if only_ignored || !selected {
            println!("\nrunning 0 tests\n\ntest result: ok. 0 passed\n");

            return ExitCode::SUCCESS;
        }

        let flows = Path::new(manifest_dir).join("tests").join("flows");

        println!("\nrunning 1 test");

        match iced_test::run(program, &flows) {
            Ok(()) => {
                println!("test flows ... ok\n\ntest result: ok. 1 passed\n");

                ExitCode::SUCCESS
            }
            Err(error) => {
                println!("test flows ... FAILED\n");
                eprintln!("{error}");

                if let iced_test::Error::IceTestingFailed {
                    instruction, ..
                } = &error
                {
                    eprintln!("  at: {instruction}");
                }

                println!("\ntest result: FAILED. 0 passed; 1 failed\n");

                ExitCode::FAILURE
            }
        }
    }
}
