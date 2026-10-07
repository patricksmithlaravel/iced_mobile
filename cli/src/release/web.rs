//! The web release pipeline (design §11.3, §9.5, §12.4): a static site for
//! any host, built, optimized, content-hashed, gated and loaded once in
//! headless Chrome before the owner deploys it.
//!
//! - **Preconditions.** The wasm32 target on the project's toolchain; a
//!   `Cargo.lock` and the wasm-bindgen CLI of its version
//!   (`deps.wasm_bindgen_cli`); binaryen's `wasm-opt` at its pin
//!   (`crate::pinned`, downloaded only with `--yes`); Chrome or Chromium
//!   for the serve check.
//! - **Build.** `cargo build --profile icm-web` from `--config` (size
//!   optimized, LTO, one codegen unit; `release/compile.rs`) with `--locked`
//!   in the release target directory, `[web] rustflags` in
//!   `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS`; `wasm-bindgen --target
//!   web --no-typescript --out-name app`; `wasm-opt -Oz` with one
//!   `--enable-*` flag per target feature `rustc --print cfg` reports for
//!   the project's toolchain (and its `[web] rustflags`), so wasm-opt
//!   accepts exactly what rustc emitted.
//! - **The site** (`dist/<version>+<build>/web/site/`, the upload, and its
//!   deterministic `site.zip`): content-hashed `pkg/app-<h8>.js` and
//!   `pkg/app_bg-<h8>.wasm`, `index.html` and `404.html`, `.nojekyll`,
//!   `_headers`, `manifest.webmanifest`, the icons, `[app] resources` and
//!   `THIRD_PARTY_NOTICES.txt` (`web/release_site.rs`). Beside it:
//!   `hosting/` (nginx, Apache and Caddy header snippets) and `size.json`
//!   (the size report).
//! - **Gates** (through [`Release::check`]): `web.hashed_assets`,
//!   `web.mime` (`_headers`, then the response the page got),
//!   `web.size_budget` (the `.wasm`'s `gzip -9` size against `[web]
//!   size_budget_kb`), `web.fonts_embedded` (a text font inside the `.wasm`
//!   or a font file in the site; iced's own icon font does not count),
//!   `web.renderer_fallback` (iced's `webgl` for wasm32) and
//!   `web.serve_smoke`: icm serves the site on loopback and loads it in
//!   headless Chrome, which must report `ICM_EVENT ready`, show a page that
//!   is not blank, and log no console error (`web/smoke.rs`).
//! - **Verify.** `icm verify web` runs the same gates on a site directory
//!   (or a `site.zip`); with `--url`, it loads the deployed site instead,
//!   checking readiness, the screenshot and the `.wasm`'s MIME type on the
//!   real host.
//!
//! A static site is not signed: under `--sign auto` the release counts as
//! signed, and `artifacts.json` says why in `signing`.

use super::verify::Verify;
use super::{Pipeline, Release, owner_plans};
use crate::cargo::Select;
use crate::catalogue::CheckId;
use crate::cli::SignMode;
use crate::context::{Ctx, Project};
use crate::error::{Check, Evidence, IcmError, Result};
use crate::plan::{Plan, Step};
use crate::process::Cmd;
use crate::tools::Found;
use crate::web::release_site::{self as files, Hashed};
use crate::web::smoke::copy_tree;
use crate::web::{self, TRIPLE, smoke};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The web pipeline.
pub struct Web;

/// The site directory in the dist directory (the upload).
pub const SITE: &str = "site";

/// The zipped site, for hosts that take an archive.
pub const SITE_ZIP: &str = "site.zip";

/// The header snippets for servers that do not read `_headers`.
pub const HOSTING: &str = "hosting";

/// The size report.
pub const SIZE: &str = "size.json";

/// How long the app gets to send `ICM_EVENT ready` in the serve check.
const SMOKE_WAIT: Duration = Duration::from_secs(60);

/// How long the page runs after `ready` before the screenshot.
const SMOKE_SETTLE: Duration = Duration::from_secs(1);

/// How long wasm-opt may take.
const WASM_OPT_TIMEOUT: Duration = Duration::from_secs(20 * 60);

fn io(what: &str, path: &Path, error: impl std::fmt::Display) -> IcmError {
    IcmError::new(
        CheckId::InternalBug,
        format!("cannot {what} {}: {error}", crate::paths::display(path)),
    )
}

/// The wasm-bindgen version in the app's lock, or the error that says how
/// to get a lock.
fn lock_version(project: &Project) -> Result<String> {
    let missing = |detail: String| {
        IcmError::new(CheckId::DepsWasmBindgenCli, detail)
            .evidence(Evidence::file(project.lock_path()))
            .fix(
                "Create the lock and install the matching wasm-bindgen CLI, then commit Cargo.lock: a release builds it with --locked.",
                &["icm doctor web --fix --yes"],
            )
    };
    let Some(lock) = project.lock()? else {
        return Err(missing(format!(
            "there is no {}; a release builds the committed lock (--locked)",
            crate::paths::display(&project.lock_path())
        )));
    };
    lock.version_of("wasm-bindgen")
        .map(str::to_string)
        .ok_or_else(|| {
            missing(format!(
                "wasm-bindgen is not in {}; an iced web app depends on it through iced",
                crate::paths::display(&project.lock_path())
            ))
        })
}

/// `[web] rustflags` for cargo, in the variable that leaves global
/// RUSTFLAGS alone.
fn rustflags_env(project: &Project) -> Vec<(String, String)> {
    let rustflags = &project.config.config.web.rustflags;
    if rustflags.is_empty() {
        Vec::new()
    } else {
        vec![(
            "CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS".to_string(),
            rustflags.join(" "),
        )]
    }
}

/// `rustc --print cfg` for wasm32 with the project's toolchain and `[web]
/// rustflags`: the target features rustc compiles for.
fn cfg_cmd(project: &Project) -> Cmd {
    Cmd::tool("rustc")
        .args(["--print", "cfg", "--target", TRIPLE])
        .args(&project.config.config.web.rustflags)
        .cwd(project.dir())
        .timeout(Duration::from_secs(60))
}

/// `wasm-opt -Oz <flags> <input> -o <output>`.
fn wasm_opt_cmd(wasm_opt: &Path, flags: &[String], input: &Path, output: &Path) -> Cmd {
    Cmd::new(wasm_opt)
        .arg("-Oz")
        .args(flags)
        .arg(input)
        .arg("-o")
        .arg(output)
        .timeout(WASM_OPT_TIMEOUT)
}

/// Chrome or Chromium, as `icm run web` finds it.
fn chrome(ctx: &mut Ctx) -> Result<Found> {
    let host = ctx.host()?.clone();
    crate::tools::chrome(&host, &ctx.env).map_err(|error| {
        error.fix(
            "Install Google Chrome (or Chromium), or point ICM_CHROME or host.toml's chrome at it: the release loads the site in headless Chrome (web.serve_smoke).",
            &["brew install --cask google-chrome"],
        )
    })
}

/// `wasm-opt --version`, e.g. `wasm-opt version 133 (version_133)`.
fn wasm_opt_version(ctx: &Ctx, wasm_opt: &Path) -> Option<String> {
    let outcome = ctx
        .probe(
            &Cmd::new(wasm_opt)
                .arg("--version")
                .timeout(Duration::from_secs(30)),
        )
        .ok()?;
    let text = outcome.stdout_text();
    let line = text.lines().next()?.trim();
    Some(line.strip_prefix("wasm-opt ").unwrap_or(line).to_string())
}

/// Every file under `dir`, relative and `/`-separated, sorted.
fn tree(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(relative) = path.strip_prefix(dir) {
                found.push(
                    relative
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/"),
                );
            }
        }
    }
    found.sort();
    found
}

/// The deterministic zip of a site: its files at the archive's root, in
/// name order, stored, with fixed timestamps.
pub fn zip_site(site: &Path, out: &Path) -> std::io::Result<()> {
    use crate::android::zip::{Entry, Source, write};
    let entries: Vec<Entry> = tree(site)
        .into_iter()
        .map(|name| Entry {
            source: Source::File(site.join(&name)),
            name,
        })
        .collect();
    write(out, &entries)
}

/// The `gzip -9` size of a file (the `.gz` is kept in `dir`).
fn gzip_size(ctx: &Ctx, file: &Path, dir: &Path) -> Result<u64> {
    std::fs::create_dir_all(dir).map_err(|e| io("create", dir, e))?;
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let out = dir.join(format!("{name}.gz"));
    let cmd = Cmd::new("/bin/sh")
        .args(["-c", "\"$1\" -9 -n -c -- \"$2\" > \"$3\"", "sh"])
        .arg(crate::process::tool_path("gzip"))
        .arg(file)
        .arg(&out)
        .timeout(Duration::from_secs(300));
    let step = format!("gzip.{name}");
    let outcome = ctx.step(&step, &cmd)?;
    if !outcome.success() {
        return Err(ctx.step_failure(&step, CheckId::ToolFailed, &outcome));
    }
    std::fs::metadata(&out)
        .map(|m| m.len())
        .map_err(|e| io("read", &out, e))
}

/// The `<base href>` of a page, if it has one.
fn base_of(html: &str) -> Option<String> {
    let rest = &html[html.find("<base href=\"")? + "<base href=\"".len()..];
    let end = rest.find('"')?;
    Some(rest[..end].replace("&amp;", "&"))
}

/// What the static gates found in a site.
struct Site {
    /// The hashed module names, when `index.html` loads them.
    hashed: Option<Hashed>,
    /// `[web] public_url` as the page's `<base href>` says it (`/`
    /// without one).
    public_url: String,
}

/// `web.hashed_assets`: `index.html` loads `pkg/<name>-<h8>.js` and
/// `.wasm`, both exist and their names carry their content's hash.
fn hashed_check(site: &Path) -> (Check, Site) {
    let index = site.join("index.html");
    let fail = |detail: String| Check::fail(CheckId::WebHashedAssets, detail);
    let Ok(html) = std::fs::read_to_string(&index) else {
        let check = fail(format!("{} has no index.html", crate::paths::display(site)))
            .evidence(Evidence::file(site));
        return (
            check,
            Site {
                hashed: None,
                public_url: "/".into(),
            },
        );
    };
    let public_url = base_of(&html).unwrap_or_else(|| "/".to_string());
    let Some(hashed) = files::parse_index(&html) else {
        let check = fail(
            "index.html loads no wasm-bindgen module by name (`import init from \"./pkg/…js\"` and `module_or_path: \"./pkg/…wasm\"`)".to_string(),
        )
        .evidence(Evidence::file(&index));
        return (
            check,
            Site {
                hashed: None,
                public_url,
            },
        );
    };
    let mut problems = Vec::new();
    for name in [&hashed.js, &hashed.wasm] {
        let path = site.join(name);
        let Some(hash) = files::hash_in_name(name) else {
            problems.push(format!("{name} has no content hash in its name"));
            continue;
        };
        match crate::hash::sha256_file(&path) {
            Ok(sha) if sha.starts_with(hash) => {}
            Ok(sha) => problems.push(format!(
                "{name}'s content hashes to {}, not {hash} (changed after the release?)",
                files::h8(&sha)
            )),
            Err(error) => problems.push(format!("{name}: {error}")),
        }
    }
    let check = if problems.is_empty() {
        Check::pass(
            CheckId::WebHashedAssets,
            format!(
                "{} and {} are named by their content",
                hashed.js, hashed.wasm
            ),
        )
    } else {
        fail(problems.join("; ")).evidence(Evidence::file(&index))
    };
    (
        check,
        Site {
            hashed: Some(hashed),
            public_url,
        },
    )
}

/// `web.mime` from `_headers`: the `.wasm`'s declared type.
fn headers_check(site: &Path, hashed: &Hashed, public_url: &str) -> Check {
    let path = site.join("_headers");
    let wanted = format!("{}{}", files::path_prefix(public_url), hashed.wasm);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Check::warn(
            CheckId::WebMime,
            "the site has no _headers: the host must serve .wasm as application/wasm itself (`icm verify web --url` checks the deployed host)",
        );
    };
    match files::header_content_type(&text, &wanted) {
        Some(value) if value == "application/wasm" => Check::pass(
            CheckId::WebMime,
            format!("_headers serves {wanted} as application/wasm (Netlify, Cloudflare Pages)"),
        ),
        Some(value) => Check::fail(
            CheckId::WebMime,
            format!("_headers serves {wanted} as `{value}`, not application/wasm"),
        )
        .evidence(Evidence::file(&path)),
        None => Check::fail(
            CheckId::WebMime,
            format!("_headers sets no Content-Type for {wanted}"),
        )
        .evidence(Evidence::file(&path)),
    }
}

/// `web.size_budget`: the `.wasm`'s gzip size against `[web]
/// size_budget_kb`.
fn size_check(wasm: &Path, bytes: u64, gzip: u64, budget_kb: u64) -> Check {
    let shown = |n: u64| format!("{:.1} KB", n as f64 / 1024.0);
    let detail = format!(
        "{} is {} ({} gzipped); the budget is {budget_kb} KB gzipped",
        wasm.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        shown(bytes),
        shown(gzip)
    );
    if gzip <= budget_kb * 1024 {
        Check::pass(CheckId::WebSizeBudget, detail)
    } else {
        Check::fail(CheckId::WebSizeBudget, detail)
            .evidence(Evidence::file(wasm))
            .fix(
                "Shrink the app (drop features and dependencies, avoid large embedded assets), or raise [web] size_budget_kb in icm.toml.",
                &[],
            )
    }
}

/// `web.fonts_embedded`: a text font inside the `.wasm` (iced's icon font
/// does not count), or a font file in the site that the app loads.
fn fonts_check(site: &Path, wasm: &Path) -> Check {
    let bytes = std::fs::read(wasm).unwrap_or_default();
    let families = files::wasm_fonts(&bytes);
    let text: Vec<&str> = families
        .iter()
        .map(String::as_str)
        .filter(|family| !files::icon_only(family))
        .collect();
    if !text.is_empty() {
        return Check::pass(
            CheckId::WebFontsEmbedded,
            format!("the .wasm embeds {}", text.join(", ")),
        );
    }
    let font_files = files::font_files(site);
    if !font_files.is_empty() {
        return Check::pass(
            CheckId::WebFontsEmbedded,
            format!(
                "the .wasm embeds no text font, and the site carries {} for the app to load",
                font_files.join(", ")
            ),
        );
    }
    Check::fail(
        CheckId::WebFontsEmbedded,
        format!(
            "the .wasm embeds no text font{} and the site has no font file; the web has no system fonts, so text draws as nothing",
            if families.is_empty() {
                String::new()
            } else {
                format!(" (only {})", families.join(", "))
            }
        ),
    )
    .evidence(Evidence::file(wasm))
    .fix(
        "Enable iced's `fira-sans` feature for wasm32 (or ship a font in [app] resources and load it).",
        &[web::FEATURES_FIX],
    )
}

/// The size report (`size.json`, the result's `size`).
fn size_report(
    site: &Path,
    hashed: &Hashed,
    wasm: (u64, u64),
    js: (u64, u64),
    before_opt: Option<u64>,
    budget_kb: u64,
) -> Value {
    let names = tree(site);
    let total: u64 = names
        .iter()
        .filter_map(|name| std::fs::metadata(site.join(name)).ok())
        .map(|m| m.len())
        .sum();
    json!({
        "wasm": {
            "path": hashed.wasm,
            "bytes": wasm.0,
            "gzip": wasm.1,
            "before_wasm_opt": before_opt,
            "budget_kb": budget_kb,
        },
        "js": {"path": hashed.js, "bytes": js.0, "gzip": js.1},
        "site": {"bytes": total, "files": names.len()},
    })
}

/// The static gates on a site (`web.hashed_assets`, `web.mime` from
/// `_headers`, `web.size_budget`, `web.fonts_embedded`) and the size
/// report; `scratch` holds the `.gz` files.
fn site_checks(
    ctx: &Ctx,
    site: &Path,
    budget_kb: u64,
    before_opt: Option<u64>,
    scratch: &Path,
) -> Result<(Vec<Check>, Site, Option<Value>)> {
    let (hashed_check, facts) = hashed_check(site);
    let mut checks = vec![hashed_check];
    let Some(hashed) = facts.hashed.clone() else {
        return Ok((checks, facts, None));
    };
    checks.push(headers_check(site, &hashed, &facts.public_url));
    let wasm = site.join(&hashed.wasm);
    let js = site.join(&hashed.js);
    if !wasm.is_file() || !js.is_file() {
        return Ok((checks, facts, None));
    }
    let size = |path: &Path| std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let wasm_sizes = (size(&wasm), gzip_size(ctx, &wasm, scratch)?);
    let js_sizes = (size(&js), gzip_size(ctx, &js, scratch)?);
    checks.push(size_check(&wasm, wasm_sizes.0, wasm_sizes.1, budget_kb));
    checks.push(fonts_check(site, &wasm));
    let report = size_report(site, &hashed, wasm_sizes, js_sizes, before_opt, budget_kb);
    ctx.rep.progress(format!(
        "size: {} {:.1} KB ({:.1} KB gzipped, budget {budget_kb} KB); {} {:.1} KB",
        hashed.wasm,
        wasm_sizes.0 as f64 / 1024.0,
        wasm_sizes.1 as f64 / 1024.0,
        hashed.js,
        js_sizes.0 as f64 / 1024.0,
    ));
    Ok((checks, facts, Some(report)))
}

/// The serve check, as checks; the screenshot and console are reported as
/// artifacts and the report as the result's `smoke`.
fn serve(ctx: &mut Ctx, target: &smoke::Target, dir: &Path, deployed: bool) -> Result<Vec<Check>> {
    let chrome = chrome(ctx)?;
    let wait = ctx
        .remaining()
        .map_or(SMOKE_WAIT, |left| left.min(SMOKE_WAIT));
    let options = smoke::Options {
        chrome: chrome.path.clone(),
        viewport: web::viewport::Viewport::parse(web::viewport::DEFAULT)
            .map_err(|detail| IcmError::new(CheckId::InternalBug, detail))?,
        dir: dir.to_path_buf(),
        wait,
        settle: SMOKE_SETTLE,
    };
    let started = std::time::Instant::now();
    ctx.rep.step_begin(
        "web.serve_smoke",
        &[
            crate::paths::display(&chrome.path),
            "--headless=new".to_string(),
            match target {
                smoke::Target::Site { root, path } => {
                    format!("<{} served at {path}>", crate::paths::display(root))
                }
                smoke::Target::Url(url) => smoke::url_with_query(url),
            },
        ],
        &[],
        None,
    );
    let report = smoke::run(target, &options);
    ctx.rep.step_end_internal(
        "web.serve_smoke",
        report.is_ok(),
        started.elapsed().as_millis() as u64,
    );
    let report = match report {
        Ok(report) => report,
        Err(error) => {
            return Ok(vec![
                Check::fail(
                    CheckId::WebServeSmoke,
                    format!("the serve check could not run headless Chrome: {error}"),
                )
                .evidence(Evidence::file(dir.join("chrome.log")))
                .fix(
                    "Check the Chrome path (`icm print tools`) and that `icm run web` works.",
                    &["icm print tools"],
                ),
            ]);
        }
    };
    if let smoke::Outcome::Interrupted(signal) = report.outcome {
        return Err(crate::output::interrupted(signal));
    }
    if let Some(png) = &report.screenshot {
        let mut extra = serde_json::Map::new();
        let _ = extra.insert(
            "blank".into(),
            json!(report.blankness.map(|b| b.is_blank())),
        );
        ctx.rep.artifact_with("screenshot", png, extra);
    }
    if let Some(preview) = &report.preview {
        ctx.rep.artifact("preview", preview);
    }
    ctx.rep.artifact("console", &report.console);
    ctx.rep.set("smoke", report.to_json());
    Ok(smoke::checks(&report, deployed))
}

/// Where a command's serve check keeps its files: the run directory.
fn smoke_dir(ctx: &Ctx, fallback: &Path) -> PathBuf {
    ctx.rep
        .run_dir()
        .unwrap_or_else(|| fallback.to_path_buf())
        .join("smoke")
}

impl Pipeline for Web {
    fn plan(&self, ctx: &Ctx, rel: &Release) -> Result<Plan> {
        let project = &rel.project;
        let bin = project.bin_for("web")?;
        let mut plan = Plan::new();
        plan.push(Step::internal(
            "web.toolchain",
            &format!("check the {TRIPLE} target on the project's toolchain"),
        ));
        let bindgen = match lock_version(project) {
            Ok(version) => match crate::tools::wasm_bindgen(&version, &ctx.env) {
                Ok(found) => {
                    plan.push(
                        Step::internal(
                            "deps.wasm_bindgen_cli",
                            &format!(
                                "wasm-bindgen {version} ({})",
                                crate::paths::display(&found.path)
                            ),
                        )
                        .gate(CheckId::DepsWasmBindgenCli),
                    );
                    found.path
                }
                Err(_) => {
                    plan.push(
                        Step::internal(
                            "deps.wasm_bindgen_cli",
                            &format!("no wasm-bindgen CLI {version} yet: the release stops with deps.wasm_bindgen_cli (`icm doctor web --fix --yes`)"),
                        )
                        .gate(CheckId::DepsWasmBindgenCli),
                    );
                    PathBuf::from("wasm-bindgen")
                }
            },
            Err(error) => {
                plan.push(
                    Step::internal("deps.wasm_bindgen_cli", &error.detail)
                        .gate(CheckId::DepsWasmBindgenCli),
                );
                PathBuf::from("wasm-bindgen")
            }
        };
        let wasm_opt = match crate::pinned::find(&ctx.env, "wasm-opt") {
            Ok(found) => {
                plan.push(Step::internal(
                    "pinned.wasm-opt",
                    &format!(
                        "wasm-opt {} ({}, {})",
                        found.version.as_deref().unwrap_or("override"),
                        crate::paths::display(&found.path),
                        found.source
                    ),
                ));
                found.path
            }
            Err(error) => {
                plan.push(
                    Step::internal(
                        "pinned.wasm-opt",
                        &format!(
                            "{}{}",
                            error.detail,
                            if ctx.global.yes && !ctx.global.offline {
                                " (--yes: the release downloads it, sha256-checked)"
                            } else {
                                " (the release stops with env.tool_missing; add --yes to download it)"
                            }
                        ),
                    )
                    .gate(CheckId::EnvToolMissing),
                );
                PathBuf::from("wasm-opt")
            }
        };
        plan.push(Step::internal(
            "web.chrome",
            "find Chrome or Chromium for the serve check (host.toml chrome, ICM_CHROME, the usual places)",
        ));

        let invocation = rel.invocation("build", Select::Bin(bin.clone()), Some(TRIPLE));
        plan.push(
            Step::exec(
                "cargo.build",
                invocation.cmd().envs(
                    rustflags_env(project)
                        .iter()
                        .map(|(k, v)| (k.as_str(), v.as_str())),
                ),
            )
            .on_fail(CheckId::BuildCompileError),
        );
        let wasm = rel.artifacts_dir(Some(TRIPLE)).join(format!("{bin}.wasm"));
        let bindgen_dir = rel.gen_dir.join("bindgen");
        plan.push(Step::exec(
            "wasm-bindgen",
            web::bindgen_cmd(&bindgen, &wasm, &bindgen_dir, true),
        ));
        plan.push(Step::exec("rustc.cfg", cfg_cmd(project)));
        plan.push(Step::exec(
            "wasm-opt",
            wasm_opt_cmd(
                &wasm_opt,
                &["--enable-<each target_feature rustc.cfg prints>".to_string()],
                &bindgen_dir.join("app_bg.wasm"),
                &rel.gen_dir.join("opt/app_bg.wasm"),
            ),
        ));
        let site = rel.dist.join(SITE);
        plan.push(Step::internal(
            "site.generate",
            &format!(
                "write {}: pkg/app-<h8>.js and pkg/app_bg-<h8>.wasm (content-hashed), index.html and 404.html (<base href=\"{}\">), .nojekyll, _headers, manifest.webmanifest, the icons, [app] resources and THIRD_PARTY_NOTICES.txt; {HOSTING}/ (nginx, Apache, Caddy) beside it",
                crate::paths::display(&site),
                files::base_href(&project.config.config.web.public_url)
            ),
        ));
        plan.push(
            Step::internal(
                "web.gates",
                &format!(
                    "content hashes, _headers, the .wasm's gzip size against {} KB, embedded fonts, iced's webgl feature",
                    project.config.config.web.size_budget_kb
                ),
            )
            .gate(CheckId::WebHashedAssets)
            .gate(CheckId::WebMime)
            .gate(CheckId::WebSizeBudget)
            .gate(CheckId::WebFontsEmbedded)
            .gate(CheckId::WebRendererFallback),
        );
        plan.push(
            Step::internal(
                "web.serve_smoke",
                "serve the site on 127.0.0.1 and load it in headless Chrome (?icm_events=1): ICM_EVENT ready, a screenshot that is not blank, no console error, the .wasm as application/wasm",
            )
            .gate(CheckId::WebServeSmoke)
            .gate(CheckId::WebMime),
        );
        plan.push(Step::internal(
            "site.zip",
            &format!(
                "write {} (deterministic)",
                crate::paths::display(&rel.dist.join(SITE_ZIP))
            ),
        ));
        Ok(plan)
    }

    fn preconditions(&self, ctx: &mut Ctx, rel: &mut Release) -> Result<()> {
        let project = rel.project.clone();
        // The wasm32 target on the toolchain the project resolves.
        let toolchain = crate::toolchain::active(project.dir())?;
        for check in crate::toolchain::check_targets(&toolchain, &[TRIPLE.to_string()]) {
            if check.failed() {
                return Err(check.into_error());
            }
            rel.check(ctx, check);
        }

        let version = lock_version(&project)?;
        let bindgen = crate::tools::wasm_bindgen(&version, &ctx.env)?;
        rel.check(
            ctx,
            Check::pass(
                CheckId::DepsWasmBindgenCli,
                format!(
                    "wasm-bindgen {version} ({}, {})",
                    crate::paths::display(&bindgen.path),
                    bindgen.source
                ),
            ),
        );
        rel.tool("wasm-bindgen", version);

        let wasm_opt = crate::pinned::require(ctx, "wasm-opt")?;
        let version = wasm_opt_version(ctx, &wasm_opt.path)
            .or(wasm_opt.version.clone())
            .unwrap_or_else(|| "unknown".to_string());
        rel.tool("wasm-opt", version);

        let chrome = chrome(ctx)?;
        ctx.rep.progress(format!(
            "the serve check uses {}",
            crate::paths::display(&chrome.path)
        ));
        Ok(())
    }

    fn build(&self, ctx: &mut Ctx, rel: &mut Release) -> Result<()> {
        let project = rel.project.clone();
        let web_config = project.config.config.web.clone();
        let bin = project.bin_for("web")?;
        let version = lock_version(&project)?;
        let bindgen = crate::tools::wasm_bindgen(&version, &ctx.env)?;
        let wasm_opt = crate::pinned::find(&ctx.env, "wasm-opt")?;

        // 1. cargo, with the size-optimized profile.
        let invocation = rel.invocation("build", Select::Bin(bin.clone()), Some(TRIPLE));
        let output = rel
            .cargo(
                ctx,
                "cargo.build",
                &invocation,
                &rustflags_env(&project),
                None,
            )
            .map_err(|error| web::getrandom_backend(ctx, error))?;
        let wasm = output
            .file_with_extension(&bin, "wasm")
            .map(Path::to_path_buf)
            .unwrap_or_else(|| rel.artifacts_dir(Some(TRIPLE)).join(format!("{bin}.wasm")));
        if !wasm.is_file() {
            return Err(IcmError::new(
                CheckId::BuildCargoFailed,
                format!("cargo built no {}", crate::paths::display(&wasm)),
            ));
        }

        // 2. wasm-bindgen.
        let bindgen_dir = rel.gen_dir.join("bindgen");
        let _ = std::fs::remove_dir_all(&bindgen_dir);
        let outcome = ctx.step(
            "wasm-bindgen",
            &web::bindgen_cmd(&bindgen.path, &wasm, &bindgen_dir, true),
        )?;
        if !outcome.success() {
            let text = outcome.stderr_text();
            let id = if text.contains("schema version") || text.contains("different bindgen format")
            {
                CheckId::DepsWasmBindgenCli
            } else {
                CheckId::ToolFailed
            };
            return Err(ctx.step_failure("wasm-bindgen", id, &outcome));
        }
        let out = web::site::OUT_NAME;
        let bound_wasm = bindgen_dir.join(format!("{out}_bg.wasm"));
        let bound_js = bindgen_dir.join(format!("{out}.js"));

        // 3. wasm-opt, with the features rustc compiled for.
        let cfg = ctx.step("rustc.cfg", &cfg_cmd(&project))?;
        if !cfg.success() {
            return Err(ctx.step_failure("rustc.cfg", CheckId::ToolFailed, &cfg));
        }
        let help = ctx
            .probe(
                &Cmd::new(&wasm_opt.path)
                    .arg("--help")
                    .timeout(Duration::from_secs(30)),
            )?
            .stdout_text();
        let (flags, skipped) = files::wasm_opt_flags(&cfg.stdout_text(), &help);
        if !skipped.is_empty() {
            ctx.rep.progress(format!(
                "wasm-opt has no flag for rustc's target feature(s) {}; left out",
                skipped.join(", ")
            ));
        }
        rel.tool("wasm-opt-features", flags.join(" "));
        let opt_dir = rel.gen_dir.join("opt");
        std::fs::create_dir_all(&opt_dir).map_err(|e| io("create", &opt_dir, e))?;
        let optimized = opt_dir.join(format!("{out}_bg.wasm"));
        let _ = std::fs::remove_file(&optimized);
        let outcome = ctx.step(
            "wasm-opt",
            &wasm_opt_cmd(&wasm_opt.path, &flags, &bound_wasm, &optimized),
        )?;
        if !outcome.success() || !optimized.is_file() {
            return Err(ctx
                .step_failure("wasm-opt", CheckId::ToolFailed, &outcome)
                .fix(
                    "Read the step log; a feature error means rustc's and wasm-opt's features differ (report it).",
                    &[],
                ));
        }
        let before_opt = std::fs::metadata(&bound_wasm).map(|m| m.len()).ok();

        // 4. The site.
        let site = rel.dist.join(SITE);
        let _ = std::fs::remove_dir_all(&site);
        let pkg = site.join("pkg");
        std::fs::create_dir_all(&pkg).map_err(|e| io("create", &pkg, e))?;
        let hash = |path: &Path| crate::hash::sha256_file(path).map_err(|e| io("read", path, e));
        let hashed = Hashed {
            js: files::hashed_name(out, "js", &hash(&bound_js)?),
            wasm: files::hashed_name(&format!("{out}_bg"), "wasm", &hash(&optimized)?),
        };
        let _ = std::fs::copy(&bound_js, site.join(&hashed.js))
            .map_err(|e| io("copy", &bound_js, e))?;
        let _ = std::fs::copy(&optimized, site.join(&hashed.wasm))
            .map_err(|e| io("copy", &optimized, e))?;
        let snippets = bindgen_dir.join("snippets");
        if snippets.is_dir() {
            copy_tree(&snippets, &pkg.join("snippets")).map_err(|e| io("copy", &snippets, e))?;
        }

        let started = std::time::Instant::now();
        let app = project.app().clone();
        let written = (|| -> std::result::Result<(), String> {
            // Resources first: the generated files win a name clash.
            for file in web::site::resources(project.dir(), &app.resources)? {
                let relative = file
                    .strip_prefix(project.dir())
                    .map_err(|_| format!("{} is outside the project", file.display()))?;
                let target = site.join(relative);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let _ = std::fs::copy(&file, &target)
                    .map_err(|e| format!("cannot copy {}: {e}", file.display()))?;
            }
            let background = web::site::color(&app.background);
            let rgb = crate::android::image::parse_hex_color(&background).unwrap_or([255; 3]);
            let icon = app
                .icon
                .as_deref()
                .map(|icon| project.dir().join(icon))
                .filter(|icon| icon.is_file());
            let icons = match &icon {
                Some(icon) => files::icons(icon, rgb)?,
                None => Vec::new(),
            };
            let put = |name: &str, bytes: &[u8]| -> std::result::Result<(), String> {
                std::fs::write(site.join(name), bytes)
                    .map_err(|e| format!("cannot write {name}: {e}"))
            };
            for (name, bytes) in &icons {
                put(name, bytes)?;
            }
            let index = files::index_html(
                &app.name,
                app.description.as_deref().unwrap_or(""),
                &background,
                &web_config.public_url,
                &hashed,
                !icons.is_empty(),
            );
            put("index.html", index.as_bytes())?;
            put("404.html", index.as_bytes())?;
            put(".nojekyll", b"")?;
            put(
                "_headers",
                files::headers(&files::path_prefix(&web_config.public_url), &hashed).as_bytes(),
            )?;
            put(
                "manifest.webmanifest",
                files::manifest(
                    &app.name,
                    app.description.as_deref().unwrap_or(""),
                    &background,
                    !icons.is_empty(),
                )
                .as_bytes(),
            )?;
            Ok(())
        })();
        ctx.rep.step_end_internal(
            "site.generate",
            written.is_ok(),
            started.elapsed().as_millis() as u64,
        );
        if let Err(detail) = written {
            return Err(IcmError::new(CheckId::ConfigInvalid, detail)
                .evidence(project.config.evidence("app.resources")));
        }

        // THIRD_PARTY_NOTICES at the site's root.
        let notices = rel.notices(ctx, Some(TRIPLE))?;
        let inside = site.join(super::notices::FILE);
        let _ = std::fs::copy(&notices, &inside).map_err(|e| io("copy", &notices, e))?;

        // Header snippets for servers that do not read _headers.
        let hosting = rel.dist.join(HOSTING);
        std::fs::create_dir_all(&hosting).map_err(|e| io("create", &hosting, e))?;
        for (name, text) in
            files::hosting_files(&files::path_prefix(&web_config.public_url), &hashed)
        {
            let path = hosting.join(name);
            std::fs::write(&path, text).map_err(|e| io("write", &path, e))?;
        }

        // 5. The static gates and the size report.
        let (checks, facts, size) = site_checks(
            ctx,
            &site,
            web_config.size_budget_kb,
            before_opt,
            &rel.gen_dir.join("size"),
        )?;
        for check in checks {
            rel.check(ctx, check);
        }
        match web::iced_features(ctx, &rel.package.manifest_path) {
            Some(features) => {
                let check = web::renderer_check(&features);
                rel.check(ctx, check);
            }
            None => rel.check(
                ctx,
                Check::skip(
                    CheckId::WebRendererFallback,
                    "cannot resolve iced's features for wasm32 (cargo metadata --offline failed)",
                ),
            ),
        }
        if let Some(size) = &size {
            let path = rel.dist.join(SIZE);
            let mut text = serde_json::to_string_pretty(size).unwrap_or_default();
            text.push('\n');
            std::fs::write(&path, text).map_err(|e| io("write", &path, e))?;
            ctx.rep.set("size", size.clone());
        }

        // 6. The serve check on the finished site.
        let smoke_dir = smoke_dir(ctx, &rel.gen_dir);
        let target = smoke::stage(&site, &facts.public_url, &smoke_dir)
            .map_err(|e| io("stage the site in", &smoke_dir, e))?;
        for check in serve(ctx, &target, &smoke_dir, false)? {
            rel.check(ctx, check);
        }
        let _ = std::fs::remove_dir_all(smoke_dir.join("root"));

        // 7. The zip, then the files.
        let zip = rel.dist.join(SITE_ZIP);
        zip_site(&site, &zip).map_err(|e| io("write", &zip, e))?;
        rel.embed_notices(&site, super::notices::FILE)?;
        rel.embed_notices(&zip, super::notices::FILE)?;
        rel.add_file("upload", "site", &site)?;
        rel.add_file("upload", "site_zip", &zip)?;
        rel.add_file("metadata", "hosting", &hosting)?;
        if size.is_some() {
            rel.add_file("metadata", "size_report", &rel.dist.join(SIZE))?;
        }

        rel.signed = rel.sign() == SignMode::Auto;
        rel.signing = json!({
            "kind": "none",
            "note": "a static site is not signed; the host serves it over HTTPS",
        });
        rel.owner_plan = Some(owner_plans::web(&rel.common(), SITE));
        Ok(())
    }

    fn verify(&self, ctx: &mut Ctx, verify: &mut Verify) -> Result<()> {
        // The serve check's files go into the run directory: the project's
        // (verify attached it) or, outside a project, the cache's.
        if ctx.rep.run_dir().is_none() {
            let root = verify
                .project
                .as_ref()
                .map_or_else(crate::paths::cache_dir, |project| project.icm_dir.clone());
            ctx.rep
                .attach(&root)
                .map_err(|e| io("create a run directory in", &root, e))?;
        }
        let scratch = ctx
            .rep
            .run_dir()
            .ok_or_else(|| IcmError::new(CheckId::InternalBug, "verify has no run directory"))?;

        // The deployed site.
        if let Some(url) = verify.url.clone() {
            for check in serve(ctx, &smoke::Target::Url(url), &scratch.join("smoke"), true)? {
                verify.check(ctx, check);
            }
            return Ok(());
        }

        let Some(artifact) = verify.artifact.clone() else {
            return Ok(());
        };
        let site = if artifact.is_dir() {
            artifact
        } else if super::notices::zip_names(&artifact).is_some() {
            let out = scratch.join("site");
            let _ = std::fs::remove_dir_all(&out);
            std::fs::create_dir_all(&out).map_err(|e| io("create", &out, e))?;
            let cmd = Cmd::tool("unzip")
                .args(["-q", "-o"])
                .arg(&artifact)
                .arg("-d")
                .arg(&out)
                .timeout(Duration::from_secs(300));
            let outcome = ctx.step("unzip", &cmd)?;
            if !outcome.success() {
                return Err(ctx.step_failure("unzip", CheckId::ToolFailed, &outcome));
            }
            out
        } else {
            return Err(IcmError::new(
                CheckId::UsageBadArgs,
                format!(
                    "{} is neither a site directory nor a zip of one",
                    crate::paths::display(&artifact)
                ),
            )
            .fix(
                "Pass the release's site directory (or site.zip), or --url for a deployed site.",
                &["icm verify web --artifact target/icm/dist/latest/web/site"],
            ));
        };

        let budget_kb = verify.project.as_ref().map_or_else(
            || crate::config::WebConfig::default().size_budget_kb,
            |project| project.config.config.web.size_budget_kb,
        );
        let (checks, facts, size) =
            site_checks(ctx, &site, budget_kb, None, &scratch.join("size"))?;
        for check in checks {
            verify.check(ctx, check);
        }
        if let Some(size) = size {
            ctx.rep.set("size", size);
        }
        let smoke_dir = scratch.join("smoke");
        let target = smoke::stage(&site, &facts.public_url, &smoke_dir)
            .map_err(|e| io("stage the site in", &smoke_dir, e))?;
        for check in serve(ctx, &target, &smoke_dir, false)? {
            verify.check(ctx, check);
        }
        let _ = std::fs::remove_dir_all(smoke_dir.join("root"));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bases_are_read_from_pages() {
        assert_eq!(
            base_of("<head><base href=\"/app/\"><title>"),
            Some("/app/".to_string())
        );
        assert_eq!(base_of("<head><title>"), None);
    }

    #[test]
    fn sites_zip_deterministically() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("site");
        std::fs::create_dir_all(site.join("pkg")).unwrap();
        std::fs::write(site.join("index.html"), "<html>").unwrap();
        std::fs::write(site.join("pkg/app_bg-0123abcd.wasm"), b"\0asm").unwrap();
        std::fs::write(site.join(".nojekyll"), b"").unwrap();
        let (a, b) = (dir.path().join("a.zip"), dir.path().join("b.zip"));
        zip_site(&site, &a).unwrap();
        zip_site(&site, &b).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
        assert_eq!(
            super::super::notices::zip_names(&a).unwrap(),
            [".nojekyll", "index.html", "pkg/app_bg-0123abcd.wasm"]
        );
    }

    #[test]
    fn hashed_names_are_checked_against_the_content() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path();
        std::fs::create_dir_all(site.join("pkg")).unwrap();
        let js = b"export default function init() {}";
        let wasm = b"\0asm\x01\0\0\0";
        let hashed = Hashed {
            js: files::hashed_name("app", "js", &crate::hash::sha256_hex(js)),
            wasm: files::hashed_name("app_bg", "wasm", &crate::hash::sha256_hex(wasm)),
        };
        std::fs::write(site.join(&hashed.js), js).unwrap();
        std::fs::write(site.join(&hashed.wasm), wasm).unwrap();
        std::fs::write(
            site.join("index.html"),
            files::index_html("A", "", "#FFFFFF", "/app/", &hashed, false),
        )
        .unwrap();
        let (check, facts) = hashed_check(site);
        assert_eq!(check.status, crate::error::Status::Pass, "{check:?}");
        assert_eq!(facts.public_url, "/app/");
        assert_eq!(facts.hashed.as_ref(), Some(&hashed));

        // _headers declares the type under the public path.
        std::fs::write(site.join("_headers"), files::headers("/app/", &hashed)).unwrap();
        assert_eq!(
            headers_check(site, &hashed, "/app/").status,
            crate::error::Status::Pass
        );
        assert_eq!(
            headers_check(site, &hashed, "/").status,
            crate::error::Status::Fail
        );

        // A module edited after the release no longer matches its name.
        std::fs::write(site.join(&hashed.wasm), b"\0asm\x01\0\0\0edited").unwrap();
        let (check, _) = hashed_check(site);
        assert_eq!(check.status, crate::error::Status::Fail);
        assert!(check.error.detail.contains("changed after the release"));
    }

    #[test]
    fn sizes_are_held_to_the_budget() {
        let wasm = Path::new("/s/pkg/app_bg-0123abcd.wasm");
        assert_eq!(
            size_check(wasm, 3_000_000, 1_000_000, 4096).status,
            crate::error::Status::Pass
        );
        let over = size_check(wasm, 9_000_000, 5_000_000, 4096);
        assert_eq!(over.status, crate::error::Status::Fail);
        assert!(over.error.detail.contains("app_bg-0123abcd.wasm"));
    }
}
