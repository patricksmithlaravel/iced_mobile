## How icm detects it

Every step has a time limit, and `--timeout` bounds the whole command. When
a limit is hit, icm sends SIGTERM to the step's whole process group, then
SIGKILL after a grace period, and fails with this id; the detail says which
limit ran out and the evidence points at the step log.

## Fix

Read the step log: a tool waiting for input or a lock is a bug to report;
legitimate slowness (a cold build) needs a larger `--timeout`, or
`--detach` followed by `icm wait <run>`.
