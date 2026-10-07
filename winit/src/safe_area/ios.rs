//! iOS: the safe area from winit's frames, and the keyboard's frame from
//! UIKit's notification.
//!
//! winit gives the window's safe-area frame as its inner position and size,
//! and its bounds as its outer ones, both in screen space
//! (`platform_impl/ios/window.rs`), so the insets need no Objective-C here.
//! The keyboard's frame arrives in `UIKeyboardWillChangeFrameNotification`,
//! observed once per process on the main queue, the way `scene.rs` observes
//! the scenes: no Objective-C class is declared.

// UIKit is reached only through Objective-C calls; each says why it is sound.
#![allow(unsafe_code)]

use super::{Frame, Physical};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2_foundation::{
    CGRect, MainThreadMarker, NSNotification, NSNotificationCenter, NSObject,
    NSOperationQueue, NSValue,
};
use objc2_ui_kit::{
    NSValueUIGeometryExtensions, UIKeyboardFrameEndUserInfoKey,
    UIKeyboardWillChangeFrameNotification,
};

use std::cell::Cell;
use std::ptr::NonNull;
use std::sync::Once;

thread_local! {
    /// Where the keyboard last said it would end up, in points, in screen
    /// space. Below the screen when it hides.
    static KEYBOARD: Cell<Option<CGRect>> = const { Cell::new(None) };
}

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

    let keyboard = KEYBOARD.get().map_or(0.0, |frame| {
        // Points to winit's physical pixels, as the frames above.
        let scale = window.scale_factor();

        super::keyboard_overlap(
            bounds,
            Frame {
                x: frame.origin.x * scale,
                y: frame.origin.y * scale,
                width: frame.size.width * scale,
                height: frame.size.height * scale,
            },
        )
    });

    Some(Physical {
        insets: super::frame_insets(bounds, safe),
        keyboard,
    })
}

/// Observes the keyboard's frame for the life of the process, once.
pub(super) fn observe_keyboard() {
    static OBSERVE: Once = Once::new();

    if MainThreadMarker::new().is_none() {
        log::warn!(
            "Safe area: not on the main thread; the keyboard is not observed"
        );
        return;
    }

    OBSERVE.call_once(|| {
        // SAFETY: the center passes a valid notification for the call.
        let block = RcBlock::new(|note: NonNull<NSNotification>| {
            KEYBOARD.set(end_frame(unsafe { note.as_ref() }));
        });

        // SAFETY: the main queue and the default center are always valid,
        // the name is UIKit's own constant, and the block runs on the main
        // queue, where `KEYBOARD` is read.
        let observer = unsafe {
            NSNotificationCenter::defaultCenter()
                .addObserverForName_object_queue_usingBlock(
                    Some(UIKeyboardWillChangeFrameNotification),
                    None,
                    Some(&NSOperationQueue::mainQueue()),
                    &block,
                )
        };

        std::mem::forget(observer);
    });
}

/// The keyboard's frame at the end of its animation, from the notice.
fn end_frame(note: &NSNotification) -> Option<CGRect> {
    // SAFETY: the user info of a keyboard notification is a dictionary, and
    // the key is UIKit's own constant.
    let value = unsafe {
        note.userInfo()?
            .objectForKey(UIKeyboardFrameEndUserInfoKey)?
    };

    // SAFETY: every object in the dictionary is an `NSObject`.
    let value = unsafe { Retained::cast::<NSObject>(value) };

    if !value.is_kind_of::<NSValue>() {
        return None;
    }

    // SAFETY: an `NSValue`, checked above, which UIKit documents to hold a
    // `CGRect` under this key.
    Some(unsafe { Retained::cast::<NSValue>(value).CGRectValue() })
}
