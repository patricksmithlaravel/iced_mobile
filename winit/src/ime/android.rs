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
//!   changed, and once after a tap on the focused field ([`touched`], after
//!   the application handled it) or a widget operation that focused a
//!   widget ([`operated`]), which brings back a keyboard the user hid. When
//!   no field asked before, what the keyboard sent since the last field
//!   belongs to no field: it is dropped, and the engine and the Java side
//!   are reset if it held anything. Before the `show`, it tells the engine
//!   whether the field is a password field, which never shows a
//!   composition (see `feed`): a composition the engine shows when the
//!   field becomes one (the application made it secure while the keyboard
//!   composed) is committed as typed text, with no new generation, and so
//!   is one held back from the field since a focus moved (an `on_submit`
//!   that focuses the password field), which a field that is no password
//!   field gets as its preedit once a tap or a focus move has decided the
//!   field. The window then redraws, and the next drain, operation or
//!   interrupt gives the field these edits before anything else. The
//!   window draws no preedit over a password field. After an Enter, Tab or
//!   Escape, from the keyboard or from hardware, a redraw decides only once
//!   a focus move from code or a tap on the field has come after the key, or
//!   once the settle window after the key has closed: the application may
//!   still move the focus in a subscription (see `feed`), so the composition
//!   waits for the window's end. While it waits, the shell asks for the
//!   redraw at the window's end, or at once when the window has closed
//!   ([`Feed::settle_at`], [`settle`]). The session's end ends that hold (see
//!   [`Feed::abandon`]): the session that starts again for the field, after
//!   the application's turn, gives its purpose before the keyboard shows.
//! - [`disable`] runs when no field asks any more. The field has lost the
//!   focus, so what the keyboard typed and composes there is dropped, as
//!   desktop input methods drop a composition then: the queue is emptied,
//!   the engine and the Java side are reset unless the engine is idle and
//!   nothing was queued (a queued report may hold text the engine never
//!   saw), and `hide()` follows. The window gets
//!   `input_method::Event::Closed` at the next drain or interrupt, which
//!   clears the preedit every text widget keeps (`text_input` and
//!   `text_editor` take it whatever their focus).
//! - A widget operation (`operation::focus`, `focus_next`, `unfocus`,
//!   `move_cursor_to`, `select_all`, ...) can move the focus or the caret
//!   while the keyboard has typed into the field, or while the engine knows
//!   the end of the field's text before the origin, which a caret moved
//!   elsewhere would make wrong ([`Feed::follows`]). What the keyboard sent
//!   before it belongs before it, so [`operate`] applies the queue first,
//!   except in the application's turn after an Enter, Tab or Escape, when
//!   the operation may answer the key (see `feed`). When that gives the
//!   field edits, the operation waits: the event loop replays an
//!   `AboutToWait`, which hands the field the edits and builds the views
//!   again, then runs it, and the operations made meanwhile after it
//!   ([`schedule`], [`deferred`]). While the keyboard is shown, the
//!   operation then runs through a [`Watch`](super::Watch), which records
//!   what it moves of the focus and of the carets of the fields that have
//!   it, and [`operated`] ends the session when the engine follows the
//!   caret: a focus moved to another widget is handled as [`disable`]
//!   handles it, and a caret moved in the field as a tap is. The caret of a
//!   field without the focus moves at once and ends nothing.
//!   While the keyboard has typed into the field, the watch holds the caret
//!   moves back, so that what the keyboard sent before them reaches the
//!   field where it was typed, a composition and what arrives while the
//!   operation runs alike, and they are made once the field has it,
//!   deferred the same way; a `select_all` then selects the word too,
//!   instead of the word replacing the selection. A widget the operation
//!   focuses, also the one that has the focus, shows the keyboard again at
//!   the next redraw, as a tap on it does. A focus the operation moved
//!   holds back the keyboard's compositions from the field until that
//!   redraw tells the shell whether it is a password field.
//! - From the first `show` to the next `hide`, the queue holds the window, so
//!   that a native call can wake the event loop: it pushes its message and
//!   requests a redraw (winit's Android loop ignores a bare wake). Native
//!   calls never call into Java and never panic across the boundary.
//! - [`Shell::drain`], when the event loop is about to wait, applies the
//!   queued messages to the engine in order and gives the edits to the
//!   window whose field asks for the keyboard (none: they are dropped).
//!   After an Enter, Tab or Escape key it stops, and the application takes
//!   its turn (see `feed`): it handles the key (a submit that clears the
//!   field), builds its view again, and the widget operations its `update`
//!   returns run (a focus moved to the next field), before the event loop
//!   replays [`Shell::resume`] and an `AboutToWait` for the rest, which then
//!   reaches the field the application left the focus in, with no preedit
//!   until the settle window has closed, or a tap or a focus move lets a
//!   redraw tell the shell that field's purpose. The Java side
//!   rebases its mirror right after such a key, as after an arrow, Home or
//!   End, with no new generation, so none of that text is dropped. When a
//!   message asks to forget the text (the session's text starts with a
//!   character that joins the text before it), the Java side is asked to
//!   rebase, with no new generation.
//! - [`Shell::interrupt`], before a touch starts, a mouse button is pressed,
//!   a hardware key other than a modifier is pressed or the window loses
//!   the focus reaches the application: the queued messages are applied
//!   first, then, unless the engine is idle, it is reset, which commits a
//!   composition, and the Java side forgets its text. When that gave the
//!   field edits, the event loop applies them and builds the views again
//!   before the event reaches the application (it returns true): a tap on a
//!   Send button submits the word being composed, even when the button is
//!   enabled only once the field holds text. When the queue holds an Enter,
//!   Tab or Escape key, the edits up to it come first and the application
//!   takes its turn; the event, handled again after it, applies the rest.
//!   The keyboard's text starts again where the caret is after the event. A
//!   modifier (Shift, Ctrl, Alt, ...) moves no caret and ends nothing: an
//!   input method that composes a hardware keyboard's letters may let it
//!   through. It reaches the application at once, before the edits queued
//!   ahead of it, and those the keyboard makes while it is held: neither
//!   takes it, since the keys the edits become come without the window's
//!   modifiers (`events`).
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
use super::edit::{Edit, Message};
use super::feed::{self, Call, Feed};
use crate::conversion;
use crate::core::widget::Operation;
use crate::core::{self, input_method, mouse, theme, window};
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

/// No field of `window` asks for the keyboard any more: hides it (see the
/// module's documentation). False when the keyboard's Java side is not
/// available, and the window must ask winit instead.
pub(crate) fn disable(window: winit::window::WindowId) -> bool {
    SESSION.with_borrow_mut(|session| session.disable(window))
}

/// After the application handled `events` of a window, with the cursor at
/// `cursor`: when one of them pressed on a widget that has the focus now,
/// as `operate` finds it by running an operation on the window's interface,
/// the next redraw that still asks for the keyboard shows it again, which
/// brings back a keyboard the user hid. A press elsewhere leaves it hidden:
/// the field that had the focus gives it up (a `text_input` does, and a
/// `text_editor` does on Android and iOS). True when the window is to redraw
/// for that.
///
/// The focus is the one after the press, which may have moved it to
/// another field, and the bounds are those the press was hit-tested
/// against. A press on the focused field holds the keyboard's compositions
/// back from it until that redraw, since the field may be a password field
/// (see [`Feed::refocused`]). That holds while the keyboard is hidden too:
/// the tap decides the field's purpose then as well, so a composition after
/// it is not held back until a second tap.
pub(crate) fn touched(
    events: &[core::Event],
    cursor: mouse::Cursor,
    operate: impl FnOnce(&mut dyn Operation),
) -> bool {
    if !SESSION.with_borrow(Session::is_installed) {
        return false;
    }

    let presses = super::presses(events, cursor);

    if presses.is_empty() {
        return false;
    }

    // The interface's widgets are not reached with the session borrowed.
    let mut focused = super::Focused::default();
    operate(&mut focused);

    let touched = presses.into_iter().any(|press| focused.contains(press));

    // A widget focused from code before keeps its request. The press may
    // have focused a password field, whose purpose the shell learns at the
    // redraw this asks for: until then the keyboard's compositions are held
    // back from it, as after an operation moved the focus.
    SESSION.with_borrow_mut(|session| {
        session.touched |= touched;

        if touched {
            session.feed.refocused();
        }
    });

    touched
}

/// How a widget operation runs on the window whose field has the keyboard,
/// as [`operate`] decides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Typing {
    /// The window: the operation runs on its interface through a
    /// [`Watch`](super::Watch).
    pub(crate) window: window::Id,
    /// Whether a focus or a caret the operation moves in the field ends
    /// what the keyboard typed ([`Feed::follows`]).
    pub(crate) follows: bool,
    /// Whether the watch holds caret moves back: the keyboard has typed
    /// into the field ([`Feed::holds`]).
    pub(crate) hold: bool,
}

/// Before a widget operation runs (see the module's documentation): what
/// the keyboard sent before it is applied first. Returns the operation, and
/// how to run it while the keyboard is shown for a field; none when it
/// waits for the field to take edits first, until the event loop takes it
/// from [`deferred`].
pub(crate) fn operate<P, C>(
    operation: Box<dyn Operation>,
    windows: &mut WindowManager<P, C>,
    events: &mut Vec<(window::Id, core::Event)>,
) -> Option<(Box<dyn Operation>, Option<Typing>)>
where
    P: Program,
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    // Operations run in the order the application made them.
    if SESSION.with_borrow(|session| !session.deferred.is_empty()) {
        defer(operation);
        return None;
    }

    let Some(window) = windows
        .iter_mut()
        .find(|(_id, window)| window.ime_requested())
        .map(|(id, _window)| id)
    else {
        return Some((operation, None));
    };

    let (edits, due) = SESSION.with_borrow_mut(|session| {
        let mut edits = Vec::new();

        if !session.is_shown() {
            return (edits, false);
        }

        let mut calls = Vec::new();
        let due = session.feed.operate(&mut Shared, &mut edits, &mut calls);
        session.perform(calls);

        (edits, due)
    });

    // Edits given to the field now, or up to a key the application has not
    // handled yet, come first.
    if deliver(edits, windows, events) || due {
        defer(operation);
        return None;
    }

    let typing = SESSION.with_borrow(|session| {
        session.is_shown().then(|| Typing {
            window,
            follows: session.feed.follows(),
            hold: session.feed.holds(),
        })
    });

    Some((operation, typing))
}

/// A widget operation ran on the window `typing` names, watched: ends the
/// keyboard's session if it moved the focus or the caret while the engine
/// follows it, defers the caret moves it held back until the field has
/// what the keyboard sent, and shows the keyboard again at the next redraw
/// if it focused a widget (see the module's documentation).
pub(crate) fn operated<P, C>(
    typing: Typing,
    moved: super::Moved,
    windows: &mut WindowManager<P, C>,
    events: &mut Vec<(window::Id, core::Event)>,
) where
    P: Program,
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    // Otherwise the engine is idle and knows nothing before its origin,
    // which moves with the caret, and the focus moves on with it.
    if typing.follows {
        if moved.focus {
            SESSION.with_borrow_mut(Session::abandon);

            events.push((
                typing.window,
                core::Event::InputMethod(input_method::Event::Closed),
            ));
        } else if moved.caret || !moved.held.is_empty() {
            // It stops after an Enter, Tab or Escape: the key left the
            // engine idle, so the rest acts at the caret wherever it is.
            let (edits, _waits) =
                SESSION.with_borrow_mut(|session| session.finish(false));

            let _ = deliver(edits, windows, events);
        }
    }

    // The field that has the focus now may be a password field: it gets no
    // preedit until a redraw tells the shell its purpose.
    if moved.focus {
        SESSION.with_borrow_mut(|session| session.feed.refocused());

        if let Some(window) = windows.get_mut(typing.window) {
            window.raw.request_redraw();
        }
    }

    if !moved.held.is_empty() {
        defer(Box::new(super::Replay::new(moved.held)));
    }

    // The application asks for the keyboard there: one the user hid shows
    // again, as at a tap on the field.
    if moved.focused {
        SESSION.with_borrow_mut(|session| session.touched = true);

        if let Some(window) = windows.get_mut(typing.window) {
            window.raw.request_redraw();
        }
    }
}

/// Whether the event loop is to run the deferred operations: true once
/// for them, then the event loop replays an `AboutToWait`, which hands the
/// field the edits they wait for, and takes them from [`deferred`].
pub(crate) fn schedule() -> bool {
    SESSION.with_borrow_mut(|session| {
        let due = !session.deferred.is_empty() && !session.scheduled;

        session.scheduled |= due;
        due
    })
}

/// The deferred operations, in order, for the event loop to run now, each
/// through [`operate`] again.
pub(crate) fn deferred() -> Vec<Box<dyn Operation>> {
    SESSION.with_borrow_mut(|session| {
        session.scheduled = false;
        session.deferred.drain(..).collect()
    })
}

/// Puts `operation` behind the deferred ones.
fn defer(operation: Box<dyn Operation>) {
    SESSION.with_borrow_mut(|session| session.deferred.push_back(operation));
}

/// The shell's side of the run's session: what the event loop calls, and
/// the end of the session when it is dropped.
pub(crate) struct Shell {
    /// The next interrupting event is the one the last interrupt returned
    /// true for, which the event loop handles again.
    replaying: bool,
}

impl Shell {
    /// Starts the run's session. A message left from the last Activity's
    /// keyboard is dropped.
    pub(crate) fn new() -> Self {
        clear_queue();

        Self { replaying: false }
    }

    /// Before `event` reaches the application: a touch that starts, a mouse
    /// button pressed, a hardware key other than a modifier pressed or the
    /// loss of the window's focus ends what the keyboard was typing (see the
    /// module's documentation).
    ///
    /// True when the field got edits, or the application is to take its
    /// turn after a key the keyboard sent before the event: the event loop
    /// then handles them, and builds the views again, before it hands
    /// `event` to the application, and calls this again with `event`, which
    /// then applies what the keyboard sent after the key, if any, the same
    /// way, or does nothing.
    #[must_use]
    pub(crate) fn interrupt<P, C>(
        &mut self,
        event: &winit::event::WindowEvent,
        windows: &mut WindowManager<P, C>,
        events: &mut Vec<(window::Id, core::Event)>,
    ) -> bool
    where
        P: Program,
        C: Compositor<Renderer = P::Renderer>,
        P::Theme: theme::Base,
    {
        use winit::event::{ElementState, TouchPhase, WindowEvent};

        match event {
            WindowEvent::Touch(touch) if touch.phase == TouchPhase::Started => {
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } => {}
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } if event.state == ElementState::Pressed
                && super::ends_typing(&event.logical_key) => {}
            WindowEvent::Focused(false) => {}
            _ => return false,
        }

        // A hardware Enter, Tab or Escape: the application may move the
        // focus after it, as after the keyboard's key (see `feed`).
        let yields = matches!(
            event,
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } if event.state == ElementState::Pressed
                && super::yields_turn(&event.logical_key)
        );

        // Handled again: done, unless the application took its turn after a
        // key, and the rest comes before the event.
        let (edits, waits) = if std::mem::take(&mut self.replaying)
            && !SESSION.with_borrow(|session| session.feed.takes_turn())
        {
            (Vec::new(), false)
        } else {
            SESSION.with_borrow_mut(|session| session.finish(yields))
        };

        closed(windows, events);

        self.replaying = deliver(edits, windows, events) || waits;
        self.replaying
    }

    /// When the event loop is about to wait: applies what the keyboard sent
    /// since it last turned, and gives the edits to the window whose field
    /// asks for the keyboard. Called before the loop's idle check. It then
    /// asks that window for the redraw a composition the hold keeps back
    /// waits for ([`settle`]), so the loop turns for it.
    ///
    /// True when the application takes its turn after an Enter, Tab or
    /// Escape key, which this gave the field or an earlier call did: the
    /// event loop then handles the edits, builds the views again and runs
    /// the operations, and then replays [`Shell::resume`] behind what it
    /// replays already, then an `AboutToWait`, which calls this again for
    /// the rest. During the turn this applies nothing.
    #[must_use]
    pub(crate) fn drain<P, C>(
        &mut self,
        windows: &mut WindowManager<P, C>,
        events: &mut Vec<(window::Id, core::Event)>,
    ) -> bool
    where
        P: Program,
        C: Compositor<Renderer = P::Renderer>,
        P::Theme: theme::Base,
    {
        let (edits, turn) = SESSION.with_borrow_mut(|session| {
            let mut edits = Vec::new();
            let mut calls = Vec::new();
            let turn = session.feed.drain(&mut Shared, &mut edits, &mut calls);
            session.perform(calls);

            (edits, turn)
        });

        closed(windows, events);

        let _ = deliver(edits, windows, events);

        settle(windows);

        turn
    }

    /// The application's turn after an Enter, Tab or Escape key ended: the
    /// next drain applies what the keyboard sent after it, which reaches the
    /// field the application left the focus in with no preedit until the
    /// settle window has closed, or a tap or a focus move lets a redraw tell
    /// the shell that field's purpose. The window redraws for that, also when
    /// the key changed nothing.
    pub(crate) fn resume(&mut self) {
        SESSION.with_borrow_mut(|session| session.feed.resume());

        if let Some(window) = &queue().window {
            window.request_redraw();
        }
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
    feed: Feed,
    /// The purpose the keyboard was last shown for, while a field asks for
    /// it.
    shown: Option<i32>,
    /// A press landed on the focused field (see [`touched`]), or an
    /// operation focused a widget ([`operated`]): the next redraw that still
    /// asks for the keyboard shows it again.
    touched: bool,
    /// The window whose field lost the keyboard, which gets
    /// `input_method::Event::Closed`.
    closed: Option<winit::window::WindowId>,
    /// The widget operations that wait for the field to take the keyboard's
    /// edits, in order.
    deferred: VecDeque<Box<dyn Operation>>,
    /// The event loop is to run them after its next `AboutToWait`.
    scheduled: bool,
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

        let touched = std::mem::take(&mut self.touched);

        if !self.install() {
            return false;
        }

        if self.shown.is_none() {
            // What the keyboard sent since the last field asked for it
            // belongs to no field.
            self.abandon();

            set_window(Some(window.clone()));
        }

        // The shell learns the field's purpose here. A password field never
        // shows a composition: one the keyboard composed under the last
        // purpose is committed as typed text, and one held back since the
        // focus moved shows in another field. The field gets that at the
        // next turn of the event loop, before anything else.
        let mut calls = Vec::new();

        if self.feed.secure(purpose == dex::PURPOSE_SECURE, &mut calls) {
            window.request_redraw();
        }

        self.perform(calls);

        if self.shown != Some(purpose) || touched {
            self.call(dex::SHOW, &[JValue::Int(purpose)]);
            self.shown = Some(purpose);
        }

        true
    }

    fn disable(&mut self, window: winit::window::WindowId) -> bool {
        self.touched = false;

        if !self.is_installed() {
            return false;
        }

        if self.shown.take().is_some() {
            self.abandon();
            self.call(dex::HIDE, &[]);
            set_window(None);
            self.closed = Some(window);
        }

        true
    }

    /// The field the keyboard typed into lost the focus: what the keyboard
    /// sent, typed and composes there is dropped, and its Java side starts
    /// over unless it holds nothing (see [`Feed::abandon`]).
    fn abandon(&mut self) {
        if let Some(generation) = self.feed.abandon(&mut Shared) {
            self.call(dex::RESET, &[JValue::Int(generation)]);
        }
    }

    /// The user or the application moves the caret: applies what the
    /// keyboard sent, then, unless the engine is idle, resets it, which
    /// commits a composition at the field's caret, and has the Java side
    /// forget its text ([`Feed::finish`]). Returns the edits for the field,
    /// and whether the application is to take its turn after an Enter, Tab
    /// or Escape key first, after which this applies the rest. `yields` says
    /// the event is a hardware Enter, Tab or Escape (see [`Feed::finish`]).
    fn finish(&mut self, yields: bool) -> (Vec<Edit>, bool) {
        let mut edits = Vec::new();

        if !self.is_installed() {
            return (edits, false);
        }

        let mut calls = Vec::new();
        let waits =
            self.feed
                .finish(&mut Shared, &mut edits, &mut calls, yields);
        self.perform(calls);

        (edits, waits)
    }

    fn is_installed(&self) -> bool {
        matches!(self.keyboard, Keyboard::Installed(_))
    }

    /// Whether the installed Java side is shown for a field.
    fn is_shown(&self) -> bool {
        self.is_installed() && self.shown.is_some()
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

    /// Makes the calls to the Java side the feed asked for, in order.
    fn perform(&self, calls: Vec<Call>) {
        for call in calls {
            match call {
                Call::Forget(generation) => {
                    self.call(dex::FORGET, &[JValue::Int(generation)]);
                }
                Call::Reset(generation) => {
                    self.call(dex::RESET, &[JValue::Int(generation)]);
                }
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
        self.feed = Feed::new();
        self.shown = None;
        self.touched = false;
        self.closed = None;
        self.deferred.clear();
        self.scheduled = false;
        self.app = None;
    }
}

/// Gives `input_method::Event::Closed` to the window whose field lost the
/// keyboard, if it is still open.
fn closed<P, C>(
    windows: &mut WindowManager<P, C>,
    events: &mut Vec<(window::Id, core::Event)>,
) where
    P: Program,
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    let Some(window) = SESSION.with_borrow_mut(|session| session.closed.take())
    else {
        return;
    };

    if let Some((id, _window)) = windows.get_mut_alias(window) {
        events
            .push((id, core::Event::InputMethod(input_method::Event::Closed)));
    }
}

/// Gives the edits to the window whose field asks for the keyboard, as its
/// events. Whether a window got any.
fn deliver<P, C>(
    edits: Vec<Edit>,
    windows: &mut WindowManager<P, C>,
    events: &mut Vec<(window::Id, core::Event)>,
) -> bool
where
    P: Program,
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    if edits.is_empty() {
        return false;
    }

    let Some((id, window)) = windows
        .iter_mut()
        .find(|(_id, window)| window.ime_requested())
    else {
        log::debug!(
            "Keyboard: {} edits dropped: no field asks for the keyboard",
            edits.len()
        );

        return false;
    };

    let modifiers = conversion::modifiers(window.state.modifiers());

    for edit in edits {
        super::events(edit, modifiers, |event| events.push((id, event)));
    }

    true
}

/// Asks the window whose field has the keyboard for the redraw that decides
/// a composition the hold keeps back: at the end of the settle window, or now
/// once it has closed (see [`Feed::settle_at`]). A window without a surface
/// is not drawn, and `Resumed` asks for its redraw.
fn settle<P, C>(windows: &mut WindowManager<P, C>)
where
    P: Program,
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    let Some(at) = SESSION.with_borrow(|session| session.feed.settle_at())
    else {
        return;
    };

    let Some((_id, window)) = windows
        .iter_mut()
        .find(|(_id, window)| window.ime_requested())
    else {
        return;
    };

    // An earlier request of the window, an animation's say, stays.
    if window.surface.is_some()
        && window.redraw_at.is_none_or(|redraw_at| at < redraw_at)
    {
        window.request_redraw(window::RedrawRequest::At(at));
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

/// The queue's messages, for the feed. The lock is held only to take them
/// and to put them back, never while the feed applies them or anything
/// calls into Java.
struct Shared;

impl feed::Queue for Shared {
    fn take(&mut self) -> VecDeque<Message> {
        std::mem::take(&mut queue().messages)
    }

    fn put_back(&mut self, mut rest: VecDeque<Message>) {
        let mut queue = queue();

        rest.append(&mut queue.messages);
        queue.messages = rest;
    }
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
