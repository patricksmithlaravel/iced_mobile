## How icm detects it

`icm test` builds every test target (`cargo test -p <pkg> --no-run`) and
runs them with `cargo test --no-fail-fast` and `ICED_TEST_BACKEND=tiny-skia`:
the unit tests, the doc tests and the app's harness (`tests/icm.rs`), which
runs every `tests/flows/*.ice`. Each failure is one `test.failed` check:

- a unit test: the test's name, its panic (`panicked at <file>:<line>`) and
  the assertion's values; `evidence` points at that line;
- a flow: the flow, the line and instruction that failed, why (`no widget
  shows the text "Count: 2"`) and the texts that were visible instead;
  `evidence` points at the line of the `.ice` file;
- a test binary that crashed (a signal, an abort) before it printed its
  summary.

`icm ui --headless ice <file>` reports the same for one flow.

## Fix

Fix the code or the test the evidence names, then rerun just it:
`icm test --filter <name> --json -q`, or for a flow
`icm ui --headless ice tests/flows/<name>.ice --json -q`.

For a flow, `icm ui --headless tree` lists every widget with its exact
text and bounds: `click "<text>"` and `expect "<text>"` match a widget's
whole text. Host tests and `.ice` clicks use a mouse; confirm touch
behaviour with `icm run ios-sim` or `icm run android`.
