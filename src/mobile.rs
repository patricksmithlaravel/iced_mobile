//! Run your application on Android and iOS.
//!
//! An iced application runs on a phone unchanged: the same
//! [`application`](crate::application()), the same `update` and `view`, the
//! same `run`. What a phone changes is how the program starts:
//!
//! - **Android** loads the crate's library as a shared object (a `cdylib`)
//!   and calls an exported `android_main` function in it, on a thread of its
//!   own. [`android_main!`](crate::android_main) defines that function.
//! - **iOS** starts an ordinary binary. Its `main` calls [`init_logger`] and
//!   then your `run` function, which never returns on iOS: winit hands the
//!   thread to UIKit.
//!
//! Depend on `iced` alone. Everything a mobile entry point needs from the
//! shell is re-exported here. A crate that also depends on `iced_winit`
//! directly must take it from the same source as `iced`, character for
//! character, or the build holds two copies of the shell and
//! `set_android_app` fills the one `iced` does not use. Do not depend on
//! winit from crates.io either: iced runs on a fork of it (see
//! [winit](#winit)).
//!
//! # Features
//!
//! iced's default features include four for mobile. Three do nothing on
//! other targets; `mobile-logger` also gives the desktop and the web a
//! logger, through [`init_logger`]:
//!
//! - `android-native-activity`: runs in Android's `NativeActivity`, which
//!   needs no Java code. An Android build needs exactly one activity feature.
//! - `mobile-logger`: lets [`init_logger`] install a logger: logcat,
//!   os_log, stderr on the desktop, the console on the web.
//! - `mobile-fira-sans`: embeds Fira Sans and makes it the default font. It
//!   is licensed under the SIL Open Font License 1.1: ship the notice in
//!   `graphics/fonts/OFL.txt` of this repository with the app.
//! - `mobile-system-fonts`: lets text fall back to the system's fonts for
//!   the scripts the embedded fonts lack (CJK, Arabic, Hebrew, Indic, Thai,
//!   ...), and for serif and monospaced text.
//!
//! With `default-features = false`, all four are gone along with the rest,
//! and nothing says so. List the ones the app needs:
//!
//! ```toml
//! iced = { git = "...", rev = "...", default-features = false, features = [
//!     "wgpu", "tiny-skia", "thread-pool", # a renderer and an executor
//!     "android-native-activity",          # or "android-game-activity"
//!     "mobile-logger",
//!     "mobile-fira-sans",
//!     "mobile-system-fonts",
//! ] }
//! ```
//!
//! - Without an activity feature an Android build fails inside
//!   android-activity, with errors that never name iced:
//!   ``error[E0583]: file not found for module `activity_impl` `` and
//!   `Either "game-activity" or "native-activity" must be enabled as
//!   features`, followed by advice about multiple versions and `[patch]` that
//!   does not apply. Add `android-native-activity`.
//! - Without `mobile-logger`, [`init_logger`] installs no logger: iced's logs
//!   and the panics its hook logs go nowhere, unless the app installs a
//!   logger itself.
//! - Without `mobile-fira-sans`, the default font is the system's sans-serif
//!   (Roboto, Helvetica Neue) if `mobile-system-fonts` is on. Without both,
//!   text in the default font is not drawn at all: set a `default_font` the
//!   app embeds, and load it.
//!
//! # Example
//!
//! `Cargo.toml`:
//!
//! ```toml
//! [package]
//! name = "myapp"
//! edition = "2024"
//!
//! [dependencies]
//! iced = { git = "https://github.com/patricksmithlaravel/iced_mobile", rev = "<full commit hash>" }
//! ```
//!
//! With `src/lib.rs` and `src/main.rs`, Cargo builds a library and a binary,
//! both named `myapp`. The binary is the desktop and the iOS executable.
//! Android needs the library as a `cdylib`; ask for it in the Android build
//! only:
//!
//! ```sh
//! cargo rustc --lib --crate-type cdylib --target aarch64-linux-android
//! ```
//!
//! Listing `crate-type = ["cdylib", "rlib"]` under `[lib]` works too, but
//! then every desktop build links a `cdylib` it does not use, and on Windows
//! (MSVC) the library's `myapp.pdb` and the binary's collide in the output
//! directory (Cargo warns of an "output filename collision",
//! rust-lang/cargo#6313). If you list it, give the binary another name with a
//! `[[bin]]` section, and name the iOS bundle's executable after it.
//!
//! `src/lib.rs`:
//!
//! ```no_run,standalone_crate
//! use iced::widget::{button, column, text, Column};
//!
//! pub fn run() -> iced::Result {
//!     iced::run(update, view)
//! }
//!
//! // Android's entry point. Defines nothing on other targets.
//! iced::android_main!(run);
//!
//! #[derive(Debug, Clone)]
//! enum Message {
//!     Increment,
//! }
//!
//! fn update(value: &mut u64, message: Message) {
//!     match message {
//!         Message::Increment => *value += 1,
//!     }
//! }
//!
//! fn view(value: &u64) -> Column<'_, Message> {
//!     column![text(value), button("+").on_press(Message::Increment)]
//! }
//! ```
//!
//! `src/main.rs`, for iOS and the desktop:
//!
//! ```no_run,standalone_crate
//! # mod myapp { pub fn run() -> iced::Result { Ok(()) } }
//! fn main() -> iced::Result {
//!     iced::mobile::init_logger(); // os_log on iOS; stderr on the desktop
//!     myapp::run()
//! }
//! ```
//!
//! # Android
//!
//! - The manifest's `android.app.lib_name` meta-data must be the library's
//!   name (`myapp` for `libmyapp.so`), or Android finds no `android_main`.
//! - Give the activity `android:launchMode="singleTask"`. iced runs one
//!   Activity at a time, and the process ends if Android starts a second
//!   one beside it (into another task, from another app or a
//!   notification); with `singleTask`, Android hands such a launch to the
//!   running Activity instead.
//! - Give the activity the full `android:configChanges` list, so that the
//!   application keeps its state:
//!   `mcc|mnc|locale|touchscreen|keyboard|keyboardHidden|navigation|orientation|screenLayout|uiMode|screenSize|smallestScreenSize|density|layoutDirection|colorMode|grammaticalGender|fontScale|fontWeightAdjustment`,
//!   and `assetsPaths` when the manifest is linked against API 36 or later
//!   (older android.jar files do not know the name). With it, rotation, a
//!   dark-mode switch, a new font scale, a change of resource overlays
//!   (SystemUI applies its theme overlays during an emulator's first boots)
//!   and the like leave the Activity in place (a rotation arrives as a
//!   resize). Without it, Android destroys the Activity for each of them and
//!   starts a new one, and the application starts over ([Activity
//!   destruction](#android-activity-destruction)).
//! - Back: with targetSdk 36, Android handles Back itself (predictive back)
//!   and finishes the Activity, and the application ends with it; the next
//!   launch starts it over. To handle Back in the application instead
//!   (going back a screen, say), set
//!   `android:enableOnBackInvokedCallback="false"` on the `<application>`:
//!   Back then reaches the app as a key press,
//!   `Key::Named(Named::BrowserBack)`, and nothing else happens, as with a
//!   lower targetSdk. To leave the screen on Back at the app's root and keep
//!   the application's state, call `Activity.moveTaskToBack(true)` through
//!   JNI.
//! - `cargo check --target aarch64-linux-android` needs no NDK with
//!   `NativeActivity`; `cargo build` and `cargo rustc` need the NDK's linker.
//! - For a GameActivity, turn on `android-game-activity`, set
//!   `default-features = false` and list the other features as above. Cargo
//!   turns a feature on for the whole build when any crate asks for it, so
//!   every crate in the graph that depends on `iced` must do the same. If one
//!   uses iced's defaults (third-party widget crates usually do, and so does
//!   every crates.io dependent when this fork replaces iced through
//!   `[patch.crates-io]`), `android-native-activity` comes back and
//!   android-activity stops the build: `The "game-activity" and
//!   "native-activity" features cannot be enabled at the same time`. Such a
//!   graph can only use `NativeActivity`.
//!
//! # iOS
//!
//! Apps built with the iOS 27 SDK must adopt UIKit's scene life cycle, or
//! UIKit stops them at launch and says why only in the system log. Add this
//! to the top-level `<dict>` of the app's `Info.plist`; iced puts its windows
//! into the scene it declares:
//!
//! ```xml
//! <key>UIApplicationSceneManifest</key>
//! <dict>
//!     <key>UIApplicationSupportsMultipleScenes</key>
//!     <false/>
//!     <key>UISceneConfigurations</key>
//!     <dict>
//!         <key>UIWindowSceneSessionRoleApplication</key>
//!         <array>
//!             <dict>
//!                 <key>UISceneConfigurationName</key>
//!                 <string>Default</string>
//!             </dict>
//!         </array>
//!     </dict>
//! </dict>
//! ```
//!
//! Without it, iced prints this block to stderr and the log at launch; in a
//! debug build linked with the iOS 27 SDK or later it also panics.
//!
//! # Safe area
//!
//! A phone application fills the screen: it draws under the status bar, the
//! notch or Dynamic Island, the home indicator or navigation bar, and the
//! on-screen keyboard. [`safe_area()`] reports what covers each edge, as a
//! [`SafeArea`] in the same logical pixels as the layout, and
//! [`SafeArea::padding`] turns it into the padding of the root container:
//!
//! ```no_run
//! use iced::mobile::{self, SafeArea};
//! use iced::widget::{container, text};
//! use iced::{Element, Fill, Subscription};
//!
//! #[derive(Default)]
//! struct App {
//!     safe_area: SafeArea,
//! }
//!
//! #[derive(Debug, Clone)]
//! enum Message {
//!     SafeAreaChanged(SafeArea),
//! }
//!
//! impl App {
//!     fn update(&mut self, message: Message) {
//!         match message {
//!             Message::SafeAreaChanged(safe_area) => self.safe_area = safe_area,
//!         }
//!     }
//!
//!     fn view(&self) -> Element<'_, Message> {
//!         // 16 around the content, beside what the system covers; the
//!         // bottom rises with the keyboard.
//!         container(text("Hello"))
//!             .padding(self.safe_area.padding(16))
//!             .width(Fill)
//!             .height(Fill)
//!             .into()
//!     }
//!
//!     fn subscription(&self) -> Subscription<Message> {
//!         mobile::safe_area().map(Message::SafeAreaChanged)
//!     }
//! }
//!
//! pub fn run() -> iced::Result {
//!     iced::application(App::default, App::update, App::view)
//!         .subscription(App::subscription)
//!         .run()
//! }
//! ```
//!
//! - **iOS:** the window's safe-area insets (the status bar, the Dynamic
//!   Island or notch, the home indicator), and the keyboard's height from
//!   UIKit's keyboard notifications.
//! - **Android:** the system bars and the display cutout, and the
//!   keyboard's height, from the root view's `WindowInsets` (read through
//!   JNI). On Android 15 and later an app that targets SDK 35 or later is
//!   drawn edge to edge, under both bars.
//! - **Desktop and web:** [`SafeArea::ZERO`], once. A mobile browser's
//!   notch is not reported.
//!
//! The first value arrives when the window opens, then one per change:
//! rotation, a new display cutout, the keyboard showing or hiding, and a
//! new scale factor of the application's own
//! ([`Application::scale_factor`](crate::Application::scale_factor)), which
//! changes the logical pixels the same insets measure. On
//! Android a change can take a few tenths of a second to settle: the shell
//! reads again while the system bars settle, and every quarter of a second
//! while a text input has the focus, since the keyboard sends no event of
//! its own when the app draws edge to edge. A turn from one landscape to
//! the other resizes nothing either: the shell compares the display's
//! rotation after a redraw, which Android asks for then. A keyboard counts
//! only while a text input of the app has the focus (and for the second it
//! takes to slide away), so the keyboard another app left up does not
//! raise the bottom when the app comes back. On iOS the window can report
//! zero insets for a frame, under the launch screen, before it is in its
//! scene. On phones every window fills the screen, so they share one safe
//! area.
//!
//! [`SafeArea::keyboard`] is measured from the bottom edge, so it covers the
//! bottom inset: [`SafeArea::padding`] takes the larger of the two.
//!
//! Headless, `icm`'s harness (`iced_test::agent`: `.ice` flows, `shot
//! --headless` and `ui --headless`) stands in for the shell: a viewport the
//! size of a device preset gets that device's safe area before the
//! application boots (`iphone-17`: 62 top and 34 bottom; `iphone-se`: 20
//! top; `pixel-9`: 54.1 top and 24 bottom; zero for `web-mobile` and
//! `desktop`), so headless layouts match the device's. A viewport of any
//! other size gets none, and `iced_test`'s `Simulator` runs no
//! subscription: keep a padding of your own until a value arrives, or set
//! one in a unit test with [`SafeArea::new`] and
//! [`SafeArea::with_keyboard`].
//!
//! # Logs
//!
//! With [`init_logger`] (which [`android_main!`](crate::android_main) calls
//! for you), iced's messages, your `log` calls and every panic go to:
//!
//! - Android: `adb logcat -s iced` (stdout and stderr are under
//!   `RustStdoutStderr`). For `debug!` records, run `adb shell setprop
//!   log.tag.iced DEBUG` (`VERBOSE` for `trace!`), then end the process
//!   (`adb shell am force-stop <package>`) and start the app again: the
//!   level is read once per process, and a new Activity may run in the old
//!   one.
//! - iOS: `xcrun simctl spawn booted log stream --level info --predicate
//!   'subsystem == "iced"'`. Records up to `info!` pass; launch with
//!   `SIMCTL_CHILD_RUST_LOG=debug` for `debug!`, which the unified log shows
//!   only with `--level info` or `--level debug` (`trace!` needs
//!   `--level debug`).
//! - Desktop: stderr, one line per record. `RUST_LOG` takes `env_logger`'s
//!   directives (`debug`, `info,iced_wgpu=warn`, ...).
//! - Web: the browser's console. The page's `rust_log` query parameter
//!   takes the same directives (`?rust_log=debug`).
//!
//! # Clipboard
//!
//! [`clipboard::read`](crate::clipboard::read) and
//! [`clipboard::write`](crate::clipboard::write) work on phones: they read
//! and write the system's clipboard as text, through `ClipboardManager` on
//! Android and `UIPasteboard` on iOS. The clipboard needs no window there,
//! so a task from the boot function works too. Each system adds rules of
//! its own:
//!
//! - Android 10 and later let only the app with the input focus read the
//!   clipboard: a read in the background gives `None`. Android 12 and later
//!   show a toast when an app reads what another app copied, and 13 and
//!   later confirm a copy with an overlay. An item without text (a URI or an
//!   `Intent`) reads as `None`, unless the clip says it is text.
//! - iOS 16 and later ask the user ("Allow Paste") when an app reads text
//!   that another app copied, unless they allowed it in Settings. The read
//!   waits for the answer, and gives `None` when they decline.
//!
//! So read only when the user asked to paste, never at launch or on a
//! timer. `clipboard::read_primary` gives `None` and
//! `clipboard::write_primary` does nothing: phones have no primary
//! selection. There is no long-press edit menu: give the fields that need
//! it Copy and Paste buttons.
//!
//! On Android, `text_input` and `text_editor` also take Ctrl+C, X, V and A
//! from a hardware keyboard (an emulator's host keyboard, ChromeOS, a
//! keyboard over USB or Bluetooth): winit reports no modifier keys there,
//! so the shell follows them itself, and lets go of them when the window
//! loses the focus. iOS hardware keyboards send no modifier keys, so the
//! shortcuts never fire there.
//!
//! ```no_run
//! use iced::Task;
//!
//! #[derive(Default)]
//! struct App {
//!     address: String,
//!     draft: String,
//! }
//!
//! #[derive(Debug, Clone)]
//! enum Message {
//!     Copy,
//!     Paste,
//!     Pasted(Option<String>),
//! }
//!
//! fn update(app: &mut App, message: Message) -> Task<Message> {
//!     match message {
//!         Message::Copy => iced::clipboard::write(app.address.clone()),
//!         Message::Paste => iced::clipboard::read().map(Message::Pasted),
//!         Message::Pasted(text) => {
//!             app.draft = text.unwrap_or_default();
//!             Task::none()
//!         }
//!     }
//! }
//! ```
//!
//! # Events for launchers
//!
//! A launcher such as `icm` learns that the app started, drew its first
//! frame, was suspended or resumed, panicked or stopped from `ICM_EVENT`
//! lines, which the shell writes only when the run opts in: `ICM_EVENTS=1`
//! in the environment (desktop; `SIMCTL_CHILD_ICM_EVENTS=1` on the iOS
//! simulator), `adb shell setprop debug.icm.events 1` on Android, or
//! `?icm_events=1` in the page's address on the web. `iced_winit::icm`
//! describes the protocol. On Android, `adb shell setprop debug.iced.backend
//! tiny-skia` also does what `ICED_BACKEND` does elsewhere.
//!
//! # Lifecycle
//!
//! Two channels report the application's life:
//!
//! - [`lifecycle()`] is a subscription that delivers a [`LifecycleEvent`]
//!   to `update`: [`Foreground`](LifecycleEvent::Foreground),
//!   [`Active`](LifecycleEvent::Active),
//!   [`Inactive`](LifecycleEvent::Inactive),
//!   [`Background`](LifecycleEvent::Background) and
//!   [`MemoryWarning`](LifecycleEvent::MemoryWarning), with the same meaning
//!   on iOS and Android. Hide what is on screen on `Inactive` (Control
//!   Center, a Face ID prompt, a call, the notification shade), lock or pause
//!   on `Background`, free caches on `MemoryWarning`. The messages arrive a
//!   moment after the event. More variants may be added, so a `match` on it
//!   needs a wildcard arm.
//! - [`on_lifecycle`] runs a hook on the event loop's thread whenever winit
//!   reports the application suspended or resumed, before iced acts on it,
//!   with a [`Lifecycle`]: save there what must outlive the process.
//!   [`Lifecycle::Suspended`] means different things per platform: on iOS
//!   the application is about to stop being active, which also happens for
//!   Control Center, notifications and Face ID; on Android its window is
//!   going away. On the web it fires when the page goes into the
//!   back-forward cache. The desktop never sends it. [`Lifecycle`] has two
//!   variants, `Suspended` and `Resumed`, and no others.
//!
//! [`LifecycleEvent`] and [`Lifecycle`] have the full tables. Lock on
//! `Background`, not on `Inactive` or `Suspended`: an unlock that asks for
//! Face ID makes the app inactive again, and would loop.
//!
//! ```no_run,standalone_crate
//! use iced::mobile::{self, Lifecycle, LifecycleEvent};
//! use iced::widget::text;
//! use iced::{Element, Subscription};
//!
//! #[derive(Default)]
//! struct Wallet {
//!     hidden: bool,
//!     locked: bool,
//! }
//!
//! #[derive(Debug, Clone)]
//! enum Message {
//!     Lifecycle(LifecycleEvent),
//! }
//!
//! impl Wallet {
//!     fn update(&mut self, message: Message) {
//!         match message {
//!             Message::Lifecycle(LifecycleEvent::Inactive) => {
//!                 self.hidden = true;
//!             }
//!             Message::Lifecycle(LifecycleEvent::Active) => {
//!                 self.hidden = false;
//!             }
//!             Message::Lifecycle(LifecycleEvent::Background) => {
//!                 self.locked = true;
//!             }
//!             Message::Lifecycle(_) => {}
//!         }
//!     }
//!
//!     fn view(&self) -> Element<'_, Message> {
//!         text(if self.hidden || self.locked { "****" } else { "1 234.56" })
//!             .into()
//!     }
//!
//!     fn subscription(&self) -> Subscription<Message> {
//!         mobile::lifecycle().map(Message::Lifecycle)
//!     }
//! }
//!
//! /// Runs before iced acts: what must outlive the process is saved here.
//! fn hook(event: Lifecycle) {
//!     match event {
//!         Lifecycle::Suspended => save(),
//!         Lifecycle::Resumed => {}
//!     }
//! }
//!
//! fn save() {
//!     // Write what must not be lost to a file.
//! }
//!
//! pub fn run() -> iced::Result {
//!     mobile::on_lifecycle(hook);
//!
//!     iced::application(Wallet::default, Wallet::update, Wallet::view)
//!         .subscription(Wallet::subscription)
//!         .run()
//! }
//! ```
//!
//! On Android:
//!
//! - The messages reach `update` while the Activity is paused or stopped
//!   too, but iced draws only while Android lets the Activity run, so hiding
//!   content on `Inactive` does not reliably keep it out of the Recents
//!   thumbnail. To keep content out of Recents and screenshots, set
//!   `FLAG_SECURE` on the window (`AndroidApp::set_window_flags` with
//!   `WindowManagerFlags::SECURE`).
//! - While a system window opens over the app, the focus can come and go
//!   more than once: `Inactive`, `Active`, `Inactive`.
//! - When Android destroys the Activity (Back), the application can end
//!   before its `Background` message arrives: save in the hook, on
//!   [`Lifecycle::Suspended`], which always comes first.
//! - `MemoryWarning` depends on the Activity. With NativeActivity (the
//!   default) it comes from `onLowMemory`, which is rare; `onTrimMemory`
//!   does not reach the app. With GameActivity (`android-game-activity`)
//!   every `onTrimMemory` arrives as `MemoryWarning`, without its level:
//!   `TRIM_MEMORY_UI_HIDDEN` brings one each time the app goes to the
//!   background, and it cannot be told from a real shortage. Free there
//!   only what is cheap to build again.
//!
//! # Android: Activity destruction
//!
//! Android destroys the Activity on Back (see [Android](#android)), for a
//! configuration change the manifest does not list, or with the developer
//! option "Don't keep activities". The process often lives on, and the next
//! Activity runs in it. iced follows the Activity:
//!
//! 1. [`Lifecycle::Suspended`] has come first, when the window went away
//!    (just before, or when the app left the screen).
//! 2. The event loop ends, and the application is dropped: its state, its
//!    windows, its renderer and its executor. The futures and subscriptions
//!    still running on it end, a subscription stuck on sending a message
//!    (the event loop takes none while the app is in the background) too.
//!    The function that runs it returns `Ok(())`, and
//!    `mobile::activity_destroyed` says why.
//! 3. `android_main` returns, as the Activity's `onDestroy` waits for it.
//! 4. The next Activity calls `android_main` again, on a thread of its own:
//!    the function that runs the application runs again, and a new
//!    application starts, from its boot function.
//!
//! Whatever the application keeps in memory is lost: save what must outlive
//! the Activity on [`Lifecycle::Suspended`], which always comes before.
//!
//! Memory is another matter: Android never destroys a single Activity to
//! free memory. It kills the whole process of an app in the background,
//! without a word to it: nothing above runs, and the next launch starts a
//! new process (a cold start). Saving on [`Lifecycle::Suspended`] covers
//! that case too, since the app was suspended when it left the screen.
//! What belongs to the process stays: the logger, the panic hook, the
//! [`on_lifecycle`] hook (setting the same one again does nothing), the
//! fonts loaded so far (a font the next application loads again, from its
//! settings or with `font::load`, is not added twice), and the
//! application's own statics. A static that keeps an `AndroidApp` (for JNI,
//! say) must take each Activity's new one: a `OnceLock` would keep the
//! first, whose Activity is gone.
//!
//! Since the function that runs the application runs once per Activity,
//! whatever it sets up for the whole process must accept a second call: a
//! logger or `tracing` subscriber of your own, a panic hook, a global
//! runtime. Use the forms that report "already set" instead of panicking
//! (`env_logger::try_init()`, `tracing_subscriber`'s `try_init()`), and
//! ignore that error, or guard the setup with a `std::sync::Once`.
//! `env_logger::init()` and `tracing_subscriber`'s `init()` panic the
//! second time, and a panic in that function ends the process, so the app
//! would crash on every relaunch that reuses it (after Back, say).
//!
//! [`android_main!`](crate::android_main) does steps 3 and 4 for you. The
//! process ends instead after a panic on the thread that runs the
//! application (`update`, `view` and the shell run there), and when the
//! application stops on its own while its Activity is still on screen. A
//! panic on another thread, such as the executor's, which runs tasks and
//! subscriptions, ends that thread or task alone: the hook logs it, and the
//! application goes on (see [`android_main!`](crate::android_main)).
//!
//! Launching the app again a moment after Back, before Android has destroyed
//! the Activity it finished, starts a second Activity while the first one
//! still runs. android-activity 0.6.0 aborts the process then; with 0.6.1,
//! iced cannot build the second event loop and the process ends. Android
//! then starts the new Activity in a new process, or the next launch does.
//!
//! # Dark mode
//!
//! On Android and iOS the shell reads the system's light or dark mode when
//! the application starts and whenever the user switches it, as the desktop
//! does:
//!
//! - An application without `.theme(..)` draws iced's default theme for the
//!   mode, `Theme::Light` or `Theme::Dark`, from its first frame, and
//!   switches with the system while it runs.
//! - [`system::theme`](crate::system::theme) answers the mode, and
//!   [`system::theme_changes`](crate::system::theme_changes) reports it once
//!   at start and again on every switch, for an application that picks its
//!   own themes. A theme set with `.theme(..)` is drawn as it is, whatever
//!   the system's mode. A theme function that returns `Option<Theme>` lets
//!   the system decide again with `None` (a System choice beside Light and
//!   Dark), and the default theme for the system's mode is drawn at once:
//!
//! ```no_run,standalone_crate
//! use iced::widget::text;
//! use iced::{Element, Subscription, Theme, theme};
//!
//! #[derive(Default)]
//! struct App {
//!     mode: theme::Mode,
//! }
//!
//! #[derive(Debug, Clone)]
//! enum Message {
//!     AppearanceChanged(theme::Mode),
//! }
//!
//! impl App {
//!     fn update(&mut self, message: Message) {
//!         match message {
//!             Message::AppearanceChanged(mode) => self.mode = mode,
//!         }
//!     }
//!
//!     fn view(&self) -> Element<'_, Message> {
//!         text("Hello").into()
//!     }
//!
//!     fn subscription(&self) -> Subscription<Message> {
//!         iced::system::theme_changes().map(Message::AppearanceChanged)
//!     }
//!
//!     fn theme(&self) -> Theme {
//!         match self.mode {
//!             theme::Mode::Dark => Theme::TokyoNight,
//!             theme::Mode::Light | theme::Mode::None => Theme::Light,
//!         }
//!     }
//! }
//!
//! pub fn run() -> iced::Result {
//!     iced::application(App::default, App::update, App::view)
//!         .subscription(App::subscription)
//!         .theme(App::theme)
//!         .run()
//! }
//! ```
//!
//! - Android reads the night bits of the `uiMode` in the application's
//!   resources (through JNI; the copy android-activity keeps is not updated
//!   when the Activity handles the change itself). Keep `uiMode` in the
//!   manifest's `configChanges` (see [Android](#android)): without it, a
//!   switch destroys the Activity and the application starts over.
//! - iOS reads the style of the main screen's traits, which is the
//!   system's: a window's override does not change it.
//!
//! The system bars belong to the platform:
//!
//! - Android takes the bars' icon colour from the Activity's theme
//!   (`android:windowLightStatusBar` and `android:windowLightNavigationBar`)
//!   when it creates the Activity. Give those a `values-night` variant (icm
//!   generates one) and the icons suit the mode the application starts in.
//!   A switch while it runs keeps them as they were (dark on dark, or white
//!   on white) until the next launch: iced cannot change them without Java
//!   code.
//! - iOS colours the status bar for the system's mode. That suits the
//!   default theme; an application that forces a light theme in dark mode
//!   (or the reverse) gets a status bar in the other mode's colours.
//!
//! # winit
//!
//! iced builds against winit 0.30.13 vendored in this repository
//! (`vendor/winit`; its `PATCHES.md` lists the patches), which carries
//! Android fixes until they ship in a winit release (winit PR #4739): the
//! event loop ends when the Activity is destroyed, a new one can be built in
//! the same process, and content rect changes are reported (as `Resized`).
//! It also carries an iOS fix still to be proposed upstream: text inserted
//! as only a line break or a tab is reported as the Enter or Tab key.
//!
//! An application must not depend on winit from crates.io: the build would
//! hold two copies of winit, and iced would use its own copy alone. The two
//! share no types but `AndroidApp` (it comes from android-activity), and on
//! iOS both would declare winit's Objective-C classes under the same names.
//! Use what iced re-exports instead:
//!
//! - `iced::mobile::AndroidApp`, for `android_main` and for JNI
//!   (`AndroidApp::vm_as_ptr`, `AndroidApp::activity_as_ptr`);
//! - the rest of winit as `iced_winit::winit`, through a dependency on
//!   `iced_winit` from the same source as `iced`, for instance
//!   `iced_winit::winit::platform::android::activity::WindowManagerFlags`.
//!
//! An application that depended on winit to name `AndroidApp` or to turn
//! on `android-native-activity` drops that dependency. The activity feature
//! comes from iced's defaults, or, with `default-features = false`, from
//! `features = ["android-native-activity", ...]` on `iced` (or on
//! `iced_winit`, which has the same feature). In the code,
//! `winit::platform::android::activity::AndroidApp` becomes
//! `iced::mobile::AndroidApp`, or the same path under `iced_winit::winit`.
//! `cargo tree -i winit --target all` must then list one winit, from the
//! same git source as `iced`. Because winit (and its `dpi` crate) come from
//! that source too, a supply-chain allow-list of git sources needs no new
//! entry for them.
//!
//! # Known limitations
//!
//! - **Exiting.** `iced::exit` and closing the last window are ignored on
//!   Android and iOS, with a warning in the log: the system ends a mobile
//!   app. On Android, finish the Activity through JNI (`Activity.finish`)
//!   to leave: the application ends as when Android destroys it. To end
//!   the process instead, call `libc::_exit`: `std::process::exit` runs
//!   exit handlers that make Android's renderer threads abort (SIGABRT).
//!   On iOS a window being opened can replace the last one: open the new
//!   window before closing the old one.
//! - **One window on Android.** Android gives an app one native window, so
//!   a second `window::open` is refused with an error in the log, and its
//!   task ends without an id.
//! - **No edit menu.** A long press in a text field shows no menu and no
//!   selection handles, and the copy, cut and paste shortcuts of text
//!   fields fire only from a hardware keyboard on Android. The
//!   [clipboard](#clipboard) itself works: offer Copy and Paste buttons
//!   where they matter.
//! - **Line breaks in inserted text on iOS.** UIKit hands over text one
//!   insertion at a time. An insertion that is only a line break (`"\n"`,
//!   `"\r"`, or `"\r\n"` counted once) is one Return, as from the
//!   keyboard's Return key, and one that is only `"\t"` is Tab, however
//!   soon after other keys it comes. A line break or tab inside longer
//!   inserted text (dictation, a keyboard suggestion, a third-party
//!   keyboard) is dropped: it neither submits a `text_input` nor breaks the
//!   line in a `text_editor`.
//! - **Android activity destruction** ends the application, which starts
//!   over in the next Activity ([Activity
//!   destruction](#android-activity-destruction)). The manifest settings
//!   above avoid the common causes but Back.
//! - **Android emulator without a GPU.** Its default headless GPU mode offers
//!   llvmpipe (lavapipe), which cannot run iced's wgpu shaders, so iced
//!   draws with tiny-skia on the CPU there. Boot the emulator with
//!   `-gpu swiftshader_indirect` to exercise wgpu.
//! - **Emoji** draw blank on both platforms: the system emoji fonts are in
//!   formats the text stack cannot draw (COLRv1 on Android, `emjc` images
//!   on iOS). Android's flags draw.
//! - **Chinese on iOS.** PingFang cannot be drawn either, so Han characters
//!   fall back to Hiragino Sans, which lacks many simplified Chinese
//!   characters (这, 们, ...); those show the missing-glyph box. Embed a font
//!   for Chinese text.
//! - **Dark mode and the system bars.** On Android a dark-mode switch while
//!   the app runs leaves the bars' icons as the launch theme had them, and
//!   on iOS the status bar follows the system's mode even when the app
//!   forces a theme ([Dark mode](#dark-mode)).
//! - **The keyboard's height on Android** arrives up to a quarter of a
//!   second after the keyboard when the app draws edge to edge: the shell
//!   reads it every 250 ms while a text input has the focus ([Safe
//!   area](#safe-area)).
//! - **Lifecycle messages come late.** [`lifecycle()`] delivers them a
//!   moment after the event, and when Android destroys the Activity the
//!   application can end before `Background` arrives: save in
//!   [`on_lifecycle`] instead ([Lifecycle](#lifecycle)).
//! - **`MemoryWarning` with GameActivity** comes each time the app goes to
//!   the background: GameActivity turns every `onTrimMemory` into one,
//!   without its level, so it cannot be told from a real shortage
//!   ([Lifecycle](#lifecycle)).

pub use crate::shell::{Lifecycle, LifecycleEvent, lifecycle, on_lifecycle};

/// The Android activity handle that android-activity gives `android_main`.
#[cfg(target_os = "android")]
#[cfg_attr(docsrs, doc(cfg(target_os = "android")))]
pub use crate::shell::winit::platform::android::activity::AndroidApp;

/// Hands the `AndroidApp` that `android_main` receives to the shell; it must
/// be called before the application runs, each time `android_main` runs.
/// [`android_main!`](crate::android_main) does it for you.
#[cfg(target_os = "android")]
#[cfg_attr(docsrs, doc(cfg(target_os = "android")))]
pub use crate::shell::set_android_app;

/// Whether the application that last ran on this thread ended because
/// Android destroyed its Activity: `android_main` must then return.
/// [`android_main!`](crate::android_main) acts on it for you.
#[cfg(target_os = "android")]
#[cfg_attr(docsrs, doc(cfg(target_os = "android")))]
pub use crate::shell::activity_destroyed;

pub use crate::shell::{SafeArea, safe_area};

/// The Android activity handle that android-activity gives `android_main`:
/// winit's `platform::android::activity::AndroidApp`, re-exported.
///
/// It exists only on Android; this stands in for it in documentation built
/// for other targets.
#[cfg(all(docsrs, not(target_os = "android")))]
#[doc(cfg(target_os = "android"))]
pub struct AndroidApp(());

/// Hands the `AndroidApp` that `android_main` receives to the shell; it must
/// be called before the application runs, each time `android_main` runs.
/// [`android_main!`](crate::android_main) does it for you.
///
/// It exists only on Android; this stands in for it in documentation built
/// for other targets.
#[cfg(all(docsrs, not(target_os = "android")))]
#[doc(cfg(target_os = "android"))]
pub fn set_android_app(_app: AndroidApp) {}

/// Whether the application that last ran on this thread ended because
/// Android destroyed its Activity: `android_main` must then return.
/// [`android_main!`](crate::android_main) acts on it for you.
///
/// It exists only on Android; this stands in for it in documentation built
/// for other targets.
#[cfg(all(docsrs, not(target_os = "android")))]
#[doc(cfg(target_os = "android"))]
pub fn activity_destroyed() -> bool {
    false
}

/// Installs the platform's logger for the `log` crate, so that iced's
/// messages and your own `log` calls can be read, and a panic hook that logs
/// every panic. Call it first thing; calling it again does nothing.
///
/// - Android: logcat, under the tag `iced`, each message prefixed with its
///   module (`adb logcat -s iced`).
/// - iOS: the unified log, under the subsystem `iced`, with one category per
///   module (`log stream --predicate 'subsystem == "iced"'`).
/// - Desktop (macOS, Linux, Windows, ...): stderr, one line per record:
///   `[2026-10-06T12:34:56.789Z INFO  my_app] message`, the time in UTC.
/// - Web: the browser's console, with the console method of each record's
///   level (`console.error`, `warn`, `info`; `debug` for `debug!` and
///   `trace!`, which the console shows at its "Verbose" level).
///
/// Records up to `Info` pass. On the desktop and the web, the shell's and
/// the wgpu renderer's own `Info` records (the window attributes, the
/// adapters and surface formats, more than a hundred lines at every start)
/// are left out, and their warnings and errors kept. To see more, or less,
/// before the application starts:
///
/// - Android: `adb shell setprop log.tag.iced DEBUG` (Android's own names,
///   `VERBOSE` to `ERROR`, are accepted too), or a `RUST_LOG` environment
///   variable holding a single level (`off`, `error`, `warn`, `info`,
///   `debug` or `trace`), which takes precedence.
/// - iOS: the `RUST_LOG` environment variable, holding a single level; on
///   the simulator, launch with `SIMCTL_CHILD_RUST_LOG=debug`. The unified
///   log hides `debug!` and `trace!` records unless asked: `log stream
///   --level info` shows `debug!`, and `--level debug` shows `trace!` as
///   well.
/// - Desktop: `RUST_LOG`, with `env_logger`'s directives: a level for every
///   target (`debug`), levels for the targets starting with a name
///   (`info,iced_wgpu=warn,my_app=trace`), or a name alone for every record
///   of those targets (`my_app`). With directives but no bare level, only
///   the targets they name are logged. `RUST_LOG=info` brings back the
///   start-up records.
/// - Web: the same directives in the page's `rust_log` query parameter
///   (`index.html?rust_log=debug`).
///
/// The level is read once, when the logger is installed, and is only the
/// starting point: the application can raise or lower it later with
/// `log::set_max_level`. On the desktop and the web, raising it above every
/// level the directives give lets every record up to it through. On
/// Android, a new Activity may run in the process of the last one, whose
/// logger and level it keeps.
///
/// The panic hook logs each panic, with its thread and location, through
/// `log::error!`, then runs the hook that was in place, which writes it to
/// stderr. A panic then reaches the same log as the rest, which matters on
/// iOS, where stderr is lost unless the app was launched from a console
/// (`simctl launch --console-pty`), and on the web, where stderr goes
/// nowhere. On the desktop, while the stderr logger is the one in place,
/// the hook does not log the panic, which the previous hook already prints
/// there.
///
/// When the run asked for `ICM_EVENT` lines (see the [module
/// documentation](self)), it also installs the hook that reports a panic as
/// one; the shell does so too when it starts.
///
/// If a logger is already installed, by an earlier call or by the
/// application, it stays in place, and the panic hook is still installed,
/// once. Without the `mobile-logger` feature (on by default) only the panic
/// hook is installed.
///
/// # A logger of your own
///
/// With `mobile-logger`, this installs the process's `log` logger on every
/// target, the desktop and the web included, and `log` takes one logger per
/// process. Install yours (`env_logger`, `tracing_subscriber`'s `fmt`,
/// `console_log`) **before** calling `init_logger`: iced's logger is then
/// skipped and the panic hook logs panics through yours. Installed after
/// it, yours fails, and the usual one-line initializers panic at startup:
/// `env_logger::init()` with "env_logger::init should not be called after
/// logger initialized", `tracing_subscriber::fmt::init()` with "Unable to
/// install global subscriber". On Android, where
/// [`android_main!`](crate::android_main) calls `init_logger` before your
/// function, write `iced::android_main!(run, logger = false)` so your
/// function can install its logger first. Your function then runs once per
/// Activity, and the next Activity of the process finds your logger in
/// place: install it with `try_init()` and ignore the error (or behind a
/// `std::sync::Once`), since the one-line `init()` forms panic the second
/// time, which ends the process. See [Activity
/// destruction](self#android-activity-destruction).
///
/// With your logger in place, the hook still logs each panic through
/// `log::error!` before the previous hook prints it to stderr, so on the
/// desktop a logger that writes to stderr shows a panic twice.
pub fn init_logger() {
    log_panics();

    crate::shell::icm::install_panic_hook();

    #[cfg(all(feature = "mobile-logger", target_os = "android"))]
    init_android_logger();

    #[cfg(all(feature = "mobile-logger", target_os = "ios"))]
    init_oslog();

    #[cfg(all(
        feature = "mobile-logger",
        not(any(
            target_os = "android",
            target_os = "ios",
            target_arch = "wasm32"
        ))
    ))]
    init_stderr_logger();

    #[cfg(all(feature = "mobile-logger", target_arch = "wasm32"))]
    init_console_logger();
}

#[cfg(any(
    test,
    all(
        feature = "mobile-logger",
        not(any(target_os = "android", target_os = "ios"))
    )
))]
mod logger;

/// Whether the stderr logger of [`init_logger`] is the one in place.
#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    target_arch = "wasm32"
)))]
static STDERR_LOGGER: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(all(
    feature = "mobile-logger",
    not(any(
        target_os = "android",
        target_os = "ios",
        target_arch = "wasm32"
    ))
))]
fn init_stderr_logger() {
    use std::sync::OnceLock;
    use std::sync::atomic;

    static LOGGER: OnceLock<logger::Stderr> = OnceLock::new();

    let logger = LOGGER.get_or_init(|| logger::Stderr {
        filter: logger::Filter::from_env(),
    });

    if log::set_logger(logger).is_ok() {
        log::set_max_level(logger.filter.max());
        STDERR_LOGGER.store(true, atomic::Ordering::Relaxed);
    }
}

#[cfg(all(feature = "mobile-logger", target_arch = "wasm32"))]
fn init_console_logger() {
    use std::sync::OnceLock;

    static LOGGER: OnceLock<logger::Console> = OnceLock::new();

    let logger = LOGGER.get_or_init(|| logger::Console {
        filter: logger::Filter::from_query(),
    });

    if log::set_logger(logger).is_ok() {
        log::set_max_level(logger.filter.max());
    }
}

#[cfg(all(feature = "mobile-logger", target_os = "android"))]
fn init_android_logger() {
    use std::sync::OnceLock;

    static LOGGER: OnceLock<android_logger::AndroidLogger> = OnceLock::new();

    // No level in the logger's own configuration: android_logger would keep
    // it as a filter of its own, and `log::set_max_level` could not raise it
    // later. The maximum level is set only once this logger is installed.
    let logger = LOGGER.get_or_init(|| {
        android_logger::AndroidLogger::new(
            android_logger::Config::default().with_tag("iced"),
        )
    });

    if log::set_logger(logger).is_ok() {
        log::set_max_level(max_level());
    }
}

#[cfg(all(feature = "mobile-logger", target_os = "ios"))]
fn init_oslog() {
    // `OsLogger::level_filter` would set the maximum level even when another
    // logger is already installed; only set it once this one is.
    if oslog::OsLogger::new("iced").init().is_ok() {
        log::set_max_level(max_level());
    }
}

/// The level `RUST_LOG` holds, then on Android the one `log.tag.iced` holds,
/// then `Info`.
#[cfg(all(
    feature = "mobile-logger",
    any(target_os = "android", target_os = "ios")
))]
fn max_level() -> log::LevelFilter {
    let from_env = std::env::var("RUST_LOG")
        .ok()
        .and_then(|level| level.trim().parse().ok());

    #[cfg(target_os = "android")]
    let from_env = from_env.or_else(|| {
        system_property(c"log.tag.iced").and_then(|level| android_level(&level))
    });

    from_env.unwrap_or(log::LevelFilter::Info)
}

/// A level as `setprop log.tag.<tag>` takes it (`VERBOSE`, `DEBUG`, `INFO`,
/// `WARN`, `ERROR`, `ASSERT` or `SUPPRESS`), or as the `log` crate names it.
#[cfg(any(all(feature = "mobile-logger", target_os = "android"), test))]
fn android_level(level: &str) -> Option<log::LevelFilter> {
    let level = level.trim();

    if level.eq_ignore_ascii_case("verbose") {
        Some(log::LevelFilter::Trace)
    } else if level.eq_ignore_ascii_case("assert")
        || level.eq_ignore_ascii_case("suppress")
    {
        Some(log::LevelFilter::Off)
    } else {
        level.parse().ok()
    }
}

/// The value of an Android system property, if it is set.
#[cfg(all(feature = "mobile-logger", target_os = "android"))]
fn system_property(name: &std::ffi::CStr) -> Option<String> {
    crate::shell::icm::system_property(name)
}

/// Logs every panic through `log::error!`, then runs the hook that was in
/// place. Installed once per process.
///
/// On the desktop the previous hook prints the panic to stderr, so it is not
/// logged while the stderr logger is the one in place: it would be printed
/// twice.
fn log_panics() {
    use std::sync::Once;

    static HOOK: Once = Once::new();

    HOOK.call_once(|| {
        let previous = std::panic::take_hook();

        std::panic::set_hook(Box::new(move |info| {
            #[cfg(not(any(
                target_os = "android",
                target_os = "ios",
                target_arch = "wasm32"
            )))]
            let log = !STDERR_LOGGER.load(std::sync::atomic::Ordering::Relaxed);

            #[cfg(any(
                target_os = "android",
                target_os = "ios",
                target_arch = "wasm32"
            ))]
            let log = true;

            if log {
                let thread = std::thread::current();
                let name = thread.name().unwrap_or("<unnamed>");

                log::error!("thread '{name}' {info}");
            }

            previous(info);
        }));
    });
}

/// Defines `android_main`, the function Android calls to start the
/// application, given the path of a function that runs it and returns an
/// [`iced::Result`](crate::Result).
///
/// On Android, the `android_main` it defines:
///
/// 1. installs the platform logger and a panic hook with
///    [`mobile::init_logger`]: iced's messages, yours and every panic, with
///    its location, go to logcat under the tag `iced` (`adb logcat -s
///    iced`), and panics to stderr (`RustStdoutStderr`) as well;
/// 2. hands its `AndroidApp` to the shell with `mobile::set_android_app`;
/// 3. calls your function, catching a panic on the thread that runs it, and
///    logs how it ended;
/// 4. returns if the Activity was destroyed, and otherwise ends the process
///    with `_exit`: status 0 when your function returned `Ok`, 1 when it
///    returned an error or panicked. `std::process::exit` would run the
///    process's exit handlers, which destroy what Android's renderer threads
///    still use, and those threads would abort with SIGABRT.
///
/// Step 3 catches only a panic on the thread of `android_main`, where your
/// function runs the application: its boot function, `update`, `view`, the
/// `subscription` function and the shell. A panic on any other thread (the
/// executor's threads, which run tasks and subscription streams, or a thread
/// the application spawned) is not caught there and does not end the
/// process. The panic hook logs it, and only that thread unwinds (with
/// `tokio`, only that task): the application goes on without what it was
/// doing, so a task's message never arrives, or a subscription sends no
/// more. Where such a panic must not pass unnoticed, catch it where it
/// happens (`iced::futures::FutureExt::catch_unwind` on the task's future)
/// and turn it into a message.
///
/// Android calls `android_main` once per Activity, on a thread of its own,
/// and may call it again in the same process when it starts a new Activity
/// (after Back, or for a configuration change the manifest does not list).
/// When Android destroys the Activity, the event loop ends, the application
/// is dropped, and your function returns `Ok`. `android_main` returns then,
/// as the Activity's `onDestroy` waits for it, and the next Activity's
/// `android_main` runs your function again: a new application, from its
/// boot function, in the same process. See [the module
/// documentation](crate::mobile#android-activity-destruction).
///
/// Otherwise your function returned while its Activity is still on screen:
/// the application could not go on (no usable graphics backend, for
/// example), since `iced::exit` and closing the last window are ignored on
/// Android, or your function did not run it. The process ends then, as it
/// does after a panic that step 3 caught, which may have left process-wide
/// state (locks, the font system) half-updated for the next Activity.
///
/// To install a logger of your own (another tag, a filter, a `tracing`
/// bridge), write `iced::android_main!(run, logger = false)`: step 1 then
/// installs only the panic hook, and your function installs the logger
/// before it runs the application. With the first form, iced's logger is in
/// place before your code runs, so installing another one fails.
///
/// Your function runs once per Activity, not once per process: anything it
/// sets up for the whole process, a logger first of all, must accept a
/// second call. Use `env_logger::try_init()` or `tracing_subscriber`'s
/// `try_init()` and ignore the error, or a `std::sync::Once`: their
/// `init()` panics when a logger is already set, and that panic ends the
/// process the second time an Activity runs your function.
///
/// On every other target it defines nothing, so the same line serves every
/// build, but the path is still checked: it must name a function taking
/// nothing and returning an [`iced::Result`](crate::Result), or the build
/// fails on the desktop too, not only on Android.
///
/// Call it once, at module level, in the library crate: Android loads the
/// library (built as a `cdylib`), not a binary. The manifest's
/// `android.app.lib_name` must be the library's name. The calling crate needs
/// no dependency other than `iced`.
///
/// # Example
/// ```no_run,standalone_crate
/// use iced::widget::{button, text};
/// use iced::Element;
///
/// pub fn run() -> iced::Result {
///     iced::run(update, view)
/// }
///
/// iced::android_main!(run);
///
/// fn update(count: &mut u32, _message: ()) {
///     *count += 1;
/// }
///
/// fn view(count: &u32) -> Element<'_, ()> {
///     button(text(count)).on_press(()).into()
/// }
/// ```
///
/// A wrong path fails everywhere:
/// ```compile_fail,standalone_crate
/// iced::android_main!(does_not_exist);
/// ```
///
/// [`mobile::init_logger`]: crate::mobile::init_logger
#[macro_export]
macro_rules! android_main {
    ($run:path $(,)?) => {
        $crate::android_main!($run, logger = true);
    };
    ($run:path, logger = $logger:literal $(,)?) => {
        #[cfg(target_os = "android")]
        #[unsafe(no_mangle)]
        fn android_main(app: $crate::mobile::AndroidApp) {
            $crate::mobile::__android_main(app, $run, $logger);
        }

        // Nothing runs it off Android, but the arguments are checked there
        // too, so a wrong path or signature fails on every target.
        #[cfg(not(target_os = "android"))]
        const _: (fn() -> $crate::Result, bool) = ($run, $logger);
    };
}

/// The body of the `android_main` that [`android_main!`](crate::android_main)
/// defines. Not public API.
#[doc(hidden)]
#[cfg(target_os = "android")]
pub fn __android_main(
    app: AndroidApp,
    run: fn() -> crate::Result,
    logger: bool,
) {
    if logger {
        init_logger();
    } else {
        log_panics();
    }

    set_android_app(app);

    // A panic of this thread, which runs the event loop. The hook has
    // logged it. It may have left process-wide state (a poisoned lock, a
    // half-updated font system) to the next Activity, so the process ends.
    // Other threads' panics never reach this: the hook logs them, and they
    // end only their own thread.
    let Ok(result) = std::panic::catch_unwind(run) else {
        log::error!("the application panicked; ending the process");
        end_process(1);
    };

    // The Activity's `onDestroy` waits for `android_main` to return, and the
    // next Activity runs `android_main` again, in this process.
    if activity_destroyed() {
        if let Err(error) = result {
            log::error!(
                "the application stopped with an error: {error} ({error:?})"
            );
        }

        log::info!(
            "the Activity was destroyed; android_main returns, and the next \
            Activity starts the application again"
        );

        return;
    }

    // The Activity is still on screen. android-activity would finish it once
    // `android_main` returns, but with android-activity 0.6.0 its `onPause`
    // then waits for this thread forever, and the app stops responding.
    let code = match result {
        Ok(()) => {
            log::info!("the application stopped; ending the process");
            0
        }
        Err(error) => {
            log::error!(
                "the application stopped with an error: {error} ({error:?}); \
                ending the process"
            );
            1
        }
    };

    end_process(code)
}

/// Ends the process with `status`, skipping the exit handlers that
/// `std::process::exit` runs.
///
/// Those handlers include the C++ static destructors, which destroy a mutex
/// of Android's renderer (libhwui) while its `hwuiTask` threads still lock
/// it. The threads then abort ("FORTIFY: pthread_mutex_lock called on a
/// destroyed mutex", then SIGABRT), and the process is recorded as a native
/// crash or as an exit, whichever comes first.
#[cfg(target_os = "android")]
#[allow(unsafe_code)]
fn end_process(status: i32) -> ! {
    use std::io::Write;

    // bionic's <unistd.h>.
    unsafe extern "C" {
        fn _exit(status: std::ffi::c_int) -> !;
    }

    // `std::process::exit` would flush it; stderr is unbuffered.
    let _ = std::io::stdout().flush();

    // SAFETY: `_exit` takes any status, and ends the process at once.
    unsafe { _exit(status) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_levels_take_android_and_log_names() {
        use log::LevelFilter;

        for (name, level) in [
            ("VERBOSE", LevelFilter::Trace),
            ("DEBUG", LevelFilter::Debug),
            ("INFO", LevelFilter::Info),
            ("WARN", LevelFilter::Warn),
            ("ERROR", LevelFilter::Error),
            ("ASSERT", LevelFilter::Off),
            ("SUPPRESS", LevelFilter::Off),
            ("trace", LevelFilter::Trace),
            ("debug", LevelFilter::Debug),
            (" info\n", LevelFilter::Info),
            ("off", LevelFilter::Off),
        ] {
            assert_eq!(android_level(name), Some(level), "{name:?}");
        }

        assert_eq!(android_level(""), None);
        assert_eq!(android_level("loud"), None);
    }
}
