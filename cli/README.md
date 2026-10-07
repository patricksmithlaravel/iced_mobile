# icm

The iced_mobile app tool. The design is `docs/icm/DESIGN.md` (Appendix C
overrides earlier sections; Appendix D records what the code decided).

```sh
cargo install --locked --path cli          # from a fork checkout
cargo run --manifest-path cli/Cargo.toml -- explain --list
```

`cli/` is its own workspace with its own `Cargo.lock`; the root
`Cargo.toml` excludes it, so CLI dependencies never enter the framework's
lock. It links no iced crate.

## Checks for every change

```sh
cd cli
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Layout

| Module | What it owns |
|---|---|
| `lib.rs` | entry: parse, `--detach`, reporter, panic hook (exit 70), signal watchdog |
| `cli.rs` | the clap surface; later-phase commands parse as `External` |
| `commands/` | one file per command (`stop.rs` holds `stop` and `ps`, `build.rs` `build --all`); `mod.rs` dispatches, per platform where a platform owns the command (unimplemented ones exit 2 `usage.not_implemented`) |
| `context.rs` | `Ctx` (flags, reporter, env, deadline, host.toml, project) and `Project` (icm.toml + `cargo metadata` + paths) |
| `output/` | the output contract: `Reporter`, human lines (`human.rs`), run dirs (`rundir.rs`) |
| `catalogue.rs` | every check/error id with exit code, level, `by`, title, fix; `docs/explain/<id>.md` adds detail |
| `error.rs` | `IcmError` (one `errors[]` entry), `Check`, `Evidence`, `Fix`, `Diagnostic` |
| `exit.rs` | the stable exit codes |
| `process.rs` | the runner: stdin null, process groups, timeouts, file-backed output, redaction |
| `signals.rs` | SIGINT/SIGTERM/SIGHUP: record, kill registered groups |
| `plan.rs` | `Plan`/`Step`; `--dry-run` prints, `execute` runs |
| `config/` | icm.toml types, discovery, validation with `file:line` (`source.rs` keeps the spans) |
| `host.rs` | `~/.config/icm/host.toml` |
| `tools.rs` | discovery: Android SDK/NDK, JDK 17+, Xcode, Chrome, wasm-bindgen; `android_env`, `ndk_env` |
| `toolchain.rs` | the project's active toolchain and its installed targets |
| `cargo.rs` | `cargo metadata`, `Cargo.lock`, cargo invocations and JSON messages, deployment-target stamps |
| `deps.rs` | lockfile checks (`deps.single_iced`, ...) |
| `locks.rs` | per-project, per-platform locks |
| `screen.rs` | preview/px/pt coordinates for `shot` and `input` |
| `template.rs` | the embedded `examples/app` and how `icm new` fills it; `icm explain config.<key>` |
| `doctor/` | what each platform needs (`gather`) and the fixes `doctor --fix [--yes]` runs (`fix.rs`) |
| `managed.rs`, `simctl.rs` | the `icm-` simulator and AVD names, Android's per-user dirs; `simctl list -j` parsing |
| `session.rs` | `target/icm/sessions/<platform>.json` read generically: `ps` lists every record; `stop --all` (`commands/stop.rs`) hands the dev platforms' records to their own stop and ends any other by what it says |
| `image.rs` | PNG decode/encode, previews, cropping, blank detection |
| `platform/desktop/` | `build`/`run`/`shot`/`logs`/`stop desktop`: launch, readiness, window capture (`macos.rs` FFI, `linux.rs` X11), headless fallback (`headless.rs`), log records (`logs.rs`) |
| `platform/ios_sim/` | `build`/`run`/`logs`/`shot`/`stop`/`input ios-sim`: simulator choice (`simctl.rs`), bundle and plists (`bundle.rs`, `plist.rs`), Mach-O gates (`macho.rs`), App Store screenshots on a store-size simulator (`store.rs`), PNG preview and blank detection (`image.rs`), log normalization (`logs.rs`), the session file (`session.rs`); fake-tool tests in `tests/ios_sim.rs` |
| `platform/ios_device/` | `build`/`run`/`shot`/`logs`/`devices ios-device` through `xcrun devicectl` (`devicectl.rs` parses its JSON): device choice, development signing, install, the detached `--console` launch, readiness, screenshots; the session record that `icm stop` ends; fake-tool tests in `tests/ios_device.rs` |
| `preview.rs` | after a capture: PNG decode, `screen.preview.png`, blank detection (`run.screen_blank`) |
| `sessions.rs` | `target/icm/sessions/<platform>.json` records: write (mode 0600), list, alive (pid plus a command-line marker), terminate |
| `web/` | the web platform: build, wasm-bindgen and the site (`site.rs`); the detached session host (`host.rs`) with its std server (`server.rs`), headless Chrome over `--remote-debugging-pipe` (`cdp.rs`), the page recorder both use (`page.rs`) and console capture (`console.rs`); the control client (`client.rs`); viewports (`viewport.rs`); for releases, the release site (`release_site.rs`: hashed names, `index.html`, `_headers` and server snippets, icons, the wasm-opt flags, fonts inside a `.wasm`) and the serve check in headless Chrome (`smoke.rs`), driven by `release/web.rs` |
| `raster.rs` | screenshots as pixels: PNG decode/encode, the preview, blank detection (`raster::examine` for every capture) |
| `harness/` | the app's headless harness (`tests/icm.rs`, protocol 1): build it, run `icm-shot`/`icm-tree`/`icm-ice`, judge the answer; `libtest.rs` reads `cargo test` output |
| `signatures.rs` | known failure signatures (design §13.4) → `likely_causes`; `signatures::annotate(error, text, &Facts)` |
| `hooks.rs` | project hooks, `[checks] <platform>` scripts; every platform's `run` calls `hooks::run_for` once the app is up |
| `version.rs`, `buildinfo.rs`, `gitinfo.rs` | version ordering, what the build embedded, the default framework pin |
| `release/` | `release`, `verify`, `upload-commands`, `ledger`, `diagnose`: the core every target shares (`mod.rs`: preconditions, owner items, the `Pipeline` contract), `gates.rs` (`--sign none` and owner items), `dist.rs` and `manifest.rs` (`target/icm/dist/`, `artifacts.json`), `compile.rs` (release profiles by `--config`, `target/icm/release-target`, deployment-target stamps), `notices.rs` (THIRD_PARTY_NOTICES from `cargo metadata`, Fira Sans's OFL, the `release.notices` gate), `upload.rs` (`UPLOAD.md`, `upload.sh`), `owner_plans.rs` (the only file with upload or notarize argv), `ledger.rs`, `verify.rs`; one pipeline per target (`ios.rs` builds and gates the App Store `.ipa`, `android.rs` the Google Play `.aab`, `web.rs` the static site, and `macos.rs`, `windows.rs`, `linux.rs` the desktop installers below) and `fake.rs`, the stand-in pipeline of `icm __test release` |
| `ios/` | iOS device builds, shared by `icm release ios` (`release/ios.rs`) and `ios-device`: the device bundle and its gates (`bundle.rs`), `DT*` keys (`dt.rs`), identities (`identity.rs`), provisioning profiles (`profile.rs`), entitlements, codesign with the keychain watchdog, Mach-O symbols and UUIDs (`macho.rs`), the privacy scan, the dSYM gates, the `.ipa` (`ipa.rs`), an XML plist reader and SHA-1 |
| `pinned.rs` | the tools icm downloads itself (`tools.toml`, embedded: version, URL, size, sha256 per host): find, install with `--yes` (curl, sha256 check, unpack), doctor's WARN and `--fix --yes` |
| `policy.rs` | the dated store policy table (`policy/stores.toml`, embedded): the floor in force on a day, `env.policy_stale`, upcoming floors; `icm print policy` |
| `release/macos.rs`, `release/windows.rs`, `release/linux.rs` (and their directories), `release/desktop.rs` | the desktop release pipelines (phase 5). macOS: the identity and codesign (`macos/sign.rs`), Info.plist and entitlements (`macos/bundle.rs`), the `.app`, its zip and the DMG in two stages, `diagnose notarytool` (`macos/notary.rs`). Windows: `app.rc`, `app.wxs` and `installer.nsi` (`windows/files.rs`), the static-CRT build, the PE gates, `sign_command`. Linux: `DEBIAN/control`, the `.desktop` entry and `AppRun` (`linux/files.rs`), the `.deb` and the AppImage. Shared (`desktop.rs`, `desktop/`): the host check (`ICM_HOST_OS`), icons (PNG, iconset, ICO), and the PE, ELF and ar readers |
| `android/` | `build`/`run`/`stop`/`shot`/`logs`/`input`/`devices` for Android (`doctor android` is `doctor/`): APK pipeline, managed AVD, adb, logcat, session; the release bundle's layout, tool-output parsing and store gates (`bundle.rs`, used by `release/android.rs`), `run --from-aab`, and `test --on android --lifecycle` (`lifecycle.rs`) (`android/mod.rs` has the module map) |

## Writing a command

1. Add the variant and its `Args` to `cli.rs` (two examples in `after_help`).
2. Add `commands/<name>.rs` and route it in `commands/mod.rs`.
3. Get what you need from `Ctx`: `ctx.project()?` (attaches the run
   directory under `target/icm`), `ctx.host()?`, `ctx.env`.
4. Run processes with `ctx.step(name, &cmd)` (a `STEP` line, `step` events,
   `steps/NN-<name>.log`) or build a `Plan` and honour `ctx.dry_run()`.
   Cargo builds go through `ctx.cargo(name, &Invocation, env)`, which turns
   compiler messages into `diagnostic` events and failures into `build.*`.
   Apple builds call `ctx.deployment_target(...)` first and write the stamp
   after success.
5. Report with `ctx.rep`: `check(Check::pass|warn|fail(...))` for findings
   (a FAIL here is non-blocking: exit 1 unless something blocks),
   `artifact(kind, path)`, `ready(...)`, `next(cmd, why)`, `set(key, value)`
   for result fields, `latest(platform)`.
6. Return `Err(IcmError::new(CheckId::..., detail))` for the failure that
   stops the command; it becomes `errors[0]` and sets the exit code.

Never print to stdout yourself: in human mode stdout carries only protocol
lines, with `--json` only NDJSON. Content commands (`print`, `explain`) use
`ctx.rep.content(text)`.

## Adding an id

Add a line to the `catalogue!` block in `catalogue.rs` (id, exit code when it
fails, default level, who fixes it, title, fix). `icm explain <id>` renders
it. For a common failure, also write `docs/explain/<id>.md` (embedded by
`build.rs`; a test checks every file names a real id).

## Testing

- Unit tests live next to the code.
- `tests/cli.rs` runs the binary. Each test sets `ICM_CACHE_DIR` and
  `ICM_HOST_CONFIG` to a temp dir and copies fixtures from `tests/fixtures/`.
- `tests/desktop.rs` runs, logs, captures and stops `fixtures/desktop`, a
  windowless stand-in app (`--env ICM_FIXTURE=ready|panic|exit|hang`), and
  kills whatever it started.
- `tests/web.rs` drives the web pipeline against real headless Chrome with
  a fake cargo and wasm-bindgen (`ICM_TOOL_CARGO`, `ICM_TOOL_WASM_BINDGEN`)
  whose JavaScript "app" speaks `ICM_EVENT`; it skips without Chrome or the
  wasm32 target.
- `icm __test <scenario>` (hidden) exercises the core end to end: `sleep`
  (timeouts, signals, `--detach`), `panic`, `fail <id>`, `checks`, `plan`,
  `project`, `lock`, `deployment`, `busy` (the signal watchdog), `hooks
  <platform>` (the project's `[checks]` scripts, without a device).
- Harness commands (`test`, `shot --headless`, `ui`) run against a fake
  cargo and a fake harness in `tests/harness.rs`; the real one is the
  template's.
- `tests/macos_windows.rs` calls CoreGraphics on its own: next to unit
  tests that spawn children, those calls got the children killed.
- Fake tools: `ICM_TOOL_<NAME>=/path/to/script` replaces any external tool
  (`xcrun`, `adb`, `cargo`, ...).
- `tests/ci.rs` checks the fork's CI in `.github/`: the release tag rule
  (`.github/ci/tag.sh`), that the workflows run AGENTS.md's check commands
  and only scripts that exist, and that nothing there formats with
  `--all`, reads a secret or uploads.
- `tests/project.rs` covers `new`, `check`, `doctor`, `stop`/`ps` and
  `explain config.<key>`. `check` compiles `tests/fixtures/checkapp`, whose
  `iced` is a local stand-in, offline in a second; `doctor` runs against a
  fake SDK, JDK, Rust sysroot (`ICM_TOOL_RUSTC`/`RUSTUP`) and Xcode
  (`DEVELOPER_DIR`, `ICM_TOOL_XCRUN`/`XCODEBUILD`), so no test downloads
  anything or touches a real simulator, emulator or `~/.android`. Its
  fake `curl` copies `file://` URLs and refuses the network, so pinned
  tools install from local files (`ICM_TOOLS_TOML`).
- `tests/release.rs` runs the release core through `icm __test
  release|verify <target>` (a stand-in pipeline that writes a small file):
  owner items and `--sign none`, `artifacts.json`, `dist/latest`,
  `UPLOAD.md`, `upload.sh` (run with a fake `xcrun` and `icm`), the
  ledger, verify's hash check, THIRD_PARTY_NOTICES on `fixtures/release`
  (path dependencies with licence files, a build and a dev dependency, a
  stand-in iced and iced_graphics with Fira Sans's licence), and every
  target's `--dry-run` planning without writing a file.
- `tests/ios_release.rs` runs `icm release ios`, `verify ios` and
  `diagnose altool` against fake `cargo`, `xcrun`, `codesign`, `security`,
  `xcodebuild` and `sw_vers` (`fixtures/fake-ios/`), a synthetic device
  Mach-O and fake profiles, with the real `plutil`, `ditto`, `zip` and
  `unzip` (macOS only).
- `tests/android_release.rs` runs `icm release|verify android` and
  `icm diagnose play` against a fake cargo (synthetic ELF libraries) and
  stand-ins for aapt2, bundletool (`java -jar`), jar, keytool, jarsigner,
  apksigner and zipalign that log their argv and write real zips with
  `zip`/`unzip` (it skips without them).
- `tests/web_release.rs` runs `icm release web` and `icm verify web` on
  `fixtures/web-release` with a fake cargo build, wasm-bindgen and
  wasm-opt (real `cargo metadata`, `rustc --print cfg` and gzip) and real
  headless Chrome for the serve check; `verify web --url` goes to a test
  server that serves the site with a right and a wrong `.wasm` type.
- `tests/desktop_release.rs` runs the desktop pipelines on `fixtures/release`.
  - On macOS: a real ad-hoc-signed release, its DMG and `icm verify macos`
    (cargo, dsymutil, iconutil, codesign, hdiutil). Then the signed two-stage
    flow, with fake `security`, `codesign`, `spctl` and `xcrun stapler` as the
    owner's identity and Apple's notary service.
  - Everywhere: Windows and Linux with `ICM_HOST_OS` and fake tools (cargo's
    build step, rc, wix, makensis, signtool, the signing command; dpkg-deb,
    dpkg-shlibdeps, appimagetool, lintian).
  - When installed, the real makensis and dpkg-deb build the generated
    installer and package.
  - `ICM_KEYCHAIN` always names a file of the test, so the user's keychains
    are never searched.

## Environment

| Variable | Effect |
|---|---|
| `ICM_JSON=1` | same as `--json` |
| `ICM_CONFIG`, `ICM_TIMEOUT` | defaults for `--config`, `--timeout` |
| `ICM_CACHE_DIR` | icm's cache (runs outside a project, pinned tools) |
| `ICM_HOST_CONFIG` | the host.toml to read |
| `ICM_TOOL_<NAME>` | the path of an external tool |
| `ICM_CHROME` | the Chrome executable |
| `ANDROID_USER_HOME`, `ANDROID_AVD_HOME` | where AVDs live (default `~/.android`, `~/.android/avd`); tests point them at a temp dir. The debug keystore sits next to host.toml (`android/debug.keystore`) |
| `ICED_TEST_BACKEND` | the backend the app's harness draws with (default `tiny-skia`) |
| `ICM_BUILD_FRAMEWORK` | at build time: force the default framework pin (`tag:`/`rev:`/`path:`) |
| `ICM_TOOLS_TOML` | a pinned-tools table to use instead of the embedded `tools.toml` (a mirror; the tests serve `file://` URLs) |
| `ICM_TODAY` | `YYYY-MM-DD`: the day the store policy table is read for (icm's tests) |
| `ICM_KEYCHAIN` | the keychain Apple signing (iOS and macOS) searches instead of the user's search list (overrides host.toml `signing_keychain`) |
| `ICM_PROVISIONING_PROFILES` | `:`-separated directories searched for provisioning profiles instead of Xcode's |
| `ICM_CODESIGN_TIMEOUT` | seconds before a codesign that waits for a keychain dialog is stopped (default 60) |
| `ICM_HOST_OS` | `macos`, `windows` or `linux`: the host the desktop release pipelines assume (icm's tests) |
| `ICM_LINUX_LIB_DIRS` | `:`-separated directories the AppImage's bundled libraries are copied from (icm's tests; default: the host's library directories) |
| `ICM_RUN_ID`, `ICM_RUN_DIR`, `ICM_RUN_ROOT`, `ICM_DETACHED` | internal: a detached child's run |
