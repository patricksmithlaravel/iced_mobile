## How icm detects it

`icm logs`, `icm shot` and `icm input` act on what `icm run <platform>` left
running, recorded in `target/icm/sessions/<platform>.json` (the device, the
app's pid, the launch mark and the live log files). This fails when no run
started a session in this project, or when the device it used is no longer
running (for example a simulator that was shut down).

## Fix

```sh
icm run <platform> --json -q
```
