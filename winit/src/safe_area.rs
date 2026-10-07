//! The safe area: the edges of the screen that the system's own UI covers.
//!
//! The shell reads it from the platform whenever something may have changed
//! it, and publishes it to [`safe_area`] subscriptions when it did:
//!
//! - Android: the root view's `WindowInsets`, read through JNI, and the
//!   content rect `NativeActivity` reports, which stands in for them while
//!   they cannot be read (`safe_area/android.rs`).
//! - iOS: winit's safe-area frame against the window's bounds
//!   (`safe_area/ios.rs`).
//! - Elsewhere: [`SafeArea::ZERO`], once.
use crate::Control;
use crate::broadcast::Broadcast;
use crate::core::Padding;
use crate::core::theme;
use crate::futures::Subscription;
use crate::futures::futures::channel::mpsc;
use crate::graphics::Compositor;
use crate::program::Program;
use crate::window::{Window, WindowManager};

#[cfg(any(target_os = "android", test))]
use crate::core::time::{Duration, Instant};

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "ios")]
mod ios;

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
    /// The shell does not report the keyboard yet: it is always 0.
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
/// Android and iOS report it once the window exists and again on rotation
/// and a cutout change; the desktop and the web report
/// [`SafeArea::ZERO`] once. Headless tests (`iced_test`) have no shell and
/// report nothing. On phones every window fills the screen, so they share
/// one safe area: the one of the window that changed last.
pub fn safe_area() -> Subscription<SafeArea> {
    Subscription::run(subscribe)
}

/// The latest safe area, for subscriptions that start later.
static SAFE_AREA: Broadcast<SafeArea> = Broadcast::new(true);

fn subscribe() -> mpsc::UnboundedReceiver<SafeArea> {
    SAFE_AREA.subscribe()
}

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
    SAFE_AREA.reset();
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
        self.polls.changed(Instant::now());
    }

    /// Reads again what may have changed since the event loop last turned,
    /// and asks it to turn again when the next read is due.
    ///
    /// - iOS: every window, at every turn. These are a few property reads,
    ///   and UIKit may change the safe area without resizing the window (as
    ///   it moves it into its scene).
    /// - Android: when a poll is due, over the 600 ms after a change.
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
            let drawable = windows
                .iter_mut()
                .any(|(_id, window)| window.surface.is_some());

            // The native window is gone: nothing to read until it is back,
            // and `Resumed` reads it then.
            if !drawable {
                self.polls = Polls::default();
                return;
            }

            if self.polls.due(now) {
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
        SAFE_AREA.publish(area);
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

        SafeArea {
            insets: Padding {
                top,
                right,
                bottom,
                left,
            },
            keyboard: logical(self.keyboard),
        }
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

/// Android: the root view's insets, in physical pixels.
#[cfg(any(target_os = "android", test))]
#[derive(Debug, Clone, Copy, PartialEq)]
struct RootInsets {
    /// The system bars and the display cutout: top, right, bottom, left.
    bars: [i32; 4],
}

/// Android: the safe area of a `width` × `height` native window, from the
/// root view's insets when they could be read, else from the content rect
/// `NativeActivity` reports (`[left, top, right, bottom]`).
///
/// The content rect is where `NativeActivity`'s content view sits: inside
/// the bars on a system that does not draw edge to edge (before Android 15,
/// or below targetSdk 35), the whole window when it does. While the screen
/// rotates it can still have the old orientation's size: a rect that does
/// not fit in the window is such a stale one, and unknown.
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
            keyboard: 0.0,
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
}

#[cfg(any(target_os = "android", test))]
impl Polls {
    /// How often to read while the system bars settle, after a change.
    const SETTLE_EVERY: Duration = Duration::from_millis(100);
    /// For how long, after a change.
    const SETTLE_FOR: Duration = Duration::from_millis(600);

    /// Something changed at `now`: read again while it settles.
    fn changed(&mut self, now: Instant) {
        self.settle = Some((now + Self::SETTLE_EVERY, now + Self::SETTLE_FOR));
    }

    /// Whether a read is due at `now`. A poll that is due moves on to its
    /// next read, or ends.
    fn due(&mut self, now: Instant) -> bool {
        let Some((next, last)) = self.settle else {
            return false;
        };

        if next > now {
            return false;
        }

        let next = now + Self::SETTLE_EVERY;
        self.settle = (next <= last).then_some((next, last));

        true
    }

    /// When the next read is due, if one is.
    fn next(&self) -> Option<Instant> {
        self.settle.map(|(next, _last)| next)
    }
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
    fn android_takes_the_root_insets_and_falls_back_to_the_content_rect() {
        let root = RootInsets {
            bars: [142, 0, 63, 0],
        };

        // Edge to edge: the content rect is the whole window.
        assert_eq!(
            android_area(1080, 2424, [0, 0, 1080, 2424], Some(root)),
            Some(Physical {
                insets: [142.0, 0.0, 63.0, 0.0],
                keyboard: 0.0,
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
}
