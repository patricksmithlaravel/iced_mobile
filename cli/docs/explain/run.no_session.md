## How icm detects it

`icm shot`, `icm input` and `icm logs` work on the app `icm run` started
and left running. They read this project's session record,
`target/icm/sessions/<platform>.json` (the device, the app's pid, the
launch mark and the live log files), and check that its process is still
the one that wrote it. This fails when no run started a session in this
project, its process has exited or did not answer, or the device it used is
no longer running (for example a simulator that was shut down).

`icm logs web` and `icm logs desktop` still read the last session's logs
after it ends; `shot` and `input` need a live one.

## Fix

```sh
icm run <platform> --json -q
icm ps                            # what is running
```
