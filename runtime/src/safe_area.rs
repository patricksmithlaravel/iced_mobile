//! The safe area: the edges of the screen that the system's own UI covers.
//!
//! A shell reads the safe area from the platform and [publishes](publish)
//! it here; applications receive it with [`safe_area`]. `iced_winit`
//! publishes the insets and the keyboard of Android and iOS, and
//! [`SafeArea::ZERO`] on the desktop and the web. `iced_test`'s headless
//! harness publishes the safe area of the phone its viewport stands for.
use crate::core::Padding;
use crate::futures::futures::channel::mpsc;
use crate::futures::subscription::Subscription;

use std::sync::{Mutex, PoisonError};

/// What covers the edges of the screen the application fills, in the same
/// logical pixels as its layout.
///
/// On a phone the application draws under the status bar, the notch or
/// Dynamic Island, the home indicator or navigation bar, and the on-screen
/// keyboard. Pad the root of the view with [`SafeArea::padding`] to keep
/// its content clear of them, and receive the area with [`safe_area`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[non_exhaustive]
pub struct SafeArea {
    /// The system's own UI over each edge: the status bar, the notch, Dynamic
    /// Island or display cutout, and the home indicator or navigation bar.
    pub insets: Padding,
    /// How far the on-screen keyboard reaches up from the bottom edge; 0 while
    /// it is hidden.
    ///
    /// It is measured from the bottom edge, so it includes the bottom inset
    /// it covers: take the larger of the two, as [`SafeArea::padding`] does.
    pub keyboard: f32,
}

impl SafeArea {
    /// Nothing covered: the desktop and the web.
    pub const ZERO: Self = Self {
        insets: Padding::ZERO,
        keyboard: 0.0,
    };

    /// A safe area with these insets and no keyboard (for tests and previews).
    pub const fn new(insets: Padding) -> Self {
        Self {
            insets,
            keyboard: 0.0,
        }
    }

    /// The same safe area with a keyboard of this height.
    pub const fn with_keyboard(self, height: f32) -> Self {
        Self {
            keyboard: height,
            ..self
        }
    }

    /// The padding for a root container: `margin` plus the inset on each
    /// edge, the bottom raised to the keyboard's top while it shows.
    pub fn padding(self, margin: impl Into<Padding>) -> Padding {
        let margin = margin.into();

        Padding {
            top: margin.top + self.insets.top,
            right: margin.right + self.insets.right,
            bottom: margin.bottom + self.insets.bottom.max(self.keyboard),
            left: margin.left + self.insets.left,
        }
    }
}

/// The safe area: the current one as soon as the shell knows it, then every
/// change.
///
/// Android and iOS report it once the window exists and again on rotation,
/// a cutout change and the keyboard; the desktop and the web report
/// [`SafeArea::ZERO`] once. `iced_test`'s headless harness reports the safe
/// area of the phone its viewport stands for, and nothing for a viewport of
/// another size; its `Simulator` runs no subscription. On phones every
/// window fills the screen, so they share one safe area: the one of the
/// window that changed last.
pub fn safe_area() -> Subscription<SafeArea> {
    Subscription::run(subscribe)
}

/// Hands a new safe area to every [`safe_area`] subscription, and keeps it
/// for those that start later. Shells and test harnesses call it, when the
/// safe area is first known and whenever it changes.
pub fn publish(area: SafeArea) {
    let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);

    state
        .subscribers
        .retain(|subscriber| subscriber.unbounded_send(area).is_ok());
    state.latest = Some(area);
}

/// Forgets the safe area published last, before a new application starts
/// its subscriptions: a shell can run one application after another in the
/// same process (Android runs one per Activity), and the next one's window
/// may not have the same area.
pub fn reset() {
    STATE.lock().unwrap_or_else(PoisonError::into_inner).latest = None;
}

/// The latest safe area, and the subscriptions listening.
static STATE: Mutex<State> = Mutex::new(State {
    latest: None,
    subscribers: Vec::new(),
});

struct State {
    latest: Option<SafeArea>,
    subscribers: Vec<mpsc::UnboundedSender<SafeArea>>,
}

/// The areas published from now on, the latest first.
fn subscribe() -> mpsc::UnboundedReceiver<SafeArea> {
    let (sender, receiver) = mpsc::unbounded();
    let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);

    if let Some(latest) = state.latest {
        let _ = sender.unbounded_send(latest);
    }

    state.subscribers.push(sender);
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISLAND: SafeArea = SafeArea::new(Padding {
        top: 62.0,
        right: 0.0,
        bottom: 34.0,
        left: 0.0,
    });

    #[test]
    fn padding_adds_the_insets_and_rises_with_the_keyboard() {
        assert_eq!(
            ISLAND.padding(16),
            Padding {
                top: 78.0,
                right: 16.0,
                bottom: 50.0,
                left: 16.0,
            }
        );

        // The keyboard covers the home indicator: the larger one counts.
        assert_eq!(ISLAND.with_keyboard(336.0).padding(16).bottom, 352.0);
        assert_eq!(ISLAND.with_keyboard(20.0).padding(16).bottom, 50.0);

        assert_eq!(SafeArea::ZERO.padding(8), Padding::new(8.0));
        assert_eq!(SafeArea::default(), SafeArea::ZERO);
    }

    /// The only test that touches the process-wide state.
    #[test]
    fn subscribers_get_the_latest_then_every_change_until_a_reset() {
        // `try_recv` replaces it in futures 0.3.32, which some lockfiles
        // do not have yet.
        #[allow(deprecated)]
        let next = |receiver: &mut mpsc::UnboundedReceiver<SafeArea>| {
            receiver.try_next().ok().flatten()
        };

        reset();
        publish(ISLAND);

        let mut early = subscribe();
        assert_eq!(next(&mut early), Some(ISLAND));

        let typing = ISLAND.with_keyboard(336.0);
        publish(typing);
        assert_eq!(next(&mut early), Some(typing));
        assert_eq!(next(&mut early), None);

        // A dropped subscriber is let go of at the next publish.
        drop(subscribe());
        publish(ISLAND);
        assert_eq!(STATE.lock().unwrap().subscribers.len(), 1);

        // After a reset, a new subscriber waits for the next value.
        reset();
        let mut late = subscribe();
        assert_eq!(next(&mut late), None);

        publish(SafeArea::ZERO);
        assert_eq!(next(&mut late), Some(SafeArea::ZERO));
    }
}
