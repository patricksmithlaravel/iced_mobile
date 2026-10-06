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
//! for you), iced's messages, your `log` calls and every panic go to:
//!
//! - Android: `adb logcat -s iced` (stdout and stderr are under
//!   `RustStdoutStderr`). For `debug!` records, run `adb shell setprop
//!   log.tag.iced DEBUG` and start the app again (`VERBOSE` for `trace!`).
//! - iOS: `xcrun simctl spawn booted log stream --level info --predicate
//!   'subsystem == "iced"'`. Records up to `info!` pass; launch with
//!   `SIMCTL_CHILD_RUST_LOG=debug` for `debug!`, which the unified log shows
//!   only with `--level info` or `--level debug` (`trace!` needs
//!   `--level debug`).
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
/// messages and your own `log` calls can be read, and a panic hook that logs
/// every panic.
///
/// - Android: logcat, under the tag `iced`, each message prefixed with its
///   module (`adb logcat -s iced`).
/// - iOS: the unified log, under the subsystem `iced`, with one category per
///   module (`log stream --predicate 'subsystem == "iced"'`).
/// - Other targets: nothing. Install the logger of your choice as usual.
///
/// Records up to `Info` pass. To see more, set a single level (`off`,
/// `error`, `warn`, `info`, `debug` or `trace`) before the application
/// starts:
///
/// - Android: `adb shell setprop log.tag.iced DEBUG` (Android's own names,
///   `VERBOSE` to `ERROR`, are accepted too), or a `RUST_LOG` environment
///   variable, which takes precedence.
/// - iOS: the `RUST_LOG` environment variable; on the simulator, launch with
///   `SIMCTL_CHILD_RUST_LOG=debug`. The unified log hides `debug!` and
///   `trace!` records unless asked: `log stream --level info` shows
///   `debug!`, and `--level debug` shows `trace!` as well.
///
/// The level is only the starting point: the application can raise or lower
/// it later with `log::set_max_level`.
///
/// On Android and iOS, the panic hook logs each panic, with its thread and
/// location, through `log::error!`, then runs the hook that was in place,
/// which writes it to stderr. A panic then reaches the same log as the rest,
/// which matters on iOS, where stderr is lost unless the app was launched
/// from a console (`simctl launch --console-pty`).
///
/// If a logger is already installed, by an earlier call or by the
/// application, it stays in place, and the panic hook is still installed,
/// once. Without the `mobile-logger` feature (on by default) only the panic
/// hook is installed.
pub fn init_logger() {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    log_panics();

    #[cfg(all(feature = "mobile-logger", target_os = "android"))]
    init_android_logger();

    #[cfg(all(feature = "mobile-logger", target_os = "ios"))]
    init_oslog();
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
#[allow(unsafe_code)]
fn system_property(name: &std::ffi::CStr) -> Option<String> {
    use std::ffi::{CStr, c_char, c_int};

    // bionic's <sys/system_properties.h>.
    unsafe extern "C" {
        fn __system_property_get(
            name: *const c_char,
            value: *mut c_char,
        ) -> c_int;
    }

    // PROP_VALUE_MAX: a value and its terminating NUL fit in 92 bytes.
    let mut value = [0 as c_char; 92];

    // SAFETY: `name` is NUL-terminated, and `value` has the PROP_VALUE_MAX
    // bytes bionic writes at most, NUL included.
    let length =
        unsafe { __system_property_get(name.as_ptr(), value.as_mut_ptr()) };

    if length <= 0 {
        return None;
    }

    // SAFETY: bionic NUL-terminated what it wrote.
    let value = unsafe { CStr::from_ptr(value.as_ptr()) };

    Some(value.to_string_lossy().into_owned())
}

/// Logs every panic through `log::error!`, then runs the hook that was in
/// place. Installed once per process.
#[cfg(any(target_os = "android", target_os = "ios"))]
fn log_panics() {
    use std::sync::Once;

    static HOOK: Once = Once::new();

    HOOK.call_once(|| {
        let previous = std::panic::take_hook();

        std::panic::set_hook(Box::new(move |info| {
            let thread = std::thread::current();
            let name = thread.name().unwrap_or("<unnamed>");

            log::error!("thread '{name}' {info}");

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
/// 4. ends the process with `std::process::exit`: status 0 when your
///    function returned `Ok`, 1 when it returned an error or panicked.
///
/// Step 4 is needed because winit allows one event loop per process, and
/// Android usually keeps the process alive once `android_main` returns: the
/// next launch would run `android_main` again in it and fail to create the
/// event loop, launch after launch, until the process is killed. On Android
/// your function returns only when the application cannot go on (no usable
/// graphics backend, for example), since `iced::exit` and closing the last
/// window are ignored there.
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
) -> ! {
    if logger {
        init_logger();
    } else {
        log_panics();
    }

    set_android_app(app);

    // winit allows one event loop per process, and Android usually keeps the
    // process alive after `android_main` returns: the next launch would run
    // `android_main` again in it and fail to build an event loop
    // (RecreationAttempt), launch after launch. Ending the process gives the
    // next launch a fresh one. A panic is caught for the same reason; the
    // hook has logged it.
    let code = match std::panic::catch_unwind(run) {
        Ok(Ok(())) => {
            log::info!("the application stopped; ending the process");
            0
        }
        Ok(Err(error)) => {
            log::error!(
                "the application stopped with an error: {error} ({error:?}); \
                ending the process"
            );
            1
        }
        Err(_panic) => {
            log::error!("the application panicked; ending the process");
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
