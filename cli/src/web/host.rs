//! The web session host: `icm __session web --request <file>`, the detached
//! process `icm run web` starts (design §3 "Sessions", §10.2 step 5).
//!
//! It serves the site on loopback, drives headless Chrome over the
//! DevTools pipe, writes the page's console to `console.ndjson` (as the
//! page logged it; the copies icm keeps in run directories are redacted),
//! and answers icm's control requests (status, screenshot, input, stop) on
//! `POST /__icm/control`. It runs until `icm stop web`, a newer `icm run
//! web` for the project, a signal, or Chrome exiting; then it closes Chrome
//! and removes its session record.
//!
//! Startup is a handshake: the host writes `startup.json` in the session
//! directory, `{"ok": true, ...}` once the page is loading, or
//! `{"ok": false, "id": ..., "detail": ...}` (`web.port_busy`,
//! `web.chrome_failed`), and `icm run web` waits for it.
//!
//! The files icm itself writes there that name the page's URL, whose query
//! may carry a secret, are private and short-lived: `request.json` (mode
//! 0600) goes once the host has read it, `startup.json` (0600) once `icm
//! run web` has, and the Chrome profile when the host ends
//! ([`remove_private_files`] after a host that could not). The host counts
//! the query's secret-named values as secrets, so its own output
//! (`session.log`) has them redacted.

use super::cdp::{self, Conn};
use super::console;
use super::page::PageLog;
use super::server::{self, Handler};
use super::viewport::Viewport;
use crate::catalogue::CheckId;
use crate::context::Ctx;
use crate::error::{Evidence, IcmError, Result};
use crate::sessions;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the host's command line contains (the session record's `marker`).
pub const MARKER: &str = "__session web";

/// How long one DevTools command may take.
const CDP_TIMEOUT: Duration = Duration::from_secs(20);

/// What `icm run web` asks the host to do (written to a file, so the
/// command line stays short and free of secrets).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    /// The project directory.
    pub project_dir: PathBuf,
    /// `target/icm/sessions`.
    pub sessions_dir: PathBuf,
    /// `target/icm/sessions/web`: console, Chrome profile and logs.
    pub session_dir: PathBuf,
    /// The site to serve.
    pub site: PathBuf,
    /// The port (0: any free port).
    pub port: u16,
    /// The emulated viewport.
    pub viewport: Value,
    /// The Chrome executable.
    pub chrome: PathBuf,
    /// The run that started the session.
    pub run: String,
    /// That run's directory.
    pub run_dir: Option<PathBuf>,
    /// Extra query parameters for the page (`--env K=V` → `k=v`).
    pub query: Vec<(String, String)>,
    /// The result's `app` object.
    pub app: Value,
    /// `debug` or `release`.
    pub profile: String,
}

/// The files of a session directory.
pub struct Files {
    /// The console records.
    pub console: PathBuf,
    /// The startup handshake.
    pub startup: PathBuf,
    /// Chrome's own output.
    pub chrome_log: PathBuf,
    /// The host's output.
    pub host_log: PathBuf,
    /// The request.
    pub request: PathBuf,
}

impl Files {
    /// The files under a session directory.
    pub fn new(session_dir: &Path) -> Files {
        Files {
            console: session_dir.join("console.ndjson"),
            startup: session_dir.join("startup.json"),
            chrome_log: session_dir.join("chrome.log"),
            host_log: session_dir.join("session.log"),
            request: session_dir.join("request.json"),
        }
    }
}

/// The page URL for a port and query.
pub fn page_url(port: u16, query: &[(String, String)], base_query: &str) -> String {
    let mut url = format!("http://127.0.0.1:{port}/?{base_query}");
    for (key, value) in query {
        url.push('&');
        url.push_str(&encode_component(key));
        url.push('=');
        url.push_str(&encode_component(value));
    }
    url
}

/// Percent-encodes a query component ([`crate::process::url_encoded`],
/// which the secret values' forms share).
pub fn encode_component(text: &str) -> String {
    crate::process::url_encoded(text)
}

/// A random hex token from the system's random source.
pub fn token() -> String {
    let mut bytes = [0u8; 16];
    let read = File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut bytes));
    if read.is_err() {
        // Not secret-grade, but unique enough to tell sessions apart.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        bytes = (nanos ^ u128::from(std::process::id())).to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The session's shared state: written by the DevTools reader thread and
/// the server's threads, read by control requests.
struct State {
    token: String,
    url: String,
    log: PageLog,
    conn: Mutex<Option<Arc<Conn>>>,
    page: Mutex<Option<String>>,
    viewport: Mutex<Viewport>,
    product: Mutex<String>,
    chrome_alive: AtomicBool,
    stop: AtomicBool,
    navigated_ms: AtomicU64,
    chrome_pid: AtomicU64,
}

impl State {
    fn write(&self, record: &Value) {
        self.log.write(record);
    }

    fn conn_and_page(&self) -> std::result::Result<(Arc<Conn>, String), String> {
        let conn = self
            .conn
            .lock()
            .ok()
            .and_then(|conn| conn.clone())
            .ok_or("Chrome is not connected")?;
        let page = self
            .page
            .lock()
            .ok()
            .and_then(|page| page.clone())
            .ok_or("no page is attached")?;
        Ok((conn, page))
    }

    fn page_call(&self, method: &str, params: Value) -> std::result::Result<Value, String> {
        let (conn, page) = self.conn_and_page()?;
        conn.call(Some(&page), method, params, CDP_TIMEOUT)
    }

    fn status(&self, probe: bool) -> Value {
        let canvas = if probe {
            self.page_call(
                "Runtime.evaluate",
                json!({
                    "expression": "(() => { const c = document.querySelector('canvas'); return c ? [c.width, c.height] : null; })()",
                    "returnByValue": true,
                }),
            )
            .ok()
            .and_then(|reply| reply.get("result").and_then(|r| r.get("value")).cloned())
            .filter(|value| !value.is_null())
        } else {
            None
        };
        json!({
            "ok": true,
            "url": self.url,
            "start": self.log.start(),
            "ready": self.log.ready(),
            "panics": self.log.panics(),
            "warnings": self.log.warnings(),
            "exceptions": self.log.exceptions(),
            "errors": self.log.errors(),
            "crashed": self.log.crashed(),
            "chrome": {
                "alive": self.chrome_alive.load(Ordering::SeqCst),
                "pid": self.chrome_pid.load(Ordering::SeqCst),
                "product": self.product.lock().map(|p| p.clone()).unwrap_or_default(),
            },
            "navigated_ms": self.navigated_ms.load(Ordering::SeqCst),
            "viewport": self.viewport.lock().map(|v| v.to_json()).unwrap_or(Value::Null),
            "canvas": canvas,
        })
    }

    fn screenshot(&self, out: &Path) -> std::result::Result<Value, String> {
        let reply = self.page_call(
            "Page.captureScreenshot",
            json!({"format": "png", "fromSurface": true, "captureBeyondViewport": false}),
        )?;
        let data = reply
            .get("data")
            .and_then(Value::as_str)
            .ok_or("Page.captureScreenshot returned no data")?;
        let bytes = cdp::base64_decode(data)?;
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        std::fs::write(out, &bytes)
            .map_err(|error| format!("cannot write {}: {error}", out.display()))?;
        let viewport = self
            .viewport
            .lock()
            .map(|v| v.to_json())
            .unwrap_or(Value::Null);
        Ok(json!({"ok": true, "path": out, "bytes": bytes.len(), "viewport": viewport}))
    }

    fn mobile(&self) -> bool {
        self.viewport.lock().map(|v| v.mobile).unwrap_or(false)
    }

    fn mouse(&self, kind: &str, x: f64, y: f64, buttons: u32) -> std::result::Result<(), String> {
        let mut params = json!({"type": kind, "x": x, "y": y, "buttons": buttons});
        if kind != "mouseMoved" {
            params["button"] = json!("left");
            params["clickCount"] = json!(1);
        }
        self.page_call("Input.dispatchMouseEvent", params)
            .map(|_| ())
    }

    fn touch(&self, kind: &str, points: &[(f64, f64)]) -> std::result::Result<(), String> {
        let points: Vec<Value> = points
            .iter()
            .map(|(x, y)| json!({"x": x, "y": y, "id": 0}))
            .collect();
        self.page_call(
            "Input.dispatchTouchEvent",
            json!({"type": kind, "touchPoints": points}),
        )
        .map(|_| ())
    }

    fn tap(&self, x: f64, y: f64) -> std::result::Result<Value, String> {
        if self.mobile() {
            self.touch("touchStart", &[(x, y)])?;
            std::thread::sleep(Duration::from_millis(50));
            self.touch("touchEnd", &[])?;
            Ok(json!({"ok": true, "pointer": "touch"}))
        } else {
            self.mouse("mouseMoved", x, y, 0)?;
            self.mouse("mousePressed", x, y, 1)?;
            std::thread::sleep(Duration::from_millis(30));
            self.mouse("mouseReleased", x, y, 0)?;
            Ok(json!({"ok": true, "pointer": "mouse"}))
        }
    }

    fn swipe(
        &self,
        from: (f64, f64),
        to: (f64, f64),
        ms: u64,
    ) -> std::result::Result<Value, String> {
        let steps = (ms / 16).clamp(2, 120);
        let pause = Duration::from_millis(ms / steps);
        let point = |i: u64| {
            let t = i as f64 / steps as f64;
            (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t)
        };
        if self.mobile() {
            self.touch("touchStart", &[from])?;
            for i in 1..=steps {
                std::thread::sleep(pause);
                self.touch("touchMove", &[point(i)])?;
            }
            self.touch("touchEnd", &[])?;
            Ok(json!({"ok": true, "pointer": "touch"}))
        } else {
            self.mouse("mouseMoved", from.0, from.1, 0)?;
            self.mouse("mousePressed", from.0, from.1, 1)?;
            for i in 1..=steps {
                std::thread::sleep(pause);
                let (x, y) = point(i);
                self.mouse("mouseMoved", x, y, 1)?;
            }
            self.mouse("mouseReleased", to.0, to.1, 0)?;
            Ok(json!({"ok": true, "pointer": "mouse"}))
        }
    }

    fn key(&self, key: &str) -> std::result::Result<(), String> {
        let (name, code, vk, text) = match key {
            "enter" => ("Enter", "Enter", 13, Some("\r")),
            "tab" => ("Tab", "Tab", 9, None),
            "escape" => ("Escape", "Escape", 27, None),
            "backspace" => ("Backspace", "Backspace", 8, None),
            other => return Err(format!("no key `{other}` on the web")),
        };
        let mut down = json!({
            "type": if text.is_some() { "keyDown" } else { "rawKeyDown" },
            "key": name, "code": code,
            "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk,
        });
        if let Some(text) = text {
            down["text"] = json!(text);
            down["unmodifiedText"] = json!(text);
        }
        self.page_call("Input.dispatchKeyEvent", down)?;
        self.page_call(
            "Input.dispatchKeyEvent",
            json!({"type": "keyUp", "key": name, "code": code,
                   "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk}),
        )?;
        Ok(())
    }

    fn text(&self, text: &str) -> std::result::Result<(), String> {
        for c in text.chars() {
            if c == '\n' {
                self.key("enter")?;
                continue;
            }
            let s = c.to_string();
            self.page_call(
                "Input.dispatchKeyEvent",
                json!({"type": "keyDown", "key": s, "text": s, "unmodifiedText": s}),
            )?;
            self.page_call("Input.dispatchKeyEvent", json!({"type": "keyUp", "key": s}))?;
        }
        Ok(())
    }

    fn set_viewport(&self, viewport: Viewport) -> std::result::Result<(), String> {
        self.page_call("Emulation.setDeviceMetricsOverride", viewport.metrics())?;
        if let Ok(mut current) = self.viewport.lock() {
            *current = viewport;
        }
        Ok(())
    }

    fn handle(&self, request: &Value) -> std::result::Result<Value, String> {
        let op = request.get("op").and_then(Value::as_str).unwrap_or("");
        let number = |key: &str| -> std::result::Result<f64, String> {
            request
                .get(key)
                .and_then(Value::as_f64)
                .ok_or_else(|| format!("`{op}` needs a number `{key}`"))
        };
        match op {
            "status" => Ok(self.status(
                request
                    .get("probe")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            )),
            "screenshot" => {
                let out = request
                    .get("out")
                    .and_then(Value::as_str)
                    .ok_or("`screenshot` needs `out`")?;
                self.screenshot(Path::new(out))
            }
            "tap" => self.tap(number("x")?, number("y")?),
            "swipe" => self.swipe(
                (number("x1")?, number("y1")?),
                (number("x2")?, number("y2")?),
                request.get("ms").and_then(Value::as_u64).unwrap_or(300),
            ),
            "text" => {
                let text = request
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or("`text` needs `text`")?;
                self.text(text).map(|()| json!({"ok": true}))
            }
            "key" => {
                let key = request
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or("`key` needs `key`")?;
                self.key(key).map(|()| json!({"ok": true}))
            }
            "appearance" => {
                let mode = request
                    .get("mode")
                    .and_then(Value::as_str)
                    .ok_or("`appearance` needs `mode`")?;
                self.page_call(
                    "Emulation.setEmulatedMedia",
                    json!({"features": [{"name": "prefers-color-scheme", "value": mode}]}),
                )
                .map(|_| json!({"ok": true}))
            }
            "rotate" => {
                let landscape =
                    request.get("orientation").and_then(Value::as_str) == Some("landscape");
                let viewport = self
                    .viewport
                    .lock()
                    .map(|v| v.oriented(landscape))
                    .map_err(|_| "the viewport is poisoned".to_string())?;
                self.set_viewport(viewport.clone())?;
                Ok(json!({"ok": true, "viewport": viewport.to_json()}))
            }
            "stop" => {
                self.stop.store(true, Ordering::SeqCst);
                Ok(json!({"ok": true}))
            }
            "eval" => {
                // Diagnostics: evaluate an expression in the page.
                let expression = request
                    .get("expression")
                    .and_then(Value::as_str)
                    .ok_or("`eval` needs `expression`")?;
                let reply = self.page_call(
                    "Runtime.evaluate",
                    json!({"expression": expression, "returnByValue": true, "awaitPromise": true}),
                )?;
                Ok(
                    json!({"ok": true, "result": reply.get("result"), "exception": reply.get("exceptionDetails")}),
                )
            }
            other => Err(format!("unknown control op `{other}`")),
        }
    }
}

struct Control(Arc<State>);

impl Handler for Control {
    fn token(&self) -> &str {
        &self.0.token
    }

    fn control(&self, request: &Value) -> (u16, Value) {
        match self.0.handle(request) {
            Ok(reply) => (200, reply),
            Err(error) => (500, json!({"ok": false, "error": error})),
        }
    }

    fn forwarded(&self, record: &Value) {
        let field = |key: &str| record.get(key).and_then(Value::as_str).unwrap_or("");
        let level = console::console_level(field("level"));
        let epoch = record
            .get("epoch_ms")
            .and_then(Value::as_u64)
            .unwrap_or_else(console::now_ms);
        let msg = field("msg");
        let tag = if console::icm_event(msg).is_some() {
            console::ICM_EVENT_TAG
        } else {
            ""
        };
        self.0
            .log
            .write(&console::record(epoch, "forwarder", level, tag, msg));
    }
}

/// Writes the startup handshake, readable only by the user: it names the
/// page's URL, whose query may carry a secret. `icm run web` removes it
/// once it has read it.
fn startup(files: &Files, value: &Value) {
    let _ = crate::output::rundir::write_private(
        &files.startup,
        serde_json::to_string_pretty(value)
            .unwrap_or_default()
            .as_bytes(),
    );
}

/// Removes what a session leaves in its directory that holds the page's
/// URL, and so any secret its query carries, without being the page's,
/// Chrome's or the host's output: the request, the startup handshake and
/// the Chrome profile (its history, sessions and top sites). The host does
/// when it ends, and `icm stop web` and the next session do after a host
/// that could not.
pub fn remove_private_files(session_dir: &Path) {
    let files = Files::new(session_dir);
    let _ = std::fs::remove_file(&files.request);
    let _ = std::fs::remove_file(&files.startup);
    let _ = std::fs::remove_dir_all(cdp::profile_dir(session_dir));
}

/// Removes the Chrome profile when the host ends, however it ends (after
/// Chrome, which is closed first).
struct ProfileGuard(PathBuf);

impl Drop for ProfileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fail(files: &Files, id: CheckId, detail: String) -> IcmError {
    startup(
        files,
        &json!({"ok": false, "id": id.id(), "detail": detail, "pid": std::process::id()}),
    );
    IcmError::new(id, detail)
}

/// Runs the host (`icm __session web --request <file>`).
pub fn main(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    let usage = || {
        IcmError::new(
            CheckId::UsageBadArgs,
            "usage: icm __session web --request <file> (internal; started by `icm run web`)",
        )
    };
    let (Some("web"), Some("--request"), Some(path)) = (
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
        args.get(2),
    ) else {
        return Err(usage());
    };

    let text = std::fs::read_to_string(path)
        .map_err(|error| IcmError::new(CheckId::UsageBadArgs, format!("{path}: {error}")))?;
    let request: Request = serde_json::from_str(&text)
        .map_err(|error| IcmError::new(CheckId::UsageBadArgs, format!("{path}: {error}")))?;
    // The request holds the query's values as given: read once, then gone.
    let _ = std::fs::remove_file(path);
    // A secret-named pair is a secret here too: the host's own output
    // (`session.log`) names the page's URL.
    for (key, value) in &request.query {
        crate::process::remember_secret(key, value);
    }
    let files = Files::new(&request.session_dir);
    let viewport = Viewport::from_json(&request.viewport)
        .ok_or_else(|| IcmError::new(CheckId::UsageBadArgs, "the request has no viewport"))?;

    // A fresh console and Chrome profile for every session.
    let _ = std::fs::remove_file(&files.console);
    let profile = cdp::profile_dir(&request.session_dir);
    let _ = std::fs::remove_dir_all(&profile);
    let _profile = ProfileGuard(profile.clone());
    let _ = std::fs::write(&files.chrome_log, b"");

    let listener = server::bind(request.port).map_err(|error| {
        fail(
            &files,
            CheckId::WebPortBusy,
            format!("cannot listen on 127.0.0.1:{}: {error}", request.port),
        )
    })?;
    let port = listener
        .local_addr()
        .map(|a| a.port())
        .unwrap_or(request.port);

    let console_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&files.console)
        .map_err(|error| {
            fail(
                &files,
                CheckId::InternalBug,
                format!("cannot open {}: {error}", files.console.display()),
            )
        })?;

    let url = page_url(port, &request.query, super::site::HEADLESS_QUERY);
    let state = Arc::new(State {
        token: token(),
        url: url.clone(),
        log: PageLog::new(Some(console_file)),
        conn: Mutex::new(None),
        page: Mutex::new(None),
        viewport: Mutex::new(viewport.clone()),
        product: Mutex::new(String::new()),
        chrome_alive: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        navigated_ms: AtomicU64::new(0),
        chrome_pid: AtomicU64::new(0),
    });
    server::serve(
        listener,
        request.site.clone(),
        Arc::new(Control(Arc::clone(&state))),
    );

    let events = Arc::clone(&state);
    let mut browser = cdp::launch(
        &request.chrome,
        &cdp::chrome_args(&profile, viewport.scale),
        &files.chrome_log,
        Box::new(move |event| events.log.on_event(&event)),
    )
    .map_err(|error| {
        fail(
            &files,
            CheckId::WebChromeFailed,
            format!("cannot start {}: {error}", request.chrome.display()),
        )
    })?;
    state.chrome_alive.store(true, Ordering::SeqCst);
    state
        .chrome_pid
        .store(u64::from(browser.child.id()), Ordering::SeqCst);
    if let Ok(mut conn) = state.conn.lock() {
        *conn = Some(Arc::clone(&browser.conn));
    }

    let setup = (|| -> std::result::Result<String, String> {
        let version = browser
            .conn
            .call(None, "Browser.getVersion", json!({}), CDP_TIMEOUT)?;
        let product = version
            .get("product")
            .and_then(Value::as_str)
            .unwrap_or("Chrome")
            .to_string();
        if let Ok(mut slot) = state.product.lock() {
            *slot = product.clone();
        }
        let page = cdp::attach_page(&browser.conn, Duration::from_secs(10))?;
        if let Ok(mut slot) = state.page.lock() {
            *slot = Some(page);
        }
        for method in [
            "Runtime.enable",
            "Log.enable",
            "Page.enable",
            "Inspector.enable",
        ] {
            let _ = state.page_call(method, json!({}))?;
        }
        let _ = state.page_call("Emulation.setDeviceMetricsOverride", viewport.metrics())?;
        if viewport.mobile {
            let _ = state.page_call(
                "Emulation.setTouchEmulationEnabled",
                json!({"enabled": true, "maxTouchPoints": 5}),
            )?;
        }
        Ok(product)
    })();
    let product = match setup {
        Ok(product) => product,
        Err(error) => {
            browser.close();
            return Err(fail(
                &files,
                CheckId::WebChromeFailed,
                format!(
                    "Chrome did not accept the DevTools session: {error}{}",
                    tail_of(&files.chrome_log)
                ),
            ));
        }
    };

    let pid = std::process::id() as i32;
    // What the pid is now, for a later command to check before it signals
    // the pid (a pid is reused once this process exits). A host the OS will
    // not describe to itself would be taken for no live session by every
    // later command (a record whose identity is unavailable is never
    // alive), and nothing could stop it, so it does not start.
    let identity = crate::procid::capture(pid);
    if let Some(reason) = &identity.unavailable {
        browser.close();
        return Err(fail(
            &files,
            CheckId::WebHostIdentity,
            format!(
                "cannot read the process identity of this web session host (pid {pid}: {reason}), so no later command could tell it from another process that takes its pid, or stop it"
            ),
        ));
    }
    let record = json!({
        "schema": sessions::SCHEMA,
        "platform": "web",
        "pid": pid,
        "pgid": pid,
        "identity": identity,
        "marker": MARKER,
        "run": request.run,
        "run_dir": request.run_dir,
        "started": crate::time::Utc::now().rfc3339(),
        "project_dir": request.project_dir,
        "app": request.app,
        "profile": request.profile,
        "url": url.replace(super::site::HEADLESS_QUERY, super::site::SHOW_QUERY),
        "page_url": url,
        "port": port,
        "site": request.site,
        "viewport": viewport.to_json(),
        "control": {"url": format!("http://127.0.0.1:{port}{}", server::CONTROL_PATH), "token": state.token},
        "chrome": {"pid": browser.child.id(), "path": request.chrome, "product": product},
        "logs": {
            "console": files.console,
            "chrome": files.chrome_log,
            "session": files.host_log,
        },
    });
    if let Err(error) = sessions::write(&request.sessions_dir, "web", &record) {
        browser.close();
        return Err(fail(
            &files,
            CheckId::InternalBug,
            format!("cannot write the session record: {error}"),
        ));
    }

    state
        .navigated_ms
        .store(console::now_ms(), Ordering::SeqCst);
    match state.page_call("Page.navigate", json!({"url": url})) {
        Ok(reply) if reply.get("errorText").is_none() => {}
        Ok(reply) => {
            browser.close();
            sessions::remove(&request.sessions_dir, "web", pid);
            return Err(fail(
                &files,
                CheckId::WebChromeFailed,
                format!(
                    "Chrome could not load {url}: {}",
                    reply["errorText"].as_str().unwrap_or("error")
                ),
            ));
        }
        Err(error) => {
            browser.close();
            sessions::remove(&request.sessions_dir, "web", pid);
            return Err(fail(&files, CheckId::WebChromeFailed, error));
        }
    }

    startup(
        &files,
        &json!({"ok": true, "pid": pid, "port": port, "url": url, "product": product}),
    );
    ctx.rep
        .progress(format!("serving {} on {url}", request.site.display()));

    // Serve until asked to stop, signalled, or Chrome goes away.
    let ended = loop {
        if let Some(signal) = crate::signals::pending() {
            break format!("received {}", crate::signals::name(signal));
        }
        if state.stop.load(Ordering::SeqCst) {
            break "stopped by icm".to_string();
        }
        if !browser.running() || browser.conn.is_closed() {
            state.chrome_alive.store(false, Ordering::SeqCst);
            state.write(&console::record(
                console::now_ms(),
                "crash",
                "error",
                "chrome",
                "headless Chrome exited; the session ends",
            ));
            break "Chrome exited".to_string();
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    ctx.rep.progress(format!("ending the session: {ended}"));
    state.chrome_alive.store(false, Ordering::SeqCst);
    browser.close();
    sessions::remove(&request.sessions_dir, "web", pid);
    let _ = std::fs::remove_file(&files.startup);
    state.log.flush();
    ctx.rep.summary(format!("the web session ended: {ended}"));
    if ended == "Chrome exited" {
        return Err(IcmError::new(CheckId::WebChromeFailed, ended)
            .evidence(Evidence::file(&files.chrome_log)));
    }
    Ok(())
}

/// The last lines of a log, for error details.
fn tail_of(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.contains("CVDisplayLink") && !line.trim().is_empty())
        .collect();
    let tail = lines[lines.len().saturating_sub(5)..].join("\n");
    if tail.is_empty() {
        String::new()
    } else {
        format!("\n{tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_carry_the_query() {
        assert_eq!(
            page_url(8787, &[], "icm_events=1&icm_cdp=1"),
            "http://127.0.0.1:8787/?icm_events=1&icm_cdp=1"
        );
        assert_eq!(
            page_url(
                1,
                &[("rust_log".into(), "info,app=debug".into())],
                "icm_events=1"
            ),
            "http://127.0.0.1:1/?icm_events=1&rust_log=info,app%3Ddebug"
        );
    }

    #[test]
    fn tokens_are_random_hex() {
        let (a, b) = (token(), token());
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
