//! The application's life: the hook a shell runs at once on what winit
//! reports, and the transitions [`lifecycle`] delivers to `update`.
use crate::broadcast::Broadcast;
use crate::futures::Subscription;
use crate::icm;

#[cfg(any(target_os = "android", test))]
use crate::core::time::{Duration, Instant};

use std::sync::{Mutex, OnceLock, PoisonError};

/// The application's life, as two channels report it.
///
/// [`on_lifecycle`] runs a hook on the event loop's thread, before iced acts,
/// with what winit says: [`Suspended`](Self::Suspended) and
/// [`Resumed`](Self::Resumed), and nothing else.
///
/// | Platform | `Suspended` | `Resumed` |
/// |---|---|---|
/// | Android | the native window is going away, as the application leaves the screen | the native window exists, at launch and on return |
/// | iOS | the application is about to stop being active, which also happens for Control Center, notifications and Face ID | it became active, at launch and on return |
/// | Web | the page is hidden into the back-forward cache (`pagehide`, persisted) | at launch, and when the page comes back from that cache (`pageshow`) |
/// | Desktop | never | once, at launch |
///
/// [`lifecycle()`] delivers the other five to `update`, as messages, a
/// moment after the event:
///
/// | Variant | iOS | Android | Web | Desktop |
/// |---|---|---|---|---|
/// | `Foreground` | `willEnterForeground`, and at launch | the native window exists: at launch and on every return | at launch, and `pageshow` from the back-forward cache | once, at launch |
/// | `Active` | `didBecomeActive` | the window gained the focus while visible | with `Foreground` | once, at launch |
/// | `Inactive` | `willResignActive`: Control Center, Notification Center, Face ID, calls, the app switcher, and before `Background` | the window lost the focus: the notification shade, system dialogs and permission prompts, Recents, split screen, and before `Background` | before `Background` | never |
/// | `Background` | `didEnterBackground` | the native window is gone: Home, another app, the screen turned off, and before Android destroys the Activity | `pagehide` into the back-forward cache | never |
/// | `MemoryWarning` | `didReceiveMemoryWarning` | NativeActivity: `onLowMemory`, which is rare (`onTrimMemory` does not reach the app). GameActivity: every `onTrimMemory`, `TRIM_MEMORY_UI_HIDDEN` on each trip to the background included, without its level | never | never |
///
/// The four states follow one another as `Background`, `Foreground`, then
/// `Active` and `Inactive` in turn, then `Background`: `Foreground` always
/// comes before `Active`, `Inactive` always between `Active` and
/// `Background`, and none repeats. An application that comes back and leaves
/// again without taking input goes from `Foreground` straight to
/// `Background`. A Face ID prompt or Control Center gives
/// `Inactive` then `Active`, never `Background`, so locking on `Background`
/// cannot loop through the unlock's own prompt.
///
/// More variants may be added, so a `match` on it needs a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Lifecycle {
    /// [`on_lifecycle`] only. winit's `Suspended`: on Android the native
    /// window is going away, as the application leaves the screen; on iOS
    /// it is about to stop being active.
    Suspended,
    /// [`on_lifecycle`] only. winit's `Resumed`.
    Resumed,
    /// [`lifecycle()`] only. The application became visible: at launch, and
    /// on every return from the background.
    Foreground,
    /// [`lifecycle()`] only. It is visible and takes input.
    Active,
    /// [`lifecycle()`] only. It stopped taking input; it may still be
    /// visible.
    Inactive,
    /// [`lifecycle()`] only. It is no longer visible.
    Background,
    /// [`lifecycle()`] only. The system is short of memory: free caches and
    /// whatever can be built again. On Android with GameActivity it also
    /// comes each time the application goes to the background (see the
    /// table).
    MemoryWarning,
}

impl Lifecycle {
    /// The name `ICM_EVENT` lines give it.
    fn name(self) -> &'static str {
        match self {
            Self::Suspended => "suspended",
            Self::Resumed => "resumed",
            Self::Foreground => "foreground",
            Self::Active => "active",
            Self::Inactive => "inactive",
            Self::Background => "background",
            Self::MemoryWarning => "memory_warning",
        }
    }
}

/// The hook of [`on_lifecycle`].
static HOOK: OnceLock<fn(Lifecycle)> = OnceLock::new();

/// Calls `hook` on the event loop's thread whenever winit reports the
/// application suspended or resumed, before iced acts on it.
///
/// The hook receives [`Lifecycle::Suspended`] and [`Lifecycle::Resumed`]
/// only. To react to the application's life in `update`, subscribe to
/// [`lifecycle()`] instead: its messages arrive a moment later, so what must
/// be done before the system acts (saving what must outlive the process)
/// belongs here.
///
/// Set it before [`run`](crate::run). There is one hook for the process: only
/// the first call sets it. A later call with the same function does nothing,
/// so an application can set it each time it starts (on Android, each time
/// `android_main` runs for a new Activity). A later call with another
/// function is ignored with a warning in the log.
pub fn on_lifecycle(hook: fn(Lifecycle)) {
    if let Err(hook) = HOOK.set(hook)
        && HOOK
            .get()
            .is_some_and(|current| !std::ptr::fn_addr_eq(*current, hook))
    {
        log::warn!(
            "on_lifecycle: a hook is already set, and only the first one is \
            called; this one is ignored. Call the second from the first \
            instead."
        );
    }
}

/// The application's life as it changes, for `update`:
/// [`Foreground`](Lifecycle::Foreground), [`Active`](Lifecycle::Active),
/// [`Inactive`](Lifecycle::Inactive), [`Background`](Lifecycle::Background)
/// and [`MemoryWarning`](Lifecycle::MemoryWarning), with the meanings and
/// the order in the table on [`Lifecycle`].
///
/// ```no_run
/// # mod iced { pub use iced_winit::futures::Subscription; pub mod mobile { pub use iced_winit::{Lifecycle, lifecycle}; } }
/// use iced::Subscription;
/// use iced::mobile::{self, Lifecycle};
///
/// #[derive(Debug, Clone)]
/// enum Message {
///     Lifecycle(Lifecycle),
/// }
///
/// struct Wallet {
///     hidden: bool,
///     locked: bool,
/// }
///
/// impl Wallet {
///     fn update(&mut self, message: Message) {
///         match message {
///             Message::Lifecycle(Lifecycle::Inactive) => self.hidden = true,
///             Message::Lifecycle(Lifecycle::Active) => self.hidden = false,
///             Message::Lifecycle(Lifecycle::Background) => self.locked = true,
///             Message::Lifecycle(_) => {}
///         }
///     }
///
///     fn subscription(&self) -> Subscription<Message> {
///         mobile::lifecycle().map(Message::Lifecycle)
///     }
/// }
/// ```
///
/// A subscription sees the changes made while it runs: one the application
/// returns from `subscription` from the start, the usual case, sees the first
/// `Foreground` and `Active`. Headless tests (`iced_test`) have no shell and
/// see nothing; send the messages to `update` yourself.
///
/// The messages reach `update` a moment after the event, through the
/// executor: what must happen before the system acts goes in
/// [`on_lifecycle`]. When Android destroys the Activity (Back), the
/// application can end before its `Background` arrives. Android lets the
/// application draw only while the Activity runs, so hiding content on
/// `Inactive` does not reliably keep it out of the Recents thumbnail there:
/// set `FLAG_SECURE` on the window for that.
pub fn lifecycle() -> Subscription<Lifecycle> {
    Subscription::run(|| EVENTS.subscribe())
}

/// The transitions, to every [`lifecycle`] subscription.
static EVENTS: Broadcast<Lifecycle> = Broadcast::new(false);

/// The visibility and focus of the application running now.
static TRACKER: Mutex<Tracker> = Mutex::new(Tracker::new());

/// Starts tracking a new application, from the background. Android runs one
/// per Activity in the same process.
pub(crate) fn start() {
    *TRACKER.lock().unwrap_or_else(PoisonError::into_inner) = Tracker::new();

    EVENTS.reset();

    #[cfg(target_os = "android")]
    {
        *WAKEUPS.lock().unwrap_or_else(PoisonError::into_inner) =
            Wakeups::new();
    }
}

/// Reports what winit said.
///
/// For `Resumed` and `Suspended`, it emits the `lifecycle` event and runs
/// the hook first, before iced acts on them, as it always did. Then it
/// publishes the transitions the input causes to every [`lifecycle`]
/// subscription, each with an `app_state` event.
pub(crate) fn report(input: Input) {
    if let Some(event) = input.hook_event() {
        icm::lifecycle(event.name());

        if let Some(hook) = HOOK.get() {
            hook(event);
        }
    }

    let transitions = TRACKER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .feed(Platform::current(), input);

    for transition in transitions {
        icm::app_state(transition.name());
        EVENTS.publish(transition);

        #[cfg(target_os = "android")]
        WAKEUPS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .schedule(Instant::now());
    }
}

/// What the shell tells the tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Input {
    /// winit's `Resumed`.
    Resumed,
    /// winit's `Suspended`.
    Suspended,
    /// `WindowEvent::Occluded`.
    Occluded(bool),
    /// `WindowEvent::Focused`.
    Focused(bool),
    /// winit's `MemoryWarning`.
    MemoryWarning,
}

impl Input {
    /// The input a window event gives, if any.
    pub(crate) fn of(event: &winit::event::WindowEvent) -> Option<Self> {
        match event {
            winit::event::WindowEvent::Occluded(occluded) => {
                Some(Self::Occluded(*occluded))
            }
            winit::event::WindowEvent::Focused(focused) => {
                Some(Self::Focused(*focused))
            }
            _ => None,
        }
    }

    /// What the hook receives for this input: `Suspended` and `Resumed`
    /// only, as before [`lifecycle`] existed.
    fn hook_event(self) -> Option<Lifecycle> {
        match self {
            Self::Resumed => Some(Lifecycle::Resumed),
            Self::Suspended => Some(Lifecycle::Suspended),
            Self::Occluded(_) | Self::Focused(_) | Self::MemoryWarning => None,
        }
    }
}

/// Where the application runs, which decides what each input means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    Ios,
    Android,
    Web,
    Desktop,
}

impl Platform {
    /// The platform of this build.
    const fn current() -> Self {
        if cfg!(target_os = "ios") {
            Self::Ios
        } else if cfg!(target_os = "android") {
            Self::Android
        } else if cfg!(target_arch = "wasm32") {
            Self::Web
        } else {
            Self::Desktop
        }
    }
}

/// The application's state, as the tracker derives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Background,
    Inactive,
    Active,
}

/// The visibility and focus, from which the transitions are derived.
#[derive(Debug)]
struct Tracker {
    visible: bool,
    focused: bool,
}

impl Tracker {
    /// In the background, as an application starts.
    const fn new() -> Self {
        Self {
            visible: false,
            focused: false,
        }
    }

    fn phase(&self) -> Phase {
        match (self.visible, self.focused) {
            (false, _) => Phase::Background,
            (true, false) => Phase::Inactive,
            (true, true) => Phase::Active,
        }
    }

    /// The transitions `input` causes on `platform`, at most two, in order.
    ///
    /// | Input | iOS | Android | Web | Desktop |
    /// |---|---|---|---|---|
    /// | `Resumed` | visible, focused | visible | visible, focused | visible, focused |
    /// | `Suspended` | not focused | not visible | not visible, not focused | ignored |
    /// | `Occluded(o)` | visible = `!o` | ignored | ignored | ignored |
    /// | `Focused(f)` | ignored | focused = `f` | ignored | ignored |
    fn feed(
        &mut self,
        platform: Platform,
        input: Input,
    ) -> impl Iterator<Item = Lifecycle> + use<> {
        use Platform::{Android, Desktop, Ios, Web};

        let before = self.phase();

        match (platform, input) {
            (_, Input::MemoryWarning) => {
                return [Some(Lifecycle::MemoryWarning), None]
                    .into_iter()
                    .flatten();
            }
            (Ios | Web | Desktop, Input::Resumed) => {
                self.visible = true;
                self.focused = true;
            }
            (Android, Input::Resumed) => self.visible = true,
            (Ios, Input::Suspended) => self.focused = false,
            (Android, Input::Suspended) => self.visible = false,
            (Web, Input::Suspended) => {
                self.visible = false;
                self.focused = false;
            }
            (Ios, Input::Occluded(occluded)) => self.visible = !occluded,
            (Android, Input::Focused(focused)) => self.focused = focused,
            (Desktop, Input::Suspended)
            | (Android | Web | Desktop, Input::Occluded(_))
            | (Ios | Web | Desktop, Input::Focused(_)) => {}
        }

        transitions(before, self.phase()).into_iter().flatten()
    }
}

/// What the application sees when its state goes from `before` to `after`.
fn transitions(before: Phase, after: Phase) -> [Option<Lifecycle>; 2] {
    use Lifecycle::{Active, Background, Foreground, Inactive};

    match (before, after) {
        (Phase::Background, Phase::Inactive) => [Some(Foreground), None],
        (Phase::Background, Phase::Active) => [Some(Foreground), Some(Active)],
        (Phase::Inactive, Phase::Active) => [Some(Active), None],
        (Phase::Active, Phase::Inactive) => [Some(Inactive), None],
        (Phase::Active, Phase::Background) => {
            [Some(Inactive), Some(Background)]
        }
        (Phase::Inactive, Phase::Background) => [Some(Background), None],
        (Phase::Background, Phase::Background)
        | (Phase::Inactive, Phase::Inactive)
        | (Phase::Active, Phase::Active) => [None, None],
    }
}

/// Android: the wake-ups that hand transitions to the application.
#[cfg(target_os = "android")]
static WAKEUPS: Mutex<Wakeups> = Mutex::new(Wakeups::new());

/// Android: makes the event loop wake up a few times after a transition, so
/// that its message reaches `update` while the Activity is paused. The shell
/// calls it last, when the event loop is about to wait.
#[cfg(target_os = "android")]
pub(crate) fn wake_up(event_loop: &winit::event_loop::ActiveEventLoop) {
    let now = Instant::now();

    let next = WAKEUPS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .next(now);

    if let Some(wakeup) = next
        && let Some(flow) = wake_up_by(event_loop.control_flow(), wakeup)
    {
        event_loop.set_control_flow(flow);
    }
}

/// Android: when the event loop wakes up to hand transitions to the
/// application.
///
/// A subscription's message reaches the event loop through winit's proxy,
/// whose wake-up winit ignores once Android has paused the Activity. The
/// message would wait for the next event, often the return to the app, and
/// arrive in one burst with `Foreground` and `Active`. A timeout still runs
/// an iteration of the loop, which hands over the messages waiting.
#[cfg(any(target_os = "android", test))]
#[derive(Debug)]
struct Wakeups {
    /// In order.
    at: Vec<Instant>,
}

#[cfg(any(target_os = "android", test))]
impl Wakeups {
    /// How long after a transition the event loop wakes up: the message
    /// crosses the executor's threads first.
    const AFTER: [Duration; 4] = [
        Duration::from_millis(10),
        Duration::from_millis(50),
        Duration::from_millis(250),
        Duration::from_millis(1000),
    ];

    const fn new() -> Self {
        Self { at: Vec::new() }
    }

    /// Wakes up after a transition published at `now`.
    fn schedule(&mut self, now: Instant) {
        self.at = Self::AFTER.iter().map(|after| now + *after).collect();
    }

    /// The next wake-up after `now`; those passed are forgotten.
    fn next(&mut self, now: Instant) -> Option<Instant> {
        self.at.retain(|at| *at > now);
        self.at.first().copied()
    }
}

/// The control flow that also wakes up at `wakeup`, or `None` when
/// `current` already wakes up by then.
///
/// A deadline that has passed already is kept: winit turns the loop again at
/// once for it, and it is a redraw or a timer that is due.
#[cfg(any(target_os = "android", test))]
fn wake_up_by(
    current: winit::event_loop::ControlFlow,
    wakeup: Instant,
) -> Option<winit::event_loop::ControlFlow> {
    use winit::event_loop::ControlFlow;

    match current {
        ControlFlow::Poll => None,
        ControlFlow::WaitUntil(at) if at <= wakeup => None,
        ControlFlow::Wait | ControlFlow::WaitUntil(_) => {
            Some(ControlFlow::WaitUntil(wakeup))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use Lifecycle::{Active, Background, Foreground, Inactive, MemoryWarning};

    const PLATFORMS: [Platform; 4] = [
        Platform::Ios,
        Platform::Android,
        Platform::Web,
        Platform::Desktop,
    ];

    /// Every transition a new tracker gives for `inputs`.
    fn feed(platform: Platform, inputs: &[Input]) -> Vec<Lifecycle> {
        let mut tracker = Tracker::new();

        inputs
            .iter()
            .flat_map(|input| tracker.feed(platform, *input))
            .collect()
    }

    #[test]
    fn ios_follows_the_application_notifications() {
        use Input::{Focused, Occluded, Resumed, Suspended};

        let ios = |inputs: &[Input]| feed(Platform::Ios, inputs);

        // Launch: `willEnterForeground` finds the window, or comes before it.
        assert_eq!(ios(&[Occluded(false), Resumed]), [Foreground, Active]);
        assert_eq!(ios(&[Resumed]), [Foreground, Active]);

        // Control Center, Notification Center, a Face ID prompt: inactive,
        // never in the background.
        assert_eq!(
            ios(&[Resumed, Suspended, Resumed]),
            [Foreground, Active, Inactive, Active]
        );

        // Home, then back; two windows each see `Occluded`.
        assert_eq!(
            ios(&[
                Resumed,
                Suspended,
                Occluded(true),
                Occluded(true),
                Occluded(false),
                Occluded(false),
                Resumed,
            ]),
            [Foreground, Active, Inactive, Background, Foreground, Active]
        );

        // The focus of a window is not iOS's lifecycle.
        assert_eq!(ios(&[Resumed, Focused(false)]), [Foreground, Active]);
    }

    #[test]
    fn android_follows_the_native_window_and_its_focus() {
        use Input::{Focused, Occluded, Resumed, Suspended};

        let android = |inputs: &[Input]| feed(Platform::Android, inputs);

        // Launch, with the focus after or before the window.
        assert_eq!(android(&[Resumed, Focused(true)]), [Foreground, Active]);
        assert_eq!(android(&[Focused(true), Resumed]), [Foreground, Active]);

        // The notification shade, a permission prompt, split screen.
        assert_eq!(
            android(&[Resumed, Focused(true), Focused(false), Focused(true)]),
            [Foreground, Active, Inactive, Active]
        );

        // Home, with the focus lost before or after the window.
        assert_eq!(
            android(&[Resumed, Focused(true), Focused(false), Suspended]),
            [Foreground, Active, Inactive, Background]
        );
        assert_eq!(
            android(&[Resumed, Focused(true), Suspended, Focused(false)]),
            [Foreground, Active, Inactive, Background]
        );

        // The return.
        assert_eq!(
            android(&[
                Resumed,
                Focused(true),
                Focused(false),
                Suspended,
                Resumed,
                Focused(true),
            ]),
            [Foreground, Active, Inactive, Background, Foreground, Active]
        );

        // `Occluded` is not Android's lifecycle.
        assert_eq!(
            android(&[Resumed, Focused(true), Occluded(true)]),
            [Foreground, Active]
        );
    }

    #[test]
    fn the_web_follows_the_back_forward_cache() {
        use Input::{Focused, Occluded, Resumed, Suspended};

        assert_eq!(
            feed(
                Platform::Web,
                &[Resumed, Focused(false), Occluded(true), Suspended, Resumed]
            ),
            [Foreground, Active, Inactive, Background, Foreground, Active]
        );
    }

    #[test]
    fn the_desktop_is_active_once() {
        use Input::{Focused, Occluded, Resumed, Suspended};

        assert_eq!(
            feed(
                Platform::Desktop,
                &[Resumed, Focused(false), Occluded(true), Suspended]
            ),
            [Foreground, Active]
        );
    }

    #[test]
    fn memory_warnings_pass_through_without_a_change() {
        for platform in PLATFORMS {
            assert_eq!(
                feed(platform, &[Input::MemoryWarning]),
                [MemoryWarning],
                "{platform:?}"
            );

            assert_eq!(
                feed(
                    platform,
                    &[
                        Input::Resumed,
                        Input::Focused(true),
                        Input::MemoryWarning,
                        Input::MemoryWarning
                    ]
                ),
                [Foreground, Active, MemoryWarning, MemoryWarning],
                "{platform:?}"
            );
        }
    }

    /// Every sequence of up to five inputs, on every platform, keeps the
    /// order of the table on [`Lifecycle`], with no repeats.
    #[test]
    fn transitions_keep_their_order_whatever_the_inputs() {
        const INPUTS: [Input; 6] = [
            Input::Resumed,
            Input::Suspended,
            Input::Occluded(true),
            Input::Occluded(false),
            Input::Focused(true),
            Input::Focused(false),
        ];

        fn check(platform: Platform, inputs: &mut Vec<Input>, depth: usize) {
            let mut state = Background;

            for transition in feed(platform, inputs) {
                let allowed = match transition {
                    Foreground => state == Background,
                    Active => state == Foreground || state == Inactive,
                    Inactive => state == Active,
                    Background => state == Inactive || state == Foreground,
                    _ => false,
                };

                assert!(
                    allowed,
                    "{platform:?}: {transition:?} after {state:?} for \
                    {inputs:?}"
                );

                state = transition;
            }

            if depth == 0 {
                return;
            }

            for input in INPUTS {
                inputs.push(input);
                check(platform, inputs, depth - 1);
                let _ = inputs.pop();
            }
        }

        for platform in PLATFORMS {
            check(platform, &mut Vec::new(), 5);
        }
    }

    #[test]
    fn the_hook_receives_suspended_and_resumed_only() {
        assert_eq!(Input::Resumed.hook_event(), Some(Lifecycle::Resumed));
        assert_eq!(Input::Suspended.hook_event(), Some(Lifecycle::Suspended));

        for input in [
            Input::Occluded(true),
            Input::Occluded(false),
            Input::Focused(true),
            Input::Focused(false),
            Input::MemoryWarning,
        ] {
            assert_eq!(input.hook_event(), None, "{input:?}");
        }
    }

    #[test]
    fn window_events_give_occluded_and_focused() {
        use winit::event::WindowEvent;

        assert_eq!(
            Input::of(&WindowEvent::Occluded(true)),
            Some(Input::Occluded(true))
        );
        assert_eq!(
            Input::of(&WindowEvent::Focused(false)),
            Some(Input::Focused(false))
        );
        assert_eq!(Input::of(&WindowEvent::CloseRequested), None);
    }

    /// The only test that touches the process-wide state.
    #[test]
    fn reports_reach_subscribers_and_a_new_application_starts_over() {
        let received = |subscriber: &mut _| {
            std::iter::from_fn(|| crate::try_next(subscriber).ok().flatten())
                .collect::<Vec<_>>()
        };

        start();
        let mut subscriber = EVENTS.subscribe();

        report(Input::Resumed);
        report(Input::Focused(false));
        report(Input::MemoryWarning);

        // The desktop's table, where the tests run.
        assert_eq!(
            received(&mut subscriber),
            [Foreground, Active, MemoryWarning]
        );

        // The next application starts in the background.
        start();
        report(Input::Resumed);

        assert_eq!(received(&mut subscriber), [Foreground, Active]);
    }

    #[test]
    fn wakeups_follow_a_transition_then_stop() {
        let now = Instant::now();
        let after = Duration::from_millis;
        let mut wakeups = Wakeups::new();

        assert_eq!(wakeups.next(now), None);

        wakeups.schedule(now);
        assert_eq!(wakeups.next(now), Some(now + after(10)));
        assert_eq!(wakeups.next(now + after(10)), Some(now + after(50)));
        assert_eq!(wakeups.next(now + after(300)), Some(now + after(1000)));
        assert_eq!(wakeups.next(now + after(1000)), None);

        // A later transition starts over.
        let later = now + after(5000);
        wakeups.schedule(later);
        assert_eq!(wakeups.next(later), Some(later + after(10)));
    }

    #[test]
    fn a_wakeup_keeps_an_earlier_deadline() {
        use winit::event_loop::ControlFlow;

        let now = Instant::now();
        let after = Duration::from_millis;
        let wakeup = now + after(50);

        assert_eq!(
            wake_up_by(ControlFlow::Wait, wakeup),
            Some(ControlFlow::WaitUntil(wakeup))
        );
        assert_eq!(wake_up_by(ControlFlow::Poll, wakeup), None);
        assert_eq!(
            wake_up_by(ControlFlow::WaitUntil(now + after(16)), wakeup),
            None
        );
        assert_eq!(
            wake_up_by(ControlFlow::WaitUntil(now + after(500)), wakeup),
            Some(ControlFlow::WaitUntil(wakeup))
        );

        // A deadline that has passed is due now: a redraw or a timer that
        // must not wait for the wake-up.
        assert_eq!(wake_up_by(ControlFlow::WaitUntil(now), wakeup), None);
        assert_eq!(
            wake_up_by(
                ControlFlow::WaitUntil(
                    now.checked_sub(after(5)).unwrap_or(now)
                ),
                wakeup
            ),
            None
        );
    }
}
