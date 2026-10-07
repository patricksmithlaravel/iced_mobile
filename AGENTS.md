# AGENTS.md: working on iced_mobile

This guide is for coding agents and contributors who change this repository: the framework, `icm`
or the template. To build an app with iced_mobile, read the `AGENTS.md` that `icm new` writes into
every app instead. Its source is [examples/app/AGENTS.md](examples/app/AGENTS.md).

## What this repository is

- A fork of [iced](https://github.com/iced-rs/iced), based on upstream's 0.14 branch at
  `38237dd29`. Every later commit belongs to the fork. The branch's manifest says `iced` 0.14.1,
  but upstream never published that version (crates.io stops at `iced` 0.14.0). Upstream declined
  mobile support, so this is an independent project.
- It adds Android and iOS support to the framework, `icm` (the CLI in `cli/`) and the app template
  (`examples/app/`).
- `main` is the only development branch. `tawara/0.14-mobile` is kept as it is for the Tawara
  wallet's pin. Do not commit to it.
- Releases are tags `v0.14.x-mobile.N`, and one tag releases the framework and `icm` together.

## Layout

| Path | What it is |
|---|---|
| `src/mobile.rs` | `iced::mobile` and `iced::android_main!`: the app-facing mobile API. Its docs are the main mobile documentation (features, manifest and plist keys, logs, lifecycle, Activity destruction, known limitations). |
| `winit/src/lib.rs` | `iced_winit`: `set_android_app`, `activity_destroyed`, `Lifecycle`, `on_lifecycle`, and the Android and iOS handling in the event loop |
| `winit/src/icm.rs` | the `ICM_EVENT` protocol (start, ready, lifecycle, panic, warning, exit) that launchers read |
| `winit/src/scene.rs`, `winit/src/ios_sdk.rs` | iOS: windows in the UIKit scene, and the SDK the app was linked with |
| `vendor/winit/`, `vendor/patches/` | winit 0.30.13 with Android fixes, its own workspace; `vendor/winit/PATCHES.md` lists the patches |
| `core/`, `widget/`, `runtime/`, `graphics/`, `renderer/`, `wgpu/`, `tiny_skia/`, `futures/`, `test/`, ... | the upstream crates, with small mobile changes |
| `cli/` | `icm`: its own workspace and `Cargo.lock`. Links no iced crate. `cli/README.md` maps its modules. |
| `examples/app/` | the template `icm new` copies; a member of the root workspace. `cli/build.rs` embeds it. |
| `docs/agents/limitations.md` | known limitations; generated app `AGENTS.md` files embed everything below its marker line |
| `docs/icm/DESIGN.md` | icm's design. Appendix C overrides earlier sections, and Appendix D records what the code decided. Where code and design disagree, the code and its tests win. |
| `docs/mobile/review-2026-10-06.md` | a dated snapshot review; many items are fixed since, so check the code |

## Checks

Run them before every commit. At the root (the framework):

```sh
cargo fmt -- --check
cargo check -p iced
cargo check -p iced --target aarch64-apple-ios-sim
cargo check -p iced --target aarch64-linux-android
cargo check -p iced --target wasm32-unknown-unknown
ICED_TEST_BACKEND=tiny-skia cargo test --workspace
```

- Use plain `cargo fmt`, **never `cargo fmt --all`**: `--all` also formats path dependencies and
  would rewrite `vendor/winit` with this repository's settings.
- The targets come from
  `rustup target add aarch64-apple-ios-sim aarch64-linux-android wasm32-unknown-unknown`. The
  matrix is run on macOS. The Android check needs no NDK.
- For a crate you changed, also run `cargo clippy -p <crate> --no-deps -- -D warnings`. When the
  change is gated to a mobile target, add `--target` for it. Alone, `iced_winit` enables no Android
  activity, so on Android add `--features android-native-activity`, or lint `-p iced`, which
  enables one by default:

  ```sh
  cargo clippy -p iced_winit --no-deps --target aarch64-apple-ios-sim -- -D warnings
  cargo clippy -p iced_winit --features android-native-activity --no-deps --target aarch64-linux-android -- -D warnings
  ```

In `cli/`, for any change to icm:

```sh
cd cli
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

`cargo test` in `cli/` uses fake tools and temporary directories. It downloads nothing and touches no
real simulator or emulator. `tests/web.rs` and `tests/web_release.rs` use headless Chrome when it is
installed and skip otherwise.

### CI

GitHub Actions runs these checks and more: `.github/workflows/framework.yml` (the commands above,
the template on every platform, tests on macOS, Linux and Windows, the `rust-version` check) and
`.github/workflows/icm.yml` (icm's checks, the `cargo install --git` check, a fresh resolve of a new
app, the desktop releases with `--sign none`, and the tag check). The jobs' logic is in
`.github/ci/*.sh`, which run locally too. `cli/tests/ci.rs` fails when the workflows stop running a
command listed above, so change both together. `icm-ios.yml`, `icm-android.yml`, `icm-web.yml` and
`icm-desktop.yml` run each platform's acceptance and real-host jobs when `cli/` or the template
changes; the Windows jobs wait for the repository variable `ICM_WINDOWS_HOST`, since icm does not
build on Windows yet. Actions has not run on the fork yet: the owner enables workflows on its Actions
tab. Never add a step that uploads, publishes or reads a secret.

### Acceptance scripts

These are end-to-end runs from the design's §18. Each step prints PASS, FAIL or SKIP, and the script
exits 1 if any step failed. Outputs go to `$ACCEPT` (a new temporary directory by default).

- **`cli/tests/accept/phase0.sh`** covers the framework prerequisites: the check matrix, the
  template's deliverables, the `ICM_EVENT` opt-in, the `.ice` flows and a headless screenshot.
  - It needs macOS, `/usr/bin/jq` and the three targets above.
  - It builds in the checkout's own `target/` (it runs `./target/debug/app`), so do not set
    `CARGO_TARGET_DIR`.
  - It opens the template's desktop window twice for a few seconds.
  - The MSRV step runs only when the workspace's `rust-version` toolchain is installed. The tag step
    runs only with `ICM_ACCEPT_OWNER_PUSHED=1`.
- **`cli/tests/accept/phase1.sh`** covers icm's whole dev loop on desktop, web, the iOS Simulator and
  an Android emulator. It takes about ten minutes with a warm cache, so run it in the background.
  - It needs macOS with Xcode, the Android command-line tools, Chrome, `/usr/bin/jq` and the network:
    it runs `icm doctor --fix --yes`, which downloads what is missing.
  - It installs icm from the checkout into `$ACCEPT/icm`. icm's cache, `host.toml` and Android's AVD
    home also live in `$ACCEPT`. Outside it: the managed iOS simulator (`icm-iphone-*`) in the user's
    device set, the emulator's `~/.android/modem-nv-ram-<port>`, cargo's registry cache, and whatever
    `doctor --fix --yes` installs (SDK packages and the NDK in the user's Android SDK, the template's
    rustup toolchain and targets, Homebrew's openjdk@21, the iOS platform).
  - It stops early if any Android device other than icm's managed `icm-api<N>` emulator is online,
    an `icm-test-*` emulator included.
  - Its Tawara steps clone `$TAWARA` (default `~/Tawara-mobile`) read-only into `$ACCEPT`. Set
    `TAWARA` to a path that does not exist to skip them.
- **`cli/tests/accept/phase2.sh`** covers the App Store release without the owner's signing assets:
  `icm release ios` stopping for the owner, `--sign none` with its IPA, gates and `verify`, an RGBA
  icon, broken inputs, `diagnose altool`, App Store screenshots and the ios-device commands that
  need no device. Then the signed path with throwaway material: a release signed with a self-signed
  `Apple Distribution: icm test (ICMTEST001)` identity and a fake App Store profile, every gate
  PASS, exit 9 for the untrusted certificate, and `verify` on its IPA. It takes a few minutes and
  needs macOS with Xcode 26 or later and `/usr/bin/jq`.
  - icm only reads signing assets, and the script keeps the user's out of its reach: unless set,
    `ICM_KEYCHAIN` names a keychain file that does not exist and `ICM_PROVISIONING_PROFILES` an
    empty directory in `$ACCEPT`. The test identity lives in a temporary keychain made by
    `test-identity.sh`, which never joins the search list (a step checks it) and is deleted at the
    end; the profile is a CMS envelope signed by a throwaway key.
  - It creates the managed `icm-iphone-<n>-pro-max-ios-<version>` simulator when it is missing and
    shuts it down. The owner's signed release, upload and a physical iPhone are SKIP
    (`ICM_ACCEPT_DEVICE=1` runs on a connected, provisioned iPhone).
- **`cli/tests/accept/phase3.sh`** covers the Google Play release and the lifecycle suite: a signed
  release of the template with a throwaway upload key (random password in `ICM_TEST_STOREPASS`)
  and its smoke install, the unset-password owner exit, `--sign none --apk`, the universal APK
  installed on the managed emulator, `verify`, `run --from-aab`, `diagnose play` and
  `test --on android --lifecycle`. Its setup and isolation are phase1.sh's; the owner's Play
  Console uploads are SKIPs. It builds two ABIs in release, so run it in the background.
- **`cli/tests/accept/phase4.sh`** covers the web release: `icm release web` on a new template app,
  `icm verify web`, and `icm verify web --url` against a local static server (a right and a wrong
  `.wasm` type). It deploys nothing.
  - It needs Chrome, `python3`, `/usr/bin/jq` and the network: `icm doctor web --fix --yes` creates
    the app's lock and installs the wasm-bindgen CLI and the pinned `wasm-opt` into icm's cache
    (`$ACCEPT/cache` unless `ICM_CACHE_DIR` is set).
- **`cli/tests/accept/phase5.sh`** covers the desktop releases of the host it runs on with the
  template app, and takes a few minutes. On every host the other two targets are refused (exit 4).
  - macOS: an unsigned release and its DMG, which it mounts and verifies, and the app launched. A
    release signed with a throwaway self-signed identity (`test-identity.sh`) in a temporary
    keychain, which never joins the user's search list (the script checks the list is unchanged)
    and is deleted at the end. The owner's notarization steps are SKIP.
  - Linux: the `.deb` and the AppImage (`--sign none`), `linux.glibc_floor`, `icm verify linux`,
    `dpkg -i` and `dpkg -r` (as root, or with `ICM_ACCEPT_INSTALL=1` and passwordless sudo), and
    the AppImage's first frame under `xvfb-run` or the session's display.
  - Windows (Git Bash): the `.msi` and the NSIS installer (`--sign none`), `icm verify windows`,
    and both installed and uninstalled with `ICM_ACCEPT_INSTALL=1`. icm does not build on Windows
    yet, so these steps wait for that.
  - The Windows and Linux branches have not run yet; `.github/workflows/icm-desktop.yml` runs the
    same checks inline on those hosts.

## Rules

- **Keep the API that Tawara pins stable.** `iced_winit::set_android_app`, `iced_winit::on_lifecycle`
  and `iced_winit::Lifecycle`, with their `iced::mobile` re-exports. You may add to them, but do not
  rename them, remove them or change their signatures.
- **Gate mobile behaviour** behind `target_os = "android"` / `target_os = "ios"`. Desktop and wasm
  behave as upstream does unless an app or a launcher opts in: a feature, `iced::mobile::init_logger`,
  or `ICM_EVENTS=1` (`?icm_events=1` on the web).
- **Framework crate versions stay upstream's** (`iced` 0.14.1, `iced_widget` 0.14.2), so
  `[patch.crates-io]` keeps matching. Only `cli/Cargo.toml` carries `-mobile.N`, and it must equal
  the release tag without its `v`.
- **Change `vendor/winit` only for fixes that belong upstream.** Each change also goes into a patch
  file in `vendor/patches/` and a row in `vendor/winit/PATCHES.md`. Anything iced-specific goes in
  `winit/` (`iced_winit`). Apps must never need crates.io winit.
- **The template may use only crates already in the root `Cargo.lock`** (`iced`, `iced_test`, `log`).
  After changing it, `git diff Cargo.lock` must show no new package. Generated apps follow the
  template, so check `icm new` output after template changes (`cli/tests/project.rs`).
- **Keep the docs in step with behaviour:**
  - A change to icm's behaviour updates `docs/icm/DESIGN.md` in the same commit, in Appendix D for
    decisions the code makes.
  - A new check or error id goes into `cli/src/catalogue.rs`, plus `cli/docs/explain/<id>.md` for a
    common one.
  - A limitation that is fixed or found updates `docs/agents/limitations.md` and the Known
    limitations in `src/mobile.rs`.
- **Commit messages:** `area: imperative summary`, with comma-separated areas when a commit spans
  several (`icm`, `iced`, `winit`, `examples/app`, `docs`, `workspace`, `graphics`, ...). The body
  says why the change is needed and what it changes, and ends with what was verified (the commands
  and, for device work, what was seen). Add no co-author, "generated with" or other tool
  attribution lines, in commits or anywhere else.
- **Pushing and tagging are the owner's actions.** Never move a tag.

## Devices

- icm creates devices named `icm-*`: the `icm-api<N>` AVD and the `icm-iphone-*` simulators. It
  boots and shuts down only those, and sets up only those at boot. But `icm run android` sets debug
  properties on whatever device it targets, and `icm input android appearance|rotate|font-scale`
  changes that device's system settings.
- Name any throwaway simulator or AVD you create `icm-test-*`, and delete it when done. icm creates
  or boots an `icm-test-*` device only when you name it (`--avd`), and `stop --shutdown` leaves it
  running unless icm booted it. But `icm run android` uses one, and sets it up like its own emulator (animations off, stay
  awake), when it is the only online device. Keep one online only while you test, or pass
  `--device`. For Android, point `ANDROID_USER_HOME` and
  `ANDROID_AVD_HOME` at a temporary directory, as `phase1.sh` does.
- Never boot, wipe, delete or install onto the owner's other AVDs, simulators or phones. Check
  `adb devices` first: when its own emulator is not up, `icm run android` installs onto the single
  online Android device.
- `--dry-run` (or `icm print plan <cmd…>`) prints the plan and touches no device, browser or
  file for `new`, `doctor`, `release`, `ledger mark-uploaded`, and `build`, `run`, `stop`, `shot`,
  `logs`, `input` and `devices android` on every platform (`build --all` and `stop --all`
  included). `check`, `test`, `ui`, `shot --headless` and `verify` ignore it and run for real,
  except `test --on android --lifecycle`, which drives a device and honours it.
- `icm test --on android --lifecycle` changes the device's night mode, rotation, font scale and
  font weight, sends Home and Back and kills the app's process (`am kill`); it restores the
  settings at the end. `icm release android` installs the release onto icm's running emulator (or
  the device `$ANDROID_SERIAL` or host.toml `android.device` names) unless `--no-smoke`.

## Where to look for platform behaviour

- **What an app sees:** `src/mobile.rs` and `docs/agents/limitations.md`.
- **The shell:** `winit/src/lib.rs` handles Android suspend and resume, surfaces, Activity
  destruction, iOS exit and window rules, and the lifecycle hook. `winit/src/conversion.rs` maps
  keys, including Return and Tab on Android and iOS. `winit/src/icm.rs` has the event protocol.
  `winit/src/scene.rs` puts iOS windows into the scene.
- **winit itself:** `vendor/winit/src/platform_impl/android/` and `.../ios/`.
- **What icm does per platform:** `cli/src/android/`, `cli/src/platform/ios_sim/`, `cli/src/web/` and
  `cli/src/platform/desktop/`. Pipelines are in `docs/icm/DESIGN.md` §10, and the output contract is
  in §4. Physical iPhones: `cli/src/platform/ios_device/`.
- **Releases:** the core and one pipeline per target in `cli/src/release/` (§11, §12; Appendix D
  has what each pipeline decided). iOS adds `cli/src/ios/`, Android `cli/src/android/bundle.rs`, the
  web `cli/src/web/release_site.rs` and `smoke.rs`, and the desktop `release/{macos,windows,linux}/`
  and `release/desktop/`.
