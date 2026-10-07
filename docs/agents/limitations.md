# Known limitations of iced_mobile

What an app built on this tag cannot do yet, as an app author meets it, and
what to do instead. Sources: the Known limitations in `src/mobile.rs`, and
the items of `docs/mobile/review-2026-10-06.md` that are still open. Update
this file in the commit that fixes or finds a limitation.

<!-- icm: AGENTS.md embeds everything below this line -->

- **The safe area comes from a running app only** (iOS, Android). The app
  draws under the status bar, the notch or Dynamic Island, the home
  indicator or navigation bar (Android draws edge to edge from targetSdk
  35) and the keyboard. `iced::mobile::safe_area()` reports what covers
  each edge and the keyboard's height; pad the root with
  `SafeArea::padding`. Headless runs (`.ice` flows, `icm shot --headless`,
  `icm ui --headless`, unit tests) report nothing, so keep a padding of your
  own until a value arrives. The template pads phones a fixed 64 top and 48
  bottom (`safe_area()` in `src/lib.rs`), which keeps its headless layouts
  the same as the phone's but follows neither the device nor the keyboard.
  On Android the keyboard's height can arrive a quarter of a second after
  the keyboard. The desktop and the web report zero, a phone browser's
  notch included. Keep controls away from the rounded corners.
- **No edit menu on phones.** A long press in a text field shows no menu
  and no selection handles, and the Ctrl shortcuts of text fields fire
  only from a hardware keyboard on Android (iOS hardware keyboards send no
  modifier keys). The clipboard itself works: `iced::clipboard::read` and
  `write` use the system's, so give fields that need it Copy and Paste
  buttons.
  Read only when the user asks to paste: Android 10 and later give `None`
  to an app without the input focus, and iOS 16 and later ask the user
  ("Allow Paste") before an app reads what another app copied.
- **Limited text input** (iOS, Android).
  - Android (NativeActivity) receives key events only: no composition,
    autocorrect, swipe typing or suggestions, and characters outside the key
    map (many accented letters, all CJK) can be lost. iOS has no marked
    text, so CJK input methods do not compose either. Do not promise
    non-ASCII input on Android, and type into the app on a device.
  - The keyboard covers fields in the lower half of the screen unless the
    root is padded with `SafeArea::padding` from `iced::mobile::safe_area()`,
    whose bottom rises with it. The template's fixed padding does not, so
    its field is at the top.
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
- **Dark mode reaches the app, not the system bars.** Phones report the
  system's mode at launch and on every switch: without `.theme(..)` the app
  follows it, and `iced::system::theme_changes()` reports it. On Android
  the bars' icons are set when the app starts (icm's generated theme has
  dark-mode values: white icons on `#2B2D31` for a light `[app]
  background`); after a switch while the app runs they keep that colour
  (dark on dark, or white on white) until the next launch. An app that
  forces a light theme in dark mode declares its own `IcmTheme` style in
  `platform/android/res` so the launch window and icons stay light. On
  iOS the status bar always follows the system's mode, also when the app
  forces the other one.
- **Two lifecycle channels, with different timing.**
  `iced::mobile::lifecycle()` delivers `Foreground`, `Active`, `Inactive`,
  `Background` and `MemoryWarning` to `update`, the same on iOS and
  Android, a moment after the event. Hide sensitive content on `Inactive`
  (Control Center, Face ID prompts, calls, the notification shade) and lock
  on `Background`, never on `Inactive`: an unlock that asks for Face ID
  makes the app inactive again, and loops. `on_lifecycle` runs one plain
  `fn` (setting the same one again does nothing; a different one is
  ignored with a warning) before iced acts, with `Lifecycle::Suspended`
  and `Resumed` only, whose meaning differs per platform: on iOS the app is
  about to become inactive, on Android its native window is going away, on
  the web the page goes into the back-forward cache, and the desktop never
  sends it. Save what must survive there, not on a `lifecycle()` message.
  On Android the app draws only while its activity runs, so hiding on
  `Inactive` does not reliably keep content out of Recents (set
  `FLAG_SECURE`); the focus can come and go more than once while a system
  window opens; Back can end the app before its `Background` message
  arrives; and `MemoryWarning` is rare (`onLowMemory` only).
- **When Android destroys the activity, the app starts over.** Back at the
  app's root, a configuration change missing from `configChanges` and the
  "Don't keep activities" developer option destroy the Android activity: the
  application is dropped (after `Lifecycle::Suspended`) and the next
  activity starts it again, from its boot function, often in the same
  process: `run()` runs again, so what it sets up for the process (a logger
  of your own) must accept a second call (`try_init()`, error ignored).
  Whatever it kept in memory is gone. icm's generated manifest lists every
  `configChanges` value (with `assetsPaths` from target_sdk 36: an
  emulator's theme overlays change during its first boots), so rotation,
  dark mode and font scale keep the state; never remove one. To free memory,
  Android instead kills the process of an app in the background, without
  warning; the next launch is a cold start. Save what must survive on
  `Suspended`, which covers both. Back is Android's by default (`[android]
  back = "system"`); an app that handles Back itself (going back a screen)
  sets `back = "key"`: Back then reaches it as
  `Key::Named(Named::BrowserBack)` and never closes it, so at the root it
  does nothing. Launching the app again a moment after Back can end the
  process; the next launch works.
- **One window on Android.** A second `window::open` is refused with an error
  in the log, and its task ends without an id. Navigate inside one window.
  On iOS, a window being opened may replace the last one: open the new
  window before closing the old.
- **Apps cannot quit themselves on phones.** `iced::exit()` and closing the
  last window are ignored on Android and iOS, with a warning in the log; the
  system ends mobile apps. Never call them there. On Android, finishing the
  activity through JNI (`Activity.finish`) leaves the app as Back does.
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
