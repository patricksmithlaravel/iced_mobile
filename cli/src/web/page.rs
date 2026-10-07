//! What a page in headless Chrome reports over the DevTools protocol, as
//! the session host ([`super::host`]) and the release serve check
//! ([`super::smoke`]) both record it: console lines (with `ICM_EVENT` lines
//! parsed into `start`, `ready`, `panic` and `warning`), uncaught
//! exceptions, Chrome's own log, renderer crashes and, when the Network
//! domain is enabled, the responses the page received.
//!
//! Every record also goes to `console.ndjson` (see [`super::console`]).

use super::console;
use serde_json::{Value, json};
use std::fs::File;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// How many exception records [`PageLog::exceptions`] keeps.
const KEEP_EXCEPTIONS: usize = 5;

/// How many error records [`PageLog::error_records`] keeps.
const KEEP_ERRORS: usize = 20;

/// How many responses [`PageLog::responses`] keeps.
const KEEP_RESPONSES: usize = 500;

/// The page's log so far.
pub struct PageLog {
    console: Mutex<Option<File>>,
    start: Mutex<Option<Value>>,
    ready: Mutex<Option<Value>>,
    panics: Mutex<Vec<Value>>,
    warnings: Mutex<Vec<Value>>,
    exceptions: Mutex<Vec<Value>>,
    error_records: Mutex<Vec<Value>>,
    responses: Mutex<Vec<Value>>,
    errors: AtomicU64,
    crashed: AtomicBool,
}

impl PageLog {
    /// A log that appends its records to `console` (if given).
    pub fn new(console: Option<File>) -> PageLog {
        PageLog {
            console: Mutex::new(console),
            start: Mutex::new(None),
            ready: Mutex::new(None),
            panics: Mutex::new(Vec::new()),
            warnings: Mutex::new(Vec::new()),
            exceptions: Mutex::new(Vec::new()),
            error_records: Mutex::new(Vec::new()),
            responses: Mutex::new(Vec::new()),
            errors: AtomicU64::new(0),
            crashed: AtomicBool::new(false),
        }
    }

    /// Writes a record to the console file (error records are also kept
    /// for [`PageLog::error_records`]).
    pub fn write(&self, record: &Value) {
        if record["level"] == "error"
            && record["tag"] != console::ICM_EVENT_TAG
            && let Ok(mut kept) = self.error_records.lock()
            && kept.len() < KEEP_ERRORS
        {
            kept.push(record.clone());
        }
        if let Ok(mut console) = self.console.lock()
            && let Some(file) = console.as_mut()
        {
            let mut line = serde_json::to_string(record).unwrap_or_default();
            line.push('\n');
            let _ = file.write_all(line.as_bytes());
        }
    }

    /// Flushes the console file.
    pub fn flush(&self) {
        if let Ok(mut console) = self.console.lock()
            && let Some(file) = console.as_mut()
        {
            let _ = file.flush();
        }
    }

    /// One DevTools event.
    pub fn on_event(&self, event: &Value) {
        let method = event.get("method").and_then(Value::as_str).unwrap_or("");
        let params = event.get("params").cloned().unwrap_or(Value::Null);
        let str_of = |value: &Value, key: &str| -> String {
            value
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };

        match method {
            "Runtime.consoleAPICalled" => {
                let kind = str_of(&params, "type");
                let args = params
                    .get("args")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let msg = console::format_args(&args);
                let epoch = params
                    .get("timestamp")
                    .and_then(Value::as_f64)
                    .map(|ms| ms as u64)
                    .unwrap_or_else(console::now_ms);
                let level = console::console_level(&kind);
                let mut tag = "";
                if let Some(icm) = console::icm_event(&msg) {
                    tag = console::ICM_EVENT_TAG;
                    self.on_icm_event(icm, epoch);
                } else if level == "error" {
                    let _ = self.errors.fetch_add(1, Ordering::SeqCst);
                }
                self.write(&console::record(epoch, "console", level, tag, &msg));
            }
            "Runtime.exceptionThrown" => {
                let details = params
                    .get("exceptionDetails")
                    .cloned()
                    .unwrap_or(Value::Null);
                let description = details
                    .get("exception")
                    .and_then(|e| e.get("description"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| str_of(&details, "text"));
                let url = str_of(&details, "url");
                let location = if url.is_empty() {
                    String::new()
                } else {
                    format!(
                        " (at {url}:{}:{})",
                        details
                            .get("lineNumber")
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                            + 1,
                        details
                            .get("columnNumber")
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                            + 1
                    )
                };
                let record = console::record(
                    console::now_ms(),
                    "exception",
                    "error",
                    "exception",
                    &format!("{description}{location}"),
                );
                let _ = self.errors.fetch_add(1, Ordering::SeqCst);
                if let Ok(mut exceptions) = self.exceptions.lock()
                    && exceptions.len() < KEEP_EXCEPTIONS
                {
                    exceptions.push(record.clone());
                }
                self.write(&record);
            }
            "Log.entryAdded" => {
                let entry = params.get("entry").cloned().unwrap_or(Value::Null);
                let mut msg = str_of(&entry, "text");
                let url = str_of(&entry, "url");
                if !url.is_empty() && !msg.contains(&url) {
                    msg.push_str(&format!(" ({url})"));
                }
                let epoch = entry
                    .get("timestamp")
                    .and_then(Value::as_f64)
                    .map(|ms| ms as u64)
                    .unwrap_or_else(console::now_ms);
                self.write(&console::record(
                    epoch,
                    "browser",
                    console::browser_level(&str_of(&entry, "level"), &msg),
                    &str_of(&entry, "source"),
                    &msg,
                ));
            }
            "Network.responseReceived" => {
                let response = params.get("response").cloned().unwrap_or(Value::Null);
                let url = str_of(&response, "url");
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    return;
                }
                let header = |name: &str| -> Value {
                    response
                        .get("headers")
                        .and_then(Value::as_object)
                        .and_then(|headers| {
                            headers
                                .iter()
                                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                                .and_then(|(_, value)| value.as_str())
                        })
                        .map_or(Value::Null, |value| json!(value))
                };
                let entry = json!({
                    "url": url,
                    "status": response.get("status").and_then(Value::as_f64).map(|s| s as u64),
                    "mime": str_of(&response, "mimeType"),
                    "type": str_of(&params, "type"),
                    "content_type": header("content-type"),
                    "cache_control": header("cache-control"),
                });
                if let Ok(mut responses) = self.responses.lock()
                    && responses.len() < KEEP_RESPONSES
                {
                    responses.push(entry);
                }
            }
            "Inspector.targetCrashed" | "Target.targetCrashed" => {
                self.crashed.store(true, Ordering::SeqCst);
                self.write(&console::record(
                    console::now_ms(),
                    "crash",
                    "error",
                    "renderer",
                    "the page's renderer process crashed",
                ));
            }
            _ => {}
        }
    }

    fn on_icm_event(&self, mut event: Value, epoch: u64) {
        event["epoch_ms"] = json!(epoch);
        let slot = match event.get("kind").and_then(Value::as_str).unwrap_or("") {
            "ready" => &self.ready,
            "start" => &self.start,
            "panic" => {
                if let Ok(mut panics) = self.panics.lock() {
                    panics.push(event);
                }
                return;
            }
            "warning" => {
                if let Ok(mut warnings) = self.warnings.lock() {
                    warnings.push(event);
                }
                return;
            }
            _ => return,
        };
        if let Ok(mut slot) = slot.lock()
            && slot.is_none()
        {
            *slot = Some(event);
        }
    }

    /// The first `ICM_EVENT start`.
    pub fn start(&self) -> Option<Value> {
        self.start.lock().ok().and_then(|v| v.clone())
    }

    /// The first `ICM_EVENT ready`.
    pub fn ready(&self) -> Option<Value> {
        self.ready.lock().ok().and_then(|v| v.clone())
    }

    /// Every `ICM_EVENT panic`.
    pub fn panics(&self) -> Vec<Value> {
        self.panics.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// Every `ICM_EVENT warning`.
    pub fn warnings(&self) -> Vec<Value> {
        self.warnings.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// The first uncaught exceptions.
    pub fn exceptions(&self) -> Vec<Value> {
        self.exceptions
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// The first error-level records of any source (console errors,
    /// exceptions, failed loads, crashes), `ICM_EVENT` lines excepted.
    pub fn error_records(&self) -> Vec<Value> {
        self.error_records
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// The http(s) responses the page received (with the Network domain
    /// enabled): `{url, status, mime, type, content_type, cache_control}`.
    pub fn responses(&self) -> Vec<Value> {
        self.responses.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// Console errors and exceptions so far.
    pub fn errors(&self) -> u64 {
        self.errors.load(Ordering::SeqCst)
    }

    /// Whether the renderer crashed.
    pub fn crashed(&self) -> bool {
        self.crashed.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn console_event(kind: &str, text: &str) -> Value {
        json!({"method": "Runtime.consoleAPICalled", "params": {
            "type": kind, "timestamp": 1000.0, "args": [{"type": "string", "value": text}]
        }})
    }

    #[test]
    fn events_are_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("console.ndjson");
        let log = PageLog::new(Some(File::create(&path).unwrap()));
        log.on_event(&console_event(
            "log",
            r#"ICM_EVENT {"v":1,"kind":"start","protocol":1}"#,
        ));
        log.on_event(&console_event(
            "log",
            r#"ICM_EVENT {"v":1,"kind":"ready","window":{"size":[2,3]}}"#,
        ));
        log.on_event(&console_event("error", "boom"));
        log.on_event(&json!({"method": "Log.entryAdded", "params": {"entry": {
            "level": "error", "source": "network", "text": "Failed to load resource", "url": "http://h/x.js"
        }}}));
        log.on_event(&json!({"method": "Network.responseReceived", "params": {"type": "Fetch", "response": {
            "url": "http://h/pkg/app_bg-0123abcd.wasm", "status": 200, "mimeType": "application/wasm",
            "headers": {"Content-Type": "application/wasm", "cache-control": "no-store"}
        }}}));
        log.on_event(
            &json!({"method": "Network.responseReceived", "params": {"response": {
                "url": "data:,", "status": 200, "mimeType": "text/plain"
            }}}),
        );
        log.flush();

        assert_eq!(log.start().unwrap()["protocol"], 1);
        assert_eq!(log.ready().unwrap()["window"]["size"][1], 3);
        assert_eq!(log.ready().unwrap()["epoch_ms"], 1000);
        assert_eq!(log.errors(), 1);
        let errors = log.error_records();
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0]["msg"], "boom");
        assert_eq!(errors[1]["source"], "browser");
        let responses = log.responses();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["mime"], "application/wasm");
        assert_eq!(responses[0]["content_type"], "application/wasm");
        assert_eq!(responses[0]["cache_control"], "no-store");
        assert_eq!(responses[0]["status"], 200);
        let lines = std::fs::read_to_string(&path).unwrap();
        assert_eq!(lines.lines().count(), 4);
    }
}
