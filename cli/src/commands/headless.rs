//! `icm shot --headless` and `icm ui --headless tree|find|ice`: the real
//! view, rendered or inspected through the app's harness with no device,
//! window, GPU or OS permission (design §13.2).
//!
//! - `shot --headless [--viewport <preset|WxH[@scale]>]... [--all-viewports]
//!   [--theme light|dark] [--preset <name>] [--wait <dur>] [--out-dir <dir>]`
//!   writes `target/icm/host/shots/<viewport>-<theme>[-<preset>].png` and a
//!   `.preview.png` next to each (long edge at most 1024 px), and warns
//!   `run.screen_blank` for a single-colour render.
//! - `ui --headless tree [--viewport V]` lists every widget a selector can
//!   see, with its kind, id, text, bounds and on-screen rectangle in logical
//!   pixels; `find <selector>` keeps those matching `#id` or a text;
//!   `ice <file>` runs one `.ice` flow and reports each instruction.
//!
//! In human mode `ui` prints its answer (the tree, the matches, the steps)
//! on stdout, like `print`; with `--json` the answer is in the result.

use crate::catalogue::CheckId;
use crate::cli::{ShotArgs, Theme, UiAction, UiArgs};
use crate::context::{Ctx, Project};
use crate::error::{Check, Evidence, IcmError, Result};
use crate::harness::{self, Harness, Viewport};
use crate::raster;
use crate::screen::Screen;
use crate::signatures::{self, Facts};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long the harness lets the app's boot tasks run before a tree.
const TREE_WAIT_MS: u64 = 500;

/// The default `.ice` timeout (the harness's own default).
const ICE_TIMEOUT: Duration = Duration::from_secs(30);

fn usage(detail: impl Into<String>) -> IcmError {
    IcmError::new(CheckId::UsageBadArgs, detail)
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The viewports `[test] viewports` lists (already validated).
fn configured(project: &Project) -> Vec<Viewport> {
    project
        .config
        .config
        .test
        .viewports
        .iter()
        .filter_map(|text| Viewport::parse(text).ok())
        .collect()
}

/// The viewport used when none is given: the first of `[test] viewports`,
/// else `iphone-17`.
fn default_viewport(project: &Project) -> Viewport {
    configured(project)
        .into_iter()
        .next()
        .unwrap_or_else(|| Viewport::parse("iphone-17").expect("a preset"))
}

fn theme_name(theme: Option<Theme>) -> &'static str {
    match theme {
        Some(Theme::Dark) => "dark",
        Some(Theme::Light) | None => "light",
    }
}

/// A file-name-safe form of a label part.
fn file_part(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ---- shot --headless -------------------------------------------------------------------

/// Runs `icm shot --headless`.
pub fn shot(ctx: &mut Ctx, args: &ShotArgs) -> Result<()> {
    if let Some(platform) = args.platform {
        return Err(usage(format!(
            "--headless renders through the app's harness without a device; drop `{}`, or drop --headless to capture the running app",
            platform.as_str()
        )));
    }
    let mut viewports = Vec::new();
    for text in &args.viewport {
        let viewport = Viewport::parse(text).map_err(usage)?;
        if !viewports
            .iter()
            .any(|v: &Viewport| v.label == viewport.label)
        {
            viewports.push(viewport);
        }
    }

    let project = ctx.project()?.clone();
    if args.all_viewports {
        for viewport in configured(&project) {
            if !viewports.iter().any(|v| v.label == viewport.label) {
                viewports.push(viewport);
            }
        }
    }
    if viewports.is_empty() {
        viewports.push(default_viewport(&project));
    }
    if viewports.len() > 1 && args.out.is_some() {
        return Err(usage(
            "--out names one file, but several viewports were asked for; use --out-dir",
        ));
    }
    if viewports.len() > 1 && args.name.is_some() {
        return Err(usage(
            "--name labels one screenshot, but several viewports were asked for",
        ));
    }

    let theme = theme_name(args.theme);
    let out_dir = args
        .out_dir
        .as_deref()
        .map(absolute)
        .unwrap_or_else(|| project.icm_dir.join("host").join("shots"));
    std::fs::create_dir_all(&out_dir).map_err(|error| {
        IcmError::new(
            CheckId::ToolFailed,
            format!("cannot create {}: {error}", crate::paths::display(&out_dir)),
        )
    })?;

    let harness = harness::build(ctx)?;
    let wait_ms = args.wait.as_millis().to_string();
    let several = viewports.len() > 1;

    let jobs: Vec<Job> = viewports
        .iter()
        .map(|viewport| {
            let label = match &args.name {
                Some(name) => file_part(name),
                None => {
                    let mut label = format!("{}-{theme}", file_part(&viewport.label));
                    if let Some(preset) = &args.preset {
                        label.push('-');
                        label.push_str(&file_part(preset));
                    }
                    label
                }
            };
            let path = match &args.out {
                Some(out) => absolute(out),
                None => out_dir.join(format!("{label}.png")),
            };
            let preview = raster::preview_path(&path);

            let mut command = vec!["icm-shot".to_string()];
            command.extend(viewport.args());
            command.extend(["--theme".to_string(), theme.to_string()]);
            if let Some(preset) = &args.preset {
                command.extend(["--preset".to_string(), preset.clone()]);
            }
            command.extend([
                "--wait-ms".to_string(),
                wait_ms.clone(),
                "--out".to_string(),
                path.display().to_string(),
            ]);
            Job {
                viewport: viewport.clone(),
                label,
                path,
                preview,
                command,
            }
        })
        .collect();

    // Each render is its own process on one CPU core: run a few at once.
    let rendered = render_all(ctx, &harness, &jobs);

    let mut shots = Vec::new();
    let mut blank = 0;
    for (job, outcome) in jobs.iter().zip(rendered) {
        let (reply, examined) = outcome?;
        let scale = reply
            .result
            .get("scale")
            .and_then(Value::as_f64)
            .unwrap_or(job.viewport.scale);
        let screen = Screen::new(examined.size, scale);

        let (shot_kind, preview_kind) = if several {
            (
                format!("screenshot.{}", job.label),
                format!("preview.{}", job.label),
            )
        } else {
            ("screenshot".to_string(), "preview".to_string())
        };
        let mut fields = examined.artifact_fields(&job.path);
        let _ = fields.insert("viewport".into(), json!(job.viewport.label));
        ctx.rep.artifact_with(&shot_kind, &job.path, fields);
        let mut preview_fields = Map::new();
        let _ = preview_fields.insert(
            "size".into(),
            json!([examined.preview_size.0, examined.preview_size.1]),
        );
        ctx.rep
            .artifact_with(&preview_kind, &job.preview, preview_fields);

        if examined.blankness.blank {
            blank += 1;
            let facts = Facts {
                platform: Some("headless"),
                screen_blank: true,
                alive: true,
                ..Facts::default()
            };
            let mut check = Check::warn(
                CheckId::RunScreenBlank,
                format!("{}: {}", job.label, examined.blankness.describe()),
            )
            .evidence(Evidence::file(&job.path))
            .fix(
                "The view drew one colour: check that App::view returns its content and that its text has a font (likely_causes says more).",
                &["icm ui --headless tree --json -q"],
            );
            check.error = signatures::annotate(check.error, "", &facts);
            ctx.rep.check(check);
        }

        if !several {
            ctx.rep.set("screen", screen.to_json());
        }
        shots.push(json!({
            "label": job.label,
            "viewport": job.viewport.label,
            "theme": theme,
            "drawn_theme": reply.result.get("theme"),
            "preset": args.preset,
            "path": crate::paths::display(&job.path),
            "preview": crate::paths::display(&job.preview),
            "size": [examined.size.0, examined.size.1],
            "logical": [job.viewport.size.0, job.viewport.size.1],
            "scale": scale,
            "screen": screen.to_json(),
            "blank": examined.blankness.blank,
            "dominant": examined.blankness.hex(),
        }));
    }

    ctx.rep.set("backend", json!(harness.backend));
    let labels: Vec<&str> = jobs.iter().map(|job| job.label.as_str()).collect();
    let mut summary = format!(
        "rendered {} headless screenshot(s) with {}: {}",
        shots.len(),
        harness.backend,
        labels.join(", ")
    );
    if blank > 0 {
        summary.push_str(&format!("; {blank} of them a single colour"));
    }
    ctx.rep.summary(summary);
    ctx.rep.set("shots", Value::Array(shots));
    ctx.rep.next(
        "icm ui --headless tree --json -q",
        "every widget with its text and bounds",
    );
    Ok(())
}

/// One headless screenshot to take.
struct Job {
    viewport: Viewport,
    label: String,
    path: PathBuf,
    preview: PathBuf,
    command: Vec<String>,
}

/// How many renders run at once.
const PARALLEL: usize = 4;

/// Renders every job (a few at a time) and examines each PNG; the results
/// come back in the jobs' order.
fn render_all(
    ctx: &Ctx,
    harness: &Harness,
    jobs: &[Job],
) -> Vec<Result<(harness::Reply, raster::Examined)>> {
    let mut results = Vec::with_capacity(jobs.len());
    for chunk in jobs.chunks(PARALLEL) {
        let chunk_results: Vec<Result<(harness::Reply, raster::Examined)>> =
            std::thread::scope(|scope| {
                let handles: Vec<_> = chunk
                    .iter()
                    .map(|job| scope.spawn(move || render(ctx, harness, job)))
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| {
                        handle.join().unwrap_or_else(|_| {
                            Err(IcmError::new(
                                CheckId::InternalBug,
                                "a render thread panicked",
                            ))
                        })
                    })
                    .collect()
            });
        let failed = chunk_results.iter().any(Result::is_err);
        results.extend(chunk_results);
        if failed {
            break;
        }
    }
    results
}

fn render(ctx: &Ctx, harness: &Harness, job: &Job) -> Result<(harness::Reply, raster::Examined)> {
    // A failed render must not leave the last run's picture behind.
    let _ = std::fs::remove_file(&job.path);
    let _ = std::fs::remove_file(&job.preview);

    let step = format!("harness.shot.{}", job.label);
    let reply = harness.call(ctx, &step, &job.command, harness::COMMAND_TIMEOUT)?;
    if !reply.ok || !job.path.is_file() {
        return Err(IcmError::new(
            CheckId::ToolFailed,
            format!(
                "the harness did not write {} for {}",
                crate::paths::display(&job.path),
                job.viewport.label
            ),
        ));
    }
    let examined = raster::examine(&job.path, &job.preview).map_err(|error| {
        IcmError::new(
            CheckId::ToolFailed,
            format!(
                "cannot read the harness's PNG {}: {error}",
                crate::paths::display(&job.path)
            ),
        )
        .evidence(Evidence::file(&job.path))
    })?;
    Ok((reply, examined))
}

// ---- ui --headless ---------------------------------------------------------------------

/// Runs `icm ui --headless …`.
pub fn ui(ctx: &mut Ctx, args: &UiArgs) -> Result<()> {
    match &args.action {
        UiAction::Tree { viewport } => tree(ctx, viewport.as_deref(), None),
        UiAction::Find { selector } => tree(ctx, None, Some(selector)),
        UiAction::Ice { file } => ice(ctx, file),
    }
}

/// Where a command's answer files go: the run directory, else
/// `target/icm/host`.
fn answer_dir(ctx: &Ctx, project: &Project) -> PathBuf {
    ctx.rep
        .run_dir()
        .unwrap_or_else(|| project.icm_dir.join("host"))
}

fn tree(ctx: &mut Ctx, viewport: Option<&str>, selector: Option<&str>) -> Result<()> {
    let explicit = viewport.map(Viewport::parse).transpose().map_err(usage)?;
    let project = ctx.project()?.clone();
    let viewport = explicit.unwrap_or_else(|| default_viewport(&project));
    let harness = harness::build(ctx)?;

    let out = answer_dir(ctx, &project).join("tree.json");
    let command = vec![
        "icm-tree".to_string(),
        "--viewport".to_string(),
        viewport.arg.clone(),
        "--wait-ms".to_string(),
        TREE_WAIT_MS.to_string(),
        "--out".to_string(),
        out.display().to_string(),
    ];
    let _ = harness.call(ctx, "harness.tree", &command, harness::COMMAND_TIMEOUT)?;
    let tree = read_json(&out)?;
    let widgets = tree
        .get("widgets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    ctx.rep.artifact("tree", &out);

    let Some(selector) = selector else {
        ctx.rep.set(
            "tree",
            json!({
                "viewport": viewport.label,
                "size": [viewport.size.0, viewport.size.1],
                "scale": viewport.scale,
                "widgets": widgets,
            }),
        );
        ctx.rep.summary(format!(
            "{} widget(s) at {} ({}x{} logical px)",
            widgets.len(),
            viewport.label,
            viewport.size.0,
            viewport.size.1
        ));
        ctx.rep.content(render_tree(&viewport, &widgets));
        ctx.rep.next(
            "icm ui --headless find \"<text>\" --json -q",
            "the widgets showing a text, with their centres",
        );
        return Ok(());
    };

    let (matched, how) = select(&widgets, selector);
    ctx.rep.set("selector", json!(selector));
    ctx.rep.set("viewport", json!(viewport.label));
    ctx.rep.set("matches", Value::Array(matched.clone()));
    if matched.is_empty() {
        let texts: Vec<String> = widgets
            .iter()
            .filter(|w| w.get("visible").is_some_and(|v| !v.is_null()))
            .filter_map(|w| w.get("text").and_then(Value::as_str))
            .take(20)
            .map(|t| format!("{t:?}"))
            .collect();
        let mut detail = format!("no widget at {} matches {selector:?}", viewport.label);
        if !texts.is_empty() {
            detail.push_str(&format!("; visible texts: {}", texts.join(", ")));
        }
        return Err(IcmError::new(CheckId::UiSelectorNotFound, detail)
            .evidence(Evidence::file(&out))
            .fix(
                "Match a widget's exact text, or its id as #id; `icm ui --headless tree` lists them.",
                &["icm ui --headless tree --json -q"],
            ));
    }
    ctx.rep.summary(format!(
        "{} widget(s) match {selector:?} ({how}) at {}",
        matched.len(),
        viewport.label
    ));
    ctx.rep.content(render_tree(&viewport, &matched));
    Ok(())
}

fn read_json(path: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        IcmError::new(
            CheckId::ToolFailed,
            format!(
                "the harness did not write {}: {error}",
                crate::paths::display(path)
            ),
        )
    })?;
    serde_json::from_str(&text).map_err(|error| {
        IcmError::new(
            CheckId::ToolFailed,
            format!(
                "the harness wrote invalid JSON to {}: {error}",
                crate::paths::display(path)
            ),
        )
        .evidence(Evidence::file(path))
    })
}

/// The widgets a selector matches, each with its `center` (logical
/// pixels, of the on-screen rectangle), and how it matched.
///
/// `#name` (or `id:name`) matches an id; any other text matches a widget's
/// text exactly, or, when none does, as a case-insensitive substring.
pub fn select(widgets: &[Value], selector: &str) -> (Vec<Value>, &'static str) {
    let selector = selector.trim();
    let unquoted = selector
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(selector);

    let with_center = |widget: &Value| {
        let mut widget = widget.clone();
        let rect = widget
            .get("visible")
            .filter(|v| !v.is_null())
            .or_else(|| widget.get("bounds"))
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_f64).collect::<Vec<_>>());
        if let Some(r) = rect
            && r.len() == 4
        {
            widget["center"] = json!([round1(r[0] + r[2] / 2.0), round1(r[1] + r[3] / 2.0)]);
        }
        widget
    };

    if let Some(id) = unquoted
        .strip_prefix('#')
        .or_else(|| unquoted.strip_prefix("id:"))
    {
        let found = widgets
            .iter()
            .filter(|w| w.get("id").and_then(Value::as_str) == Some(id))
            .map(with_center)
            .collect();
        return (found, "id");
    }

    let text = unquoted.strip_prefix("text:").unwrap_or(unquoted);
    let exact: Vec<Value> = widgets
        .iter()
        .filter(|w| w.get("text").and_then(Value::as_str) == Some(text))
        .map(with_center)
        .collect();
    if !exact.is_empty() {
        return (exact, "exact text");
    }
    let lower = text.to_lowercase();
    let contains = widgets
        .iter()
        .filter(|w| {
            w.get("text")
                .and_then(Value::as_str)
                .is_some_and(|t| t.to_lowercase().contains(&lower))
        })
        .map(with_center)
        .collect();
    (contains, "text contains")
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn rect_text(value: Option<&Value>) -> Option<String> {
    let r: Vec<f64> = value?
        .as_array()?
        .iter()
        .filter_map(Value::as_f64)
        .collect();
    (r.len() == 4).then(|| {
        format!(
            "{},{} {}x{}",
            round1(r[0]),
            round1(r[1]),
            round1(r[2]),
            round1(r[3])
        )
    })
}

/// The human listing: one line per widget that has a text or an id.
pub fn render_tree(viewport: &Viewport, widgets: &[Value]) -> String {
    let mut out = format!(
        "viewport {} ({}x{} logical px, scale {})\n",
        viewport.label, viewport.size.0, viewport.size.1, viewport.scale
    );
    for widget in widgets {
        let kind = widget.get("kind").and_then(Value::as_str).unwrap_or("?");
        let id = widget.get("id").and_then(Value::as_str);
        let text = widget.get("text").and_then(Value::as_str);
        if id.is_none() && text.is_none() {
            continue;
        }
        let mut line = kind.to_string();
        if let Some(id) = id {
            line.push_str(&format!(" #{id}"));
        }
        if let Some(text) = text {
            line.push_str(&format!(" {text:?}"));
        }
        match rect_text(widget.get("visible").filter(|v| !v.is_null())) {
            Some(rect) => line.push_str(&format!("  at {rect}")),
            None => line.push_str("  (scrolled or clipped away)"),
        }
        if let Some(focused) = widget.get("focused").and_then(Value::as_bool) {
            line.push_str(if focused { "  focused" } else { "" });
        }
        if let Some(center) = widget.get("center").and_then(Value::as_array) {
            let c: Vec<String> = center.iter().map(|v| v.to_string()).collect();
            line.push_str(&format!("  center {}", c.join(",")));
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn ice(ctx: &mut Ctx, file: &Path) -> Result<()> {
    let path = absolute(file);
    if !path.is_file() {
        return Err(usage(format!(
            "no .ice flow at {}",
            crate::paths::display(&path)
        )));
    }
    let project = ctx.project()?.clone();
    let harness: Harness = harness::build(ctx)?;

    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "flow".to_string());
    let report = answer_dir(ctx, &project).join(format!("ice-{}.json", file_part(&stem)));
    let timeout = match ctx.remaining() {
        Some(left) if left < ICE_TIMEOUT => left.max(Duration::from_secs(1)),
        _ => ICE_TIMEOUT,
    };
    let command = vec![
        "icm-ice".to_string(),
        path.display().to_string(),
        "--report".to_string(),
        report.display().to_string(),
        "--timeout-ms".to_string(),
        timeout.as_millis().to_string(),
    ];
    let reply = harness.call(
        ctx,
        "harness.ice",
        &command,
        timeout + harness::COMMAND_TIMEOUT,
    )?;
    ctx.rep.artifact("report", &report);
    ctx.rep.set("flow", reply.result.clone());
    ctx.rep.content(render_flow(&reply.result));
    if let Some(error) = harness::report_flow(ctx, &reply.result) {
        return Err(error);
    }
    if reply.ok {
        ctx.rep.summary(format!(
            "{} passed",
            reply
                .result
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("the flow")
        ));
    }
    Ok(())
}

/// The human listing of a flow's steps.
pub fn render_flow(flow: &Value) -> String {
    let mut out = String::new();
    let name = flow.get("name").and_then(Value::as_str).unwrap_or("flow");
    let passed = flow.get("passed").and_then(Value::as_bool).unwrap_or(false);
    out.push_str(&format!(
        "{name}: {}\n",
        if passed { "passed" } else { "FAILED" }
    ));
    if let Some(error) = flow.get("error").and_then(Value::as_str) {
        out.push_str(&format!("  {error}\n"));
    }
    for step in flow
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let line = step.get("line").and_then(Value::as_u64).unwrap_or(0);
        let status = step.get("status").and_then(Value::as_str).unwrap_or("?");
        let instruction = step
            .get("instruction")
            .and_then(Value::as_str)
            .unwrap_or("");
        out.push_str(&format!("  line {line:>3} {status:<7} {instruction}\n"));
        if let Some(reason) = step.get("reason").and_then(Value::as_str) {
            out.push_str(&format!("           {reason}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widgets() -> Vec<Value> {
        serde_json::from_str(
            r#"[
            {"kind":"container","id":null,"bounds":[0,0,402,874],"visible":[0,0,402,874]},
            {"kind":"text","id":null,"text":"Count: 0","bounds":[16,22.8,251.776,31.2],"visible":[16,22.8,251.776,31.2]},
            {"kind":"text","id":null,"text":"Increment","bounds":[295.776,28,74.224,20.8],"visible":[295.776,28,74.224,20.8]},
            {"kind":"text_input","id":"new-item","text":"New item","focused":false,"bounds":[16,76.8,301.696,44.8],"visible":[16,76.8,301.696,44.8]},
            {"kind":"text","id":null,"text":"Item 30","bounds":[16,1500,100,20],"visible":null}
        ]"#,
        )
        .unwrap()
    }

    #[test]
    fn selectors_match_ids_and_texts() {
        let all = widgets();
        let (found, how) = select(&all, "#new-item");
        assert_eq!((found.len(), how), (1, "id"));
        assert_eq!(found[0]["center"], json!([166.8, 99.2]));

        let (found, how) = select(&all, "Increment");
        assert_eq!((found.len(), how), (1, "exact text"));
        assert_eq!(found[0]["center"], json!([332.9, 38.4]));

        let (found, how) = select(&all, "count");
        assert_eq!((found.len(), how), (1, "text contains"));

        let (found, _) = select(&all, "\"Item 30\"");
        assert_eq!(found[0]["center"], json!([66.0, 1510.0]));

        assert!(select(&all, "#nothing").0.is_empty());
        assert!(select(&all, "Missing").0.is_empty());
    }

    #[test]
    fn trees_render_for_people() {
        let viewport = Viewport::parse("iphone-17").unwrap();
        let text = render_tree(&viewport, &widgets());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "viewport iphone-17 (402x874 logical px, scale 3)");
        assert_eq!(lines[1], "text \"Count: 0\"  at 16,22.8 251.8x31.2");
        assert_eq!(
            lines[3],
            "text_input #new-item \"New item\"  at 16,76.8 301.7x44.8"
        );
        assert_eq!(lines[4], "text \"Item 30\"  (scrolled or clipped away)");
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn flows_render_step_by_step() {
        let flow = json!({"name":"flows::smoke","passed":false,"error":null,"steps":[
            {"line":4,"instruction":"expect \"Count: 0\"","status":"passed","ms":2},
            {"line":5,"instruction":"click \"Nope\"","status":"failed","ms":1,"reason":"its target is not in the view","texts":["Count: 0"]},
            {"line":6,"instruction":"expect \"Count: 1\"","status":"skipped","ms":0}
        ]});
        let text = render_flow(&flow);
        assert!(text.starts_with("flows::smoke: FAILED\n"));
        assert!(text.contains(
            "  line   5 failed  click \"Nope\"\n           its target is not in the view\n"
        ));
    }

    #[test]
    fn labels_are_file_safe() {
        assert_eq!(file_part("800x600@2"), "800x600@2");
        assert_eq!(file_part("logged in/home"), "logged_in_home");
    }
}
