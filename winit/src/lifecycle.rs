//! The application's life as winit reports it, and the hook a shell runs on
//! it.
use crate::icm;

/// What winit says of the application's life, for a shell that must act on
/// it at once: lock a wallet, hide what is on screen.
///
/// | Platform | `Suspended` | `Resumed` |
/// |---|---|---|
/// | Android | the native window is going away, as the application leaves the screen | the native window exists, at launch and on return |
/// | iOS | the application is about to stop being active, which also happens for Control Center, notifications and Face ID | it became active, at launch and on return |
/// | Web | the page is hidden into the back-forward cache (`pagehide`, persisted) | at launch, and when the page comes back from that cache (`pageshow`) |
/// | Desktop | never | once, at launch |
///
/// More variants may be added, so a `match` on it needs a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Lifecycle {
    /// winit's `Suspended`: on Android the native window is going away, as
    /// the application leaves the screen; on iOS it is about to stop being
    /// active.
    Suspended,
    /// winit's `Resumed`.
    Resumed,
}

static LIFECYCLE: std::sync::OnceLock<fn(Lifecycle)> =
    std::sync::OnceLock::new();

/// Calls `hook` on the event loop's thread whenever winit reports the
/// application suspended or resumed, before iced acts on it.
///
/// Set it before [`run`](crate::run). There is one hook for the process: only
/// the first call sets it. A later call with the same function does nothing,
/// so an application can set it each time it starts (on Android, each time
/// `android_main` runs for a new Activity). A later call with another
/// function is ignored with a warning in the log.
pub fn on_lifecycle(hook: fn(Lifecycle)) {
    if let Err(hook) = LIFECYCLE.set(hook)
        && LIFECYCLE
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

/// Reports what winit said: emits the `lifecycle` event and runs the hook.
pub(crate) fn report(event: Lifecycle) {
    icm::lifecycle(match event {
        Lifecycle::Suspended => "suspended",
        Lifecycle::Resumed => "resumed",
    });

    if let Some(hook) = LIFECYCLE.get() {
        hook(event);
    }
}
