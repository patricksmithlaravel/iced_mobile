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
//! - `feed`: the engine fed the queue in order, and the application's turn
//!   after the keyboard's Enter, Tab or Escape.
//! - `android`: the JNI glue, the queue the Java side fills, and the
//!   shell's side of a session (`Shell`, `enable`, `disable`, `operate`,
//!   `operated`, and the operations it defers).
//! - [`Watch`]: the widget operations that move the focus, or the caret of a
//!   field that has it ([`FocusedInputs`]), while the keyboard types, found
//!   out as they run, and [`Replay`], which makes the caret moves a watch
//!   held back.
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
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
pub(crate) mod feed;

#[cfg(all(target_os = "android", feature = "android-native-activity"))]
mod android;

#[cfg(all(target_os = "android", feature = "android-native-activity"))]
pub(crate) use android::{
    Shell, deferred, disable, enable, operate, operated, schedule,
    set_android_app, touched,
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
use crate::core::{
    self, Point, Rectangle, SmolStr, Vector, input_method, keyboard, mouse,
    touch,
};

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
/// Select all is the text widgets' own shortcut, Ctrl+A: the A key pressed
/// and released with `Modifiers::COMMAND` added to the window's modifiers,
/// between two `ModifiersChanged` when that adds it, since `text_input`
/// reads the modifiers from those and `text_editor` from the key; the
/// second puts back the window's own.
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
            let named = keyboard::Key::Named;

            // The key, its physical key, its text, and the modifiers it is
            // pressed with.
            let (key, code, text, with) = match key {
                edit::Key::Backspace => (
                    named(Named::Backspace),
                    Code::Backspace,
                    Some("\u{8}"),
                    modifiers,
                ),
                edit::Key::Delete => (
                    named(Named::Delete),
                    Code::Delete,
                    Some("\u{7f}"),
                    modifiers,
                ),
                edit::Key::Left => {
                    (named(Named::ArrowLeft), Code::ArrowLeft, None, modifiers)
                }
                edit::Key::Right => (
                    named(Named::ArrowRight),
                    Code::ArrowRight,
                    None,
                    modifiers,
                ),
                edit::Key::Up => {
                    (named(Named::ArrowUp), Code::ArrowUp, None, modifiers)
                }
                edit::Key::Down => {
                    (named(Named::ArrowDown), Code::ArrowDown, None, modifiers)
                }
                edit::Key::Home => {
                    (named(Named::Home), Code::Home, None, modifiers)
                }
                edit::Key::End => {
                    (named(Named::End), Code::End, None, modifiers)
                }
                edit::Key::Enter => {
                    (named(Named::Enter), Code::Enter, Some("\r"), modifiers)
                }
                edit::Key::Tab => {
                    (named(Named::Tab), Code::Tab, Some("\t"), modifiers)
                }
                edit::Key::Escape => (
                    named(Named::Escape),
                    Code::Escape,
                    Some("\u{1b}"),
                    modifiers,
                ),
                edit::Key::SelectAll => (
                    keyboard::Key::Character(SmolStr::new("a")),
                    Code::KeyA,
                    None,
                    modifiers | keyboard::Modifiers::COMMAND,
                ),
            };

            let physical_key = Physical::Code(code);
            let shortcut = with != modifiers;

            if shortcut {
                push(core::Event::Keyboard(keyboard::Event::ModifiersChanged(
                    with,
                )));
            }

            push(core::Event::Keyboard(keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key.clone(),
                physical_key,
                location: keyboard::Location::Standard,
                modifiers: with,
                text: text.map(SmolStr::new),
                repeat: false,
            }));

            push(core::Event::Keyboard(keyboard::Event::KeyReleased {
                key: key.clone(),
                modified_key: key,
                physical_key,
                location: keyboard::Location::Standard,
                modifiers: with,
            }));

            if shortcut {
                push(core::Event::Keyboard(keyboard::Event::ModifiersChanged(
                    modifiers,
                )));
            }
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

/// Where `events` press on the window: each finger that touches down, and
/// the cursor for each mouse button pressed.
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
fn presses(events: &[core::Event], cursor: mouse::Cursor) -> Vec<Point> {
    events
        .iter()
        .filter_map(|event| match event {
            core::Event::Touch(touch::Event::FingerPressed {
                position,
                ..
            }) => Some(*position),
            core::Event::Mouse(mouse::Event::ButtonPressed(_)) => {
                cursor.position()
            }
            _ => None,
        })
        .collect()
}

/// An operation that finds where the focused widgets are drawn: their
/// bounds, moved by the scroll offset of each scrollable around them and cut
/// to what those scrollables show. Every focused widget counts: a
/// `text_editor` focused from code keeps the focus when the user taps a
/// `text_input`.
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
#[derive(Debug, Default)]
pub(crate) struct Focused {
    /// The scroll offset of the scrollables around the widgets visited now.
    translation: Vector,
    /// What those scrollables show of the window; none: all of it.
    shown: Option<Rectangle>,
    /// The scrollable just visited, for the contents it traverses next: its
    /// offset and its bounds.
    scrollable: Option<(Vector, Rectangle)>,
    /// The focused widgets, as much of each as is drawn.
    focused: Vec<Rectangle>,
}

#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
impl Focused {
    /// Whether `point` lands on a focused widget.
    pub(crate) fn contains(&self, point: Point) -> bool {
        self.focused.iter().any(|bounds| bounds.contains(point))
    }
}

#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
impl Operation for Focused {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        let (translation, shown) = (self.translation, self.shown);

        // A scrollable's contents move by its offset and show inside it,
        // where the scrollables around it draw it.
        if let Some((offset, bounds)) = self.scrollable.take() {
            let viewport = bounds - translation;

            self.shown = Some(match shown {
                None => viewport,
                Some(shown) => shown.intersection(&viewport).unwrap_or(
                    Rectangle::new(viewport.position(), core::Size::ZERO),
                ),
            });
            self.translation = translation + offset;
        }

        operate(self);

        self.translation = translation;
        self.shown = shown;
    }

    fn container(&mut self, _id: Option<&Id>, _bounds: Rectangle) {
        self.scrollable = None;
    }

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        bounds: Rectangle,
        _content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn Scrollable,
    ) {
        self.scrollable = Some((translation, bounds));
    }

    fn focusable(
        &mut self,
        _id: Option<&Id>,
        bounds: Rectangle,
        state: &mut dyn Focusable,
    ) {
        self.scrollable = None;

        if !state.is_focused() {
            return;
        }

        let drawn = bounds - self.translation;
        let shown = match self.shown {
            None => Some(drawn),
            Some(shown) => shown.intersection(&drawn),
        };

        self.focused.extend(shown);
    }

    fn text_input(
        &mut self,
        _id: Option<&Id>,
        _bounds: Rectangle,
        _state: &mut dyn TextInput,
    ) {
        self.scrollable = None;
    }

    fn text(&mut self, _id: Option<&Id>, _bounds: Rectangle, _text: &str) {
        self.scrollable = None;
    }

    fn custom(
        &mut self,
        _id: Option<&Id>,
        _bounds: Rectangle,
        _state: &mut dyn std::any::Any,
    ) {
        self.scrollable = None;
    }
}

/// An operation that finds the text inputs that have the focus: the fields
/// the keyboard types into, whose carets are the keyboard's.
///
/// A text input is known by its id and its bounds: `text_input` hands its
/// state to an operation as a text input, then as a focusable, with the
/// same id and bounds, and two fields never share both.
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
#[derive(Debug, Default)]
pub(crate) struct FocusedInputs {
    inputs: Vec<(Option<Id>, Rectangle)>,
}

#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
impl FocusedInputs {
    /// Whether the widget with `id` and `bounds` has the focus.
    fn contains(&self, id: Option<&Id>, bounds: Rectangle) -> bool {
        self.inputs
            .iter()
            .any(|(input, at)| input.as_ref() == id && *at == bounds)
    }
}

#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
impl Operation for FocusedInputs {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn focusable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        state: &mut dyn Focusable,
    ) {
        if state.is_focused() {
            self.inputs.push((id.cloned(), bounds));
        }
    }
}

/// What a widget operation did to the focus and to the text inputs that
/// have it, and what it held back.
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Moved {
    /// A widget gained or lost the focus.
    pub(crate) focus: bool,
    /// The caret or selection of a text input that has the focus was set,
    /// or the focused widget was focused again (which moves `text_input`'s
    /// caret to its end).
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
/// in [`Moved`], what that one does to the focus and to the text inputs
/// that have it ([`FocusedInputs`], found before it runs). The keyboard's
/// engine follows the caret only through what it types, so a focus or a
/// caret the application moves itself ends what it typed. The caret of a
/// text input without the focus is not the keyboard's: its moves are made
/// at once and end nothing.
///
/// A `hold`ing watch is used while the keyboard composes, so that the
/// composition can be committed where it was typed before the caret moves:
/// it does not make the caret moves of a focused widget with an id, and
/// records them in [`Moved::held`] instead, for a [`Replay`] to make once
/// the field has the composition. Focus moves, and caret moves of a widget
/// without an id (which a replay cannot find), are made at once.
#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
pub(crate) struct Watch<'a, T> {
    inner: &'a mut dyn Operation<T>,
    moved: &'a mut Moved,
    hold: bool,
    focused: &'a FocusedInputs,
}

#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
impl<'a, T> Watch<'a, T> {
    /// Watches `inner` over the interface where `focused` found the text
    /// inputs with the focus, recording into `moved`, holding back their
    /// caret moves if `hold`.
    pub(crate) fn new(
        inner: &'a mut dyn Operation<T>,
        moved: &'a mut Moved,
        hold: bool,
        focused: &'a FocusedInputs,
    ) -> Self {
        Self {
            inner,
            moved,
            hold,
            focused,
        }
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
        let focused = self.focused;

        self.inner.traverse(&mut |inner| {
            operate(&mut Watch {
                inner,
                moved: &mut *moved,
                hold,
                focused,
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
        // Not a field the keyboard types into.
        if !self.focused.contains(id, bounds) {
            self.inner.text_input(id, bounds, state);
            return;
        }

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

    /// Runs `operation` over text inputs with these ids (or none), one
    /// below the other, nested in a container the way widgets hand their
    /// children on, as each operation pass does: through a [`Watch`] that
    /// holds caret moves back or not, or unwatched. What it moved.
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
                        for (n, (id, input)) in inputs.iter_mut().enumerate() {
                            let id = ids.then_some(&*id);
                            let bounds =
                                rectangle(0.0, n as f32 * 40.0, 200.0, 40.0);

                            operation.text_input(id, bounds, input);
                            operation.focusable(id, bounds, input);
                        }
                    });
                };

                match watch {
                    Some(hold) => {
                        // The shell finds the focused fields first.
                        let mut focused = FocusedInputs::default();
                        operate(&mut focused);

                        operate(&mut Watch::new(
                            operation.as_mut(),
                            &mut moved,
                            hold,
                            &focused,
                        ));
                    }
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

        // Only the focused field's are held back: the others are made at
        // once.
        let mut inputs = fields(Some(1));
        let moved = run_with(Box::new(Many), &mut inputs, true, Some(true));
        assert!(moved.focus && !moved.caret);
        assert_eq!(
            moved.held,
            [Held::All(Id::new("b")), Held::To(Id::new("b"), 2)]
        );
        assert!(inputs.iter().all(|(_id, input)| !input.focused));
        assert_eq!(
            inputs
                .iter()
                .map(|(_id, input)| input.cursor)
                .collect::<Vec<_>>(),
            [2, 1, 2]
        );

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
    fn a_watch_ignores_caret_moves_in_fields_without_the_focus() {
        for hold in [false, true] {
            for (operation, held) in caret_moves() {
                // Focusing "b" moves the focus there: not a caret move.
                if let Held::Focus(_) = held {
                    continue;
                }

                // The keyboard types into "a"; the application moves the
                // caret of "b".
                let mut inputs = fields(Some(0));

                assert_eq!(
                    run_with(operation, &mut inputs, true, Some(hold)),
                    Moved::default(),
                    "{held:?}"
                );
                assert!(
                    inputs[1].1.cursor != 1 || inputs[1].1.selection.is_some(),
                    "{held:?} was not made at once"
                );
                assert_eq!(
                    (inputs[0].1.cursor, inputs[0].1.selection),
                    (1, None),
                    "{held:?}"
                );
            }

            // No field has the focus.
            let mut inputs = fields(None);
            assert_eq!(
                run_with(
                    Box::new(text_input::select_all(Id::new("c"))),
                    &mut inputs,
                    true,
                    Some(hold)
                ),
                Moved::default()
            );
            assert_eq!(inputs[2].1.selection, Some((0, 9)));
        }

        // Fields without an id are known by their bounds: of an operation
        // that moves every caret, only the focused field's move counts.
        struct EveryCaret;

        impl Operation for EveryCaret {
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
                state.move_cursor_to(5);
            }
        }

        for focused in [None, Some(2)] {
            let mut inputs = fields(focused);

            assert_eq!(
                run_with(Box::new(EveryCaret), &mut inputs, false, Some(true)),
                Moved {
                    caret: focused.is_some(),
                    ..Moved::default()
                },
                "{focused:?}"
            );
            assert!(inputs.iter().all(|(_id, input)| input.cursor == 5));
        }
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
    fn presses_are_fingers_down_and_mouse_buttons_at_the_cursor() {
        let finger = Point::new(10.0, 20.0);
        let pointer = Point::new(3.0, 4.0);
        let id = touch::Finger(1);

        let events = [
            core::Event::Touch(touch::Event::FingerPressed {
                id,
                position: finger,
            }),
            core::Event::Touch(touch::Event::FingerMoved {
                id,
                position: pointer,
            }),
            core::Event::Touch(touch::Event::FingerLifted {
                id,
                position: pointer,
            }),
            core::Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left,
            )),
            core::Event::Mouse(mouse::Event::ButtonReleased(
                mouse::Button::Left,
            )),
            core::Event::Mouse(mouse::Event::CursorMoved { position: finger }),
        ];

        assert_eq!(
            presses(&events, mouse::Cursor::Available(pointer)),
            [finger, pointer]
        );
        assert_eq!(presses(&events, mouse::Cursor::Unavailable), [finger]);
        assert_eq!(
            presses(&events[1..3], mouse::Cursor::Available(pointer)),
            []
        );
    }

    /// A scrollable's state, which nothing scrolls here.
    struct Scrolled;

    impl Scrollable for Scrolled {
        fn snap_to(
            &mut self,
            _offset: crate::core::widget::operation::scrollable::RelativeOffset<
                Option<f32>,
            >,
        ) {
        }

        fn scroll_to(
            &mut self,
            _offset: crate::core::widget::operation::scrollable::AbsoluteOffset<
                Option<f32>,
            >,
        ) {
        }

        fn scroll_by(
            &mut self,
            _offset: crate::core::widget::operation::scrollable::AbsoluteOffset,
            _bounds: Rectangle,
            _content_bounds: Rectangle,
        ) {
        }
    }

    fn rectangle(x: f32, y: f32, width: f32, height: f32) -> Rectangle {
        Rectangle::new(Point::new(x, y), core::Size::new(width, height))
    }

    /// A focusable widget at `bounds`, focused or not.
    fn widget(operation: &mut dyn Operation, bounds: Rectangle, focused: bool) {
        let mut input = Input {
            focused,
            ..Input::default()
        };

        operation.text_input(None, bounds, &mut input);
        operation.focusable(None, bounds, &mut input);
    }

    /// A scrollable at `bounds`, scrolled by `offset`, whose contents
    /// `content` hands on, the way `scrollable` operates.
    fn scrollable(
        operation: &mut dyn Operation,
        bounds: Rectangle,
        offset: Vector,
        content: &mut dyn FnMut(&mut dyn Operation),
    ) {
        operation.scrollable(None, bounds, bounds, offset, &mut Scrolled);
        operation.traverse(&mut |operation| {
            operation.container(None, bounds);
            operation.traverse(content);
        });
    }

    #[test]
    fn the_focused_widget_is_found_where_it_is_drawn() {
        let mut focused = Focused::default();

        // A toolbar on top, a page scrolled by 150 below it with a focused
        // editor inside, and a focused field under the page.
        focused.container(None, rectangle(0.0, 0.0, 200.0, 400.0));
        focused.traverse(&mut |operation| {
            widget(operation, rectangle(0.0, 0.0, 200.0, 40.0), false);

            scrollable(
                operation,
                rectangle(0.0, 40.0, 200.0, 200.0),
                Vector::new(0.0, 150.0),
                &mut |operation| {
                    widget(
                        operation,
                        rectangle(0.0, 40.0, 200.0, 100.0),
                        false,
                    );
                    widget(
                        operation,
                        rectangle(0.0, 160.0, 200.0, 100.0),
                        true,
                    );
                },
            );

            widget(operation, rectangle(0.0, 300.0, 200.0, 40.0), true);
        });

        // The editor is drawn at 10 to 110, but the page shows it from 40.
        for (x, y, inside) in [
            (100.0, 20.0, false),
            (100.0, 45.0, true),
            (100.0, 105.0, true),
            (100.0, 115.0, false),
            (100.0, 200.0, false),
            (100.0, 320.0, true),
            (210.0, 320.0, false),
        ] {
            assert_eq!(
                focused.contains(Point::new(x, y)),
                inside,
                "({x}, {y})"
            );
        }
    }

    #[test]
    fn scrollables_inside_scrollables_add_up() {
        let mut focused = Focused::default();

        scrollable(
            &mut focused,
            rectangle(0.0, 0.0, 300.0, 300.0),
            Vector::new(0.0, 100.0),
            &mut |operation| {
                scrollable(
                    operation,
                    rectangle(0.0, 200.0, 300.0, 100.0),
                    Vector::new(50.0, 0.0),
                    &mut |operation| {
                        widget(
                            operation,
                            rectangle(60.0, 200.0, 40.0, 40.0),
                            true,
                        );
                    },
                );
            },
        );

        // Drawn at (10, 100) to (50, 140), inside both.
        assert!(focused.contains(Point::new(20.0, 120.0)));
        assert!(!focused.contains(Point::new(70.0, 220.0)));
        assert!(!focused.contains(Point::new(20.0, 220.0)));

        // A focused widget scrolled out of sight is never pressed.
        let mut focused = Focused::default();

        scrollable(
            &mut focused,
            rectangle(0.0, 0.0, 100.0, 100.0),
            Vector::new(0.0, 500.0),
            &mut |operation| {
                widget(operation, rectangle(0.0, 0.0, 100.0, 100.0), true);
            },
        );

        assert!(!focused.contains(Point::new(50.0, 50.0)));
        assert!(!focused.contains(Point::new(50.0, -450.0)));
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
    fn select_all_is_the_fields_shortcut() {
        let shortcut = |modifiers: keyboard::Modifiers| {
            let key = keyboard::Key::Character(SmolStr::new("a"));
            let with = modifiers | keyboard::Modifiers::COMMAND;

            [
                core::Event::Keyboard(keyboard::Event::KeyPressed {
                    key: key.clone(),
                    modified_key: key.clone(),
                    physical_key: Physical::Code(Code::KeyA),
                    location: keyboard::Location::Standard,
                    modifiers: with,
                    text: None,
                    repeat: false,
                }),
                core::Event::Keyboard(keyboard::Event::KeyReleased {
                    key: key.clone(),
                    modified_key: key,
                    physical_key: Physical::Code(Code::KeyA),
                    location: keyboard::Location::Standard,
                    modifiers: with,
                }),
            ]
        };

        // `text_input` takes the command modifier from `ModifiersChanged`,
        // which then gives back the window's own.
        let [pressed, released] = shortcut(keyboard::Modifiers::SHIFT);
        assert_eq!(
            convert(edit::Edit::Key(edit::Key::SelectAll)),
            [
                core::Event::Keyboard(keyboard::Event::ModifiersChanged(
                    keyboard::Modifiers::SHIFT | keyboard::Modifiers::COMMAND
                )),
                pressed,
                released,
                core::Event::Keyboard(keyboard::Event::ModifiersChanged(
                    keyboard::Modifiers::SHIFT
                )),
            ]
        );

        // With it held already, none is needed.
        let mut out = Vec::new();
        events(
            edit::Edit::Key(edit::Key::SelectAll),
            keyboard::Modifiers::COMMAND,
            |event| out.push(event),
        );
        assert_eq!(out, shortcut(keyboard::Modifiers::COMMAND));
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
