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
//! Depend on `iced` alone. Its default features select Android's
//! `NativeActivity` (`android-native-activity`) and a platform logger
//! (`mobile-logger`); both are inert on other targets. Everything a mobile
//! entry point needs from the shell is re-exported here. A crate that also
//! depends on `iced_winit` directly must take it from the same source as
//! `iced`, character for character, or the build holds two copies of the
//! shell and `set_android_app` fills the one `iced` does not use.
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
//! [lib]
//! # cdylib: Android loads libmyapp.so; rlib: the binary links the library.
//! crate-type = ["cdylib", "rlib"]
//!
//! [[bin]]
//! # The iOS executable, and the desktop one.
//! name = "myapp"
//! path = "src/main.rs"
//!
//! [dependencies]
//! iced = { git = "https://github.com/patricksmithlaravel/iced_mobile", rev = "<full commit hash>" }
//! ```
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
//! // Android's entry point. Expands to nothing on other targets.
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
//!     iced::mobile::init_logger(); // os_log on iOS; nothing on the desktop
//!     myapp::run()
//! }
//! ```
//!
//! # Android
//!
//! - The manifest's `android.app.lib_name` meta-data must be the `[lib]`
//!   name (`myapp` for `libmyapp.so`), or Android finds no `android_main`.
//! - Give the activity the full `android:configChanges` list (orientation,
//!   screen size, `uiMode`, locale, font scale and the rest). Without it
//!   Android recreates the activity on rotation or a dark-mode switch, and
//!   winit 0.30 cannot start a second event loop in the same process.
//! - `cargo check --target aarch64-linux-android` needs no NDK with the
//!   default `NativeActivity`; `cargo build` needs the NDK's linker.
//! - For a GameActivity, turn on `android-game-activity` and set
//!   `default-features = false` on every `iced` dependency in the build (see
//!   the feature's comment in iced's `Cargo.toml`).
//!
//! # iOS
//!
//! - Apps built with the iOS 27 SDK must declare a
//!   `UIApplicationSceneManifest` in their `Info.plist`, or UIKit refuses to
//!   launch them. The snippet is in the documentation of
//!   `winit/src/scene.rs` in this repository.
//!
//! # Logs
//!
//! With [`init_logger`] (which [`android_main!`](crate::android_main) calls
//! for you):
//!
//! - Android: `adb logcat -s iced` (stdout and stderr are under
//!   `RustStdoutStderr`).
//! - iOS: `xcrun simctl spawn booted log stream --predicate 'subsystem ==
//!   "iced"'`.
//!
//! # Lifecycle
//!
//! [`on_lifecycle`] runs a hook on the event loop's thread whenever winit
//! reports the application suspended or resumed, before iced acts on it.
//! [`Lifecycle::Suspended`] means different things per platform: on iOS the
//! application is about to stop being active, which also happens for
//! Control Center, notifications and Face ID; on Android its window is going
//! away. The desktop never sends it.

pub use crate::shell::{Lifecycle, on_lifecycle};

/// The Android activity handle that android-activity gives `android_main`.
#[cfg(target_os = "android")]
#[cfg_attr(docsrs, doc(cfg(target_os = "android")))]
pub use crate::shell::winit::platform::android::activity::AndroidApp;

#[cfg(target_os = "android")]
#[cfg_attr(docsrs, doc(cfg(target_os = "android")))]
pub use crate::shell::set_android_app;

/// Installs the platform's logger for the `log` crate, so that iced's
/// messages and your own `log` calls can be read.
///
/// - Android: logcat, under the tag `iced`, each message prefixed with its
///   module (`adb logcat -s iced`).
/// - iOS: the unified log, under the subsystem `iced`, with one category per
///   module (`log stream --predicate 'subsystem == "iced"'`).
/// - Other targets: nothing. Install the logger of your choice as usual.
///
/// Records up to `Info` pass, unless the `RUST_LOG` environment variable
/// holds a single level: `off`, `error`, `warn`, `info`, `debug` or `trace`.
/// On the iOS simulator, launch with `SIMCTL_CHILD_RUST_LOG=debug` to set it.
///
/// If a logger is already installed, by an earlier call or by the
/// application, it stays in place. Without the `mobile-logger` feature (on
/// by default) this does nothing.
pub fn init_logger() {
    #[cfg(all(feature = "mobile-logger", target_os = "android"))]
    android_logger::init_once(
        android_logger::Config::default()
            .with_tag("iced")
            .with_max_level(max_level()),
    );

    #[cfg(all(feature = "mobile-logger", target_os = "ios"))]
    init_oslog();
}

#[cfg(all(feature = "mobile-logger", target_os = "ios"))]
fn init_oslog() {
    // `OsLogger::level_filter` would set the maximum level even when another
    // logger is already installed; only set it once this one is.
    if oslog::OsLogger::new("iced").init().is_ok() {
        log::set_max_level(max_level());
    }
}

#[cfg(all(
    feature = "mobile-logger",
    any(target_os = "android", target_os = "ios")
))]
fn max_level() -> log::LevelFilter {
    std::env::var("RUST_LOG")
        .ok()
        .and_then(|level| level.trim().parse().ok())
        .unwrap_or(log::LevelFilter::Info)
}

/// Defines `android_main`, the function Android calls to start the
/// application, given the path of a function that runs it and returns an
/// [`iced::Result`](crate::Result).
///
/// On Android, the `android_main` it defines:
///
/// 1. installs the platform logger with [`mobile::init_logger`];
/// 2. installs a panic hook that logs every panic, with its location, through
///    `log::error!` (`adb logcat -s iced`), then runs the previous hook,
///    which writes it to stderr (`RustStdoutStderr`);
/// 3. hands its `AndroidApp` to the shell with `mobile::set_android_app`;
/// 4. calls your function, and logs the error it returns, if any.
///
/// On every other target it expands to nothing, so the same line serves
/// every build.
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
/// [`mobile::init_logger`]: crate::mobile::init_logger
#[macro_export]
macro_rules! android_main {
    ($run:path $(,)?) => {
        #[cfg(target_os = "android")]
        #[unsafe(no_mangle)]
        fn android_main(app: $crate::mobile::AndroidApp) {
            $crate::mobile::__android_main(app, $run);
        }
    };
}

/// The body of the `android_main` that [`android_main!`](crate::android_main)
/// defines. Not public API.
#[doc(hidden)]
#[cfg(target_os = "android")]
pub fn __android_main(app: AndroidApp, run: fn() -> crate::Result) {
    use std::sync::Once;

    // `android_main` runs again for each new Activity in the same process;
    // the hook is installed once.
    static PANIC_HOOK: Once = Once::new();

    init_logger();

    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();

        std::panic::set_hook(Box::new(move |info| {
            let thread = std::thread::current();
            let name = thread.name().unwrap_or("<unnamed>");

            log::error!("thread '{name}' {info}");

            previous(info);
        }));
    });

    set_android_app(app);

    if let Err(error) = run() {
        log::error!(
            "the application stopped with an error: {error} ({error:?})"
        );
    }
}
