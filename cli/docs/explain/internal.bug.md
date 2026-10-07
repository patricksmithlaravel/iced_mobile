## What happened

icm itself panicked or reached a state it cannot handle. The result is still
written (exit 70), with the panic message in `errors[0].detail`, and the run
directory keeps `events.ndjson` and the step logs.

## Fix

Report it with the `run_dir`. Rerunning sometimes works around a transient
cause (a file removed mid-run).
