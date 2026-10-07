//! Android: the modifier keys of a hardware keyboard.
//!
//! winit's Android backend never sends `ModifiersChanged`, and text fields
//! take Ctrl only from it, so Ctrl+C, X, V and A from a hardware keyboard (an
//! emulator's host keyboard, ChromeOS, a keyboard over USB or Bluetooth)
//! never reached them. The shell follows the presses and releases of the
//! modifier keys instead, and gives each change to the window as
//! `ModifiersChanged`. iOS hardware keyboards send no modifier keys at all.
use winit::event::ElementState;
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};

/// The modifier keys, each with the state it gives.
const KEYS: [(KeyCode, ModifiersState); 8] = [
    (KeyCode::ShiftLeft, ModifiersState::SHIFT),
    (KeyCode::ShiftRight, ModifiersState::SHIFT),
    (KeyCode::ControlLeft, ModifiersState::CONTROL),
    (KeyCode::ControlRight, ModifiersState::CONTROL),
    (KeyCode::AltLeft, ModifiersState::ALT),
    (KeyCode::AltRight, ModifiersState::ALT),
    (KeyCode::SuperLeft, ModifiersState::SUPER),
    (KeyCode::SuperRight, ModifiersState::SUPER),
];

/// The modifier keys held down, the left and right ones apart.
#[derive(Debug, Default)]
pub(crate) struct Modifiers {
    /// One bit per entry of [`KEYS`].
    held: u8,
}

impl Modifiers {
    /// The state the held keys give.
    pub(crate) fn state(&self) -> ModifiersState {
        KEYS.iter()
            .enumerate()
            .filter(|(index, _)| self.held & (1 << index) != 0)
            .fold(ModifiersState::empty(), |state, (_, (_, modifier))| {
                state | *modifier
            })
    }

    /// Follows a key's press or release: the new state, when it changed.
    pub(crate) fn key(
        &mut self,
        key: PhysicalKey,
        state: ElementState,
    ) -> Option<ModifiersState> {
        let PhysicalKey::Code(code) = key else {
            return None;
        };

        let index = KEYS.iter().position(|(key, _)| *key == code)?;
        let before = self.state();

        match state {
            ElementState::Pressed => self.held |= 1 << index,
            ElementState::Released => self.held &= !(1 << index),
        }

        let after = self.state();

        (after != before).then_some(after)
    }

    /// Lets go of every key, when the window loses the focus or the native
    /// window goes away: a key released meanwhile never reaches the app.
    /// The new state, when it changed.
    pub(crate) fn release_all(&mut self) -> Option<ModifiersState> {
        let before = self.state();

        self.held = 0;

        (!before.is_empty()).then_some(ModifiersState::empty())
    }

    /// Follows a window event: a modifier key, or the loss of focus.
    #[cfg(target_os = "android")]
    pub(crate) fn update(
        &mut self,
        event: &winit::event::WindowEvent,
    ) -> Option<ModifiersState> {
        use winit::event::WindowEvent;

        match event {
            WindowEvent::KeyboardInput { event, .. } => {
                self.key(event.physical_key, event.state)
            }
            WindowEvent::Focused(false) => self.release_all(),
            _ => None,
        }
    }
}

/// Gives `window` the modifier `state`, as winit's `ModifiersChanged` would:
/// its own record, read by the key events that follow, and a
/// `keyboard::Event::ModifiersChanged` for its widgets.
#[cfg(target_os = "android")]
pub(crate) fn notify<P, C>(
    program: &crate::program::Instance<P>,
    id: crate::window::Id,
    window: &mut crate::window::Window<P, C>,
    state: ModifiersState,
    events: &mut Vec<(crate::window::Id, crate::core::Event)>,
) where
    P: crate::program::Program,
    C: crate::graphics::Compositor<Renderer = P::Renderer>,
    P::Theme: crate::core::theme::Base,
{
    let event = winit::event::WindowEvent::ModifiersChanged(state.into());

    window.state.update(program, &window.raw, &event);

    if let Some(event) = crate::conversion::window_event(
        event,
        window.state.scale_factor(),
        window.state.modifiers(),
    ) {
        events.push((id, event));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ElementState::{Pressed, Released};

    fn key(code: KeyCode) -> PhysicalKey {
        PhysicalKey::Code(code)
    }

    #[test]
    fn left_and_right_keys_are_held_apart() {
        let mut modifiers = Modifiers::default();

        assert_eq!(
            modifiers.key(key(KeyCode::ControlLeft), Pressed),
            Some(ModifiersState::CONTROL)
        );
        assert_eq!(modifiers.key(key(KeyCode::ControlRight), Pressed), None);
        assert_eq!(modifiers.key(key(KeyCode::ControlLeft), Released), None);
        assert_eq!(modifiers.state(), ModifiersState::CONTROL);
        assert_eq!(
            modifiers.key(key(KeyCode::ControlRight), Released),
            Some(ModifiersState::empty())
        );
    }

    #[test]
    fn modifiers_combine_and_repeats_change_nothing() {
        let mut modifiers = Modifiers::default();

        assert_eq!(
            modifiers.key(key(KeyCode::ShiftLeft), Pressed),
            Some(ModifiersState::SHIFT)
        );
        assert_eq!(modifiers.key(key(KeyCode::ShiftLeft), Pressed), None);
        assert_eq!(
            modifiers.key(key(KeyCode::ControlRight), Pressed),
            Some(ModifiersState::SHIFT | ModifiersState::CONTROL)
        );
        assert_eq!(
            modifiers.key(key(KeyCode::AltLeft), Pressed),
            Some(
                ModifiersState::SHIFT
                    | ModifiersState::CONTROL
                    | ModifiersState::ALT
            )
        );
        assert_eq!(
            modifiers.key(key(KeyCode::SuperRight), Pressed),
            Some(
                ModifiersState::SHIFT
                    | ModifiersState::CONTROL
                    | ModifiersState::ALT
                    | ModifiersState::SUPER
            )
        );
        assert_eq!(
            modifiers.key(key(KeyCode::ShiftLeft), Released),
            Some(
                ModifiersState::CONTROL
                    | ModifiersState::ALT
                    | ModifiersState::SUPER
            )
        );
    }

    #[test]
    fn other_keys_and_stray_releases_change_nothing() {
        let mut modifiers = Modifiers::default();

        assert_eq!(modifiers.key(key(KeyCode::KeyC), Pressed), None);
        assert_eq!(modifiers.key(key(KeyCode::ShiftRight), Released), None);
        assert_eq!(
            modifiers.key(
                PhysicalKey::Unidentified(
                    winit::keyboard::NativeKeyCode::Unidentified
                ),
                Pressed
            ),
            None
        );
        assert_eq!(modifiers.state(), ModifiersState::empty());
    }

    #[test]
    fn release_all_lets_go_of_held_keys() {
        let mut modifiers = Modifiers::default();

        assert_eq!(modifiers.release_all(), None);

        let _ = modifiers.key(key(KeyCode::ControlLeft), Pressed);
        let _ = modifiers.key(key(KeyCode::ShiftRight), Pressed);

        assert_eq!(modifiers.release_all(), Some(ModifiersState::empty()));
        assert_eq!(modifiers.state(), ModifiersState::empty());
        assert_eq!(modifiers.release_all(), None);

        // A release that arrives after the focus came back is ignored, and
        // the next press counts again.
        assert_eq!(modifiers.key(key(KeyCode::ControlLeft), Released), None);
        assert_eq!(
            modifiers.key(key(KeyCode::ControlLeft), Pressed),
            Some(ModifiersState::CONTROL)
        );
    }
}
