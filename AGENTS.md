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
real simulator or emulator. `tests/web.rs` uses headless Chrome when it is installed and skips
otherwise.

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
- Do not rely on `--dry-run` or `icm print plan` to keep a command harmless yet. Only `new`,
  `doctor`, `selftest`, `build` and `run` on desktop and ios-sim, and `shot` and `stop` on desktop
  honour it. Everything else runs for real, including every web and Android command, `check`,
  `test`, `build --all` and `stop --all`.

## Where to look for platform behaviour

- **What an app sees:** `src/mobile.rs` and `docs/agents/limitations.md`.
- **The shell:** `winit/src/lib.rs` handles Android suspend and resume, surfaces, Activity
  destruction, iOS exit and window rules, and the lifecycle hook. `winit/src/conversion.rs` maps
  keys, including Return and Tab on Android and iOS. `winit/src/icm.rs` has the event protocol.
  `winit/src/scene.rs` puts iOS windows into the scene.
- **winit itself:** `vendor/winit/src/platform_impl/android/` and `.../ios/`.
- **What icm does per platform:** `cli/src/android/`, `cli/src/platform/ios_sim/`, `cli/src/web/` and
  `cli/src/platform/desktop/`. Pipelines are in `docs/icm/DESIGN.md` §10, and the output contract is
  in §4.
