//! The macOS window listing and screen-capture preflight
//! (`platform::desktop::macos`) against the real CoreGraphics.
//!
//! These calls live in a test binary of their own: running them while
//! other tests in the same process spawned children got those children
//! killed (SIGKILL) now and then, which made the desktop unit tests that
//! check a child's exit status flaky. icm itself makes these calls from
//! one thread, after the app is launched.

#![cfg(target_os = "macos")]

use icm::platform::desktop::macos::{
    main_display_scale, main_window, screen_capture_allowed, windows,
};

#[test]
fn listing_windows_works_without_permission() {
    // The test process owns no window.
    let pid = std::process::id() as i32;
    let all = windows();
    assert!(all.iter().all(|window| window.pid != pid));
    assert!(main_window(pid).is_none());
    // Only a query: it must not prompt or crash.
    let _ = screen_capture_allowed();
    if let Some(scale) = main_display_scale() {
        assert!((1.0..=4.0).contains(&scale), "{scale}");
    }
}
