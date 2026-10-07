<div align="center">

<img src="docs/logo.svg" width="140px" />

# iced_mobile

[iced] on the desktop, the web, **Android and iOS**, plus `icm`, a tool that creates,
runs and tests an app on all four from one Rust codebase.

</div>

iced is a cross-platform GUI library for Rust in the style of [The Elm Architecture]: state,
messages, `update` and `view`. iced_mobile is upstream iced's 0.14 branch with Android and iOS
support added. The same `update`, `view` and `run` work on a phone, and `icm` creates an app from a
template, then checks, builds, runs, screenshots, logs and tests it on the desktop, the web, the iOS
Simulator and Android. Publishing to the App Store, Google Play, web hosts and desktop installers is
planned.

**Status: experimental.** The first release is `v0.14.1-mobile.1`. This is an independent fork of
[iced-rs/iced](https://github.com/iced-rs/iced), not endorsed by the iced project. Upstream has
declined mobile support ([iced-rs/iced#302](https://github.com/iced-rs/iced/issues/302)), so the
mobile work lives here. Report problems with this fork here (see [Contributing](#contributing)),
not upstream.

## Platforms

| Platform | What works | Main limitations |
|---|---|---|
| Desktop (macOS, Linux, Windows) | Behaves as upstream iced on its 0.14 branch. `icm run desktop` builds, launches, screenshots and reads logs on macOS and Linux hosts. | icm cannot build installers yet, and does not run on Windows hosts yet. |
| Web (wasm32) | Behaves as upstream iced. `icm run web` builds with wasm-bindgen, serves the app locally and drives it in headless Chrome (screenshots, console logs, input). | No `icm release` for the web (a site to deploy) and no hosting yet. |
| iOS (iPhone) | UIKit scene life cycle (required with the iOS 27 SDK), suspend/resume hook, logging to the unified log, Fira Sans by default with fallback to system fonts. `icm run ios-sim` builds the `.app`, installs and launches it on a managed simulator, screenshots it and reads its logs. `icm release ios` builds a signed, store-checked `.ipa` with its dSYM and the owner's upload commands; `icm run ios-device` installs and launches on an iPhone through `devicectl`. | Device runs and signed releases need the owner's certificates and profiles, and device runs are tested with stand-in tools only so far. No input on a physical iPhone. No tap, swipe, text, key or rotate input on the simulator yet (appearance, font scale, background and foreground work), so test the UI with `.ice` flows and `icm ui --headless`. No iPad. |
| Android | `NativeActivity` (no Java code), suspend/resume, a destroyed Activity (the app starts over in the next one), logging to logcat, Fira Sans with system-font fallback. `icm run android` builds and signs a debug APK and runs it on a managed emulator or a device connected over adb, with screenshots, logs and tap, swipe, text and key input. | No Google Play bundles yet. Text input gets key events only (no composition). One window. |

Both phones: no safe-area insets, no clipboard and no dark-mode detection yet. Apps cannot quit
themselves, and some touch interactions differ from the desktop. The full list, with workarounds,
is in [docs/agents/limitations.md](docs/agents/limitations.md).

## Quick start with icm

```sh
# needs Rust 1.89 or newer
cargo install --locked --git https://github.com/patricksmithlaravel/iced_mobile --tag v0.14.1-mobile.1 icm

icm new hello && cd hello       # the template app, pinned to this release
icm doctor --fix --yes          # check the machine and install what is missing (downloads)
icm check                       # compile every platform without a device
icm run desktop                 # or web, ios-sim, android: build, launch, wait for the first frame, screenshot
icm logs desktop --level warn   # the running app's logs (name the platform you ran)
icm test                        # unit tests and tests/flows/*.ice, headless
icm shot --headless --all-viewports   # screenshots at phone and desktop sizes, no device
icm stop --all --shutdown       # stop the apps and shut down icm's simulator and emulator
```

`icm run` returns while the app keeps running and prints the path of the screenshot. `icm --help`
lists every command.

- **doctor:** `icm doctor --fix` without `--yes` makes only local changes. In a new app it exits 4,
  because creating `Cargo.lock` needs `--yes`. Exit 4 from doctor means `--fix --yes` has more to
  install. For phone work only, name the platforms: `icm doctor ios-sim android --fix --yes`.
- **Which Android device:** when icm's own emulator is not running and exactly one other device or
  emulator is online (your phone, or one of your own AVDs), `icm run android` installs onto that
  one. icm boots its emulator only when nothing else is online. Check `icm devices android` first,
  or pass `--device <serial>` or `--avd icm-api36`.
- **Stopping:** `icm stop --all` stops the apps and leaves the simulator and emulator booted for
  the next run. `--shutdown` also turns off the ones icm manages (but not an emulator it booted for
  another app).
- **One target directory per app:** icm keeps its runs and sessions in the Cargo target directory
  (`target/icm`). Do not share one `CARGO_TARGET_DIR` between apps, or `icm logs`, `ps` and `stop`
  see the other app's sessions.

What the host needs (`icm doctor` checks each item and says how to fix it):

- Rust through rustup. The template pins its toolchain in `rust-toolchain.toml`, and
  `icm doctor --fix --yes` installs it with the targets it needs.
- **iOS:** macOS with Xcode 26 or newer, with its licence accepted. `--fix --yes` downloads the iOS
  Simulator runtime (about 8 GB) if it is missing.
- **Android:** the Android SDK command-line tools (for example
  `brew install --cask android-commandlinetools`). `--fix --yes` installs the SDK packages, an NDK
  (r28 or newer) and the managed emulator, and OpenJDK 21 through Homebrew when no JDK 17+ is
  found. sdkmanager installs nothing until the SDK licences are accepted, and you accept them
  yourself, so a fresh machine takes three steps:
  1. Run `icm doctor --fix --yes`. It installs the JDK if needed and reports
     `env.licenses_not_accepted`.
  2. Run the `sdkmanager --sdk_root=<sdk> --licenses` command it prints. sdkmanager needs
     `JAVA_HOME` set to a JDK 17 or newer. Homebrew's openjdk@21 is not on `PATH`, so set it
     first: `export JAVA_HOME=$(brew --prefix openjdk@21)/libexec/openjdk.jdk/Contents/Home`.
  3. Run `icm doctor --fix --yes` again to install the SDK packages, the NDK and the emulator.
- **Web:** Google Chrome or Chromium. `icm run web` runs the app in headless Chrome (readiness,
  screenshots, console logs, input) and fails without it. `--fix --yes` installs the
  `wasm-bindgen` CLI that matches the app's lockfile.
- **Desktop screenshots on macOS** need Screen Recording permission for the app that runs icm.
  Without it, icm renders the view headlessly instead and warns.
- **Release tools:** doctor warns while a tool the platform's releases need is missing, and
  `--fix --yes` downloads it at the version, size and sha256 pinned in
  [cli/tools.toml](cli/tools.toml): bundletool for Android, binaryen's `wasm-opt` for the web,
  appimagetool and the AppImage runtime for Linux. The dev loop does not need them.

icm runs on macOS and Linux hosts and is tested on macOS. Windows hosts are not supported yet.
[cli/README.md](cli/README.md) describes the tool's internals.
[docs/icm/DESIGN.md](docs/icm/DESIGN.md) is icm's design, written before implementation: Appendix C
corrects it, Appendix D records what the code decided, and where they differ the code wins.

## Using the framework without icm

Take every iced crate from this repository at one tag, written the same way on every line.
Different spellings of the source make Cargo build two copies of iced:

```toml
[dependencies]
iced = { git = "https://github.com/patricksmithlaravel/iced_mobile", tag = "v0.14.1-mobile.1" }

[dev-dependencies]
iced_test = { git = "https://github.com/patricksmithlaravel/iced_mobile", tag = "v0.14.1-mobile.1" }
```

`src/lib.rs` runs the app, and defines Android's entry point:

```rust
pub fn run() -> iced::Result {
    iced::mobile::init_logger(); // logcat, os_log, stderr or the browser console
    iced::run(update, view)
}

iced::android_main!(run); // defines `android_main` on Android, nothing elsewhere

// `update` and `view` as in any iced app
```

`src/main.rs` is the desktop, web and iOS executable:

```rust
fn main() -> iced::Result {
    myapp::run() // never returns on iOS
}
```

- **Android** loads the library as a `cdylib`. Build it with
  `cargo rustc --lib --crate-type cdylib --target aarch64-linux-android` using the NDK's linker
  (`icm print env android` prints the environment). In the manifest, the activity is
  `android.app.NativeActivity`, its `android.app.lib_name` meta-data is the library's name, it uses
  `android:launchMode="singleTask"`, and it lists every `android:configChanges` value. Without
  that list, rotation, a dark-mode switch or a font-scale change restarts the app.
- **iOS** apps built with the iOS 27 SDK need a `UIApplicationSceneManifest` in `Info.plist`, or
  UIKit stops them at launch.
- **Do not depend on winit from crates.io.** iced uses the winit vendored in this repository, and a
  second copy clashes with it: duplicate Objective-C classes on iOS, and on Android a second winit
  whose types iced never sees (only `AndroidApp` is shared). Use `iced::mobile::AndroidApp`, or
  `iced_winit::winit` from the same git source.
- Leave iced's default features on. With `default-features = false`, list a renderer and an
  executor (`wgpu`, `tiny-skia`, `thread-pool`) and the mobile features again:
  `android-native-activity`, `mobile-logger`, `mobile-fira-sans`, `mobile-system-fonts` (see the
  example in `iced::mobile`).

[`src/mobile.rs`](src/mobile.rs) documents all of this in full, in the module `iced::mobile`
(`cargo doc -p iced --no-deps --open`): the features, the manifest and plist keys, logging, the
lifecycle, what happens when Android destroys an Activity, and the known limitations. icm generates these
native files from `icm.toml` on every build. Its generators are working references: the Android
manifest in [cli/src/android/manifest.rs](cli/src/android/manifest.rs), and `Info.plist` in
[cli/src/platform/ios_sim/plist.rs](cli/src/platform/ios_sim/plist.rs). Crates that depend on
crates.io `iced` 0.14 can be pointed at the fork with `[patch.crates-io]`, since the framework
crates keep upstream's version numbers.

## For coding agents

- **Building an app:** every app from `icm new` has an `AGENTS.md` (from
  [examples/app/AGENTS.md](examples/app/AGENTS.md) and
  [docs/agents/limitations.md](docs/agents/limitations.md)). It gives the check, run, look, test
  loop, the rules that fail silently when broken, and the known limitations of the pinned release.
- **Output contract:** with `--json`, stdout is NDJSON and the last line is always the result object
  (`ok`, `exit`, `summary`, `errors[]`, `artifacts`, `next`). `-q` prints only that line. Each error
  has an `id`, `evidence`, and a `fix` whose `by` says who acts: `agent`, `doctor`, `doctor-yes` or
  `owner`. `--detach` returns at once, and `icm wait <run>` collects the result of a long build.
- **Exit codes** are stable: 0 ok, 1 check failed, 2 usage, 3 config, 4 environment, 5 build,
  6 tool, 7 device, 8 timeout, **9 owner needed** (stop and hand `errors[0].fix` to a person),
  10 app died or never drew, 70 icm bug, 130 interrupted.
- **Explanations:** `icm explain <id>`, `icm explain exit-codes` and `icm explain --list`.
  `icm print commands --json` gives the whole command surface.
- **icm never uploads, publishes or notarizes.** Release signing keys and their passwords stay with
  the owner.
- **Working on this repository:** read [AGENTS.md](AGENTS.md) for the checks, the rules and the
  commit style.

## Repository layout

| Path | What it is |
|---|---|
| [`src/mobile.rs`](src/mobile.rs) | `iced::mobile` and `iced::android_main!`: the app-facing mobile API and its documentation |
| [`winit/`](winit/) | `iced_winit`, the shell: the Android and iOS life cycle, the `ICM_EVENT` protocol (`icm.rs`), iOS scenes (`scene.rs`) |
| [`vendor/winit/`](vendor/winit/) | winit 0.30.13 with Android fixes; [PATCHES.md](vendor/winit/PATCHES.md) lists them, `vendor/patches/` keeps them as files |
| `core/`, `widget/`, `runtime/`, `graphics/`, `renderer/`, `wgpu/`, `tiny_skia/`, `futures/`, ... | the upstream iced crates, with small mobile changes |
| [`test/`](test/) | `iced_test`, the headless harness that `icm test`, `icm shot --headless` and `icm ui` drive |
| [`cli/`](cli/) | `icm`, in its own Cargo workspace with its own lockfile |
| [`examples/app/`](examples/app/) | the template that `icm new` copies |
| [`examples/`](examples/) | upstream's examples |
| [`docs/agents/limitations.md`](docs/agents/limitations.md) | known limitations of the current release |
| [`docs/icm/DESIGN.md`](docs/icm/DESIGN.md) | icm's design and phased plan, written before implementation (where it and the code differ, the code wins) |
| [`docs/mobile/review-2026-10-06.md`](docs/mobile/review-2026-10-06.md) | a dated review of the mobile work (a snapshot; much of it is fixed since) |
| [`graphics/fonts/`](graphics/fonts/) | Fira Sans and its licence, iced's icon font |

## Versioning and branches

- Releases are tags `v0.14.x-mobile.N`. One tag releases the framework and `icm` together, and
  `icm new` pins the tag that `icm` was built from. Tags are never moved.
- `main` is the only development branch. (`tawara/0.14-mobile` is kept for one app's existing pin.)
- The base is the head of upstream's 0.14 branch (`38237dd29`). Its manifest says `iced` 0.14.1, but
  upstream never published an `iced` 0.14.1: crates.io has `iced` 0.14.0, plus later patch releases
  of `iced_widget` (0.14.2), `iced_winit` (0.14.1) and `iced_tiny_skia` (0.14.1). The framework
  crates keep the branch's version numbers (`iced` 0.14.1), so `[patch.crates-io]` still matches.
  Only the `icm` version carries the `-mobile.N` suffix.
- Upstream changes come in deliberately. The fork stays on 0.14 and takes changes from 0.15 only
  where they clearly improve things. If the fork moves to a 0.15 base, tags become
  `v0.15.x-mobile.N`.
- winit is vendored in `vendor/winit`, so an app needs no `[patch]` section and no second git
  source. Each patch there is meant for upstream winit (winit PR #4739).

## Roadmap

- **Releases** (design §11 and §18, phases 3 to 5; the App Store and iPhone runs are in): `icm
  release` for Google Play (`.aab`) with a lifecycle test suite, static web hosting, and
  desktop installers (`.app`/`.dmg`, `.msi`/`.exe`, `.deb`/AppImage). icm builds and checks the
  artifacts and prints the upload commands, but the owner runs them.
- **Framework:** safe-area insets, the clipboard and dark-mode detection on Android and iOS, and a
  lifecycle subscription that reaches `update` (today `on_lifecycle` takes a plain `fn`).
- **Agent bridge** (phase 6): tap, type and read the widget tree of a running app on every platform.
- **CI:** GitHub Actions does not run on this fork yet, and the workflows in `.github/workflows`
  come from upstream. The plan for the fork's own CI is in design §17.

## License and credits

- iced_mobile is under the MIT license ([LICENSE](LICENSE)), copyright Héctor Ramón and the iced
  contributors. iced is their work; this fork adds the mobile support and `icm`.
- Fira Sans (`graphics/fonts/FiraSans-Regular.ttf`) is under the SIL Open Font License 1.1
  ([graphics/fonts/OFL.txt](graphics/fonts/OFL.txt)). An app that embeds it (the `fira-sans` and
  `mobile-fira-sans` features) must ship that notice. Every app from `icm new` embeds it:
  `mobile-fira-sans` is a default feature and the template turns on `fira-sans`. icm does not add
  the notice to the `.app`, the APK or the web site yet, so ship `OFL.txt` with your app yourself,
  for example on its licences screen or as a file next to it.
- The vendored winit and its `dpi` crate are under the Apache License 2.0
  ([vendor/winit/LICENSE](vendor/winit/LICENSE)).
- The template's icon (`examples/app/assets/icon.png`) is a placeholder made for this repository.
  Replace it in your app.

## Contributing

Changes come as pull requests against `main` of this repository, never upstream. GitHub issues
and discussions are not enabled here yet, so a pull request is also the way to report a problem:
one that adds a failing test or a note to [docs/agents/limitations.md](docs/agents/limitations.md)
is enough. When icm exits 70 (a bug in icm), include the `result.json` and `events.ndjson` from the
run directory it names (`run_dir`). Read [AGENTS.md](AGENTS.md) first for the checks, the rules
and the commit style.

[CONTRIBUTING.md](CONTRIBUTING.md), [CHANGELOG.md](CHANGELOG.md) and [ROADMAP.md](ROADMAP.md) are
upstream's files for iced itself: the changelog stops at upstream's 0.14.0, and the roadmap is
upstream's. This README's [Platforms](#platforms) and [Roadmap](#roadmap) sections describe the
fork. `v0.14.1-mobile.1` has no separate release notes.

## Upstream iced

For iced itself (guides, API documentation and community), see the upstream project:
[iced.rs](https://iced.rs), [the book](https://book.iced.rs/),
[docs.rs/iced](https://docs.rs/iced/), [examples](https://github.com/iced-rs/iced/tree/0.14/examples),
[Discourse](https://discourse.iced.rs/) and [Discord](https://discord.gg/3xZJ65GAhd). Those
resources describe upstream iced and do not cover this fork's mobile support. You can support
iced's author through [GitHub Sponsors](https://github.com/sponsors/hecrj).

[iced]: https://github.com/iced-rs/iced
[The Elm Architecture]: https://guide.elm-lang.org/architecture/
