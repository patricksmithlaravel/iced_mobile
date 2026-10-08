# AGENTS.md — {{name}} ({{id}}) · iced_mobile {{framework_tag}} · icm {{icm_version}}

One Rust codebase (`src/lib.rs`) runs on the desktop, the web, iOS and Android. Build, run, see and
test it ONLY through `icm`, and read the last JSON line of every command: it is the result.

## The loop
1. `icm check --all --json -q`: compiles every platform. Fix what `errors[]` lists (file, line, message).
2. Fresh checkout or new dependencies: prewarm every build with `icm build --all --detach --json -q`,
   then repeat `icm wait <run> --timeout 9m --json -q` until it returns the result.
3. `icm run <desktop|web|ios-sim|android> --json -q`: builds, installs, launches, waits for the first
   frame, takes a screenshot and returns while the app keeps running. Then OPEN `artifacts.preview`
   and look at it. Every run, every time.
4. `icm logs <platform> --level warn --json` when anything looks wrong (it re-reads the live logs).
   Trust the `app` and `crash` records: the app's own output, panics and crash reports. On ios-sim,
   `--source system` adds the system log, mostly other processes' errors that merely mention the app.
5. `icm test --json -q`: unit tests and `tests/flows/*.ice` in a headless renderer.
   `icm test --on android --lifecycle --json -q` puts the app through rotation, dark mode, font
   scale, Home, Back and a process kill on the emulator; every step must keep or restart it cleanly.
6. `icm shot --headless --all-viewports --json -q`: the layout at phone and desktop sizes, no device.
7. `icm ui --headless tree --json`: the widget tree with bounds, for writing `.ice` flows.
   `icm ui --headless find "<text>" --viewport pixel-9 --json` gives a widget's centre in points at
   that viewport (for `icm input android tap X Y --space pt`); `icm ui --headless ice <file>` reruns
   one flow step by step.
8. `icm stop --all` when done.

A first build can outlast your command timeout. Add `--detach`: `icm run android --detach --json -q`
returns at once with the run id and `"status":"running"`; then repeat
`icm wait <run> --timeout 9m --json -q` until it returns the run's result.

Input on a running app: `icm input <android|web> tap X Y` (also `swipe`, `text`, `key`). X and Y are
pixels of the `screen.preview.png` you looked at; the result's `screen` gives the other scales, and
`--space px|pt` takes device pixels or points instead. On ios-sim and the desktop, drive the UI with
`.ice` flows.

## Results
- Exit 0 ok · 1 check/test failed · 2 usage · 3 config · 4 environment not ready (follow `fix.by`)
  · 5 build · 6 tool · 7 device · 8 timeout · 9 OWNER NEEDED · 10 app crashed or never drew · 70 icm bug.
- Exit 9: STOP. Give the owner `errors[0].fix`. Do not work around it, and never guess credentials.
- Every error has an id: `icm explain <id>`. Read the files in `errors[].evidence`.
- `fix.by` says who acts: `doctor` and `doctor-yes`: run `fix.commands` (`icm doctor <platform> --fix`,
  `--yes` to download); `agent`: you; `owner`: STOP as for exit 9. Exit 4 can need any of them, so
  read every entry of `errors[]`.
- Raw `simctl launch` and `adb install` exit codes prove nothing; `icm run` checks the app is alive and drew.

## Where things are
- App code: `src/lib.rs`. `src/main.rs`, `tests/icm.rs` and the `iced::android_main!(run)` line are
  fixed: don't edit them (except `logger = false`, below).
- Identity, icon, permissions, orientations, signing references: `icm.toml` (`icm explain config.<key>`).
- Version: Cargo.toml `version`. Store build number: icm.toml `[app] build`.
- Info.plist, AndroidManifest.xml, PrivacyInfo.xcprivacy and index.html are GENERATED from icm.toml on
  every build. Never create them; change icm.toml.
- Outputs: `target/icm/latest/<platform>/` (screen.png, screen.preview.png, app.log, result.json); the
  newest result of any command is `target/icm/last.json`. Values of secret-named variables
  (`*TOKEN*`, `*KEY*`, `*SECRET*`, `*PASS*`, `*PRIVATE*`) read `<redacted>` there and on stdout; the
  live files icm reads in `target/icm/sessions/` are the app's own output, unredacted, while it
  runs. A session keeps the secret values given with `--env` in a `secrets.json` (mode 0600)
  there, so later commands redact them too. A desktop app also inherits icm's environment, but icm
  keeps none of those values in a file: later commands read them from the running app, and
  `icm stop desktop` redacts its live files. If the app ended by itself, `icm logs desktop` from a
  shell without such a variable shows only the run's redacted copies and warns
  `desktop.logs.secret_unknown`, so hand a secret the app logs with `--env`.
- Flows: `tests/flows/*.ice`. A header, a `-----` line, then one instruction per line:
  ```
  viewport: 402x874
  mode: Immediate
  -----
  click "Increment"
  type "Milk"
  type enter
  expect "Count: 1"
  ```
  `click "<text>"` finds a widget by its exact text (a field by its placeholder, or its value once
  typed into), the first one when several show it; `type "<text>"` types into the focused field;
  `type enter|tab|escape|backspace` presses a key; `expect "<text>"` passes when some widget shows
  exactly that text. `mode: Immediate` goes on at once; `mode: Zen` waits for every task an
  instruction starts, as `tests/flows/copy_paste.ice` does for the clipboard.
- Headless viewports the size of a phone preset (`402x874` is `iphone-17`, `412x915` is `pixel-9`)
  get that phone's safe area, so they lay out as the phone does, and the headless clipboard starts
  empty in each flow. `iced::system::theme_changes()` and `iced::mobile::lifecycle()` report nothing
  headless (`icm shot --headless --theme dark` still draws the default theme dark).

## Rules that fail silently when broken
- Keep every iced line on the same git URL and tag (or rev), character for character.
- Never call `iced::exit()` or close the last window on Android or iOS.
- Keep `[android] back = "system"` unless `src/lib.rs` handles `Key::Named(Named::BrowserBack)`:
  `"key"` sends Back to the app, and an app that ignores it cannot be left with Back.
- Android may end the app and start it over (Back at its root, or killing its process in the
  background to free memory): save what must survive on `Lifecycle::Suspended`
  (`iced::mobile::on_lifecycle`). To react in the UI (hide content, pause, lock), subscribe to
  `iced::mobile::lifecycle()`, as `src/lib.rs` does: its `LifecycleEvent` messages come too late
  for saving, and a `match` on them needs a wildcard arm. Hide
  content on `Inactive`, lock on `Background`, never on `Inactive` (a Face ID prompt makes the app
  inactive, and the unlock would loop).
- Keep padding the root with the safe area (`App::padding` in `src/lib.rs`): phones draw under the
  status bar, the notch, the home indicator or navigation bar (Android targetSdk 36 is edge to edge)
  and the keyboard. `iced::mobile::safe_area()` reports them, once the window exists; the fixed
  `fallback_padding` stands in until then.
- Keep the theme following the system: no fixed `.theme(..)`, or a theme per mode from
  `iced::system::theme_changes()`. icm's Android window and bar icons follow the system's mode, so a
  light UI forced in dark mode loses its status bar there (white icons on white) unless the app
  declares its own `IcmTheme` in `platform/android/res`.
- Read the clipboard only when the user asks to paste (`Message::Paste`): Android gives `None` to an
  app without the input focus, and iOS asks the user before an app reads what another app copied.
  Text fields have no edit menu on phones, so keep Copy and Paste buttons where they matter.
- `.ice` `click` and host tests use a mouse; phones use touch. Confirm UI changes with `icm run ios-sim`
  and `icm run android`, and look at the screenshot.
- Keep `features = ["fira-sans"]` and the Fira Sans `default_font`: every platform and the headless
  renderer then draw the same glyphs, and the web has no system fonts to fall back on.
- Don't set `default-features = false` on iced: it drops the Android activity, the mobile logger and
  the mobile fonts without a word.
- Leave `[lib]` without `crate-type`: icm builds Android's shared library itself.
- A logger of your own (`env_logger`, `tracing_subscriber`) is installed in `run()` BEFORE
  `iced::mobile::init_logger()`, and the Android line becomes `iced::android_main!(run, logger = false)`.
  `log` takes one logger per process: installed after iced's, yours panics at startup.
- On Android `run()` runs again for each new activity in the same process (after Back, say), so
  everything it sets up for the process must accept a second call: `try_init()` with the error
  ignored, or a `std::sync::Once`. A second `env_logger::init()` panics, and the app crashes.

## Releases belong to the owner
- `icm release ios --sign none --json -q` builds and checks the App Store `.ipa` without the
  owner's certificates; a signed release, `UPLOAD.md` and `upload.sh` are the owner's. App Store
  screenshots: `icm run ios-sim --store`, then `icm shot ios-sim --store --name <screen>`.
- `icm release android --sign none --json -q` builds and checks the Google Play `.aab` without the
  owner's upload key (`--apk` adds a universal APK); while icm's emulator runs, it also installs the
  release there once (`--no-smoke` skips that).
- `icm release macos --sign none --json -q`, then the same with `--dmg`, builds the macOS `.app`,
  its zip and the DMG; `icm release linux --sign none --json -q` builds the `.deb` and the AppImage.
  Each desktop target builds on its own OS, and icm does not run on Windows yet.
- NEVER upload, publish or notarize anything, and never run `altool`, `notarytool`, `fastlane`,
  `wrangler` or any other command that does.
- Signing identities, profiles, keystores and their passwords are the owner's. Don't create or guess them.
- `icm release web --json -q` builds the static site, runs its gates and loads it once in headless
  Chrome; it lands in `target/icm/dist/latest/web/`. Deploying it is the owner's
  (`icm upload-commands web` prints the commands).

## Known limitations of iced_mobile {{framework_tag}}
{{limitations}}
