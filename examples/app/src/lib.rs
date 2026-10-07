//! The whole app. The same code runs on the desktop, the web, iOS and
//! Android: `src/main.rs` starts it on the desktop, the web and iOS, and
//! `iced::android_main!` at the end of this file starts it on Android.
//!
//! [`application`] is the program. [`run`] runs it, and `tests/icm.rs`
//! drives it headless: the `.ice` flows in `tests/flows`, `icm shot
//! --headless` and `icm ui --headless`.
use iced::widget::{
    Column, button, column, container, operation, responsive, row, scrollable,
    text, text_input,
};
use iced::{
    Application, Center, Element, Fill, Font, Padding, Program, Size, Task,
    Theme,
};

/// The id of the text field: `operation::focus(INPUT)` focuses it, and a
/// selector can find it by id.
const INPUT: &str = "new-item";

/// The id of the list, for scrolling it from `update`.
const LIST: &str = "items";

/// Padding that makes a button about 44 points tall, the smallest target
/// Apple and Google recommend for a finger.
const TAP: Padding = Padding {
    top: 12.0,
    right: 16.0,
    bottom: 12.0,
    left: 16.0,
};

/// The state of the app.
#[derive(Debug)]
pub struct App {
    count: i64,
    draft: String,
    items: Vec<String>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            count: 0,
            draft: String::new(),
            items: (1..=20).map(|i| format!("Item {i}")).collect(),
        }
    }
}

/// Everything that can happen in the app.
#[derive(Debug, Clone)]
pub enum Message {
    /// The "Increment" button was pressed.
    Increment,
    /// The text in the field changed.
    DraftChanged(String),
    /// "Add" was pressed, or Return in the field.
    Add,
    /// The "Remove" button of the item at this index was pressed.
    Remove(usize),
}

impl App {
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Increment => {
                self.count += 1;

                Task::none()
            }
            Message::DraftChanged(draft) => {
                self.draft = draft;

                Task::none()
            }
            // Return on a phone's keyboard submits too: iced turns it into
            // Enter on iOS and Android.
            Message::Add => {
                let item = self.draft.trim();

                if item.is_empty() {
                    return Task::none();
                }

                log::info!("added {item:?}");

                self.items.push(item.to_owned());
                self.draft.clear();

                // Show the new item. After Return the field keeps the
                // focus, so a phone's keyboard stays up for the next one.
                operation::snap_to_end(LIST)
            }
            Message::Remove(index) => {
                if index < self.items.len() {
                    let item = self.items.remove(index);

                    log::info!("removed {item:?}");
                }

                Task::none()
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        // The root padding depends on the window's size (see `safe_area`).
        responsive(move |size| self.screen(size)).into()
    }

    fn screen(&self, size: Size) -> Element<'_, Message> {
        let counter = row![
            text(format!("Count: {}", self.count)).size(24).width(Fill),
            button("Increment")
                .padding(TAP)
                .on_press(Message::Increment),
        ]
        .spacing(12)
        .align_y(Center);

        // At the top of the screen: iced does not know how tall a phone's
        // keyboard is, and the keyboard covers fields in the lower half.
        let form = row![
            text_input("New item", &self.draft)
                .id(INPUT)
                .on_input(Message::DraftChanged)
                .on_submit(Message::Add)
                .padding(12),
            button("Add").padding(TAP).on_press_maybe(
                (!self.draft.trim().is_empty()).then_some(Message::Add)
            ),
        ]
        .spacing(8)
        .align_y(Center);

        // On a touch screen, a drag that starts on a button does not scroll
        // (iced-rs/iced#2004). The text fills each row and the button stays
        // small, so most of a row can start a scroll.
        let items = Column::with_children(self.items.iter().enumerate().map(
            |(index, item)| {
                row![
                    text(item).width(Fill),
                    button("Remove")
                        .style(button::text)
                        .padding(TAP)
                        .on_press(Message::Remove(index)),
                ]
                .align_y(Center)
                .into()
            },
        ))
        .spacing(4);

        let content = column![
            counter,
            form,
            scrollable(items)
                .id(LIST)
                .spacing(8)
                .width(Fill)
                .height(Fill),
        ]
        .spacing(16);

        container(content)
            .padding(safe_area(size))
            .width(Fill)
            .height(Fill)
            .into()
    }
}

/// Windows narrower than this, in logical pixels, are laid out as a phone.
const PHONE_WIDTH: f32 = 600.0;

/// Room for the status bar, the notch or Dynamic Island, and the home
/// indicator or navigation bar. iced has no safe-area API yet, and Android
/// apps targeting SDK 35 or later draw edge to edge, so the root view pads
/// for them on iOS and Android. A window narrower than a phone gets the
/// same padding anywhere, so headless renders at phone viewports (`icm shot
/// --headless`, `icm ui --headless`, `.ice` flows) lay out as the phone
/// does.
fn safe_area(size: Size) -> Padding {
    if cfg!(any(target_os = "ios", target_os = "android"))
        || size.width < PHONE_WIDTH
    {
        Padding::new(16.0).top(64.0).bottom(48.0)
    } else {
        Padding::new(16.0)
    }
}

/// The program, shared by [`run`] and by `tests/icm.rs` (flows, headless
/// screenshots, the widget tree).
pub fn application()
-> Application<impl Program<Message = Message, Theme = Theme>> {
    iced::application(App::default, App::update, App::view)
        .title("App")
        // Embedded by the `fira-sans` feature: every platform, the headless
        // renderer included, draws the same glyphs.
        .default_font(Font::with_name("Fira Sans"))
}

/// Runs the app. `src/main.rs` calls it on the desktop, the web and iOS, and
/// `android_main` below calls it on Android.
///
/// # Errors
///
/// When the app cannot start: no window, or no graphics backend.
pub fn run() -> iced::Result {
    // The platform's logger and a panic hook. Calling it twice is harmless.
    // A logger of your own (env_logger, tracing_subscriber) goes BEFORE this
    // line, with `iced::android_main!(run, logger = false)` below: `log`
    // takes one logger per process, and installing one after iced's panics.
    // On Android `run` runs again for each new activity in the same process,
    // so install it with `try_init()` and ignore the error (or behind a
    // `std::sync::Once`): a second `init()` panics and ends the app.
    iced::mobile::init_logger();

    application().run()
}

// Android's entry point, which calls `run`. It defines nothing on other
// targets.
iced::android_main!(run);

#[cfg(test)]
mod tests {
    use super::*;

    use iced::keyboard::key;
    use iced::touch;
    use iced::{Event, Point, Settings, Vector};
    use iced_test::{Simulator, simulator};

    // A phone delivers touches, not mouse events, and the shell moves iced's
    // cursor to every touch first. These helpers do the same, so the tests
    // see what a phone does; `Simulator::click` and `.ice` flows use a mouse.

    fn touch(
        ui: &mut Simulator<'_, Message>,
        position: Point,
        event: fn(touch::Finger, Point) -> touch::Event,
    ) {
        ui.point_at(position);

        let _ = ui.simulate([Event::Touch(event(touch::Finger(0), position))]);
    }

    fn pressed(id: touch::Finger, position: Point) -> touch::Event {
        touch::Event::FingerPressed { id, position }
    }

    fn moved(id: touch::Finger, position: Point) -> touch::Event {
        touch::Event::FingerMoved { id, position }
    }

    fn lifted(id: touch::Finger, position: Point) -> touch::Event {
        touch::Event::FingerLifted { id, position }
    }

    fn center_of(ui: &mut Simulator<'_, Message>, text: &str) -> Point {
        ui.find(text)
            .unwrap_or_else(|error| panic!("{text:?}: {error}"))
            .visible_bounds()
            .unwrap_or_else(|| panic!("{text:?} is not visible"))
            .center()
    }

    fn tap(ui: &mut Simulator<'_, Message>, text: &str) {
        let position = center_of(ui, text);

        touch(ui, position, pressed);
        touch(ui, position, lifted);
    }

    #[test]
    fn a_tap_increments_the_count() {
        let mut app = App::default();
        let mut ui = simulator(app.view());

        tap(&mut ui, "Increment");

        for message in ui.into_messages() {
            let _ = app.update(message);
        }

        assert_eq!(app.count, 1);
    }

    #[test]
    fn return_adds_the_item_and_clears_the_field() {
        let mut app = App::default();
        let mut ui = simulator(app.view());

        tap(&mut ui, "New item");
        let _ = ui.typewrite("Milk");
        let _ = ui.tap_key(key::Named::Enter);

        for message in ui.into_messages() {
            let _ = app.update(message);
        }

        assert_eq!(app.items.last().map(String::as_str), Some("Milk"));
        assert_eq!(app.draft, "");
    }

    #[test]
    fn a_drag_that_starts_on_a_row_scrolls_the_list() {
        let app = App::default();
        let mut ui = Simulator::with_size(
            Settings::default(),
            (402.0, 874.0),
            app.view(),
        );

        let start = center_of(&mut ui, "Item 5");
        let end = start - Vector::new(0.0, 100.0);

        touch(&mut ui, start, pressed);
        touch(&mut ui, end, moved);
        touch(&mut ui, end, lifted);

        let after = center_of(&mut ui, "Item 5");

        assert!(
            (after.y - end.y).abs() < 1.0,
            "Item 5 should follow the finger to {end:?}; it is at {after:?}"
        );
    }
}
