//! macOS window lookup and the screen-capture preflight, in-process
//! (design §10.1 steps 3 and 4).
//!
//! A few CoreGraphics and CoreFoundation calls through plain C FFI, so the
//! CLI needs no Objective-C binding crates:
//!
//! - `CGWindowListCopyWindowInfo` lists every window with its owner's pid,
//!   layer, bounds and number. The number is the id `screencapture -l`
//!   takes. Listing windows needs no permission.
//! - `CGPreflightScreenCaptureAccess` says whether this process (that is,
//!   the terminal or app that started icm, which macOS holds responsible
//!   for it) may record the screen. It never prompts. Without that
//!   permission `screencapture` captures no window content, so icm warns
//!   `desktop.shot.permission` and renders the view headlessly instead.

use std::ffi::c_void;

type CFTypeRef = *const c_void;
type CFIndex = isize;
type CFTypeID = usize;
type Boolean = u8;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

const K_CF_NUMBER_SINT64_TYPE: CFIndex = 4;
const K_CG_WINDOW_LIST_OPTION_ALL: u32 = 0;
const K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS: u32 = 1 << 4;
const K_CG_NULL_WINDOW_ID: u32 = 0;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFArrayGetCount(array: CFTypeRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(array: CFTypeRef, index: CFIndex) -> CFTypeRef;
    fn CFDictionaryGetValue(dictionary: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn CFNumberGetValue(number: CFTypeRef, kind: CFIndex, value: *mut c_void) -> Boolean;
    fn CFBooleanGetValue(boolean: CFTypeRef) -> Boolean;
    fn CFGetTypeID(object: CFTypeRef) -> CFTypeID;
    fn CFNumberGetTypeID() -> CFTypeID;
    fn CFBooleanGetTypeID() -> CFTypeID;
    fn CFDictionaryGetTypeID() -> CFTypeID;
    fn CFRelease(object: CFTypeRef);
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    static kCGWindowNumber: CFTypeRef;
    static kCGWindowOwnerPID: CFTypeRef;
    static kCGWindowLayer: CFTypeRef;
    static kCGWindowBounds: CFTypeRef;
    static kCGWindowIsOnscreen: CFTypeRef;
    static kCGWindowAlpha: CFTypeRef;

    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFTypeRef;
    fn CGRectMakeWithDictionaryRepresentation(dictionary: CFTypeRef, rect: *mut CGRect) -> bool;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayCopyDisplayMode(display: u32) -> CFTypeRef;
    fn CGDisplayModeGetWidth(mode: CFTypeRef) -> usize;
    fn CGDisplayModeGetPixelWidth(mode: CFTypeRef) -> usize;
    fn CGDisplayModeRelease(mode: CFTypeRef);
}

/// A window, as the window server lists it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    /// The window number (`screencapture -l <id>`).
    pub id: u32,
    /// The owning process.
    pub pid: i32,
    /// The window layer; 0 is the normal application layer.
    pub layer: i64,
    /// The frame in points (title bar included): x, y, width, height.
    pub bounds: (f64, f64, f64, f64),
    /// Whether it is on screen.
    pub onscreen: bool,
    /// Its opacity.
    pub alpha: f64,
}

impl Window {
    /// Its area in square points.
    pub fn area(&self) -> f64 {
        self.bounds.2 * self.bounds.3
    }
}

/// Reads a CFNumber as an i64.
///
/// # Safety
///
/// `value` is null or a valid CF object.
unsafe fn number(value: CFTypeRef) -> Option<i64> {
    // SAFETY: the caller passes null or a CF object; the type is checked
    // before CFNumberGetValue reads it into a properly sized i64.
    unsafe {
        if value.is_null() || CFGetTypeID(value) != CFNumberGetTypeID() {
            return None;
        }
        let mut out: i64 = 0;
        (CFNumberGetValue(
            value,
            K_CF_NUMBER_SINT64_TYPE,
            (&raw mut out).cast::<c_void>(),
        ) != 0)
            .then_some(out)
    }
}

/// Every window of every process (the desktop's own elements left out).
pub fn windows() -> Vec<Window> {
    let mut out = Vec::new();

    // SAFETY: CGWindowListCopyWindowInfo returns a CFArray of CFDictionary
    // (or null) that this function owns and releases. Every value read from
    // a dictionary is type-checked before use, and none outlives the array.
    unsafe {
        let list = CGWindowListCopyWindowInfo(
            K_CG_WINDOW_LIST_OPTION_ALL | K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS,
            K_CG_NULL_WINDOW_ID,
        );
        if list.is_null() {
            return out;
        }

        for index in 0..CFArrayGetCount(list) {
            let info = CFArrayGetValueAtIndex(list, index);
            if info.is_null() || CFGetTypeID(info) != CFDictionaryGetTypeID() {
                continue;
            }

            let get = |key: CFTypeRef| CFDictionaryGetValue(info, key);
            let (Some(id), Some(pid)) =
                (number(get(kCGWindowNumber)), number(get(kCGWindowOwnerPID)))
            else {
                continue;
            };

            let mut rect = CGRect::default();
            let bounds = get(kCGWindowBounds);
            let has_bounds = !bounds.is_null()
                && CFGetTypeID(bounds) == CFDictionaryGetTypeID()
                && CGRectMakeWithDictionaryRepresentation(bounds, &raw mut rect);

            let onscreen = get(kCGWindowIsOnscreen);
            let onscreen = !onscreen.is_null()
                && CFGetTypeID(onscreen) == CFBooleanGetTypeID()
                && CFBooleanGetValue(onscreen) != 0;

            let alpha = get(kCGWindowAlpha);
            let alpha = if !alpha.is_null() && CFGetTypeID(alpha) == CFNumberGetTypeID() {
                let mut value: f64 = 1.0;
                // kCFNumberFloat64Type
                let _ = CFNumberGetValue(alpha, 6, (&raw mut value).cast::<c_void>());
                value
            } else {
                1.0
            };

            out.push(Window {
                id: id as u32,
                pid: pid as i32,
                layer: number(get(kCGWindowLayer)).unwrap_or(0),
                bounds: if has_bounds {
                    (
                        rect.origin.x,
                        rect.origin.y,
                        rect.size.width,
                        rect.size.height,
                    )
                } else {
                    (0.0, 0.0, 0.0, 0.0)
                },
                onscreen,
                alpha,
            });
        }

        CFRelease(list);
    }

    out
}

/// The app's main window: a normal-layer window of `pid`, on screen if one
/// is, the largest first.
pub fn main_window(pid: i32) -> Option<Window> {
    pick_main(windows().into_iter().filter(|window| window.pid == pid))
}

/// Picks the main window among one process's windows.
pub fn pick_main(windows: impl Iterator<Item = Window>) -> Option<Window> {
    windows
        .filter(|window| window.layer == 0 && window.area() > 0.0)
        .max_by(|a, b| {
            (a.onscreen, a.alpha > 0.0)
                .cmp(&(b.onscreen, b.alpha > 0.0))
                .then(a.area().total_cmp(&b.area()))
        })
}

/// Whether macOS lets this process (and so `screencapture`) record the
/// screen. Never prompts.
pub fn screen_capture_allowed() -> bool {
    // SAFETY: a plain query without arguments.
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// The main display's backing scale (2 on a Retina display).
pub fn main_display_scale() -> Option<f64> {
    // SAFETY: the display mode is released after its sizes are read.
    unsafe {
        let mode = CGDisplayCopyDisplayMode(CGMainDisplayID());
        if mode.is_null() {
            return None;
        }
        let points = CGDisplayModeGetWidth(mode);
        let pixels = CGDisplayModeGetPixelWidth(mode);
        CGDisplayModeRelease(mode);
        (points > 0 && pixels > 0).then(|| pixels as f64 / points as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The CoreGraphics calls themselves are tested in
    // `tests/macos_windows.rs`, a process of their own: run next to the
    // unit tests that spawn children, they got those children killed
    // (SIGKILL) now and then.

    #[test]
    fn the_main_window_is_the_largest_normal_one_on_screen() {
        let window = |id, layer, size: f64, onscreen| Window {
            id,
            pid: 7,
            layer,
            bounds: (0.0, 0.0, size, size),
            onscreen,
            alpha: 1.0,
        };
        let picked = pick_main(
            [
                window(1, 0, 100.0, false),
                window(2, 0, 50.0, true),
                window(3, 25, 500.0, true),
                window(4, 0, 0.0, true),
            ]
            .into_iter(),
        );
        assert_eq!(picked.map(|w| w.id), Some(2));
        assert!(pick_main(std::iter::empty()).is_none());
    }
}
