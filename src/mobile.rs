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
//! [`on_lifecycle`] runs a hook on the event loop's thread whenever winit
//! reports the application suspended or resumed, before iced acts on it.
//! [`Lifecycle::Suspended`] means different things per platform: on iOS the
//! application is about to stop being active, which also happens for
//! Control Center, notifications and Face ID; on Android its window is going
//! away. On the web it fires when the page goes into the back-forward cache.
//! The desktop never sends it. [`Lifecycle`] has the full table.
//!
//! # Android: Activity destruction
//!
//! Android destroys the Activity on Back (see [Android](#android)), for a
//! configuration change the manifest does not list, with the developer
//! option "Don't keep activities", or to reclaim memory while the app is
//! in the background. The process often lives on, and the next Activity
//! runs in it. iced follows the Activity:
//!
//! 1. [`Lifecycle::Suspended`] has come first, when the window went away
//!    (just before, or when the app left the screen).
//! 2. The event loop ends, and the application is dropped: its state, its
//!    windows, its renderer and its executor, with the futures and
//!    subscriptions still running on it. The function that runs it returns
//!    `Ok(())`, and `mobile::activity_destroyed` says why.
//! 3. `android_main` returns, as the Activity's `onDestroy` waits for it.
//! 4. The next Activity calls `android_main` again, on a thread of its own,
//!    and a new application starts, from its boot function.
//!
//! Whatever the application keeps in memory is lost: save what must outlive
//! the Activity on [`Lifecycle::Suspended`], which always comes before.
//! What belongs to the process stays: the logger, the panic hook, the
//! [`on_lifecycle`] hook (setting the same one again does nothing), the
//! fonts loaded so far, and the application's own statics. A static that
//! keeps an `AndroidApp` (for JNI, say) must take each Activity's new one:
//! a `OnceLock` would keep the first, whose Activity is gone.
//!
//! [`android_main!`](crate::android_main) does steps 3 and 4 for you. The
//! process ends instead after a panic, and when the application stops on
//! its own while its Activity is still on screen.
//!
//! Launching the app again a moment after Back, before Android has destroyed
//! the Activity it finished, starts a second Activity while the first one
//! still runs. android-activity 0.6.0 aborts the process then; with 0.6.1,
//! iced cannot build the second event loop and the process ends. Android
//! then starts the new Activity in a new process, or the next launch does.
//!
//! # winit
//!
//! iced takes winit 0.30.13 from a fork,
//! [patricksmithlaravel/winit](https://github.com/patricksmithlaravel/winit),
//! branch `iced-mobile/0.30`, which carries Android fixes until they ship in
//! a winit release (winit PR #4739): the event loop ends when the Activity
//! is destroyed, a new one can be built in the same process, and content
//! rect changes are reported (as `Resized`).
//!
//! An application must not depend on winit from crates.io: the build would
//! hold two copies of winit, and iced would use the fork alone. The two
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
//! fork's git URL.
//!
//! # Known limitations
//!
//! - **Exiting.** `iced::exit` and closing the last window are ignored on
//!   Android and iOS, with a warning in the log: the system ends a mobile
//!   app. On Android, finish the Activity through JNI (`Activity.finish`)
//!   to leave: the application ends as when Android destroys it. On iOS a
//!   window being opened can replace the last one: open the new window
//!   before closing the old one.
//! - **One window on Android.** Android gives an app one native window, so
//!   a second `window::open` is refused with an error in the log, and its
//!   task ends without an id.
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
//! - **Not yet available on mobile:** safe-area insets (pad the root view),
//!   the clipboard, and detecting dark mode.

pub use crate::shell::{Lifecycle, on_lifecycle};

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
/// function can install its logger first.
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
/// 3. calls your function, catching a panic, and logs how it ended;
/// 4. returns if the Activity was destroyed, and otherwise ends the process
///    with `std::process::exit`: status 0 when your function returned `Ok`,
///    1 when it returned an error or panicked.
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
/// does after a panic, which may have left process-wide state (locks, the
/// font system) half-updated for the next Activity.
///
/// To install a logger of your own (another tag, a filter, a `tracing`
/// bridge), write `iced::android_main!(run, logger = false)`: step 1 then
/// installs only the panic hook, and your function installs the logger
/// before it runs the application. With the first form, iced's logger is in
/// place before your code runs, so installing another one fails.
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

    // The hook has logged the panic. It may have left process-wide state
    // (a poisoned lock, a half-updated font system) to the next Activity,
    // so the process ends.
    let Ok(result) = std::panic::catch_unwind(run) else {
        log::error!("the application panicked; ending the process");
        std::process::exit(1);
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

    std::process::exit(code)
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
