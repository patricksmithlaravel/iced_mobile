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
//!   shell's side of a session (`Shell`, `enable`, `disable`, `operated`).
//! - [`Watch`]: the widget operations that move the focus or a caret while
//!   the keyboard types, found out as they run.
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
    Shell, disable, enable, is_typing, operated, set_android_app,
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

/// What a widget operation did to the focus and to the text inputs.
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Moved {
    /// A widget gained or lost the focus.
    pub(crate) focus: bool,
    /// A text input's caret or selection was set, or the focused widget
    /// was focused again (which moves `text_input`'s caret to its end).
    pub(crate) caret: bool,
}

/// An operation that hands every widget to the one it watches and records,
/// in [`Moved`], what that one does to the focus and the text inputs. The
/// keyboard's engine follows the caret only through what it types, so a
/// focus or a caret the application moves itself ends what it typed.
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
pub(crate) struct Watch<'a, T> {
    inner: &'a mut dyn Operation<T>,
    moved: &'a mut Moved,
}

#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
impl<'a, T> Watch<'a, T> {
    /// Watches `inner`, recording into `moved`.
    pub(crate) fn new(
        inner: &'a mut dyn Operation<T>,
        moved: &'a mut Moved,
    ) -> Self {
        Self { inner, moved }
    }
}

#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
impl<T> Operation<T> for Watch<'_, T> {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
        let moved = &mut *self.moved;

        self.inner.traverse(&mut |inner| {
            operate(&mut Watch {
                inner,
                moved: &mut *moved,
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
        if self.state.is_focused() {
            self.moved.caret = true;
        } else {
            self.moved.focus = true;
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
        self.moved.caret = true;
        self.state.move_cursor_to_front();
    }

    fn move_cursor_to_end(&mut self) {
        self.moved.caret = true;
        self.state.move_cursor_to_end();
    }

    fn move_cursor_to(&mut self, position: usize) {
        self.moved.caret = true;
        self.state.move_cursor_to(position);
    }

    fn select_all(&mut self) {
        self.moved.caret = true;
        self.state.select_all();
    }

    fn select_range(&mut self, start: usize, end: usize) {
        self.moved.caret = true;
        self.state.select_range(start, end);
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
    }

    impl Focusable for Input {
        fn is_focused(&self) -> bool {
            self.focused
        }

        fn focus(&mut self) {
            self.focused = true;
            self.cursor = 9;
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
            self.cursor = 0;
        }

        fn move_cursor_to_end(&mut self) {
            self.cursor = 9;
        }

        fn move_cursor_to(&mut self, position: usize) {
            self.cursor = position;
        }

        fn select_all(&mut self) {}

        fn select_range(&mut self, _start: usize, _end: usize) {}
    }

    /// Runs `operation`, watched, over text inputs with these ids, nested
    /// in a container the way widgets hand their children on, as each
    /// operation pass does; what it moved.
    fn run(
        mut operation: Box<dyn Operation>,
        inputs: &mut [(Id, Input)],
    ) -> Moved {
        let mut moved = Moved::default();

        loop {
            {
                let mut watch = Watch::new(operation.as_mut(), &mut moved);

                watch.container(None, Rectangle::default());
                watch.traverse(&mut |operation| {
                    for (id, input) in inputs.iter_mut() {
                        operation.text_input(
                            Some(id),
                            Rectangle::default(),
                            input,
                        );
                        operation.focusable(
                            Some(id),
                            Rectangle::default(),
                            input,
                        );
                    }
                });
            }

            match operation.finish() {
                Outcome::Chain(next) => operation = next,
                Outcome::None | Outcome::Some(()) => return moved,
            }
        }
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
                    },
                )
            })
            .collect()
    }

    #[test]
    fn a_watch_finds_focus_moves() {
        for (operation, focused, moved) in [
            (
                Box::new(focusable::focus(Id::new("b"))) as Box<dyn Operation>,
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
                run(operation, &mut inputs),
                Moved {
                    focus: true,
                    caret: false
                },
                "{focused:?}"
            );

            let now = inputs.iter().position(|(_id, input)| input.focused);
            assert_eq!(now, moved, "the operation still runs");
        }
    }

    #[test]
    fn a_watch_finds_caret_moves() {
        // Focusing the focused field moves its caret.
        let mut inputs = fields(Some(1));
        assert_eq!(
            run(Box::new(focusable::focus(Id::new("b"))), &mut inputs),
            Moved {
                focus: false,
                caret: true
            }
        );
        assert_eq!(inputs[1].1.cursor, 9);

        for operation in [
            Box::new(text_input::move_cursor_to_front(Id::new("b")))
                as Box<dyn Operation>,
            Box::new(text_input::move_cursor_to_end(Id::new("b"))),
            Box::new(text_input::move_cursor_to(Id::new("b"), 0)),
            Box::new(text_input::select_all(Id::new("b"))),
        ] {
            let mut inputs = fields(Some(1));

            assert_eq!(
                run(operation, &mut inputs),
                Moved {
                    focus: false,
                    caret: true
                }
            );
        }
    }

    #[test]
    fn a_watch_ignores_what_moves_nothing() {
        for operation in [
            Box::new(focusable::unfocus()) as Box<dyn Operation>,
            Box::new(focusable::focus(Id::new("nowhere"))),
            Box::new(text_input::move_cursor_to_end(Id::new("nowhere"))),
        ] {
            let mut inputs = fields(None);

            assert_eq!(run(operation, &mut inputs), Moved::default());
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
