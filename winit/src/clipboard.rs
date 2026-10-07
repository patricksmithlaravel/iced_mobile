//! Access the clipboard.
//!
//! On Android and iOS, the standard clipboard is the system's:
//! `ClipboardManager` and `UIPasteboard`. It needs no window, so an
//! [unconnected](Clipboard::unconnected) [`Clipboard`] reads and writes it
//! too. Phones have no primary clipboard: reading it gives `None`, and
//! writing to it does nothing.

use crate::core::clipboard::Kind;
use std::sync::Arc;
use winit::window::{Window, WindowId};

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "ios")]
mod ios;

// window_clipboard connects on phones, but every read and write fails there.
#[cfg(target_os = "android")]
use android as system;
#[cfg(target_os = "ios")]
use ios as system;

/// A buffer for short-term storage and transfer within and between
/// applications.
pub struct Clipboard {
    state: State,
}

enum State {
    Connected {
        clipboard: window_clipboard::Clipboard,
        // Held until drop to satisfy the safety invariants of
        // `window_clipboard::Clipboard`.
        //
        // Note that the field ordering is load-bearing.
        #[allow(dead_code)]
        window: Arc<Window>,
    },
    Unavailable,
}

impl Clipboard {
    /// Creates a new [`Clipboard`] for the given window.
    pub fn connect(window: Arc<Window>) -> Clipboard {
        // SAFETY: The window handle will stay alive throughout the entire
        // lifetime of the `window_clipboard::Clipboard` because we hold
        // the `Arc<Window>` together with `State`, and enum variant fields
        // get dropped in declaration order.
        #[allow(unsafe_code)]
        let clipboard =
            unsafe { window_clipboard::Clipboard::connect(&window) };

        let state = match clipboard {
            Ok(clipboard) => State::Connected { clipboard, window },
            Err(_) => State::Unavailable,
        };

        Clipboard { state }
    }

    /// Creates a new [`Clipboard`] that isn't associated with a window.
    /// This clipboard will never contain a copied value, except on Android
    /// and iOS, where it reads and writes the system's clipboard.
    pub fn unconnected() -> Clipboard {
        Clipboard {
            state: State::Unavailable,
        }
    }

    /// Reads the current content of the [`Clipboard`] as text.
    pub fn read(&self, kind: Kind) -> Option<String> {
        #[cfg(any(target_os = "android", target_os = "ios"))]
        if kind == Kind::Standard {
            return system::read();
        }

        match &self.state {
            State::Connected { clipboard, .. } => match kind {
                Kind::Standard => clipboard.read().ok(),
                Kind::Primary => clipboard.read_primary().and_then(Result::ok),
            },
            State::Unavailable => None,
        }
    }

    /// Writes the given text contents to the [`Clipboard`].
    pub fn write(&mut self, kind: Kind, contents: String) {
        #[cfg(any(target_os = "android", target_os = "ios"))]
        if kind == Kind::Standard {
            system::write(&contents);
            return;
        }

        match &mut self.state {
            State::Connected { clipboard, .. } => {
                let result = match kind {
                    Kind::Standard => clipboard.write(contents),
                    Kind::Primary => {
                        clipboard.write_primary(contents).unwrap_or(Ok(()))
                    }
                };

                match result {
                    Ok(()) => {}
                    Err(error) => {
                        log::warn!("error writing to clipboard: {error}");
                    }
                }
            }
            State::Unavailable => {}
        }
    }

    /// Returns the identifier of the window used to create the [`Clipboard`], if any.
    pub fn window_id(&self) -> Option<WindowId> {
        match &self.state {
            State::Connected { window, .. } => Some(window.id()),
            State::Unavailable => None,
        }
    }
}

impl crate::core::Clipboard for Clipboard {
    fn read(&self, kind: Kind) -> Option<String> {
        self.read(kind)
    }

    fn write(&mut self, kind: Kind, contents: String) {
        self.write(kind, contents);
    }
}
