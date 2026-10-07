//! The shell's side of the safe area: the edges of the screen that the
//! system's own UI covers.
//!
//! The shell reads it from the platform whenever something may have changed
//! it, and publishes it to [`safe_area`] subscriptions (`iced_runtime`'s
//! `safe_area` module) when it did:
//!
//! - Android: the root view's `WindowInsets`, read through JNI, and the
//!   content rect `NativeActivity` reports, which stands in for them while
//!   they cannot be read (`safe_area/android.rs`). A turn of the display by
//!   half a circle changes no size and no configuration, so a redraw makes
//!   the shell compare the display's rotation as well.
//! - iOS: winit's safe-area frame against the window's bounds, and the
//!   keyboard's frame from UIKit's notification (`safe_area/ios.rs`).
//! - Elsewhere: [`SafeArea::ZERO`], once.
use crate::Control;
use crate::core::theme;
use crate::futures::futures::channel::mpsc;
use crate::graphics::Compositor;
use crate::program::Program;
use crate::runtime;
use crate::window::{Window, WindowManager};

pub use crate::runtime::safe_area::{SafeArea, safe_area};

#[cfg(any(target_os = "android", target_os = "ios", test))]
use crate::core::Padding;
#[cfg(any(target_os = "android", test))]
use crate::core::time::{Duration, Instant};

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "ios")]
mod ios;

/// Keeps the `AndroidApp` of the application about to run on this thread,
/// for the JNI calls that read its insets.
#[cfg(target_os = "android")]
pub(crate) fn set_android_app(
    app: winit::platform::android::activity::AndroidApp,
) {
    android::set_app(app);
}

/// Forgets the safe area the last application reported, before a new one
/// starts its subscriptions: Android runs one application per Activity in
/// the same process, and the new window may not have the old one's area.
pub(crate) fn reset() {
    runtime::safe_area::reset();
}

/// The shell's side: reads the safe area when something may have changed
/// it, and publishes it when it did.
pub(crate) struct Shell {
    /// What each window reported last.
    windows: Vec<(winit::window::WindowId, SafeArea)>,
    /// What was published last.
    published: Option<SafeArea>,
    /// Android: when to read again.
    #[cfg(target_os = "android")]
    polls: Polls,
}

/// The application ends: its Activity is let go of.
#[cfg(target_os = "android")]
impl Drop for Shell {
    fn drop(&mut self) {
        android::forget_app();
    }
}

impl Shell {
    pub(crate) fn new() -> Self {
        #[cfg(target_os = "ios")]
        ios::observe_keyboard();

        Self {
            windows: Vec::new(),
            published: None,
            #[cfg(target_os = "android")]
            polls: Polls::default(),
        }
    }

    /// Reads the safe area of `window`, which was just opened, resized or
    /// given a new native window, and publishes it if it changed.
    ///
    /// On Android it is read again over the next 600 ms: the system bars
    /// and the content rect settle after the event that announced them.
    pub(crate) fn refresh<P, C>(&mut self, window: &Window<P, C>)
    where
        P: Program,
        C: Compositor<Renderer = P::Renderer>,
        P::Theme: theme::Base,
    {
        // Android: no native window, nothing to read.
        if window.surface.is_none() {
            return;
        }

        self.read(window);

        #[cfg(target_os = "android")]
        {
            self.polls.changed(Instant::now());

            // The rotation this area belongs to, for `redrawn`.
            if let Some(rotation) = android::rotation() {
                let _ = self.polls.rotated(rotation);
            }
        }
    }

    /// Android: a window is about to be drawn.
    ///
    /// Turning the display by half a circle (from one landscape to the
    /// other) moves the bars and the cutout to the other edges, but resizes
    /// no window and changes no configuration: Android only asks for a
    /// redraw. So after a redraw the next [`poll`](Self::poll) compares the
    /// display's rotation with the last one, at most every 250 ms, and reads
    /// the safe area again when it turned.
    #[cfg(target_os = "android")]
    pub(crate) fn redrawn(&mut self) {
        self.polls.redrawn(Instant::now());
    }

    /// Reads again what may have changed since the event loop last turned,
    /// and asks it to turn again when the next read is due.
    ///
    /// - iOS: every window, at every turn. These are a few property reads,
    ///   and UIKit may change the safe area without resizing the window (as
    ///   it moves it into its scene), or announce the keyboard's frame in a
    ///   notification that only wakes the loop.
    /// - Android: when a poll is due: over the 600 ms after a change, and
    ///   every 250 ms while a window asks for the keyboard and for a second
    ///   after, since the keyboard moves no window and sends no event once
    ///   the system draws edge to edge. And after a redraw, when the
    ///   display's rotation differs from the last one read
    ///   ([`redrawn`](Self::redrawn)).
    /// - Elsewhere: nothing.
    ///
    /// Called before the event loop's idle check, so that a poll is
    /// scheduled even when nothing else happened.
    pub(crate) fn poll<P, C>(
        &mut self,
        windows: &mut WindowManager<P, C>,
        control: &mut mpsc::UnboundedSender<Control>,
    ) where
        P: Program,
        C: Compositor<Renderer = P::Renderer>,
        P::Theme: theme::Base,
    {
        #[cfg(target_os = "ios")]
        {
            let _ = control;
            self.forget_closed(windows);

            for (_id, window) in windows.iter_mut() {
                self.read(window);
            }
        }

        #[cfg(target_os = "android")]
        {
            use winit::event_loop::ControlFlow;

            self.forget_closed(windows);

            let now = Instant::now();
            let mut drawable = false;
            let mut typing = false;

            for (_id, window) in windows.iter_mut() {
                if window.surface.is_some() {
                    drawable = true;
                    typing |= window.ime_requested();
                }
            }

            // The native window is gone: nothing to read until it is back,
            // and `Resumed` reads it then.
            if !drawable {
                self.polls = Polls::default();
                return;
            }

            self.polls.typing(typing, now);

            let mut due = self.polls.due(now);

            if self.polls.turn_due(now)
                && let Some(rotation) = android::rotation()
                && self.polls.rotated(rotation)
            {
                log::debug!("Safe area: the display turned ({rotation})");

                self.polls.changed(now);
                due = true;
            }

            if due {
                for (_id, window) in windows.iter_mut() {
                    if window.surface.is_some() {
                        self.read(window);
                    }
                }
            }

            if let Some(at) = self.polls.next() {
                let _ = control.start_send(Control::ChangeFlow(
                    ControlFlow::WaitUntil(at),
                ));
            }
        }

        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        let _ = (windows, control);
    }

    /// Forgets the windows that were closed.
    #[cfg(any(target_os = "android", target_os = "ios"))]
    fn forget_closed<P, C>(&mut self, windows: &mut WindowManager<P, C>)
    where
        P: Program,
        C: Compositor<Renderer = P::Renderer>,
        P::Theme: theme::Base,
    {
        let open: Vec<_> = windows
            .iter_mut()
            .map(|(_id, window)| window.raw.id())
            .collect();

        self.windows.retain(|(id, _area)| open.contains(id));
    }

    /// Reads the safe area of `window`, and publishes it if it is new for
    /// that window and not what was published last.
    fn read<P, C>(&mut self, window: &Window<P, C>)
    where
        P: Program,
        C: Compositor<Renderer = P::Renderer>,
        P::Theme: theme::Base,
    {
        let Some(area) = read(&window.raw, window.state.scale_factor()) else {
            return;
        };

        let id = window.raw.id();

        match self.windows.iter_mut().find(|(known, _area)| *known == id) {
            Some((_id, last)) if *last == area => return,
            Some((_id, last)) => *last = area,
            None => self.windows.push((id, area)),
        }

        if self.published == Some(area) {
            return;
        }

        log::debug!("Safe area: {area:?}");

        self.published = Some(area);
        crate::icm::safe_area(area.insets, area.keyboard);
        runtime::safe_area::publish(area);
    }
}

/// The safe area of `window`, whose scale factor (the application's
/// included) is `scale_factor`, or `None` while it cannot be known.
#[cfg(target_os = "android")]
fn read(window: &winit::window::Window, scale_factor: f32) -> Option<SafeArea> {
    android::read(window).map(|area| area.to_logical(scale_factor))
}

/// The safe area of `window`, whose scale factor (the application's
/// included) is `scale_factor`, or `None` while it cannot be known.
#[cfg(target_os = "ios")]
fn read(window: &winit::window::Window, scale_factor: f32) -> Option<SafeArea> {
    ios::read(window).map(|area| area.to_logical(scale_factor))
}

/// Nothing covers a desktop window or a web page.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn read(
    _window: &winit::window::Window,
    _scale_factor: f32,
) -> Option<SafeArea> {
    Some(SafeArea::ZERO)
}

/// A safe area in the platform's physical pixels.
#[cfg(any(target_os = "android", target_os = "ios", test))]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Physical {
    /// The top, right, bottom and left insets.
    insets: [f32; 4],
    /// The keyboard's height above the bottom edge.
    keyboard: f32,
}

#[cfg(any(target_os = "android", target_os = "ios", test))]
impl Physical {
    /// In logical pixels, for a scale factor that includes the
    /// application's own.
    fn to_logical(self, scale_factor: f32) -> SafeArea {
        let logical = |pixels: f32| pixels.max(0.0) / scale_factor;
        let [top, right, bottom, left] = self.insets.map(logical);

        SafeArea::new(Padding {
            top,
            right,
            bottom,
            left,
        })
        .with_keyboard(logical(self.keyboard))
    }
}

/// A rectangle in screen space: its origin and its size.
#[cfg(any(target_os = "ios", test))]
#[derive(Debug, Clone, Copy, PartialEq)]
struct Frame {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// iOS: the insets of the safe-area frame `inner` inside the window's
/// bounds `outer`: top, right, bottom and left.
#[cfg(any(target_os = "ios", test))]
fn frame_insets(outer: Frame, inner: Frame) -> [f32; 4] {
    [
        inner.y - outer.y,
        (outer.x + outer.width) - (inner.x + inner.width),
        (outer.y + outer.height) - (inner.y + inner.height),
        inner.x - outer.x,
    ]
    .map(|inset| inset.max(0.0) as f32)
}

/// iOS: how far `keyboard` reaches up into `window` from its bottom edge,
/// or 0 when it is off screen or beside it.
#[cfg(any(target_os = "ios", test))]
fn keyboard_overlap(window: Frame, keyboard: Frame) -> f32 {
    let beside = keyboard.x >= window.x + window.width
        || keyboard.x + keyboard.width <= window.x
        || keyboard.width <= 0.0
        || keyboard.height <= 0.0;

    if beside {
        return 0.0;
    }

    let bottom = window.y + window.height;
    let top = keyboard.y.max(window.y);

    (bottom - top).clamp(0.0, window.height) as f32
}

/// Android: the root view's insets, in physical pixels.
#[cfg(any(target_os = "android", test))]
#[derive(Debug, Clone, Copy, PartialEq)]
struct RootInsets {
    /// The system bars and the display cutout: top, right, bottom, left.
    bars: [i32; 4],
    /// The keyboard, from the bottom edge.
    keyboard: i32,
}

/// Android: the safe area of a `width` × `height` native window, from the
/// root view's insets when they could be read, else from the content rect
/// `NativeActivity` reports (`[left, top, right, bottom]`).
///
/// The content rect is where `NativeActivity`'s content view sits: inside
/// the bars on a system that does not draw edge to edge (before Android 15,
/// or below targetSdk 35), the whole window when it does. It stands in for
/// the bars only. On such a system the content view also makes room for the
/// keyboard, which the rect cannot tell from a bar, and while the screen
/// rotates the rect can still have the old orientation's size: a rect that
/// does not fit in the window is such a stale one, and unknown.
#[cfg(any(target_os = "android", test))]
fn android_area(
    width: u32,
    height: u32,
    content: [i32; 4],
    root: Option<RootInsets>,
) -> Option<Physical> {
    if width == 0 || height == 0 {
        return None;
    }

    if let Some(root) = root {
        return Some(Physical {
            insets: root.bars.map(|inset| inset.max(0) as f32),
            keyboard: root.keyboard.max(0) as f32,
        });
    }

    let width = i32::try_from(width).ok()?;
    let height = i32::try_from(height).ok()?;
    let [left, top, right, bottom] = content;

    // Empty before `NativeActivity` has laid its content view out.
    let fits = 0 <= left
        && left < right
        && right <= width
        && 0 <= top
        && top < bottom
        && bottom <= height;

    fits.then(|| Physical {
        insets: [top, width - right, height - bottom, left]
            .map(|inset| inset as f32),
        keyboard: 0.0,
    })
}

/// Android: when the shell reads the safe area again, beside the events that
/// announce a change.
#[cfg(any(target_os = "android", test))]
#[derive(Debug, Default)]
struct Polls {
    /// After a change: the next read, and the last one.
    settle: Option<(Instant, Instant)>,
    /// For the keyboard: the next read.
    keyboard: Option<Instant>,
    /// Whether a window asks for the keyboard.
    typing: bool,
    /// The last keyboard read after every window let the keyboard go.
    typed_until: Option<Instant>,
    /// After a redraw: when to compare the display's rotation.
    turn: Option<Instant>,
    /// When it was compared last.
    compared: Option<Instant>,
    /// The display's rotation, as read last.
    rotation: Option<i32>,
}

#[cfg(any(target_os = "android", test))]
impl Polls {
    /// How often to read while the system bars settle, after a change.
    const SETTLE_EVERY: Duration = Duration::from_millis(100);
    /// For how long, after a change.
    const SETTLE_FOR: Duration = Duration::from_millis(600);
    /// How often to read while a window asks for the keyboard.
    const KEYBOARD_EVERY: Duration = Duration::from_millis(250);
    /// For how long after the last window let it go: it slides away.
    const KEYBOARD_AFTER: Duration = Duration::from_secs(1);
    /// How often a redraw may compare the display's rotation.
    const TURN_EVERY: Duration = Duration::from_millis(250);

    /// Something changed at `now`: read again while it settles.
    fn changed(&mut self, now: Instant) {
        self.settle = Some((now + Self::SETTLE_EVERY, now + Self::SETTLE_FOR));
    }

    /// Whether a window asks for the keyboard, at `now`.
    fn typing(&mut self, typing: bool, now: Instant) {
        if typing == self.typing {
            return;
        }

        self.typing = typing;

        if typing {
            self.typed_until = None;
            self.keyboard = Some(now + Self::SETTLE_EVERY);
        } else {
            self.typed_until = Some(now + Self::KEYBOARD_AFTER);
            self.keyboard = self.keyboard.or(Some(now + Self::KEYBOARD_EVERY));
        }
    }

    /// Whether a read is due at `now`. Each poll that is due moves on to its
    /// next read, or ends.
    fn due(&mut self, now: Instant) -> bool {
        let mut due = false;

        if let Some((next, last)) = self.settle
            && next <= now
        {
            due = true;

            let next = now + Self::SETTLE_EVERY;
            self.settle = (next <= last).then_some((next, last));
        }

        if let Some(next) = self.keyboard
            && next <= now
        {
            due = true;

            let next = now + Self::KEYBOARD_EVERY;
            let wanted = self.typing
                || self.typed_until.is_some_and(|until| next <= until);

            self.keyboard = wanted.then_some(next);
        }

        due
    }

    /// A window was drawn at `now`: compare the display's rotation soon, no
    /// sooner than [`TURN_EVERY`](Self::TURN_EVERY) after the last time.
    fn redrawn(&mut self, now: Instant) {
        if self.turn.is_none() {
            let earliest = self
                .compared
                .map_or(now, |compared| compared + Self::TURN_EVERY);

            self.turn = Some(earliest.max(now));
        }
    }

    /// Whether the display's rotation is to be compared at `now`.
    fn turn_due(&mut self, now: Instant) -> bool {
        match self.turn {
            Some(at) if at <= now => {
                self.turn = None;
                self.compared = Some(now);

                true
            }
            _ => false,
        }
    }

    /// The display's rotation is `rotation`: whether it turned since the
    /// last one read.
    fn rotated(&mut self, rotation: i32) -> bool {
        self.rotation
            .replace(rotation)
            .is_some_and(|last| last != rotation)
    }

    /// When the next read or comparison is due, if one is.
    fn next(&self) -> Option<Instant> {
        let settle = self.settle.map(|(next, _last)| next);

        [settle, self.keyboard, self.turn]
            .into_iter()
            .flatten()
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_pixels_become_logical_with_the_app_scale_factor() {
        let physical = Physical {
            insets: [186.0, 0.0, 102.0, -3.0],
            keyboard: 1008.0,
        };

        let area = physical.to_logical(3.0);
        assert_eq!(area.insets, Padding::ZERO.top(62.0).bottom(34.0));
        assert_eq!(area.keyboard, 336.0);

        // An application scale factor of 2 on a 3x screen: 6 pixels a unit.
        let area = physical.to_logical(6.0);
        assert_eq!(area.insets.top, 31.0);
        assert_eq!(area.keyboard, 168.0);
    }

    #[test]
    fn ios_insets_are_the_safe_frame_inside_the_bounds() {
        let frame = |x, y, width, height| Frame {
            x,
            y,
            width,
            height,
        };

        // Portrait, Dynamic Island (iPhone 17 at 3x).
        let bounds = frame(0.0, 0.0, 1206.0, 2622.0);
        assert_eq!(
            frame_insets(bounds, frame(0.0, 186.0, 1206.0, 2334.0)),
            [186.0, 0.0, 102.0, 0.0]
        );

        // Landscape: the island on the left, the home indicator below.
        let bounds = frame(0.0, 0.0, 2622.0, 1206.0);
        assert_eq!(
            frame_insets(bounds, frame(186.0, 0.0, 2250.0, 1143.0)),
            [0.0, 186.0, 63.0, 186.0]
        );

        // Before the window is in its scene: no safe area yet.
        assert_eq!(frame_insets(bounds, bounds), [0.0; 4]);
    }

    #[test]
    fn the_ios_keyboard_counts_where_it_covers_the_window() {
        let frame = |x, y, width, height| Frame {
            x,
            y,
            width,
            height,
        };
        let window = frame(0.0, 0.0, 402.0, 874.0);

        assert_eq!(
            keyboard_overlap(window, frame(0.0, 538.0, 402.0, 336.0)),
            336.0
        );

        // Hidden: below the screen.
        assert_eq!(
            keyboard_overlap(window, frame(0.0, 874.0, 402.0, 336.0)),
            0.0
        );

        // Beside the window, or empty.
        assert_eq!(
            keyboard_overlap(window, frame(402.0, 538.0, 402.0, 336.0)),
            0.0
        );
        assert_eq!(keyboard_overlap(window, frame(0.0, 0.0, 0.0, 0.0)), 0.0);
    }

    #[test]
    fn android_takes_the_root_insets_and_falls_back_to_the_content_rect() {
        let root = RootInsets {
            bars: [142, 0, 63, 0],
            keyboard: 0,
        };

        // Edge to edge: the content rect is the whole window.
        assert_eq!(
            android_area(1080, 2424, [0, 0, 1080, 2424], Some(root)),
            Some(Physical {
                insets: [142.0, 0.0, 63.0, 0.0],
                keyboard: 0.0,
            })
        );

        // Typing: the root insets have the keyboard; a content view that
        // made room for it changes nothing.
        let typing = RootInsets {
            bars: [142, 0, 126, 0],
            keyboard: 883,
        };
        assert_eq!(
            android_area(1080, 2424, [0, 142, 1080, 1541], Some(typing)),
            Some(Physical {
                insets: [142.0, 0.0, 126.0, 0.0],
                keyboard: 883.0,
            })
        );

        // The insets cannot be read: the content rect stands in for them.
        assert_eq!(
            android_area(1080, 2424, [0, 142, 1080, 2298], None),
            Some(Physical {
                insets: [142.0, 0.0, 126.0, 0.0],
                keyboard: 0.0,
            })
        );

        // Not laid out yet, the landscape rect of a window that has just
        // turned to portrait, or no window.
        assert_eq!(android_area(1080, 2424, [0, 0, 0, 0], None), None);
        assert_eq!(android_area(1080, 2424, [0, 0, 2424, 1080], None), None);
        assert_eq!(android_area(0, 0, [0, 0, 1080, 2424], Some(root)), None);
    }

    #[test]
    fn android_polls_settle_after_a_change() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut polls = Polls::default();

        assert_eq!(polls.next(), None);

        polls.changed(start);
        assert_eq!(polls.next(), Some(at(100)));
        assert!(!polls.due(at(50)));

        let mut reads = 0;
        let mut now = at(100);

        while let Some(next) = polls.next() {
            now = next.max(now);
            assert!(polls.due(now));
            reads += 1;
        }

        assert_eq!(reads, 6);
        assert!(now <= at(600));
    }

    #[test]
    fn android_polls_compare_the_rotation_after_a_redraw() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut polls = Polls::default();

        // The rotation of the first read is no turn.
        assert!(!polls.rotated(1));

        // A redraw compares at once, the first time.
        polls.redrawn(start);
        assert_eq!(polls.next(), Some(start));
        assert!(polls.turn_due(start));
        assert!(!polls.rotated(1));
        assert_eq!(polls.next(), None);

        // Frames 16 ms apart compare once every 250 ms.
        polls.redrawn(at(16));
        assert_eq!(polls.next(), Some(at(250)));
        assert!(!polls.turn_due(at(32)));
        polls.redrawn(at(32));
        assert_eq!(polls.next(), Some(at(250)));
        assert!(polls.turn_due(at(250)));

        // From ROTATION_90 to ROTATION_270: a turn, once.
        assert!(polls.rotated(3));
        assert!(!polls.rotated(3));

        // A redraw long after compares at once again.
        polls.redrawn(at(2000));
        assert!(polls.turn_due(at(2000)));
    }

    #[test]
    fn android_polls_follow_the_keyboard_and_end_after_it() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut polls = Polls::default();

        polls.typing(true, start);
        assert_eq!(polls.next(), Some(at(100)));

        // Every 250 ms for as long as a window asks for it.
        assert!(polls.due(at(100)));
        assert_eq!(polls.next(), Some(at(350)));
        assert!(polls.due(at(350)));
        assert_eq!(polls.next(), Some(at(600)));

        // Let go at 500 ms: a second more, then nothing.
        polls.typing(false, at(500));

        let mut now = at(500);
        while let Some(next) = polls.next() {
            now = next;
            assert!(polls.due(now));
        }

        assert!(now > at(1250) && now <= at(1500), "{:?}", now - start);

        // Asking again starts over.
        polls.typing(true, at(2000));
        assert_eq!(polls.next(), Some(at(2100)));
    }
}
