//! End-to-end tests of the web pipeline: `icm run web`, `ps`, `input`,
//! `logs`, `shot`, replacement and `stop`, against real headless Chrome.
//!
//! cargo and wasm-bindgen are fakes (`ICM_TOOL_CARGO`,
//! `ICM_TOOL_WASM_BINDGEN`): the fake wasm-bindgen writes a small
//! JavaScript "app" that speaks the `ICM_EVENT` protocol, draws a canvas
//! and logs the pointer and key events it receives (and an `api_token` its
//! URL carries, plain, as JSON and in the URL encodings a page has), so
//! the tests cover icm's server, session, DevTools pipe,
//! readiness, screenshots and input in seconds without compiling iced. The
//! Chrome tests skip (and say so) on a machine without Chrome or the wasm32
//! target.

use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[path = "support/secret.rs"]
mod secret;

const BIN: &str = env!("CARGO_BIN_EXE_icm");

const FAKE_CARGO: &str = r#"#!/bin/sh
# icm's tests: a fake cargo. `build` writes an empty wasm and reports it;
# `metadata --filter-platform` reports iced's features; the rest is cargo.
case "$1" in
build)
  out="$CARGO_TARGET_DIR/wasm32-unknown-unknown/debug"
  mkdir -p "$out"
  printf 'wasm' > "$out/fixture-app.wasm"
  echo "{\"reason\":\"compiler-artifact\",\"package_id\":\"fixture-app 0.3.0\",\"target\":{\"name\":\"fixture-app\",\"kind\":[\"bin\"]},\"filenames\":[\"$out/fixture-app.wasm\"],\"executable\":\"$out/fixture-app.wasm\",\"fresh\":false}"
  echo '{"reason":"build-finished","success":true}'
  exit 0
  ;;
metadata)
  for arg in "$@"; do
    if [ "$arg" = "--filter-platform" ]; then
      echo '{"packages":[{"name":"iced","id":"iced-test-id"}],"resolve":{"nodes":[{"id":"iced-test-id","features":["fira-sans","webgl","wgpu"]}]}}'
      exit 0
    fi
  done
  ;;
esac
exec cargo "$@"
"#;

const FAKE_WASM_BINDGEN: &str = r##"#!/bin/sh
# icm's tests: a fake wasm-bindgen that writes a JavaScript "app".
while [ $# -gt 0 ]; do
  case "$1" in
    --out-dir) out="$2"; shift ;;
  esac
  shift
done
mkdir -p "$out"
printf 'wasm' > "$out/app_bg.wasm"
cat > "$out/app.js" <<'EOF'
export default async function init() {
  const query = new URLSearchParams(location.search);
  const event = (json) => console.log("ICM_EVENT " + JSON.stringify(json));
  if (query.get("noready") === "1") return;
  event({v: 1, kind: "start", protocol: 1, framework: "test", pid: null, platform: "web", bridge: null});
  const token = query.get("api_token");
  if (token) {
    console.log("signed in with " + token);
    console.log(JSON.stringify({token}));
    console.log("GET https://api.example.com/v1?" + new URLSearchParams({token}) + "&uri=" + encodeURIComponent(token));
    console.log("lower " + encodeURIComponent(token).replace(/%[0-9A-F]{2}/g, (hex) => hex.toLowerCase()));
    console.log("INFO payload " + JSON.stringify({t: token}).replace(/\//g, "\\u002f"));
    console.warn("token " + token);
    event({v: 1, kind: "warning", code: "fixture.token", message: "token " + token});
  }
  if (query.get("panic") === "1") {
    const message = token ? "rejected token " + token : "test panic";
    event({v: 1, kind: "panic", message, location: "src/lib.rs:7:5", thread: "main"});
    return;
  }
  const canvas = document.createElement("canvas");
  canvas.width = innerWidth * devicePixelRatio;
  canvas.height = innerHeight * devicePixelRatio;
  canvas.style.width = "100%";
  canvas.style.height = "100%";
  canvas.tabIndex = 0;
  document.body.appendChild(canvas);
  const g = canvas.getContext("2d");
  g.fillStyle = "#ffffff"; g.fillRect(0, 0, canvas.width, canvas.height);
  g.fillStyle = "#3355ff"; g.fillRect(0, 0, canvas.width / 2, canvas.height / 2);
  g.fillStyle = "#ff3355"; g.fillRect(canvas.width / 2, canvas.height / 2, canvas.width / 2, canvas.height / 2);
  canvas.addEventListener("pointerdown", (e) =>
    console.log("tap at " + Math.round(e.clientX) + "," + Math.round(e.clientY) + " " + e.pointerType));
  canvas.addEventListener("keydown", (e) => console.log("key " + e.key));
  requestAnimationFrame(() => event({v: 1, kind: "ready", ms: 1,
    window: {size: [innerWidth, innerHeight], physical: [canvas.width, canvas.height], scale: devicePixelRatio},
    backend: "canvas2d", adapter: "none", api: "2d"}));
}
EOF
"##;

struct Sandbox {
    cache: tempfile::TempDir,
    project: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Sandbox {
        let cache = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        copy_dir(&fixtures().join("app"), project.path());
        // The app's lock pins wasm-bindgen, as every iced web app's does.
        let lock = project.path().join("Cargo.lock");
        let mut text = std::fs::read_to_string(&lock).unwrap();
        text.push_str("\n[[package]]\nname = \"wasm-bindgen\"\nversion = \"0.2.106\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n");
        std::fs::write(&lock, text).unwrap();

        for (name, body) in [
            ("fake-cargo", FAKE_CARGO),
            ("fake-wasm-bindgen", FAKE_WASM_BINDGEN),
        ] {
            let path = cache.path().join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Sandbox { cache, project }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        let _ = command
            .args(args)
            .current_dir(self.project.path())
            .env("ICM_CACHE_DIR", self.cache.path())
            .env("ICM_HOST_CONFIG", self.cache.path().join("no-host.toml"))
            .env("CARGO_TARGET_DIR", self.project.path().join("target"))
            .env("ICM_TOOL_CARGO", self.cache.path().join("fake-cargo"))
            .env(
                "ICM_TOOL_WASM_BINDGEN",
                self.cache.path().join("fake-wasm-bindgen"),
            )
            .stdin(Stdio::null());
        for var in [
            "ICM_JSON",
            "ICM_CONFIG",
            "ICM_TIMEOUT",
            "ICM_RUN_ID",
            "ICM_RUN_DIR",
            "ICM_RUN_ROOT",
            "ICM_DETACHED",
        ] {
            let _ = command.env_remove(var);
        }
        command
    }

    /// Runs icm with `--json -q` and returns the result object.
    fn result(&self, args: &[&str]) -> Value {
        self.result_with(args, &[])
    }

    /// [`Sandbox::result`] with more variables in icm's environment.
    fn result_with(&self, args: &[&str], env: &[(&str, &str)]) -> Value {
        let mut full = args.to_vec();
        full.extend(["--json", "-q"]);
        let output: Output = self
            .command(&full)
            .envs(env.iter().copied())
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let result: Value = serde_json::from_str(text.trim()).unwrap_or_else(|error| {
            panic!(
                "{args:?}: not one JSON line ({error}): {text}\nstderr: {}",
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

    fn path(&self, value: &Value) -> PathBuf {
        let path = PathBuf::from(value.as_str().expect("a path"));
        if path.is_absolute() {
            path
        } else {
            self.project.path().join(path)
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // Never leave a Chrome behind, whatever failed.
        let _ = self.command(&["stop", "--all"]).output();
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

fn process_exists(pid: i64) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
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

#[test]
fn run_web_drives_headless_chrome() {
    if let Some(reason) = skip_reason() {
        eprintln!("skipped: {reason}");
        return;
    }
    let sandbox = Sandbox::new();

    // run: build (fakes), site, session, ready, screenshot.
    let run = sandbox.result(&["run", "web", "--port", "0", "--settle", "200ms"]);
    assert_eq!(run["exit"], 0, "{run}");
    assert_eq!(run["process"]["ready"]["source"], "icm_event");
    assert!(run["process"]["pid"].as_i64().unwrap() > 0);
    let url = run["artifacts"]["url"].as_str().unwrap();
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    assert!(!url.contains("icm_cdp"), "{url}");
    assert!(sandbox.path(&run["artifacts"]["preview"]).is_file());
    assert_eq!(run["screen"]["px"], serde_json::json!([1024, 768]));
    let warnings: Vec<&str> = run["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["id"].as_str())
        .collect();
    assert!(!warnings.contains(&"run.screen_blank"), "{run}");
    assert!(
        run["checks"]["pass"].as_u64().unwrap() >= 4,
        "{}",
        run["checks"]
    );
    let site = sandbox.path(&run["artifacts"]["site"]);
    let index = std::fs::read_to_string(site.join("index.html")).unwrap();
    assert!(index.contains("<title>Fixture</title>"), "{index}");
    let first_pid = run["process"]["pid"].as_i64().unwrap();

    // The server: .wasm is application/wasm, paths cannot escape.
    let port: u16 = url
        .trim_start_matches("http://127.0.0.1:")
        .split('/')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let head = |path: &str| -> String {
        let output = Command::new("curl")
            .args([
                "-s",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code} %{content_type}",
            ])
            .arg(format!("http://127.0.0.1:{port}{path}"))
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    assert_eq!(head("/pkg/app_bg.wasm"), "200 application/wasm");
    assert!(head("/%2e%2e/Cargo.toml").starts_with("400"));

    // ps lists it.
    let ps = sandbox.result(&["ps"]);
    assert_eq!(ps["sessions"][0]["platform"], "web");
    assert_eq!(ps["sessions"][0]["running"], true, "{ps}");

    // Input in preview pixels reaches the page (a desktop page: a mouse).
    let tap = sandbox.result(&["input", "web", "tap", "100", "50"]);
    assert_eq!(tap["exit"], 0, "{tap}");
    assert_eq!(tap["input"]["pointer"], "mouse");
    let text = sandbox.result(&["input", "web", "text", "hi"]);
    assert_eq!(text["exit"], 0, "{text}");
    let key = sandbox.result(&["input", "web", "key", "enter"]);
    assert_eq!(key["exit"], 0, "{key}");
    let outside = sandbox.result(&["input", "web", "tap", "5000", "5"]);
    assert_eq!(outside["exit"], 2);
    let home = sandbox.result(&["input", "web", "key", "home"]);
    assert_eq!(home["errors"][0]["id"], "input.unsupported");

    // logs re-read the live console.
    let logs = sandbox.result(&["logs", "web", "--grep", "tap at"]);
    let messages: Vec<&str> = logs["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["msg"].as_str())
        .collect();
    assert_eq!(messages, ["tap at 100,50 mouse"], "{logs}");
    let keys = sandbox.result(&["logs", "web", "--grep", "key "]);
    let keys: Vec<&str> = keys["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["msg"].as_str())
        .collect();
    assert_eq!(keys, ["key h", "key i", "key Enter"]);
    let quiet = sandbox.result(&["logs", "web", "--level", "error"]);
    assert_eq!(quiet["records"].as_array().unwrap().len(), 0, "{quiet}");

    // shot captures again.
    let shot = sandbox.result(&["shot", "web"]);
    assert_eq!(shot["exit"], 0, "{shot}");
    assert!(sandbox.path(&shot["artifacts"]["screenshot"]).is_file());

    // A second run replaces the session (Appendix C item 13): a phone this
    // time, with touch input and the preview coordinate space.
    let again = sandbox.result(&[
        "run",
        "web",
        "--no-build",
        "--port",
        "0",
        "--viewport",
        "iphone-17",
        "--settle",
        "200ms",
    ]);
    assert_eq!(again["exit"], 0, "{again}");
    let second_pid = again["process"]["pid"].as_i64().unwrap();
    assert_ne!(first_pid, second_pid);
    assert!(!process_exists(first_pid), "the old session still runs");
    assert_eq!(again["screen"]["px"], serde_json::json!([1206, 2622]));
    assert_eq!(again["screen"]["preview"], serde_json::json!([471, 1024]));
    let tap = sandbox.result(&["input", "web", "tap", "470", "1023"]);
    assert_eq!(tap["input"]["pointer"], "touch", "{tap}");
    let logs = sandbox.result(&["logs", "web", "--grep", "tap at"]);
    assert_eq!(logs["records"][0]["msg"], "tap at 401,873 touch", "{logs}");

    // stop ends it and Chrome with it.
    let stop = sandbox.result(&["stop", "web"]);
    assert_eq!(stop["exit"], 0, "{stop}");
    assert_eq!(stop["stopped"][0]["stopped"], "terminated");
    assert!(!process_exists(second_pid));
    let ps = sandbox.result(&["ps"]);
    assert_eq!(ps["sessions"].as_array().unwrap().len(), 0);
    let stop = sandbox.result(&["stop", "--all"]);
    assert_eq!(stop["exit"], 0);
    assert!(
        stop["summary"]
            .as_str()
            .unwrap()
            .starts_with("nothing was running"),
        "{stop}"
    );

    // With the session gone, logs still read its console; shot cannot.
    let logs = sandbox.result(&["logs", "web"]);
    assert_eq!(logs["exit"], 0, "{logs}");
    assert_eq!(logs["logs"]["live"], false);
    let shot = sandbox.result(&["shot", "web"]);
    assert_eq!(shot["errors"][0]["id"], "run.no_session");
    assert_eq!(shot["exit"], 7);
}

#[test]
fn run_web_reports_panics_and_pages_that_never_draw() {
    if let Some(reason) = skip_reason() {
        eprintln!("skipped: {reason}");
        return;
    }
    let sandbox = Sandbox::new();

    let panic = sandbox.result(&["run", "web", "--port", "0", "--env", "PANIC=1"]);
    assert_eq!(panic["exit"], 10, "{panic}");
    assert_eq!(panic["errors"][0]["id"], "run.app_panicked");
    assert_eq!(
        panic["errors"][0]["detail"],
        "panicked at src/lib.rs:7:5: test panic"
    );
    let evidence = sandbox.path(&panic["errors"][0]["evidence"][0]["path"]);
    assert!(
        evidence.starts_with(sandbox.path(&panic["run_dir"])),
        "{evidence:?}"
    );
    assert!(evidence.is_file());
    assert_eq!(panic["process"]["alive"], false);

    let never = sandbox.result(&[
        "run",
        "web",
        "--no-build",
        "--port",
        "0",
        "--env",
        "NOREADY=1",
        "--wait-ready",
        "2s",
    ]);
    assert_eq!(never["exit"], 10, "{never}");
    assert_eq!(never["errors"][0]["id"], "run.not_ready");

    let stop = sandbox.result(&["stop", "web"]);
    assert_eq!(stop["exit"], 0);
}

/// What a command keeps in its run directory holds no secret the page
/// logged: the value of a secret-named variable in icm's environment,
/// handed to the page with `--env` (so percent-encoded in its URL) and
/// logged plain, as JSON, in an `ICM_EVENT` warning and in a panic, is
/// `<redacted>` in the console's copy, `logs.ndjson`, `app.log`, events
/// and results, raw or escaped. The session's live console in
/// `target/icm/sessions/web` is the page's own output and keeps it.
#[test]
fn run_directories_keep_no_secret() {
    if let Some(reason) = skip_reason() {
        eprintln!("skipped: {reason}");
        return;
    }
    let sandbox = Sandbox::new();
    let env = [(secret::NAME, secret::TOKEN)];
    let pair = format!("API_TOKEN={}", secret::TOKEN);
    let icm = sandbox.project.path().join("target/icm");

    let run = sandbox.result_with(
        &[
            "run", "web", "--port", "0", "--settle", "200ms", "--env", &pair,
        ],
        &env,
    );
    assert_eq!(run["exit"], 0, "{run}");
    assert!(
        run["artifacts"]["url"]
            .as_str()
            .unwrap()
            .ends_with("api_token=<redacted>"),
        "{run}"
    );
    let app_log = std::fs::read_to_string(sandbox.path(&run["artifacts"]["app_log"])).unwrap();
    assert!(app_log.contains("signed in with <redacted>"), "{app_log}");
    assert!(app_log.contains("{\"token\":\"<redacted>\"}"), "{app_log}");
    assert!(secret::holds(&icm.join("sessions/web/console.ndjson")));

    let logs = sandbox.result_with(&["logs", "web", "--grep", "signed in|token"], &env);
    assert_eq!(logs["exit"], 0, "{logs}");
    assert_eq!(
        logs["records"][0]["msg"], "signed in with <redacted>",
        "{logs}"
    );
    let logs_file = std::fs::read_to_string(sandbox.path(&logs["artifacts"]["logs"])).unwrap();
    assert!(logs_file.contains("token <redacted>"), "{logs_file}");

    let panic = sandbox.result_with(
        &[
            "run",
            "web",
            "--no-build",
            "--port",
            "0",
            "--env",
            "PANIC=1",
            "--env",
            &pair,
        ],
        &env,
    );
    assert_eq!(panic["exit"], 10, "{panic}");
    assert_eq!(
        panic["errors"][0]["detail"],
        "panicked at src/lib.rs:7:5: rejected token <redacted>"
    );
    assert_eq!(sandbox.result_with(&["stop", "web"], &env)["exit"], 0);

    secret::assert_kept_nowhere(&icm.join("runs"));
    let last = std::fs::read_to_string(icm.join("last.json")).unwrap();
    assert!(
        secret::forms()
            .iter()
            .all(|form| !last.contains(form.as_str()))
    );
}

/// A later command whose environment lacks the secret (`icm logs web`,
/// `shot`, `stop` from another shell after `icm run web --env
/// API_TOKEN=…`) still redacts what the page logged and the URL that
/// carries it: the session keeps the secret values of the page's query in
/// a 0600 file next to its live files, and every command of the project
/// reads them.
#[test]
fn later_commands_without_the_secret_keep_none() {
    use std::os::unix::fs::PermissionsExt;
    if let Some(reason) = skip_reason() {
        eprintln!("skipped: {reason}");
        return;
    }
    let sandbox = Sandbox::new();
    let pair = format!("API_TOKEN={}", secret::TOKEN);
    let icm = sandbox.project.path().join("target/icm");

    let run = sandbox.result(&[
        "run", "web", "--port", "0", "--settle", "200ms", "--env", &pair,
    ]);
    assert_eq!(run["exit"], 0, "{run}");
    assert!(secret::holds(&icm.join("sessions/web/console.ndjson")));
    let kept = icm.join("sessions/web/secrets.json");
    let mode = std::fs::metadata(&kept).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);

    let logs = sandbox.result(&["logs", "web", "--grep", "signed in"]);
    assert_eq!(logs["exit"], 0, "{logs}");
    assert_eq!(
        logs["records"][0]["msg"], "signed in with <redacted>",
        "{logs}"
    );
    for args in [
        &["logs", "web", "--raw"][..],
        &["shot", "web"],
        &["ps"],
        &["stop", "web"],
        &["logs", "web"],
    ] {
        let result = sandbox.result(args);
        assert_eq!(result["exit"], 0, "{args:?}: {result}");
    }

    secret::assert_kept_nowhere(&icm.join("runs"));
    secret::assert_kept_nowhere(&icm.join("latest"));
    let last = std::fs::read_to_string(icm.join("last.json")).unwrap();
    assert!(
        secret::forms()
            .iter()
            .all(|form| !last.contains(form.as_str())),
        "{last}"
    );
}

#[test]
fn web_commands_without_a_session() {
    let sandbox = Sandbox::new();
    for args in [
        vec!["shot", "web"],
        vec!["input", "web", "tap", "1", "1"],
        vec!["logs", "web"],
    ] {
        let result = sandbox.result(&args);
        assert_eq!(result["exit"], 7, "{args:?}: {result}");
        assert_eq!(result["errors"][0]["id"], "run.no_session", "{args:?}");
        assert!(
            result["errors"][0]["fix"]["commands"][0]
                .as_str()
                .unwrap()
                .contains("icm run web")
        );
    }
    let stop = sandbox.result(&["stop", "web"]);
    assert_eq!(stop["exit"], 0);
    assert!(
        stop["summary"]
            .as_str()
            .unwrap()
            .starts_with("nothing was running"),
        "{stop}"
    );
    let ps = sandbox.result(&["ps"]);
    assert_eq!(ps["exit"], 0);
    assert_eq!(ps["sessions"].as_array().unwrap().len(), 0);
    let bad = sandbox.result(&["run", "web", "--viewport", "tablet"]);
    assert_eq!(bad["exit"], 2);
    assert!(
        bad["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("iphone-17")
    );
    let session = sandbox.command(&["__session", "web"]).output().unwrap();
    assert_eq!(session.status.code(), Some(2));
}
