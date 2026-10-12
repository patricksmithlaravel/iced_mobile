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
//!
//! # Password fields
//!
//! The shell learns the purpose of the field that asks for the keyboard
//! only when the window redraws, and [`Feed::secure`] then tells the
//! engine whether it is a password field, which never shows a composition
//! ([`Engine::secure`]). The rest that the resume applies reaches the field
//! the application focused in its turn before that redraw, and so may what
//! the keyboard sends after an operation moved the focus, or after a tap
//! landed on the field that has it ([`Feed::refocused`]): that field may be
//! a password field (an `on_submit` that focuses it, or a tap on one), and
//! the keyboard composed for the field that had the focus. Until the
//! redraw, the field gets what the keyboard commits but no preedit: the
//! engine holds the composition back, and the
//! redraw commits it as typed text in a password field, or gives it to
//! another field as its preedit, which the next drain, operation or
//! interruption hands the field before anything else. A composition after
//! the keyboard's Enter, Tab or Escape so shows a redraw later than
//! without the turn.
//!
//! A field that becomes a password field while the engine shows a
//! composition in it (the application made it secure) gets the
//! composition committed as typed text at that redraw too, and the window
//! draws no preedit over a password field. From then on the engine reads
//! what the keyboard composes as typed text, also the reports it sends
//! before it learns the purpose.
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
    /// Edits the field has not had yet, which come before any other: a
    /// composition committed when the field became a password field, or
    /// shown once the shell learnt that it is none ([`Feed::secure`]).
    pending: Vec<Edit>,
    /// The focus may have moved to a field whose purpose the shell learns
    /// only at the next redraw: the application took its turn after a key
    /// that yields, or an operation moved the focus, or a tap landed on the
    /// field that has it ([`Feed::refocused`]).
    /// Until that redraw ([`Feed::secure`]) the field gets no preedit: the
    /// engine still shows the keyboard's composition, and the redraw gives
    /// it to the field, or commits it as typed text in a password field.
    unknown: bool,
}

impl Feed {
    /// A feed with nothing typed and no turn.
    pub(crate) fn new() -> Self {
        Self {
            engine: Engine::new(),
            turn: Turn::None,
            pending: Vec::new(),
            unknown: false,
        }
    }

    /// At a redraw, the purpose the field that asks for the keyboard has:
    /// whether it is a password field. One never shows a composition
    /// ([`Engine::secure`]): a preedit the engine shows when the field
    /// becomes one is committed as typed text. After a turn or a focus
    /// moved, the composition held back from the field until now is shown
    /// in it, unless it was so committed. The next drain, operation or
    /// interruption gives the field these edits before anything else. True
    /// when there are any: the event loop is to turn for them.
    pub(crate) fn secure(
        &mut self,
        secure: bool,
        calls: &mut Vec<Call>,
    ) -> bool {
        let mut edits = Vec::new();

        if self.engine.secure(secure, &mut edits) == Request::Forget {
            calls.push(Call::Forget(self.engine.generation()));
        }

        if std::mem::take(&mut self.unknown) {
            // The field shows no preedit: none to clear, and the one the
            // engine shows still, if any, is the field's now.
            edits.retain(|edit| !matches!(edit, Edit::Preedit(..)));
            edits.extend(self.engine.preedit());
        }

        let given = !edits.is_empty();
        self.pending.append(&mut edits);

        given
    }

    /// An operation moved the focus, or a press landed on the field that has
    /// it (a tap, which focuses a `text_input` by itself), while the
    /// keyboard is shown: the field that has it now gets no preedit until
    /// the next redraw tells the shell its purpose ([`Feed::secure`]).
    pub(crate) fn refocused(&mut self) {
        self.unknown = true;
    }

    /// When the event loop is about to wait: applies the queue and pushes
    /// the edits for the field. True when the event loop is to replay
    /// [`Feed::resume`] behind what it replays already: the application's
    /// turn starts, after a key this applied or one applied before.
    ///
    /// During the turn it applies nothing. The edits the field has not had
    /// yet ([`Feed::secure`]) come first, whatever the turn.
    pub(crate) fn drain(
        &mut self,
        queue: &mut impl Queue,
        edits: &mut Vec<Edit>,
        calls: &mut Vec<Call>,
    ) -> bool {
        edits.append(&mut self.pending);

        match self.turn {
            Turn::Due => {
                // This `AboutToWait` hands the application the key.
                self.turn = Turn::Taken;
                return true;
            }
            Turn::Taken => return false,
            Turn::None => {}
        }

        let from = edits.len();
        let turn = self.apply(queue, edits, calls);
        self.hold(edits, from);

        if turn {
            self.turn = Turn::Taken;
        }

        turn
    }

    /// The application's turn ended: the next drain applies the rest of the
    /// queue, which reaches the field that has the focus now, with no
    /// preedit until the next redraw ([`Feed::secure`]). A turn that is due
    /// stays so.
    pub(crate) fn resume(&mut self) {
        if self.turn == Turn::Taken {
            self.turn = Turn::None;
            self.unknown = true;
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
        edits.append(&mut self.pending);

        let from = edits.len();

        if self.turn == Turn::None && self.apply(queue, edits, calls) {
            self.turn = Turn::Due;
        }

        self.hold(edits, from);
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
        edits.append(&mut self.pending);

        if self.turn == Turn::Due {
            return true;
        }

        // A tap handled again after the application's turn goes on, in the
        // field that has the focus now.
        if self.turn == Turn::Taken {
            self.turn = Turn::None;
            self.unknown = true;
        }

        let from = edits.len();
        let waits = self.apply(queue, edits, calls);

        if waits {
            self.turn = Turn::Due;
        } else if let Some(generation) = self.engine.interrupt(edits) {
            calls.push(Call::Reset(generation));
        }

        self.hold(edits, from);
        waits
    }

    /// The field lost the keyboard: what is queued is dropped with what the
    /// engine knows and the edits the field has not had. Returns the
    /// generation for `Keyboard.reset`, when the Java side may hold text
    /// ([`Engine::abandon`]).
    pub(crate) fn abandon(&mut self, queue: &mut impl Queue) -> Option<i32> {
        let mut dropped = queue.take();
        self.pending.clear();

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
    /// the field has what the keyboard sent meanwhile: the engine is not
    /// idle. Its edits are worked out from the caret it knows, so a message
    /// that arrives while the operation runs must reach the field before
    /// the caret moves. An idle engine's edits act at the field's caret,
    /// wherever it is.
    pub(crate) fn holds(&self) -> bool {
        !self.engine.is_idle()
    }

    /// Holds back the preedits among `edits[from..]` while the purpose of
    /// the field is unknown ([`Feed::unknown`]): the engine still shows
    /// them, and the next redraw gives the field the last one.
    fn hold(&self, edits: &mut Vec<Edit>, from: usize) {
        if self.unknown {
            let applied = edits.split_off(from);

            edits.extend(
                applied
                    .into_iter()
                    .filter(|edit| !matches!(edit, Edit::Preedit(..))),
            );
        }
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

    use crate::ime::edit::{KEYCODE_ENTER, KEYCODE_ESCAPE, KEYCODE_TAB, Key};

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
    /// it all or none of it, in the first generation.
    fn state(text: &str, composing: bool) -> Message {
        state_in(0, text, composing)
    }

    /// `state` in `generation`, which a reset starts.
    fn state_in(generation: i32, text: &str, composing: bool) -> Message {
        let end = i32::try_from(text.encode_utf16().count()).unwrap();

        Message::State {
            generation,
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
        /// Which fields are password fields (`.secure(true)`).
        secure: [bool; 2],
        /// The preedits the window drew over a field, and the field.
        drawn: Vec<(usize, String)>,
        /// Every preedit a field got, and the field.
        got: Vec<(usize, String)>,
    }

    impl Form {
        /// The application handles the edits a window got, in order.
        fn handle(&mut self, edits: &[Edit]) {
            for edit in edits {
                let field = &mut self.fields[self.focus];

                match edit {
                    Edit::Commit(text) => field.push_str(text),
                    Edit::Preedit(text, _) => {
                        self.preedit.clone_from(text);

                        if !text.is_empty() {
                            self.got.push((self.focus, text.clone()));
                        }
                    }
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

        /// The window redraws: the focused field asks for the keyboard with
        /// its purpose, which the feed learns (`android::enable`), and the
        /// window draws the preedit over it as Android's does
        /// (`window::overlay`). Whether the feed committed a composition
        /// for the field, which the event loop turns for.
        fn redraw(&mut self, feed: &mut Feed) -> bool {
            use crate::core::input_method::{Preedit, Purpose};

            let secure = self.secure[self.focus];
            let mut calls = Vec::new();
            let committed = feed.secure(secure, &mut calls);
            assert_eq!(calls, []);

            let purpose = if secure {
                Purpose::Secure
            } else {
                Purpose::Normal
            };

            let preedit = Preedit {
                content: self.preedit.clone(),
                selection: None,
                text_size: None,
            };

            if let Some(drawn) =
                crate::window::overlay(purpose, Some(&preedit), true)
            {
                self.drawn.push((self.focus, drawn.content.clone()));
            }

            committed
        }
    }

    #[test]
    fn the_field_focused_at_enter_gets_what_is_typed_after_it() {
        for (code, pressed) in [
            (KEYCODE_ENTER, Key::Enter),
            (KEYCODE_TAB, Key::Tab),
            (KEYCODE_ESCAPE, Key::Escape),
        ] {
            for secure in [false, true] {
                for composes in [false, true] {
                    let label = format!("{code} {secure} {composes}");

                    // A login form whose `on_submit` (or the widget that
                    // takes Tab or Escape) moves the focus to the next
                    // field: the user types the username, the key and the
                    // password's first letter before the event loop turns.
                    let mut feed = Feed::new();
                    let mut form = Form {
                        secure: [false, secure],
                        ..Form::default()
                    };
                    let mut queue = VecDeque::from([
                        state("sam", false),
                        key(code),
                        rebase(),
                        state("p", composes),
                    ]);
                    assert!(!form.redraw(&mut feed));

                    // The drain stops after the key and starts the turn.
                    let fed = drain(&mut feed, &mut queue);
                    assert!(fed.answer, "{label}: a resume is replayed");
                    assert_eq!(
                        fed.edits,
                        [commit("sam"), Edit::Key(pressed)],
                        "{label}"
                    );
                    form.handle(&fed.edits);
                    assert_eq!(
                        form.submitted,
                        if code == KEYCODE_ENTER {
                            vec!["sam"]
                        } else {
                            vec![]
                        },
                        "{label}"
                    );

                    // Another letter arrives while the application handles
                    // it.
                    queue.push_back(state("pw", composes));

                    // `update` answers with `focus_next`, which runs at
                    // once, before the rest, and ends nothing: the key left
                    // the engine idle.
                    let fed = operate(&mut feed, &mut queue);
                    assert!(
                        !fed.answer
                            && fed.edits.is_empty()
                            && fed.calls.is_empty(),
                        "{label}"
                    );
                    assert!(!feed.follows() && !feed.holds(), "{label}");
                    form.focus_next();

                    // An `AboutToWait` before the resume applies nothing.
                    let fed = drain(&mut feed, &mut queue);
                    assert!(!fed.answer && fed.edits.is_empty(), "{label}");
                    assert!(feed.takes_turn(), "{label}");

                    // The resume: the rest goes to the second field, before
                    // the redraw that tells the shell its purpose. What the
                    // keyboard commits reaches it; what it composes, for
                    // the first field's purpose, is held back.
                    feed.resume();
                    let fed = drain(&mut feed, &mut queue);
                    assert!(!fed.answer && fed.calls.is_empty(), "{label}");
                    assert_eq!(
                        fed.edits,
                        if composes {
                            vec![]
                        } else {
                            vec![commit("p"), commit("w")]
                        },
                        "{label}"
                    );
                    form.handle(&fed.edits);
                    assert!(queue.is_empty(), "{label}");

                    // The redraw tells the shell the field's purpose: the
                    // composition is the password field's typed text, or
                    // the normal field's preedit, which the next
                    // `AboutToWait` gives the field first.
                    assert_eq!(form.redraw(&mut feed), composes, "{label}");

                    if secure {
                        // The keyboard learns the purpose: it finishes the
                        // composition, reports its text with nothing
                        // composing and rebases (`KeyboardView.show`), which
                        // changes nothing.
                        queue.extend([state("pw", false), rebase()]);
                    }

                    let fed = drain(&mut feed, &mut queue);
                    assert!(!fed.answer && fed.calls.is_empty(), "{label}");
                    assert_eq!(
                        fed.edits,
                        match (composes, secure) {
                            (false, _) => vec![],
                            (true, false) => vec![preedit("pw")],
                            (true, true) => vec![commit("pw")],
                        },
                        "{label}"
                    );
                    form.handle(&fed.edits);
                    assert!(!form.redraw(&mut feed), "{label}");

                    // A normal field shows the composition; a password field
                    // has it as typed text, never as its preedit.
                    let shown = composes && !secure;
                    assert_eq!(
                        form.got,
                        if shown {
                            vec![(1, "pw".to_owned())]
                        } else {
                            vec![]
                        },
                        "{label}"
                    );
                    assert_eq!(
                        form.fields,
                        ["sam", if shown { "" } else { "pw" }],
                        "{label}"
                    );
                    assert_eq!(
                        form.preedit,
                        if shown { "pw" } else { "" },
                        "{label}"
                    );
                    assert_eq!(
                        form.drawn,
                        if shown {
                            vec![(1, "pw".to_owned())]
                        } else {
                            vec![]
                        },
                        "{label}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_password_field_reads_what_the_keyboard_composed_before_it_learnt_so() {
        // A login form whose `on_submit` focuses the password field: the
        // keyboard goes on composing for the username's purpose until it
        // runs the `show` the redraw posts.
        let mut feed = Feed::new();
        let mut form = Form {
            secure: [false, true],
            ..Form::default()
        };
        let mut queue = VecDeque::from([
            state("sam", false),
            key(KEYCODE_ENTER),
            rebase(),
            state("pw", true),
        ]);
        let _ = form.redraw(&mut feed);

        let fed = drain(&mut feed, &mut queue);
        form.handle(&fed.edits);
        let _ = operate(&mut feed, &mut queue);
        form.focus_next();
        feed.resume();
        let fed = drain(&mut feed, &mut queue);
        assert_eq!(fed.edits, []);

        // The redraw commits "pw"; the keyboard composes "pwd" meanwhile.
        assert!(form.redraw(&mut feed));
        queue.push_back(state("pwd", true));

        // The field gets "pw", then "d" as typed text: no preedit.
        let fed = drain(&mut feed, &mut queue);
        assert_eq!(fed.edits, [commit("pw"), commit("d")]);
        form.handle(&fed.edits);

        // A revision too, worked out as typed text.
        queue.push_back(state("pwe", true));
        let fed = drain(&mut feed, &mut queue);
        assert_eq!(
            fed.edits,
            [
                Edit::Key(Key::Previous),
                Edit::Key(Key::Delete),
                commit("e")
            ]
        );

        // The keyboard learns the purpose: its report changes nothing, and
        // its rebase ends the session's text where it is. The keyboard's
        // next letter is the password's.
        queue.extend([state("pwe", false), rebase(), state("x", false)]);
        let fed = drain(&mut feed, &mut queue);
        assert_eq!(fed.edits, [commit("x")]);
        assert!(fed.calls.is_empty());
        assert_eq!(form.drawn, []);
        assert_eq!(form.got, []);
    }

    #[test]
    fn a_focus_moved_from_code_holds_compositions_back_until_the_redraw() {
        for secure in [false, true] {
            // A task focuses the second field while the keyboard is idle,
            // and the keyboard composes before the window redraws.
            let mut feed = Feed::new();
            let mut form = Form {
                secure: [false, secure],
                ..Form::default()
            };
            let mut queue = VecDeque::new();
            assert!(!form.redraw(&mut feed));

            let fed = operate(&mut feed, &mut queue);
            assert!(!fed.answer && fed.edits.is_empty(), "{secure}");
            assert!(!feed.follows(), "{secure}");
            form.focus_next();
            feed.refocused();

            queue.extend([state("p", true), state("pw", true)]);
            let fed = drain(&mut feed, &mut queue);
            assert_eq!(fed.edits, [], "{secure}");

            // The redraw: typed text for a password field, a preedit for
            // another.
            assert!(form.redraw(&mut feed), "{secure}");
            let fed = drain(&mut feed, &mut queue);
            assert_eq!(
                fed.edits,
                if secure {
                    [commit("pw")]
                } else {
                    [preedit("pw")]
                },
                "{secure}"
            );
            form.handle(&fed.edits);
            assert!(!form.redraw(&mut feed), "{secure}");

            assert_eq!(
                (form.fields[1].as_str(), form.preedit.as_str()),
                if secure { ("pw", "") } else { ("", "pw") },
                "{secure}"
            );
            assert_eq!(
                form.drawn,
                if secure {
                    vec![]
                } else {
                    vec![(1, "pw".to_owned())]
                },
                "{secure}"
            );
            assert_eq!(form.got, form.drawn, "{secure}");

            // Once the shell knows the purpose, compositions go on as
            // before: typed text, or the preedit at once.
            queue.push_back(state("pwd", true));
            let fed = drain(&mut feed, &mut queue);
            assert_eq!(
                fed.edits,
                if secure {
                    [commit("d")]
                } else {
                    [preedit("pwd")]
                },
                "{secure}"
            );
        }
    }

    #[test]
    fn a_tap_onto_a_field_holds_compositions_back_until_the_redraw() {
        for secure in [false, true] {
            // The keyboard composes in the first field. A tap there commits
            // the composition (its interruption, `finish`), and lands on the
            // second field, which the application focuses: the press
            // refocuses it, as `touched` does. The keyboard composes into
            // the second field before the window redraws.
            let mut feed = Feed::new();
            let mut form = Form {
                secure: [false, secure],
                ..Form::default()
            };
            let mut queue = VecDeque::from([state("s", true)]);
            assert!(!form.redraw(&mut feed));

            let fed = drain(&mut feed, &mut queue);
            assert_eq!(fed.edits, [preedit("s")], "{secure}");
            form.handle(&fed.edits);

            let fed = finish(&mut feed, &mut queue);
            assert!(!fed.answer, "{secure}");
            assert_eq!(fed.edits, [clear(), commit("s")], "{secure}");
            assert_eq!(fed.calls, [Call::Reset(1)], "{secure}");
            form.handle(&fed.edits);

            form.focus_next();
            feed.refocused();

            queue.extend([state_in(1, "p", true), state_in(1, "pw", true)]);
            let fed = drain(&mut feed, &mut queue);
            assert_eq!(fed.edits, [], "{secure}");

            // The redraw: typed text for a password field, a preedit for
            // another.
            assert!(form.redraw(&mut feed), "{secure}");
            let fed = drain(&mut feed, &mut queue);
            assert_eq!(
                fed.edits,
                if secure {
                    [commit("pw")]
                } else {
                    [preedit("pw")]
                },
                "{secure}"
            );
            form.handle(&fed.edits);
            assert!(!form.redraw(&mut feed), "{secure}");

            assert_eq!(
                (form.fields[1].as_str(), form.preedit.as_str()),
                if secure { ("pw", "") } else { ("", "pw") },
                "{secure}"
            );
            assert_eq!(
                form.drawn,
                if secure {
                    vec![]
                } else {
                    vec![(1, "pw".to_owned())]
                },
                "{secure}"
            );

            queue.push_back(state_in(1, "pwd", true));
            let fed = drain(&mut feed, &mut queue);
            assert_eq!(
                fed.edits,
                if secure {
                    [commit("d")]
                } else {
                    [preedit("pwd")]
                },
                "{secure}"
            );
        }
    }

    #[test]
    fn a_subscription_tab_drops_the_composition_it_leaves_open() {
        // A credential keyboard sends a username, Tab, then composes the
        // password. The application takes Tab from a subscription, so the
        // composition is applied before the focus moves: the focus move
        // ends the session while the engine follows the caret, which drops
        // the composition (`abandon`), as any focus the application moves
        // does. Nothing is committed to either field.
        let mut feed = Feed::new();
        let mut form = Form {
            secure: [false, true],
            ..Form::default()
        };
        let mut queue = VecDeque::from([state("sam", false), key(KEYCODE_TAB)]);

        let fed = drain(&mut feed, &mut queue);
        assert!(fed.answer);
        assert_eq!(fed.edits, [commit("sam"), Edit::Key(Key::Tab)]);
        form.handle(&fed.edits);

        // The application's turn does nothing with the key, and ends.
        queue.extend([rebase(), state("p", true), state("pw", true)]);
        feed.resume();
        let fed = drain(&mut feed, &mut queue);
        assert!(!fed.answer && fed.edits.is_empty());

        // The subscription's message moves the focus.
        let fed = operate(&mut feed, &mut queue);
        assert!(!fed.answer && fed.edits.is_empty());
        assert!(feed.follows());
        assert_eq!(feed.abandon(&mut queue), Some(1));
        assert!(queue.is_empty());
        form.focus_next();

        // The password field gets no composition afterwards.
        assert!(!form.redraw(&mut feed));
        let fed = drain(&mut feed, &mut queue);
        assert!(!fed.answer && fed.edits.is_empty());
        form.handle(&fed.edits);
        assert_eq!(form.fields, ["sam", ""]);
        assert!(form.got.is_empty());
    }

    #[test]
    fn a_composition_that_waits_for_the_field_comes_before_any_other_edit() {
        // The application made the field secure while the keyboard composed
        // "pw": the redraw committed it, and a tap, or an operation, comes
        // before the next `AboutToWait`.
        for tap in [false, true] {
            let mut feed = Feed::new();
            let mut queue = VecDeque::from([state("pw", true)]);
            let _ = drain(&mut feed, &mut queue);

            let mut calls = Vec::new();
            assert!(feed.secure(true, &mut calls));
            assert_eq!(calls, []);

            // Again at the next redraw: nothing more.
            assert!(!feed.secure(true, &mut calls));

            queue.push_back(state("pwd", true));

            let fed = if tap {
                finish(&mut feed, &mut queue)
            } else {
                operate(&mut feed, &mut queue)
            };

            assert!(!fed.answer, "{tap}");
            assert_eq!(
                fed.edits,
                [clear(), commit("pw"), commit("d")],
                "{tap}"
            );
            assert_eq!(
                fed.calls,
                if tap { vec![Call::Reset(1)] } else { vec![] },
                "{tap}"
            );

            // Given once.
            let fed = drain(&mut feed, &mut queue);
            assert!(fed.edits.is_empty(), "{tap}");
        }

        // The field loses the keyboard first: it is dropped, with the rest.
        let mut feed = Feed::new();
        let mut queue = VecDeque::from([state("pw", true)]);
        let _ = drain(&mut feed, &mut queue);
        assert!(feed.secure(true, &mut Vec::new()));
        assert_eq!(feed.abandon(&mut queue), Some(1));
        assert!(drain(&mut feed, &mut queue).edits.is_empty());
    }

    #[test]
    fn a_field_that_is_no_longer_a_password_field_shows_compositions_again() {
        let mut feed = Feed::new();
        let mut queue = VecDeque::from([state("ab", true)]);

        // While the field is a password field, a composition is typed text.
        assert!(!feed.secure(true, &mut Vec::new()));
        assert_eq!(drain(&mut feed, &mut queue).edits, [commit("ab")]);

        // The application shows the password: nothing to commit, and the
        // keyboard's next composition is a preedit once it learnt so.
        assert!(!feed.secure(false, &mut Vec::new()));
        queue.extend([rebase(), state("c", true)]);
        assert_eq!(drain(&mut feed, &mut queue).edits, [preedit("c")]);
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
        // composition before it, then starts a generation. The field the
        // turn left the focus in never shows the composition: no redraw has
        // told the shell its purpose yet.
        assert!(feed.takes_turn());
        let fed = finish(&mut feed, &mut queue);
        assert!(!fed.answer);
        assert_eq!(fed.edits, [commit("x")]);
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
    fn caret_moves_wait_whenever_the_keyboard_has_typed() {
        let mut feed = Feed::new();
        let mut queue = VecDeque::new();
        assert!(!feed.holds());

        // The keyboard commits each letter: nothing composes, but the
        // engine works out its edits from the caret it knows.
        queue.push_back(state("world", false));
        let _ = drain(&mut feed, &mut queue);
        assert!(feed.follows() && feed.holds());

        // An operation that moves the caret: nothing is queued before it,
        // and its moves wait.
        let fed = operate(&mut feed, &mut queue);
        assert!(!fed.answer && fed.edits.is_empty());
        assert!(feed.holds());

        // A letter arrives while it runs: it reaches the field at the caret
        // the engine knows, before the moves are made, and the engine starts
        // over.
        queue.push_back(state("worlds", false));
        let fed = finish(&mut feed, &mut queue);
        assert!(!fed.answer);
        assert_eq!(fed.edits, [commit("s")]);
        assert_eq!(fed.calls, [Call::Reset(1)]);
        assert!(!feed.holds() && !feed.follows());

        // A revision too: worked out from the caret the engine knows.
        queue.push_back(Message::State {
            generation: 1,
            text: "wrold".to_owned(),
            selection: (5, 5),
            composing: (-1, -1),
        });
        let _ = drain(&mut feed, &mut queue);
        queue.push_back(Message::State {
            generation: 1,
            text: "world".to_owned(),
            selection: (5, 5),
            composing: (-1, -1),
        });
        assert!(feed.holds());
        let fed = finish(&mut feed, &mut queue);
        assert_eq!(
            fed.edits,
            [
                Edit::Key(Key::Previous),
                Edit::Key(Key::Previous),
                Edit::Key(Key::Previous),
                Edit::Key(Key::Previous),
                Edit::Key(Key::Delete),
                Edit::Key(Key::Delete),
                commit("or"),
                Edit::Key(Key::Next),
                Edit::Key(Key::Next),
            ]
        );
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
