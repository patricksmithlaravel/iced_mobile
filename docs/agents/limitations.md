# Known limitations of iced_mobile

What an app built on this tag cannot do yet, as an app author meets it, and
what to do instead. Sources: the Known limitations in `src/mobile.rs`, and
the items of `docs/mobile/review-2026-10-06.md` that are still open. Update
this file in the commit that fixes or finds a limitation.

<!-- icm: AGENTS.md embeds everything below this line -->

- **No safe-area insets** (iOS, Android). The app draws under the status
  bar, the notch or Dynamic Island and the home indicator on iOS, and under
  the status and navigation bars on Android, which draws edge to edge from
  targetSdk 35. Pad the root view (the template's `safe_area()` in
  `src/lib.rs` pads 64 top and 48 bottom on phones), and keep controls away
  from the corners.
- **No clipboard on phones.** Reads return nothing and writes only log a
  warning. There is no long-press edit menu, and the Cmd/Ctrl shortcuts of
  text fields never fire. Do not build a feature on copy and paste there.
- **Limited text input** (iOS, Android).
  - Android (NativeActivity) receives key events only: no composition,
    autocorrect, swipe typing or suggestions, and characters outside the key
    map (many accented letters, all CJK) can be lost. iOS has no marked
    text, so CJK input methods do not compose either. Do not promise
    non-ASCII input on Android, and type into the app on a device.
  - iced does not know how tall the keyboard is, so it covers fields in the
    lower half of the screen. Put text fields near the top.
  - If the user hides the keyboard while a field keeps the focus, tapping
    the field does not bring it back until the field has lost the focus
    once.
  - Password fields (`.secure(true)`) are not marked secure to the system
    keyboard, which may suggest or learn what is typed.
  - iOS hardware keyboards deliver typed characters and Backspace only: no
    arrows, Escape or Cmd shortcuts.
- **A drag that starts on a button does not scroll** (any touch screen,
  iced-rs/iced#2004). A `scrollable` only scrolls when the finger lands on
  something that does not take touches: text, space, the gaps between
  widgets. Buttons, `text_input`, checkbox, toggler, radio, slider,
  `mouse_area` and `pick_list` take them. There is no momentum. Let text
  fill most of each list row and keep its buttons small, as the template's
  list does, or leave gaps between rows.
- **`text_editor`, rich-text links and `pick_list` menus ignore touch.** A
  tap cannot focus a `text_editor` (focusing it from code works), links in
  `rich_text` and `markdown` cannot be tapped, and `pick_list` and
  `combo_box` menus select on touch-down, so a long menu cannot be
  scrolled. Use `text_input`, buttons for links, and short menus.
- **Hover sticks after a tap.** A touch moves iced's cursor and nothing moves
  it away, so the last widget tapped keeps its hovered style and tooltips
  stay open. Do not put anything essential in a tooltip.
- **Dark mode is not detected on phones.** The system theme reads as unknown
  and changes never arrive, so the app draws in iced's default theme. Set a
  theme explicitly (`.theme(..)`), with an in-app switch if the app needs
  dark mode.
- **`Lifecycle::Suspended` means something different per platform.** On iOS
  the app is about to become inactive, which also happens for Control
  Center, Notification Center, Face ID prompts and calls; on Android its
  native window is going away; on the web the page goes into the
  back-forward cache; the desktop never sends it. `on_lifecycle` takes one
  plain `fn` (a second call is ignored with a warning) and cannot reach
  `update` by itself. Hide sensitive content on `Suspended`, but do not
  lock, log out or stop work on it alone: an unlock that asks for Face ID
  suspends the app again, and loops.
- **Destroying the Android activity freezes the app** (fixing it needs a
  winit patch, rust-windowing/winit#4739). Whatever destroys the activity
  while the process lives on (predictive Back, a configuration change
  missing from `configChanges`, the "Don't keep activities" developer
  option) freezes the app, and the next launch hangs until the process is
  killed. icm's generated manifest lists every `configChanges` value
  (with `assetsPaths` from target_sdk 36: an emulator's theme overlays
  change during its first boots) and,
  with `[android] back = "key"`, turns predictive Back off: Back then
  reaches the app as `Key::Named(Named::BrowserBack)` and never closes it.
  Handle that key for in-app navigation, never remove a `configChanges`
  value, and do not test with "Don't keep activities".
- **One window on Android.** A second `window::open` is refused with an error
  in the log, and its task ends without an id. Navigate inside one window.
  On iOS, a window being opened may replace the last one: open the new
  window before closing the old.
- **Apps cannot quit themselves on phones.** `iced::exit()` and closing the
  last window are ignored on Android and iOS, with a warning in the log; the
  system ends mobile apps. Never call them there.
- **iOS ignores most window settings** (size, position, title, decorations,
  level, icon): the window fills the screen. `window::Event::Opened` reports
  the safe-area size; the following `Resized` has the real one.
- **iPad is not supported.** `[ios] devices` accepts only `iphone`.
- **Fonts beyond Latin.** The embedded Fira Sans covers Latin, Greek and
  Cyrillic. Other scripts fall back to the phone's own fonts
  (`mobile-system-fonts`), with gaps: emoji draw blank on both platforms
  (Android's flags draw), and on iOS many simplified Chinese characters show
  the missing-glyph box. Embed a font for any script the app depends on
  (`.font(include_bytes!(..))` on the application) and check it on a
  device.
- **The Android emulator without a GPU draws on the CPU.** Its default
  headless GPU (lavapipe) cannot run iced's shaders, so iced falls back to
  tiny-skia. Screenshots look the same; judge performance on a device.
- **Host tests use a mouse.** `.ice` `click` and `Simulator::click` send
  mouse events, and the headless harness has no platform shell, so
  touch-only behaviour (the drag rule above, how a phone keyboard's Return
  reaches the app) does not show on the host. Simulate `Event::Touch` in unit tests, as the
  template's tests in `src/lib.rs` do, and look at UI changes with
  `icm run ios-sim` and `icm run android`.
- **iced's `sysinfo` feature breaks iOS builds** once libc is 0.2.190 or
  newer (sysinfo 0.33 calls macOS-only functions). Leave it off for iOS.
