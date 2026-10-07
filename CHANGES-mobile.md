# iced_mobile releases

What changed in each release of the fork, newest first. A release is one tag, `v0.14.x-mobile.N`,
for the framework and `icm` together. [CHANGELOG.md](CHANGELOG.md) is upstream's and stops at
iced 0.14.0.

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
  a newer icm.
- `icm stop --shutdown` leaves running a simulator that another project's run booted, as it
  already did for the emulator, and `stop --all --shutdown` without a session considers only the
  simulator the project's runs use.
- Every file a run directory keeps from an app's or a tool's output (log copies, consoles, crash
  reports) redacts secret values on every platform, as stdout does, also where the output escapes
  them as JSON or percent-encodes them. The live files in `target/icm/sessions/` stay the app's own
  output.
- Android: readiness and logs count only the app's own processes, the lifecycle suite tells a
  destroyed Activity from a live one after a rotation, and a rotation warns when the app is locked
  to one axis.
- Releases: `icm ledger mark-uploaded` refuses a release that cannot have been uploaded, literal
  signing secrets stay out of every finding and record, `upload.sh` runs `icm diagnose` when a
  store tool fails, `THIRD_PARTY_NOTICES` lists the Rust standard library, the AppImage leaves
  `libwayland-client` to the host, and `icm verify macos` accepts a stapled app or DMG.
