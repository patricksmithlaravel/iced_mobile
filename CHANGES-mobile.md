# iced_mobile releases

What changed in each release of the fork, newest first. A release is one tag, `v0.14.x-mobile.N`,
for the framework and `icm` together. Unreleased lists what `main` has changed since the newest
release; the next release's section takes it over. [CHANGELOG.md](CHANGELOG.md) is upstream's and
stops at iced 0.14.0.

## Unreleased

The changes since `v0.14.1-mobile.2`, among them the fixes from its re-verification.

- `icm run desktop` no longer writes a value the app inherited from icm's environment to the app's
  live files. The app's stdout and stderr go through a log collector (`icm __session desktop`, a
  process of its own, started after the app), which writes `app.stdout` and `app.stderr` with the
  secret values icm knows redacted as it reads them. Before, the live files held the app's raw
  output while it ran, so a `logs` from a shell that could not learn the value printed it, and an
  app that had ended by itself kept it in them. `logs` of such a session warns nothing about
  inherited secrets, and reads its live files as they are. A session from before this change keeps
  the old guard, and `docs/agents/limitations.md` says what the collector does not redact. `icm
  stop` ends the collector with its app, after it has written the app's last output.
- `icm logs` (with or without `--raw`), `icm shot` and `icm stop` from a shell without an
  inherited secret no longer print or keep its value when the app logged it, then replaced its
  process with the same program and another value of the variable. The collector wrote the original
  value redacted, and a rotated value the app inherited is redacted too. Before, `logs` learned the
  new value from the running app and printed the original one from the raw live files. A session
  from before the collector keeps that gap while its app runs; once the app has ended, `logs` warns
  and prints neither value.
- `icm` redacts the base64 of a secret as it redacts the secret. Before, the base64 of an inherited
  or handed secret reached the desktop live files, `app.log`, `logs.ndjson`, `result.json`,
  `events.ndjson` and `logs` output unredacted: a whole value, a `Basic` header's (the secret after
  `user:`), and a percent-encoded or JSON-escaped one. A base64 encoding that a line break splits is
  still written as the app wrote it (`docs/agents/limitations.md`). The release's secret scan
  (`release.secret_in_artifacts`) finds the base64 too, so a value that a build bakes in only as
  base64 makes the release unuploadable, where the scan passed it before.
- `icm ps` no longer calls an ios-device session stale once its console process has ended. A kept
  record (its terminate command failed) outlives that process while the app may still run on the
  phone, and `ps` said `stale (its processes are gone)` and counted nothing running, so an agent
  following it could skip the retry. It now says the app's state is unknown and names
  `icm stop ios-device`, which asks the device, and its result names that command as the next step.
- **iOS:** the vendored winit reports an insertion of only a line break as Return and one of only
  `\t` as Tab, deciding per insertion, so a Return typed right after another key in the same turn
  of the event loop is no longer lost.
- The known limitations in an app's `AGENTS.md` say that iOS hardware keyboards deliver Return and
  Tab as keys, beside typed characters and Backspace, where they said typed characters and
  Backspace only.
- **Android:** the safe area is published again when the app changes its own scale factor.
- **Android:** the on-screen keyboard shows on Android 16 (API 36). iced_winit requires
  android-activity 0.6.1, and the fork's `Cargo.lock` holds it: 0.6.0 asked Android to show the
  keyboard for a view that is not the served one there, so tapping a `text_input` showed none. An
  app whose `Cargo.lock` still has 0.6.0 gets 0.6.1 when it moves to the next release's tag, or now
  with `cargo update -p android-activity`.
- `icm logs desktop` of an app that ended by itself never takes a variable of the same name in its
  own shell for a secret the app inherited: it reads the run's redacted copies and warns
  `desktop.logs.secret_unknown` whatever the shell holds. Before, a shell where that variable held
  another value printed the old one. Read the logs while the app runs, or hand the secret with
  `--env`.
- When icm cannot read the identity of a process it starts (the OS would not describe it, or it
  had already exited), the record now says so, with the reason, where it held nothing and so looked
  like a record an older icm wrote, which icm still judges by the pid: the desktop app by its
  executable, ios-device's console by when the file was written, the iOS Simulator's collector by
  its command line, the web host by a marker, so a process that passed that test was signalled. Such
  a pid is now never taken for the process icm started and never signalled. `icm run` warns
  `run.identity_unavailable` for the desktop app, the iOS Simulator's app and collector,
  ios-device's console and the Android emulator; `icm stop` and `ps` say so when a
  process runs under such a pid (so do the desktop's `logs` and `shot`), and `stop` leaves it
  running where it would have said "already gone", and its summary names the process it left
  running (`desktop pid 4242 left running (cannot be verified)`) where it said that nothing was
  running; an ios-device app that `stop` terminated counts as stopped whatever the console's
  standing, and the next ios-device `run` warns about a previous console it cannot verify. The web
  session host does not start without its own identity (`web.host_identity`, exit 4, where it was
  `internal.bug`). `AGENTS.md` and
  `docs/icm/DESIGN.md` now say exactly what the identity rule guarantees: it is the start time, not
  the program or the environment.
- `icm stop ios-device` no longer counts a record as a stopped session when the app was not running
  on the phone and its console process had already gone: nothing was ended, the record is only
  removed, and the summary says nothing was running (it said `stopped 1 session(s)`). The console
  icm ends still counts, as before (`tests/ios_device.rs`:
  `a_record_that_ended_nothing_is_not_counted_as_stopped`).
- `icm run ios-device` of an app that reports no `start` event (`ICM_EVENT`), so the process
  probe found it, now records the pid the device lists for the installed app, and `icm stop
  ios-device` terminates that pid after the same device check as for an app that reported
  `start`. Before, such a record had no pid and no terminate command, so `stop` removed it
  without asking the device and sent no terminate command. A run that finds no single listed
  process for the installed app still records no pid (`tests/ios_device.rs`:
  `a_probe_ready_app_is_terminated_by_the_pid_the_device_lists`,
  `a_probe_ready_copy_in_another_container_is_not_recorded_as_the_app`). Not seen on a phone.
- `icm stop ios-device` no longer reads the recorded pid as unlisted when a process in the device's
  listing gives its `processIdentifier` as something other than an integer (a string or a float).
  That entry was dropped, so `stop` said the app was not running, terminated nothing and removed the
  record while the app ran. The listing now fails as unconfirmed, as an unreadable one does, when
  that process may be the app (no executable, an unreadable one, or the app's own): the record stays,
  the WARN names the `processIdentifier`, and the next stop asks again
  (`tests/ios_device.rs`: `stop_keeps_the_record_when_a_listed_pid_is_not_an_integer`;
  `devicectl::running_processes`). A process whose executable is another program is left out, as
  before, and `stop` asks the device about the recorded pid
  (`stop_terminates_the_app_past_an_unrelated_process_without_an_integer_pid`). Not seen on a phone:
  devicectl writes integer pids. The same listing gives a run the pid of an app that reported no
  `start` event (`listed_app_pid`). A listing that holds such a process which may be the app records
  no pid, as an unreadable listing does, and `stop` then removes that record without asking the
  device; that case is not fixed here. Another program's entry does not stop the run from recording
  the app's pid (`a_run_records_the_apps_pid_past_an_unrelated_process_without_an_integer_pid`).
- `icm logs desktop` of a running app whose environment could not be read back in full no longer
  prints the secrets it inherited. A read of the process that returned its arguments and no
  variables (macOS does for a platform binary), or another environment than the app's (the app
  replaced its process with `exec`, keeping its pid), made every inherited secret known, so `logs`
  from a shell with another value of the variable printed the app's token in its output,
  `result.json` and `events.ndjson` with no warning. Now an inherited secret counts as known only
  when the read returned a variable of its name, and a process that runs another program than the
  one `icm run` launched is not read at all; the others warn `desktop.logs.secret_unknown` and
  `logs` reads the run's redacted copies. What icm cannot know, in a session from before the log
  collector: an app that replaces itself with the same program and another value of a same-named
  variable still counts as known by name while it runs, so `logs` prints the original value, and
  after `stop` the app's raw live `app.stdout` and `app.stderr` keep a token icm never learned. Hand
  a secret that matters to `icm run desktop --env NAME=…`.
- Desktop session records keep only the protocol fields of the app's `ready` event, redacted, so a
  secret an app put in a field of its own no longer stays in them.
- `icm stop --shutdown` leaves a managed simulator or emulator running when it cannot read which
  project booted it, and a run that cannot mark the device as its own says so
  (`ios.sim.owner_unknown`, `android.emulator.owner_unknown`).
- Under `--json`, `--help` and usage errors name the command, not the value of a global option
  written before it (`icm --config x.toml run --help`), nor the word after a misspelled option
  (`icm --conf x.toml run`).
- `icm print plan <command> --help` answers with that command's help and exit 0, where it was a
  `usage.bad_args` error (exit 2) whose detail was the help's first line.
- When `icm run android --wait-ready` runs out, the error says only what the readiness polls saw,
  where it used to say the app was alive. After Android relaunched the activity, it says so, where
  it used to say the wait ended before the resumed-activity probe starts.
- `icm wait` reports a detached run whose icm died without a result as `run.detached_lost`, where it
  waited out its whole timeout when another process had taken the pid. `icm run` and `icm --detach`
  record the icm's start time beside its pid, and `prune` keeps a run directory only while that
  process runs; a run an older icm started is still judged by its pid.
- `icm stop desktop` signals the pid in a desktop session only while it is the app `icm run`
  started: its start time is recorded beside the pid, where only the executable the pid ran was
  checked, so another instance of the same program that took the pid, started by hand say, was
  stopped. The same test guards the read of the running app's environment. A session from before
  keeps the executable test, and counts as not running when `ps` cannot say.
- `icm stop web`, `icm ps` and the web commands take the pid in a web session's record for the
  session host only while that process is the one that wrote the record: the host records its start
  time, and the marker (`__session web` in the command line) that matched every session host remains
  only for records from before. Before, `icm stop web` killed any process under the pid whose
  command line held the marker, another session host included, and one under a record with no
  marker. `icm stop ios-device`, `icm ps` and `icm stop --all` check the pids of the other records
  the same way: the identity `run` recorded for the console process, else the old test of the file's
  write time.
- `icm stop ios-sim`, `icm run ios-sim --attach` and the next `icm run` on another simulator take
  the pid in an iOS Simulator session for the app only while that process is the one `icm run`
  launched (and `logs` and `input` report it running on the same test): its start time is recorded
  beside the pid, as the log collector's now is. Before, any process under the number counted, so
  `icm stop ios-sim` ran `simctl terminate` for an app that had exited and failed, and a run on
  another simulator ended the app there. A session an older icm wrote has no start time and says the
  app is not running; end that app with `xcrun simctl terminate <udid> <bundle id>`.
- `icm stop android` takes the emulator on a session's serial for the one its run booted only while
  that emulator's process is verified. `icm run` records the process's start time beside its pid,
  and a pid that has exited, that another process has taken, or that an older session recorded
  without a start time proves nothing. Then icm reads the AVD and the owner of whatever runs on that
  serial now, so another project's emulator on the same port stays up, and the app is not
  force-stopped there. It also reads the owner before it force-stops or shuts down an emulator it
  has a live record of, and signals the emulator's process only while that process is the recorded
  one. Before, a stale record whose pid another live process had taken made `stop --shutdown` send
  `am force-stop` and `emu kill` to another project's emulator without reading its owner, and then
  SIGTERM that process.
- `icm stop android` decides about an emulator whose record says icm booted it, but whose recorded
  process has ended or was replaced by another process, as it does for a record from before start
  times: by the emulator's `debug.icm.booted_by`. When it names this project, the app is
  force-stopped and `--shutdown` shuts the emulator down with `adb emu kill`, and the recorded pid
  is never signalled. When it names another project, both are left running
  (`android.emulator.shared`); when it cannot be read (`android.emulator.owner_unknown`), or is
  unset on an AVD icm does not manage, both are left running too (`run.no_session` says "the
  recorded emulator process has ended or was replaced; ..." and what else icm knows). Before, such a
  session on an `icm-test-*` emulator left the app and the emulator running without reading the
  property and said icm did not boot it, where the record says it did, and `--shutdown` did not look
  at a per-serial record in that state at all.
- `icm stop --shutdown` no longer reports an emulator that ignored `adb emu kill` as stopped when
  icm has no verified process of it to signal (a session from before start times, no record). It
  keeps the emulator's record, warns `android.emulator.shutdown_failed` with the command that tries
  again, says in the summary that it did not shut down, and lists it as `still_running` in the
  result of `icm stop android` and `icm stop`. Before, it waited 30 s, reported the emulator under
  `stopped`, deleted its record and said nothing, with the emulator still listed by `adb devices`.
  The wait is shorter when `--timeout` leaves less.
- `icm stop --shutdown` no longer takes a failed question for an emulator that is gone. When `adb
  emu kill` failed and `adb devices` failed too (the adb server went away), and icm had no verified
  process of the emulator to read instead, it reported the emulator as stopped, deleted its record
  and said nothing. Now the emulator is listed as `unverified` in the result of `icm stop android`
  and `icm stop` (not under `stopped`), its record stays, `android.emulator.shutdown_failed` warns
  that it may still be running, names what failed and says what to run, and the next `stop
  --shutdown` shuts it down once adb answers. An emulator that is still listed after a failed `emu
  kill` is reported as still running with the failure, not as having ignored the kill. A recorded
  emulator process that the OS would not describe settles nothing either: adb is asked, as it is
  when there is no process.
- `icm stop ios-device` (and `icm stop --all`) terminates the app on the phone only after the device
  lists the recorded pid as the app. The record's `devicectl device process terminate --pid` names
  a pid of the phone, which the phone reuses like any other, so `stop` asks `devicectl device info
  processes` first and runs the command only when that pid runs the app's executable. When the
  device lists another process under the pid, or none, nothing is terminated and the record is
  removed. When the device cannot be asked (not connected or locked, devicectl failing), nothing is
  terminated, `ios.device.stop_unconfirmed` warns, the device is listed as `unverified` in the
  result (the summary says the app may still be running), and the record stays so that the next
  `stop` asks again. icm's own console process is ended too while its identity checks out (one whose
  identity icm could not read is left running, with a warning). Before, the stored command
  ran as it stood, so an app that had exited or been restarted left its pid to another process of
  the phone, which `stop` terminated. The device lists an executable as a file URL with its path
  percent-escaped, so an app named `My App` or `Café` runs in `My%20App.app` or `Caf%C3%A9.app`; the
  listing's file URL is decoded once, and only the decoded path is compared with the app's bundle
  (the readiness probe too), so such an app is recognised and terminated, and not taken for another
  process, which would have left it running with the record removed. A listing that does not decode,
  or names another URL scheme, confirms nothing: `stop` terminates nothing and warns
  `ios.device.stop_unconfirmed`. (Not seen on a phone.)
- **iOS:** `icm stop ios-device` compares a listed executable with the app's bundle only after
  decoding its file URL once, never as raw text. So a bundle named `100%25.app` is not taken for
  the app `100%.app`, which the device lists as `100%25.app/…`.
- **iOS:** `icm stop ios-device` compares the listed executable and the app's install in Unicode
  NFC, as the file system does, since it finds one name under its NFC and its NFD spelling alike.
  A listing in the other form (`Cafe` and U+0301 for `Café`) is the app, not another process.
  Before, the byte comparison took it for another process, so `stop` said the app was not running
  and removed the record while it ran. icm now depends on the `unicode-normalization` crate for
  this. (Fake tools only; not seen on a phone.)
- **iOS:** `icm stop ios-device` ends the recorded app pid only while the device lists it running
  the app's executable in the directory devicectl installed the app in (`app.install_url` in the
  record, from the install output's `installationURL`). Before, only the last two path components
  (`<bundle>/<binary>`) were compared, so a copy of an app with the same bundle name, in another
  container and under the reused pid, was terminated and reported as terminated. A record with no
  install URL, or an install output that does not name the app, leaves the pid unconfirmed: nothing
  is terminated and the record stays. (Fake tools only; the `installationURL` field is not seen on
  a phone.)
- `icm stop ios-device` keeps the record when the device lists the recorded app pid with no usable
  executable path (none, an empty one, or one that does not decode). That is not proof the app is
  gone: nothing is terminated, `ios.device.stop_unconfirmed` warns, the device is listed as
  `unverified` and the record stays for the next `stop`. Before, the record was removed and the app
  counted as stopped. (Not seen on a phone.)
- `icm stop ios-device` keeps the record when its terminate command fails. The entry is not counted
  under `stopped`, the device is listed as `unverified`, and the summary says the app may still be
  running because its terminate command failed. The console process icm started is still ended, and
  the next `stop` asks the device again. The result lists the entry under `kept`, with the pids
  icm left running (named in the summary) and the processes it ended. Before, the record was
  removed and the app counted as stopped, so a retry after the tool recovered said nothing was
  running. (Fake tools only; not seen on a phone.)
- Plain `icm stop android` (and `icm stop --all`) says why it left the app running when the device
  names another project as the emulator's owner (`android.emulator.shared`), with the `adb` command
  that stops the app by hand, where it said nothing.
- `icm ps` judges an iOS Simulator session's app by the start time `run` recorded, as `icm stop
  ios-sim` does, where it still listed a pid another process had taken as the running app when the
  session file had been rewritten since (`shot`, `run --attach`).
- `icm stop android --dry-run` states the rules the stop follows for the app and the emulator.
- `icm doctor --fix --yes` in a new app on a machine without the toolchain the app's
  `rust-toolchain.toml` pins now finishes the job in one run. It installs the toolchain, then
  resolves the app again and checks what needs it: the app's `Cargo.lock` and the wasm-bindgen CLI
  it names, `deps.skew` and the AVD's API level. Before, the app's first failure (`cargo metadata`
  could not run without the toolchain) stayed the result after the fix: exit 4
  (`env.rust_toolchain_missing`) under the summary `ready`, with no lock and no wasm-bindgen CLI, so
  `icm release web` could not run until a second `doctor`. Seen on the first runs of the web and
  Android workflows on GitHub's Linux runner. Without `--fix`, the missing toolchain is now reported
  once, by the `rust.toolchain` check with its fix, and the hint of a check without a platform no
  longer reads `icm doctor  --fix` with two spaces.

Text input on Android, and secure fields on Android and iOS:

- **Android:** the system keyboard composes, suggests, autocorrects and swipe-types into
  `text_input` and `text_editor`, and accented letters and CJK arrive whole. With NativeActivity
  (the default) on Android 8 (API 26) and later, `iced_winit` embeds a small dex
  (`winit/android/classes.dex`, built from `winit/android/java/` by `winit/android/build-dex.sh`)
  and loads it at run time, so APKs still have no Java code: its view gives the keyboard an input
  connection, and the shell turns what the keyboard does to that view's text into key presses,
  commits and preedits for the focused field. A word the keyboard composes (CJK input methods and
  some other keyboards; Gboard on the Android 16 emulator commits each letter in Latin-script
  languages) shows underlined at the caret and joins the value once the keyboard commits it. The
  keyboard sees only the text it typed since it last started over: when the field got the focus,
  at a touch that starts, after its Enter, Tab, Escape or arrow keys, after a caret moved from code,
  and at the other points `docs/agents/limitations.md` lists. Before, the keyboard could only send
  key events: no composition, suggestions or swipe typing, and characters outside the key map were
  lost. Before Android 8 and when the input connection cannot be installed (one warning in the log),
  nothing changes, but when only the keyboard's view could not be added to the Activity (its
  warning is under the log tag `iced_mobile`, not `iced`), a tap on the focused field or focusing a
  field from code brings back a keyboard the user hid. Nor with GameActivity, whose input connection
  hands the keyboard's text to winit as text events that the vendored winit drops, so what the
  keyboard commits there does not reach fields, which the docs now say.
- **Android:** everything the keyboard types, a single letter and its Paste included, reaches a
  field as an input method's commit, as from a desktop input method: `text_input` hands it to
  `on_input`, never to `on_paste`, and `text_editor` gets it as `Edit::Paste`, never as
  `Edit::Insert`, so an editor that drops `Edit::Paste` takes no typing from the keyboard. Its
  deletions and caret moves come as Delete, Backspace and arrow keys, one per cluster, pressed and
  released: what it deletes in the text it typed goes with Delete from the start of the change,
  which takes a whole cluster in a `text_editor` too, where Backspace deletes one character. The
  caret moves inside the text it typed go in the order of the text, also in a right-to-left line of
  a `text_editor` (below). Subscriptions (`keyboard::listen`, `event::listen`) see what the field
  does not capture, every release among them, and when a modifier is held on a hardware keyboard, or
  for Select all (Ctrl+A), a `ModifiersChanged` before the key and one after it that puts the
  window's back.
  Whatever the field, the keyboard is told it edits single-line text with no action
  (`IME_ACTION_NONE`), so its action key is a plain Enter, never Done, Next, Search or Send, and the
  keyboard's view is left out of Android's autofill (`IMPORTANT_FOR_AUTOFILL_NO`), so autofill (a
  password manager's, and the credentials it suggests in the keyboard) offers nothing, in password
  fields either.
- **Android:** the keyboard's Enter, from its key, its action button or a commit of a lone line
  break, reaches the app as the Enter key after the text typed before it, and submits a
  `text_input`; its Tab, from its key or a commit of a lone tab, is the Tab key. The app handles
  Enter, Tab and Escape, builds its view again and runs the widget operations its `update` returns
  for them, before the text typed after them reaches a field, also when a tap or an operation comes
  in between: a field cleared on submit gets that text alone, and the field `on_submit` moves the
  focus to gets it. None of that text is dropped while a field has the focus, however soon it
  follows; when the key leaves no field with the focus (Escape unfocuses a `text_input` or a
  `text_editor`, an `on_submit` may remove the field), what the keyboard sent right after it is
  dropped. A key the app handles from a subscription (`keyboard::listen`, how an app takes Tab) or a
  task comes after that text, which then goes to the field that had the focus. Since such a message
  may still move the focus, a word the keyboard composes after an Enter, Tab or Escape, its own or a
  hardware keyboard's, shows in no field, the one that has the focus included, until a short window
  closes (100 milliseconds after the key's turn ends): then the first redraw shows it in the field
  that has the focus, and the shell asks for that redraw itself while a word waits. A tap on the
  field, or a focus move from code, shows it at the next redraw. A focus move from a subscription
  that comes after the window is not waited for: a word the keyboard sends before it shows in the
  field that has the focus until the move ends the session. The hold ends with the keyboard's
  session, and the next session gives the field's purpose before the keyboard shows. What it commits
  reaches the field that has the focus at once. Line breaks and tabs inside longer committed text are
  dropped. Backspace and Delete reach the field's text before and after what the keyboard typed,
  also from keyboards that delete with `deleteSurroundingText` (AOSP's keyboard and its forks), and
  so do the arrow, Home and End keys, without dropping keys sent in quick succession. The keyboard's
  other keys no longer reach the app, as they did as key events before: its Ctrl and Meta shortcuts
  (Ctrl+C, X, V and A on keyboards that have a Ctrl key), Page Up and Down, function keys and Insert
  are dropped, and its keys come without modifiers, its own Shift or one held on a hardware
  keyboard: its arrows, Home and End move the caret without selecting, and a Ctrl held on a hardware
  keyboard does not make its deletions delete words. An arrow while the keyboard has selected some
  of the text it typed (its Select word) ends that selection at its start (Left, Up) or its end
  (Right, Down) instead, as the field's own arrows end a selection. A hardware keyboard's keys reach
  the app as key events, as before, unless the input method takes them: while a field asks for the
  keyboard, one that composes a hardware keyboard's letters (CJK input methods) types them into the
  field as commits and preedits, not `KeyPressed` events.
- **Android:** `text::editor::Motion` has two new variants there, `Previous` and `Next`: one
  grapheme cluster back and on in the order of the text, or across a line break, whatever the
  direction of the line, where `Left` and `Right` move on screen. An exhaustive `match` on `Motion`
  in an Android build needs arms for them; on every other target `Motion` is upstream's, so code
  written against crates.io `iced_core` 0.14 builds there unchanged. The shell presses the
  keyboard's caret moves inside the text it typed as ArrowLeft and ArrowRight with no physical key
  (`Physical::Unidentified(NativeCode::Unidentified)`), and `text_editor`
  (`Binding::from_key_press`) takes those as `Previous` and `Next`, so an edit the keyboard makes
  before the caret, such as the period it puts before the space after a word, lands in the right
  place in a Hebrew or Arabic line too. `text_input` moves in the order of the text for any arrow.
  The keyboard's own arrow keys keep their physical key and move on screen.
- **Android:** the keyboard's editing panel (Gboard's text editing tools) works in fields: Select all
  selects the field's text, as the field's own Ctrl+A does, and Paste inserts the clipboard's text
  at the caret without its line breaks and tabs. Cut and Copy take what the keyboard selected in the
  text it typed since it last started over, never a password.
- **Android:** any touch that starts (a tap, but also the start of a scroll or a drag, or another
  finger), a mouse button pressed, a hardware key other than a modifier or the window losing the
  focus commits the word being composed, and the app takes the commit, and builds its view again,
  before the touch: a Send button enabled only once the field holds text takes the tap that finished
  the word. Shift, Ctrl, Alt and the other modifiers leave the word alone.
- **Android:** when the app moves the focus from code (`operation::focus`, `focus_next`,
  `focus_previous`, or `focusable::unfocus` run with `iced::advanced::widget::operate`) or the
  field leaves the view, the word being composed is dropped and the field's preedit cleared. The
  window gets `input_method::Event::Closed` then, and whenever no field asks for the keyboard any
  more (a tap elsewhere, Escape), as winit's desktop backends send it when the input method is
  turned off, but never `input_method::Event::Opened`, which they send when it is turned on, so a
  widget sees `Closed` with no `Opened` before it; before, Android sent neither. The next field's
  keyboard starts with no text. A caret moved from code in the field the keyboard types into
  (`move_cursor_to`, `select_all`) commits the word where it was typed first, then moves; the caret
  of another field moves and leaves the word alone. What the keyboard typed before such an
  operation, and what it sends while the operation runs, reaches the field before the caret moves.
  Any widget operation the app returns while the keyboard has sent edits the field has not had yet,
  or while the app takes its turn after the keyboard's Enter, Tab or Escape, a scroll or a query
  too, runs once the field has them and the app has handled them, a turn of the event loop later,
  and the operations returned after it wait behind it, in order. An app whose `on_input`, or whose
  `text_editor` `on_action`, refuses or changes what the keyboard typed starts the keyboard over
  with `operation::move_cursor_to_end` on a `text_input` or `operation::focus` on a `text_editor`
  (`docs/agents/limitations.md`).
- **Android:** while no field asks for the keyboard, the keyboard's view is no text editor, so a
  hardware keyboard's keys reach the app as before, also with input methods that compose hardware
  keys.
- **Android:** tapping a field brings back the keyboard the user hid, also a `text_editor` focused
  from code, which otherwise ignores the tap. A tap elsewhere leaves it hidden. Focusing a field
  from code (`operation::focus`) brings it back too, also the field that has the focus (a
  `text_editor` an Edit button focuses again), and so does the window getting the focus back
  (after the notification shade, a system dialog or another app), since nothing tells the shell
  that the user hid it. From Android 11 the shell also shows the keyboard again when the system
  drops a show, which it can around a restart of the input or the window getting the focus, until
  it sees the keyboard come up; a keyboard the user hides right after it came up stays hidden. Only
  one that was still up as the window got the focus back, or as the focus moved between a password
  field and another kind of field, can come back once when the user hides it within half a second.
  Before, the field had to lose the focus once, as it still has with GameActivity, before Android 8
  and when the input connection cannot be installed (unless only the keyboard's view could not be
  added).
- Password fields (`.secure(true)`) are marked secure to the system keyboard on Android with the
  input connection above, and on iOS: the Android keyboard shows its password layout without
  suggestions. On Android the input type is `TYPE_TEXT_VARIATION_PASSWORD` with
  `IME_FLAG_NO_PERSONALIZED_LEARNING`, and what a keyboard composes for a password field reaches it
  as typed text, never as a preedit drawn in clear, also when the keyboard composed it as the last
  field's keyboard. The shell learns a field's purpose only at a redraw, so what the keyboard
  composes after the focus moves (the Enter whose `on_submit` focuses the password field, a tap, a
  focus move from code) shows in no field until a redraw has decided the purpose of the field that
  has the focus, and in a password field that redraw commits it as typed text. After an Enter, Tab
  or Escape the redraw decides only once a tap on the field or a focus move from code has come after
  the key, or the short window after the key has closed, or a keyboard session starts for the field
  again (above). A field the app makes secure
  while the keyboard composes gets its composition as typed text at the next redraw, and what the
  keyboard composes before it is set up for the password field is read as typed text too. On Android
  and iOS the window never draws a password field's preedit (the desktop and the web draw it as
  upstream does). What the keyboard commits is not held back: a password it commits in the same
  burst as a Tab the app takes in a subscription reaches the field that had the focus, in clear
  (`docs/agents/limitations.md`). On an edit of a password field, a `text_input` builds its
  paragraph from the bullets: the paragraph logs the text it is built from at trace level, which in
  v0.14.1-mobile.2 was the password. On iOS the vendored winit implements `Window::set_ime_purpose`
  (`vendor/patches/winit-0003-ios-ime-purpose.patch`): the view answers `isSecureTextEntry` and
  turns off autocorrection, spell checking, auto-capitalisation and smart punctuation for a
  password, the terminal purpose turns off the same without secure entry, and a changed purpose
  reloads the keyboard that is shown. `iced_winit` gives the view a field's purpose before it makes
  the view the first responder, so a keyboard shown while none was up is set up for its field at
  once, with no reload. On the iOS 27.0 Simulator the password keyboard has no emoji or dictation
  key, and the keyboard switches to and from it as the focus moves between a password field and
  another; a physical iPhone has not been tried. When the focus moves straight from a password field
  to another field, with the keyboard up, the keyboard there can label its numbers key ".?123"
  instead of "123" until its numbers are shown once or it is hidden and shown again; it types as
  usual. The iOS keyboard shows no suggestions in any field anyway. Before, the keyboard was not
  told that a field held a password.
- On Android and iOS, a `text_editor` focused from code gives up its focus when the user taps
  outside it, as a `text_input` does. Before, only a mouse press outside it took the focus, and a
  touch is the only press there, so it kept the focus when the user tapped another field and took
  the keyboard's text alongside it, a password included, which it showed in clear. While both still
  ask for the keyboard, `InputMethod::merge` makes the merged request secure there when any of them
  is, so the keyboard is set up for the password field. A tap still cannot focus a
  `text_editor`, and the desktop and the web keep upstream's rule.
- On Android and iOS, `text_editor` draws no placeholder while it shows a preedit, as `text_input`
  does, so a word the Android keyboard composes into an empty editor is not drawn over its
  placeholder (iOS sends no preedit). The desktop and the web draw it as upstream does.
- The known limitations in an app's `AGENTS.md` describe text input as it now is, and say that the
  iOS keyboard shows no predictions in any field: the app's view takes key input only.

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
  a newer icm. A `min_icm` must name a release (`0.14.1-mobile.N`); a plain `0.14.1`, which no icm
  meets, is `config.invalid`. Every fix command runs as it is: where icm cannot name the release
  to install, the command lists the release tags.
- `icm stop --shutdown` leaves running a simulator that another project's run booted, as it
  already did for the emulator, and `stop --all --shutdown` without a session considers only the
  simulator the project's runs use.
- Every file a run directory keeps from an app's or a tool's output (log copies, consoles, crash
  reports) redacts secret values on every platform, as stdout does, also where the output escapes
  them as JSON (inside a log line too) or percent-encodes them in any URL encoding. A value of a
  secret-named variable that starts with `/` counts unless it is a path. The live files in
  `target/icm/sessions/` stay the app's own output while it runs; a session keeps the values of
  its app's secret-named `--env` (on the web, the page's query) in a `secrets.json` (mode 0600)
  beside them, so `logs`, `shot` and `stop` from a shell without the secret redact it too. icm
  keeps no value of a secret-named variable it only inherits (as a desktop app does) in any file:
  a later command reads it from the running desktop app's environment, and the command that stops
  the app redacts its live files. `logs` of a desktop app that ended by itself, from a shell
  without such a variable, reads the run's redacted copies and warns
  `desktop.logs.secret_unknown`.
- Android: readiness and logs count only the app's own processes, the lifecycle suite tells a
  destroyed Activity from a live one after a rotation, and a rotation warns when the app is locked
  to one axis.
- Releases: `icm ledger mark-uploaded` refuses a release that cannot have been uploaded, literal
  signing secrets stay out of every finding and record, a value of a secret-named variable that
  the build baked into a shipped file fails `release.secret_in_artifacts` (the release is then not
  uploadable; `icm verify` checks it again), `upload.sh` runs `icm diagnose` when a store tool
  fails, `THIRD_PARTY_NOTICES` lists the Rust standard library, the AppImage leaves
  `libwayland-client` to the host, and `icm verify macos` accepts a stapled app or DMG.
