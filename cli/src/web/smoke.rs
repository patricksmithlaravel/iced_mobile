//! The serve check (design §11.3 step 4, `web.serve_smoke`; §12.4): a site
//! loaded in headless Chrome the way a visitor's browser loads it, with
//! `?icm_events=1` so the framework announces itself.
//!
//! - [`Target::Site`]: icm serves a site directory on loopback with the dev
//!   server's code (GET and HEAD only, `.wasm` as `application/wasm`), at
//!   the path `[web] public_url` names, so `<base href>` resolves as on the
//!   host ([`stage`]).
//! - [`Target::Url`]: a deployed site (`icm verify web --url`).
//!
//! Chrome runs with the session's switches and a throwaway profile, with
//! the Network domain on, so the report lists every response with its MIME
//! type: the `.wasm` must be `application/wasm`, since wasm-bindgen's loader
//! otherwise falls back to a slower path with a console warning, and
//! [`checks`] makes that a `web.mime` FAIL. The check waits for `ICM_EVENT
//! ready` (or, from an app that never announces itself, a canvas with a
//! size after 5 s), lets the page settle, takes a screenshot and closes
//! Chrome. It runs inside icm's own process and leaves nothing running.

use super::cdp;
use super::page::PageLog;
use super::server::{self, Handler};
use super::viewport::Viewport;
use crate::catalogue::CheckId;
use crate::error::{Check, Evidence};
use crate::preview::Blankness;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The query the page loads with: the framework's events on.
pub const QUERY: &str = "icm_events=1";

/// How long one DevTools command may take.
const CDP_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a page without `ICM_EVENT start` gets before a drawn canvas
/// counts as ready.
const PROBE_AFTER: Duration = Duration::from_secs(5);

/// What to load.
#[derive(Clone, Debug)]
pub enum Target {
    /// A site directory icm serves on loopback, and the path the page is
    /// at (`/`, or `/app/` for `public_url = "/app/"`).
    Site {
        /// The directory served as the server's root.
        root: PathBuf,
        /// The page's path under it.
        path: String,
    },
    /// A deployed site.
    Url(String),
}

/// How to run the check.
#[derive(Clone, Debug)]
pub struct Options {
    /// Chrome or Chromium.
    pub chrome: PathBuf,
    /// The emulated viewport.
    pub viewport: Viewport,
    /// Where `console.ndjson`, `chrome.log`, `screen.png` and the throwaway
    /// Chrome profile go.
    pub dir: PathBuf,
    /// How long the app has to announce `ready`.
    pub wait: Duration,
    /// How long the page runs after `ready` before the screenshot.
    pub settle: Duration,
}

/// How the page ended up.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// It drew its first frame (`source`: `icm_event` or `probe`).
    Ready {
        /// `icm_event` or `probe`.
        source: &'static str,
        /// From navigation to `ready`.
        ms: Option<u64>,
    },
    /// The app panicked (the `ICM_EVENT panic`).
    Panicked(Value),
    /// Chrome could not load the URL (`net::ERR_…`).
    LoadFailed(String),
    /// The renderer crashed.
    Crashed,
    /// Chrome exited.
    ChromeExited,
    /// Nothing within the wait; `started` when the app sent `start`.
    TimedOut {
        /// Whether `ICM_EVENT start` arrived.
        started: bool,
    },
    /// A signal arrived.
    Interrupted(i32),
}

/// What the check saw.
#[derive(Clone, Debug)]
pub struct Report {
    /// The URL Chrome loaded.
    pub url: String,
    /// Chrome's product string.
    pub product: String,
    /// How it ended.
    pub outcome: Outcome,
    /// The `ready` event (window size, renderer backend).
    pub ready: Value,
    /// `ICM_EVENT warning`s (a missing default font).
    pub warnings: Vec<Value>,
    /// The error records (console errors, exceptions, failed loads).
    pub errors: Vec<Value>,
    /// The http(s) responses.
    pub responses: Vec<Value>,
    /// The screenshot, once ready.
    pub screenshot: Option<PathBuf>,
    /// Its preview.
    pub preview: Option<PathBuf>,
    /// How blank the screenshot is.
    pub blankness: Option<Blankness>,
    /// The console records.
    pub console: PathBuf,
    /// Chrome's output.
    pub chrome_log: PathBuf,
}

impl Report {
    /// The responses for `.wasm` files.
    pub fn wasm_responses(&self) -> Vec<&Value> {
        self.responses
            .iter()
            .filter(|response| {
                let url = response["url"].as_str().unwrap_or("");
                let path = url.split(['?', '#']).next().unwrap_or(url);
                path.ends_with(".wasm")
            })
            .collect()
    }

    /// The result's `smoke` object.
    pub fn to_json(&self) -> Value {
        let (status, source, ms) = match &self.outcome {
            Outcome::Ready { source, ms } => ("ready", Some(*source), *ms),
            Outcome::Panicked(_) => ("panicked", None, None),
            Outcome::LoadFailed(_) => ("load_failed", None, None),
            Outcome::Crashed => ("crashed", None, None),
            Outcome::ChromeExited => ("chrome_exited", None, None),
            Outcome::TimedOut { .. } => ("timed_out", None, None),
            Outcome::Interrupted(_) => ("interrupted", None, None),
        };
        json!({
            "url": self.url,
            "browser": self.product,
            "status": status,
            "ready": {"source": source, "ms": ms},
            "window": self.ready["window"],
            "renderer": {"backend": self.ready["backend"], "api": self.ready["api"]},
            "errors": self.errors.len(),
            "wasm": self.wasm_responses().iter().map(|r| json!({
                "url": r["url"], "status": r["status"], "mime": r["mime"], "cache_control": r["cache_control"],
            })).collect::<Vec<_>>(),
            "blank": self.blankness.map(|b| b.is_blank()),
            "screenshot": self.screenshot.as_deref().map(crate::paths::display),
            "console": crate::paths::display(&self.console),
        })
    }
}

/// The page URL for a deployed site: `?icm_events=1` added to its query.
pub fn url_with_query(url: &str) -> String {
    let (base, fragment) = match url.split_once('#') {
        Some((base, fragment)) => (base, Some(fragment)),
        None => (url, None),
    };
    let separator = if base.contains('?') { '&' } else { '?' };
    let mut out = format!("{base}{separator}{QUERY}");
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

/// Lays out a site for serving at `public_url`'s path: the site itself
/// when that is `/`, else a copy under `<dir>/root/<path>/` (an absolute
/// `public_url` also has its `<base href>` pointed at the local path, so
/// the page loads the copy and not the deployed files). Returns the
/// [`Target::Site`].
pub fn stage(site: &Path, public_url: &str, dir: &Path) -> std::io::Result<Target> {
    let prefix = super::release_site::path_prefix(public_url);
    if prefix == "/" && !public_url.starts_with("https://") {
        return Ok(Target::Site {
            root: site.to_path_buf(),
            path: "/".to_string(),
        });
    }
    let root = dir.join("root");
    let _ = std::fs::remove_dir_all(&root);
    let mut inside = root.clone();
    for part in prefix.split('/').filter(|part| !part.is_empty()) {
        inside.push(part);
    }
    copy_tree(site, &inside)?;
    if public_url.starts_with("https://") {
        for page in ["index.html", "404.html"] {
            let path = inside.join(page);
            if let Ok(text) = std::fs::read_to_string(&path) {
                let base = super::site::escape(&super::release_site::base_href(public_url));
                let text = text.replace(
                    &format!("<base href=\"{base}\">"),
                    &format!("<base href=\"{prefix}\">"),
                );
                std::fs::write(&path, text)?;
            }
        }
    }
    Ok(Target::Site { root, path: prefix })
}

/// Copies a directory tree.
pub fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            let _ = std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// The server's other endpoints: none for the serve check.
struct Static {
    token: String,
}

impl Handler for Static {
    fn token(&self) -> &str {
        &self.token
    }

    fn control(&self, _request: &Value) -> (u16, Value) {
        (404, json!({"ok": false, "error": "no control channel"}))
    }

    fn forwarded(&self, _record: &Value) {}
}

/// Chrome and its throwaway profile, closed and removed when dropped.
struct Running {
    browser: cdp::Browser,
    profile: PathBuf,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.browser.close();
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

/// Runs the check. An `Err` is a failure of the check itself (no port,
/// no Chrome, a DevTools error), not of the site. The console and Chrome's
/// log it leaves in `options.dir` (a run directory) have the secret values
/// icm knows redacted, whatever the outcome.
pub fn run(target: &Target, options: &Options) -> Result<Report, String> {
    let report = drive(target, options);
    for name in ["console.ndjson", "chrome.log"] {
        crate::process::redact_in_place(&options.dir.join(name));
    }
    report
}

/// [`run`] up to Chrome's exit.
fn drive(target: &Target, options: &Options) -> Result<Report, String> {
    std::fs::create_dir_all(&options.dir)
        .map_err(|error| format!("cannot create {}: {error}", options.dir.display()))?;
    let console_path = options.dir.join("console.ndjson");
    let chrome_log = options.dir.join("chrome.log");
    let console_file = std::fs::File::create(&console_path)
        .map_err(|error| format!("cannot create {}: {error}", console_path.display()))?;
    let _ = std::fs::write(&chrome_log, b"");

    let url = match target {
        Target::Site { root, path } => {
            let listener =
                server::bind(0).map_err(|error| format!("cannot listen on 127.0.0.1: {error}"))?;
            let port = listener
                .local_addr()
                .map_err(|error| error.to_string())?
                .port();
            server::serve(
                listener,
                root.clone(),
                Arc::new(Static {
                    token: super::host::token(),
                }),
            );
            format!("http://127.0.0.1:{port}{path}?{QUERY}")
        }
        Target::Url(url) => url_with_query(url),
    };

    let log = Arc::new(PageLog::new(Some(console_file)));
    let events = Arc::clone(&log);
    let profile = options.dir.join("chrome-profile");
    let _ = std::fs::remove_dir_all(&profile);
    let browser = cdp::launch(
        &options.chrome,
        &cdp::chrome_args(&profile, options.viewport.scale),
        &chrome_log,
        Box::new(move |event| events.on_event(&event)),
    )
    .map_err(|error| format!("cannot start {}: {error}", options.chrome.display()))?;
    let mut running = Running { browser, profile };
    let conn = Arc::clone(&running.browser.conn);

    let product = conn
        .call(None, "Browser.getVersion", json!({}), CDP_TIMEOUT)?
        .get("product")
        .and_then(Value::as_str)
        .unwrap_or("Chrome")
        .to_string();
    let page = cdp::attach_page(&conn, Duration::from_secs(10))?;
    let call = |method: &str, params: Value| conn.call(Some(&page), method, params, CDP_TIMEOUT);
    for method in [
        "Runtime.enable",
        "Log.enable",
        "Page.enable",
        "Network.enable",
        "Inspector.enable",
    ] {
        let _ = call(method, json!({}))?;
    }
    let _ = call(
        "Emulation.setDeviceMetricsOverride",
        options.viewport.metrics(),
    )?;
    if options.viewport.mobile {
        let _ = call(
            "Emulation.setTouchEmulationEnabled",
            json!({"enabled": true, "maxTouchPoints": 5}),
        )?;
    }

    let navigated = super::console::now_ms();
    let started = Instant::now();
    let reply = call("Page.navigate", json!({"url": url}))?;
    let mut outcome = match reply.get("errorText").and_then(Value::as_str) {
        Some(error) if !error.is_empty() => Some(Outcome::LoadFailed(error.to_string())),
        _ => None,
    };

    let mut probe_hits = 0;
    while outcome.is_none() {
        if let Some(panic) = log.panics().into_iter().next() {
            outcome = Some(Outcome::Panicked(panic));
        } else if let Some(ready) = log.ready() {
            let received = ready["epoch_ms"].as_u64().unwrap_or(navigated);
            outcome = Some(Outcome::Ready {
                source: "icm_event",
                ms: Some(received.saturating_sub(navigated)),
            });
        } else if log.crashed() {
            outcome = Some(Outcome::Crashed);
        } else if !running.browser.running() || conn.is_closed() {
            outcome = Some(Outcome::ChromeExited);
        } else if let Some(signal) = crate::signals::pending() {
            outcome = Some(Outcome::Interrupted(signal));
        } else if started.elapsed() >= options.wait {
            outcome = Some(Outcome::TimedOut {
                started: log.start().is_some(),
            });
        } else {
            if log.start().is_none() && started.elapsed() >= PROBE_AFTER {
                let canvas = call(
                    "Runtime.evaluate",
                    json!({
                        "expression": "(() => { const c = document.querySelector('canvas'); return c ? [c.width, c.height] : null; })()",
                        "returnByValue": true,
                    }),
                )
                .ok()
                .and_then(|reply| reply["result"]["value"].as_array().cloned());
                if canvas.is_some_and(|size| size.iter().all(|v| v.as_u64().unwrap_or(0) > 0)) {
                    probe_hits += 1;
                    if probe_hits >= 2 {
                        outcome = Some(Outcome::Ready {
                            source: "probe",
                            ms: None,
                        });
                        continue;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let mut outcome = outcome.unwrap_or(Outcome::ChromeExited);

    // Let the page run a little: a panic in the first frames counts.
    let mut screenshot = None;
    let mut preview = None;
    let mut blankness = None;
    if matches!(outcome, Outcome::Ready { .. }) {
        let until = Instant::now() + options.settle;
        while Instant::now() < until {
            if let Some(signal) = crate::signals::pending() {
                outcome = Outcome::Interrupted(signal);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if let Some(panic) = log.panics().into_iter().next() {
            outcome = Outcome::Panicked(panic);
        }
    }
    if matches!(outcome, Outcome::Ready { .. }) {
        let reply = call(
            "Page.captureScreenshot",
            json!({"format": "png", "fromSurface": true, "captureBeyondViewport": false}),
        )?;
        let data = reply
            .get("data")
            .and_then(Value::as_str)
            .ok_or("Page.captureScreenshot returned no data")?;
        let bytes = cdp::base64_decode(data)?;
        let png = options.dir.join("screen.png");
        std::fs::write(&png, &bytes)
            .map_err(|error| format!("cannot write {}: {error}", png.display()))?;
        let preview_path = crate::preview::preview_path(&png);
        let shot = crate::preview::process(&png, &preview_path, options.viewport.scale)?;
        blankness = Some(shot.blankness);
        screenshot = Some(png);
        preview = Some(preview_path);
    }

    drop(running);
    log.flush();
    Ok(Report {
        url,
        product,
        outcome,
        ready: log.ready().unwrap_or(Value::Null),
        warnings: log.warnings(),
        errors: log.error_records(),
        responses: log.responses(),
        screenshot,
        preview,
        blankness,
        console: console_path,
        chrome_log,
    })
}

/// The gates a report decides: `web.serve_smoke` (ready, not blank, no
/// console errors) and `web.mime` (every `.wasm` the page fetched came as
/// `application/wasm`). `deployed` says whether the site is the owner's
/// host (the fix then names the host's settings) or icm's own server.
pub fn checks(report: &Report, deployed: bool) -> Vec<Check> {
    let mut checks = vec![serve_check(report)];
    checks.extend(mime_checks(report, deployed));
    // The framework's own warning: no default font (text draws as nothing).
    for warning in &report.warnings {
        if warning["code"] == "font.default_missing" {
            checks.push(
                Check::warn(
                    CheckId::RunFontMissing,
                    warning["message"]
                        .as_str()
                        .unwrap_or("the default font is missing"),
                )
                .evidence(Evidence::file(&report.console)),
            );
        }
    }
    checks
}

fn first_error(report: &Report) -> Option<String> {
    report
        .errors
        .first()
        .map(|record| record["msg"].as_str().unwrap_or("an error").to_string())
}

fn serve_check(report: &Report) -> Check {
    let console = Evidence::file(&report.console);
    let fix_cmds: &[&str] = &["icm run web --release --json -q"];
    match &report.outcome {
        Outcome::Ready { source, ms } => {
            let when = match ms {
                Some(ms) => format!("ICM_EVENT ready after {ms} ms"),
                None => format!("a drawn canvas (source: {source}; the app sent no ICM_EVENT)"),
            };
            if let Some(blank) = report.blankness.filter(Blankness::is_blank) {
                let mut check = Check::fail(
                    CheckId::WebServeSmoke,
                    format!(
                        "{} loaded ({when}) but its screenshot is blank: {:.1}% of the pixels are {}",
                        report.url,
                        blank.fraction * 100.0,
                        blank.hex()
                    ),
                )
                .fix(
                    "Compare with `icm run web --release`; check the fonts (web.fonts_embedded) and the console.",
                    fix_cmds,
                );
                if let Some(png) = &report.screenshot {
                    check = check.evidence(Evidence::file(png));
                }
                return check.evidence(console);
            }
            if !report.errors.is_empty() {
                return Check::fail(
                    CheckId::WebServeSmoke,
                    format!(
                        "{} loaded ({when}) but the console has {} error(s); the first: {}",
                        report.url,
                        report.errors.len(),
                        first_error(report).unwrap_or_default()
                    ),
                )
                .evidence(console.with_excerpt(first_error(report).unwrap_or_default()))
                .fix("Read the console records and fix what they name.", fix_cmds);
            }
            Check::pass(
                CheckId::WebServeSmoke,
                format!(
                    "{} in {}: {when}, the screenshot is not blank, the console has no errors",
                    report.url, report.product
                ),
            )
        }
        Outcome::Panicked(panic) => {
            let message = panic["message"].as_str().unwrap_or("panic");
            let detail = match panic["location"].as_str() {
                Some(location) => format!("the app panicked at {location}: {message}"),
                None => format!("the app panicked: {message}"),
            };
            Check::fail(CheckId::WebServeSmoke, format!("{}: {detail}", report.url))
                .evidence(console.with_excerpt(format!("ICM_EVENT panic: {message}")))
                .fix(
                    "Fix the panic; `icm run web --release` reproduces it.",
                    fix_cmds,
                )
        }
        Outcome::LoadFailed(error) => Check::fail(
            CheckId::WebServeSmoke,
            format!("Chrome could not load {}: {error}", report.url),
        )
        .evidence(Evidence::file(&report.chrome_log))
        .fix(
            "Check the URL and that the site is deployed and reachable.",
            &[],
        ),
        Outcome::Crashed => Check::fail(
            CheckId::WebServeSmoke,
            format!("{}: the page's renderer crashed", report.url),
        )
        .evidence(console),
        Outcome::ChromeExited => Check::fail(
            CheckId::WebServeSmoke,
            format!("headless Chrome exited while loading {}", report.url),
        )
        .evidence(Evidence::file(&report.chrome_log)),
        Outcome::TimedOut { started } => {
            let mut detail = if *started {
                format!(
                    "{}: the app started but sent no ICM_EVENT ready",
                    report.url
                )
            } else {
                format!(
                    "{}: the app neither started nor drew (no ICM_EVENT start and no canvas)",
                    report.url
                )
            };
            if let Some(error) = first_error(report) {
                detail.push_str(&format!("; the console's first error: {error}"));
            }
            Check::fail(CheckId::WebServeSmoke, detail)
                .evidence(console)
                .fix(
                    "Read the console records; `icm run web --release` shows the same page with logs.",
                    fix_cmds,
                )
        }
        Outcome::Interrupted(signal) => Check::fail(
            CheckId::WebServeSmoke,
            format!(
                "the check was interrupted ({})",
                crate::signals::name(*signal)
            ),
        ),
    }
}

fn mime_checks(report: &Report, deployed: bool) -> Vec<Check> {
    let wasm = report.wasm_responses();
    if wasm.is_empty() {
        if !matches!(report.outcome, Outcome::Ready { .. }) {
            // The page never got as far; web.serve_smoke says why.
            return Vec::new();
        }
        return vec![
            Check::warn(
                CheckId::WebMime,
                format!("{} fetched no .wasm, so its type is unknown", report.url),
            )
            .evidence(Evidence::file(&report.console)),
        ];
    }
    wasm.iter()
        .map(|response| {
            let url = response["url"].as_str().unwrap_or("");
            let mime = response["mime"].as_str().unwrap_or("");
            let status = response["status"].as_u64().unwrap_or(0);
            if mime == "application/wasm" && (200..300).contains(&status) {
                Check::pass(
                    CheckId::WebMime,
                    format!("{url} is served as application/wasm"),
                )
            } else {
                let check = Check::fail(
                    CheckId::WebMime,
                    format!(
                        "{url} is served with status {status} as `{}` (Content-Type: {}); browsers need application/wasm to stream-compile it",
                        if mime.is_empty() { "nothing" } else { mime },
                        response["content_type"].as_str().unwrap_or("none")
                    ),
                );
                if deployed {
                    check.fix(
                        "Set the host's type for .wasm to application/wasm: `_headers` (Netlify, Cloudflare Pages), hosting/ in the release (nginx, Apache, Caddy), or the `aws s3 cp --content-type application/wasm` line in UPLOAD.md; then deploy again.",
                        &["icm upload-commands web"],
                    )
                } else {
                    check
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(outcome: Outcome) -> Report {
        Report {
            url: "http://127.0.0.1:1/?icm_events=1".into(),
            product: "HeadlessChrome/154".into(),
            outcome,
            ready: Value::Null,
            warnings: vec![],
            errors: vec![],
            responses: vec![],
            screenshot: None,
            preview: None,
            blankness: Some(Blankness {
                fraction: 0.4,
                color: [255, 255, 255],
            }),
            console: PathBuf::from("/tmp/console.ndjson"),
            chrome_log: PathBuf::from("/tmp/chrome.log"),
        }
    }

    fn statuses(checks: &[Check]) -> Vec<(String, crate::error::Status)> {
        checks
            .iter()
            .map(|c| (c.id().to_string(), c.status))
            .collect()
    }

    #[test]
    fn queries_are_added_to_urls() {
        assert_eq!(
            url_with_query("https://a.example/"),
            "https://a.example/?icm_events=1"
        );
        assert_eq!(
            url_with_query("https://a.example/app/?x=1#top"),
            "https://a.example/app/?x=1&icm_events=1#top"
        );
    }

    #[test]
    fn a_ready_page_with_its_wasm_typed_passes() {
        use crate::error::Status;
        let mut ready = report(Outcome::Ready {
            source: "icm_event",
            ms: Some(900),
        });
        ready.responses = vec![
            json!({"url": "http://h/pkg/app-0123abcd.js", "status": 200, "mime": "text/javascript"}),
            json!({"url": "http://h/pkg/app_bg-0123abcd.wasm?v=1", "status": 200, "mime": "application/wasm"}),
        ];
        let local = checks(&ready, false);
        assert_eq!(
            statuses(&local),
            [
                ("web.serve_smoke".to_string(), Status::Pass),
                ("web.mime".to_string(), Status::Pass)
            ]
        );
        assert_eq!(ready.to_json()["wasm"][0]["mime"], "application/wasm");

        // A wrong type on the host fails web.mime with the host's fix.
        ready.responses[1]["mime"] = json!("application/octet-stream");
        let deployed = checks(&ready, true);
        assert_eq!(deployed[1].status, Status::Fail);
        assert!(deployed[1].error.fix.summary.contains("_headers"));

        // Console errors and a blank screen fail the serve check.
        ready.errors = vec![json!({"msg": "boom", "level": "error"})];
        assert!(serve_check(&ready).error.detail.contains("the first: boom"));
        ready.errors.clear();
        ready.blankness = Some(Blankness {
            fraction: 1.0,
            color: [0, 0, 0],
        });
        let blank = serve_check(&ready);
        assert_eq!(blank.status, Status::Fail);
        assert!(
            blank.error.detail.contains("blank"),
            "{}",
            blank.error.detail
        );
    }

    #[test]
    fn pages_that_never_draw_fail() {
        use crate::error::Status;
        let panicked = report(Outcome::Panicked(
            json!({"message": "oops", "location": "src/lib.rs:3:1"}),
        ));
        let checks = checks(&panicked, false);
        assert_eq!(checks.len(), 1, "no web.mime verdict without a load");
        assert_eq!(checks[0].status, Status::Fail);
        assert!(checks[0].error.detail.ends_with("at src/lib.rs:3:1: oops"));
        let timed_out = serve_check(&report(Outcome::TimedOut { started: false }));
        assert!(timed_out.error.detail.contains("neither started nor drew"));
        let failed = serve_check(&report(Outcome::LoadFailed(
            "net::ERR_NAME_NOT_RESOLVED".into(),
        )));
        assert!(failed.error.detail.contains("ERR_NAME_NOT_RESOLVED"));
    }

    #[test]
    fn sites_under_a_path_are_staged_there() {
        let site = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(site.path().join("pkg")).unwrap();
        std::fs::write(
            site.path().join("index.html"),
            "<base href=\"https://cdn.example/app/\">",
        )
        .unwrap();
        std::fs::write(site.path().join("pkg/a.js"), "x").unwrap();
        let work = tempfile::tempdir().unwrap();

        match stage(site.path(), "/", work.path()).unwrap() {
            Target::Site { root, path } => {
                assert_eq!(root, site.path());
                assert_eq!(path, "/");
            }
            Target::Url(_) => panic!("a site"),
        }
        match stage(site.path(), "https://cdn.example/app", work.path()).unwrap() {
            Target::Site { root, path } => {
                assert_eq!(path, "/app/");
                assert!(root.join("app/pkg/a.js").is_file());
                assert_eq!(
                    std::fs::read_to_string(root.join("app/index.html")).unwrap(),
                    "<base href=\"/app/\">"
                );
            }
            Target::Url(_) => panic!("a site"),
        }
    }
}
