## How icm detects it

`icm shot --headless`, `icm ui --headless` and the flows of `icm test` run
the app's harness: the package's `icm` test target, built with
`cargo test --test icm --no-run`. icm reports this id when

- the package has no test target named `icm` in `cargo metadata`, or
- the target runs libtest's harness (it rejects `--viewport`, or prints
  `running N tests`) instead of `iced_test::agent::main`, or
- it printed no `ICM_HARNESS {"protocol":1}` line, which the harness prints
  before running any of the app's code.

`icm test` still runs the unit tests without it and reports this as a
WARN: the flows were not run.

## Fix

Add what `icm new` writes. In Cargo.toml:

```toml
[[test]]
name = "icm"
path = "tests/icm.rs"
harness = false

[dev-dependencies]
iced_test = { git = "…", tag = "…" }   # the same source as iced, character for character
```

`tests/icm.rs`:

```rust
fn main() -> std::process::ExitCode {
    iced_test::agent::main(my_app::application(), env!("CARGO_MANIFEST_DIR"))
}
```

`application()` is the function `run()` uses, returning the
`iced::application(...)` builder.
