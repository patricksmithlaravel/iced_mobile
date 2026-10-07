//! iOS: the safe area from winit's frames.
//!
//! winit gives the window's safe-area frame as its inner position and size,
//! and its bounds as its outer ones, both in screen space
//! (`platform_impl/ios/window.rs`), so the insets need no Objective-C here.

use super::{Frame, Physical};

/// The safe area of `window` in physical pixels.
pub(super) fn read(window: &winit::window::Window) -> Option<Physical> {
    let outer = window.outer_position().ok()?;
    let outer_size = window.outer_size();
    let inner = window.inner_position().ok()?;
    let inner_size = window.inner_size();

    let bounds = Frame {
        x: f64::from(outer.x),
        y: f64::from(outer.y),
        width: f64::from(outer_size.width),
        height: f64::from(outer_size.height),
    };

    let safe = Frame {
        x: f64::from(inner.x),
        y: f64::from(inner.y),
        width: f64::from(inner_size.width),
        height: f64::from(inner_size.height),
    };

    Some(Physical {
        insets: super::frame_insets(bounds, safe),
        keyboard: 0.0,
    })
}
