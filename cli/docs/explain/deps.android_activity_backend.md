## How icm detects it

When Android is among the platforms it checks, `icm check` runs
`cargo tree -p <package> --target <android triple> -e normal,build -f '{p}|{f}'`
and reads the features of `android-activity`, the crate winit runs the app's
activity through. That is the feature set the Android library build gets:
features only another workspace member or a dev-dependency turns on do not
count.

Exactly one backend must be on, and it must be the one `[android] activity`
names, because the generated manifest starts that activity:

- `native-activity` (iced's default `android-native-activity`):
  `android.app.NativeActivity`, `[android] activity = "native"`;
- `game-activity` (iced's `android-game-activity`), `activity = "game"`
  (reserved until icm can package Java code).

It fails when:

- no backend is on: `default-features = false` on iced drops
  `android-native-activity`, and android-activity stops the build with
  ``file not found for module `activity_impl` ``;
- both are on: one crate turns on `android-game-activity` while iced's
  defaults (in any crate that depends on iced) keep `android-native-activity`;
- two android-activity crates each bring a backend;
- the backend is not the one the manifest starts, so the app would not start;
- there is no android-activity at all in the Android build.

A `cargo tree` that fails is a SKIP; `cargo check` runs next and reports why.

## Fix

Turn on exactly one backend: keep iced's default features, or list
`android-native-activity` with `default-features = false` (`iced_winit`
has the same feature). Do not depend on winit directly for it: iced brings
its own winit, and a second one fails `deps.single_winit`. To find which
crate turns on each backend:

```sh
cargo tree -p <package> --target aarch64-linux-android -e features -i android-activity
```
