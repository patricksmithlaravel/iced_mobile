//! End-to-end tests of the release core: `icm release`, `verify`,
//! `upload-commands`, `ledger` and `diagnose`. Most tests run the core
//! through `icm __test release|verify <target>`, whose stand-in pipeline
//! writes a small file where the artifact would be; each target's own
//! pipeline has its own test file (`ios_release.rs`, `android_release.rs`,
//! `web_release.rs`, `desktop_release.rs`). Nothing is built, signed or
//! uploaded; `upload.sh` runs against fake `xcrun` and `icm` scripts.

use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_icm");

const SCRUB: &[&str] = &[
    "ICM_JSON",
    "ICM_CONFIG",
    "ICM_TIMEOUT",
    "ICM_RUN_ID",
    "ICM_RUN_DIR",
    "ICM_RUN_ROOT",
    "ICM_DETACHED",
    "ICM_TOOLS_TOML",
    "ICM_KEYCHAIN",
    "ASC_KEY_ID",
    "ASC_ISSUER_ID",
    "ASC_APP_ID",
];

/// A copy of `tests/fixtures/app` with its own cache and target dirs.
struct App {
    root: tempfile::TempDir,
    env: Vec<(String, String)>,
}

impl App {
    fn new() -> App {
        let root = tempfile::tempdir().unwrap();
        copy_dir(&fixtures().join("release"), &root.path().join("app"));
        App {
            root,
            env: vec![("ICM_TODAY".into(), "2026-10-07".into())],
        }
    }

    fn dir(&self) -> PathBuf {
        self.root.path().join("app")
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.path().join(relative)
    }

    fn set(&mut self, key: &str, value: impl AsRef<std::ffi::OsStr>) {
        self.env
            .push((key.into(), value.as_ref().to_string_lossy().into_owned()));
    }

    /// Appends to the app's icm.toml.
    fn config(&self, extra: &str) {
        let path = self.dir().join("icm.toml");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(extra);
        std::fs::write(path, text).unwrap();
    }

    /// The owner's answers and a real (not placeholder) icon.
    fn ready_for_ios(&self) {
        write_png_header(&self.dir().join("assets/icon.png"), 1024);
        let path = self.dir().join("icm.toml");
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("[app]\n", "[app]\nicon = \"assets/icon.png\"\n");
        std::fs::write(&path, text).unwrap();
        self.config(
            "\n[ios]\nteam_id = \"ABCDE12345\"\nuses_non_exempt_encryption = false\nasc_app_id = \"1234567890\"\n\n[store]\nprivacy_policy_url = \"https://acme.example/privacy\"\nsupport_url = \"https://acme.example/support\"\n",
        );
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        let _ = command
            .args(args)
            .current_dir(self.dir())
            .env("ICM_CACHE_DIR", self.path("cache"))
            .env("ICM_HOST_CONFIG", self.path("host.toml"))
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

/// A PNG signature and IHDR: all `icm check`'s icon check reads.
fn write_png_header(path: &Path, size: u32) {
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    bytes.extend(size.to_be_bytes());
    bytes.extend(size.to_be_bytes());
    bytes.extend([8, 2, 0, 0, 0, 0, 0, 0, 0]);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn write_exe(path: &Path, script: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn result(output: &Output) -> Value {
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "--json -q prints only the result: {text}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(result["type"], "result");
    assert_eq!(result["ok"], result["exit"] == 0);
    assert_eq!(output.status.code().map(i64::from), result["exit"].as_i64());
    result
}

fn ids(result: &Value, key: &str) -> Vec<String> {
    result[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn every_target_pipeline_plans_without_writing() {
    let app = App::new();
    for target in ["ios", "android", "web", "macos", "windows", "linux"] {
        // Every target has a pipeline; its own tests build with it.
        let plan = app.json(&["release", target, "--dry-run"]);
        assert_eq!(plan["exit"], 0, "{target}: {plan}");
        assert!(plan["plan"].is_array(), "{target}: {plan}");
        // Nothing was written.
        assert!(!app.dir().join("target/icm/dist").exists(), "{target}");
    }
    // The iOS pipeline's parser reads altool's JSON; a Cargo.toml is not.
    let diagnose = app.json(&["diagnose", "altool", "Cargo.toml"]);
    assert_eq!(diagnose["errors"][0]["id"], "usage.bad_args");
}

#[test]
fn flags_belong_to_their_target() {
    let app = App::new();
    for (target, flag) in [
        ("ios", "--apk"),
        ("web", "--no-smoke"),
        ("android", "--dmg"),
        ("linux", "--universal"),
        ("macos", "--via-xcode-export"),
    ] {
        let result = app.json(&["release", target, flag]);
        assert_eq!(result["exit"], 2, "{target} {flag}");
        assert_eq!(result["errors"][0]["id"], "usage.bad_args");
    }
    let url = app.json(&["verify", "ios", "--url", "https://x.example"]);
    assert_eq!(url["errors"][0]["id"], "usage.bad_args");
}

#[test]
fn a_signed_release_stops_for_the_owner_before_building() {
    let app = App::new();
    let result = app.json(&["__test", "release", "ios"]);
    assert_eq!(result["exit"], 9, "{result}");
    assert_eq!(result["errors"][0]["fix"]["by"], "owner");
    let errors = ids(&result, "errors");
    for id in ["app.icon.placeholder", "config.owner_decision"] {
        assert!(errors.contains(&id.to_string()), "{id} not in {errors:?}");
    }
    // Every owner item is listed, and nothing was built.
    let steps = result["owner_steps"].as_array().unwrap();
    assert_eq!(steps.len(), 3, "{result}");
    assert!(steps.iter().all(|step| step["kind"] == "fix"));
    assert!(!app.dir().join("target/icm/dist").exists());
    assert!(
        result["summary"]
            .as_str()
            .unwrap()
            .starts_with("the owner must act"),
        "{result}"
    );
}

#[test]
fn an_unsigned_release_writes_the_dist_and_refuses_to_upload() {
    let app = App::new();
    let result = app.json(&["__test", "release", "ios", "--sign", "none"]);
    assert_eq!(result["exit"], 0, "{result}");
    let warnings = ids(&result, "warnings");
    assert!(
        warnings.contains(&"config.owner_decision".to_string()),
        "{warnings:?}"
    );
    assert!(
        warnings.contains(&"app.icon.placeholder".to_string()),
        "{warnings:?}"
    );
    assert_eq!(result["release"]["uploadable"], false);
    assert_eq!(result["release"]["sign"], "none");

    let manifest_path = app.abs(&result["artifacts"]["manifest"]);
    let ipa = app.abs(&result["artifacts"]["ipa"]);
    assert!(
        ipa.ends_with("target/icm/dist/0.3.0+7/ios/Fixture.ipa"),
        "{}",
        ipa.display()
    );
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    assert_eq!(manifest["schema"], "icm.artifacts/1");
    assert_eq!(manifest["app"]["build"], 7);
    assert_eq!(manifest["sign"], "none");
    assert_eq!(manifest["uploadable"], false);
    let file = |role: &str| -> Value {
        manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["role"] == role)
            .cloned()
            .unwrap_or_else(|| panic!("no {role} file in {manifest}"))
    };
    assert_eq!(file("upload")["path"], "Fixture.ipa");
    assert_eq!(file("upload")["kind"], "ipa");
    assert_eq!(
        file("upload")["sha256"],
        icm::hash::sha256_hex(&std::fs::read(&ipa).unwrap())
    );
    assert_eq!(file("notices")["path"], "THIRD_PARTY_NOTICES.txt");
    assert_eq!(
        manifest["notices"][0],
        serde_json::json!({"artifact": "Fixture.ipa", "path": "Payload/Fixture.app/THIRD_PARTY_NOTICES.txt"})
    );
    assert!(
        manifest["checks"]["ids_warn"]
            .as_array()
            .unwrap()
            .contains(&Value::from("config.owner_decision"))
    );
    // The fixture's iced is a path dependency.
    assert_eq!(manifest["framework"]["source"], "path");

    // dist/latest/ios points at it.
    let latest = app.dir().join("target/icm/dist/latest/ios");
    assert_eq!(
        std::fs::read_link(&latest).unwrap(),
        Path::new("../0.3.0+7/ios")
    );

    // upload.sh refuses an unsigned release with exit 9.
    let sh = app.abs(&result["artifacts"]["upload_sh"]);
    let output = Command::new("bash").arg(&sh).output().unwrap();
    assert_eq!(output.status.code(), Some(9));
    assert!(String::from_utf8_lossy(&output.stderr).contains("not uploadable"));

    // icm verify uses the release's severities and checks the hashes.
    let verify = app.json(&["__test", "verify", "ios"]);
    assert_eq!(verify["exit"], 0, "{verify}");
    assert_eq!(verify["verify"]["sign"], "none");
    std::fs::write(&ipa, b"tampered").unwrap();
    let tampered = app.json(&["__test", "verify", "ios"]);
    assert_eq!(tampered["exit"], 1, "{tampered}");
    assert_eq!(tampered["errors"][0]["id"], "release.artifact_changed");

    // upload-commands prints UPLOAD.md.
    let output = app.command(&["upload-commands", "ios"]).output().unwrap();
    let md = String::from_utf8_lossy(&output.stdout);
    assert!(
        md.starts_with("# Upload: Fixture 0.3.0 (build 7) to App Store Connect"),
        "{md}"
    );
    assert!(md.contains("**Not uploadable:**"), "{md}");
}

#[test]
fn a_signed_release_runs_through_upload_sh_and_the_ledger() {
    let app = App::new();
    app.ready_for_ios();
    let result = app.json(&["__test", "release", "ios"]);
    assert_eq!(result["exit"], 0, "{result}");
    assert_eq!(result["release"]["uploadable"], true, "{result}");
    assert!(
        result["owner_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step["kind"] == "upload"),
        "{result}"
    );
    let dist = app.abs(&result["artifacts"]["dist"]);
    let md = std::fs::read_to_string(dist.join("UPLOAD.md")).unwrap();
    assert!(md.contains("**Uploadable:** yes"), "{md}");
    assert!(md.contains("--apple-id 1234567890"), "{md}");
    assert!(
        md.contains("Privacy policy URL: https://acme.example/privacy"),
        "{md}"
    );

    // upload.sh: exit 9 while the API key variables are unset.
    let sh = dist.join("upload.sh");
    let bare = Command::new("bash")
        .arg(&sh)
        .env_remove("ASC_KEY_ID")
        .env_remove("ASC_ISSUER_ID")
        .output()
        .unwrap();
    assert_eq!(bare.status.code(), Some(9));
    assert!(String::from_utf8_lossy(&bare.stderr).contains("set ASC_KEY_ID"));

    // With them, it runs xcrun (a fake that records its argv) and icm.
    let bin = app.path("fakebin");
    write_exe(
        &bin.join("xcrun"),
        &format!(
            "#!/bin/sh\necho \"xcrun $*\" >> '{}'\necho '{{\"success-message\":\"ok\"}}'\n",
            app.path("xcrun.log").display()
        ),
    );
    write_exe(
        &bin.join("icm"),
        &format!(
            "#!/bin/sh\nif [ \"$1\" = diagnose ]; then echo \"diagnose $*\" >> '{log}'; exit 0; fi\nexport ICM_CACHE_DIR='{cache}' ICM_HOST_CONFIG='{host}' CARGO_TARGET_DIR='{target}'\nexec '{bin}' \"$@\"\n",
            log = app.path("xcrun.log").display(),
            cache = app.path("cache").display(),
            host = app.path("host.toml").display(),
            target = app.dir().join("target").display(),
            bin = BIN,
        ),
    );
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let run = Command::new("bash")
        .arg(&sh)
        .env("PATH", path)
        .env("ASC_KEY_ID", "KEY123")
        .env("ASC_ISSUER_ID", "issuer-uuid")
        .output()
        .unwrap();
    assert_eq!(
        run.status.code(),
        Some(0),
        "{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let log = std::fs::read_to_string(app.path("xcrun.log")).unwrap();
    assert!(
        log.contains(&format!(
            "xcrun altool --validate-app {}/Fixture.ipa --api-key KEY123 --api-issuer issuer-uuid --output-format json",
            dist.display()
        )),
        "{log}"
    );
    assert!(log.contains("--upload-package"), "{log}");
    assert!(log.contains("diagnose diagnose altool"), "{log}");
    assert!(dist.join("validate.json").is_file());

    // The ledger recorded the upload; the same build is now refused.
    let ledger = std::fs::read_to_string(app.dir().join(".icm/ledger.toml")).unwrap();
    assert!(ledger.contains("target = \"ios\""), "{ledger}");
    assert!(ledger.contains("build = 7"), "{ledger}");
    let shown = app.json(&["ledger", "show"]);
    assert_eq!(shown["uploads"][0]["build"], 7, "{shown}");
    assert_eq!(shown["uploads"][0]["artifact"], "Fixture.ipa");

    let again = app.json(&["__test", "release", "ios"]);
    assert_eq!(again["exit"], 1, "{again}");
    assert_eq!(again["errors"][0]["id"], "version.build_not_increased");
    assert!(
        again["errors"][0]["fix"]["summary"]
            .as_str()
            .unwrap()
            .contains("[app] build = 8")
    );
    // Unsigned builds only warn about it.
    let unsigned = app.json(&["__test", "release", "ios", "--sign", "none"]);
    assert_eq!(unsigned["exit"], 0, "{unsigned}");
    assert!(ids(&unsigned, "warnings").contains(&"version.build_not_increased".to_string()));
}

#[test]
fn upload_sh_diagnoses_a_failed_upload() {
    let app = App::new();
    app.ready_for_ios();
    let result = app.json(&["__test", "release", "ios"]);
    assert_eq!(result["exit"], 0, "{result}");
    let dist = app.abs(&result["artifacts"]["dist"]);

    // altool rejects the API key (it prints its JSON and exits 1); the
    // real icm diagnoses it.
    let bin = app.path("fakebin");
    write_exe(
        &bin.join("xcrun"),
        "#!/bin/sh\necho '{\"product-errors\":[{\"code\":-19209,\"message\":\"Failed to authenticate for session: (401) NOT_AUTHORIZED\"}]}'\nexit 1\n",
    );
    write_exe(
        &bin.join("icm"),
        &format!(
            "#!/bin/sh\nexport ICM_CACHE_DIR='{cache}' ICM_HOST_CONFIG='{host}' CARGO_TARGET_DIR='{target}'\nexec '{bin}' \"$@\"\n",
            cache = app.path("cache").display(),
            host = app.path("host.toml").display(),
            target = app.dir().join("target").display(),
            bin = BIN,
        ),
    );
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let run = Command::new("bash")
        .arg(dist.join("upload.sh"))
        .env("PATH", path)
        .env("ASC_KEY_ID", "KEY123")
        .env("ASC_ISSUER_ID", "issuer-uuid")
        .output()
        .unwrap();
    let output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    // The owner's exit with the catalogue id, not altool's 1.
    assert_eq!(run.status.code(), Some(9), "{output}");
    assert!(output.contains("ios.asc.auth"), "{output}");
    assert!(dist.join("validate.json").is_file());
    // Nothing was recorded.
    assert!(!app.dir().join(".icm/ledger.toml").exists());
}

#[test]
fn android_signing_problems_still_build_the_unsigned_bundle() {
    let mut app = App::new();
    app.ready_for_ios();
    app.config(&format!(
        "\n[android.signing]\nupload = {{ keystore = \"{}\", alias = \"upload\", store_pass_env = \"ICM_TEST_STOREPASS\" }}\n",
        app.path("up.jks").display()
    ));

    let missing = app.json(&["__test", "release", "android"]);
    assert_eq!(missing["exit"], 9, "{missing}");
    assert_eq!(missing["errors"][0]["id"], "android.keystore.missing");
    assert!(ids(&missing, "errors").contains(&"android.keystore.password_env_unset".to_string()));
    // Deferred: the bundle and the documents are there.
    assert!(app.abs(&missing["artifacts"]["aab"]).is_file(), "{missing}");
    assert!(app.abs(&missing["artifacts"]["upload_md"]).is_file());
    assert_eq!(missing["release"]["uploadable"], false);
    assert!(
        missing["summary"]
            .as_str()
            .unwrap()
            .starts_with("built Fixture 0.3.0 (build 7) for android"),
        "{missing}"
    );
    // The owner's items, then the owner's plan.
    let kinds: Vec<&str> = missing["owner_steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds.first(), Some(&"fix"), "{kinds:?}");
    assert!(kinds.contains(&"web"), "{kinds:?}");

    std::fs::write(app.path("up.jks"), b"keystore").unwrap();
    let unset = app.json(&["__test", "release", "android"]);
    assert_eq!(unset["exit"], 9, "{unset}");
    assert_eq!(
        unset["errors"][0]["id"],
        "android.keystore.password_env_unset"
    );
    // The variable's name, never a value.
    assert!(
        unset["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("ICM_TEST_STOREPASS")
    );

    app.set("ICM_TEST_STOREPASS", "correct-horse-battery");
    let ok = app.json(&["__test", "release", "android"]);
    assert_eq!(ok["exit"], 0, "{ok}");
    let dist = app.abs(&ok["artifacts"]["dist"]);
    // The first Android release is manual: upload.sh says so (exit 9).
    let first = Command::new("bash")
        .arg(dist.join("upload.sh"))
        .output()
        .unwrap();
    assert_eq!(first.status.code(), Some(9));
    assert!(String::from_utf8_lossy(&first.stderr).contains("first Android release"));
    let all = std::fs::read_to_string(dist.join("artifacts.json")).unwrap()
        + &std::fs::read_to_string(dist.join("UPLOAD.md")).unwrap();
    assert!(!all.contains("correct-horse-battery"));
}

#[test]
fn releases_come_from_a_clean_commit() {
    let app = App::new();
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(app.dir())
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    std::fs::write(app.dir().join(".gitignore"), "target/\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "init"]);

    let clean = app.json(&["__test", "release", "web", "--sign", "none"]);
    assert_eq!(clean["exit"], 0, "{clean}");
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(app.abs(&clean["artifacts"]["manifest"])).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["source"]["dirty"], false);
    assert!(manifest["source"]["git_rev"].as_str().unwrap().len() == 40);
    // The web site is a directory, hashed as a whole.
    assert!(
        manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["kind"] == "site")
    );

    app.config("\n# a change\n");
    let dirty = app.json(&["__test", "release", "web", "--sign", "none"]);
    assert_eq!(dirty["exit"], 1, "{dirty}");
    assert_eq!(dirty["errors"][0]["id"], "release.dirty_tree");
    let allowed = app.json(&[
        "__test",
        "release",
        "web",
        "--sign",
        "none",
        "--allow-dirty",
    ]);
    assert_eq!(allowed["exit"], 0, "{allowed}");

    // A Cargo.lock that is not committed: the commit cannot rebuild what
    // --locked builds, so the tree is not clean.
    git(&["checkout", "--", "icm.toml"]);
    git(&["rm", "-q", "--cached", "Cargo.lock"]);
    git(&["commit", "-qm", "untrack the lock"]);
    let untracked = app.json(&["__test", "release", "ios", "--sign", "none"]);
    assert_eq!(untracked["exit"], 1, "{untracked}");
    assert_eq!(untracked["errors"][0]["id"], "release.dirty_tree");
    let detail = untracked["errors"][0]["detail"].as_str().unwrap();
    assert!(detail.contains("Cargo.lock is not committed"), "{detail}");
    assert!(
        untracked["errors"][0]["fix"]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "git add Cargo.lock"),
        "{untracked}"
    );
    let recorded = app.json(&[
        "__test",
        "release",
        "ios",
        "--sign",
        "none",
        "--allow-dirty",
    ]);
    assert_eq!(recorded["exit"], 0, "{recorded}");
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(app.abs(&recorded["artifacts"]["manifest"])).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["source"]["dirty"], true, "{manifest}");

    // No Cargo.lock at all: a precondition, before anything is built.
    std::fs::remove_file(app.dir().join("Cargo.lock")).unwrap();
    std::fs::remove_dir_all(app.dir().join("target/icm/dist")).unwrap();
    let missing = app.json(&["__test", "release", "ios", "--sign", "none"]);
    assert_eq!(missing["exit"], 1, "{missing}");
    assert_eq!(missing["errors"][0]["id"], "release.lock_missing");
    assert_eq!(
        missing["errors"][0]["fix"]["commands"][0],
        "icm check ios-device --json -q"
    );
    assert!(!app.dir().join("target/icm/dist/0.3.0+7/ios").exists());
    let web = app.json(&["__test", "release", "web", "--sign", "none"]);
    assert_eq!(
        web["errors"][0]["fix"]["commands"][0],
        "icm doctor web --fix --yes"
    );
}

#[test]
fn versions_must_be_plain() {
    let app = App::new();
    let manifest = app.dir().join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("version = \"0.3.0\"", "version = \"0.3.0-beta.1\"");
    std::fs::write(&manifest, text).unwrap();
    let result = app.json(&["__test", "release", "linux", "--sign", "none"]);
    assert_eq!(result["exit"], 3, "{result}");
    assert_eq!(result["errors"][0]["id"], "version.format");
    assert_eq!(result["errors"][0]["evidence"][0]["path"], "Cargo.toml");
    assert!(
        result["errors"][0]["evidence"][0]["line"]
            .as_u64()
            .is_some()
    );
}

#[test]
fn dry_runs_print_the_plan_and_write_nothing() {
    let app = App::new();
    let result = app.json(&["__test", "release", "macos", "--dry-run"]);
    assert_eq!(result["exit"], 0, "{result}");
    let names: Vec<&str> = result["plan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "release.preconditions",
            "release.dist",
            "fake.build",
            "release.manifest",
            "release.upload",
            "release.latest"
        ]
    );
    assert!(!app.dir().join("target/icm/dist").exists());

    // macOS's plan shows what the real run does: a Developer ID signs with
    // a secure timestamp and the keychain icm searches, and stage 2 keeps
    // the dist directory that holds the stapled app.
    let mut app = app;
    app.set("ICM_KEYCHAIN", "/k/build.keychain-db");
    let step = |result: &Value, name: &str| -> String {
        result["plan"]
            .as_array()
            .unwrap()
            .iter()
            .find(|step| step["name"] == name)
            .unwrap_or_else(|| panic!("no {name} in {result}"))["display"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let stage1 = app.json(&["release", "macos", "--dry-run"]);
    assert_eq!(stage1["exit"], 0, "{stage1}");
    let codesign = step(&stage1, "codesign.app");
    assert!(codesign.contains(" --timestamp "), "{codesign}");
    assert!(
        codesign.contains("--keychain /k/build.keychain-db"),
        "{codesign}"
    );
    assert!(
        step(&stage1, "security.find-identity").ends_with("/k/build.keychain-db"),
        "{stage1}"
    );
    assert!(
        step(&stage1, "release.dist").starts_with("(icm) empty "),
        "{stage1}"
    );
    let stage2 = app.json(&["release", "macos", "--dmg", "--dry-run"]);
    assert_eq!(stage2["exit"], 0, "{stage2}");
    assert!(
        step(&stage2, "release.dist").starts_with("(icm) keep "),
        "{stage2}"
    );
    let codesign = step(&stage2, "codesign.dmg");
    assert!(codesign.contains(" --timestamp "), "{codesign}");
    assert!(
        codesign.contains("--keychain /k/build.keychain-db"),
        "{codesign}"
    );
    // A test identity named in icm.toml signs without a timestamp.
    app.config("\n[desktop.macos]\nidentity = \"icm-test Code Signing\"\n");
    let named = app.json(&["release", "macos", "--dry-run"]);
    assert!(
        step(&named, "codesign.app").contains("--timestamp=none"),
        "{named}"
    );
    let unsigned = app.json(&["release", "macos", "--sign", "none", "--dry-run"]);
    assert!(
        step(&unsigned, "codesign.app").contains("--sign - "),
        "{unsigned}"
    );
    assert!(!app.dir().join("target/icm/dist").exists());
}

#[test]
fn release_builds_use_their_profile_dir_and_stamps() {
    let mut app = App::new();
    let real_cargo = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v cargo"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let log = app.path("cargo.log");
    write_exe(
        &app.path("fakebin/cargo"),
        &format!(
            "#!/bin/sh\ncase \"$1\" in build|clean) echo \"$* | IPHONEOS_DEPLOYMENT_TARGET=${{IPHONEOS_DEPLOYMENT_TARGET:-}}\" >> '{}'; exit 0;; esac\nexec '{real_cargo}' \"$@\"\n",
            log.display()
        ),
    );
    app.set("ICM_TOOL_CARGO", app.path("fakebin/cargo"));
    let target_dir = app.dir().join("target/icm/release-target");
    let read_log = || std::fs::read_to_string(&log).unwrap_or_default();

    let web = app.json(&["__test", "release-build", "web"]);
    assert_eq!(web["exit"], 0, "{web}");
    let line = read_log();
    assert!(
        line.starts_with("build --config profile.icm-web.inherits=\"release\" --config profile.icm-web.opt-level=\"z\""),
        "{line}"
    );
    assert!(
        line.contains("--target wasm32-unknown-unknown --profile icm-web"),
        "{line}"
    );
    assert!(line.contains("--locked"), "{line}");
    assert!(
        line.contains(&format!("--target-dir {}", target_dir.display())),
        "{line}"
    );

    // An Apple target: the deployment target in cargo's environment and a
    // stamp in the release directory.
    std::fs::remove_file(&log).unwrap();
    let ios = app.json(&["__test", "release-build", "ios", "--min-os", "16.0"]);
    assert_eq!(ios["exit"], 0, "{ios}");
    let line = read_log();
    assert!(line.contains("--release"), "{line}");
    assert!(
        line.contains("profile.release.debug=\"line-tables-only\""),
        "{line}"
    );
    assert!(
        line.ends_with("IPHONEOS_DEPLOYMENT_TARGET=16.0\n"),
        "{line}"
    );
    assert!(!line.contains("clean"), "{line}");
    let stamp = target_dir.join("stamps/deployment-aarch64-apple-ios-release.txt");
    assert_eq!(
        std::fs::read_to_string(&stamp).unwrap().trim(),
        "IPHONEOS_DEPLOYMENT_TARGET=16.0"
    );

    // A new minimum OS cleans the app package in the release directory
    // first, so it relinks.
    std::fs::create_dir_all(target_dir.join("aarch64-apple-ios/release")).unwrap();
    std::fs::remove_file(&log).unwrap();
    let changed = app.json(&["__test", "release-build", "ios", "--min-os", "17.0"]);
    assert_eq!(changed["exit"], 0, "{changed}");
    let lines = read_log();
    let first = lines.lines().next().unwrap();
    assert!(first.starts_with("clean --manifest-path"), "{lines}");
    assert!(
        first.contains("-p release-app --target aarch64-apple-ios --release"),
        "{lines}"
    );
    assert!(
        first.contains(&format!("--target-dir {}", target_dir.display())),
        "{lines}"
    );
    assert!(
        lines
            .lines()
            .nth(1)
            .unwrap()
            .ends_with("IPHONEOS_DEPLOYMENT_TARGET=17.0")
    );
    assert_eq!(
        std::fs::read_to_string(&stamp).unwrap().trim(),
        "IPHONEOS_DEPLOYMENT_TARGET=17.0"
    );
    // The dev stamps are not touched.
    assert!(!app.dir().join("target/icm/stamps").exists());
}

#[test]
fn releases_carry_third_party_notices() {
    let app = App::new();
    let result = app.json(&["__test", "release", "ios", "--sign", "none"]);
    assert_eq!(result["exit"], 0, "{result}");
    let notices = std::fs::read_to_string(app.abs(&result["artifacts"]["notices"])).unwrap();
    // The shipped packages, with their licences; Fira Sans's OFL first.
    assert!(notices.starts_with("THIRD-PARTY NOTICES"), "{notices}");
    assert!(
        notices.contains("Fira Sans: SIL Open Font License 1.1"),
        "{notices}"
    );
    assert!(
        notices.contains("SIL OPEN FONT LICENSE Version 1.1 (the fixture"),
        "{notices}"
    );
    for line in [
        "iced 0.14.1: MIT (https://github.com/patricksmithlaravel/iced_mobile)",
        "iced_graphics 0.14.1: MIT",
        "notes_dep 1.2.0: MIT OR Apache-2.0",
        "no_licence 0.1.0: no licence declared",
        "notes_dep: the MIT licence",
        "notes_dep: the Apache License 2.0",
        "Copyright (fixture) the iced stand-in's authors",
    ] {
        assert!(notices.contains(line), "{line:?} missing from:\n{notices}");
    }
    // The standard library every artifact links, with the crates it
    // vendors, and the Rust project's licence texts.
    for line in [
        "The Rust standard library (rustc ",
        "\nstd ",
        "\ncompiler_builtins ",
        "\nhashbrown ",
        "Copyright (c) The Rust Project Developers",
        "LLVM Exceptions to the Apache 2.0 License",
    ] {
        assert!(notices.contains(line), "{line:?} missing from:\n{notices}");
    }
    // Build and dev dependencies, and the app itself, never ship.
    for absent in ["build_only", "dev_only", "release-app"] {
        assert!(!notices.contains(absent), "{absent} in:\n{notices}");
    }
    // iced_graphics has no licence file: the nearest one up its tree is not
    // taken past the app's workspace root.
    assert!(notices.contains("iced_graphics 0.14.1 (MIT)"), "{notices}");
    assert!(ids(&result, "warnings").contains(&"release.licence_unknown".to_string()));

    // The gate looked inside the .ipa.
    let ipa = app.abs(&result["artifacts"]["ipa"]);
    assert_eq!(
        icm::release::notices::presence(&ipa, "Payload/Fixture.app/THIRD_PARTY_NOTICES.txt"),
        icm::release::notices::Presence::Present
    );
    let verify = app.json(&[
        "__test",
        "verify",
        "ios",
        "--artifact",
        ipa.to_str().unwrap(),
    ]);
    assert_eq!(verify["exit"], 0, "{verify}");
    assert_eq!(verify["checks"]["fail"], 0, "{verify}");
    // verify with --artifact found the project without resolving it for
    // the newest release; its run is still the project's.
    let run_dir = app.abs(&verify["run_dir"]);
    assert!(
        run_dir.starts_with(app.dir().join("target/icm/runs")),
        "{}",
        run_dir.display()
    );

    // Without `fira-sans`, Fira Sans ships only on the phones
    // (`mobile-fira-sans`, on by default).
    let manifest = app.dir().join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace(", features = [\"fira-sans\"]", "");
    std::fs::write(&manifest, text).unwrap();
    let linux = app.json(&["__test", "release", "linux", "--sign", "none"]);
    let text = std::fs::read_to_string(app.abs(&linux["artifacts"]["notices"])).unwrap();
    assert!(!text.contains("Fira Sans"), "{text}");
    let android = app.json(&["__test", "release", "android", "--sign", "none"]);
    let text = std::fs::read_to_string(app.abs(&android["artifacts"]["notices"])).unwrap();
    assert!(
        text.contains("Fira Sans: SIL Open Font License 1.1"),
        "{text}"
    );
    // A .deb is not looked inside: the place is declared (INFO).
    assert!(
        std::fs::read_to_string(app.abs(&linux["artifacts"]["manifest"]))
            .unwrap()
            .contains("\"path\": \"doc/THIRD_PARTY_NOTICES.txt\"")
    );
}

/// A value of a secret-named variable in icm's environment that the build
/// baked into what ships fails `release.secret_in_artifacts`, naming the
/// variable and never the value, and the release is not uploadable; under
/// `--sign none` it is a WARN. `icm verify` searches again.
#[test]
fn a_baked_in_secret_value_is_not_uploadable() {
    let mut app = App::new();
    app.ready_for_ios();
    let token = "zq9x-baked-into-the-app-17";
    app.set("ICM_TEST_RELEASE_TOKEN", token);
    app.set("ICM_FAKE_BAKE", "ICM_TEST_RELEASE_TOKEN");

    let signed = app.json(&["__test", "release", "ios"]);
    assert_eq!(signed["exit"], 1, "{signed}");
    assert_eq!(signed["release"]["uploadable"], false, "{signed}");
    assert_eq!(
        signed["checks"]["failed"],
        serde_json::json!(["release.secret_in_artifacts"])
    );
    let dist = app.abs(&signed["artifacts"]["dist"]);
    let md = std::fs::read_to_string(dist.join("UPLOAD.md")).unwrap();
    assert!(md.contains("release.secret_in_artifacts"), "{md}");
    let events =
        std::fs::read_to_string(app.abs(&signed["run_dir"]).join("events.ndjson")).unwrap();
    assert!(
        events.contains("the value of `ICM_TEST_RELEASE_TOKEN` (secret-named"),
        "{events}"
    );
    assert!(!events.contains(token) && !signed.to_string().contains(token));

    let verify = app.json(&["__test", "verify", "ios"]);
    assert_eq!(verify["exit"], 1, "{verify}");
    assert_eq!(
        verify["checks"]["failed"],
        serde_json::json!(["release.secret_in_artifacts"])
    );

    let unsigned = app.json(&["__test", "release", "ios", "--sign", "none"]);
    assert_eq!(unsigned["exit"], 0, "{unsigned}");
    assert!(
        ids(&unsigned, "warnings").contains(&"release.secret_in_artifacts".to_string()),
        "{unsigned}"
    );
}

#[test]
fn the_ledger_marks_uploads_once() {
    let app = App::new();
    let none = app.json(&["ledger", "mark-uploaded", "ios"]);
    assert_eq!(none["exit"], 2, "{none}");
    assert_eq!(none["errors"][0]["id"], "release.not_found");
    // Recording a build without its release is the owner's call.
    assert_eq!(none["errors"][0]["fix"]["by"], "owner", "{none}");

    let forced = app.json(&["ledger", "mark-uploaded", "android", "--build", "3"]);
    assert_eq!(forced["exit"], 0, "{forced}");
    assert_eq!(forced["warnings"][0]["id"], "release.not_found");
    let again = app.json(&["ledger", "mark-uploaded", "android", "--build", "3"]);
    assert!(
        again["summary"]
            .as_str()
            .unwrap()
            .contains("already recorded")
    );
    let shown = app.json(&["ledger", "show"]);
    assert_eq!(shown["uploads"].as_array().unwrap().len(), 1);

    // A release that is not uploadable cannot have been uploaded: refused
    // (the dry run too), unless the owner forces it.
    let unsigned = app.json(&["__test", "release", "ios", "--sign", "none"]);
    assert_eq!(unsigned["exit"], 0, "{unsigned}");
    for args in [
        &["ledger", "mark-uploaded", "ios", "--dry-run"][..],
        &["ledger", "mark-uploaded", "ios"][..],
    ] {
        let refused = app.json(args);
        assert_eq!(refused["exit"], 9, "{refused}");
        assert_eq!(refused["errors"][0]["id"], "release.not_uploadable");
        assert_eq!(refused["errors"][0]["fix"]["by"], "owner");
        assert!(
            refused["errors"][0]["detail"]
                .as_str()
                .unwrap()
                .contains("it is unsigned"),
            "{refused}"
        );
    }
    assert_eq!(
        app.json(&["ledger", "show"])["uploads"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let forced = app.json(&["ledger", "mark-uploaded", "ios", "--force"]);
    assert_eq!(forced["exit"], 0, "{forced}");
    assert_eq!(forced["warnings"][0]["id"], "release.not_uploadable");
    assert_eq!(
        app.json(&["ledger", "show"])["uploads"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // An uploadable release's dry run says nothing was recorded.
    let signed = App::new();
    signed.ready_for_ios();
    let release = signed.json(&["__test", "release", "ios"]);
    assert_eq!(release["exit"], 0, "{release}");
    let plan = signed.json(&["ledger", "mark-uploaded", "ios", "--dry-run"]);
    assert_eq!(plan["exit"], 0, "{plan}");
    assert_eq!(
        plan["summary"],
        "the plan of icm ledger mark-uploaded ios (--dry-run: nothing was recorded)"
    );
    assert!(!signed.dir().join(".icm/ledger.toml").exists());
}
