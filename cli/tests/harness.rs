//! End-to-end tests of `icm test`, `icm shot --headless` and `icm ui
//! --headless` against a fake cargo (`ICM_TOOL_CARGO`) and a fake harness
//! that speak the real protocols: cargo's JSON messages and libtest output,
//! and harness protocol 1. `cargo metadata` still runs the real cargo.
//!
//! The real harness is exercised on the template (examples/app) by hand and
//! in the phase 1 acceptance script; these tests pin icm's side of the
//! contract without building iced.

use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_icm");

/// A project with a fake cargo and a fake harness.
struct Fake {
    _dir: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    env: Vec<(String, String)>,
}

const CARGO: &str = r#"#!/bin/sh
# A fake cargo: metadata goes to the real one; test builds report the fake
# harness as the icm test executable; test runs replay canned output.
if [ "$1" = metadata ]; then exec "$REAL_CARGO" "$@"; fi
echo "$*" >> "$FAKE_DIR/cargo.argv"
case "$*" in
  *--no-run*)
    if [ "$FAKE_BUILD" = error ]; then
      cat "$FAKE_DIR/compiler-error.json"
      echo 'error: could not compile `fixture-app` (lib) due to 1 previous error' >&2
      exit 101
    fi
    printf '{"reason":"compiler-artifact","package_id":"path+file:///p#fixture-app@0.3.0","target":{"name":"icm","kind":["test"]},"filenames":["%s"],"executable":"%s","fresh":true}\n' "$FAKE_DIR/harness" "$FAKE_DIR/harness"
    echo '{"reason":"build-finished","success":true}'
    exit 0 ;;
esac
cat "$FAKE_DIR/test.stdout"
cat "$FAKE_DIR/test.stderr" >&2
exit "${FAKE_TEST_EXIT:-0}"
"#;

const HARNESS: &str = r#"#!/bin/sh
# A fake app harness speaking protocol 1 (or $FAKE_PROTOCOL).
echo "$*" >> "$FAKE_DIR/harness.argv"
mode="${FAKE_MODE:-ok}"
if [ "$mode" = libtest ]; then
  echo "error: Unrecognized option: 'viewport'" >&2
  exit 101
fi
echo "ICM_HARNESS {\"protocol\":${FAKE_PROTOCOL:-1}}"
if [ "$mode" = panic ]; then
  echo "thread 'main' (7) panicked at src/lib.rs:2:5:" >&2
  echo "the view broke" >&2
  exit 101
fi
if [ "$mode" = nopreset ]; then
  echo 'ICM_HARNESS_RESULT {"protocol":1,"ok":false,"error":"the preset \"x\" does not exist (available presets: [\"empty\"])"}'
  exit 2
fi
cmd="$1"; shift
out=""; report=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out) out="$2"; shift ;;
    --report) report="$2"; shift ;;
  esac
  shift
done
case "$cmd" in
  icm-shot)
    cp "${FAKE_PNG:-$FAKE_DIR/screen.png}" "$out"
    echo 'ICM_HARNESS_RESULT {"protocol":1,"kind":"shot","ok":true,"size":[1206,2622],"viewport":[402,874],"scale":3,"theme":"light","backend":"tiny-skia"}' ;;
  icm-tree)
    cp "$FAKE_DIR/tree.json" "$out"
    echo "ICM_HARNESS_RESULT {\"protocol\":1,\"kind\":\"tree\",\"ok\":true,\"out\":\"$out\",\"widgets\":4}" ;;
  icm-ice)
    cp "$FAKE_DIR/ice.json" "$report"
    printf 'ICM_HARNESS_RESULT '
    cat "$FAKE_DIR/ice.json"
    exit 1 ;;
  *)
    echo 'ICM_HARNESS_RESULT {"protocol":1,"ok":false,"error":"unknown command"}'
    exit 2 ;;
esac
"#;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn executable(path: &Path, text: &str) {
    write(path, text);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn png(path: &Path, width: u32, height: u32, text_rows: bool) {
    let mut image = icm::raster::Image::filled(width, height, [255, 255, 255, 255]);
    if text_rows {
        for y in (20..height).step_by(40) {
            for x in 10..width - 10 {
                image.set_pixel(x, y, [0, 0, 0, 255]);
                image.set_pixel(x, y + 1, [0, 0, 0, 255]);
            }
        }
    }
    icm::raster::write(path, &image).unwrap();
}

impl Fake {
    fn new(with_target: bool) -> Fake {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let project = root.join("app");
        let test_target = if with_target {
            "\n[[test]]\nname = \"icm\"\npath = \"tests/icm.rs\"\nharness = false\n"
        } else {
            ""
        };
        write(
            &project.join("Cargo.toml"),
            &format!(
                "[package]\nname = \"fixture-app\"\nversion = \"0.3.0\"\nedition = \"2024\"\npublish = false\nautotests = false\n\n[lib]\npath = \"src/lib.rs\"\n{test_target}\n[workspace]\n"
            ),
        );
        write(
            &project.join("icm.toml"),
            "schema = 1\nmin_icm = \"0.14.1-mobile.1\"\n\n[app]\nname = \"Fixture\"\nid = \"com.acme.fixture\"\n\n[test]\nviewports = [\"iphone-17\", \"desktop\"]\n",
        );
        write(
            &project.join("src/lib.rs"),
            "pub fn view() {\n    todo!()\n}\n",
        );
        write(&project.join("tests/icm.rs"), "fn main() {}\n");
        write(
            &project.join("tests/flows/smoke.ice"),
            "viewport: 402x874\nmode: Immediate\n-----\nclick \"Increment\"\nexpect \"Count: 9\"\n",
        );

        let fake = root.join("fake");
        executable(&fake.join("cargo"), CARGO);
        executable(&fake.join("harness"), HARNESS);
        png(&fake.join("screen.png"), 1206, 2622, true);
        png(&fake.join("blank.png"), 1206, 2622, false);
        write(
            &fake.join("tree.json"),
            &json!({"protocol":1,"kind":"tree","ok":true,"viewport":[402,874],"widgets":[
                {"kind":"container","id":null,"bounds":[0,0,402,874],"visible":[0,0,402,874]},
                {"kind":"text","id":null,"text":"Count: 0","bounds":[16,22.8,251.776,31.2],"visible":[16,22.8,251.776,31.2]},
                {"kind":"text","id":null,"text":"Increment","bounds":[295.776,28,74.224,20.8],"visible":[295.776,28,74.224,20.8]},
                {"kind":"text_input","id":"new-item","text":"New item","focused":false,"bounds":[16,76.8,301.696,44.8],"visible":[16,76.8,301.696,44.8]}
            ]})
            .to_string(),
        );
        let flow = project.join("tests/flows/smoke.ice");
        write(
            &fake.join("ice.json"),
            &json!({"protocol":1,"kind":"ice","ok":false,"name":"flows::smoke","file":flow,"passed":false,"ms":5,"error":null,"steps":[
                {"line":4,"instruction":"click \"Increment\"","status":"passed","ms":1},
                {"line":5,"instruction":"expect \"Count: 9\"","status":"failed","ms":1,"reason":"no widget shows the text \"Count: 9\"","texts":["Count: 1","Increment"]}
            ]})
            .to_string(),
        );
        write(
            &fake.join("compiler-error.json"),
            &format!(
                "{}\n",
                json!({"reason":"compiler-message","package_id":"path+file:///p#fixture-app@0.3.0",
                    "target":{"name":"fixture-app","kind":["lib"]},
                    "message":{"level":"error","code":{"code":"E0308"},"message":"mismatched types",
                        "rendered":"error[E0308]: mismatched types\n --> src/lib.rs:2:5\n",
                        "spans":[{"file_name":"src/lib.rs","line_start":2,"column_start":5,"is_primary":true}]}})
            ),
        );
        write(&fake.join("test.stdout"), "");
        write(&fake.join("test.stderr"), "");

        let real_cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let env = vec![
            (
                "ICM_TOOL_CARGO".to_string(),
                fake.join("cargo").display().to_string(),
            ),
            ("REAL_CARGO".to_string(), real_cargo),
            ("FAKE_DIR".to_string(), fake.display().to_string()),
            (
                "ICM_CACHE_DIR".to_string(),
                root.join("cache").display().to_string(),
            ),
            (
                "ICM_HOST_CONFIG".to_string(),
                root.join("no-host.toml").display().to_string(),
            ),
            (
                "CARGO_TARGET_DIR".to_string(),
                project.join("target").display().to_string(),
            ),
        ];
        Fake {
            _dir: dir,
            root,
            project,
            env,
        }
    }

    fn fake(&self, name: &str) -> PathBuf {
        self.root.join("fake").join(name)
    }

    fn run_with(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let mut command = Command::new(BIN);
        let _ = command
            .args(args)
            .current_dir(&self.project)
            .stdin(Stdio::null());
        for var in [
            "ICM_JSON",
            "ICM_CONFIG",
            "ICM_TIMEOUT",
            "ICM_RUN_ID",
            "ICM_RUN_DIR",
            "ICM_RUN_ROOT",
            "ICM_DETACHED",
            "ICED_TEST_BACKEND",
            "FAKE_MODE",
            "FAKE_PROTOCOL",
            "FAKE_BUILD",
            "FAKE_PNG",
            "FAKE_TEST_EXIT",
        ] {
            let _ = command.env_remove(var);
        }
        for (key, value) in &self.env {
            let _ = command.env(key, value);
        }
        for (key, value) in extra {
            let _ = command.env(key, value);
        }
        command.output().unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with(args, &[])
    }

    fn argv(&self, tool: &str) -> String {
        std::fs::read_to_string(self.fake(&format!("{tool}.argv"))).unwrap_or_default()
    }

    fn path(&self, value: &Value) -> PathBuf {
        let path = PathBuf::from(value.as_str().expect("a path"));
        if path.is_absolute() {
            path
        } else {
            self.project.join(path)
        }
    }
}

/// The result line, after checking every line is NDJSON with the result
/// last.
fn final_result(output: &Output) -> Value {
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("bad line {line:?}: {e}")))
        .collect();
    let last = lines.last().cloned().unwrap_or_else(|| {
        panic!(
            "no output; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(last["type"], "result", "{last}");
    assert_eq!(
        output.status.code(),
        last["exit"].as_i64().map(|code| code as i32),
        "the exit code is the result's"
    );
    last
}

fn check<'a>(result: &'a Value, id: &str) -> Option<&'a Value> {
    result["errors"]
        .as_array()
        .into_iter()
        .chain(result["warnings"].as_array())
        .flatten()
        .find(|error| error["id"] == id)
}

#[test]
fn headless_shots_write_screenshots_and_previews() {
    let fake = Fake::new(true);
    let output = fake.run(&[
        "shot",
        "--headless",
        "--viewport",
        "iphone-17",
        "--viewport",
        "800x600@2",
        "--theme",
        "dark",
        "--json",
        "-q",
    ]);
    let result = final_result(&output);
    assert_eq!(result["exit"], 0, "{result:#}");
    assert_eq!(result["command"], "shot");
    assert_eq!(result["target"], "headless");
    let shots = result["shots"].as_array().unwrap();
    assert_eq!(shots.len(), 2);
    assert_eq!(shots[0]["label"], "iphone-17-dark");
    assert_eq!(shots[1]["label"], "800x600@2-dark");
    for shot in shots {
        let path = fake.path(&shot["path"]);
        let preview = fake.path(&shot["preview"]);
        assert!(path.is_file(), "{}", path.display());
        assert!(path.starts_with(fake.project.join("target/icm/host/shots")));
        let small = icm::raster::read(&preview).unwrap();
        assert_eq!((small.width, small.height), (471, 1024));
        assert_eq!(shot["blank"], false);
        assert_eq!(shot["screen"]["preview"], json!([471, 1024]));
    }
    assert!(result["artifacts"]["screenshot.iphone-17-dark"].is_string());
    assert!(result["artifacts"]["preview.800x600@2-dark"].is_string());
    assert!(
        result["screen"].is_null(),
        "several shots have no single screen"
    );

    let argv = fake.argv("harness");
    assert!(argv.contains("icm-shot --viewport iphone-17 --theme dark --wait-ms 500 --out "));
    assert!(argv.contains("icm-shot --viewport 800x600 --scale 2 --theme dark"));
    let cargo = fake.argv("cargo");
    assert!(
        cargo.contains("test --manifest-path")
            && cargo.contains("-p fixture-app --test icm --no-run --message-format=json"),
        "{cargo}"
    );

    // One viewport: the run's usual artifact names and its screen.
    let result = final_result(&fake.run(&["shot", "--headless", "--json", "-q"]));
    assert_eq!(result["exit"], 0);
    assert!(result["artifacts"]["preview"].is_string());
    assert_eq!(result["screen"]["px"], json!([1206, 2622]));
    assert_eq!(result["shots"][0]["viewport"], "iphone-17");

    // --all-viewports takes [test] viewports.
    let result =
        final_result(&fake.run(&["shot", "--headless", "--all-viewports", "--json", "-q"]));
    let labels: Vec<&str> = result["shots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|shot| shot["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["iphone-17-light", "desktop-light"]);
}

#[test]
fn blank_headless_shots_warn() {
    let fake = Fake::new(true);
    let blank = fake.fake("blank.png").display().to_string();
    let result = final_result(&fake.run_with(
        &["shot", "--headless", "--json", "-q"],
        &[("FAKE_PNG", blank.as_str())],
    ));
    assert_eq!(result["exit"], 0);
    let warning = check(&result, "run.screen_blank").expect("a blank warning");
    assert!(warning["detail"].as_str().unwrap().contains("#ffffff"));
    assert!(!warning["likely_causes"].as_array().unwrap().is_empty());

    let strict = final_result(&fake.run_with(
        &["shot", "--headless", "--strict", "--json", "-q"],
        &[("FAKE_PNG", blank.as_str())],
    ));
    assert_eq!(strict["exit"], 1);
}

#[test]
fn shot_usage_errors() {
    let fake = Fake::new(true);
    let result =
        final_result(&fake.run(&["shot", "--headless", "--viewport", "huge", "--json", "-q"]));
    assert_eq!(result["exit"], 2);
    assert!(
        result["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("iphone-17")
    );

    let output = fake.run(&[
        "shot",
        "--headless",
        "--viewport",
        "iphone-17",
        "--viewport",
        "desktop",
        "--out",
        "x.png",
        "--json",
        "-q",
    ]);
    assert_eq!(final_result(&output)["exit"], 2);

    let output = fake.run(&["shot", "android", "--headless", "--json", "-q"]);
    assert_eq!(final_result(&output)["exit"], 2);
}

#[test]
fn tree_and_find() {
    let fake = Fake::new(true);
    let result = final_result(&fake.run(&["ui", "--headless", "tree", "--json", "-q"]));
    assert_eq!(result["exit"], 0, "{result:#}");
    assert_eq!(result["tree"]["viewport"], "iphone-17");
    assert_eq!(result["tree"]["widgets"].as_array().unwrap().len(), 4);
    assert!(fake.path(&result["artifacts"]["tree"]).is_file());

    // Human mode prints the listing on stdout.
    let output = fake.run(&["ui", "--headless", "tree", "--viewport", "pixel-9"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(
        text.starts_with("viewport pixel-9 (412x915 logical px, scale 2.625)\n"),
        "{text}"
    );
    assert!(text.contains("text_input #new-item \"New item\"  at 16,76.8 301.7x44.8"));
    assert!(
        fake.argv("harness")
            .contains("icm-tree --viewport pixel-9 --wait-ms 500 --out ")
    );

    let found = final_result(&fake.run(&["ui", "--headless", "find", "Increment", "--json", "-q"]));
    assert_eq!(found["exit"], 0);
    assert_eq!(found["matches"][0]["center"], json!([332.9, 38.4]));

    let by_id = final_result(&fake.run(&["ui", "find", "#new-item", "--json", "-q"]));
    assert_eq!(by_id["matches"][0]["kind"], "text_input");

    let missing = final_result(&fake.run(&["ui", "--headless", "find", "Nope", "--json", "-q"]));
    assert_eq!(missing["exit"], 1);
    assert_eq!(missing["errors"][0]["id"], "ui.selector_not_found");
    assert!(
        missing["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("\"Count: 0\"")
    );
}

#[test]
fn a_failing_flow_points_at_its_line() {
    let fake = Fake::new(true);
    let result = final_result(&fake.run(&[
        "ui",
        "--headless",
        "ice",
        "tests/flows/smoke.ice",
        "--json",
        "-q",
    ]));
    assert_eq!(result["exit"], 1, "{result:#}");
    let error = &result["errors"][0];
    assert_eq!(error["id"], "test.failed");
    assert_eq!(error["evidence"][0]["line"], 5);
    assert!(
        error["evidence"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("tests/flows/smoke.ice")
    );
    assert!(
        error["detail"]
            .as_str()
            .unwrap()
            .contains("visible texts: \"Count: 1\"")
    );
    assert_eq!(result["flow"]["name"], "flows::smoke");

    let missing = final_result(&fake.run(&["ui", "--headless", "ice", "nope.ice", "--json", "-q"]));
    assert_eq!(missing["exit"], 2);
}

#[test]
fn harness_failures_have_their_own_exit_codes() {
    let fake = Fake::new(true);

    let panicked = final_result(&fake.run_with(
        &["shot", "--headless", "--json", "-q"],
        &[("FAKE_MODE", "panic")],
    ));
    assert_eq!(panicked["exit"], 10, "{panicked:#}");
    let error = &panicked["errors"][0];
    assert_eq!(error["id"], "run.app_panicked");
    assert!(error["detail"].as_str().unwrap().contains("the view broke"));
    assert_eq!(error["evidence"][0]["path"], "src/lib.rs");
    assert_eq!(error["evidence"][0]["line"], 2);

    let newer = final_result(&fake.run_with(
        &["ui", "--headless", "tree", "--json", "-q"],
        &[("FAKE_PROTOCOL", "2")],
    ));
    assert_eq!(newer["exit"], 4);
    assert_eq!(newer["errors"][0]["id"], "harness.protocol_mismatch");

    let libtest = final_result(&fake.run_with(
        &["shot", "--headless", "--json", "-q"],
        &[("FAKE_MODE", "libtest")],
    ));
    assert_eq!(libtest["exit"], 3);
    assert_eq!(libtest["errors"][0]["id"], "harness.missing");
    assert!(
        libtest["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("harness = false")
    );

    let preset = final_result(&fake.run_with(
        &["shot", "--headless", "--preset", "x", "--json", "-q"],
        &[("FAKE_MODE", "nopreset")],
    ));
    assert_eq!(preset["exit"], 2);
    assert!(
        preset["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("available presets")
    );

    let compile = final_result(&fake.run_with(
        &["ui", "--headless", "tree", "--json", "-q"],
        &[("FAKE_BUILD", "error")],
    ));
    assert_eq!(compile["exit"], 5);
    assert_eq!(compile["errors"][0]["id"], "build.compile_error");
    assert_eq!(compile["errors"][0]["diagnostics"][0]["line"], 2);

    let none = Fake::new(false);
    let missing = final_result(&none.run(&["shot", "--headless", "--json", "-q"]));
    assert_eq!(missing["exit"], 3);
    assert_eq!(missing["errors"][0]["id"], "harness.missing");
    assert!(
        none.argv("cargo").is_empty(),
        "nothing is built without a harness"
    );
}

#[test]
fn icm_test_reports_tests_and_flows() {
    let fake = Fake::new(true);
    let flows = json!({"protocol":1,"kind":"flows","ok":false,"passed":1,"failed":1,"flows":[
        {"name":"flows::add","file":fake.project.join("tests/flows/add.ice"),"passed":true,"ms":3,"error":null,"steps":[{"line":4,"instruction":"click \"Add\"","status":"passed","ms":1}]},
        {"name":"flows::smoke","file":fake.project.join("tests/flows/smoke.ice"),"passed":false,"ms":4,"error":null,"steps":[
            {"line":4,"instruction":"click \"Increment\"","status":"passed","ms":1},
            {"line":5,"instruction":"expect \"Count: 9\"","status":"failed","ms":1,"reason":"no widget shows the text \"Count: 9\"","texts":["Count: 1"]}]}
    ]});
    write(
        &fake.fake("test.stdout"),
        &format!(
            "{{\"reason\":\"compiler-artifact\",\"fresh\":true}}\n\nrunning 2 tests\ntest tests::good ... ok\ntest tests::bad ... FAILED\n\nfailures:\n\n---- tests::bad stdout ----\n\nthread 'tests::bad' (9) panicked at src/lib.rs:2:5:\nassertion `left == right` failed\n  left: 1\n right: 2\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\n\nfailures:\n    tests::bad\n\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\nICM_HARNESS {{\"protocol\":1}}\nflow flows::add ... ok (3 ms)\nflow flows::smoke ... FAILED (4 ms)\nICM_HARNESS_RESULT {flows}\n\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\n"
        ),
    );
    write(
        &fake.fake("test.stderr"),
        "     Running unittests src/lib.rs (/t/a)\nerror: test failed, to rerun pass `--lib`\n     Running tests/icm.rs (/t/icm)\nerror: test failed, to rerun pass `--test icm`\n   Doc-tests fixture_app\nerror: 2 targets failed:\n",
    );

    let output = fake.run_with(
        &["test", "--filter", "smoke", "--json"],
        &[("FAKE_TEST_EXIT", "101")],
    );
    let result = final_result(&output);
    assert_eq!(result["exit"], 1, "{result:#}");
    assert_eq!(result["tests"]["passed"], 2);
    assert_eq!(result["tests"]["failed"], 2);
    assert_eq!(result["tests"]["backend"], "tiny-skia");
    let failed: Vec<&Value> = result["errors"].as_array().unwrap().iter().collect();
    assert_eq!(failed.len(), 2, "{failed:#?}");
    assert_eq!(
        failed[0]["detail"],
        "tests::bad (unittests src/lib.rs) panicked at src/lib.rs:2:5: assertion `left == right` failed\n  left: 1\n right: 2"
    );
    assert_eq!(failed[0]["evidence"][0]["path"], "src/lib.rs");
    assert_eq!(failed[0]["evidence"][0]["line"], 2);
    assert!(
        failed[1]["detail"]
            .as_str()
            .unwrap()
            .starts_with("flows::smoke line 5")
    );
    assert_eq!(failed[1]["evidence"][0]["line"], 5);

    // The passing suite and flow are checks too; empty suites are not.
    let text = String::from_utf8_lossy(&output.stdout);
    let passes: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["type"] == "check" && event["status"] == "pass")
        .collect();
    let details: Vec<&str> = passes
        .iter()
        .map(|event| event["detail"].as_str().unwrap())
        .collect();
    assert_eq!(
        details,
        ["flows::add: 1 step(s) passed in 3 ms"],
        "{details:?}"
    );

    let cargo = fake.argv("cargo");
    assert!(cargo.contains("--no-run"), "{cargo}");
    assert!(
        cargo.contains("--no-fail-fast --message-format=json -- smoke"),
        "{cargo}"
    );
}

#[test]
fn icm_test_passes_and_blocks_on_unparsed_failures() {
    let fake = Fake::new(true);
    write(
        &fake.fake("test.stdout"),
        "\nrunning 3 tests\ntest a ... ok\ntest b ... ok\ntest c ... ignored\n\ntest result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\nICM_HARNESS {\"protocol\":1}\nICM_HARNESS_RESULT {\"protocol\":1,\"kind\":\"flows\",\"ok\":true,\"passed\":0,\"failed\":0,\"flows\":[]}\n",
    );
    let result = final_result(&fake.run(&["test", "--json", "-q"]));
    assert_eq!(result["exit"], 0, "{result:#}");
    assert_eq!(
        result["summary"],
        "2 test(s) passed (including 0 flow(s)), 1 ignored"
    );

    // cargo failed, but no test did: the run itself is the failure.
    let result =
        final_result(&fake.run_with(&["test", "--json", "-q"], &[("FAKE_TEST_EXIT", "101")]));
    assert_eq!(result["exit"], 1);
    assert_eq!(result["errors"][0]["id"], "test.failed");

    let later = final_result(&fake.run(&["test", "--on", "android", "--json", "-q"]));
    assert_eq!(later["exit"], 2);
    assert_eq!(later["errors"][0]["id"], "usage.not_implemented");

    // Flows kept outside tests/flows run one by one through icm-ice.
    let config = fake.project.join("icm.toml");
    let text = std::fs::read_to_string(&config)
        .unwrap()
        .replace("[test]\n", "[test]\nflows = \"ui-flows\"\n");
    write(&config, &text);
    write(
        &fake.project.join("ui-flows/login.ice"),
        "viewport: 402x874\nmode: Immediate\n-----\nexpect \"Count: 0\"\n",
    );
    let result = final_result(&fake.run(&["test", "--json", "-q"]));
    assert_eq!(
        result["exit"], 1,
        "the fake reports every icm-ice as failed"
    );
    assert!(
        fake.argv("harness").contains(&format!(
            "icm-ice {} --report ",
            fake.project.join("ui-flows/login.ice").display()
        )),
        "{}",
        fake.argv("harness")
    );
}

#[test]
fn project_hooks_report_checks() {
    let fake = Fake::new(true);
    let config = fake.project.join("icm.toml");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str("\n[checks]\ndesktop = [\"hooks/ok.sh\", \"hooks/bad.sh\"]\n");
    write(&config, &text);
    executable(
        &fake.project.join("hooks/ok.sh"),
        "#!/bin/sh\necho \"CHECK PASS alive: $ICM_PLATFORM $ICM_APP_ID\"\n",
    );
    write(
        &fake.project.join("hooks/bad.sh"),
        "echo 'CHECK WARN slow: 3s to first frame'\necho broken >&2\nexit 2\n",
    );

    let result = final_result(&fake.run(&["__test", "hooks", "desktop", "--json", "-q"]));
    assert_eq!(result["exit"], 1, "{result:#}");
    assert_eq!(result["errors"][0]["id"], "hook.bad");
    assert!(
        result["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("exit 2")
    );
    assert_eq!(result["warnings"][0]["id"], "hook.slow");
    let hooks = result["hooks"].as_array().unwrap();
    assert_eq!(hooks.len(), 2);
    assert_eq!(hooks[0]["ok"], true);
    assert_eq!(hooks[0]["checks"][0]["id"], "hook.alive");
    assert_eq!(hooks[1]["exit"], 2);
    assert_eq!(result["checks"]["pass"], 1);

    let none = final_result(&fake.run(&["__test", "hooks", "android", "--json", "-q"]));
    assert_eq!(none["exit"], 0);
    assert_eq!(none["summary"], "no [checks] hooks for android in icm.toml");
}
