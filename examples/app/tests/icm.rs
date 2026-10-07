//! The app's headless harness. `cargo test` runs the `.ice` flows in
//! `tests/flows`; `icm test`, `icm shot --headless` and `icm ui --headless`
//! run this binary with arguments. Leave it as it is.
use std::process::ExitCode;

fn main() -> ExitCode {
    iced_test::agent::main(app::application(), env!("CARGO_MANIFEST_DIR"))
}
