use crate::conversion;
use crate::core::{Color, Size};
use crate::core::{mouse, theme, window};
use crate::graphics::Viewport;
use crate::program::{self, Program};

use winit::event::{Touch, WindowEvent};
use winit::window::Window;

use std::fmt::{Debug, Formatter};

/// The state of the window of a [`Program`].
pub struct State<P: Program>
where
    P::Theme: theme::Base,
{
    title: String,
    scale_factor: f32,
    viewport: Viewport,
    surface_version: u64,
    cursor_position: Option<winit::dpi::PhysicalPosition<f64>>,
    modifiers: winit::keyboard::ModifiersState,
    theme: Option<P::Theme>,
    theme_mode: theme::Mode,
    default_theme: P::Theme,
    style: theme::Style,
}

impl<P: Program> Debug for State<P>
where
    P::Theme: theme::Base,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("window::State")
            .field("title", &self.title)
            .field("scale_factor", &self.scale_factor)
            .field("viewport", &self.viewport)
            .field("cursor_position", &self.cursor_position)
            .field("style", &self.style)
            .finish()
    }
}

impl<P: Program> State<P>
where
    P::Theme: theme::Base,
{
    /// Creates a new [`State`] for the provided [`Program`]'s `window`.
    pub fn new(
        program: &program::Instance<P>,
        window_id: window::Id,
        window: &Window,
        system_theme: theme::Mode,
    ) -> Self {
        let title = program.title(window_id);
        let scale_factor = program.scale_factor(window_id);
        let theme = program.theme(window_id);
        let theme_mode =
            theme.as_ref().map(theme::Base::mode).unwrap_or_default();
        let default_theme = <P::Theme as theme::Base>::default(system_theme);
        let style = program.style(theme.as_ref().unwrap_or(&default_theme));

        let viewport = {
            let physical_size = window.inner_size();

            Viewport::with_physical_size(
                Size::new(physical_size.width, physical_size.height),
                window.scale_factor() as f32 * scale_factor,
            )
        };

        Self {
            title,
            scale_factor,
            viewport,
            surface_version: 0,
            cursor_position: None,
            modifiers: winit::keyboard::ModifiersState::default(),
            theme,
            theme_mode,
            default_theme,
            style,
        }
    }

    pub fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    pub fn surface_version(&self) -> u64 {
        self.surface_version
    }

    pub fn physical_size(&self) -> Size<u32> {
        self.viewport.physical_size()
    }

    pub fn logical_size(&self) -> Size<f32> {
        self.viewport.logical_size()
    }

    pub fn scale_factor(&self) -> f32 {
        self.viewport.scale_factor()
    }

    pub fn cursor(&self) -> mouse::Cursor {
        self.cursor_position
            .map(|cursor_position| {
                conversion::cursor_position(
                    cursor_position,
                    self.viewport.scale_factor(),
                )
            })
            .map(mouse::Cursor::Available)
            .unwrap_or(mouse::Cursor::Unavailable)
    }

    pub fn modifiers(&self) -> winit::keyboard::ModifiersState {
        self.modifiers
    }

    pub fn theme(&self) -> &P::Theme {
        self.theme.as_ref().unwrap_or(&self.default_theme)
    }

    pub fn theme_mode(&self) -> theme::Mode {
        self.theme_mode
    }

    pub fn background_color(&self) -> Color {
        self.style.background_color
    }

    pub fn text_color(&self) -> Color {
        self.style.text_color
    }

    pub fn update(
        &mut self,
        program: &program::Instance<P>,
        window: &Window,
        event: &WindowEvent,
    ) {
        match event {
            WindowEvent::Resized(new_size) => {
                let size = Size::new(new_size.width, new_size.height);

                self.viewport = Viewport::with_physical_size(
                    size,
                    window.scale_factor() as f32 * self.scale_factor,
                );
                self.surface_version += 1;
            }
            WindowEvent::ScaleFactorChanged {
                scale_factor: new_scale_factor,
                ..
            } => {
                let size = self.viewport.physical_size();

                self.viewport = Viewport::with_physical_size(
                    size,
                    *new_scale_factor as f32 * self.scale_factor,
                );
                self.surface_version += 1;
            }
            WindowEvent::CursorMoved { position, .. }
            | WindowEvent::Touch(Touch {
                location: position, ..
            }) => {
                self.cursor_position = Some(*position);
            }
            WindowEvent::CursorLeft { .. } => {
                self.cursor_position = None;
            }
            WindowEvent::ModifiersChanged(new_modifiers) => {
                self.modifiers = new_modifiers.state();
            }
            WindowEvent::ThemeChanged(theme) => {
                self.default_theme = <P::Theme as theme::Base>::default(
                    conversion::theme_mode(*theme),
                );

                if self.theme.is_none() {
                    self.style = program.style(&self.default_theme);
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }

    pub fn synchronize(
        &mut self,
        program: &program::Instance<P>,
        window_id: window::Id,
        window: &Window,
    ) {
        // Update window title
        let new_title = program.title(window_id);

        if self.title != new_title {
            window.set_title(&new_title);
            self.title = new_title;
        }

        // Update scale factor
        let new_scale_factor = program.scale_factor(window_id);

        if self.scale_factor != new_scale_factor {
            self.viewport = Viewport::with_physical_size(
                self.viewport.physical_size(),
                window.scale_factor() as f32 * new_scale_factor,
            );

            self.scale_factor = new_scale_factor;
        }

        // Update theme and appearance
        self.theme = program.theme(window_id);
        self.style = program.style(self.theme());

        let new_mode = self
            .theme
            .as_ref()
            .map(theme::Base::mode)
            .unwrap_or_default();

        if self.theme_mode != new_mode {
            if new_mode == theme::Mode::None {
                let (shown, guess) = release(
                    self.theme_mode,
                    theme::Base::mode(&self.default_theme),
                    TOLD_EVERY_SYSTEM_CHANGE,
                );

                window.set_theme(conversion::window_theme(shown));

                if let Some(guess) = guess {
                    self.default_theme =
                        <P::Theme as theme::Base>::default(guess);

                    if self.theme.is_none() {
                        self.style = program.style(&self.default_theme);
                    }
                }
            } else {
                window.set_theme(conversion::window_theme(new_mode));
            }

            self.theme_mode = new_mode;
        }
    }
}

/// Whether every window is told each change of the system's light or dark
/// mode, whatever theme the application forces: Linux by mundy's stream, and
/// Android and iOS by the runner's reads (`appearance`), both through
/// `NotifyTheme`. A window's default theme is then always the system's.
///
/// Elsewhere winit reports the window's own theme, which follows a forced
/// one, and after `set_theme(None)` sends `ThemeChanged` only when the
/// system's mode differs from the forced one.
const TOLD_EVERY_SYSTEM_CHANGE: bool = cfg!(any(
    target_os = "linux",
    target_os = "android",
    target_os = "ios"
));

/// What a window does when the application stops forcing a theme of mode
/// `forced` and lets the system decide, while its default theme has mode
/// `default`: the mode to give the window, and the mode to rebuild the
/// default theme with, if it must be guessed.
///
/// When the window is `told` every system change, its default theme is the
/// system's already and stays. Otherwise the forced mode is the guess: the
/// window followed it, and winit says so if the system's mode differs.
fn release(
    forced: theme::Mode,
    default: theme::Mode,
    told: bool,
) -> (theme::Mode, Option<theme::Mode>) {
    if told {
        (default, None)
    } else {
        (theme::Mode::None, Some(forced))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_released_theme_follows_the_system_where_every_change_is_told() {
        // A phone in light mode: the app forced Dark, then lets the system
        // decide. The default theme stays the system's Light.
        assert_eq!(
            release(theme::Mode::Dark, theme::Mode::Light, true),
            (theme::Mode::Light, None)
        );

        // macOS or Windows: the window followed the forced Dark, so Dark is
        // the guess until winit reports the system's mode.
        assert_eq!(
            release(theme::Mode::Dark, theme::Mode::Light, false),
            (theme::Mode::None, Some(theme::Mode::Dark))
        );
    }
}
