//! End-to-end tests of `icm release web` and `icm verify web` (design
//! §11.3, §12.4) on `tests/fixtures/web-release`.
//!
//! cargo's build, wasm-bindgen and wasm-opt are fakes (`ICM_TOOL_CARGO`,
//! `ICM_TOOL_WASM_BINDGEN`, `ICM_TOOL_WASM_OPT`): the fake wasm-bindgen
//! writes a small JavaScript "app" that fetches its `.wasm`, draws a canvas
//! and speaks the `ICM_EVENT` protocol, so the site generation, the gates,
//! the serve check in real headless Chrome, `artifacts.json`, `UPLOAD.md`
//! and `icm verify web` (on the files, and on a "deployed" copy served by a
//! test server with a right and a wrong `.wasm` type) all run in seconds.
//! cargo metadata (the notices, iced's features) and `rustc --print cfg`
//! are real. The Chrome tests skip (and say so) without Chrome or the wasm32
//! target. Nothing is downloaded or deployed.

use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[path = "support/secret.rs"]
mod secret;

const BIN: &str = env!("CARGO_BIN_EXE_icm");

const FAKE_CARGO: &str = r#"#!/bin/sh
# icm's tests: cargo for the web release. `build` writes a stand-in .wasm and
# reports it; everything else (metadata for the notices and for iced's
# features) is the real cargo on the fixture's path packages.
case "$1" in
build)
  echo "$*" >> "$(dirname "$0")/cargo-build.args"
  out="$CARGO_TARGET_DIR/wasm32-unknown-unknown/icm-web"
  mkdir -p "$out"
  printf 'wasm' > "$out/web-app.wasm"
  echo "{\"reason\":\"compiler-artifact\",\"package_id\":\"web-app 0.2.0\",\"target\":{\"name\":\"web-app\",\"kind\":[\"bin\"]},\"filenames\":[\"$out/web-app.wasm\"],\"executable\":\"$out/web-app.wasm\",\"fresh\":false}"
  echo '{"reason":"build-finished","success":true}'
  exit 0
  ;;
esac
exec cargo "$@"
"#;

const FAKE_WASM_BINDGEN: &str = r##"#!/bin/sh
# icm's tests: a fake wasm-bindgen that writes a JavaScript "app" and a
# module of $FAKE_WASM_KB random kilobytes (incompressible: the size gate).
while [ $# -gt 0 ]; do
  case "$1" in
    --out-dir) out="$2"; shift ;;
  esac
  shift
done
mkdir -p "$out"
printf '\000asm\001\000\000\000' > "$out/app_bg.wasm"
head -c "$(( ${FAKE_WASM_KB:-1} * 1024 ))" /dev/urandom >> "$out/app_bg.wasm"
# An app built with a secret (ICM_TEST_API_TOKEN), which it logs.
token=$(printf '%s' "${ICM_TEST_API_TOKEN:-}" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')
printf 'const token = "%s";\n' "$token" > "$out/app.js"
cat >> "$out/app.js" <<'EOF'
export default async function init(options) {
  const event = (json) => console.log("ICM_EVENT " + JSON.stringify(json));
  const response = await fetch(options.module_or_path);
  await response.arrayBuffer();
  event({v: 1, kind: "start", protocol: 1, framework: "test", pid: null, platform: "web", bridge: null});
  if (token) {
    console.log("signed in with " + token);
    console.log(JSON.stringify({token}));
  }
  const canvas = document.createElement("canvas");
  canvas.width = innerWidth * devicePixelRatio;
  canvas.height = innerHeight * devicePixelRatio;
  canvas.style.width = "100%";
  canvas.style.height = "100%";
  document.body.appendChild(canvas);
  const g = canvas.getContext("2d");
  g.fillStyle = "#ffffff"; g.fillRect(0, 0, canvas.width, canvas.height);
  g.fillStyle = "#3355ff"; g.fillRect(0, 0, canvas.width / 2, canvas.height / 2);
  requestAnimationFrame(() => event({v: 1, kind: "ready", ms: 1,
    window: {size: [innerWidth, innerHeight], physical: [canvas.width, canvas.height], scale: devicePixelRatio},
    backend: "canvas2d", adapter: "none", api: "2d"}));
}
EOF
"##;

const FAKE_WASM_OPT: &str = r#"#!/bin/sh
# icm's tests: a fake wasm-opt that records its arguments and copies.
case "$1" in
--version) echo "wasm-opt version 133 (version_133)"; exit 0 ;;
--help)
  for f in sign-ext mutable-globals bulk-memory bulk-memory-opt nontrapping-float-to-int \
    reference-types multivalue simd relaxed-simd tail-call extended-const call-indirect-overlong; do
    echo "  --enable-$f   enable $f"
  done
  exit 0 ;;
esac
echo "$*" > "$(dirname "$0")/wasm-opt.args"
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift ;;
    -*) ;;
    *) in="$1" ;;
  esac
  shift
done
cp "$in" "$out"
"#;

const SCRUB: &[&str] = &[
    "ICM_JSON",
    "ICM_CONFIG",
    "ICM_TIMEOUT",
    "ICM_RUN_ID",
    "ICM_RUN_DIR",
    "ICM_RUN_ROOT",
    "ICM_DETACHED",
    "ICM_TOOLS_TOML",
    "ICM_TOOL_WASM_OPT",
    "WEB_URL",
    "WEB_DEPLOY_TARGET",
];

/// A copy of the fixture with its own cache, target dir and fake tools.
struct App {
    root: tempfile::TempDir,
    env: Vec<(String, String)>,
}

impl App {
    fn new() -> App {
        let root = tempfile::tempdir().unwrap();
        copy_dir(&fixtures().join("web-release"), &root.path().join("app"));
        let tools = root.path().join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        for (name, body) in [
            ("cargo", FAKE_CARGO),
            ("wasm-bindgen", FAKE_WASM_BINDGEN),
            ("wasm-opt", FAKE_WASM_OPT),
        ] {
            let path = tools.join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let tool = |name: &str| tools.join(name).display().to_string();
        App {
            env: vec![
                ("ICM_TODAY".into(), "2026-10-07".into()),
                ("ICM_TOOL_CARGO".into(), tool("cargo")),
                ("ICM_TOOL_WASM_BINDGEN".into(), tool("wasm-bindgen")),
                ("ICM_TOOL_WASM_OPT".into(), tool("wasm-opt")),
            ],
            root,
        }
    }

    fn dir(&self) -> PathBuf {
        self.root.path().join("app")
    }

    fn tools(&self) -> PathBuf {
        self.root.path().join("tools")
    }

    fn set(&mut self, key: &str, value: &str) {
        self.env.retain(|(k, _)| k != key);
        self.env.push((key.into(), value.into()));
    }

    fn unset(&mut self, key: &str) {
        self.env.retain(|(k, _)| k != key);
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        let _ = command
            .args(args)
            .current_dir(self.dir())
            .env("ICM_CACHE_DIR", self.root.path().join("cache"))
            .env("ICM_HOST_CONFIG", self.root.path().join("host.toml"))
            .env("CARGO_TARGET_DIR", self.dir().join("target"))
            .stdin(Stdio::null());
        for var in SCRUB {
            let _ = command.env_remove(var);
        }
        for (key, value) in &self.env {
            let _ = command.env(key, value);
        }
        command
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut full: Vec<&str> = args.to_vec();
        full.extend(["--json", "-q"]);
        result(&self.command(&full).output().unwrap())
    }

    /// A result field holding a path, absolute.
    fn abs(&self, value: &Value) -> PathBuf {
        let path = PathBuf::from(
            value
                .as_str()
                .unwrap_or_else(|| panic!("not a path: {value}")),
        );
        if path.is_absolute() {
            path
        } else {
            self.dir().join(path)
        }
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            let _ = std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn result(output: &Output) -> Value {
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let result: Value = serde_json::from_str(text.trim()).unwrap_or_else(|error| {
        panic!(
            "not one JSON line ({error}): {text}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(
        output.status.code().map(i64::from),
        result["exit"].as_i64(),
        "{result}"
    );
    result
}

/// The ids of the checks a run reported with `status`, from its events.
fn checks(app: &App, result: &Value, status: &str) -> Vec<String> {
    let dir = app.abs(&result["run_dir"]);
    let events = std::fs::read_to_string(dir.join("events.ndjson")).unwrap();
    events
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event["type"] == "check" && event["status"] == status)
        .filter_map(|event| event["id"].as_str().map(str::to_string))
        .collect()
}

fn failed(result: &Value) -> Vec<String> {
    result["checks"]["failed"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|id| id.as_str().map(str::to_string))
        .collect()
}

/// Why the Chrome tests cannot run here, if they cannot.
fn skip_reason() -> Option<String> {
    let chrome = std::env::var_os("ICM_CHROME")
        .map(PathBuf::from)
        .into_iter()
        .chain([
            PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            PathBuf::from("/usr/bin/google-chrome"),
            PathBuf::from("/usr/bin/chromium"),
        ])
        .any(|path| path.is_file());
    if !chrome {
        return Some("no Chrome".into());
    }
    wasm32_missing()
}

fn wasm32_missing() -> Option<String> {
    let sysroot = Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())?;
    if !Path::new(&sysroot)
        .join("lib/rustlib/wasm32-unknown-unknown")
        .is_dir()
    {
        return Some("no wasm32-unknown-unknown target".into());
    }
    None
}

/// A stand-in for the owner's host: serves `site` under `/app/` with the
/// given type for `.wasm`, until the test ends.
fn serve(site: PathBuf, wasm_type: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let site = site.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    return;
                }
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                        break;
                    }
                }
                let path = line.split_whitespace().nth(1).unwrap_or("/");
                let path = path.split('?').next().unwrap_or("/");
                let relative = path.strip_prefix("/app/").unwrap_or("");
                let relative = if relative.is_empty() {
                    "index.html"
                } else {
                    relative
                };
                let file = site.join(relative);
                let (status, body) = match std::fs::read(&file) {
                    Ok(body) if !relative.contains("..") => ("200 OK", body),
                    _ => ("404 Not Found", b"not found".to_vec()),
                };
                let kind = match file.extension().and_then(|e| e.to_str()) {
                    Some("html") => "text/html",
                    Some("js") => "text/javascript",
                    Some("wasm") => wasm_type,
                    Some("png") => "image/png",
                    Some("webmanifest") => "application/manifest+json",
                    _ => "application/octet-stream",
                };
                let mut stream = stream;
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            });
        }
    });
    port
}

/// The `--enable-*` flags the fake wasm-opt should have been given: one per
/// target feature rustc reports for wasm32, where the fake lists a flag.
fn expected_flags(app: &App) -> Vec<String> {
    let cfg = Command::new("rustc")
        .args(["--print", "cfg", "--target", "wasm32-unknown-unknown"])
        .current_dir(app.dir())
        .output()
        .unwrap();
    let help = Command::new(app.tools().join("wasm-opt"))
        .arg("--help")
        .output()
        .unwrap();
    icm::web::release_site::wasm_opt_flags(
        &String::from_utf8_lossy(&cfg.stdout),
        &String::from_utf8_lossy(&help.stdout),
    )
    .0
}

#[test]
fn release_web_builds_gates_serves_and_verifies_the_site() {
    if let Some(reason) = skip_reason() {
        eprintln!("skipped: {reason}");
        return;
    }
    let app = App::new();
    let release = app.json(&["release", "web", "--allow-dirty"]);
    assert_eq!(release["exit"], 0, "{release}");
    assert_eq!(release["release"]["uploadable"], true, "{release}");

    // The build: the release profile from --config, locked, and wasm-opt
    // with exactly the features rustc compiles for.
    let cargo = std::fs::read_to_string(app.tools().join("cargo-build.args")).unwrap();
    assert!(cargo.contains("--profile icm-web"), "{cargo}");
    assert!(cargo.contains("--locked"), "{cargo}");
    assert!(cargo.contains("--target wasm32-unknown-unknown"), "{cargo}");
    let opt = std::fs::read_to_string(app.tools().join("wasm-opt.args")).unwrap();
    assert!(opt.starts_with("-Oz "), "{opt}");
    let flags: Vec<&str> = opt
        .split_whitespace()
        .filter(|w| w.starts_with("--enable-"))
        .collect();
    assert_eq!(flags, expected_flags(&app), "{opt}");
    assert!(!flags.is_empty());

    // The site.
    let site = app.abs(&release["artifacts"]["site"]);
    let index = std::fs::read_to_string(site.join("index.html")).unwrap();
    assert!(index.contains("<base href=\"/app/\">"), "{index}");
    assert!(index.contains("module_or_path: \"./pkg/app_bg-"), "{index}");
    assert!(!index.contains("/__icm/log"), "no dev forwarder");
    assert_eq!(
        std::fs::read_to_string(site.join("404.html")).unwrap(),
        index
    );
    let hashed = icm::web::release_site::parse_index(&index).unwrap();
    for name in [&hashed.js, &hashed.wasm] {
        let hash = icm::web::release_site::hash_in_name(name).unwrap();
        let sha = icm::hash::sha256_file(&site.join(name)).unwrap();
        assert!(sha.starts_with(hash), "{name}");
    }
    let headers = std::fs::read_to_string(site.join("_headers")).unwrap();
    assert!(
        headers.contains(&format!(
            "/app/{}\n  Content-Type: application/wasm",
            hashed.wasm
        )),
        "{headers}"
    );
    for file in [
        ".nojekyll",
        "manifest.webmanifest",
        "icon-32.png",
        "icon-180.png",
        "icon-192.png",
        "icon-512.png",
        "icon-maskable-512.png",
        "fonts/Fixture.ttf",
        "THIRD_PARTY_NOTICES.txt",
    ] {
        assert!(site.join(file).is_file(), "{file}");
    }
    let notices = std::fs::read_to_string(site.join("THIRD_PARTY_NOTICES.txt")).unwrap();
    assert!(notices.contains("Fira Sans"), "{notices}");
    assert!(notices.contains("wasm-bindgen"), "{notices}");
    let zip = app.abs(&release["artifacts"]["site_zip"]);
    let names = icm::release::notices::zip_names(&zip).unwrap();
    assert!(names.contains(&"index.html".to_string()), "{names:?}");
    assert!(names.contains(&hashed.wasm), "{names:?}");
    let dist = app.abs(&release["artifacts"]["dist"]);
    for file in [
        "hosting/nginx.conf",
        "hosting/apache.htaccess",
        "hosting/Caddyfile",
    ] {
        let text = std::fs::read_to_string(dist.join(file)).unwrap();
        assert!(text.contains(&hashed.wasm), "{file}: {text}");
    }
    let size: Value =
        serde_json::from_str(&std::fs::read_to_string(dist.join("size.json")).unwrap()).unwrap();
    assert_eq!(size["wasm"]["path"], Value::String(hashed.wasm.clone()));
    assert!(size["wasm"]["gzip"].as_u64().unwrap() > 0);
    assert_eq!(release["size"]["wasm"]["budget_kb"], 64);

    // The gates, the serve check among them.
    let passed = checks(&app, &release, "pass");
    for id in [
        "web.hashed_assets",
        "web.mime",
        "web.size_budget",
        "web.fonts_embedded",
        "web.renderer_fallback",
        "web.serve_smoke",
        "release.notices",
        "deps.wasm_bindgen_cli",
    ] {
        assert!(passed.contains(&id.to_string()), "{id} in {passed:?}");
    }
    assert_eq!(release["smoke"]["status"], "ready", "{release}");
    assert_eq!(release["smoke"]["ready"]["source"], "icm_event");
    assert_eq!(release["smoke"]["blank"], false);
    assert_eq!(release["smoke"]["wasm"][0]["mime"], "application/wasm");
    assert!(
        release["smoke"]["url"]
            .as_str()
            .unwrap()
            .ends_with("/app/?icm_events=1")
    );
    assert!(app.abs(&release["artifacts"]["screenshot"]).is_file());

    // What the owner runs, never icm.
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(app.abs(&release["artifacts"]["manifest"])).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["files"][1]["kind"], "site", "{manifest}");
    assert_eq!(manifest["signing"]["kind"], "none");
    assert_eq!(manifest["tools"]["wasm-opt"], "version 133 (version_133)");
    let upload_md = std::fs::read_to_string(app.abs(&release["artifacts"]["upload_md"])).unwrap();
    assert!(upload_md.contains("rsync -av --delete"), "{upload_md}");
    assert!(upload_md.contains("hosting/"), "{upload_md}");
    let upload_sh = app.abs(&release["artifacts"]["upload_sh"]);
    let refused = Command::new("bash")
        .arg(&upload_sh)
        .env_remove("WEB_DEPLOY_TARGET")
        .output()
        .unwrap();
    assert_eq!(
        refused.status.code(),
        Some(9),
        "upload.sh needs its variables"
    );

    // verify: the same gates on the files, and the serve check again.
    let verify = app.json(&["verify", "web"]);
    assert_eq!(verify["exit"], 0, "{verify}");
    assert_eq!(verify["smoke"]["status"], "ready", "{verify}");
    let verified = checks(&app, &verify, "pass");
    assert!(verified.contains(&"web.serve_smoke".to_string()));
    assert!(verified.contains(&"web.hashed_assets".to_string()));
    let smoke_dir = app.abs(&verify["run_dir"]).join("smoke");
    assert!(smoke_dir.join("console.ndjson").is_file());
    assert!(
        !smoke_dir.join("chrome-profile").exists(),
        "the profile is removed"
    );

    // verify --url: a "deployed" copy that serves .wasm right, then wrong.
    let good = serve(site.clone(), "application/wasm");
    let deployed = app.json(&[
        "verify",
        "web",
        "--url",
        &format!("http://127.0.0.1:{good}/app/"),
    ]);
    assert_eq!(deployed["exit"], 0, "{deployed}");
    assert!(checks(&app, &deployed, "pass").contains(&"web.mime".to_string()));
    let bad = serve(site.clone(), "application/octet-stream");
    let wrong = app.json(&[
        "verify",
        "web",
        "--url",
        &format!("http://127.0.0.1:{bad}/app/"),
    ]);
    assert_eq!(wrong["exit"], 1, "{wrong}");
    assert_eq!(failed(&wrong), ["web.mime"], "{wrong}");
    assert!(
        wrong["errors"][0]["fix"]["summary"]
            .as_str()
            .unwrap()
            .contains("_headers"),
        "{wrong}"
    );

    // A site changed after the release fails verify.
    std::fs::write(site.join(&hashed.wasm), b"\0asm\x01\0\0\0edited").unwrap();
    let tampered = app.json(&["verify", "web"]);
    assert_eq!(tampered["exit"], 1, "{tampered}");
    let tampered = failed(&tampered);
    assert!(
        tampered.contains(&"web.hashed_assets".to_string()),
        "{tampered:?}"
    );
    assert!(
        tampered.contains(&"release.artifact_changed".to_string()),
        "{tampered:?}"
    );
}

#[test]
fn a_web_release_over_budget_and_without_fonts_is_not_uploadable() {
    if let Some(reason) = skip_reason() {
        eprintln!("skipped: {reason}");
        return;
    }
    let mut app = App::new();
    let config = app.dir().join("icm.toml");
    let text = std::fs::read_to_string(&config)
        .unwrap()
        .replace("resources = [\"fonts/*.ttf\"]\n", "");
    std::fs::write(&config, text).unwrap();
    app.set("FAKE_WASM_KB", "200");
    let release = app.json(&["release", "web", "--allow-dirty"]);
    assert_eq!(release["exit"], 1, "{release}");
    assert_eq!(
        failed(&release),
        ["web.size_budget", "web.fonts_embedded"],
        "{release}"
    );
    assert_eq!(release["release"]["uploadable"], false);
    assert!(
        release["release"]["not_uploadable"]
            .as_str()
            .unwrap()
            .contains("web.size_budget"),
        "{release}"
    );
    // The serve check still ran, and passed.
    assert!(checks(&app, &release, "pass").contains(&"web.serve_smoke".to_string()));
}

/// The serve check's console in the run directory holds no secret the
/// page logged: an app built with the value of a secret-named variable in
/// icm's environment logs it plain and as JSON, and the console, Chrome's
/// log, the events and the result have `<redacted>`, raw or escaped (the
/// site itself, the release's own code, is in the dist directory).
#[test]
fn the_serve_check_keeps_no_secret() {
    if let Some(reason) = skip_reason() {
        eprintln!("skipped: {reason}");
        return;
    }
    let mut app = App::new();
    app.set(secret::NAME, secret::TOKEN);
    let release = app.json(&["release", "web", "--allow-dirty"]);
    assert_eq!(release["exit"], 0, "{release}");
    assert!(checks(&app, &release, "pass").contains(&"web.serve_smoke".to_string()));
    let run_dir = app.abs(&release["run_dir"]);
    let console = std::fs::read_to_string(run_dir.join("smoke/console.ndjson")).unwrap();
    assert!(console.contains("signed in with <redacted>"), "{console}");
    assert!(
        console.contains("{\\\"token\\\":\\\"<redacted>\\\"}"),
        "{console}"
    );
    secret::assert_kept_nowhere(&run_dir);
    assert!(secret::leaks(&app.abs(&release["artifacts"]["site"])).len() == 1);
}

#[test]
fn web_release_plans_and_preconditions() {
    if let Some(reason) = wasm32_missing() {
        eprintln!("skipped: {reason}");
        return;
    }
    let mut app = App::new();

    // --dry-run: the steps, nothing written.
    let plan = app.json(&["release", "web", "--dry-run"]);
    assert_eq!(plan["exit"], 0, "{plan}");
    let names: Vec<&str> = plan["plan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["name"].as_str().unwrap())
        .collect();
    for name in [
        "cargo.build",
        "wasm-bindgen",
        "rustc.cfg",
        "wasm-opt",
        "site.generate",
        "site.zip",
        "web.gates",
        "web.serve_smoke",
    ] {
        assert!(names.contains(&name), "{name} in {names:?}");
    }
    assert!(!app.dir().join("target/icm/dist").exists());

    // Without wasm-opt (and without --yes), the release stops before the
    // build and says how to install it.
    app.unset("ICM_TOOL_WASM_OPT");
    let missing = app.json(&["release", "web", "--allow-dirty"]);
    assert_eq!(missing["exit"], 4, "{missing}");
    assert_eq!(missing["errors"][0]["id"], "env.tool_missing");
    assert_eq!(
        missing["errors"][0]["fix"]["commands"][0],
        "icm doctor web --fix --yes"
    );
    assert!(!app.dir().join("target/icm/dist").exists());

    // Without a lock, nothing says which wasm-bindgen to use.
    std::fs::remove_file(app.dir().join("Cargo.lock")).unwrap();
    let unlocked = app.json(&["release", "web", "--allow-dirty"]);
    assert_eq!(unlocked["exit"], 4, "{unlocked}");
    assert_eq!(unlocked["errors"][0]["id"], "deps.wasm_bindgen_cli");
}

#[test]
fn verify_web_takes_sites_only() {
    let app = App::new();
    let not_a_site = app.json(&["verify", "web", "--artifact", "icm.toml"]);
    assert_eq!(not_a_site["exit"], 2, "{not_a_site}");
    assert_eq!(not_a_site["errors"][0]["id"], "usage.bad_args");
    let none = app.json(&["verify", "web"]);
    assert_eq!(none["errors"][0]["id"], "release.not_found", "{none}");
}
