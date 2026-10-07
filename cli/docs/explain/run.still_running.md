## What it means

`icm wait <run>` waited for its whole `--timeout` (9 minutes by default) and
the detached run has not finished. Nothing failed: the run continues in the
background, in its own session.

## Fix

Call the same command again:

```sh
icm wait <run> --timeout 9m --json -q
```

Each call returns when the run finishes or the timeout ends, so it fits under
an agent's 10-minute command limit.
