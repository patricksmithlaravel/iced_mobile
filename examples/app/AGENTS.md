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
- Exit 0 ok · 1 check/test failed · 2 usage · 3 config · 4 environment (`icm doctor <platform> --fix --yes`)
  · 5 build · 6 tool · 7 device · 8 timeout · 9 OWNER NEEDED · 10 app crashed or never drew · 70 icm bug.
- Exit 9: STOP. Give the owner `errors[0].fix`. Do not work around it, and never guess credentials.
- Every error has an id: `icm explain <id>`. Read the files in `errors[].evidence`.
- `fix.by` says who acts: agent | doctor | doctor-yes | owner.
- Raw `simctl launch` and `adb install` exit codes prove nothing; `icm run` checks the app is alive and drew.

## Where things are
- App code: `src/lib.rs`. `src/main.rs`, `tests/icm.rs` and the `iced::android_main!(run)` line are
  fixed: don't edit them (except `logger = false`, below).
- Identity, icon, permissions, orientations, signing references: `icm.toml` (`icm explain config.<key>`).
- Version: Cargo.toml `version`. Store build number: icm.toml `[app] build`.
- Info.plist, AndroidManifest.xml, PrivacyInfo.xcprivacy and index.html are GENERATED from icm.toml on
  every build. Never create them; change icm.toml.
- Outputs: `target/icm/latest/<platform>/` (screen.png, screen.preview.png, app.log, result.json); the
  newest result of any command is `target/icm/last.json`.
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
  typed into); `type "<text>"` types into the focused field; `type enter|tab|escape|backspace` presses
  a key; `expect "<text>"` passes when some widget shows exactly that text.

## Rules that fail silently when broken
- Keep every iced line on the same git URL and tag (or rev), character for character.
- Never call `iced::exit()` or close the last window on Android or iOS.
- Keep `[android] back = "system"` unless `src/lib.rs` handles `Key::Named(Named::BrowserBack)`:
  `"key"` sends Back to the app, and an app that ignores it cannot be left with Back.
- Android may end the app and start it over (Back at its root, memory reclaim): save what must
  survive on `Lifecycle::Suspended` (`iced::mobile::on_lifecycle`).
- Keep the root padding (`safe_area` in `src/lib.rs`): there is no safe-area API yet, and Android
  targetSdk 36 draws edge to edge.
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
- This icm builds and runs development builds. Store packages come with a later icm.
- NEVER upload, publish or notarize anything, and never run `altool`, `notarytool`, `fastlane`,
  `wrangler` or any other command that does.
- Signing identities, profiles, keystores and their passwords are the owner's. Don't create or guess them.

## Known limitations of iced_mobile {{framework_tag}}
{{limitations}}
