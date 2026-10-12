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
//!   shell's side of a session (`Shell`, `enable`, `disable`).
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
pub(crate) use android::{Shell, disable, enable, set_android_app};

#[cfg(any(
    all(target_os = "android", feature = "android-native-activity"),
    test
))]
use crate::core::{self, SmolStr, input_method, keyboard};

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
/// with no cursor clears a shown one. `Opened` and `Closed` are never sent:
/// `text_input` and `text_editor` take a preedit and a commit without them,
/// and `Opened` would give every text input in the window an empty preedit.
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

#[cfg(test)]
mod tests {
    use super::*;

    use keyboard::key::{Code, Named, Physical};

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
