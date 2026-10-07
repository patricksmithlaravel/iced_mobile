//! Android and iOS: the system's light or dark mode.
//!
//! winit reports no system or window theme on either platform and never
//! sends `ThemeChanged`. The runner reads the mode where the platform keeps
//! it whenever the event loop turns (`NewEvents` and `AboutToWait`), and
//! hands a change to the instance, which treats it as Linux's theme stream
//! does (`system::Action::NotifyTheme`): the default theme, `system::theme`
//! and `system::theme_changes` follow it.
//!
//! A switch wakes the event loop on both platforms, so the next turn sees
//! it:
//!
//! - Android: the night bits of the `uiMode` of the Activity's resources'
//!   configuration (`getResources().getConfiguration()`), read through JNI.
//!   The manifest lists `uiMode` in `configChanges`, so a switch keeps the
//!   Activity and arrives as `onConfigurationChanged`, which Android calls
//!   once it has updated those resources, and which wakes the event loop.
//!   android-activity's own copy of the configuration
//!   (`AndroidApp::config`) cannot be used after launch: it is read again
//!   from the AssetManager the Activity had when it started, which Android
//!   no longer updates for a change the Activity handles (on API 36, after
//!   `cmd uimode night no` it still says night). It is the fallback, should
//!   the JNI read fail.
//! - iOS: the user interface style of the main screen's traits, which UIKit
//!   updates as it applies the change. A window's override does not change
//!   them, so the system's mode is read even when the application forces
//!   its own.

use crate::core::theme;

/// Remembers the last mode read, to notice a change.
#[derive(Debug, Default)]
pub(crate) struct Appearance {
    last: Option<theme::Mode>,
}

impl Appearance {
    /// The system's mode, when it differs from the last one read (the first
    /// read always reports).
    #[cfg(any(target_os = "android", target_os = "ios"))]
    pub(crate) fn poll(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
    ) -> Option<theme::Mode> {
        self.update(system_mode(event_loop))
    }

    /// `mode`, when it differs from the last one seen.
    fn update(&mut self, mode: theme::Mode) -> Option<theme::Mode> {
        if self.last == Some(mode) {
            return None;
        }

        self.last = Some(mode);

        Some(mode)
    }
}

/// The system's light or dark mode, read where the platform keeps it.
#[cfg(target_os = "android")]
fn system_mode(event_loop: &winit::event_loop::ActiveEventLoop) -> theme::Mode {
    use std::sync::Once;
    use winit::platform::android::ActiveEventLoopExtAndroid;

    static WARNING: Once = Once::new();

    match android::ui_mode() {
        Ok(ui_mode) => night_mode(ui_mode),
        Err(error) => {
            WARNING.call_once(|| {
                log::warn!(
                    "Reading the Activity's uiMode failed: {error}; the \
                    system theme is the one it had at launch"
                );
            });

            // `AConfiguration`'s night values are `Configuration`'s, four
            // bits lower.
            let night: i32 =
                event_loop.android_app().config().ui_mode_night().into();

            night_mode(night << 4)
        }
    }
}

/// The system's light or dark mode, read where the platform keeps it.
#[cfg(target_os = "ios")]
fn system_mode(
    _event_loop: &winit::event_loop::ActiveEventLoop,
) -> theme::Mode {
    ios::style().map_or(theme::Mode::None, style_mode)
}

/// Android: the mode of a `Configuration.uiMode`.
#[cfg(any(target_os = "android", test))]
fn night_mode(ui_mode: i32) -> theme::Mode {
    // UI_MODE_NIGHT_MASK, UI_MODE_NIGHT_NO and UI_MODE_NIGHT_YES;
    // UI_MODE_NIGHT_UNDEFINED (0) when not set.
    match ui_mode & 0x30 {
        0x10 => theme::Mode::Light,
        0x20 => theme::Mode::Dark,
        _ => theme::Mode::None,
    }
}

/// iOS: the mode of a `UIUserInterfaceStyle`.
#[cfg(any(target_os = "ios", test))]
fn style_mode(style: isize) -> theme::Mode {
    // UIUserInterfaceStyleLight and Dark; Unspecified (0) otherwise.
    match style {
        1 => theme::Mode::Light,
        2 => theme::Mode::Dark,
        _ => theme::Mode::None,
    }
}

#[cfg(target_os = "android")]
mod android {
    // The JVM is reached through JNI; each unsafe call says why it is sound.
    #![allow(unsafe_code)]

    use jni::objects::JObject;
    use jni::vm::JavaVM;
    use jni::{Env, jni_sig, jni_str};

    /// `getResources().getConfiguration().uiMode` of the Activity.
    ///
    /// It runs on the thread of `android_main`, which android-activity
    /// attaches to the JVM for good. The UI thread replaces what this reads
    /// before it calls `onConfigurationChanged`, and that call is what wakes
    /// the event loop, so the read after a switch sees the new mode.
    pub(super) fn ui_mode() -> jni::errors::Result<i32> {
        with_activity(|env, activity| {
            let resources = env
                .call_method(
                    activity,
                    jni_str!("getResources"),
                    jni_sig!(() -> android.content.res.Resources),
                    &[],
                )?
                .l()?;

            let configuration = env
                .call_method(
                    &resources,
                    jni_str!("getConfiguration"),
                    jni_sig!(() -> android.content.res.Configuration),
                    &[],
                )?
                .l()?;

            env.get_field(&configuration, jni_str!("uiMode"), jni_sig!(jint))?
                .i()
        })
    }

    /// Runs `f` with the thread's JNI environment and the Activity, in a
    /// local frame of its own, so no local reference outlives the call.
    fn with_activity<T>(
        f: impl FnOnce(&mut Env<'_>, &JObject<'_>) -> jni::errors::Result<T>,
    ) -> jni::errors::Result<T> {
        let context = ndk_context::android_context();

        // SAFETY: android-activity gives `ndk-context` the process's JavaVM
        // before `android_main` starts, and keeps it while the shell runs.
        let vm = unsafe { JavaVM::from_raw(context.vm().cast()) };
        let activity = context.context() as jni::sys::jobject;

        vm.attach_current_thread(|env| {
            // SAFETY: the NativeActivity's global reference, which
            // android-activity deletes only after `android_main` returns.
            // The cast borrows it: it is never wrapped in an owning
            // reference, and never deleted here.
            let activity =
                unsafe { env.as_cast_raw::<JObject<'_>>(&activity)? };

            f(env, &activity)
        })
    }
}

#[cfg(target_os = "ios")]
mod ios {
    // UIKit is reached through Objective-C calls; each says why it is sound.
    #![allow(unsafe_code)]

    use objc2_foundation::MainThreadMarker;
    use objc2_ui_kit::{UIScreen, UITraitEnvironment};

    /// The `UIUserInterfaceStyle` of the main screen's traits, or `None`
    /// off the main thread (the shell runs on it).
    pub(super) fn style() -> Option<isize> {
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("The system theme is read on the main thread only");
            return None;
        };

        // An iPhone has one screen. Its replacement, a window scene's
        // screen, needs a window, and the mode is read before there is one.
        #[allow(deprecated)]
        let screen = UIScreen::mainScreen(mtm);

        // SAFETY: a trait collection's style is a plain property read, made
        // on the main thread, where UIKit updates it.
        let style = unsafe { screen.traitCollection().userInterfaceStyle() };

        Some(style.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_android_ui_modes() {
        // UI_MODE_TYPE_NORMAL (0x01) with each night value.
        assert_eq!(night_mode(0x11), theme::Mode::Light);
        assert_eq!(night_mode(0x21), theme::Mode::Dark);
        assert_eq!(night_mode(0x01), theme::Mode::None);
        assert_eq!(night_mode(0x31), theme::Mode::None);
        // Another type (UI_MODE_TYPE_CAR) does not matter.
        assert_eq!(night_mode(0x23), theme::Mode::Dark);
        // android-activity's fallback, AConfiguration's values shifted.
        assert_eq!(night_mode(1 << 4), theme::Mode::Light);
        assert_eq!(night_mode(2 << 4), theme::Mode::Dark);
    }

    #[test]
    fn maps_ios_styles() {
        assert_eq!(style_mode(1), theme::Mode::Light);
        assert_eq!(style_mode(2), theme::Mode::Dark);
        assert_eq!(style_mode(0), theme::Mode::None);
        assert_eq!(style_mode(-1), theme::Mode::None);
    }

    #[test]
    fn reports_the_first_mode_and_each_change() {
        let mut appearance = Appearance::default();

        assert_eq!(
            appearance.update(theme::Mode::Light),
            Some(theme::Mode::Light)
        );
        assert_eq!(appearance.update(theme::Mode::Light), None);
        assert_eq!(
            appearance.update(theme::Mode::Dark),
            Some(theme::Mode::Dark)
        );
        assert_eq!(appearance.update(theme::Mode::Dark), None);
        assert_eq!(
            appearance.update(theme::Mode::None),
            Some(theme::Mode::None)
        );

        // An unknown first read reports too: `system::theme` answers it.
        let mut appearance = Appearance::default();

        assert_eq!(
            appearance.update(theme::Mode::None),
            Some(theme::Mode::None)
        );
    }
}
