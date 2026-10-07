//! Android: the safe area from the root view's `WindowInsets`, read through
//! JNI on `android_main`'s thread, and the content rect `NativeActivity`
//! reports.
//!
//! `View.getRootWindowInsets` returns the insets the view root computed
//! last, an immutable object, and checks no thread. It is read when an event
//! announces a change and while a poll is due, never per frame. Until it can
//! be read (no view yet, a Java exception), the content rect stands in for
//! the bars.

// JNI is reached only through `jni`'s calls; each unsafe one says why it is
// sound.
#![allow(unsafe_code)]

use super::{Physical, RootInsets};

use jni::objects::JObject;
use jni::{Env, JavaVM, jni_sig, jni_str};
use winit::platform::android::WindowExtAndroid;
use winit::platform::android::activity::AndroidApp;

use std::cell::RefCell;
use std::sync::atomic::{self, AtomicBool};

thread_local! {
    /// The `AndroidApp` of the application running on this thread, for its
    /// Activity: the shell runs on `android_main`'s thread.
    static APP: RefCell<Option<AndroidApp>> = const { RefCell::new(None) };
}

/// Keeps `app` for the JNI calls of the application that runs on this
/// thread, replacing the last Activity's.
pub(super) fn set_app(app: AndroidApp) {
    APP.set(Some(app));
}

/// Lets go of the `AndroidApp`, as the application ends: its Activity is
/// not kept until the thread ends.
pub(super) fn forget_app() {
    drop(APP.take());
}

/// The first SDK with `WindowInsets.Type` and `WindowInsets.getInsets`.
const TYPES: i32 = 30;

/// The first SDK with `WindowInsets.getDisplayCutout`.
const CUTOUT: i32 = 28;

/// The safe area of `window` in physical pixels, or `None` while it cannot
/// be known (no native window, or nothing laid out yet).
pub(super) fn read(window: &winit::window::Window) -> Option<Physical> {
    let size = window.inner_size();
    let content = window.content_rect();
    let sdk = window.config().sdk_version();

    let root = match root_insets(sdk) {
        Ok(root) => root,
        Err(error) => {
            // Once per process at the warning level: polls read again.
            static WARNED: AtomicBool = AtomicBool::new(false);

            if WARNED.swap(true, atomic::Ordering::Relaxed) {
                log::debug!("Safe area: the root insets: {error}");
            } else {
                log::warn!(
                    "Safe area: the root view's insets cannot be read \
                    ({error}); the content rect stands in for them"
                );
            }

            None
        }
    };

    super::android_area(
        size.width,
        size.height,
        [content.left, content.top, content.right, content.bottom],
        root,
    )
}

/// The insets of the Activity's root view, or `None` before the view is
/// attached to its window.
fn root_insets(sdk: i32) -> jni::errors::Result<Option<RootInsets>> {
    with_activity(|env, activity| {
        let window = env
            .call_method(
                activity,
                jni_str!("getWindow"),
                jni_sig!("()Landroid/view/Window;"),
                &[],
            )?
            .l()?;

        if window.is_null() {
            return Ok(None);
        }

        let decor = env
            .call_method(
                &window,
                jni_str!("getDecorView"),
                jni_sig!("()Landroid/view/View;"),
                &[],
            )?
            .l()?;

        if decor.is_null() {
            return Ok(None);
        }

        let insets = env
            .call_method(
                &decor,
                jni_str!("getRootWindowInsets"),
                jni_sig!("()Landroid/view/WindowInsets;"),
                &[],
            )?
            .l()?;

        if insets.is_null() {
            return Ok(None);
        }

        if sdk >= TYPES {
            let bars = type_mask(env, jni_str!("systemBars"))?
                | type_mask(env, jni_str!("displayCutout"))?;
            let ime = type_mask(env, jni_str!("ime"))?;

            Ok(Some(RootInsets {
                bars: insets_of(env, &insets, bars)?,
                keyboard: insets_of(env, &insets, ime)?[2],
            }))
        } else {
            let mut bars = [
                int(env, &insets, jni_str!("getStableInsetTop"))?,
                int(env, &insets, jni_str!("getStableInsetRight"))?,
                int(env, &insets, jni_str!("getStableInsetBottom"))?,
                int(env, &insets, jni_str!("getStableInsetLeft"))?,
            ];

            if sdk >= CUTOUT {
                let cutout = env
                    .call_method(
                        &insets,
                        jni_str!("getDisplayCutout"),
                        jni_sig!("()Landroid/view/DisplayCutout;"),
                        &[],
                    )?
                    .l()?;

                if !cutout.is_null() {
                    let safe = [
                        int(env, &cutout, jni_str!("getSafeInsetTop"))?,
                        int(env, &cutout, jni_str!("getSafeInsetRight"))?,
                        int(env, &cutout, jni_str!("getSafeInsetBottom"))?,
                        int(env, &cutout, jni_str!("getSafeInsetLeft"))?,
                    ];

                    for (bar, safe) in bars.iter_mut().zip(safe) {
                        *bar = (*bar).max(safe);
                    }
                }
            }

            // `NativeActivity` asks for `SOFT_INPUT_ADJUST_RESIZE`, so the
            // system-window insets include the keyboard, from the bottom
            // edge; the stable ones never do.
            let system =
                int(env, &insets, jni_str!("getSystemWindowInsetBottom"))?;

            Ok(Some(RootInsets {
                bars,
                keyboard: if system > bars[2] { system } else { 0 },
            }))
        }
    })
}

/// A `WindowInsets.Type` mask, by the name of its static method.
fn type_mask(
    env: &mut Env<'_>,
    name: &jni::strings::JNIStr,
) -> jni::errors::Result<i32> {
    env.call_static_method(
        jni_str!("android/view/WindowInsets$Type"),
        name,
        jni_sig!("()I"),
        &[],
    )?
    .i()
}

/// `insets.getInsets(mask)`: top, right, bottom and left.
fn insets_of(
    env: &mut Env<'_>,
    insets: &JObject<'_>,
    mask: i32,
) -> jni::errors::Result<[i32; 4]> {
    let edges = env
        .call_method(
            insets,
            jni_str!("getInsets"),
            jni_sig!("(I)Landroid/graphics/Insets;"),
            &[jni::JValue::Int(mask)],
        )?
        .l()?;

    let mut field = |name| {
        env.get_field(&edges, name, jni_sig!("I"))
            .and_then(jni::objects::JValueOwned::i)
    };

    Ok([
        field(jni_str!("top"))?,
        field(jni_str!("right"))?,
        field(jni_str!("bottom"))?,
        field(jni_str!("left"))?,
    ])
}

/// Calls the `int` method `name` that takes nothing.
fn int(
    env: &mut Env<'_>,
    object: &JObject<'_>,
    name: &jni::strings::JNIStr,
) -> jni::errors::Result<i32> {
    env.call_method(object, name, jni_sig!("()I"), &[])?.i()
}

/// Runs `f` with the Activity, on this thread attached to the JVM.
///
/// The Activity comes from the `AndroidApp`, not from `ndk-context`, whose
/// context is the `Application` (seen on Android 16), which has no window.
/// android-activity attaches `android_main`'s thread to the JVM for good,
/// so attaching here only checks a thread-local. Local references made in
/// `f` live in a frame of their own, which ends with it.
fn with_activity<T>(
    f: impl FnOnce(&mut Env<'_>, &JObject<'_>) -> jni::errors::Result<T>,
) -> jni::errors::Result<T> {
    let (vm, activity) = APP
        .with_borrow(|app| {
            app.as_ref()
                .map(|app| (app.vm_as_ptr(), app.activity_as_ptr()))
        })
        .ok_or(jni::errors::Error::NullPtr("the AndroidApp"))?;

    // SAFETY: android-activity documents the pointer as the process's
    // JavaVM, valid while the `AndroidApp` kept above lives.
    let vm = unsafe { JavaVM::from_raw(vm.cast()) };
    let activity = activity as jni::sys::jobject;

    vm.attach_current_thread(|env| {
        // SAFETY: the Activity's global reference, which `NativeActivity`
        // holds while the `AndroidApp` kept above lives. It is borrowed:
        // never wrapped in an owning reference, never deleted.
        let activity = unsafe { env.as_cast_raw::<JObject<'_>>(&activity)? };

        f(env, &activity)
    })
}
