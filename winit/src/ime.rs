//! Android: the system keyboard's text, through a view and input connection
//! of our own, so the keyboard can compose, suggest and correct.
//!
//! NativeActivity gives the keyboard no input connection, so it can only
//! send key events. The shell loads a small dex embedded in this crate
//! (`winit/android/`) at run time, puts its view in the Activity, and turns
//! what the keyboard does to that view's text into the key presses, commits
//! and preedits iced's widgets understand.
//!
//! - `dex`: the embedded dex, and the contract between its Java side and
//!   the shell.
//! - `edit`: the engine that turns the keyboard's reports into edits for the
//!   focused field.
//! - `android`: the JNI glue, the queue the Java side fills, and the
//!   shell's side of a session (`Shell`, `enable`, `disable`, `operate`,
//!   `operated`, and the operations it defers).
//! - [`Watch`]: the widget operations that move the focus or a caret while
//!   the keyboard types, found out as they run, and [`Replay`], which makes
//!   the caret moves a watch held back.
//!
//! Only with the `android-native-activity` feature: GameActivity has an
//! input connection of its own. Elsewhere nothing here is compiled but the
//! host tests.
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
pub(crate) mod dex;
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
pub(crate) mod edit;

#[cfg(all(target_os = "android", feature = "android-native-activity"))]
mod android;

#[cfg(all(target_os = "android", feature = "android-native-activity"))]
pub(crate) use android::{
    Shell, deferred, disable, enable, operate, operated, schedule,
    set_android_app,
};

#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
use crate::core::widget::Id;
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
use crate::core::widget::operation::{
    Focusable, Operation, Outcome, Scrollable, TextInput,
};
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
use crate::core::{self, Rectangle, SmolStr, Vector, input_method, keyboard};

/// The events an edit becomes for the focused field, in order.
///
/// A key is a press and a release shaped like the keys winit's Android
/// backend reports and `conversion` maps: the named key as both the key and
/// the modified key, its physical key, the standard location, no repeat,
/// and the text a desktop keyboard gives it (Enter "\r", Backspace "\u{8}",
/// Tab "\t", Escape "\u{1b}", Delete "\u{7f}", none for the arrows, Home and
/// End). The text widgets insert no control character, so only the named
/// key acts; `text_editor` takes Delete only with no text or "\u{7f}", and
/// moves only on a key without text.
///
/// A commit and a preedit are the input method's events; an empty preedit
/// with no cursor clears a shown one. `Opened` is never sent: `text_input`
/// and `text_editor` take a preedit and a commit without it, and it would
/// give every text input in the window an empty preedit. The session sends
/// `Closed` itself when a field loses the keyboard (`android`), as winit's
/// desktop backends do when the input method is disabled.
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
fn events(
    edit: edit::Edit,
    modifiers: keyboard::Modifiers,
    mut push: impl FnMut(core::Event),
) {
    use keyboard::key::{Code, Named, Physical};

    match edit {
        edit::Edit::Key(key) => {
            let (named, code, text) = match key {
                edit::Key::Backspace => {
                    (Named::Backspace, Code::Backspace, Some("\u{8}"))
                }
                edit::Key::Delete => {
                    (Named::Delete, Code::Delete, Some("\u{7f}"))
                }
                edit::Key::Left => (Named::ArrowLeft, Code::ArrowLeft, None),
                edit::Key::Right => (Named::ArrowRight, Code::ArrowRight, None),
                edit::Key::Up => (Named::ArrowUp, Code::ArrowUp, None),
                edit::Key::Down => (Named::ArrowDown, Code::ArrowDown, None),
                edit::Key::Home => (Named::Home, Code::Home, None),
                edit::Key::End => (Named::End, Code::End, None),
                edit::Key::Enter => (Named::Enter, Code::Enter, Some("\r")),
                edit::Key::Tab => (Named::Tab, Code::Tab, Some("\t")),
                edit::Key::Escape => {
                    (Named::Escape, Code::Escape, Some("\u{1b}"))
                }
            };

            let key = keyboard::Key::Named(named);
            let physical_key = Physical::Code(code);

            push(core::Event::Keyboard(keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key.clone(),
                physical_key,
                location: keyboard::Location::Standard,
                modifiers,
                text: text.map(SmolStr::new),
                repeat: false,
            }));

            push(core::Event::Keyboard(keyboard::Event::KeyReleased {
                key: key.clone(),
                modified_key: key,
                physical_key,
                location: keyboard::Location::Standard,
                modifiers,
            }));
        }
        edit::Edit::Commit(text) => {
            push(core::Event::InputMethod(input_method::Event::Commit(text)));
        }
        edit::Edit::Preedit(text, cursor) => {
            push(core::Event::InputMethod(input_method::Event::Preedit(
                text, cursor,
            )));
        }
    }
}

/// Whether a hardware key's press ends what the keyboard typed, as a tap
/// does (see `android`): any key but a modifier (Shift, Ctrl, Alt, Meta,
/// the locks, Fn, Sym), which moves no caret and edits nothing. An input
/// method that composes a hardware keyboard's letters may let Shift through
/// to the app: its composition stays, as in an Android `EditText` and on
/// the desktop.
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
fn ends_typing(key: &winit::keyboard::Key) -> bool {
    use winit::keyboard::{Key, NamedKey};

    !matches!(
        key,
        Key::Named(
            NamedKey::Shift
                | NamedKey::Control
                | NamedKey::Alt
                | NamedKey::AltGraph
                | NamedKey::Super
                | NamedKey::Meta
                | NamedKey::Hyper
                | NamedKey::CapsLock
                | NamedKey::NumLock
                | NamedKey::ScrollLock
                | NamedKey::Fn
                | NamedKey::FnLock
                | NamedKey::Symbol
                | NamedKey::SymbolLock
        )
    )
}

/// What a widget operation did to the focus and to the text inputs, and
/// what it held back.
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Moved {
    /// A widget gained or lost the focus.
    pub(crate) focus: bool,
    /// A text input's caret or selection was set, or the focused widget
    /// was focused again (which moves `text_input`'s caret to its end).
    pub(crate) caret: bool,
    /// The caret moves held back, in order, while the keyboard composed:
    /// they run once the field has the composition (see [`Watch`]).
    pub(crate) held: Vec<Held>,
}

/// A caret move a [`Watch`] held back: the call, and the widget it was
/// made on.
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Held {
    /// `TextInput::move_cursor_to_front`.
    Front(Id),
    /// `TextInput::move_cursor_to_end`.
    End(Id),
    /// `TextInput::move_cursor_to`.
    To(Id, usize),
    /// `TextInput::select_all`.
    All(Id),
    /// `TextInput::select_range`.
    Range(Id, usize, usize),
    /// `Focusable::focus` on the focused widget.
    Focus(Id),
}

/// An operation that hands every widget to the one it watches and records,
/// in [`Moved`], what that one does to the focus and the text inputs. The
/// keyboard's engine follows the caret only through what it types, so a
/// focus or a caret the application moves itself ends what it typed.
///
/// A `hold`ing watch is used while the keyboard composes, so that the
/// composition can be committed where it was typed before the caret moves:
/// it does not make the caret moves of a widget with an id, and records
/// them in [`Moved::held`] instead, for a [`Replay`] to make once the field
/// has the composition. Focus moves, and caret moves of a widget without an
/// id (which a replay cannot find), are made at once.
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
pub(crate) struct Watch<'a, T> {
    inner: &'a mut dyn Operation<T>,
    moved: &'a mut Moved,
    hold: bool,
}

#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
impl<'a, T> Watch<'a, T> {
    /// Watches `inner`, recording into `moved`, holding back caret moves if
    /// `hold`.
    pub(crate) fn new(
        inner: &'a mut dyn Operation<T>,
        moved: &'a mut Moved,
        hold: bool,
    ) -> Self {
        Self { inner, moved, hold }
    }
}

#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
impl<T> Operation<T> for Watch<'_, T> {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
        let moved = &mut *self.moved;
        let hold = self.hold;

        self.inner.traverse(&mut |inner| {
            operate(&mut Watch {
                inner,
                moved: &mut *moved,
                hold,
            });
        });
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        self.inner.container(id, bounds);
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        state: &mut dyn Scrollable,
    ) {
        self.inner
            .scrollable(id, bounds, content_bounds, translation, state);
    }

    fn focusable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        state: &mut dyn Focusable,
    ) {
        self.inner.focusable(
            id,
            bounds,
            &mut WatchFocus {
                state,
                moved: &mut *self.moved,
                held: id.filter(|_| self.hold),
            },
        );
    }

    fn text_input(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        state: &mut dyn TextInput,
    ) {
        self.inner.text_input(
            id,
            bounds,
            &mut WatchText {
                state,
                moved: &mut *self.moved,
                held: id.filter(|_| self.hold),
            },
        );
    }

    fn text(&mut self, id: Option<&Id>, bounds: Rectangle, text: &str) {
        self.inner.text(id, bounds, text);
    }

    fn custom(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        state: &mut dyn std::any::Any,
    ) {
        self.inner.custom(id, bounds, state);
    }

    fn finish(&self) -> Outcome<T> {
        self.inner.finish()
    }
}

/// A focusable widget's state, as a [`Watch`] hands it on.
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
struct WatchFocus<'a> {
    state: &'a mut dyn Focusable,
    moved: &'a mut Moved,
    /// The widget's id, when its caret moves are held back.
    held: Option<&'a Id>,
}

#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
impl Focusable for WatchFocus<'_> {
    fn is_focused(&self) -> bool {
        self.state.is_focused()
    }

    fn focus(&mut self) {
        if !self.state.is_focused() {
            self.moved.focus = true;
        } else if let Some(id) = self.held {
            self.moved.held.push(Held::Focus(id.clone()));
            return;
        } else {
            self.moved.caret = true;
        }

        self.state.focus();
    }

    fn unfocus(&mut self) {
        if self.state.is_focused() {
            self.moved.focus = true;
        }

        self.state.unfocus();
    }
}

/// A text input's state, as a [`Watch`] hands it on.
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
struct WatchText<'a> {
    state: &'a mut dyn TextInput,
    moved: &'a mut Moved,
    /// The widget's id, when its caret moves are held back.
    held: Option<&'a Id>,
}

#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
impl WatchText<'_> {
    /// A caret move: held back as `held` makes it, or made at once.
    fn moves(
        &mut self,
        held: impl FnOnce(Id) -> Held,
        make: impl FnOnce(&mut dyn TextInput),
    ) {
        if let Some(id) = self.held {
            self.moved.held.push(held(id.clone()));
        } else {
            self.moved.caret = true;
            make(&mut *self.state);
        }
    }
}

#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
impl TextInput for WatchText<'_> {
    fn text(&self) -> &str {
        self.state.text()
    }

    fn move_cursor_to_front(&mut self) {
        self.moves(Held::Front, |state| state.move_cursor_to_front());
    }

    fn move_cursor_to_end(&mut self) {
        self.moves(Held::End, |state| state.move_cursor_to_end());
    }

    fn move_cursor_to(&mut self, position: usize) {
        self.moves(
            |id| Held::To(id, position),
            |state| state.move_cursor_to(position),
        );
    }

    fn select_all(&mut self) {
        self.moves(Held::All, |state| state.select_all());
    }

    fn select_range(&mut self, start: usize, end: usize) {
        self.moves(
            |id| Held::Range(id, start, end),
            |state| state.select_range(start, end),
        );
    }
}

/// An operation that makes the caret moves a [`Watch`] held back, in
/// order, one per pass over the widgets: each on the widgets with its id. A
/// [`Held::Focus`] moves the caret only of a widget that still has the
/// focus, as focusing it did then: it never moves the focus.
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
pub(crate) struct Replay {
    held: Vec<Held>,
}

#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
impl Replay {
    /// Makes `held`, in order.
    pub(crate) fn new(held: Vec<Held>) -> Self {
        Self { held }
    }
}

#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
impl Operation for Replay {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn focusable(
        &mut self,
        id: Option<&Id>,
        _bounds: Rectangle,
        state: &mut dyn Focusable,
    ) {
        if let (Some(Held::Focus(target)), Some(id)) = (self.held.first(), id)
            && target == id
            && state.is_focused()
        {
            state.focus();
        }
    }

    fn text_input(
        &mut self,
        id: Option<&Id>,
        _bounds: Rectangle,
        state: &mut dyn TextInput,
    ) {
        let Some(id) = id else {
            return;
        };

        match self.held.first() {
            Some(Held::Front(target)) if target == id => {
                state.move_cursor_to_front();
            }
            Some(Held::End(target)) if target == id => {
                state.move_cursor_to_end();
            }
            Some(Held::To(target, position)) if target == id => {
                state.move_cursor_to(*position);
            }
            Some(Held::All(target)) if target == id => state.select_all(),
            Some(Held::Range(target, start, end)) if target == id => {
                state.select_range(*start, *end);
            }
            _ => {}
        }
    }

    fn finish(&self) -> Outcome<()> {
        if self.held.len() > 1 {
            Outcome::Chain(Box::new(Self::new(self.held[1..].to_vec())))
        } else {
            Outcome::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::widget::operation::{focusable, text_input};

    use keyboard::key::{Code, Named, Physical};

    /// A focusable text input's state.
    #[derive(Debug, Default)]
    struct Input {
        focused: bool,
        cursor: usize,
        selection: Option<(usize, usize)>,
    }

    impl Focusable for Input {
        fn is_focused(&self) -> bool {
            self.focused
        }

        fn focus(&mut self) {
            self.focused = true;
            self.move_cursor_to_end();
        }

        fn unfocus(&mut self) {
            self.focused = false;
        }
    }

    impl TextInput for Input {
        fn text(&self) -> &str {
            ""
        }

        fn move_cursor_to_front(&mut self) {
            self.move_cursor_to(0);
        }

        fn move_cursor_to_end(&mut self) {
            self.move_cursor_to(9);
        }

        fn move_cursor_to(&mut self, position: usize) {
            self.cursor = position;
            self.selection = None;
        }

        fn select_all(&mut self) {
            self.select_range(0, 9);
        }

        fn select_range(&mut self, start: usize, end: usize) {
            self.selection = Some((start, end));
        }
    }

    /// Runs `operation` over text inputs with these ids (or none), nested
    /// in a container the way widgets hand their children on, as each
    /// operation pass does: through a [`Watch`] that holds caret moves back
    /// or not, or unwatched. What it moved.
    fn run_with(
        mut operation: Box<dyn Operation>,
        inputs: &mut [(Id, Input)],
        ids: bool,
        watch: Option<bool>,
    ) -> Moved {
        let mut moved = Moved::default();

        loop {
            {
                let mut operate = |operation: &mut dyn Operation| {
                    operation.container(None, Rectangle::default());
                    operation.traverse(&mut |operation| {
                        for (id, input) in inputs.iter_mut() {
                            let id = ids.then_some(&*id);

                            operation.text_input(
                                id,
                                Rectangle::default(),
                                input,
                            );
                            operation.focusable(
                                id,
                                Rectangle::default(),
                                input,
                            );
                        }
                    });
                };

                match watch {
                    Some(hold) => operate(&mut Watch::new(
                        operation.as_mut(),
                        &mut moved,
                        hold,
                    )),
                    None => operate(operation.as_mut()),
                }
            }

            match operation.finish() {
                Outcome::Chain(next) => operation = next,
                Outcome::None | Outcome::Some(()) => return moved,
            }
        }
    }

    /// Runs `operation`, watched without holding anything back.
    fn run(operation: Box<dyn Operation>, inputs: &mut [(Id, Input)]) -> Moved {
        run_with(operation, inputs, true, Some(false))
    }

    /// Three text inputs, "a", "b" and "c", one of them focused.
    fn fields(focused: Option<usize>) -> Vec<(Id, Input)> {
        ["a", "b", "c"]
            .into_iter()
            .enumerate()
            .map(|(n, id)| {
                (
                    Id::new(id),
                    Input {
                        focused: focused == Some(n),
                        cursor: 1,
                        selection: None,
                    },
                )
            })
            .collect()
    }

    fn focus_moved() -> Moved {
        Moved {
            focus: true,
            ..Moved::default()
        }
    }

    fn caret_moved() -> Moved {
        Moved {
            caret: true,
            ..Moved::default()
        }
    }

    fn held(held: impl IntoIterator<Item = Held>) -> Moved {
        Moved {
            held: held.into_iter().collect(),
            ..Moved::default()
        }
    }

    /// The caret moves of the operations that make them, by id "b", and
    /// what holding each back records.
    fn caret_moves() -> Vec<(Box<dyn Operation>, Held)> {
        let b = || Id::new("b");

        vec![
            (
                Box::new(text_input::move_cursor_to_front(b())),
                Held::Front(b()),
            ),
            (
                Box::new(text_input::move_cursor_to_end(b())),
                Held::End(b()),
            ),
            (
                Box::new(text_input::move_cursor_to(b(), 4)),
                Held::To(b(), 4),
            ),
            (Box::new(text_input::select_all(b())), Held::All(b())),
            (
                Box::new(text_input::select_range(b(), 2, 3)),
                Held::Range(b(), 2, 3),
            ),
            (Box::new(focusable::focus(b())), Held::Focus(b())),
        ]
    }

    #[test]
    fn a_watch_finds_focus_moves() {
        for hold in [false, true] {
            for (operation, focused, moved) in [
                (
                    Box::new(focusable::focus(Id::new("b")))
                        as Box<dyn Operation>,
                    Some(0),
                    Some(1),
                ),
                (Box::new(focusable::focus_next()), Some(0), Some(1)),
                (Box::new(focusable::focus_previous()), Some(1), Some(0)),
                (Box::new(focusable::unfocus()), Some(2), None),
                (Box::new(focusable::focus(Id::new("a"))), None, Some(0)),
            ] {
                let mut inputs = fields(focused);

                assert_eq!(
                    run_with(operation, &mut inputs, true, Some(hold)),
                    focus_moved(),
                    "{focused:?}"
                );

                // A focus move is never held back.
                let now = inputs.iter().position(|(_id, input)| input.focused);
                assert_eq!(now, moved, "the operation still runs");
            }
        }
    }

    #[test]
    fn a_watch_finds_caret_moves() {
        for (operation, _held) in caret_moves() {
            let mut inputs = fields(Some(1));

            assert_eq!(run(operation, &mut inputs), caret_moved());
            assert!(
                inputs[1].1.cursor != 1 || inputs[1].1.selection.is_some(),
                "the operation still runs"
            );
        }

        // Focusing the focused field moves its caret to its end.
        let mut inputs = fields(Some(1));
        let _ = run(Box::new(focusable::focus(Id::new("b"))), &mut inputs);
        assert_eq!(inputs[1].1.cursor, 9);
    }

    #[test]
    fn a_holding_watch_holds_caret_moves_back() {
        for (operation, expected) in caret_moves() {
            let mut inputs = fields(Some(1));

            assert_eq!(
                run_with(operation, &mut inputs, true, Some(true)),
                held([expected.clone()])
            );
            assert_eq!(
                (inputs[1].1.cursor, inputs[1].1.selection),
                (1, None),
                "{expected:?} was made"
            );
        }

        // Moves and focus moves of one operation, in order.
        struct Many;

        impl Operation for Many {
            fn traverse(
                &mut self,
                operate: &mut dyn FnMut(&mut dyn Operation),
            ) {
                operate(self);
            }

            fn text_input(
                &mut self,
                _id: Option<&Id>,
                _bounds: Rectangle,
                state: &mut dyn TextInput,
            ) {
                state.select_all();
                state.move_cursor_to(2);
            }

            fn focusable(
                &mut self,
                _id: Option<&Id>,
                _bounds: Rectangle,
                state: &mut dyn Focusable,
            ) {
                state.unfocus();
            }
        }

        let mut inputs = fields(Some(1));
        let moved = run_with(Box::new(Many), &mut inputs, true, Some(true));
        assert!(moved.focus && !moved.caret);
        assert_eq!(moved.held.len(), 6);
        assert_eq!(
            moved.held[2..4],
            [Held::All(Id::new("b")), Held::To(Id::new("b"), 2)]
        );
        assert!(inputs.iter().all(|(_id, input)| !input.focused));

        // A text input without an id cannot be found again: its caret
        // moves are made at once.
        let mut inputs = fields(Some(1));
        assert_eq!(
            run_with(Box::new(Many), &mut inputs, false, Some(true)),
            Moved {
                focus: true,
                caret: true,
                held: Vec::new(),
            }
        );
        assert_eq!(inputs[1].1.cursor, 2);
    }

    #[test]
    fn a_replay_makes_the_held_moves_in_order() {
        let b = || Id::new("b");

        for (held, cursor, selection) in [
            (vec![Held::Front(b())], 0, None),
            (vec![Held::To(b(), 4)], 4, None),
            (vec![Held::All(b())], 1, Some((0, 9))),
            (vec![Held::Range(b(), 2, 3)], 1, Some((2, 3))),
            (vec![Held::Focus(b()), Held::Front(b())], 0, None),
            (vec![Held::Front(b()), Held::Focus(b())], 9, None),
            (vec![Held::All(b()), Held::End(b())], 9, None),
        ] {
            let mut inputs = fields(Some(1));

            assert_eq!(
                run_with(
                    Box::new(Replay::new(held.clone())),
                    &mut inputs,
                    true,
                    None
                ),
                Moved::default()
            );
            assert_eq!(
                (inputs[1].1.cursor, inputs[1].1.selection),
                (cursor, selection),
                "{held:?}"
            );
            assert_eq!(inputs[0].1.cursor, 1, "{held:?}");
            assert_eq!(inputs[2].1.cursor, 1, "{held:?}");
        }

        // Focusing again never moves the focus: a field the focus left
        // keeps its caret.
        let mut inputs = fields(Some(2));
        let _ = run_with(
            Box::new(Replay::new(vec![Held::Focus(b())])),
            &mut inputs,
            true,
            None,
        );
        assert!(!inputs[1].1.focused);
        assert_eq!(inputs[1].1.cursor, 1);
        assert!(inputs[2].1.focused);
    }

    #[test]
    fn a_watch_ignores_what_moves_nothing() {
        for hold in [false, true] {
            for operation in [
                Box::new(focusable::unfocus()) as Box<dyn Operation>,
                Box::new(focusable::focus(Id::new("nowhere"))),
                Box::new(text_input::move_cursor_to_end(Id::new("nowhere"))),
            ] {
                let mut inputs = fields(None);

                assert_eq!(
                    run_with(operation, &mut inputs, true, Some(hold)),
                    Moved::default()
                );
            }
        }
    }

    #[test]
    fn modifier_keys_leave_the_keyboard_typing() {
        use winit::keyboard::{Key, NamedKey, NativeKey};

        for named in [
            NamedKey::Shift,
            NamedKey::Control,
            NamedKey::Alt,
            NamedKey::AltGraph,
            NamedKey::Super,
            NamedKey::Meta,
            NamedKey::Hyper,
            NamedKey::CapsLock,
            NamedKey::NumLock,
            NamedKey::ScrollLock,
            NamedKey::Fn,
            NamedKey::FnLock,
            NamedKey::Symbol,
            NamedKey::SymbolLock,
        ] {
            assert!(!ends_typing(&Key::Named(named)), "{named:?}");
        }

        for key in [
            Key::Character("a".into()),
            Key::Character("ß".into()),
            Key::Named(NamedKey::Space),
            Key::Named(NamedKey::Enter),
            Key::Named(NamedKey::Tab),
            Key::Named(NamedKey::Escape),
            Key::Named(NamedKey::Backspace),
            Key::Named(NamedKey::Delete),
            Key::Named(NamedKey::ArrowLeft),
            Key::Named(NamedKey::Home),
            Key::Named(NamedKey::BrowserBack),
            Key::Dead(Some('´')),
            Key::Unidentified(NativeKey::Android(0)),
        ] {
            assert!(ends_typing(&key), "{key:?}");
        }
    }

    fn convert(edit: edit::Edit) -> Vec<core::Event> {
        let mut out = Vec::new();
        events(edit, keyboard::Modifiers::SHIFT, |event| out.push(event));
        out
    }

    fn press(named: Named, code: Code, text: Option<&str>) -> [core::Event; 2] {
        let key = keyboard::Key::Named(named);

        [
            core::Event::Keyboard(keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key.clone(),
                physical_key: Physical::Code(code),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::SHIFT,
                text: text.map(SmolStr::new),
                repeat: false,
            }),
            core::Event::Keyboard(keyboard::Event::KeyReleased {
                key: key.clone(),
                modified_key: key,
                physical_key: Physical::Code(code),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::SHIFT,
            }),
        ]
    }

    #[test]
    fn keys_are_a_press_and_a_release_with_desktop_text() {
        let cases = [
            (
                edit::Key::Backspace,
                Named::Backspace,
                Code::Backspace,
                "\u{8}",
            ),
            (edit::Key::Delete, Named::Delete, Code::Delete, "\u{7f}"),
            (edit::Key::Enter, Named::Enter, Code::Enter, "\r"),
            (edit::Key::Tab, Named::Tab, Code::Tab, "\t"),
            (edit::Key::Escape, Named::Escape, Code::Escape, "\u{1b}"),
        ];

        for (key, named, code, text) in cases {
            assert_eq!(
                convert(edit::Edit::Key(key)),
                press(named, code, Some(text)),
                "{key:?}"
            );
        }
    }

    #[test]
    fn moves_have_no_text() {
        let cases = [
            (edit::Key::Left, Named::ArrowLeft, Code::ArrowLeft),
            (edit::Key::Right, Named::ArrowRight, Code::ArrowRight),
            (edit::Key::Up, Named::ArrowUp, Code::ArrowUp),
            (edit::Key::Down, Named::ArrowDown, Code::ArrowDown),
            (edit::Key::Home, Named::Home, Code::Home),
            (edit::Key::End, Named::End, Code::End),
        ];

        for (key, named, code) in cases {
            assert_eq!(
                convert(edit::Edit::Key(key)),
                press(named, code, None),
                "{key:?}"
            );
        }
    }

    #[test]
    fn commits_and_preedits_are_input_method_events() {
        assert_eq!(
            convert(edit::Edit::Commit("héllo".to_owned())),
            [core::Event::InputMethod(input_method::Event::Commit(
                "héllo".to_owned()
            ))]
        );

        assert_eq!(
            convert(edit::Edit::Preedit("wor".to_owned(), Some(3..3))),
            [core::Event::InputMethod(input_method::Event::Preedit(
                "wor".to_owned(),
                Some(3..3)
            ))]
        );

        assert_eq!(
            convert(edit::Edit::Preedit(String::new(), None)),
            [core::Event::InputMethod(input_method::Event::Preedit(
                String::new(),
                None
            ))]
        );
    }
}
