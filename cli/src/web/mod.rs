//! The web platform (design §9.5, §10.2; Appendix C items 13 and 28).
//!
//! `icm run web`:
//! 1. gates: the wasm32 target on the project's toolchain, a wasm-bindgen
//!    CLI equal to the app's `Cargo.lock` version (`deps.wasm_bindgen_cli`),
//!    Chrome;
//! 2. `cargo build --target wasm32-unknown-unknown` (`[web] rustflags` in
//!    `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS`); a getrandom
//!    backend failure becomes `deps.getrandom_backend`;
//! 3. `wasm-bindgen --target web --no-typescript --out-name app` into
//!    `target/icm/build/web/<profile>/site/pkg`;
//! 4. the site: `index.html`, `manifest.webmanifest`, the icon and
//!    `[app] resources` ([`site`]); iced's features for wasm32 are checked
//!    (`web.fonts_embedded`: `fira-sans`, the only font a web build has;
//!    `web.renderer_fallback`: `webgl`, since headless Chrome exposes
//!    WebGPU without an adapter and wgpu then needs WebGL2);
//! 5. the session ([`host`]): replaces this project's running web session
//!    (Appendix C item 13), serves the site on `127.0.0.1:<port>`, drives
//!    headless Chrome over the DevTools pipe and records the console;
//! 6. ready: `ICM_EVENT ready` on the console (the page loads with
//!    `?icm_events=1`), or a drawn canvas when the app never announces
//!    itself (`source: probe`); a panic, an exception or a crash first is
//!    `run.app_panicked` / `run.app_died`, nothing is `run.not_ready`;
//! 7. the screenshot (`Page.captureScreenshot`), its preview and blank
//!    check, a copy of the console in the run directory; then `run`
//!    returns and the session keeps serving.
//!
//! `icm shot web`, `icm input web`, `icm logs web` and `icm stop web` talk
//! to the session; see [`client`].

pub mod cdp;
pub mod client;
pub mod console;
pub mod host;
pub mod server;
pub mod site;
pub mod viewport;

use crate::cargo::{Invocation, Select};
use crate::catalogue::CheckId;
use crate::cli::{BuildArgs, InputAction, InputArgs, Key, LogsArgs, RunArgs, ShotArgs, Theme};
use crate::context::{Ctx, Project};
use crate::error::{Check, Evidence, IcmError, Result};
use crate::process::Cmd;
use crate::screen::Space;
use crate::tools::{self, Found};
use client::Session;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use viewport::Viewport;

/// The platform name.
pub const PLATFORM: &str = "web";

/// The Rust target.
pub const TRIPLE: &str = "wasm32-unknown-unknown";

/// How long the session host may take to start Chrome and load the page.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a run without `ICM_EVENT start` waits before it accepts a drawn
/// canvas as ready.
const PROBE_AFTER: Duration = Duration::from_secs(5);

/// A finished web build.
#[derive(Clone, Debug)]
pub struct Built {
    /// `target/icm/build/web/<profile>/site`.
    pub site: PathBuf,
    /// The wasm-bindgen output.
    pub wasm: PathBuf,
    /// The wasm-bindgen CLI used.
    pub wasm_bindgen: Option<Found>,
    /// `rustc --version` of the project's toolchain.
    pub rustc: Option<String>,
}

fn profile_name(release: bool) -> &'static str {
    if release { "release" } else { "debug" }
}

fn bad_args(detail: impl Into<String>) -> IcmError {
    IcmError::new(CheckId::UsageBadArgs, detail).fix("Read `icm run --help`.", &["icm run --help"])
}

fn internal(detail: impl Into<String>) -> IcmError {
    IcmError::new(CheckId::InternalBug, detail)
}

/// `--env K=V` pairs as page query parameters (`RUST_LOG=debug` →
/// `rust_log=debug`, which iced's web logger reads).
pub fn env_query(env: &[String]) -> Result<Vec<(String, String)>> {
    env.iter()
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) if !key.is_empty() => {
                Ok((key.to_ascii_lowercase(), value.to_string()))
            }
            _ => Err(bad_args(format!("--env `{pair}` is not K=V"))),
        })
        .collect()
}

// ---- build ----------------------------------------------------------------------------

/// The wasm-bindgen version the app's lock pins.
fn lock_wasm_bindgen(project: &Project) -> Result<Option<String>> {
    Ok(project
        .lock()?
        .and_then(|lock| lock.version_of("wasm-bindgen").map(str::to_string)))
}

fn wasm_bindgen_cli(ctx: &Ctx, project: &Project) -> Result<Option<Found>> {
    let Some(version) = lock_wasm_bindgen(project)? else {
        return Ok(None);
    };
    let found = tools::wasm_bindgen(&version, &ctx.env)?;
    ctx.rep.check(Check::pass(
        CheckId::DepsWasmBindgenCli,
        format!(
            "wasm-bindgen {version} ({}, {})",
            crate::paths::display(&found.path),
            found.source
        ),
    ));
    Ok(Some(found))
}

/// Builds the site: cargo, wasm-bindgen, index.html and friends.
pub fn build(ctx: &mut Ctx, project: &Project, release: bool) -> Result<Built> {
    let profile = profile_name(release);
    let package = project.package_for(PLATFORM)?.clone();
    let bin = project.bin_for(PLATFORM)?;

    // The wasm32 target on the toolchain the project resolves (Appendix C 8).
    let toolchain = crate::toolchain::active(project.dir())?;
    let mut missing = None;
    for check in crate::toolchain::check_targets(&toolchain, &[TRIPLE.to_string()]) {
        if check.failed() && missing.is_none() {
            missing = Some(check.into_error());
        } else {
            ctx.rep.check(check);
        }
    }
    if let Some(error) = missing {
        return Err(error);
    }
    ctx.rep.set(
        "tools",
        json!({"rustc": toolchain.rustc_version(), "toolchain": toolchain.name}),
    );

    // The wasm-bindgen CLI before the long build, when the lock exists.
    let mut bindgen = if project.lock_path().exists() {
        wasm_bindgen_cli(ctx, project)?
    } else {
        None
    };

    let mut invocation = Invocation::new("build", &package.manifest_path, &package.name);
    invocation.select = Select::Bin(bin.clone());
    invocation.triple = Some(TRIPLE.to_string());
    invocation.profile = if release { "release" } else { "dev" }.to_string();
    let mut env = Vec::new();
    let rustflags = &project.config.config.web.rustflags;
    if !rustflags.is_empty() {
        env.push((
            "CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS".to_string(),
            rustflags.join(" "),
        ));
    }
    let output = ctx
        .cargo("cargo.build", &invocation, &env)
        .map_err(|error| getrandom_backend(ctx, error))?;

    let wasm = output
        .file_with_extension(&bin, "wasm")
        .map(Path::to_path_buf)
        .unwrap_or_else(|| {
            crate::cargo::artifacts_dir(
                &project.target_dir,
                Some(TRIPLE),
                invocation.profile.as_str(),
            )
            .join(format!("{bin}.wasm"))
        });
    if !wasm.is_file() {
        return Err(IcmError::new(
            CheckId::BuildCargoFailed,
            format!("cargo built no {}", crate::paths::display(&wasm)),
        ));
    }

    if bindgen.is_none() {
        bindgen = wasm_bindgen_cli(ctx, project)?;
    }
    let Some(bindgen) = bindgen else {
        return Err(IcmError::new(
            CheckId::DepsWasmBindgenCli,
            format!(
                "wasm-bindgen is not in {}; an iced web app depends on it through iced",
                crate::paths::display(&project.lock_path())
            ),
        )
        .evidence(Evidence::file(project.lock_path())));
    };

    let site = project.build_dir(PLATFORM, profile).join("site");
    let pkg = site.join("pkg");
    let _ = std::fs::remove_dir_all(&pkg);
    let mut cmd = Cmd::new(&bindgen.path)
        .arg(&wasm)
        .args([
            "--target",
            "web",
            "--no-typescript",
            "--out-name",
            site::OUT_NAME,
        ])
        .arg("--out-dir")
        .arg(&pkg)
        .timeout(Duration::from_secs(600));
    if !release {
        cmd = cmd.arg("--debug");
    }
    let outcome = ctx.step("wasm-bindgen", &cmd)?;
    if !outcome.success() {
        let text = outcome.stderr_text();
        let id = if text.contains("schema version") || text.contains("different bindgen format") {
            CheckId::DepsWasmBindgenCli
        } else {
            CheckId::ToolFailed
        };
        return Err(ctx.step_failure("wasm-bindgen", id, &outcome));
    }

    let started = Instant::now();
    let app = project.app();
    let icon = app
        .icon
        .as_deref()
        .map(|icon| project.dir().join(icon))
        .filter(|icon| icon.is_file());
    let inputs = site::Inputs {
        name: app.name.clone(),
        background: app.background.clone(),
        icon,
        project_dir: project.dir().to_path_buf(),
        resources: app.resources.clone(),
    };
    let written = site::write(&site, &inputs);
    ctx.rep.step_end_internal(
        "site.generate",
        written.is_ok(),
        started.elapsed().as_millis() as u64,
    );
    if let Err(detail) = written {
        return Err(IcmError::new(CheckId::ConfigInvalid, detail)
            .evidence(project.config.evidence("app.resources")));
    }

    feature_checks(ctx, &package.manifest_path);

    let app_wasm = pkg.join(format!("{}_bg.wasm", site::OUT_NAME));
    ctx.rep.artifact("site", &site);
    ctx.rep.artifact("wasm", &app_wasm);
    Ok(Built {
        site,
        wasm: app_wasm,
        wasm_bindgen: Some(bindgen),
        rustc: Some(toolchain.rustc_version().to_string()),
    })
}

/// Maps a wasm32 build failure caused by getrandom's missing web backend to
/// `deps.getrandom_backend`, with the lines to add.
fn getrandom_backend(ctx: &Ctx, error: IcmError) -> IcmError {
    if !matches!(
        error.check_id(),
        Some(CheckId::BuildCompileError | CheckId::BuildCargoFailed)
    ) {
        return error;
    }
    let diagnostics = ctx.rep.error_diagnostics();
    let text: String = diagnostics
        .iter()
        .map(|d| format!("{}\n{}\n", d.message, d.rendered))
        .chain(std::iter::once(error.detail.clone()))
        .collect();
    if !text.contains("getrandom") {
        return error;
    }
    let v03 = text.contains("wasm_js") || text.contains("getrandom_backend");
    let (detail, fix): (&str, Vec<String>) = if v03 {
        (
            "getrandom 0.3 is built for wasm32-unknown-unknown without its `wasm_js` backend",
            vec![
                "icm.toml [web]: rustflags = [\"--cfg\", 'getrandom_backend=\"wasm_js\"']".to_string(),
                "Cargo.toml [target.'cfg(target_arch = \"wasm32\")'.dependencies]: getrandom = { version = \"0.3\", features = [\"wasm_js\"] }".to_string(),
            ],
        )
    } else {
        (
            "getrandom 0.2 is built for wasm32-unknown-unknown without its `js` feature",
            vec!["Cargo.toml [target.'cfg(target_arch = \"wasm32\")'.dependencies]: getrandom = { version = \"0.2\", features = [\"js\"] }".to_string()],
        )
    };
    let mut mapped = IcmError::new(CheckId::DepsGetrandomBackend, detail)
        .fix(
            "Configure getrandom's web backend: add the lines below, then rerun.",
            &[],
        )
        .fix_commands(fix);
    mapped.evidence = error.evidence;
    mapped.diagnostics = diagnostics;
    mapped
}

/// iced's features as resolved for wasm32, from `cargo metadata
/// --filter-platform` (offline: the build just fetched everything).
fn iced_features(ctx: &Ctx, manifest: &Path) -> Option<Vec<String>> {
    let cmd = Cmd::tool("cargo")
        .args(["metadata", "--format-version", "1", "--offline"])
        .args(["--filter-platform", TRIPLE])
        .arg("--manifest-path")
        .arg(manifest)
        .timeout(Duration::from_secs(120));
    let outcome = ctx.probe(&cmd).ok()?;
    if !outcome.success() {
        return None;
    }
    let metadata: Value = serde_json::from_slice(&outcome.stdout).ok()?;
    let ids: Vec<&str> = metadata["packages"]
        .as_array()?
        .iter()
        .filter(|package| package["name"] == "iced")
        .filter_map(|package| package["id"].as_str())
        .collect();
    let nodes = metadata["resolve"]["nodes"].as_array()?;
    let mut features = Vec::new();
    for node in nodes {
        if node["id"].as_str().is_some_and(|id| ids.contains(&id)) {
            features.extend(
                node["features"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string),
            );
        }
    }
    (!ids.is_empty()).then_some(features)
}

/// `web.fonts_embedded` and `web.renderer_fallback` for a dev build (WARN;
/// a release makes the first a FAIL).
fn feature_checks(ctx: &Ctx, manifest: &Path) {
    let Some(features) = iced_features(ctx, manifest) else {
        ctx.rep.check(Check::skip(
            CheckId::WebFontsEmbedded,
            "cannot resolve iced's features for wasm32 (cargo metadata --offline failed)",
        ));
        return;
    };
    let has = |name: &str| features.iter().any(|f| f == name);
    let fix_line = "Cargo.toml [target.'cfg(target_arch = \"wasm32\")'.dependencies]: iced = { …, features = [\"fira-sans\", \"webgl\"] }";

    if has("fira-sans") {
        ctx.rep.check(Check::pass(
            CheckId::WebFontsEmbedded,
            "iced's `fira-sans` is on for wasm32: the app carries its font",
        ));
    } else {
        ctx.rep.check(
            Check::warn(
                CheckId::WebFontsEmbedded,
                "iced's `fira-sans` is off for wasm32; the web has no system fonts, so text draws as nothing unless the app loads a font itself",
            )
            .fix("Enable iced's `fira-sans` feature for wasm32.", &[fix_line]),
        );
    }

    if has("webgl") || !has("wgpu") {
        ctx.rep.check(Check::pass(
            CheckId::WebRendererFallback,
            if has("webgl") {
                "iced's `webgl` is on: wgpu falls back to WebGL2 when the browser offers no WebGPU adapter (headless Chrome offers none)"
            } else {
                "iced draws with tiny-skia on the web"
            },
        ));
    } else {
        // Verified in headless Chrome 154: wgpu takes the canvas for
        // WebGPU, finds no adapter, and iced's tiny-skia fallback then
        // panics creating its softbuffer surface on that canvas.
        let detail = "iced's `webgl` is off for wasm32: where the browser has no WebGPU adapter (headless Chrome, many browsers) wgpu cannot draw, and iced's tiny-skia fallback panics on the canvas wgpu already took";
        ctx.rep.check(
            Check::warn(CheckId::WebRendererFallback, detail)
                .fix("Enable iced's `webgl` feature for wasm32.", &[fix_line]),
        );
    }
}

// ---- the session ----------------------------------------------------------------------

/// A started session.
struct Started {
    session: Session,
    url: String,
    product: String,
}

fn session_dir(project: &Project) -> PathBuf {
    project.sessions_dir().join(PLATFORM)
}

/// Starts the session host for `site`, replacing this project's running
/// web session (Appendix C item 13).
fn start_session(
    ctx: &Ctx,
    project: &Project,
    site: &Path,
    viewport: &Viewport,
    port: u16,
    query: Vec<(String, String)>,
    chrome: &Found,
    profile: &str,
) -> Result<Started> {
    let sessions_dir = project.sessions_dir();
    if let Some((record, stopped)) = client::stop(&sessions_dir)
        && stopped != crate::sessions::Stopped::NotRunning
    {
        ctx.rep.progress(format!(
            "replaced the running web session (pid {})",
            crate::sessions::record_pid(&record).unwrap_or(0)
        ));
    }

    let dir = session_dir(project);
    std::fs::create_dir_all(&dir)
        .map_err(|error| internal(format!("cannot create {}: {error}", dir.display())))?;
    let files = host::Files::new(&dir);
    let _ = std::fs::remove_file(&files.startup);
    let _ = std::fs::write(&files.host_log, b"");

    let request = host::Request {
        project_dir: project.dir().to_path_buf(),
        sessions_dir: sessions_dir.clone(),
        session_dir: dir.clone(),
        site: site.to_path_buf(),
        port,
        viewport: viewport.to_json(),
        chrome: chrome.path.clone(),
        run: ctx.rep.run_id(),
        run_dir: ctx.rep.run_dir(),
        query,
        app: project.app_json(),
        profile: profile.to_string(),
    };
    std::fs::write(
        &files.request,
        serde_json::to_vec_pretty(&request).unwrap_or_default(),
    )
    .map_err(|error| internal(format!("cannot write {}: {error}", files.request.display())))?;

    let exe = std::env::current_exe()
        .map_err(|error| internal(format!("cannot find icm's own executable: {error}")))?;
    let mut cmd = Cmd::new(&exe)
        .args(["__session", "web", "--request"])
        .arg(&files.request)
        .keep_locale();
    for var in [
        "ICM_RUN_ID",
        "ICM_RUN_DIR",
        "ICM_RUN_ROOT",
        "ICM_DETACHED",
        "ICM_JSON",
        "ICM_TIMEOUT",
        "ICM_CONFIG",
    ] {
        cmd = cmd.env_remove(var);
    }
    ctx.rep.step_begin(
        "session.start",
        &cmd.display_argv(),
        &cmd.display_env(),
        cmd.cwd.as_deref(),
    );
    let started = Instant::now();
    let pid = crate::process::spawn_detached(&cmd, &files.host_log, &files.host_log)
        .map_err(|error| internal(format!("cannot start the web session: {error}")))?
        as i32;

    let deadline = started + STARTUP_TIMEOUT;
    let startup: Value = loop {
        if let Ok(text) = std::fs::read_to_string(&files.startup)
            && let Ok(value) = serde_json::from_str::<Value>(&text)
        {
            break value;
        }
        if crate::sessions::reap(pid) || !crate::signals::alive(pid) {
            // It may have written the handshake just before exiting.
            if let Ok(text) = std::fs::read_to_string(&files.startup)
                && let Ok(value) = serde_json::from_str::<Value>(&text)
            {
                break value;
            }
            ctx.rep
                .step_end_internal("session.start", false, started.elapsed().as_millis() as u64);
            return Err(IcmError::new(
                CheckId::WebChromeFailed,
                format!(
                    "the web session (pid {pid}) exited during startup{}",
                    log_tail(&files.host_log)
                ),
            )
            .evidence(Evidence::file(&files.host_log))
            .evidence(Evidence::file(&files.chrome_log)));
        }
        if let Some(signal) = crate::signals::pending() {
            crate::signals::kill_group(pid, libc::SIGTERM);
            return Err(crate::output::interrupted(signal));
        }
        if Instant::now() >= deadline || ctx.remaining().is_some_and(|r| r.is_zero()) {
            crate::signals::kill_group(pid, libc::SIGKILL);
            ctx.rep
                .step_end_internal("session.start", false, started.elapsed().as_millis() as u64);
            return Err(IcmError::new(
                CheckId::StepTimeout,
                format!(
                    "the web session did not start within {}",
                    crate::time::format_duration(STARTUP_TIMEOUT)
                ),
            )
            .evidence(Evidence::file(&files.host_log))
            .evidence(Evidence::file(&files.chrome_log)));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let ok = startup.get("ok").and_then(Value::as_bool).unwrap_or(false);
    ctx.rep
        .step_end_internal("session.start", ok, started.elapsed().as_millis() as u64);

    if !ok {
        let id = startup
            .get("id")
            .and_then(Value::as_str)
            .and_then(CheckId::from_id)
            .unwrap_or(CheckId::WebChromeFailed);
        let detail = startup
            .get("detail")
            .and_then(Value::as_str)
            .unwrap_or("the web session failed to start")
            .to_string();
        let mut error = IcmError::new(id, detail).evidence(Evidence::file(&files.host_log));
        if id == CheckId::WebPortBusy {
            error = error.fix(
                "Pass another port, or --port 0 for any free one; or stop what holds the port.",
                &["icm run web --port 0 --json -q"],
            );
        } else {
            error = error.evidence(Evidence::file(&files.chrome_log));
        }
        return Err(error);
    }

    let session = Session::find(&sessions_dir)?;
    Ok(Started {
        url: startup
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        product: startup
            .get("product")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        session,
    })
}

fn log_tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(4)..].join("\n");
    if tail.is_empty() {
        String::new()
    } else {
        format!(":\n{tail}")
    }
}

/// How the app became ready.
struct Ready {
    source: &'static str,
    ms: Option<u64>,
    event: Value,
}

fn first_error_record(console: Option<&Path>) -> Option<Value> {
    console
        .map(console::read)
        .unwrap_or_default()
        .into_iter()
        .find(|record| record["level"] == "error" && record["tag"] != console::ICM_EVENT_TAG)
}

fn panic_error(panic: &Value, console: Option<&Path>) -> IcmError {
    let message = panic["message"].as_str().unwrap_or("panic");
    let location = panic["location"].as_str();
    let detail = match location {
        Some(location) => format!("panicked at {location}: {message}"),
        None => format!("panicked: {message}"),
    };
    let mut error = IcmError::new(CheckId::RunAppPanicked, detail).fix(
        "Fix the panic at the location in the detail, then rerun.",
        &["icm run web --json -q"],
    );
    if message.contains("canvas context other than") {
        error = error.cause(
            "no WebGL fallback: wgpu took the canvas for WebGPU, found no adapter, and tiny-skia could not use the canvas; enable iced's `webgl` feature for wasm32 (web.renderer_fallback)",
        );
    } else if let Some(location) = location {
        error = error.cause(format!("a bug at {location}"));
    }
    if let Some(console) = console {
        error = error
            .evidence(Evidence::file(console).with_excerpt(format!("ICM_EVENT panic: {message}")));
    }
    error
}

/// Waits for `ICM_EVENT ready` (or the canvas probe), failing on a panic,
/// a crash, or a page that died first.
fn wait_ready(ctx: &Ctx, session: &Session, wait: Duration) -> Result<Ready> {
    let started = Instant::now();
    let limit = ctx.remaining().map_or(wait, |r| r.min(wait));
    let console = session.console();
    let mut probe_hits = 0;

    loop {
        let probe_due = started.elapsed() >= PROBE_AFTER;
        let status = session.status(probe_due)?;

        if let Some(panic) = status["panics"].as_array().and_then(|p| p.first()) {
            return Err(panic_error(panic, console.as_deref()));
        }
        if !status["ready"].is_null() {
            let event = status["ready"].clone();
            let navigated = status["navigated_ms"].as_u64().unwrap_or(0);
            let received = event["epoch_ms"].as_u64().unwrap_or(navigated);
            return Ok(Ready {
                source: "icm_event",
                ms: Some(received.saturating_sub(navigated)),
                event,
            });
        }
        if status["crashed"].as_bool() == Some(true) {
            let mut error = IcmError::new(
                CheckId::RunAppDied,
                "the page's renderer crashed before the first frame",
            );
            if let Some(console) = &console {
                error = error.evidence(Evidence::file(console));
            }
            return Err(error);
        }
        if status["chrome"]["alive"].as_bool() == Some(false) {
            return Err(IcmError::new(
                CheckId::WebChromeFailed,
                "headless Chrome exited before the app drew",
            ));
        }
        // An app that never announces itself (older framework, no
        // events): a canvas with a size is the probe.
        if status["start"].is_null()
            && status["canvas"]
                .as_array()
                .is_some_and(|size| size.iter().all(|v| v.as_u64().unwrap_or(0) > 0))
        {
            probe_hits += 1;
            if probe_hits >= 2 {
                return Ok(Ready {
                    source: "probe",
                    ms: None,
                    event: Value::Null,
                });
            }
        }

        if started.elapsed() >= limit {
            let error_record = first_error_record(console.as_deref());
            let mut error = match (&error_record, status["start"].is_null()) {
                (Some(record), _) => IcmError::new(
                    CheckId::RunAppDied,
                    format!(
                        "the page failed before the first frame: {}",
                        record["msg"].as_str().unwrap_or("an error")
                    ),
                ),
                (None, true) => IcmError::new(
                    CheckId::RunNotReady,
                    format!(
                        "the app neither started nor drew within {} (no ICM_EVENT start and no canvas)",
                        crate::time::format_duration(limit)
                    ),
                ),
                (None, false) => IcmError::new(
                    CheckId::RunNotReady,
                    format!(
                        "the app started but drew no first frame within {}",
                        crate::time::format_duration(limit)
                    ),
                ),
            };
            if let Some(console) = &console {
                let mut evidence = Evidence::file(console);
                if let Some(record) = &error_record {
                    evidence = evidence.with_excerpt(record["msg"].as_str().unwrap_or(""));
                }
                error = error.evidence(evidence);
            }
            if status["exceptions"].as_array().is_some_and(|e| {
                e.iter()
                    .any(|r| r["msg"].as_str().unwrap_or("").contains("adapter"))
            }) || first_error_record(console.as_deref())
                .is_some_and(|r| r["msg"].as_str().unwrap_or("").contains("adapter"))
            {
                error = error.cause(
                    "no GPU adapter: enable iced's `webgl` feature for wasm32 (headless Chrome has no WebGPU adapter)",
                );
            }
            return Err(error.fix(
                "Read the console, fix the app, rerun; raise --wait-ready if it is legitimately slow.",
                &["icm logs web --level warn --json -q"],
            ));
        }
        if let Some(signal) = crate::signals::pending() {
            return Err(crate::output::interrupted(signal));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Copies the session's console into the run directory as `console.ndjson`,
/// `logs.ndjson` and the readable `app.log`; returns the copy.
fn snapshot_console(ctx: &Ctx, session: &Session) -> Option<PathBuf> {
    let (Some(run_dir), Some(console_path)) = (ctx.rep.run_dir(), session.console()) else {
        return None;
    };
    let records = console::read(&console_path);
    let ndjson: String = records
        .iter()
        .map(|r| format!("{}\n", serde_json::to_string(r).unwrap_or_default()))
        .collect();
    let log: String = records
        .iter()
        .map(|r| format!("{}\n", console::line(r)))
        .collect();
    let _ = std::fs::write(run_dir.join("console.ndjson"), &ndjson);
    let _ = std::fs::write(run_dir.join("logs.ndjson"), &ndjson);
    let _ = std::fs::write(run_dir.join("app.log"), &log);
    ctx.rep.artifact("logs", &run_dir.join("logs.ndjson"));
    ctx.rep.artifact("app_log", &run_dir.join("app.log"));
    ctx.rep.artifact("console", &console_path);
    Some(run_dir.join("console.ndjson"))
}

/// A run that failed after the session started: the console is copied
/// into the run directory (the evidence then points at the copy, which
/// later sessions do not overwrite), and the session keeps running for
/// `icm logs web` and `icm shot web` until `icm stop web`.
fn fail_run(ctx: &Ctx, session: &Session, mut error: IcmError) -> IcmError {
    if let (Some(copy), Some(live)) = (snapshot_console(ctx, session), session.console()) {
        let live = crate::paths::display(&live);
        for evidence in &mut error.evidence {
            if evidence.path == live {
                evidence.path = crate::paths::display(&copy);
            }
        }
    }
    let page_alive = !matches!(
        error.check_id(),
        Some(CheckId::RunAppPanicked | CheckId::RunAppDied | CheckId::WebChromeFailed)
    );
    ctx.rep.set(
        "process",
        json!({"pid": session.pid(), "alive": page_alive && crate::sessions::alive(&session.record), "ready": {"source": "none", "ms": null}}),
    );
    ctx.rep.next(
        "icm logs web --level warn --json -q",
        "read the page's console",
    );
    ctx.rep.next("icm stop web", "end the web session");
    error
}

fn sleep_checking_signals(duration: Duration) -> Result<()> {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        if let Some(signal) = crate::signals::pending() {
            return Err(crate::output::interrupted(signal));
        }
        std::thread::sleep(Duration::from_millis(50).min(until - Instant::now()));
    }
    Ok(())
}

/// Opens the system browser at the page (`--show`).
fn show(ctx: &Ctx, session: &Session) {
    let url = session.url();
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let cmd = Cmd::tool(opener).arg(&url).timeout(Duration::from_secs(15));
    match ctx.step("browser.open", &cmd) {
        Ok(outcome) if outcome.success() => {
            ctx.rep
                .progress(format!("opened {url} in the system browser"));
        }
        _ => ctx
            .rep
            .progress(format!("could not open the system browser; visit {url}")),
    }
}

/// `icm run web`.
pub fn run(ctx: &mut Ctx, args: &RunArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    ctx.rep.latest(PLATFORM);
    let profile = profile_name(args.release);
    ctx.rep.set("profile", json!(profile));
    let viewport =
        Viewport::parse(args.viewport.as_deref().unwrap_or(viewport::DEFAULT)).map_err(bad_args)?;
    let query = env_query(&args.env)?;
    let host = ctx.host()?.clone();
    let chrome = tools::chrome(&host, &ctx.env)?;
    let _lock = ctx.lock_platform(PLATFORM)?;

    let site = if args.no_build {
        let site = project.build_dir(PLATFORM, profile).join("site");
        if !site
            .join("pkg")
            .join(format!("{}_bg.wasm", site::OUT_NAME))
            .is_file()
        {
            return Err(bad_args(format!(
                "--no-build: there is no {profile} web build at {}; run without --no-build",
                crate::paths::display(&site)
            )));
        }
        site
    } else {
        let built = build(ctx, &project, args.release)?;
        let mut tools_json = json!({
            "rustc": built.rustc,
            "wasm_bindgen": built.wasm_bindgen.as_ref().and_then(|f| f.version.clone()),
        });
        tools_json["chrome"] = json!(crate::paths::display(&chrome.path));
        ctx.rep.set("tools", tools_json);
        built.site
    };

    let started = start_session(
        ctx, &project, &site, &viewport, args.port, query, &chrome, profile,
    )?;
    let session = &started.session;
    let session_path = crate::paths::display(&session.path);
    ctx.rep.set("session", json!(session_path));
    ctx.rep.set(
        "device",
        json!({"kind": "browser", "name": started.product, "viewport": viewport.to_json()}),
    );
    let url = started.url.replace(site::HEADLESS_QUERY, site::SHOW_QUERY);
    ctx.rep.artifact_value("url", &url);

    let ready = match wait_ready(ctx, session, args.wait_ready) {
        Ok(ready) => ready,
        Err(error) => return Err(fail_run(ctx, session, error)),
    };

    let window = &ready.event["window"];
    let detail = match (window["size"].as_array(), ready.ms) {
        (Some(size), Some(ms)) => format!(
            "first frame {}x{}@{} after {} (source: {})",
            size.first().and_then(Value::as_u64).unwrap_or(0),
            size.get(1).and_then(Value::as_u64).unwrap_or(0),
            window["scale"].as_f64().unwrap_or(1.0),
            crate::time::format_duration(Duration::from_millis(ms)),
            ready.source
        ),
        _ => format!("the page drew a canvas (source: {})", ready.source),
    };
    ctx.rep.check(Check::pass(CheckId::RunReady, detail));
    if !ready.event["backend"].is_null() {
        ctx.rep.set(
            "device",
            json!({
                "kind": "browser",
                "name": started.product,
                "viewport": viewport.to_json(),
                "renderer": {
                    "backend": ready.event["backend"],
                    "api": ready.event["api"],
                    "adapter": ready.event["adapter"],
                },
            }),
        );
    }
    ctx.rep.ready(json!({
        "url": url,
        "source": ready.source,
        "ms_since_launch": ready.ms,
        "window": {"size": window["size"], "scale": window["scale"]},
    }));

    sleep_checking_signals(args.settle)?;
    let status = session.status(false)?;
    if let Some(panic) = status["panics"].as_array().and_then(|p| p.first()) {
        let error = panic_error(panic, session.console().as_deref());
        return Err(fail_run(ctx, session, error));
    }
    for warning in status["warnings"].as_array().into_iter().flatten() {
        if warning["code"] == "font.default_missing" {
            ctx.rep.check(Check::warn(
                CheckId::RunFontMissing,
                warning["message"]
                    .as_str()
                    .unwrap_or("the default font is missing"),
            ));
        }
    }
    ctx.rep.check(Check::pass(
        CheckId::RunAlive,
        format!("the page is running in {}", started.product),
    ));
    ctx.rep.set(
        "process",
        json!({"pid": session.pid(), "alive": true, "ready": {"source": ready.source, "ms": ready.ms}}),
    );

    if !args.no_shot {
        capture(ctx, session, None, args.expect_content)?;
    }
    let _ = snapshot_console(ctx, session);

    if args.show {
        show(ctx, session);
    }

    // The project's `[checks] web` scripts (design §13.6); the page's URL
    // is `ICM_URL`.
    crate::hooks::run_for(
        ctx,
        &project,
        &crate::hooks::HookContext {
            platform: PLATFORM.to_string(),
            pid: u32::try_from(session.pid()).ok(),
            logs: session.console(),
            env: vec![("ICM_URL".to_string(), url.clone())],
            ..crate::hooks::HookContext::default()
        },
    )?;

    let errors = status["errors"].as_u64().unwrap_or(0);
    ctx.rep.summary(format!(
        "the web app is ready at {} (session pid {}){}",
        url,
        session.pid(),
        if errors > 0 {
            format!("; the console has {errors} error(s)")
        } else {
            String::new()
        }
    ));
    ctx.rep.next(
        "icm logs web --level warn --json -q",
        "read the page's console",
    );
    ctx.rep.next(
        "icm input web tap <x> <y> --json -q",
        "tap at screen.preview.png coordinates",
    );
    ctx.rep
        .next("icm shot web --json -q", "capture the page again");
    ctx.rep.next("icm stop web", "end the web session");

    if args.attach {
        attach(ctx, session)?;
    }
    Ok(())
}

/// `--attach`: streams the console until Ctrl-C, then ends the session.
fn attach(ctx: &Ctx, session: &Session) -> Result<()> {
    let Some(path) = session.console() else {
        return Ok(());
    };
    let mut seen = console::read(&path).len();
    ctx.rep
        .progress("streaming the console; Ctrl-C ends the session");
    loop {
        if crate::signals::pending().is_some() || !crate::sessions::alive(&session.record) {
            break;
        }
        let records = console::read(&path);
        for record in records.iter().skip(seen) {
            emit_log(ctx, record);
        }
        seen = records.len();
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = session.stop();
    Ok(())
}

/// Takes a screenshot into the run directory (or `out`) and reports it.
fn capture(ctx: &Ctx, session: &Session, out: Option<&Path>, expect_content: bool) -> Result<()> {
    let png = match out {
        Some(out) => std::path::absolute(out).unwrap_or_else(|_| out.to_path_buf()),
        None => ctx
            .rep
            .run_dir()
            .map(|dir| dir.join("screen.png"))
            .ok_or_else(|| internal("no run directory for the screenshot"))?,
    };
    let reply = session.screenshot(&png)?;
    let scale = Viewport::from_json(&reply["viewport"]).map_or(1.0, |v| v.scale);
    crate::preview::finish(
        &ctx.rep,
        &png,
        scale,
        expect_content,
        &["icm logs web --level warn --json -q", "icm shot --headless"],
    )
    .map_err(|detail| {
        IcmError::new(CheckId::WebChromeFailed, detail).evidence(Evidence::file(&png))
    })?;
    Ok(())
}

/// `icm build web`.
pub fn build_command(ctx: &mut Ctx, args: &BuildArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    ctx.rep.set("profile", json!(profile_name(args.release)));
    let _lock = ctx.lock_platform(PLATFORM)?;
    let built = build(ctx, &project, args.release)?;
    ctx.rep.set(
        "tools",
        json!({
            "rustc": built.rustc,
            "wasm_bindgen": built.wasm_bindgen.as_ref().and_then(|f| f.version.clone()),
        }),
    );
    ctx.rep.summary(format!(
        "built the web site at {}",
        crate::paths::display(&built.site)
    ));
    ctx.rep.next(
        "icm run web --no-build --json -q",
        "serve it and open it in headless Chrome",
    );
    Ok(())
}

/// `icm shot web`.
pub fn shot(ctx: &mut Ctx, args: &ShotArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    let session = Session::find(&project.sessions_dir())?;
    let out = match (&args.out, &args.name) {
        (Some(out), _) => Some(out.clone()),
        (None, Some(name)) => ctx.rep.run_dir().map(|dir| dir.join(format!("{name}.png"))),
        (None, None) => None,
    };
    capture(ctx, &session, out.as_deref(), false)?;
    ctx.rep
        .set("session", json!(crate::paths::display(&session.path)));
    ctx.rep
        .summary(format!("captured the web page at {}", session.url()));
    Ok(())
}

/// `icm input web`.
pub fn input(ctx: &mut Ctx, args: &InputArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    let session = Session::find(&project.sessions_dir())?;
    let viewport = session.viewport()?;
    let screen = viewport.screen();
    ctx.rep.set("screen", screen.to_json());

    let point = |x: f64, y: f64| -> Result<(f64, f64)> {
        if !screen.contains(x, y, args.space) {
            return Err(bad_args(format!(
                "({x}, {y}) in {} space is outside the screen ({})",
                space_name(args.space),
                screen.to_json()
            ))
            .fix(
                "Use coordinates inside screen.preview.png (or pass --space px|pt).",
                &[],
            ));
        }
        Ok(screen.to_pt(x, y, args.space))
    };

    let unsupported = |what: &str| -> IcmError {
        IcmError::new(
            CheckId::InputUnsupported,
            format!("{what} is not available on the web"),
        )
    };

    let (request, describe) = match &args.action {
        InputAction::Tap { x, y } => {
            let (cx, cy) = point(*x, *y)?;
            (
                json!({"op": "tap", "x": cx, "y": cy}),
                json!({"action": "tap", "css": [cx, cy]}),
            )
        }
        InputAction::Swipe { x1, y1, x2, y2, ms } => {
            let (ax, ay) = point(*x1, *y1)?;
            let (bx, by) = point(*x2, *y2)?;
            let ms = ms.unwrap_or(300);
            (
                json!({"op": "swipe", "x1": ax, "y1": ay, "x2": bx, "y2": by, "ms": ms}),
                json!({"action": "swipe", "css": [[ax, ay], [bx, by]], "ms": ms}),
            )
        }
        InputAction::Text { text } => (
            json!({"op": "text", "text": text}),
            json!({"action": "text", "chars": text.chars().count()}),
        ),
        InputAction::Key { key } => {
            let name = match key {
                Key::Enter => "enter",
                Key::Tab => "tab",
                Key::Escape => "escape",
                Key::Back => return Err(unsupported("`key back` (use `key escape`)")),
                Key::Home => return Err(unsupported("`key home`")),
            };
            (
                json!({"op": "key", "key": name}),
                json!({"action": "key", "key": name}),
            )
        }
        InputAction::Appearance { mode } => {
            let mode = match mode {
                Theme::Light => "light",
                Theme::Dark => "dark",
            };
            (
                json!({"op": "appearance", "mode": mode}),
                json!({"action": "appearance", "mode": mode}),
            )
        }
        InputAction::Rotate { orientation } => {
            let name = match orientation {
                crate::cli::Rotation::Portrait => "portrait",
                crate::cli::Rotation::Landscape => "landscape",
            };
            (
                json!({"op": "rotate", "orientation": name}),
                json!({"action": "rotate", "orientation": name}),
            )
        }
        InputAction::FontScale { .. } => return Err(unsupported("`font-scale`")),
        InputAction::Background => return Err(unsupported("`background`")),
        InputAction::Foreground => return Err(unsupported("`foreground`")),
    };

    let reply = session.call(request, Duration::from_secs(60))?;
    let mut describe = describe;
    if let Some(pointer) = reply.get("pointer") {
        describe["pointer"] = pointer.clone();
    }
    if let Some(viewport) = reply.get("viewport").and_then(Viewport::from_json) {
        ctx.rep.set("screen", viewport.screen().to_json());
    }
    ctx.rep.set("input", describe.clone());
    ctx.rep
        .set("session", json!(crate::paths::display(&session.path)));
    ctx.rep.summary(format!(
        "sent {} to the web page",
        describe["action"].as_str().unwrap_or("input")
    ));
    ctx.rep.next("icm shot web --json -q", "see the result");
    Ok(())
}

fn space_name(space: Space) -> &'static str {
    match space {
        Space::Preview => "preview",
        Space::Px => "px",
        Space::Pt => "pt",
    }
}

fn emit_log(ctx: &Ctx, record: &Value) {
    let mut event = record.clone();
    event["type"] = json!("log");
    ctx.rep.emit(event);
}

/// `icm logs web`: re-reads the session's console file.
pub fn logs(ctx: &mut Ctx, args: &LogsArgs) -> Result<()> {
    let project = ctx.project()?.clone();
    let sessions_dir = project.sessions_dir();
    let live = Session::find(&sessions_dir);
    let path = match &live {
        Ok(session) => session.console(),
        Err(_) => None,
    }
    .or_else(|| {
        let fallback = host::Files::new(&session_dir(&project)).console;
        fallback.is_file().then_some(fallback)
    });
    let Some(path) = path else {
        return Err(live
            .err()
            .unwrap_or_else(|| client::no_session("no web session has run for this project")));
    };

    if args.raw {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        let tail = lines[lines.len().saturating_sub(args.tail)..].join("\n");
        ctx.rep.content(&tail);
        ctx.rep.artifact("console", &path);
        return Ok(());
    }

    let filter = console::Filter {
        since_ms: console::since(&args.since, console::now_ms()).map_err(bad_args)?,
        level: args.level,
        source: Some(args.source),
        grep: args.grep.clone(),
    };
    let all = console::read(&path);
    let kept: Vec<&Value> = all.iter().filter(|r| filter.keeps(r)).collect();
    let tail: Vec<Value> = kept[kept.len().saturating_sub(args.tail)..]
        .iter()
        .map(|r| (*r).clone())
        .collect();
    for record in &tail {
        emit_log(ctx, record);
    }

    if let Some(run_dir) = ctx.rep.run_dir() {
        let ndjson: String = tail
            .iter()
            .map(|r| format!("{}\n", serde_json::to_string(r).unwrap_or_default()))
            .collect();
        let log: String = tail
            .iter()
            .map(|r| format!("{}\n", console::line(r)))
            .collect();
        let _ = std::fs::write(run_dir.join("logs.ndjson"), ndjson);
        let _ = std::fs::write(run_dir.join("app.log"), log);
        ctx.rep.artifact("logs", &run_dir.join("logs.ndjson"));
        ctx.rep.artifact("app_log", &run_dir.join("app.log"));
    }
    ctx.rep.artifact("console", &path);
    ctx.rep.set("records", json!(tail));
    ctx.rep.set(
        "logs",
        json!({"total": all.len(), "matched": kept.len(), "shown": tail.len(), "live": live.is_ok()}),
    );
    ctx.rep.summary(format!(
        "{} of {} console record(s){}",
        tail.len(),
        all.len(),
        if live.is_ok() {
            ""
        } else {
            " (the session has ended)"
        }
    ));

    if args.follow {
        let mut seen = all.len();
        while crate::signals::pending().is_none() {
            std::thread::sleep(Duration::from_millis(250));
            let records = console::read(&path);
            for record in records.iter().skip(seen).filter(|r| filter.keeps(r)) {
                emit_log(ctx, record);
            }
            seen = records.len();
            if let Ok(session) = &live
                && !crate::sessions::alive(&session.record)
            {
                break;
            }
        }
    }
    Ok(())
}

/// Stops this project's web session for `icm stop`; returns a JSON entry
/// for the result, or `None` when none was running.
pub fn stop(project: &Project) -> Option<Value> {
    let (record, stopped) = client::stop(&project.sessions_dir())?;
    Some(json!({
        "platform": PLATFORM,
        "pid": crate::sessions::record_pid(&record),
        "url": record.get("url"),
        "stopped": match stopped {
            crate::sessions::Stopped::NotRunning => "was not running",
            crate::sessions::Stopped::Terminated => "terminated",
            crate::sessions::Stopped::Killed => "killed",
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_pairs_become_query_parameters() {
        let query = env_query(&["RUST_LOG=debug".into(), "A=b=c".into()]).unwrap();
        assert_eq!(
            query,
            vec![
                ("rust_log".to_string(), "debug".to_string()),
                ("a".to_string(), "b=c".to_string())
            ]
        );
        assert!(env_query(&["nope".into()]).is_err());
    }

    #[test]
    fn panics_name_their_location() {
        let error = panic_error(
            &json!({"message": "index out of bounds", "location": "src/lib.rs:41:9"}),
            None,
        );
        assert_eq!(error.id, "run.app_panicked");
        assert_eq!(error.exit, crate::exit::Exit::AppDied);
        assert_eq!(
            error.detail,
            "panicked at src/lib.rs:41:9: index out of bounds"
        );
        assert_eq!(error.likely_causes, ["a bug at src/lib.rs:41:9"]);
    }
}
