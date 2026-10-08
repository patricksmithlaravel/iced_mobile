## How icm detects it

No first frame was seen within `--wait-ready` (30 s by default): no
`ICM_EVENT ready` line, and the platform probe (a window, a resumed activity)
did not confirm one either. The detail says what icm saw: usually that the app
is alive; on Android, after a `--wait-ready` shorter than the app takes to
start, it may say that no process of the app was seen yet, so whether it is
alive is not known.

Common causes: the app blocks the main thread at start-up; the GPU adapter
could not be created (try `--env ICED_BACKEND=tiny-skia`); on Android the
activity was recreated and the new one did not draw (`run.activity_recreated`:
the run then fails ten seconds after the relaunch, and `likely_causes` and the
fix name its cause); on iOS the scene manifest is missing.

## Fix

```sh
icm logs <platform> --level warn
```

Raise `--wait-ready` only if start-up is legitimately slow.
