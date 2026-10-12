//! Android (NativeActivity): the keyboard's Java side, loaded from the
//! embedded dex, and what it reports turned into the focused field's events.
//!
//! # A session
//!
//! The shell runs on `android_main`'s thread, and so does everything here
//! but the three native methods, which the Java side calls on the UI thread.
//!
//! - [`set_android_app`] keeps the run's `AndroidApp`, for its JavaVM and
//!   Activity. [`Shell::new`] starts the run's session with an empty queue.
//! - The first time a field asks for the keyboard, [`enable`] installs the
//!   Java side for the Activity: the dex is loaded with
//!   `InMemoryDexClassLoader` (API 26), whose parent is the Activity's class
//!   loader, and the natives are registered, once per process; then
//!   `Keyboard.install(activity)` adds the view. If anything fails (an older
//!   Android, a Java exception), one warning is logged and [`enable`] and
//!   [`disable`] return false for the rest of the run: the window then calls
//!   winit's `set_ime_allowed` and `set_ime_purpose`, as without the dex.
//! - [`enable`] runs at every redraw while a field asks for the keyboard. It
//!   calls `show(purpose)` when no field asked before, when the purpose
//!   changed, and once after a tap whose events the application has handled
//!   while the field still asks for it, which brings back a keyboard the
//!   user hid. When no field asked before, what the keyboard sent since the
//!   last field belongs to no field: it is dropped, and the engine and the
//!   Java side are reset if it held anything. [`disable`] calls `hide()`.
//! - From the first `show` to the next `hide`, the queue holds the window, so
//!   that a native call can wake the event loop: it pushes its message and
//!   requests a redraw (winit's Android loop ignores a bare wake). Native
//!   calls never call into Java and never panic across the boundary.
//! - [`Shell::drain`], when the event loop is about to wait, applies the
//!   queued messages to the engine in order and gives the edits to the
//!   window whose field asks for the keyboard (none: they are dropped). When
//!   a message asks for a reset (an Enter, Tab, Escape, arrow, Home or End
//!   key), the engine is reset before the next message is applied, and the
//!   Java side adopts the new generation; the messages it sent before that
//!   are dropped as stale.
//! - [`Shell::interrupt`], before a touch starts, a mouse button is pressed,
//!   a hardware key is pressed or the window loses the focus reaches the
//!   application: the queued messages are applied first, then, unless the
//!   engine is idle, it is reset, which commits a composition, and the Java
//!   side forgets its text. Those edits reach the field ahead of the event,
//!   so a tap on a Send button submits the word being composed, and the
//!   keyboard's text starts again where the caret is after the event.
//! - Dropping the [`Shell`] at the end of the run uninstalls the Java side,
//!   deletes its global reference, empties the queue and lets go of the
//!   window and the `AndroidApp`. The class and its loader stay loaded for
//!   the next Activity.
//!
//! Nothing here holds the queue's lock while it calls into Java, and nothing
//! logs what the keyboard typed.
// JNI is reached through `jni`'s calls, and through the raw functions the
// native methods need; each unsafe block says why it is sound.
#![allow(unsafe_code)]

use super::dex::{self, Method};
use super::edit::{Edit, Engine, Message};
use crate::conversion;
use crate::core::{self, input_method, theme, window};
use crate::graphics::Compositor;
use crate::program::Program;
use crate::window::WindowManager;

use jni::objects::{Global, JClass, JObject, JValue, JValueOwned};
use jni::signature::RuntimeMethodSignature;
use jni::strings::JNIString;
use jni::sys::{JNIEnv, jclass, jint, jstring};
use jni::{Env, JavaVM, NativeMethod, jni_sig, jni_str};
use winit::platform::android::activity::AndroidApp;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::panic;
use std::sync::atomic::{self, AtomicBool};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

/// The first SDK with `dalvik.system.InMemoryDexClassLoader`.
const IN_MEMORY_DEX: i32 = 26;

thread_local! {
    /// The session of the application running on this thread: the shell
    /// runs on `android_main`'s thread.
    static SESSION: RefCell<Session> = RefCell::new(Session::default());
}

/// What the native methods have sent and the shell has not applied yet,
/// and the window to wake when they send more.
struct Queue {
    messages: VecDeque<Message>,
    /// Held from the first `show` to the next `hide`.
    window: Option<Arc<winit::window::Window>>,
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue {
    messages: VecDeque::new(),
    window: None,
});

/// The Java class, loaded once per process.
static CLASS: OnceLock<Global<JClass<'static>>> = OnceLock::new();

/// Keeps `app` for the keyboard of the application that runs on this
/// thread, replacing the last Activity's.
pub(crate) fn set_android_app(app: AndroidApp) {
    SESSION.with_borrow_mut(|session| session.app = Some(app));
}

/// A field asks for the keyboard with `purpose`, at a redraw: shows it when
/// needed (see the module's documentation). False when the keyboard's Java
/// side is not available, and the window must ask winit instead.
pub(crate) fn enable(
    window: &Arc<winit::window::Window>,
    purpose: input_method::Purpose,
) -> bool {
    SESSION.with_borrow_mut(|session| session.enable(window, purpose))
}

/// No field asks for the keyboard any more: hides it. False when the
/// keyboard's Java side is not available, and the window must ask winit
/// instead.
pub(crate) fn disable() -> bool {
    SESSION.with_borrow_mut(Session::disable)
}

/// The shell's side of the run's session: what the event loop calls, and
/// the end of the session when it is dropped.
pub(crate) struct Shell;

impl Shell {
    /// Starts the run's session. A message left from the last Activity's
    /// keyboard is dropped.
    pub(crate) fn new() -> Self {
        clear_queue();

        Self
    }

    /// Before `event` reaches the application: a touch that starts, a mouse
    /// button pressed, a hardware key pressed or the loss of the window's
    /// focus ends what the keyboard was typing (see the module's
    /// documentation).
    pub(crate) fn interrupt<P, C>(
        &mut self,
        event: &winit::event::WindowEvent,
        windows: &mut WindowManager<P, C>,
        events: &mut Vec<(window::Id, core::Event)>,
    ) where
        P: Program,
        C: Compositor<Renderer = P::Renderer>,
        P::Theme: theme::Base,
    {
        use winit::event::{ElementState, TouchPhase, WindowEvent};

        let touch = match event {
            WindowEvent::Touch(touch) if touch.phase == TouchPhase::Started => {
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } => true,
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } if event.state == ElementState::Pressed => false,
            WindowEvent::Focused(false) => false,
            _ => return,
        };

        let edits = SESSION.with_borrow_mut(|session| {
            let mut edits = Vec::new();

            if !session.is_installed() {
                return edits;
            }

            if touch {
                session.touch = Touch::Pending;
            }

            session.apply_queue(&mut edits);

            if !session.engine.is_idle() {
                let generation = session.engine.reset(&mut edits);
                session.call(dex::RESET, &[JValue::Int(generation)]);
            }

            edits
        });

        deliver(edits, windows, events);
    }

    /// When the event loop is about to wait: applies what the keyboard sent
    /// since it last turned, and gives the edits to the window whose field
    /// asks for the keyboard. Called before the loop's idle check.
    pub(crate) fn drain<P, C>(
        &mut self,
        windows: &mut WindowManager<P, C>,
        events: &mut Vec<(window::Id, core::Event)>,
    ) where
        P: Program,
        C: Compositor<Renderer = P::Renderer>,
        P::Theme: theme::Base,
    {
        let edits = SESSION.with_borrow_mut(|session| {
            // The application handles this turn's events, the tap's among
            // them, before the next redraw asks for the keyboard.
            if session.touch == Touch::Pending {
                session.touch = Touch::Armed;
            }

            let mut edits = Vec::new();
            session.apply_queue(&mut edits);

            edits
        });

        deliver(edits, windows, events);
    }
}

/// The run ends: the Java side goes, and with it the Activity.
impl Drop for Shell {
    fn drop(&mut self) {
        let _ = SESSION.try_with(|session| session.borrow_mut().end());

        clear_queue();
    }
}

/// The keyboard of the run, and what the engine knows of its text.
#[derive(Default)]
struct Session {
    app: Option<AndroidApp>,
    keyboard: Keyboard,
    engine: Engine,
    /// The purpose the keyboard was last shown for, while a field asks for
    /// it.
    shown: Option<i32>,
    touch: Touch,
}

/// The Java side for the run's Activity.
#[derive(Default)]
enum Keyboard {
    /// No field asked for the keyboard yet.
    #[default]
    Absent,
    /// The `iced_mobile.Keyboard` that `install` returned.
    Installed(Global<JObject<'static>>),
    /// It cannot be installed: winit's calls stand in for it.
    Failed,
}

/// A touch, until it brings the keyboard back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Touch {
    #[default]
    None,
    /// A touch started; the application has not handled it yet.
    Pending,
    /// The application handled it: the next redraw that still asks for the
    /// keyboard shows it again.
    Armed,
}

impl Session {
    fn enable(
        &mut self,
        window: &Arc<winit::window::Window>,
        purpose: input_method::Purpose,
    ) -> bool {
        let purpose = match purpose {
            input_method::Purpose::Normal => dex::PURPOSE_NORMAL,
            input_method::Purpose::Secure => dex::PURPOSE_SECURE,
            input_method::Purpose::Terminal => dex::PURPOSE_TERMINAL,
        };

        let touched = self.touch == Touch::Armed;

        if touched {
            self.touch = Touch::None;
        }

        if !self.install() {
            return false;
        }

        if self.shown.is_none() {
            // What the keyboard sent since the last field asked for it
            // belongs to no field.
            let mut dropped = Vec::new();
            self.apply_queue(&mut dropped);

            if !self.engine.is_idle() {
                let generation = self.engine.reset(&mut dropped);
                self.call(dex::RESET, &[JValue::Int(generation)]);
            }

            set_window(Some(window.clone()));
        }

        if self.shown != Some(purpose) || touched {
            self.call(dex::SHOW, &[JValue::Int(purpose)]);
            self.shown = Some(purpose);
        }

        true
    }

    fn disable(&mut self) -> bool {
        self.touch = Touch::None;

        if !self.is_installed() {
            return false;
        }

        if self.shown.take().is_some() {
            self.call(dex::HIDE, &[]);
            set_window(None);
        }

        true
    }

    fn is_installed(&self) -> bool {
        matches!(self.keyboard, Keyboard::Installed(_))
    }

    /// Installs the Java side the first time; whether it is installed.
    fn install(&mut self) -> bool {
        match self.keyboard {
            Keyboard::Installed(_) => return true,
            Keyboard::Failed => return false,
            Keyboard::Absent => {}
        }

        let Some(app) = &self.app else {
            self.keyboard = Keyboard::Failed;
            return false;
        };

        let sdk = app.config().sdk_version();

        if sdk < IN_MEMORY_DEX {
            warn(
                "the input connection needs API 26, so the keyboard sends key \
                events only, without composition, suggestions or secure entry",
                &format_args!("this device runs API {sdk}"),
            );

            self.keyboard = Keyboard::Failed;
            return false;
        }

        match with_activity(app, install) {
            Ok(keyboard) => {
                log::debug!("Keyboard: the input connection is installed");

                self.keyboard = Keyboard::Installed(keyboard);
                true
            }
            Err(error) => {
                warn(
                    "the input connection cannot be installed, so the \
                    keyboard sends key events only, without composition, \
                    suggestions or secure entry",
                    &error,
                );

                self.keyboard = Keyboard::Failed;
                false
            }
        }
    }

    /// Applies the queued messages in order, resetting the engine and the
    /// Java side where a message asks for it.
    fn apply_queue(&mut self, edits: &mut Vec<Edit>) {
        // The lock is let go of before anything calls into Java.
        let messages = std::mem::take(&mut queue().messages);

        for message in messages {
            if self.engine.apply(message, edits) {
                let generation = self.engine.reset(edits);
                self.call(dex::RESET, &[JValue::Int(generation)]);
            }
        }
    }

    /// Calls `method` of the installed Java side. A failure is logged; the
    /// Java side catches its own exceptions, so none is expected.
    fn call(&self, method: Method, args: &[JValue<'_>]) {
        let (Some(app), Keyboard::Installed(keyboard)) =
            (&self.app, &self.keyboard)
        else {
            return;
        };

        let result = with_activity(app, |env, _activity| {
            let signature = RuntimeMethodSignature::from_str(method.signature)?;

            let _ = env.call_method(
                keyboard,
                JNIString::new(method.name),
                signature.method_signature(),
                args,
            )?;

            Ok(())
        });

        if let Err(error) = result {
            warn(&format!("Keyboard.{} failed", method.name), &error);
        }
    }

    /// The run ends: uninstalls the Java side and forgets everything.
    fn end(&mut self) {
        self.call(dex::UNINSTALL, &[]);

        // Deletes the global reference.
        self.keyboard = Keyboard::Absent;
        self.engine = Engine::new();
        self.shown = None;
        self.touch = Touch::None;
        self.app = None;
    }
}

/// Gives the edits to the window whose field asks for the keyboard, as its
/// events.
fn deliver<P, C>(
    edits: Vec<Edit>,
    windows: &mut WindowManager<P, C>,
    events: &mut Vec<(window::Id, core::Event)>,
) where
    P: Program,
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    if edits.is_empty() {
        return;
    }

    let Some((id, window)) = windows
        .iter_mut()
        .find(|(_id, window)| window.ime_requested())
    else {
        log::debug!(
            "Keyboard: {} edits dropped: no field asks for the keyboard",
            edits.len()
        );

        return;
    };

    let modifiers = conversion::modifiers(window.state.modifiers());

    for edit in edits {
        super::events(edit, modifiers, |event| events.push((id, event)));
    }
}

/// Loads the class if this process has not, then installs the Java side
/// for `activity`.
fn install(
    env: &mut Env<'_>,
    activity: &JObject<'_>,
) -> jni::errors::Result<Global<JObject<'static>>> {
    let class = match CLASS.get() {
        Some(class) => class,
        None => {
            let class = load(env, activity)?;
            CLASS.get_or_init(|| class)
        }
    };

    let signature = RuntimeMethodSignature::from_str(dex::INSTALL.signature)?;

    let keyboard = env
        .call_static_method(
            class,
            JNIString::new(dex::INSTALL.name),
            signature.method_signature(),
            &[JValue::Object(activity)],
        )
        .and_then(JValueOwned::l)?;

    if keyboard.is_null() {
        return Err(jni::errors::Error::NullPtr("Keyboard.install"));
    }

    env.new_global_ref(&keyboard)
}

/// Loads the class from the embedded dex, with the Activity's class loader
/// as the parent, and registers its native methods.
fn load(
    env: &mut Env<'_>,
    activity: &JObject<'_>,
) -> jni::errors::Result<Global<JClass<'static>>> {
    let bytes = env.byte_array_from_slice(dex::DEX)?;

    let buffer = env
        .call_static_method(
            jni_str!("java/nio/ByteBuffer"),
            jni_str!("wrap"),
            jni_sig!("([B)Ljava/nio/ByteBuffer;"),
            &[JValue::from(&bytes)],
        )
        .and_then(JValueOwned::l)?;

    let parent = env
        .call_method(
            activity,
            jni_str!("getClassLoader"),
            jni_sig!("()Ljava/lang/ClassLoader;"),
            &[],
        )
        .and_then(JValueOwned::l)?;

    let loader = env.new_object(
        jni_str!("dalvik/system/InMemoryDexClassLoader"),
        jni_sig!("(Ljava/nio/ByteBuffer;Ljava/lang/ClassLoader;)V"),
        &[JValue::Object(&buffer), JValue::Object(&parent)],
    )?;

    let name = env.new_string(dex::KEYBOARD)?;

    let class = env
        .call_method(
            &loader,
            jni_str!("loadClass"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/Class;"),
            &[JValue::from(&name)],
        )
        .and_then(JValueOwned::l)?;

    let class = env.cast_local::<JClass<'_>>(class)?;

    register(env, &class)?;

    env.new_global_ref(&class)
}

/// Binds the class's three native methods to the functions below.
fn register(env: &mut Env<'_>, class: &JClass<'_>) -> jni::errors::Result<()> {
    let natives: [(Method, *mut c_void); 3] = [
        (dex::NATIVE_STATE, native_state as *mut c_void),
        (dex::NATIVE_KEY, native_key as *mut c_void),
        (dex::NATIVE_REBASE, native_rebase as *mut c_void),
    ];

    let names: Vec<(JNIString, JNIString, *mut c_void)> = natives
        .iter()
        .map(|(method, function)| {
            (
                JNIString::new(method.name),
                JNIString::new(method.signature),
                *function,
            )
        })
        .collect();

    let methods: Vec<NativeMethod<'_>> = names
        .iter()
        .map(|(name, signature, function)| {
            // SAFETY: each function below is `extern "system"`, takes the
            // environment, the class (the methods are static) and the
            // arguments of the signature it is paired with, in order, and
            // returns nothing, as `void` asks.
            unsafe { NativeMethod::from_raw_parts(name, signature, *function) }
        })
        .collect();

    // SAFETY: the methods are static native methods of `class`, each
    // function matching its signature (above).
    unsafe { env.register_native_methods(class, &methods) }
}

/// Runs `f` with the Activity, on this thread attached to the JVM.
///
/// The Activity comes from the `AndroidApp`, as in `safe_area`. A Java
/// exception `f` leaves is described (logged) and cleared; its error is
/// returned.
fn with_activity<T>(
    app: &AndroidApp,
    f: impl FnOnce(&mut Env<'_>, &JObject<'_>) -> jni::errors::Result<T>,
) -> jni::errors::Result<T> {
    // SAFETY: android-activity documents the pointer as the process's
    // JavaVM, valid while the `AndroidApp` lives.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let activity = app.activity_as_ptr() as jni::sys::jobject;

    vm.attach_current_thread(|env| {
        // SAFETY: the Activity's global reference, which `NativeActivity`
        // holds while the `AndroidApp` lives. It is borrowed: never wrapped
        // in an owning reference, never deleted.
        let activity = unsafe { env.as_cast_raw::<JObject<'_>>(&activity)? };

        let result = f(env, &activity);

        if env.exception_check() {
            env.exception_describe();
            env.exception_clear();
        }

        result
    })
}

/// Logs a failure of the keyboard's Java side: a warning the first time in
/// the process, then at the debug level.
fn warn(what: &str, error: &dyn std::fmt::Display) {
    static WARNED: AtomicBool = AtomicBool::new(false);

    if WARNED.swap(true, atomic::Ordering::Relaxed) {
        log::debug!("Keyboard: {what} ({error})");
    } else {
        log::warn!("Keyboard: {what} ({error})");
    }
}

/// The queue, even if a thread panicked while it held it: its state is
/// always whole.
fn queue() -> MutexGuard<'static, Queue> {
    QUEUE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Holds the window to wake, or lets go of it.
fn set_window(window: Option<Arc<winit::window::Window>>) {
    // Dropped once the lock is let go of.
    let _last = std::mem::replace(&mut queue().window, window);
}

/// Empties the queue and lets go of its window.
fn clear_queue() {
    let _left = {
        let mut queue = queue();

        (std::mem::take(&mut queue.messages), queue.window.take())
    };
}

/// UI thread: queues a message and wakes the event loop.
fn push(message: Message) {
    let mut queue = queue();

    queue.messages.push_back(message);

    // Sets a flag and wakes the looper; it blocks on nothing.
    if let Some(window) = &queue.window {
        window.request_redraw();
    }
}

/// `Keyboard.nativeState(int generation, String text, int selStart,
/// int selEnd, int composingStart, int composingEnd)`.
#[allow(clippy::too_many_arguments)]
extern "system" fn native_state(
    env: *mut JNIEnv,
    _class: jclass,
    generation: jint,
    text: jstring,
    selection_start: jint,
    selection_end: jint,
    composing_start: jint,
    composing_end: jint,
) {
    let _ = panic::catch_unwind(|| {
        // SAFETY: the JVM calls this for the native method with this
        // thread's environment and a local reference to the text, valid
        // until it returns.
        let Some(text) = (unsafe { utf16(env, text) }) else {
            return;
        };

        push(Message::State {
            generation,
            text,
            selection: (selection_start, selection_end),
            composing: (composing_start, composing_end),
        });
    });
}

/// `Keyboard.nativeKey(int generation, int keyCode)`.
extern "system" fn native_key(
    _env: *mut JNIEnv,
    _class: jclass,
    generation: jint,
    code: jint,
) {
    let _ = panic::catch_unwind(|| push(Message::Key { generation, code }));
}

/// `Keyboard.nativeRebase(int generation)`.
extern "system" fn native_rebase(
    _env: *mut JNIEnv,
    _class: jclass,
    generation: jint,
) {
    let _ = panic::catch_unwind(|| push(Message::Rebase { generation }));
}

/// A Java string read as UTF-16, so that an unpaired surrogate becomes one
/// U+FFFD and the UTF-16 indices the Java side sent still hold (the modified
/// UTF-8 path would make several of it). `None` for a null string or when
/// the read throws.
///
/// # Safety
///
/// `env` is the calling thread's JNI environment and `text` a valid
/// reference to a `java.lang.String`, or null.
unsafe fn utf16(env: *mut JNIEnv, text: jstring) -> Option<String> {
    if env.is_null() || text.is_null() {
        return None;
    }

    // SAFETY: a JNI environment points to its function table, which every
    // JVM since 1.2 fills up to `v1_2` (GetStringLength, GetStringRegion,
    // ExceptionCheck); `text` is a string reference (the caller).
    unsafe {
        let functions = &(**env).v1_2;

        let length = (functions.GetStringLength)(env, text);
        let mut units = vec![0; usize::try_from(length).ok()?];

        (functions.GetStringRegion)(env, text, 0, length, units.as_mut_ptr());

        if (functions.ExceptionCheck)(env) {
            return None;
        }

        Some(String::from_utf16_lossy(&units))
    }
}
