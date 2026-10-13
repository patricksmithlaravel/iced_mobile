## How icm detects it

Every step has a time limit, and `--timeout` bounds the whole command. When
a limit is hit, icm sends SIGTERM to the step's whole process group, then
SIGKILL after a grace period, and fails with this id; the detail says which
limit ran out and the evidence points at the step log.

## Fix

Read the step log: a tool waiting for input or a lock is a bug to report;
legitimate slowness (a cold build) needs a larger `--timeout`, or
`--detach` followed by `icm wait <run>`.

A probe with its own fixed limit, such as `rustc --print sysroot`,
`xcode-select -p`, `java -version` or `wasm-bindgen --version`, is not
raised by `--timeout`. Its fix names the command to run by hand and its
limit; a stall there is usually a hung tool, not a missing one.
