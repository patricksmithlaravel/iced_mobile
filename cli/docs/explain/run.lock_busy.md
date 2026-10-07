## How icm detects it

Each project holds one lock per platform in `target/icm/locks/<platform>.lock`
(`std::fs::File::try_lock`) while a command drives that platform, so two icm
processes never fight over one simulator or emulator. The detail names the
holder's pid and run id.

## Fix

Wait for the other command, pass `--wait-lock 2m` to wait for it, or stop it
(`icm stop <platform>`).
