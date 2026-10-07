## How icm detects it

`icm run android` reads Android's events buffer from the launch mark
(`logcat -b events`, saved as `events.txt` in the run directory) and looks for
`wm_relaunch_resume_activity` or `wm_relaunch_activity` (`am_*` before API 29)
naming the app's activity. The last field of the event is the mask of the
configuration changes that caused it; the detail names them as
`android:configChanges` does (`80000000` is `assetsPaths`) and compares them
with the manifest icm generates.

A relaunch destroys the activity, and the iced application ends with it: its
state, windows and tasks are dropped, `android_main` returns, and the new
activity starts the application again, from its boot function, usually in
the same process (a new `ICM_EVENT start`, then `ready`). The app keeps
running, so this is a WARN, but whatever it held in memory is gone, and a
user would see it reset.

It is a FAIL (exit 1 when nothing else fails) when the app did not start
over: no new `ICM_EVENT start` followed the last relaunch within ten
seconds, or the app sends no `ICM_EVENT` at all. That is what a framework
from before the Android lifecycle fix does: its winit does not end the event
loop when the activity is destroyed, so the app stops drawing and answering
input. When the app's `Cargo.lock` has a winit that does not come from
iced's own source (winit from crates.io, as before iced vendored it), the
detail says so and icm does not wait. When the relaunch comes before the
first frame and the new activity does not draw within ten seconds, the run
also fails `run.not_ready` (exit 10) with the relaunch as its likely cause.

Common causes:

- `assetsPaths`: a resource overlay changed. SystemUI applies its theme
  overlays during an emulator's first boots. icm's manifest lists
  `assetsPaths` from `[android] target_sdk = 36`; below that android.jar
  does not know the name.
- the installed APK is older than this icm's manifest (an earlier icm built
  it, or `--no-build` reused it);
- a change no `configChanges` value covers yet.

A panic "Create event loop: an event loop is already running in this
process" is related: Android started a second activity of the app while the
first still ran it, after a launch a moment after Back (a launch a moment
later works) or a start into another task, which the manifest's
`android:launchMode="singleTask"` prevents.

## Fix

Do what the detail says: rerun `icm run android` without `--no-build` for a
stale APK, raise `[android] target_sdk` to 36 for `assetsPaths`, or rerun once
a fresh emulator has settled. Never remove a `configChanges` value. Save what
must survive a destroyed activity on `Lifecycle::Suspended`, which comes
before it. For a FAIL, update the app's iced_mobile pin to one with the
Android lifecycle fix (`cargo tree -i winit --target all` then shows winit
from iced's own source), and read `icm logs android --level warn` for why
the new activity did not start the app.
