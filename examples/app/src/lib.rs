//! The whole app. The same code runs on the desktop, the web, iOS and
//! Android: `src/main.rs` starts it on the desktop, the web and iOS, and
//! `iced::android_main!` at the end of this file starts it on Android.
//!
//! [`application`] is the program. [`run`] runs it, and `tests/icm.rs`
//! drives it headless: the `.ice` flows in `tests/flows`, `icm shot
//! --headless` and `icm ui --headless`.
use iced::mobile::{LifecycleEvent, SafeArea};
use iced::theme;
use iced::widget::{
    Column, button, column, container, operation, responsive, row, scrollable,
    text, text_input,
};
use iced::{
    Application, Center, Element, Fill, Font, Padding, Program, Size,
    Subscription, Task, Theme,
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

/// The space around the content, beside what the system covers.
const MARGIN: f32 = 16.0;

/// The state of the app.
#[derive(Debug)]
pub struct App {
    /// What the status bar, the notch, the home indicator or navigation bar
    /// and the keyboard cover, once the platform has reported it.
    safe_area: Option<SafeArea>,
    /// The system's light or dark mode, as the platform last reported it.
    appearance: theme::Mode,
    /// Whether the app is active, inactive or in the background, as the
    /// platform last reported it.
    lifecycle: Option<LifecycleEvent>,
    count: i64,
    draft: String,
    items: Vec<String>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            safe_area: None,
            appearance: theme::Mode::None,
            lifecycle: None,
            count: 0,
            draft: String::new(),
            items: (1..=20).map(|i| format!("Item {i}")).collect(),
        }
    }
}

/// Everything that can happen in the app.
#[derive(Debug, Clone)]
pub enum Message {
    /// The platform reported the safe area, or a change: a rotation, the
    /// keyboard showing or hiding.
    SafeAreaChanged(SafeArea),
    /// The system switched between light and dark mode, or reported its
    /// mode at launch.
    AppearanceChanged(theme::Mode),
    /// The app came to the foreground, became active or inactive, went to
    /// the background, or the system is short of memory.
    LifecycleChanged(LifecycleEvent),
    /// The "Increment" button was pressed.
    Increment,
    /// The text in the field changed.
    DraftChanged(String),
    /// "Add" was pressed, or Return in the field.
    Add,
    /// The "Paste" button was pressed.
    Paste,
    /// The clipboard was read: its text, if it holds any.
    Pasted(Option<String>),
    /// The "Copy" button of the item at this index was pressed.
    Copy(usize),
    /// The "Remove" button of the item at this index was pressed.
    Remove(usize),
}

impl App {
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SafeAreaChanged(safe_area) => {
                self.safe_area = Some(safe_area);

                Task::none()
            }
            // The default theme follows the system already; the state keeps
            // the mode only to show it.
            Message::AppearanceChanged(appearance) => {
                self.appearance = appearance;

                Task::none()
            }
            // An app frees its caches here.
            Message::LifecycleChanged(LifecycleEvent::MemoryWarning) => {
                log::warn!("the system is short of memory");

                Task::none()
            }
            // Only shown here. An app hides what is on screen on `Inactive`
            // and locks on `Background`. What must outlive the app is saved
            // in `iced::mobile::on_lifecycle` instead, which runs before the
            // system acts: these messages come a moment later, and Android
            // can end the app before `Background` arrives.
            Message::LifecycleChanged(lifecycle) => {
                log::info!("lifecycle: {lifecycle:?}");

                self.lifecycle = Some(lifecycle);

                Task::none()
            }
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
            // Read the clipboard only when the user asks: Android gives
            // `None` to an app without the input focus, and iOS asks the
            // user before an app reads what another app copied.
            Message::Paste => iced::clipboard::read().map(Message::Pasted),
            Message::Pasted(Some(text)) => {
                // The field holds one line.
                self.draft
                    .push_str(&text.lines().collect::<Vec<_>>().join(" "));

                Task::none()
            }
            Message::Pasted(None) => {
                log::info!("nothing to paste");

                Task::none()
            }
            Message::Copy(index) => match self.items.get(index) {
                Some(item) => {
                    log::info!("copied {item:?}");

                    iced::clipboard::write(item.clone())
                }
                None => Task::none(),
            },
            Message::Remove(index) => {
                if index < self.items.len() {
                    let item = self.items.remove(index);

                    log::info!("removed {item:?}");
                }

                Task::none()
            }
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::mobile::safe_area().map(Message::SafeAreaChanged),
            iced::system::theme_changes().map(Message::AppearanceChanged),
            iced::mobile::lifecycle().map(Message::LifecycleChanged),
        ])
    }

    fn view(&self) -> Element<'_, Message> {
        // Until the platform reports the safe area, the root padding
        // depends on the window's size (see `padding`).
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

        // What the platform reports; headless unit tests have no platform
        // to ask.
        let status = row![
            text(match self.appearance {
                theme::Mode::Light => "Appearance: light",
                theme::Mode::Dark => "Appearance: dark",
                theme::Mode::None => "Appearance: not reported",
            })
            .size(14)
            .width(Fill),
            text(match self.lifecycle {
                Some(LifecycleEvent::Foreground) => "Lifecycle: foreground",
                Some(LifecycleEvent::Active) => "Lifecycle: active",
                Some(LifecycleEvent::Inactive) => "Lifecycle: inactive",
                Some(LifecycleEvent::Background) => "Lifecycle: background",
                _ => "Lifecycle: not reported",
            })
            .size(14),
        ]
        .spacing(8);

        // Near the top: the keyboard covers the lower half of the screen.
        // The root padding rises with it (`SafeArea::padding`), which keeps
        // the end of the list above it.
        let form = row![
            text_input("New item", &self.draft)
                .id(INPUT)
                .on_input(Message::DraftChanged)
                .on_submit(Message::Add)
                .padding(12),
            // Phones show no edit menu in text fields.
            button("Paste")
                .style(button::secondary)
                .padding(TAP)
                .on_press(Message::Paste),
            button("Add").padding(TAP).on_press_maybe(
                (!self.draft.trim().is_empty()).then_some(Message::Add)
            ),
        ]
        .spacing(8)
        .align_y(Center);

        // On a touch screen, a drag that starts on a button does not scroll
        // (iced-rs/iced#2004). The text fills each row and the buttons stay
        // small, so most of a row can start a scroll.
        let items = Column::with_children(self.items.iter().enumerate().map(
            |(index, item)| {
                row![
                    text(item).width(Fill),
                    button("Copy")
                        .style(button::text)
                        .padding(TAP)
                        .on_press(Message::Copy(index)),
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
            status,
            form,
            scrollable(items)
                .id(LIST)
                .spacing(8)
                .width(Fill)
                .height(Fill),
        ]
        .spacing(16);

        container(content)
            .padding(self.padding(size))
            .width(Fill)
            .height(Fill)
            .into()
    }

    /// The root padding: [`MARGIN`] beside what the system covers. Phones
    /// draw under the status bar, the notch or Dynamic Island, the home
    /// indicator or navigation bar (Android apps targeting SDK 35 or later
    /// draw edge to edge) and the keyboard, and `iced::mobile::safe_area()`
    /// reports how much of each edge they take; the desktop and the web
    /// report nothing covered. Headless, `icm shot --headless`, `icm ui
    /// --headless` and `.ice` flows report the device's own at a phone
    /// preset's size (`iphone-17`, `pixel-9`, ...), so they lay out as the
    /// phone does.
    fn padding(&self, size: Size) -> Padding {
        match self.safe_area {
            Some(safe_area) => safe_area.padding(MARGIN),
            None => fallback_padding(size),
        }
    }
}

/// Windows narrower than this, in logical pixels, are laid out as a phone.
const PHONE_WIDTH: f32 = 600.0;

/// The root padding until the safe area is reported (the first frames on
/// a phone, unit tests, headless viewports of other sizes): room for a
/// phone's bars on iOS and Android, and in any window narrower than a
/// phone.
fn fallback_padding(size: Size) -> Padding {
    if cfg!(any(target_os = "ios", target_os = "android"))
        || size.width < PHONE_WIDTH
    {
        Padding::new(MARGIN).top(64.0).bottom(48.0)
    } else {
        Padding::new(MARGIN)
    }
}

/// The program, shared by [`run`] and by `tests/icm.rs` (flows, headless
/// screenshots, the widget tree).
pub fn application()
-> Application<impl Program<Message = Message, Theme = Theme>> {
    iced::application(App::default, App::update, App::view)
        .title("App")
        // No `.theme(..)`: the default theme follows the system's light or
        // dark mode, as icm's Android window and bar icons do.
        .subscription(App::subscription)
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
    fn the_root_is_padded_with_the_safe_area_once_reported() {
        let mut app = App::default();
        let phone = Size::new(402.0, 874.0);

        // Before the platform reports it: the fixed padding.
        assert_eq!(app.padding(phone), fallback_padding(phone));

        let island = Padding::ZERO.top(62.0).bottom(34.0);
        let _ = app.update(Message::SafeAreaChanged(SafeArea::new(island)));

        assert_eq!(
            app.padding(phone),
            Padding::new(16.0).top(78.0).bottom(50.0)
        );

        // The bottom rises with the keyboard.
        let _ = app.update(Message::SafeAreaChanged(
            SafeArea::new(island).with_keyboard(336.0),
        ));

        assert_eq!(app.padding(phone).bottom, 352.0);
    }

    #[test]
    fn the_lifecycle_is_shown() {
        let mut app = App::default();

        let _ = app.update(Message::LifecycleChanged(LifecycleEvent::Inactive));
        let _ = app
            .update(Message::LifecycleChanged(LifecycleEvent::MemoryWarning));

        let mut ui = simulator(app.view());

        assert!(ui.find("Lifecycle: inactive").is_ok());
    }

    #[test]
    fn copy_and_paste_go_through_the_clipboard() {
        let mut app = App::default();
        let mut ui = simulator(app.view());

        // The first "Copy" is Item 1's.
        tap(&mut ui, "Copy");
        tap(&mut ui, "Paste");

        let messages: Vec<_> = ui.into_messages().collect();

        assert!(matches!(messages[..], [Message::Copy(0), Message::Paste]));

        // A paste joins the clipboard's lines: the field holds one.
        let _ = app.update(Message::Pasted(Some(String::from("Milk\nEggs"))));

        assert_eq!(app.draft, "Milk Eggs");
    }

    #[test]
    fn the_system_appearance_is_shown() {
        let mut app = App::default();

        // Headless, nothing reports a mode.
        {
            let mut ui = simulator(app.view());

            assert!(ui.find("Appearance: not reported").is_ok());
        }

        let _ = app.update(Message::AppearanceChanged(theme::Mode::Dark));
        let mut ui = simulator(app.view());

        assert!(ui.find("Appearance: dark").is_ok());
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
