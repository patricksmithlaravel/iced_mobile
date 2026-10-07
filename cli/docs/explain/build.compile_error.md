## Where the errors are

cargo runs with `--message-format=json-render-diagnostics`. Each compiler
message becomes a `diagnostic` event (de-duplicated across targets), and the
first ten errors are attached to `errors[0].diagnostics` with `file`, `line`,
`col` and rustc's `rendered` text, in every output mode including
`--json -q`. The full output is in the step log.

## Fix

Fix the first error first; later ones are often consequences. A target-only
error (it builds on desktop but not for `aarch64-linux-android`) usually
means a dependency or `cfg` that does not exist on that platform.
