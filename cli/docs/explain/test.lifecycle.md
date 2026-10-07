## How icm detects it

`icm test --on android --lifecycle` launches the app as `icm run android`
does, then puts it through what Android does to apps: dark mode on and off,
rotation, a larger font scale, bold text, Home and a relaunch, Back and a
relaunch, and a process kill in the background followed by a relaunch. Each
step is one `test.lifecycle` check, and the result's `lifecycle.steps` lists
them with the app's pid and a screenshot (`screen-<step>.png` in the run
directory).

A step fails when, since the step began:

- Android relaunched or destroyed the app's activity where it should not
  (`wm_relaunch_*`, `wm_destroy_activity` in `events-<step>.txt`): the app
  starts over and loses what it kept in memory;
- the app's process changed where it should have stayed, or the app sent a
  new `ICM_EVENT start` (it started over) after a configuration change or
  Home;
- the app did not draw again (`ICM_EVENT ready`) after Back or the kill, or
  its screenshot is blank;
- Android reported an ANR, or the app panicked or crashed.

Back depends on `[android] back`: with `"system"` the app must end with its
activity (or Android may move it behind the launcher) and start over at the
relaunch; with `"key"` the app receives Back and must stay.

The suite restores the device's settings (night mode, rotation, font scale,
font weight) at the end, whatever happened.

## Fix

- A relaunch names the configuration change (`orientation`, `uiMode`, …):
  `icm verify android` checks that `android:configChanges` lists it; an APK
  built by an older icm needs `icm run android` without `--no-build`.
- A new process or a new `start` after Home: something killed the app in the
  background; read `lifecycle-<step>/logcat.txt`.
- No frame after Back or the kill: the app does not start over cleanly. On
  Android `run()` runs again in the same process after Back, so everything it
  sets up for the process must accept a second call (see AGENTS.md).
- An ANR or a panic: read the evidence and `icm logs android --level warn`.
