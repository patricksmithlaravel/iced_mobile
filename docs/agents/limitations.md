# Known limitations of iced_mobile

What an app built on this tag cannot do yet, as an app author meets it, and
what to do instead. Sources: the Known limitations in `src/mobile.rs`, and
the items of `docs/mobile/review-2026-10-06.md` that are still open. Update
this file in the commit that fixes or finds a limitation.

<!-- icm: AGENTS.md embeds everything below this line -->

- **The safe area arrives with the window** (iOS, Android). The app draws
  under the status bar, the notch or Dynamic Island, the home indicator or
  navigation bar (Android draws edge to edge from targetSdk 35) and the
  keyboard. `iced::mobile::safe_area()` reports what covers each edge and
  the keyboard's height; pad the root with `SafeArea::padding`. It arrives
  once the window exists. Headless, `.ice` flows, `icm shot --headless` and
  `icm ui --headless` give a viewport the size of a device preset
  (`iphone-17`, `iphone-se`, `pixel-9`) that device's insets; other sizes
  and unit tests get none. So keep a padding of your own until a value
  arrives, as the template's `App::padding` in `src/lib.rs` does (64 top and
  48 bottom on phones). On Android the keyboard's height can arrive a
  quarter of a second after the keyboard. The desktop and the web report
  zero, a phone browser's notch included. Keep controls away from the
  rounded corners.
- **No edit menu on phones.** A long press in a text field shows no menu and
  no selection handles, and the Ctrl shortcuts of text fields fire only from
  a hardware keyboard on Android (iOS hardware keyboards send no modifier
  keys); the Android keyboard's own Select all and Paste work, its Cut and
  Copy only on what it typed (see Text input). The clipboard itself works:
  `iced::clipboard::read` and `write` use the system's, so give fields that
  need it Copy and Paste buttons. Read only when the user asks to paste:
  Android 10 and later give `None` to an app without the input focus, and
  iOS 16 and later ask the user ("Allow Paste") before an app reads what
  another app copied.
- **Text input differs from the desktop** (iOS, Android).
  - On Android (NativeActivity, the default, Android 8 and later) the
    keyboard composes, suggests, autocorrects and swipe-types, and accented
    letters and CJK arrive whole. Whether it composes is the keyboard's
    choice: Gboard can commit each letter as it is typed (it does in
    Latin-script languages on the Android 16 emulator), so `on_input` sees
    every letter and nothing is underlined, while CJK input methods and
    some other keyboards compose. A word being composed shows underlined at
    the caret and is not part of the value until the keyboard commits it (a
    space, a suggestion, Enter, or a tap anywhere): `on_input` sees the text
    without it until then. Do not count on `on_input` seeing whole words.
  - When the input connection cannot be installed (before Android 8, or
    after a failure, with one warning in the log), the keyboard reaches the
    app through key events only: no composition, suggestions or swipe
    typing, and text the keyboard does not send as keys (many accented
    letters, all CJK) can be lost. Password fields are not marked secure
    there, and a keyboard the user hid comes back only after the field has
    lost the focus once.
  - With GameActivity (`android-game-activity`) the keyboard types into
    GameActivity's own input connection, whose text reaches winit as text
    events that the vendored winit drops: what the keyboard commits there,
    plain letters included, does not reach the field, and only the keys it
    sends as key events do. Password fields are not marked secure, and a
    keyboard the user hid comes back only after the field has lost the
    focus once. GameActivity has not been run with this; use NativeActivity
    (the default) for text input.
  - On iOS the app's view takes key input only (`UIKeyInput`): the keyboard
    shows no predictions and capitalises nothing by itself in any field,
    and CJK input methods do not compose (there is no marked text).
  - The Android keyboard sees only the text it typed since it last started
    over, never the field's earlier text, so its suggestions do not see
    what was there before. It starts over when the field gets the focus;
    at a tap, a hardware key other than a modifier, or the window losing
    the focus; after its own Enter, Tab, Escape, arrow, Home or End keys
    (moving the cursor with the space bar sends arrows) and its Select all;
    after a caret move from code or a change between a password field and
    another field; once its text passes 4096 UTF-16 units; when what it typed
    starts with a character that joins the one before it (a combining mark,
    vowel sign or virama typed first, or a consonant right after a virama
    it typed before it started over); and, in a password field, after every
    edit. The field asks it for no automatic capitals.
  - The Android keyboard keeps its own copy of the text it typed and takes
    it that the field has all of it. When `on_input` refuses that text or
    changes its length (a length limit, digits only, formatting that adds
    spaces), or the app changes the field's value itself while the
    keyboard has typed into it (from a subscription or a task's result), a
    later edit that reaches back into that text, such as a Backspace into
    the previous word or an autocorrection the user undoes, can delete or
    replace characters the field shows. Typing does not bring the keyboard
    back in step; starting over (above) does. With such a field, return a
    caret move from code with the refusal or the change
    (`operation::move_cursor_to_end`): it starts the keyboard over from
    what the field shows.
  - When the app moves the focus itself (`operation::focus`, `focus_next`,
    `focus_previous`, `focusable::unfocus` run with
    `iced::advanced::widget::operate`, or the field leaving the view) while
    the Android keyboard composes a word, the word is dropped, and so is a
    word the keyboard had taken back from the field after a Backspace into
    it.
    Moving the caret of that field from code (`move_cursor_to`,
    `select_all`) commits the word where it was typed first, then moves the
    caret, so a position counts the word; the caret of another field moves
    and leaves the word alone. What the keyboard typed before such an
    operation, and what it sends while the operation runs, reaches the field
    before the caret moves. Let the user's tap or Enter move the focus where
    it can.
  - `text_input` deletes by character cluster, as on the desktop: a
    combining mark, vowel sign or virama typed right after the place the
    user tapped joins the character before it, and Backspace then deletes
    both, or the whole conjunct once a consonant follows a virama
    (`text_input` may also put its caret one character further when that
    happens in the middle of the text). `text_editor`'s Backspace can delete
    by character instead, so a combining sequence the Android keyboard
    revises can come out wrong there.
  - In Indic scripts a consonant and virama make one conjunct with the
    consonant after them (स् before त gives स्त), also when the Android
    keyboard typed one side of the virama and the field held the other,
    which the keyboard does not know. In the middle of the text, a
    consonant and virama it types right before a consonant of the field
    join it: a Backspace or another consonant from the keyboard right after
    that virama then replaces the whole conjunct, the field's consonant
    with it, and the caret ends up after the conjunct. Where the keyboard
    starts over right after a virama the field holds (a tap there, the
    focus, its Enter or arrows), a consonant it types joins that virama:
    its next change to that consonant (a vowel sign from a keyboard that
    commits each letter, or its Backspace taking the word back into a
    composition) replaces the whole conjunct, the field's consonant and
    virama with it. Typing on after a virama the keyboard typed itself is
    not affected, also when it started over by itself in between (at the
    points above that depend on what it typed).
  - On Android, an on-screen keyboard's keys reach a field only as text,
    Backspace, Delete, Enter, Tab, Escape, the arrows, Home and End. Its
    Ctrl and Meta shortcuts (Ctrl+C, X, V and A on keyboards that have a
    Ctrl key) and its other keys (Page Up and Down, function keys, Insert)
    are dropped, and its keys come without modifiers, its own Shift or one
    held on a hardware keyboard: its arrows, Home and End move the caret
    without selecting, and a Ctrl held on a hardware keyboard does not make
    its deletions delete words. A hardware keyboard's keys reach the app as
    before.
  - On Android, the keyboard's editing panel (Gboard's text editing tools)
    works in part: Select all selects the field's text (the field's own
    Ctrl+A), and Paste inserts the clipboard's text at the caret without
    its line breaks and tabs, as a paste from the keyboard's clipboard
    does. Cut and Copy take only what the keyboard selected in the text it
    typed since it last started over, never the field's other text or a
    password; Gboard offers them only then.
  - On Android, a keyboard the user hid while a field keeps the focus
    comes back at a tap on a field, when the app focuses a field from code
    (`operation::focus`, also on the field that has the focus, such as a
    `text_editor` an Edit button focuses again), and also whenever the
    window gets the focus back (after the notification shade, a system
    dialog or another app): nothing tells the app that the user hid it.
  - Password fields (`.secure(true)`) are marked secure to the Android
    keyboard with the input connection above: it shows its password layout
    without suggestions and is asked not to learn what is typed, and what
    it composes there reaches the field as typed text, so it is never drawn
    in clear. On iOS the view tells UIKit that the field is secure text
    entry: on the iOS 27.0 Simulator the keyboard then has no emoji or
    dictation key, and it switches to and from that keyboard as the focus
    moves between a password field and another (a physical iPhone has not
    been tried). When the focus moves straight from a password field that
    was typed into to another field, that field's keyboard can label its
    numbers key ".?123" instead of "123" until its numbers are shown once
    or it is hidden and shown again; it types as usual. The iOS keyboard
    shows no suggestions in any field anyway (above). On both, a
    `text_editor` focused from code that also takes the keyboard (see
    `text_editor` below) leaves the keyboard set up for the password field.
  - The keyboard covers fields in the lower half of the screen unless the
    root is padded with `SafeArea::padding` from `iced::mobile::safe_area()`,
    whose bottom rises with it, as the template's is. On Android it rises
    up to a quarter of a second late, so the template keeps its field near
    the top.
  - iOS hardware keyboards deliver typed characters, Return, Tab and
    Backspace only: no arrows, Escape or Cmd shortcuts.
  - On Android, the keyboard's Enter (its key, its action button, or a
    commit of a lone line break) reaches the app as the Enter key, after the
    text typed before it, and submits a `text_input`; its Tab (its key, or a
    commit of a lone tab) is the Tab key. The app handles Enter, Tab and
    Escape, builds its view again and runs the widget operations its
    `update` returns for them before the text the keyboard typed after them
    reaches a field, also when a tap or an operation comes in between: a
    field cleared on submit gets that text alone, and the field `on_submit`
    moves the focus to (`focus_next`) gets it. A key the app handles later,
    from a subscription (`keyboard::listen`, which is how an app takes Tab:
    `text_input` ignores it) or a task, comes after that text: what the
    keyboard sends in the same instant, such as a password a credential
    keyboard types right after a username and Tab, goes to the field that
    had the focus. A line break or tab inside longer committed text (a
    suggestion, a paste from the keyboard's clipboard) is dropped.
  - On iOS, Return and Tab count only when UIKit inserts them alone, as
    the Return and Tab keys do (`"\r\n"` is one Return). A line break or
    tab inside longer inserted text (dictation, a keyboard suggestion, a
    third-party keyboard) is dropped: it neither submits a `text_input` nor
    breaks the line in a `text_editor`. Give a form that matters a submit
    button.
- **A drag that starts on a button does not scroll** (any touch screen,
  iced-rs/iced#2004). A `scrollable` only scrolls when the finger lands on
  something that does not take touches: text, space, the gaps between
  widgets. Buttons, `text_input`, checkbox, toggler, radio, slider,
  `mouse_area` and `pick_list` take them. There is no momentum. Let text
  fill most of each list row and keep its buttons small, as the template's
  list does, or leave gaps between rows.
- **`text_editor`, rich-text links and `pick_list` menus ignore touch.** A
  tap cannot focus a `text_editor` (focusing it from code works), nor take
  the focus away from one: a `text_editor` focused from code stays focused
  when the user taps a `text_input`, and both then take what is typed, so
  a password typed into a secure field shows in clear in the editor. The
  keyboard is still set up for the password field, so it neither suggests
  nor learns the password. Move the focus away from the editor from code
  (`operation::focus` on another widget unfocuses the rest), for example
  from a Done button. Links in
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
  `iced::mobile::lifecycle()` delivers an `iced::mobile::LifecycleEvent`
  (`Foreground`, `Active`, `Inactive`, `Background` and `MemoryWarning`;
  match it with a wildcard arm, as more may come) to `update`, the same on
  iOS and Android, a moment after the event. Hide sensitive content on `Inactive`
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
  arrives; and `MemoryWarning` is rare with NativeActivity (the default:
  `onLowMemory` only), but comes on every trip to the background with
  GameActivity, which turns each `onTrimMemory` into one without its
  level. Free only what is cheap to rebuild there.
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
- **`icm run desktop` redacts only the secrets it knows.** A desktop app's output goes through a log
  collector that redacts the values of the secret-named variables the app started with, and of its
  `--env` pairs, as it writes the live files. That holds when the app replaces itself with the same
  program and another value of an inherited variable: the original value was redacted when the app
  logged it, and so is a rotated value the app inherited too. A secret the app gets some other way
  (a file, a request, a value it makes itself) and logs is not known to icm, and is written as the
  app wrote it, as on every platform. Hand such a secret to `icm run desktop --env NAME=…`. The
  collector also misses a secret that the app writes with JSON `\u` escapes inside a JSON string
  that is still open when the collector writes part of its line (after its idle wait, or past 64
  KiB), if more text follows the secret in that line than the collector holds back.
- **`icm` finds a base64 encoding of a secret only within one run of base64 characters.** A line
  break, a space or any other character outside base64 ends a run, and so does a percent escape
  other than `%2B`, `%2F` or `%3D`. An encoding that a line wrap splits (MIME or PEM output, 64 or
  76 characters a line) is written as the app wrote it.
- **A desktop session from before the log collector cannot redact every
  inherited secret.** Its live `app.stdout` and `app.stderr` hold the app's
  output raw. While the app runs, `icm logs desktop` learns the secret-named
  variables the app inherited from `icm run desktop`'s shell by reading the
  running app's environment, and a variable counts as known by its name. An
  app that replaces itself with the same program and another value of that
  variable makes `logs`, from a shell without the variable, print the original
  value from those files, and after `icm stop` they keep it. Once such an app
  has ended, `logs` warns `desktop.logs.secret_unknown` and prints neither
  value. Hand a secret the app logs to `icm run desktop --env NAME=…`, which
  every later command redacts from any shell.
