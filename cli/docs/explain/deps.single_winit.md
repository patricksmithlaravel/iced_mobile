## How icm detects it

icm reads `Cargo.lock` and lists every `winit` package. The framework vendors
its winit (winit 0.30.13 with Android fixes, in `vendor/winit` of the
iced_mobile repository), so in an app's lock the winit iced uses comes from
iced's own source: the same git URL and revision as `iced`, like its `dpi`
crate. In the framework's own workspace, or with iced as a path dependency,
both are paths. Older framework revisions took winit from crates.io.

One winit passes. Two or more fail: the build holds a second copy of winit
that iced does not use. The copies share no types (but `AndroidApp`, which
comes from android-activity), and on iOS both declare winit's Objective-C
classes under the same names. The detail names each copy's source ("from
iced's own source" for the one to keep), and the evidence lists their
lockfile lines, iced's first.

Common causes:

- the app depends on winit itself, to name `AndroidApp` or to turn on
  `android-native-activity`;
- a third-party crate depends on crates.io winit.

## Fix

Drop the app's own winit dependency. `iced::mobile::AndroidApp` is the
Android activity handle, the rest of winit is `iced_winit::winit` (with
`iced_winit` on exactly iced's git URL and tag or rev), and the activity
feature comes from iced's default features or its `android-native-activity`
feature.

For a third-party crate that needs winit, give it iced's copy in the app's
`Cargo.toml`, with iced's URL and tag or rev character for character:

```toml
[patch.crates-io]
winit = { git = "https://github.com/patricksmithlaravel/iced_mobile", tag = "<iced's tag>" }
```

`cargo tree -d --target all` shows which crate pulls each copy.
