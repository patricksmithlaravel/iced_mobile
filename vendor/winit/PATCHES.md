# winit, vendored

This directory is winit 0.30.13 (upstream tag `v0.30.13`, rust-windowing/winit)
with the patches below applied. iced_mobile builds against it through the
workspace dependency in the root `Cargo.toml`, so every app that depends on
iced_mobile gets exactly this winit and needs no `[patch]` section.

Rules:

- Only fixes that belong upstream go here, each one tied to an upstream pull
  request. Anything iced-specific belongs in `winit/` (the `iced_winit` crate).
- Every patch is also kept as a file in `vendor/patches/`, so the tree can be
  rebuilt from a clean upstream tag.
- Apps must not depend on crates.io `winit` themselves: two copies of winit in
  one binary clash (duplicate Objective-C classes on iOS, and on Android a
  second winit whose types iced never sees; only `AndroidApp` is shared). Use
  the re-export `iced_winit::winit` instead.

## Patches

| File | Upstream | What it fixes |
|---|---|---|
| `vendor/patches/winit-0001-android-exit-on-destroy.patch` | winit PR #4739, plus reporting `ContentRectChanged`, skipping input after Destroy, and docs | The event loop now ends on `MainEvent::Destroy` (Android waits in `onDestroy` for `android_main` to return, so ignoring it froze the app). Dropping the `EventLoop` lets the next Activity build a new one in the same process. Content-rect and inset changes are reported as `Resized`. |

## Updating

1. Extract the new upstream tag into a scratch directory.
2. Apply each file in `vendor/patches/` with `git apply` (drop patches that
   upstream has merged).
3. Replace this directory with the result, keeping this file, and drop the
   upstream `.github/` and `Cargo.lock`.
4. Update the version in the root `Cargo.toml` and run `cargo update -p winit`.
