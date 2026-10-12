//! The keyboard's messages applied to the engine, in order, and the
//! application's turn after the keyboard's Enter, Tab or Escape: what the
//! session (`android`) decides without Java and without windows.
//!
//! The Java side queues its messages on the UI thread, and the shell
//! applies them on `android_main`'s thread:
//!
//! - [`Feed::drain`], when the event loop is about to wait;
//! - [`Feed::operate`], before a widget operation runs on the window whose
//!   field has the keyboard, since what the keyboard sent before it belongs
//!   before it;
//! - [`Feed::finish`], before a tap, a hardware key or the loss of the
//!   window's focus reaches the application, and after an operation moved
//!   the caret of the field the keyboard types into: then the engine starts
//!   over, unless it is idle;
//! - [`Feed::abandon`], when the field loses the keyboard: they are
//!   dropped.
//!
//! # The application's turn
//!
//! The keyboard's Enter, Tab and Escape can submit the field, which the
//! application may clear, or have it move the focus. What the keyboard
//! typed after such a key belongs to the field the application leaves the
//! focus in, so the feed stops after the key ([`Request::Yield`]) and the
//! rest waits while the application takes its turn:
//!
//! 1. The `AboutToWait` that hands the field the key runs: the application
//!    handles what the field published for it (`on_submit`), builds its
//!    view again, and the widget operations its `update` returns run then,
//!    at once: [`Feed::operate`] applies nothing meanwhile, and the key left
//!    the engine idle, so a focus that moves to the next field ends nothing
//!    ([`Feed::follows`]).
//! 2. The operations that wait behind earlier ones run.
//! 3. The event loop replays [`Feed::resume`], then an `AboutToWait`, whose
//!    drain applies the rest: the text typed after the key reaches the field
//!    that has the focus then.
//!
//! A drain that stops after the key starts the turn itself. When
//! [`Feed::operate`] or [`Feed::finish`] stops after it, the edits up to
//! the key go to the field and the turn is due: nothing more is applied,
//! and operations wait, until the next `AboutToWait` hands those edits to
//! the application and the turn starts. The tap or key that waited is then
//! handled again: what the keyboard sent after the key is applied first,
//! since it came before the tap.
//!
//! What the application does later than its turn comes after the rest: a
//! message from a subscription (`keyboard::listen`, the way to handle Tab,
//! which `text_input` ignores) or from a task reaches it after the event
//! loop turned again, and the text the keyboard sent right after the key is
//! in the field that had the focus by then.
use super::edit::{Edit, Engine, Message, Request};

use std::collections::VecDeque;

/// The queue the Java side fills.
pub(crate) trait Queue {
    /// Takes every message queued, oldest first.
    fn take(&mut self) -> VecDeque<Message>;

    /// Puts `rest` back, ahead of what was queued since it was taken.
    fn put_back(&mut self, rest: VecDeque<Message>);
}

/// A call to the Java side, which the shell makes after the feed returns,
/// in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Call {
    /// `Keyboard.forget(generation)`, for [`Request::Forget`].
    Forget(i32),
    /// `Keyboard.reset(generation)`: the Java side forgets its text and
    /// starts the generation.
    Reset(i32),
}

/// Where the application is in its turn after a key that yields.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Turn {
    /// No turn: the queue applies.
    #[default]
    None,
    /// Edits up to such a key went to the field, and no `AboutToWait` has
    /// handed them to the application yet: nothing applies, and operations
    /// wait.
    Due,
    /// The application handles the key: operations run at once, and the
    /// rest of the queue waits for [`Feed::resume`].
    Taken,
}

/// The engine, and the application's turn.
#[derive(Debug, Default)]
pub(crate) struct Feed {
    engine: Engine,
    turn: Turn,
}

impl Feed {
    /// A feed with nothing typed and no turn.
    pub(crate) fn new() -> Self {
        Self {
            engine: Engine::new(),
            turn: Turn::None,
        }
    }

    /// When the event loop is about to wait: applies the queue and pushes
    /// the edits for the field. True when the event loop is to replay
    /// [`Feed::resume`] behind what it replays already: the application's
    /// turn starts, after a key this applied or one applied before.
    ///
    /// During the turn it applies nothing.
    pub(crate) fn drain(
        &mut self,
        queue: &mut impl Queue,
        edits: &mut Vec<Edit>,
        calls: &mut Vec<Call>,
    ) -> bool {
        match self.turn {
            Turn::Due => {
                // This `AboutToWait` hands the application the key.
                self.turn = Turn::Taken;
                return true;
            }
            Turn::Taken => return false,
            Turn::None => {}
        }

        if self.apply(queue, edits, calls) {
            self.turn = Turn::Taken;
            return true;
        }

        false
    }

    /// The application's turn ended: the next drain applies the rest of the
    /// queue. A turn that is due stays so.
    pub(crate) fn resume(&mut self) {
        if self.turn == Turn::Taken {
            self.turn = Turn::None;
        }
    }

    /// Before a widget operation on the window whose field has the
    /// keyboard: applies the queue, and pushes the edits for the field,
    /// unless the application takes its turn, since the operation may
    /// answer the key. True when the turn is due, so the operation waits for
    /// the next `AboutToWait`, as it waits for any edits this gives.
    pub(crate) fn operate(
        &mut self,
        queue: &mut impl Queue,
        edits: &mut Vec<Edit>,
        calls: &mut Vec<Call>,
    ) -> bool {
        if self.turn == Turn::None && self.apply(queue, edits, calls) {
            self.turn = Turn::Due;
        }

        self.turn == Turn::Due
    }

    /// Before a tap, a hardware key or the loss of the window's focus
    /// reaches the application, and after an operation moved the caret of
    /// the field: applies the queue, then, unless the engine is idle, resets
    /// it, which commits a composition ([`Engine::interrupt`]), and asks the
    /// Java side to start the new generation.
    ///
    /// True when it stopped after a key that yields, or the turn after one
    /// is due: the edits up to it come first, and the event waits until
    /// the application handled them; then this applies the rest. The engine
    /// needs no reset then: the key left it idle, and the Java side rebases
    /// after it.
    pub(crate) fn finish(
        &mut self,
        queue: &mut impl Queue,
        edits: &mut Vec<Edit>,
        calls: &mut Vec<Call>,
    ) -> bool {
        if self.turn == Turn::Due {
            return true;
        }

        // A tap handled again after the application's turn goes on.
        self.turn = Turn::None;

        if self.apply(queue, edits, calls) {
            self.turn = Turn::Due;
            return true;
        }

        if let Some(generation) = self.engine.interrupt(edits) {
            calls.push(Call::Reset(generation));
        }

        false
    }

    /// The field lost the keyboard: what is queued is dropped with what the
    /// engine knows. Returns the generation for `Keyboard.reset`, when the
    /// Java side may hold text ([`Engine::abandon`]).
    pub(crate) fn abandon(&mut self, queue: &mut impl Queue) -> Option<i32> {
        let mut dropped = queue.take();

        self.engine.abandon(dropped.make_contiguous())
    }

    /// Whether the application takes its turn after a key that yields: a
    /// tap handled again then applies the rest before it.
    pub(crate) fn takes_turn(&self) -> bool {
        self.turn == Turn::Taken
    }

    /// Whether a focus or a caret an operation moves in the field ends what
    /// the keyboard typed ([`Engine::follows_caret`]).
    pub(crate) fn follows(&self) -> bool {
        self.engine.follows_caret()
    }

    /// Whether the caret moves an operation makes in the field wait until
    /// the field has the composition: the keyboard composes.
    pub(crate) fn holds(&self) -> bool {
        self.engine.is_composing()
    }

    /// Applies the queued messages in order; stops after a key that yields
    /// and puts the rest back. True when it stopped so.
    fn apply(
        &mut self,
        queue: &mut impl Queue,
        edits: &mut Vec<Edit>,
        calls: &mut Vec<Call>,
    ) -> bool {
        let mut messages = queue.take();

        while let Some(message) = messages.pop_front() {
            match self.engine.apply(message, edits) {
                Request::None => {}
                Request::Yield => {
                    if !messages.is_empty() {
                        queue.put_back(messages);
                    }

                    return true;
                }
                Request::Forget => {
                    calls.push(Call::Forget(self.engine.generation()));
                }
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ime::edit::{KEYCODE_ENTER, KEYCODE_TAB, Key};

    impl Queue for VecDeque<Message> {
        fn take(&mut self) -> VecDeque<Message> {
            std::mem::take(self)
        }

        fn put_back(&mut self, mut rest: VecDeque<Message>) {
            rest.append(self);
            *self = rest;
        }
    }

    /// The keyboard's text, `text`, with the cursor at its end, composing
    /// it all or none of it.
    fn state(text: &str, composing: bool) -> Message {
        let end = i32::try_from(text.encode_utf16().count()).unwrap();

        Message::State {
            generation: 0,
            text: text.to_owned(),
            selection: (end, end),
            composing: if composing { (0, end) } else { (-1, -1) },
        }
    }

    fn key(code: i32) -> Message {
        Message::Key {
            generation: 0,
            code,
        }
    }

    fn rebase() -> Message {
        Message::Rebase { generation: 0 }
    }

    fn commit(text: &str) -> Edit {
        Edit::Commit(text.to_owned())
    }

    fn preedit(text: &str) -> Edit {
        Edit::Preedit(text.to_owned(), Some(text.len()..text.len()))
    }

    fn clear() -> Edit {
        Edit::Preedit(String::new(), None)
    }

    /// What a call gives: its edits, its answer, and the Java calls.
    struct Fed {
        edits: Vec<Edit>,
        answer: bool,
        calls: Vec<Call>,
    }

    fn fed(
        feed: &mut Feed,
        queue: &mut VecDeque<Message>,
        call: fn(
            &mut Feed,
            &mut VecDeque<Message>,
            &mut Vec<Edit>,
            &mut Vec<Call>,
        ) -> bool,
    ) -> Fed {
        let mut edits = Vec::new();
        let mut calls = Vec::new();
        let answer = call(feed, queue, &mut edits, &mut calls);

        Fed {
            edits,
            answer,
            calls,
        }
    }

    fn drain(feed: &mut Feed, queue: &mut VecDeque<Message>) -> Fed {
        fed(feed, queue, |feed, queue, edits, calls| {
            feed.drain(queue, edits, calls)
        })
    }

    fn operate(feed: &mut Feed, queue: &mut VecDeque<Message>) -> Fed {
        fed(feed, queue, |feed, queue, edits, calls| {
            feed.operate(queue, edits, calls)
        })
    }

    fn finish(feed: &mut Feed, queue: &mut VecDeque<Message>) -> Fed {
        fed(feed, queue, |feed, queue, edits, calls| {
            feed.finish(queue, edits, calls)
        })
    }

    /// A form of two fields like `text_input`, the first focused: a commit
    /// goes in at the end of the focused one (the caret stays there), a
    /// preedit shows in it, Backspace deletes, and Enter submits it, which
    /// the application may answer by clearing it.
    #[derive(Debug, Default)]
    struct Form {
        fields: [String; 2],
        preedit: String,
        focus: usize,
        /// What each Enter submitted.
        submitted: Vec<String>,
        /// The application clears the field it submits.
        clears: bool,
    }

    impl Form {
        /// The application handles the edits a window got, in order.
        fn handle(&mut self, edits: &[Edit]) {
            for edit in edits {
                let field = &mut self.fields[self.focus];

                match edit {
                    Edit::Commit(text) => field.push_str(text),
                    Edit::Preedit(text, _) => self.preedit.clone_from(text),
                    Edit::Key(Key::Backspace) => {
                        let _ = field.pop();
                    }
                    Edit::Key(Key::Enter) => {
                        self.submitted.push(field.clone());

                        if self.clears {
                            field.clear();
                        }
                    }
                    Edit::Key(_) => {}
                }
            }
        }

        /// The application moves the focus to the next field: the field
        /// the focus leaves drops its preedit.
        fn focus_next(&mut self) {
            self.focus = (self.focus + 1) % self.fields.len();
            self.preedit.clear();
        }
    }

    #[test]
    fn the_field_focused_at_enter_gets_what_is_typed_after_it() {
        for composes in [false, true] {
            // A login form whose `on_submit` returns `focus_next`: the user
            // types the username, Enter and the password's first letter
            // before the event loop turns.
            let mut feed = Feed::new();
            let mut form = Form::default();
            let mut queue = VecDeque::from([
                state("sam", false),
                key(KEYCODE_ENTER),
                rebase(),
                state("p", composes),
            ]);

            // The drain stops after Enter and starts the turn.
            let fed = drain(&mut feed, &mut queue);
            assert!(fed.answer, "a resume is replayed");
            assert_eq!(fed.edits, [commit("sam"), Edit::Key(Key::Enter)]);
            form.handle(&fed.edits);
            assert_eq!(form.submitted, ["sam"]);

            // Another letter arrives while the application handles it.
            queue.push_back(state("pw", composes));

            // `update` answers with `focus_next`, which runs at once, before
            // the rest, and ends nothing: the key left the engine idle.
            let fed = operate(&mut feed, &mut queue);
            assert!(
                !fed.answer && fed.edits.is_empty() && fed.calls.is_empty()
            );
            assert!(!feed.follows() && !feed.holds());
            form.focus_next();

            // An `AboutToWait` before the resume applies nothing.
            let fed = drain(&mut feed, &mut queue);
            assert!(!fed.answer && fed.edits.is_empty());
            assert!(feed.takes_turn());

            // The resume: the rest goes to the password field.
            feed.resume();
            let fed = drain(&mut feed, &mut queue);
            assert!(!fed.answer && fed.calls.is_empty());
            form.handle(&fed.edits);

            assert!(queue.is_empty());
            assert_eq!(form.fields, ["sam", if composes { "" } else { "pw" }]);
            assert_eq!(form.preedit, if composes { "pw" } else { "" });
        }
    }

    #[test]
    fn a_key_that_ends_the_queue_still_gives_the_application_its_turn() {
        // A credential keyboard sends a username, Tab, then the password,
        // which arrives only after the drain.
        let mut feed = Feed::new();
        let mut form = Form::default();
        let mut queue = VecDeque::from([state("sam", false), key(KEYCODE_TAB)]);

        let fed = drain(&mut feed, &mut queue);
        assert!(fed.answer);
        assert_eq!(fed.edits, [commit("sam"), Edit::Key(Key::Tab)]);
        form.handle(&fed.edits);

        queue.extend([rebase(), state("pw", false)]);

        // The application moves the focus in its turn.
        let fed = operate(&mut feed, &mut queue);
        assert!(!fed.answer && fed.edits.is_empty());
        form.focus_next();

        feed.resume();
        let fed = drain(&mut feed, &mut queue);
        form.handle(&fed.edits);
        assert_eq!(form.fields, ["sam", "pw"]);
    }

    #[test]
    fn an_operation_waits_for_the_turn_of_a_key_it_applies() {
        // A chat field whose `on_submit` sends and clears it; an operation
        // from a task arrives while the queue holds "hi", Enter and "x".
        let mut feed = Feed::new();
        let mut form = Form {
            clears: true,
            ..Form::default()
        };
        let mut queue = VecDeque::from([
            state("hi", false),
            key(KEYCODE_ENTER),
            rebase(),
            state("x", false),
        ]);

        // What came before the operation goes to the field, up to Enter;
        // the operation waits for the next `AboutToWait`.
        let fed = operate(&mut feed, &mut queue);
        assert!(fed.answer);
        assert_eq!(fed.edits, [commit("hi"), Edit::Key(Key::Enter)]);
        let given = fed.edits;

        // Operations made meanwhile wait too, behind it.
        let fed = operate(&mut feed, &mut queue);
        assert!(fed.answer && fed.edits.is_empty());

        // The `AboutToWait` hands the application the edits, and nothing
        // more, and starts its turn.
        let fed = drain(&mut feed, &mut queue);
        assert!(fed.answer && fed.edits.is_empty());
        form.handle(&given);
        assert_eq!(form.submitted, ["hi"]);
        assert_eq!(form.fields[0], "");

        // The operations that waited run, at once.
        let fed = operate(&mut feed, &mut queue);
        assert!(!fed.answer && fed.edits.is_empty());

        // After the resume, the cleared field gets "x" alone.
        feed.resume();
        let fed = drain(&mut feed, &mut queue);
        form.handle(&fed.edits);
        assert_eq!(form.fields[0], "x");
    }

    #[test]
    fn a_tap_waits_for_the_turn_of_a_key_queued_before_it() {
        // A tap lands while the queue holds "hi", Enter and "x" being
        // composed, in a field cleared on submit.
        let mut feed = Feed::new();
        let mut form = Form {
            clears: true,
            ..Form::default()
        };
        let mut queue = VecDeque::from([
            state("hi", false),
            key(KEYCODE_ENTER),
            rebase(),
            state("x", true),
        ]);

        // The edits up to Enter come first, and the tap waits.
        let fed = finish(&mut feed, &mut queue);
        assert!(fed.answer && fed.calls.is_empty());
        assert_eq!(fed.edits, [commit("hi"), Edit::Key(Key::Enter)]);
        let given = fed.edits;

        // The `AboutToWait` replayed for them starts the turn.
        let fed = drain(&mut feed, &mut queue);
        assert!(fed.answer && fed.edits.is_empty());
        form.handle(&given);
        assert_eq!(form.submitted, ["hi"]);

        // The tap, handled again, applies the rest and commits the
        // composition before it, then starts a generation.
        assert!(feed.takes_turn());
        let fed = finish(&mut feed, &mut queue);
        assert!(!fed.answer);
        assert_eq!(fed.edits, [preedit("x"), clear(), commit("x")]);
        assert_eq!(fed.calls, [Call::Reset(1)]);
        form.handle(&fed.edits);
        assert_eq!(form.fields[0], "x");
        assert_eq!(form.preedit, "");

        // The resume replayed for the turn finds nothing more.
        feed.resume();
        let fed = drain(&mut feed, &mut queue);
        assert!(!fed.answer && fed.edits.is_empty());
    }

    #[test]
    fn a_tap_after_the_last_key_queued_goes_on_after_the_turn() {
        let mut feed = Feed::new();
        let mut queue =
            VecDeque::from([state("hi", false), key(KEYCODE_ENTER)]);

        let fed = finish(&mut feed, &mut queue);
        assert!(fed.answer);
        assert_eq!(fed.edits, [commit("hi"), Edit::Key(Key::Enter)]);

        let fed = drain(&mut feed, &mut queue);
        assert!(fed.answer && fed.edits.is_empty());

        // Nothing more: the idle engine needs no reset, and the tap goes on.
        let fed = finish(&mut feed, &mut queue);
        assert!(!fed.answer && fed.edits.is_empty() && fed.calls.is_empty());
        assert!(!feed.takes_turn());
    }

    #[test]
    fn an_abandoned_field_drops_the_queue() {
        let mut feed = Feed::new();
        let mut queue = VecDeque::from([state("wor", true)]);

        // The first report is still queued: the Java side holds its text.
        assert_eq!(feed.abandon(&mut queue), Some(1));
        assert!(queue.is_empty());
        assert!(!feed.follows());

        // Nothing queued and nothing typed: nothing to reset.
        assert_eq!(feed.abandon(&mut queue), None);
    }
}
