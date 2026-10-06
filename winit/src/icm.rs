//! The `ICM_EVENT` protocol, version 1: one line per event that tells the
//! `icm` tool, or any other launcher, that the application started, drew its
//! first frame, changed lifecycle state, panicked or stopped.
//!
//! Nothing is written unless the run opted in, so applications behave the
//! same in debug and release builds:
//!
//! | Platform | Opt-in |
//! |---|---|
//! | Desktop | the environment variable `ICM_EVENTS=1` |
//! | iOS | the same variable, passed through `simctl launch` as `SIMCTL_CHILD_ICM_EVENTS=1` (`DEVICECTL_CHILD_ICM_EVENTS=1` on a device) |
//! | Android | the system property `debug.icm.events` set to `1` (`adb shell setprop debug.icm.events 1`) |
//! | Web | `icm_events=1` in the page's query string (`?icm_events=1`) |
//!
//! `true` is accepted in place of `1`. The choice is read once, when the
//! first event is emitted.
//!
//! Each event is one line:
//!
//! | Platform | Line |
//! |---|---|
//! | Desktop, iOS | stderr: `ICM_EVENT <json>` |
//! | Android | logcat, priority INFO, tag `ICM_EVENT`, message `<json>` (`adb logcat -s ICM_EVENT:I`) |
//! | Web | `console.log("ICM_EVENT <json>")` |
//!
//! The JSON object is on a single line and starts with `"v":1,"kind":"<kind>"`:
//!
//! | `kind` | Fields | When |
//! |---|---|---|
//! | `start` | `protocol` (1), `framework` (the iced version), `pid` (`null` on the web), `platform` (`macos`, `linux`, `windows`, `ios`, `android`, `web`, ...), `bridge` (the agent bridge protocol, `null` when it is not compiled in) | the shell starts |
//! | `ready` | `ms` (since `start`), `window{size, physical, scale}` (logical and physical size, scale factor), `backend` (`wgpu`, `tiny-skia`), `adapter`, `api` (`Metal`, `Vulkan`, `Gl`, ...) | the first frame was presented |
//! | `lifecycle` | `state` (`suspended`, `resumed`) | winit reports the application suspended or resumed; see [`Lifecycle`](crate::Lifecycle) |
//! | `panic` | `message`, `location` (`file:line:column` or `null`), `thread` | a thread panics, once [`install_panic_hook`] ran |
//! | `warning` | `code`, `message` | something degraded; see [`warning`] |
//! | `exit` | `code` (0, or 1 when the shell stopped with an error) | the shell stopped; iOS and the web never send it |
//!
//! For example:
//!
//! ```text
//! ICM_EVENT {"v":1,"kind":"ready","ms":812,"window":{"size":[402,874],"physical":[1206,2622],"scale":3},"backend":"wgpu","adapter":"Apple M4","api":"Metal"}
//! ```
//!
//! Fields may be added to a kind, and kinds may be added, within version 1.
//! A reader must ignore what it does not know.
use crate::core::Size;
use crate::core::time::Instant;
use crate::graphics::compositor;

use std::fmt::Write as _;
use std::sync::OnceLock;
use std::sync::atomic::{self, AtomicBool};

/// The version of the protocol, in every line as `"v"` and in `start` as
/// `"protocol"`.
pub const PROTOCOL: u32 = 1;

/// Returns whether this run opted in to `ICM_EVENT` lines.
///
/// See the [module documentation](self) for how a run opts in.
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();

    *ENABLED.get_or_init(opted_in)
}

/// Emits a `warning` event with a stable `code` (for example
/// `font.default_missing`) and a `message` for people, if events are on.
pub fn warning(code: &str, message: &str) {
    if !enabled() {
        return;
    }

    emit(
        Event::new("warning")
            .str("code", code)
            .str("message", message),
    );
}

/// Installs a panic hook, once per process, that emits a `panic` event and
/// then runs the hook that was in place.
///
/// Does nothing unless events are on. The shell calls it when it starts,
/// and so does `iced::mobile::init_logger`, which catches panics before the
/// shell starts too.
pub fn install_panic_hook() {
    use std::sync::Once;

    static HOOK: Once = Once::new();

    if !enabled() {
        return;
    }

    HOOK.call_once(|| {
        let previous = std::panic::take_hook();

        std::panic::set_hook(Box::new(move |info| {
            let thread = std::thread::current();

            let message =
                info.payload_as_str().unwrap_or("Box<dyn Any>").to_owned();

            let location = info.location().map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            });

            emit(
                Event::new("panic")
                    .str("message", &message)
                    .opt_str("location", location.as_deref())
                    .str("thread", thread.name().unwrap_or("<unnamed>")),
            );

            previous(info);
        }));
    });
}

/// When the shell started, for `ready`'s `ms`.
static STARTED: OnceLock<Instant> = OnceLock::new();

/// Emits `start`. Called once, as the shell starts.
pub(crate) fn start() {
    if !enabled() {
        return;
    }

    let _ = STARTED.get_or_init(Instant::now);

    install_panic_hook();

    #[cfg(not(target_arch = "wasm32"))]
    let pid = Some(std::process::id());

    // `std::process::id` panics on wasm32-unknown-unknown.
    #[cfg(target_arch = "wasm32")]
    let pid: Option<u32> = None;

    emit(
        Event::new("start")
            .number("protocol", PROTOCOL)
            .str("framework", env!("CARGO_PKG_VERSION"))
            .opt_number("pid", pid)
            .str("platform", platform())
            .null("bridge"),
    );
}

/// Whether `ready` was emitted.
static READY: AtomicBool = AtomicBool::new(false);

/// Whether events are on and `ready` is still to come, so that the shell
/// gathers what it reports only once.
pub(crate) fn awaits_ready() -> bool {
    enabled() && !READY.load(atomic::Ordering::Relaxed)
}

/// Emits `ready`, the first time it is called in the process.
pub(crate) fn ready(
    logical: Size<f32>,
    physical: Size<u32>,
    scale_factor: f32,
    backend: &str,
    information: &compositor::Information,
) {
    if !enabled() || READY.swap(true, atomic::Ordering::Relaxed) {
        return;
    }

    let ms = STARTED
        .get()
        .map(|started| started.elapsed().as_millis())
        .unwrap_or_default();

    let mut window = String::from("{\"size\":[");
    push_float(&mut window, logical.width);
    window.push(',');
    push_float(&mut window, logical.height);
    let _ = write!(
        window,
        "],\"physical\":[{},{}],\"scale\":",
        physical.width, physical.height
    );
    push_float(&mut window, scale_factor);
    window.push('}');

    emit(
        Event::new("ready")
            .number("ms", ms)
            .raw("window", &window)
            .str("backend", backend)
            .str("adapter", &information.adapter)
            .str("api", &information.backend),
    );
}

/// Emits `lifecycle`.
pub(crate) fn lifecycle(state: &str) {
    if !enabled() {
        return;
    }

    emit(Event::new("lifecycle").str("state", state));
}

/// Emits `exit`.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) fn exit(code: i32) {
    if !enabled() {
        return;
    }

    emit(Event::new("exit").number("code", code));
}

/// The name `start` gives the platform.
fn platform() -> &'static str {
    if cfg!(target_arch = "wasm32") {
        "web"
    } else {
        std::env::consts::OS
    }
}

/// Whether the run asked for events; see the module documentation.
fn opted_in() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|window| window.location().search().ok())
            .is_some_and(|search| query_opts_in(&search))
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let from_env =
            std::env::var("ICM_EVENTS").is_ok_and(|value| is_on(&value));

        #[cfg(target_os = "android")]
        let from_env = from_env
            || system_property(c"debug.icm.events")
                .is_some_and(|value| is_on(&value));

        from_env
    }
}

/// `1` and `true` turn events on.
fn is_on(value: &str) -> bool {
    let value = value.trim();

    value == "1" || value.eq_ignore_ascii_case("true")
}

/// Whether a query string (`?a=b&icm_events=1`) turns events on.
#[cfg_attr(not(any(target_arch = "wasm32", test)), allow(dead_code))]
fn query_opts_in(search: &str) -> bool {
    search
        .trim_start_matches('?')
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .any(|(key, value)| key == "icm_events" && is_on(value))
}

/// Writes one event line where the platform's launcher can read it.
fn emit(event: Event) {
    let json = event.finish();

    #[cfg(target_os = "android")]
    android_log(&json);

    #[cfg(target_arch = "wasm32")]
    web_sys::console::log_1(&format!("ICM_EVENT {json}").into());

    #[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
    {
        use std::io::Write;

        // One write for the whole line, so that lines from different
        // threads do not interleave; never `eprintln!`, which panics if
        // stderr is closed, and this runs inside the panic hook too.
        let line = format!("ICM_EVENT {json}\n");
        let _ = std::io::stderr().lock().write_all(line.as_bytes());
    }
}

/// Writes `text` to logcat with priority INFO and the tag `ICM_EVENT`.
#[cfg(target_os = "android")]
#[allow(unsafe_code)]
fn android_log(text: &str) {
    use std::ffi::{CString, c_char, c_int};

    // <android/log.h>; liblog is linked by every Android application.
    #[link(name = "log")]
    unsafe extern "C" {
        fn __android_log_write(
            priority: c_int,
            tag: *const c_char,
            text: *const c_char,
        ) -> c_int;
    }

    const ANDROID_LOG_INFO: c_int = 4;

    // The JSON escapes every control character, NUL included.
    let Ok(text) = CString::new(text) else {
        return;
    };

    // SAFETY: both strings are NUL-terminated and outlive the call.
    let _ = unsafe {
        __android_log_write(
            ANDROID_LOG_INFO,
            c"ICM_EVENT".as_ptr(),
            text.as_ptr(),
        )
    };
}

/// The value of an Android system property, if it is set and not empty.
#[cfg(target_os = "android")]
#[doc(hidden)]
#[allow(unsafe_code)]
pub fn system_property(name: &std::ffi::CStr) -> Option<String> {
    use std::ffi::{CStr, c_char, c_int};

    // bionic's <sys/system_properties.h>.
    unsafe extern "C" {
        fn __system_property_get(
            name: *const c_char,
            value: *mut c_char,
        ) -> c_int;
    }

    // PROP_VALUE_MAX: a value and its terminating NUL fit in 92 bytes.
    let mut value = [0 as c_char; 92];

    // SAFETY: `name` is NUL-terminated, and `value` has the PROP_VALUE_MAX
    // bytes bionic writes at most, NUL included.
    let length =
        unsafe { __system_property_get(name.as_ptr(), value.as_mut_ptr()) };

    if length <= 0 {
        return None;
    }

    // SAFETY: bionic NUL-terminated what it wrote.
    let value = unsafe { CStr::from_ptr(value.as_ptr()) };

    Some(value.to_string_lossy().into_owned())
}

/// A JSON object under construction, starting with `"v"` and `"kind"`.
struct Event(String);

impl Event {
    fn new(kind: &str) -> Self {
        let mut json = format!("{{\"v\":{PROTOCOL},\"kind\":");
        push_string(&mut json, kind);

        Self(json)
    }

    fn key(&mut self, key: &str) {
        self.0.push(',');
        push_string(&mut self.0, key);
        self.0.push(':');
    }

    fn str(mut self, key: &str, value: &str) -> Self {
        self.key(key);
        push_string(&mut self.0, value);
        self
    }

    fn opt_str(self, key: &str, value: Option<&str>) -> Self {
        match value {
            Some(value) => self.str(key, value),
            None => self.null(key),
        }
    }

    fn number(mut self, key: &str, value: impl std::fmt::Display) -> Self {
        self.key(key);
        let _ = write!(self.0, "{value}");
        self
    }

    fn opt_number(
        self,
        key: &str,
        value: Option<impl std::fmt::Display>,
    ) -> Self {
        match value {
            Some(value) => self.number(key, value),
            None => self.null(key),
        }
    }

    fn null(self, key: &str) -> Self {
        self.raw(key, "null")
    }

    /// `json` must be a complete JSON value.
    fn raw(mut self, key: &str, json: &str) -> Self {
        self.key(key);
        self.0.push_str(json);
        self
    }

    fn finish(mut self) -> String {
        self.0.push('}');
        self.0
    }
}

/// Appends `value` as a JSON string.
fn push_string(json: &mut String, value: &str) {
    json.push('"');

    for c in value.chars() {
        match c {
            '"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            '\n' => json.push_str("\\n"),
            '\r' => json.push_str("\\r"),
            '\t' => json.push_str("\\t"),
            c if u32::from(c) < 0x20 || c == '\u{7f}' => {
                let _ = write!(json, "\\u{:04x}", u32::from(c));
            }
            // Line and paragraph separators end a line for some readers.
            '\u{2028}' => json.push_str("\\u2028"),
            '\u{2029}' => json.push_str("\\u2029"),
            c => json.push(c),
        }
    }

    json.push('"');
}

/// Appends a number, or `null` when it is not finite.
fn push_float(json: &mut String, value: f32) {
    if value.is_finite() {
        let _ = write!(json, "{value}");
    } else {
        json.push_str("null");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_single_line_json_objects() {
        let event = Event::new("panic")
            .str(
                "message",
                "a \"quoted\"\nline\\ with \u{0}\u{1b} and \u{2028}",
            )
            .opt_str("location", None)
            .number("pid", 42)
            .opt_number("missing", None::<u32>)
            .finish();

        assert_eq!(
            event,
            "{\"v\":1,\"kind\":\"panic\",\"message\":\"a \\\"quoted\\\"\\nline\\\\ with \\u0000\\u001b and \\u2028\",\"location\":null,\"pid\":42,\"missing\":null}"
        );
        assert!(!event.contains('\n'));
    }

    #[test]
    fn floats_print_as_json_numbers() {
        let mut json = String::new();

        push_float(&mut json, 402.0);
        json.push(' ');
        push_float(&mut json, 2.625);
        json.push(' ');
        push_float(&mut json, f32::NAN);

        assert_eq!(json, "402 2.625 null");
    }

    #[test]
    fn the_query_string_opts_in() {
        assert!(query_opts_in("?icm_events=1"));
        assert!(query_opts_in("?a=b&icm_events=true"));
        assert!(!query_opts_in("?icm_events=0"));
        assert!(!query_opts_in("?icm_events"));
        assert!(!query_opts_in("?xicm_events=1"));
        assert!(!query_opts_in(""));
    }
}
