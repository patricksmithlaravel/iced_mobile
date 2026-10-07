## How icm detects it

`icm run android` reads Android's events buffer from the launch mark
(`logcat -b events`, saved as `events.txt` in the run directory) and looks for
`wm_relaunch_resume_activity` or `wm_relaunch_activity` (`am_*` before API 29)
naming the app's activity. The last field of the event is the mask of the
configuration changes that caused it; the detail names them as
`android:configChanges` does (`80000000` is `assetsPaths`) and compares them
with the manifest icm generates.

An iced app cannot survive a recreated activity: android-activity holds the
old activity's `onDestroy` until `android_main` returns, and winit 0.30 does
not end its event loop then, so the app stops drawing and answering input.
When the relaunch comes before the first frame, the run also fails
`run.not_ready` (exit 10) ten seconds after it, with the relaunch as its
likely cause; after the first frame it is this FAIL alone (exit 1).

Common causes:

- `assetsPaths`: a resource overlay changed. SystemUI applies its theme
  overlays during an emulator's first boots. icm's manifest lists
  `assetsPaths` from `[android] target_sdk = 36`; below that android.jar
  does not know the name.
- the installed APK is older than this icm's manifest (an earlier icm built
  it, or `--no-build` reused it);
- a change no `configChanges` value covers yet.

## Fix

Do what the detail says: rerun `icm run android` without `--no-build` for a
stale APK, raise `[android] target_sdk` to 36 for `assetsPaths`, or rerun once
a fresh emulator has settled. Never remove a `configChanges` value, and never
call `iced::exit` on mobile.
