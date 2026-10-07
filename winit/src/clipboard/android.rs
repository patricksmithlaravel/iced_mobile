//! Android: the system clipboard, `ClipboardManager`, through JNI.
//!
//! The calls run on the thread of `android_main`, which runs the event loop:
//! android-activity attaches it to the JVM for good and gives `ndk-context`
//! the Activity before `android_main` starts. A Java exception comes back as
//! an error, which is logged.
#![allow(unsafe_code)]

use jni::objects::{JObject, JString, JValue};
use jni::vm::JavaVM;
use jni::{Env, jni_sig, jni_str};

/// The text on the clipboard. `None` when it is empty, holds no text, or
/// Android refuses the read: from Android 10, only the app with the input
/// focus may read it.
pub(super) fn read() -> Option<String> {
    with_activity(read_text).unwrap_or_else(|error| {
        log::warn!("Reading the clipboard failed: {error}");
        None
    })
}

/// Puts `contents` on the clipboard, as plain text.
pub(super) fn write(contents: &str) {
    if let Err(error) =
        with_activity(|env, activity| write_text(env, activity, contents))
    {
        log::warn!("Writing to the clipboard failed: {error}");
    }
}

fn read_text(
    env: &mut Env<'_>,
    activity: &JObject<'_>,
) -> jni::errors::Result<Option<String>> {
    let manager = clipboard_manager(env, activity)?;

    let clip = env
        .call_method(
            &manager,
            jni_str!("getPrimaryClip"),
            jni_sig!(() -> android.content.ClipData),
            &[],
        )?
        .l()?;

    if clip.is_null() {
        return Ok(None);
    }

    let count = env
        .call_method(
            &clip,
            jni_str!("getItemCount"),
            jni_sig!(() -> jint),
            &[],
        )?
        .i()?;

    if count < 1 {
        return Ok(None);
    }

    let item = env
        .call_method(
            &clip,
            jni_str!("getItemAt"),
            jni_sig!((jint) -> android.content.ClipData::Item),
            &[JValue::Int(0)],
        )?
        .l()?;

    let mut text = env
        .call_method(
            &item,
            jni_str!("getText"),
            jni_sig!(() -> java.lang.CharSequence),
            &[],
        )?
        .l()?;

    // An item without text holds a URI or an Intent. `coerceToText` would
    // read a URI's content through its provider, on this thread: only when
    // the clip says it is text.
    if text.is_null() && is_text(env, &clip)? {
        text = env
            .call_method(
                &item,
                jni_str!("coerceToText"),
                jni_sig!((android.content.Context) -> java.lang.CharSequence),
                &[JValue::Object(activity)],
            )?
            .l()?;
    }

    if text.is_null() {
        return Ok(None);
    }

    let text = env
        .call_method(
            &text,
            jni_str!("toString"),
            jni_sig!(() -> java.lang.String),
            &[],
        )?
        .l()?;

    let text = env.cast_local::<JString<'_>>(text)?;

    if text.is_null() {
        return Ok(None);
    }

    text.try_to_string(env).map(Some)
}

/// Whether the clip's description lists a `text/*` type.
fn is_text(env: &mut Env<'_>, clip: &JObject<'_>) -> jni::errors::Result<bool> {
    let description = env
        .call_method(
            clip,
            jni_str!("getDescription"),
            jni_sig!(() -> android.content.ClipDescription),
            &[],
        )?
        .l()?;

    if description.is_null() {
        return Ok(false);
    }

    let text = env.new_string("text/*")?;

    env.call_method(
        &description,
        jni_str!("hasMimeType"),
        jni_sig!((java.lang.String) -> jboolean),
        &[JValue::Object(&text)],
    )?
    .z()
}

fn write_text(
    env: &mut Env<'_>,
    activity: &JObject<'_>,
    contents: &str,
) -> jni::errors::Result<()> {
    let manager = clipboard_manager(env, activity)?;
    let label = env.new_string("")?;
    let text = env.new_string(contents)?;

    let clip = env
        .call_static_method(
            jni_str!("android/content/ClipData"),
            jni_str!("newPlainText"),
            jni_sig!(
                (java.lang.CharSequence, java.lang.CharSequence)
                    -> android.content.ClipData
            ),
            &[JValue::Object(&label), JValue::Object(&text)],
        )?
        .l()?;

    let _ = env.call_method(
        &manager,
        jni_str!("setPrimaryClip"),
        jni_sig!((android.content.ClipData) -> void),
        &[JValue::Object(&clip)],
    )?;

    Ok(())
}

/// `activity.getSystemService(Context.CLIPBOARD_SERVICE)`.
fn clipboard_manager<'local>(
    env: &mut Env<'local>,
    activity: &JObject<'_>,
) -> jni::errors::Result<JObject<'local>> {
    let name = env.new_string("clipboard")?;

    let manager = env
        .call_method(
            activity,
            jni_str!("getSystemService"),
            jni_sig!((java.lang.String) -> java.lang.Object),
            &[JValue::Object(&name)],
        )?
        .l()?;

    if manager.is_null() {
        return Err(jni::errors::Error::NullPtr("ClipboardManager"));
    }

    Ok(manager)
}

/// Runs `f` with the thread's JNI environment and the Activity, in a local
/// frame of its own, so no local reference outlives the call.
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
        // android-activity deletes only after `android_main` returns. The
        // cast borrows it: it is never wrapped in an owning reference, and
        // never deleted here.
        let activity = unsafe { env.as_cast_raw::<JObject<'_>>(&activity)? };

        f(env, &activity)
    })
}
