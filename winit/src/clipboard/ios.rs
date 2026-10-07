//! iOS: the system clipboard, `UIPasteboard.generalPasteboard`.
#![allow(unsafe_code)]

use objc2_foundation::NSString;
use objc2_ui_kit::UIPasteboard;

/// The text on the pasteboard, if any.
///
/// From iOS 16, reading what another app copied asks the user first ("Allow
/// Paste"), unless they allowed it in Settings, and this waits for the
/// answer: a denied read gives `None`.
pub(super) fn read() -> Option<String> {
    // SAFETY: plain getters; UIKit declares `UIPasteboard` thread-safe (the
    // bindings make it `Send` and `Sync`), and the shell calls this on the
    // main thread anyway.
    let string = unsafe { UIPasteboard::generalPasteboard().string() }?;

    Some(string.to_string())
}

/// Puts `contents` on the pasteboard, as plain text.
pub(super) fn write(contents: &str) {
    let string = NSString::from_str(contents);

    // SAFETY: as in `read`; the pasteboard copies the string.
    unsafe { UIPasteboard::generalPasteboard().setString(Some(&string)) };
}
