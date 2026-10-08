# iced_mobile releases

What changed in each release of the fork, newest first. A release is one tag, `v0.14.x-mobile.N`,
for the framework and `icm` together. Unreleased lists what `main` has changed since the newest
release; the next release's section takes it over. [CHANGELOG.md](CHANGELOG.md) is upstream's and
stops at iced 0.14.0.

## Unreleased

Fixes from the re-verification of `v0.14.1-mobile.2`.

- **iOS:** the vendored winit reports an insertion of only a line break as Return and one of only
  `\t` as Tab, deciding per insertion, so a Return typed right after another key in the same turn
  of the event loop is no longer lost.
- The known limitations in an app's `AGENTS.md` say that iOS hardware keyboards deliver Return and
  Tab as keys, beside typed characters and Backspace, where they said typed characters and
  Backspace only.
- **Android:** the safe area is published again when the app changes its own scale factor.
- **Android:** the on-screen keyboard shows on Android 16 (API 36). iced_winit requires
  android-activity 0.6.1, and the fork's `Cargo.lock` holds it: 0.6.0 asked Android to show the
  keyboard for a view that is not the served one there, so tapping a `text_input` showed none. An
  app whose `Cargo.lock` still has 0.6.0 gets 0.6.1 when it moves to the next release's tag, or now
  with `cargo update -p android-activity`.
- `icm logs desktop` of an app that ended by itself never takes a variable of the same name in its
  own shell for a secret the app inherited: it reads the run's redacted copies and warns
  `desktop.logs.secret_unknown` whatever the shell holds. Before, a shell where that variable held
  another value printed the old one. Read the logs while the app runs, or hand the secret with
  `--env`.
- `icm logs desktop` of a running app whose environment could not be read back in full no longer
  prints the secrets it inherited. A read of the process that returned its arguments and no
  variables (macOS does for a platform binary), or another environment than the app's (the app
  replaced its process with `exec`, keeping its pid), made every inherited secret known, so `logs`
  from a shell with another value of the variable printed the app's token in its output,
  `result.json` and `events.ndjson` with no warning. Now an inherited secret counts as known only
  when the read returned a variable of its name, and a process that runs another program than the
  one `icm run` launched is not read at all; the others warn `desktop.logs.secret_unknown` and
  `logs` reads the run's redacted copies.
- Desktop session records keep only the protocol fields of the app's `ready` event, redacted, so a
  secret an app put in a field of its own no longer stays in them.
- `icm stop --shutdown` leaves a managed simulator or emulator running when it cannot read which
  project booted it, and a run that cannot mark the device as its own says so
  (`ios.sim.owner_unknown`, `android.emulator.owner_unknown`).
- Under `--json`, `--help` and usage errors name the command, not the value of a global option
  written before it (`icm --config x.toml run --help`), nor the word after a misspelled option
  (`icm --conf x.toml run`).
- `icm print plan <command> --help` answers with that command's help and exit 0, where it was a
  `usage.bad_args` error (exit 2) whose detail was the help's first line.
- When `icm run android --wait-ready` runs out, the error says only what the readiness polls saw,
  where it used to say the app was alive. After Android relaunched the activity, it says so, where
  it used to say the wait ended before the resumed-activity probe starts.
- `icm wait` reports a detached run whose icm died without a result as `run.detached_lost`, where it
  waited out its whole timeout when another process had taken the pid. `icm run` and `icm --detach`
  record the icm's start time beside its pid, and `prune` keeps a run directory only while that
  process runs; a run an older icm started is still judged by its pid.
- `icm stop desktop` signals the pid in a desktop session only while it is the app `icm run`
  started: its start time is recorded beside the pid, where only the executable the pid ran was
  checked, so another instance of the same program that took the pid, started by hand say, was
  stopped. The same test guards the read of the running app's environment. A session from before
  keeps the executable test, and counts as not running when `ps` cannot say.
- `icm stop web`, `icm ps` and the web commands take the pid in a web session's record for the
  session host only while that process is the one that wrote the record: the host records its start
  time, and the marker (`__session web` in the command line) that matched every session host remains
  only for records from before. Before, `icm stop web` killed any process under the pid whose
  command line held the marker, another session host included, and one under a record with no
  marker. `icm stop ios-device`, `icm ps` and `icm stop --all` check the pids of the other records
  the same way: the identity `run` recorded for the console process, else the old test of the file's
  write time.
- `icm stop ios-sim`, `icm run ios-sim --attach` and the next `icm run` on another simulator take
  the pid in an iOS Simulator session for the app only while that process is the one `icm run`
  launched (and `logs` and `input` report it running on the same test): its start time is recorded
  beside the pid, as the log collector's now is. Before, any process under the number counted, so
  `icm stop ios-sim` ran `simctl terminate` for an app that had exited and failed, and a run on
  another simulator ended the app there. A session an older icm wrote has no start time and says the
  app is not running; end that app with `xcrun simctl terminate <udid> <bundle id>`.
- `icm stop android` takes the emulator on a session's serial for the one its run booted only while
  that emulator's process is verified. `icm run` records the process's start time beside its pid,
  and a pid that has exited, that another process has taken, or that an older session recorded
  without a start time proves nothing. Then icm reads the AVD and the owner of whatever runs on that
  serial now, so another project's emulator on the same port stays up, and the app is not
  force-stopped there. It also reads the owner before it force-stops or shuts down an emulator it
  has a live record of, and signals the emulator's process only while that process is the recorded
  one. Before, a stale record whose pid another live process had taken made `stop --shutdown` send
  `am force-stop` and `emu kill` to another project's emulator without reading its owner, and then
  SIGTERM that process.
- `icm stop android` decides about an emulator whose record says icm booted it, but whose recorded
  process has ended or was replaced by another process, as it does for a record from before start
  times: by the emulator's `debug.icm.booted_by`. When it names this project, the app is
  force-stopped and `--shutdown` shuts the emulator down with `adb emu kill`, and the recorded pid
  is never signalled. When it names another project, both are left running
  (`android.emulator.shared`); when it cannot be read (`android.emulator.owner_unknown`), or is
  unset on an AVD icm does not manage, both are left running too (`run.no_session` says "the
  recorded emulator process has ended or was replaced; ..." and what else icm knows). Before, such a
  session on an `icm-test-*` emulator left the app and the emulator running without reading the
  property and said icm did not boot it, where the record says it did, and `--shutdown` did not look
  at a per-serial record in that state at all.
- `icm stop --shutdown` no longer reports an emulator that ignored `adb emu kill` as stopped when
  icm has no verified process of it to signal (a session from before start times, no record). It
  keeps the emulator's record, warns `android.emulator.shutdown_failed` with the command that tries
  again, says in the summary that it did not shut down, and lists it as `still_running` in the
  result of `icm stop android` and `icm stop`. Before, it waited 30 s, reported the emulator under
  `stopped`, deleted its record and said nothing, with the emulator still listed by `adb devices`.
  The wait is shorter when `--timeout` leaves less.
- Plain `icm stop android` (and `icm stop --all`) says why it left the app running when the device
  names another project as the emulator's owner (`android.emulator.shared`), with the `adb` command
  that stops the app by hand, where it said nothing.
- `icm ps` judges an iOS Simulator session's app by the start time `run` recorded, as `icm stop
  ios-sim` does, where it still listed a pid another process had taken as the running app when the
  session file had been rewritten since (`shot`, `run --attach`).
- `icm stop android --dry-run` states the rules the stop follows for the app and the emulator.

## Changes in v0.14.1-mobile.2

```sh
cargo install --locked --git https://github.com/patricksmithlaravel/iced_mobile --tag v0.14.1-mobile.2 icm
```

To move an app from `v0.14.1-mobile.1`, put the new tag on every iced line of its `Cargo.toml`,
build it once with `icm check --all` and commit the updated `Cargo.lock`. Apps that `icm new`
creates now say `min_icm = "0.14.1-mobile.2"`: icm 0.14.1-mobile.1 does not know their `[store]`
table and stops with `config.unknown_key`.

### Release pipelines

`icm release <target>` builds and checks what a store or a host takes. It signs with the owner's
own certificates and keys, and the owner uploads and publishes: icm never uploads, publishes or
notarizes.

- **Every target:** `--sign none` builds without the owner's signing assets. A release goes to
  `target/icm/dist/<version>+<build>/<target>/` with `artifacts.json`, `UPLOAD.md`, `upload.sh` and
  `THIRD_PARTY_NOTICES`. `icm verify`, `icm upload-commands`, `icm ledger` and `icm diagnose` work
  on it. A release needs a committed `Cargo.lock`. `icm.toml` gains `[store]` for the listing URLs
  and the names of the variables that hold upload credentials. The stores' floors come from a dated
  policy table, and the tools releases download are pinned by version, size and sha256
  ([cli/tools.toml](cli/tools.toml)).
- **iOS** (macOS with Xcode 26 or newer): `icm release ios` builds a signed, store-checked `.ipa`
  with its dSYM. `icm run ios-device` installs and launches on an iPhone through `devicectl`, and
  `icm run ios-sim --store` with `icm shot ios-sim --store` takes the App Store screenshots.
- **Android:** `icm release android` builds the Google Play `.aab`, signs it with the owner's upload
  key (passwords from environment variables), checks it with bundletool and installs it on the
  emulator as a smoke test; `--sign none --apk` adds a universal APK. `icm run android --from-aab`
  and `icm test --on android --lifecycle` (rotation, dark mode, font scale, Home, Back, a process
  kill) are new.
- **Web:** `icm release web` builds a size-optimized static site (wasm-opt, content-hashed modules,
  `_headers`, icons, licence notices) and loads it once in headless Chrome. `icm verify web --url`
  checks the deployed site.
- **Desktop, each on its own host:** `icm release macos` on macOS (a signed, hardened `.app` and the
  zip the owner notarizes, then the DMG with `--dmg`), `icm release linux` on Linux (a `.deb` and an
  AppImage) and `icm release windows` on Windows (an `.msi` and an NSIS installer). The other two
  targets exit 4 (`env.unsupported_host`). icm does not build on Windows yet, so the Windows
  installers wait for that.
- **CI:** `.github/workflows` runs the framework's and icm's checks and each platform's acceptance
  jobs. GitHub Actions does not run on the fork until the owner enables it.

### Platform services on Android and iOS

- **Safe area:** `iced::mobile::safe_area()` delivers the insets of the status bar, notch and
  navigation bar and the on-screen keyboard's height; `SafeArea::padding` pads a root container.
  Headless runs (`.ice` flows, `icm shot --headless`, `icm ui --headless`) give phone viewports
  their device's safe area.
- **Clipboard:** `iced::clipboard::read` and `write` use the system clipboard (`ClipboardManager`,
  `UIPasteboard`). On Android, Ctrl+C, X, V and A work in text fields from a hardware keyboard.
- **Dark mode:** the default theme, `system::theme()` and `system::theme_changes()` follow the
  system's mode.
- **Lifecycle:** `iced::mobile::lifecycle()` delivers the application's states to `update` as
  `LifecycleEvent`: `Foreground`, `Active`, `Inactive`, `Background` and `MemoryWarning`, with one
  meaning on both phones.

The template pads with the safe area and shows the appearance, copy and paste and the lifecycle.

### Lifecycle compatibility

`v0.14.1-mobile.1` marked `iced_winit::Lifecycle` (`iced::mobile::Lifecycle`) `#[non_exhaustive]`,
which broke every exhaustive `match` on it outside the crate. It is again an exhaustive enum of
`Suspended` and `Resumed` with its old derives, and `on_lifecycle` is unchanged. A wildcard arm
written for `v0.14.1-mobile.1` still compiles, with an `unreachable_patterns` warning, and can go.
New states go into `LifecycleEvent`, which is `#[non_exhaustive]`.

### Audit fixes

- Desktop and web behave as upstream again when a runtime is dropped: its futures keep running.
  Only Android and iOS end them.
- On iOS a line break counts as Return only when it is inserted alone, so dictation or a keyboard
  suggestion with a line break no longer submits a text field.
- `icm --dry-run` touches no device, browser or file on every path that has a plan; `check`, `ui`,
  `verify` and host `test` refuse it (exit 2). `--help` and `--version` end with a result object
  under `--json`.
- Exit 4 means the environment is not ready: follow each error's `fix.by` (doctor, agent or owner).
- icm reads `min_icm` and `schema` before the rest of `icm.toml`, so a file for a newer icm stops
  with `config.too_new` (exit 4) and the install command, not with `config.unknown_key` (exit 3)
  at the first key the newer icm added. The fix of `config.unknown_key` says the key may come from
  a newer icm. A `min_icm` must name a release (`0.14.1-mobile.N`); a plain `0.14.1`, which no icm
  meets, is `config.invalid`. Every fix command runs as it is: where icm cannot name the release
  to install, the command lists the release tags.
- `icm stop --shutdown` leaves running a simulator that another project's run booted, as it
  already did for the emulator, and `stop --all --shutdown` without a session considers only the
  simulator the project's runs use.
- Every file a run directory keeps from an app's or a tool's output (log copies, consoles, crash
  reports) redacts secret values on every platform, as stdout does, also where the output escapes
  them as JSON (inside a log line too) or percent-encodes them in any URL encoding. A value of a
  secret-named variable that starts with `/` counts unless it is a path. The live files in
  `target/icm/sessions/` stay the app's own output while it runs; a session keeps the values of
  its app's secret-named `--env` (on the web, the page's query) in a `secrets.json` (mode 0600)
  beside them, so `logs`, `shot` and `stop` from a shell without the secret redact it too. icm
  keeps no value of a secret-named variable it only inherits (as a desktop app does) in any file:
  a later command reads it from the running desktop app's environment, and the command that stops
  the app redacts its live files. `logs` of a desktop app that ended by itself, from a shell
  without such a variable, reads the run's redacted copies and warns
  `desktop.logs.secret_unknown`.
- Android: readiness and logs count only the app's own processes, the lifecycle suite tells a
  destroyed Activity from a live one after a rotation, and a rotation warns when the app is locked
  to one axis.
- Releases: `icm ledger mark-uploaded` refuses a release that cannot have been uploaded, literal
  signing secrets stay out of every finding and record, a value of a secret-named variable that
  the build baked into a shipped file fails `release.secret_in_artifacts` (the release is then not
  uploadable; `icm verify` checks it again), `upload.sh` runs `icm diagnose` when a store tool
  fails, `THIRD_PARTY_NOTICES` lists the Rust standard library, the AppImage leaves
  `libwayland-client` to the host, and `icm verify macos` accepts a stapled app or DMG.
