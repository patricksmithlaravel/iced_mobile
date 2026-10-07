## How icm detects it

The app's stderr (desktop, iOS Simulator), logcat (Android) or the browser
console (web) contains a Rust panic (`panicked at <file>:<line>:<col>`) or an
`ICM_EVENT {"kind":"panic"}` line before or after the first frame.

`errors[0].evidence` points at the panic line in the captured output, and
`likely_causes` names the location.

## Fix

Fix the panic at the location in the detail, then rerun the same
`icm run <platform>`. `icm logs <platform> --level warn` shows the context.
