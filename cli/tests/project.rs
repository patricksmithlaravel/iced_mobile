//! End-to-end tests of the project commands: `new`, `check`, `doctor`,
//! `explain config.<key>`, `stop` and `ps`. Machine tools are fakes
//! (`ICM_TOOL_*`, a fake SDK, JDK and Rust sysroot), so no test downloads
//! anything or touches a real simulator, emulator or `~/.android`.

use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_icm");

/// Variables from the developer's shell that would steer discovery.
const SCRUB: &[&str] = &[
    "ICM_JSON",
    "ICM_CONFIG",
    "ICM_TIMEOUT",
    "ICM_RUN_ID",
    "ICM_RUN_DIR",
    "ICM_RUN_ROOT",
    "ICM_DETACHED",
    "ICM_CHROME",
    "ANDROID_HOME",
    "ANDROID_SDK_ROOT",
    "ANDROID_NDK_HOME",
    "ANDROID_NDK_ROOT",
    "ANDROID_USER_HOME",
    "ANDROID_SDK_HOME",
    "ANDROID_AVD_HOME",
    "JAVA_HOME",
    "DEVELOPER_DIR",
];

struct Sandbox {
    root: tempfile::TempDir,
    cwd: PathBuf,
    env: Vec<(String, String)>,
}

impl Sandbox {
    fn new() -> Sandbox {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        Sandbox {
            cwd,
            env: Vec::new(),
            root,
        }
    }

    /// A sandbox whose working directory is a copy of a fixture project.
    fn with_fixture(name: &str) -> Sandbox {
        let sandbox = Sandbox::new();
        copy_dir(&fixtures().join(name), &sandbox.cwd);
        sandbox
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.path().join(relative)
    }

    fn set(&mut self, key: &str, value: impl AsRef<std::ffi::OsStr>) {
        self.env.push((
            key.to_string(),
            value.as_ref().to_string_lossy().into_owned(),
        ));
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        let _ = command
            .args(args)
            .current_dir(&self.cwd)
            .env("ICM_CACHE_DIR", self.path("cache"))
            .env("ICM_HOST_CONFIG", self.path("host.toml"))
            .env("CARGO_TARGET_DIR", self.cwd.join("target"))
            .stdin(Stdio::null());
        for var in SCRUB {
            let _ = command.env_remove(var);
        }
        for (key, value) in &self.env {
            let _ = command.env(key, value);
        }
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut full: Vec<&str> = args.to_vec();
        full.extend(["--json", "-q"]);
        result(&self.run(&full))
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn fork_root() -> PathBuf {
    std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()).unwrap()
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

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The result line of `--json -q`; checks the contract on the way.
fn result(output: &Output) -> Value {
    let text = stdout(output);
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

fn write_exe(path: &Path, script: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A fake rustc and rustup over a fake sysroot holding `targets`;
/// `rustup target add` installs into it.
fn fake_rust(sandbox: &mut Sandbox, targets: &[&str]) -> PathBuf {
    let sysroot = sandbox.path("sysroot");
    for target in targets {
        std::fs::create_dir_all(sysroot.join("lib/rustlib").join(target).join("lib")).unwrap();
    }
    let bin = sandbox.path("fakebin");
    write_exe(
        &bin.join("rustc"),
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n  *sysroot*) echo '{}' ;;\n  *--version*) echo 'rustc 1.98.0 (fake 2026-08-18)' ;;\n  *) exit 1 ;;\nesac\n",
            sysroot.display()
        ),
    );
    write_exe(
        &bin.join("rustup"),
        &format!(
            "#!/bin/sh\necho \"rustup $*\" >> '{log}'\nif [ \"$1 $2\" = 'show active-toolchain' ]; then echo '1.98.0-fake (default)'; exit 0; fi\nif [ \"$1 $2\" = 'target add' ]; then shift 2; [ \"$1\" = --toolchain ] && shift 2; for t in \"$@\"; do mkdir -p '{sysroot}/lib/rustlib/'\"$t\"/lib; done; exit 0; fi\nexit 1\n",
            log = sandbox.path("tools.log").display(),
            sysroot = sysroot.display()
        ),
    );
    sandbox.set("ICM_TOOL_RUSTC", bin.join("rustc"));
    sandbox.set("ICM_TOOL_RUSTUP", bin.join("rustup"));
    sysroot
}

fn tool_log(sandbox: &Sandbox) -> String {
    std::fs::read_to_string(sandbox.path("tools.log")).unwrap_or_default()
}

// ---- new ----------------------------------------------------------------------------

#[test]
fn new_creates_an_app_pinned_to_the_framework() {
    let sandbox = Sandbox::new();

    let created = sandbox.json(&[
        "new",
        "notes",
        "--id",
        "com.acme.notes",
        "--framework",
        "tag:v0.14.1-mobile.1",
    ]);
    assert_eq!(created["exit"], 0, "{created}");
    assert_eq!(created["project"]["package"], "notes");
    assert_eq!(created["project"]["framework"], "tag:v0.14.1-mobile.1");
    assert_eq!(created["app"]["id"], "com.acme.notes");
    assert_eq!(created["app"]["name"], "Notes");
    assert!(created["warnings"].as_array().unwrap().is_empty());
    assert!(
        created["next"][0]["cmd"]
            .as_str()
            .unwrap()
            .contains("icm doctor --fix --yes")
    );

    let app = sandbox.cwd.join("notes");
    for file in [
        "Cargo.toml",
        "icm.toml",
        "src/lib.rs",
        "src/main.rs",
        "tests/icm.rs",
        "tests/flows/smoke.ice",
        "assets/icon.png",
        "rust-toolchain.toml",
        ".gitignore",
        "AGENTS.md",
    ] {
        assert!(app.join(file).is_file(), "{file} was not written");
    }
    assert!(app.join(".git").is_dir(), "git init did not run");

    let cargo = std::fs::read_to_string(app.join("Cargo.toml")).unwrap();
    assert!(cargo.contains("name = \"notes\""), "{cargo}");
    let iced_lines: Vec<&str> = cargo.lines().filter(|l| l.starts_with("iced")).collect();
    assert_eq!(iced_lines.len(), 3, "{cargo}");
    for line in iced_lines {
        assert!(
            line.contains("git = \"https://github.com/patricksmithlaravel/iced_mobile\", tag = \"v0.14.1-mobile.1\""),
            "{line}"
        );
    }
    assert!(!cargo.contains("path = \"../"), "{cargo}");
    assert!(
        std::fs::read_to_string(app.join("src/main.rs"))
            .unwrap()
            .contains("notes::run()")
    );
    assert!(
        std::fs::read_to_string(app.join("tests/icm.rs"))
            .unwrap()
            .contains("notes::application()")
    );
    let icm = std::fs::read_to_string(app.join("icm.toml")).unwrap();
    assert!(icm.contains("id = \"com.acme.notes\""), "{icm}");
    assert!(icm.contains("package = \"notes\""), "{icm}");
    let agents = std::fs::read_to_string(app.join("AGENTS.md")).unwrap();
    assert!(
        agents.starts_with(
            "# AGENTS.md — Notes (com.acme.notes) · iced_mobile v0.14.1-mobile.1 · icm "
        ),
        "{agents}"
    );
    assert!(!agents.contains("{{"));
    assert!(agents.contains("No safe-area insets"));

    // A non-empty directory needs --force.
    let busy = sandbox.json(&["new", "notes", "--framework", "tag:v1"]);
    assert_eq!(busy["exit"], 2);
    assert_eq!(busy["errors"][0]["id"], "new.dir_not_empty");
    let forced = sandbox.json(&["new", "notes", "--framework", "rev:2571bdd35", "--force"]);
    assert_eq!(forced["exit"], 0, "{forced}");
    assert!(
        std::fs::read_to_string(app.join("Cargo.toml"))
            .unwrap()
            .contains("rev = \"2571bdd35\"")
    );
}

#[test]
fn new_validates_its_inputs_and_honours_flags() {
    let sandbox = Sandbox::new();

    let bad_source = sandbox.json(&["new", "a", "--framework", "branch:main"]);
    assert_eq!(bad_source["exit"], 2);
    assert_eq!(bad_source["errors"][0]["id"], "usage.bad_args");

    let not_fork = sandbox.json(&["new", "a", "--framework", "path:/"]);
    assert_eq!(not_fork["exit"], 2);
    assert!(
        not_fork["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("not a checkout")
    );

    let bad_id = sandbox.json(&["new", "a", "--id", "not an id", "--framework", "tag:v1"]);
    assert_eq!(bad_id["exit"], 3);
    assert_eq!(bad_id["errors"][0]["id"], "config.id_invalid");

    let bad_name = sandbox.json(&["new", "iced", "--framework", "tag:v1"]);
    assert_eq!(bad_name["exit"], 2);

    // --dry-run writes nothing.
    let planned = sandbox.json(&["new", "planned", "--framework", "tag:v1", "--dry-run"]);
    assert_eq!(planned["exit"], 0, "{planned}");
    assert_eq!(planned["plan"][0]["name"], "new.write");
    assert!(!sandbox.cwd.join("planned").exists());

    // --no-git, a placeholder id, and the build's default framework source.
    let plain = sandbox.json(&["new", "My Demo", "--no-git"]);
    assert_eq!(plain["exit"], 0, "{plain}");
    assert_eq!(plain["project"]["package"], "my-demo");
    assert_eq!(plain["app"]["id"], "com.example.my_demo");
    assert_eq!(plain["app"]["name"], "My Demo");
    assert_eq!(ids(&plain, "warnings"), vec!["app.id.placeholder"]);
    let framework = plain["project"]["framework"].as_str().unwrap();
    assert!(
        framework.starts_with("tag:")
            || framework.starts_with("rev:")
            || framework.starts_with("path:"),
        "{framework}"
    );
    assert!(!sandbox.cwd.join("My Demo/.git").exists());
}

#[test]
fn a_new_app_on_a_local_checkout_loads_as_a_project() {
    let sandbox = Sandbox::new();
    let fork = fork_root();
    let created = sandbox.json(&[
        "new",
        "local",
        "--framework",
        &format!("path:{}", fork.display()),
        "--no-git",
    ]);
    assert_eq!(created["exit"], 0, "{created}");
    let cargo = std::fs::read_to_string(sandbox.cwd.join("local/Cargo.toml")).unwrap();
    assert!(
        cargo.contains(&format!("path = \"{}\"", fork.join("test").display())),
        "{cargo}"
    );

    let mut inside = sandbox.command(&["print", "config", "--json", "-q"]);
    let _ = inside.current_dir(sandbox.cwd.join("local"));
    let config = result(&inside.output().unwrap());
    assert_eq!(config["exit"], 0, "{config}");
    assert_eq!(config["config"]["app"]["name"], "Local");
    assert_eq!(config["config"]["app"]["package"], "local");
    assert_eq!(config["app"]["id"], "com.example.local");

    // Inside another cargo workspace the app becomes its own workspace root,
    // so cargo does not hand it to the outer one.
    std::fs::write(
        sandbox.cwd.join("Cargo.toml"),
        "[workspace]\nmembers = []\n",
    )
    .unwrap();
    let nested = sandbox.json(&[
        "new",
        "nested",
        "--framework",
        &format!("path:{}", fork.display()),
        "--no-git",
    ]);
    assert_eq!(nested["exit"], 0, "{nested}");
    let cargo = std::fs::read_to_string(sandbox.cwd.join("nested/Cargo.toml")).unwrap();
    assert!(cargo.trim_end().ends_with("[workspace]"), "{cargo}");
    let mut inside = sandbox.command(&["print", "config", "--json", "-q"]);
    let _ = inside.current_dir(sandbox.cwd.join("nested"));
    let config = result(&inside.output().unwrap());
    assert_eq!(config["exit"], 0, "{config}");
    assert_eq!(config["config"]["app"]["package"], "nested");
}

// ---- check ----------------------------------------------------------------------------

#[test]
fn check_compiles_and_surfaces_rustc_errors_in_every_mode() {
    let sandbox = Sandbox::with_fixture("checkapp");

    let ok = sandbox.json(&["check", "desktop", "--offline"]);
    assert_eq!(ok["exit"], 0, "{ok}");
    assert!(
        sandbox.cwd.join("Cargo.lock").exists(),
        "the lock was not resolved"
    );
    assert_eq!(ok["targets"][0]["platform"], "desktop");
    assert_eq!(ok["targets"][0]["ok"], true);
    assert_eq!(ok["checks"]["fail"], 0);
    // No icon configured: a placeholder WARN; the id is not a placeholder.
    assert_eq!(ids(&ok, "warnings"), vec!["app.icon.placeholder"]);

    // A type error.
    let lib = sandbox.cwd.join("src/lib.rs");
    let mut text = std::fs::read_to_string(&lib).unwrap();
    text.push_str("\n/// Broken.\npub fn broken() -> u32 {\n    \"nope\"\n}\n");
    std::fs::write(&lib, &text).unwrap();
    let line = text.lines().position(|l| l.contains("\"nope\"")).unwrap() + 1;

    let failed = sandbox.json(&["check", "desktop", "--offline"]);
    assert_eq!(failed["exit"], 5, "{failed}");
    let error = &failed["errors"][0];
    assert_eq!(error["id"], "build.compile_error");
    assert_eq!(error["diagnostics"][0]["file"], "src/lib.rs");
    assert_eq!(error["diagnostics"][0]["line"], line);
    assert!(
        error["diagnostics"][0]["rendered"]
            .as_str()
            .unwrap()
            .contains("mismatched types")
    );
    assert_eq!(error["evidence"][0]["path"], "src/lib.rs");
    assert_eq!(error["evidence"][0]["line"], line);
    assert!(
        error["detail"]
            .as_str()
            .unwrap()
            .contains(&format!("at src/lib.rs:{line}"))
    );

    // Human -q: the failure, its file:line and the result, on stdout.
    let human = sandbox.run(&["check", "desktop", "--offline", "-q"]);
    assert_eq!(human.status.code(), Some(5));
    let text = stdout(&human);
    assert!(
        text.lines()
            .any(|l| l.starts_with("CHECK FAIL build.compile_error")),
        "{text}"
    );
    assert!(
        text.contains(&format!("evidence: src/lib.rs:{line}  mismatched types")),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.starts_with("RESULT fail check desktop exit=5")),
        "{text}"
    );

    // Full NDJSON: diagnostic events, then the result last.
    let full = sandbox.run(&["check", "desktop", "--offline", "--json"]);
    let events: Vec<Value> = stdout(&full)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "diagnostic" && e["level"] == "error")
    );
    assert_eq!(events.last().unwrap()["type"], "result");
}

#[test]
fn check_stops_at_config_and_dependency_problems() {
    // Two copies of iced: exit 3 before any compiling.
    let sandbox = Sandbox::with_fixture("twocopies");
    let deps = sandbox.json(&["check", "desktop"]);
    assert_eq!(deps["exit"], 3, "{deps}");
    assert_eq!(deps["errors"][0]["id"], "deps.single_iced");
    assert!(deps["targets"].is_null(), "nothing was compiled");

    // An unknown icm.toml key, at its line.
    let sandbox = Sandbox::with_fixture("badconfig");
    let config = sandbox.json(&["check", "desktop"]);
    assert_eq!(config["exit"], 3);
    assert_eq!(config["errors"][0]["id"], "config.unknown_key");
    assert_eq!(config["errors"][0]["evidence"][0]["line"], 6);

    // A missing icon and a binary that does not exist.
    let sandbox = Sandbox::with_fixture("checkapp");
    let icm = sandbox.cwd.join("icm.toml");
    let text = std::fs::read_to_string(&icm).unwrap().replace(
        "platforms",
        "icon = \"assets/missing.png\"\nbin = \"nope\"\nplatforms",
    );
    std::fs::write(&icm, text).unwrap();
    let broken = sandbox.json(&["check", "desktop"]);
    assert_eq!(broken["exit"], 3, "{broken}");
    let failed = ids(&broken, "errors");
    assert!(
        failed.contains(&"app.icon.invalid".to_string()),
        "{failed:?}"
    );
    assert!(
        failed.contains(&"config.bin_missing".to_string()),
        "{failed:?}"
    );
}

// ---- explain config.<key> -------------------------------------------------------------

#[test]
fn explain_documents_icm_toml_keys() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["explain", "config.app.id"]);
    assert!(output.status.success());
    let text = stdout(&output);
    assert!(text.starts_with("# config.app.id\n"), "{text}");
    assert!(text.contains("placeholder"), "{text}");

    let table = sandbox.json(&["explain", "config.android"]);
    assert!(
        table["doc"]
            .as_str()
            .unwrap()
            .contains("`config.android.target_sdk`")
    );

    // Catalogue ids still win, and unknown keys are usage errors.
    let id = sandbox.json(&["explain", "config.unknown_key"]);
    assert_eq!(id["entry"]["exit"], 3);
    let unknown = sandbox.json(&["explain", "config.app.colour"]);
    assert_eq!(unknown["exit"], 2);
}

// ---- doctor -----------------------------------------------------------------------------

/// A fake Android SDK (everything but what `missing` names), JDK and
/// command-line tools that log what they are asked to do.
fn fake_android(sandbox: &mut Sandbox, missing: &[&str]) -> PathBuf {
    let sdk = sandbox.path("sdk");
    let log = sandbox.path("tools.log");
    let abi = if cfg!(target_arch = "aarch64") {
        "arm64-v8a"
    } else {
        "x86_64"
    };
    let touch = |relative: &str, executable: bool| {
        if missing.iter().any(|m| relative.starts_with(m)) {
            return;
        }
        let path = sdk.join(relative);
        if executable {
            write_exe(&path, "#!/bin/sh\n");
        } else {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "").unwrap();
        }
    };
    touch("platform-tools/adb", true);
    touch("emulator/emulator", true);
    touch("build-tools/36.0.0/aapt2", true);
    touch("platforms/android-36/android.jar", false);
    touch(
        &format!("system-images/android-36/google_apis/{abi}/system.img"),
        false,
    );
    touch("licenses/android-sdk-license", false);
    if !missing.iter().any(|m| m.starts_with("ndk")) {
        std::fs::create_dir_all(sdk.join("ndk/29.0.14206865")).unwrap();
        std::fs::write(
            sdk.join("ndk/29.0.14206865/source.properties"),
            "Pkg.Revision = 29.0.14206865\n",
        )
        .unwrap();
    }
    write_exe(
        &sdk.join("cmdline-tools/latest/bin/sdkmanager"),
        &format!(
            "#!/bin/sh\necho \"sdkmanager $* JAVA_HOME=$JAVA_HOME\" >> '{}'\nroot=''\nfor a in \"$@\"; do case \"$a\" in --sdk_root=*) root=\"${{a#--sdk_root=}}\";; emulator) mkdir -p \"$root/emulator\" && touch \"$root/emulator/emulator\";; esac; done\n",
            log.display()
        ),
    );
    write_exe(
        &sdk.join("cmdline-tools/latest/bin/avdmanager"),
        &format!(
            "#!/bin/sh\necho \"avdmanager $* JAVA_HOME=$JAVA_HOME\" >> '{}'\nname=''\nwhile [ $# -gt 0 ]; do [ \"$1\" = -n ] && {{ shift; name=\"$1\"; }}; shift; done\nmkdir -p \"$ANDROID_USER_HOME/avd\" && touch \"$ANDROID_USER_HOME/avd/$name.ini\"\n",
            log.display()
        ),
    );

    let jdk = sandbox.path("jdk");
    write_exe(&jdk.join("bin/java"), "#!/bin/sh\n");
    std::fs::write(jdk.join("release"), "JAVA_VERSION=\"21.0.1\"\n").unwrap();
    write_exe(
        &jdk.join("bin/keytool"),
        &format!(
            "#!/bin/sh\necho \"keytool $* JAVA_HOME=$JAVA_HOME\" >> '{}'\nwhile [ $# -gt 0 ]; do [ \"$1\" = -keystore ] && {{ shift; touch \"$1\"; }}; shift; done\n",
            log.display()
        ),
    );

    std::fs::write(
        sandbox.path("host.toml"),
        format!(
            "android_sdk = \"{}\"\njava_home = \"{}\"\n",
            sdk.display(),
            jdk.display()
        ),
    )
    .unwrap();
    sandbox.set("ANDROID_USER_HOME", sandbox.path("android-home"));
    fake_rust(sandbox, &["aarch64-linux-android", "x86_64-linux-android"]);
    sdk
}

#[test]
fn doctor_android_reports_and_fixes_with_fake_tools() {
    let mut sandbox = Sandbox::new();
    let sdk = fake_android(&mut sandbox, &["emulator"]);
    let android_home = sandbox.path("android-home");

    let before = sandbox.json(&["doctor", "android"]);
    assert_eq!(before["exit"], 4, "{before}");
    let failed: Vec<String> = before["checks"]["failed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    for id in [
        "env.android_package_missing",
        "env.debug_keystore_missing",
        "env.avd_missing",
    ] {
        assert!(failed.contains(&id.to_string()), "{id} not in {failed:?}");
    }
    assert!(matches!(
        before["errors"][0]["fix"]["by"].as_str(),
        Some("doctor" | "doctor-yes")
    ));
    assert_eq!(before["tools"]["jdk"], "21.0.1");

    // --fix: the local fixes only; the emulator package needs --yes.
    let local = sandbox.json(&["doctor", "android", "--fix"]);
    assert_eq!(local["exit"], 4, "{local}");
    assert_eq!(local["errors"][0]["id"], "env.android_package_missing");
    assert_eq!(local["errors"][0]["fix"]["by"], "doctor-yes");
    assert!(android_home.join("debug.keystore").is_file());
    assert!(android_home.join("avd/icm-api36.ini").is_file());
    let log = tool_log(&sandbox);
    assert!(!log.contains("sdkmanager"), "{log}");
    let abi = if cfg!(target_arch = "aarch64") {
        "arm64-v8a"
    } else {
        "x86_64"
    };
    assert!(
        log.contains(&format!(
            "avdmanager create avd -n icm-api36 -k system-images;android-36;google_apis;{abi} -d pixel_9 JAVA_HOME={}",
            sandbox.path("jdk").display()
        )),
        "{log}"
    );
    assert!(log.contains("keytool -genkeypair"), "{log}");

    // --fix --yes installs the package; then everything passes.
    let all = sandbox.json(&["doctor", "android", "--fix", "--yes"]);
    assert_eq!(all["exit"], 0, "{all}");
    assert!(
        tool_log(&sandbox).contains(&format!(
            "sdkmanager --sdk_root={} --install emulator",
            sdk.display()
        )),
        "{}",
        tool_log(&sandbox)
    );
    assert!(!all["fixed"].as_array().unwrap().is_empty());
    let again = sandbox.json(&["doctor", "android"]);
    assert_eq!(again["exit"], 0, "{again}");

    // Licences are the owner's: exit 9, never accepted by icm.
    std::fs::remove_file(sdk.join("licenses/android-sdk-license")).unwrap();
    let owner = sandbox.json(&["doctor", "android", "--fix", "--yes"]);
    assert_eq!(owner["exit"], 9, "{owner}");
    assert_eq!(owner["errors"][0]["id"], "env.licenses_not_accepted");
    assert_eq!(owner["errors"][0]["fix"]["by"], "owner");
}

#[test]
fn doctor_installs_rust_targets_for_the_projects_toolchain() {
    let mut sandbox = Sandbox::new();
    let sysroot = fake_rust(&mut sandbox, &[]);
    let chrome = sandbox.path("chrome");
    write_exe(&chrome, "#!/bin/sh\n");
    sandbox.set("ICM_CHROME", &chrome);

    let missing = sandbox.json(&["doctor", "web"]);
    assert_eq!(missing["exit"], 4, "{missing}");
    assert_eq!(missing["errors"][0]["id"], "env.rust_target_missing");
    let commands: Vec<&str> = missing["errors"][0]["fix"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    assert_eq!(commands[0], "icm doctor web --fix --yes");
    assert!(
        commands[1].contains("rustup target add --toolchain 1.98.0-fake wasm32-unknown-unknown")
    );

    // Without --yes nothing is downloaded.
    let refused = sandbox.json(&["doctor", "web", "--fix"]);
    assert_eq!(refused["exit"], 4);
    assert!(!tool_log(&sandbox).contains("target add"));

    let fixed = sandbox.json(&["doctor", "web", "--fix", "--yes"]);
    assert_eq!(fixed["exit"], 0, "{fixed}");
    assert!(
        sysroot
            .join("lib/rustlib/wasm32-unknown-unknown/lib")
            .is_dir()
    );
    assert!(
        tool_log(&sandbox)
            .contains("rustup target add --toolchain 1.98.0-fake wasm32-unknown-unknown")
    );

    // --dry-run prints the plan and changes nothing.
    std::fs::remove_dir_all(sysroot.join("lib/rustlib/wasm32-unknown-unknown")).unwrap();
    let plan = sandbox.json(&["doctor", "web", "--fix", "--yes", "--dry-run"]);
    assert_eq!(plan["exit"], 0, "{plan}");
    assert_eq!(plan["plan"][0]["name"], "doctor.rustup.targets");
    assert!(!sysroot.join("lib/rustlib/wasm32-unknown-unknown").exists());
}

#[test]
fn doctor_web_installs_the_wasm_bindgen_of_the_apps_lock() {
    let mut sandbox = Sandbox::with_fixture("checkapp");
    fake_rust(&mut sandbox, &["wasm32-unknown-unknown"]);
    let chrome = sandbox.path("chrome");
    write_exe(&chrome, "#!/bin/sh\n");
    sandbox.set("ICM_CHROME", &chrome);
    std::fs::write(
        sandbox.cwd.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"check-app\"\nversion = \"0.2.0\"\n\n[[package]]\nname = \"iced\"\nversion = \"0.14.1\"\n\n[[package]]\nname = \"wasm-bindgen\"\nversion = \"0.2.100\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
    )
    .unwrap();

    // A cargo that installs a fake wasm-bindgen and runs the real cargo
    // for everything else.
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
    let cargo = sandbox.path("fakebin/cargo");
    write_exe(
        &cargo,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = install ]; then echo \"cargo $*\" >> '{log}'; root=''; while [ $# -gt 0 ]; do [ \"$1\" = --root ] && {{ shift; root=\"$1\"; }}; shift; done; mkdir -p \"$root/bin\"; printf '#!/bin/sh\\necho wasm-bindgen 0.2.100\\n' > \"$root/bin/wasm-bindgen\"; chmod +x \"$root/bin/wasm-bindgen\"; exit 0; fi\nexec '{real_cargo}' \"$@\"\n",
            log = sandbox.path("tools.log").display()
        ),
    );
    sandbox.set("ICM_TOOL_CARGO", &cargo);

    let missing = sandbox.json(&["doctor", "web"]);
    assert_eq!(missing["exit"], 4, "{missing}");
    assert_eq!(missing["errors"][0]["id"], "deps.wasm_bindgen_cli");
    assert_eq!(missing["errors"][0]["evidence"][0]["path"], "Cargo.lock");

    let fixed = sandbox.json(&["doctor", "web", "--fix", "--yes"]);
    assert_eq!(fixed["exit"], 0, "{fixed}");
    let log = tool_log(&sandbox);
    assert!(
        log.contains("cargo install wasm-bindgen-cli --version =0.2.100 --locked --root "),
        "{log}"
    );
    assert!(
        sandbox
            .path("cache/tools/wasm-bindgen/0.2.100/bin/wasm-bindgen")
            .is_file()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn doctor_ios_sim_creates_the_managed_simulator_with_fake_xcode() {
    let mut sandbox = Sandbox::new();
    fake_rust(&mut sandbox, &["aarch64-apple-ios-sim", "x86_64-apple-ios"]);
    let developer = sandbox.path("Xcode.app/Contents/Developer");
    std::fs::create_dir_all(developer.join("usr/bin")).unwrap();
    sandbox.set("DEVELOPER_DIR", &developer);
    write_exe(
        &sandbox.path("fakebin/xcodebuild"),
        "#!/bin/sh\necho 'Xcode 27.0'\necho 'Build version 27A266a'\n",
    );
    sandbox.set("ICM_TOOL_XCODEBUILD", sandbox.path("fakebin/xcodebuild"));

    let state = sandbox.path("devices.json");
    let runtimes = sandbox.path("runtimes.json");
    std::fs::write(
        &state,
        r#"{"devices":{"com.apple.CoreSimulator.SimRuntime.iOS-27-0":[]}}"#,
    )
    .unwrap();
    std::fs::write(
        &runtimes,
        r#"{"runtimes":[{"identifier":"com.apple.CoreSimulator.SimRuntime.iOS-27-0","version":"27.0","name":"iOS 27.0","platform":"iOS","buildversion":"24A434","isAvailable":true,
          "supportedDeviceTypes":[{"name":"iPhone 17 Pro","identifier":"com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro","productFamily":"iPhone"},
                                  {"name":"iPhone 17","identifier":"com.apple.CoreSimulator.SimDeviceType.iPhone-17","productFamily":"iPhone"}]}]}"#,
    )
    .unwrap();
    write_exe(
        &sandbox.path("fakebin/xcrun"),
        &format!(
            "#!/bin/sh\necho \"xcrun $* DEVELOPER_DIR=$DEVELOPER_DIR\" >> '{log}'\ncase \"$*\" in\n  'simctl list -j runtimes available') cat '{runtimes}' ;;\n  'simctl list -j devices') cat '{state}' ;;\n  'simctl create '*) printf '{{\"devices\":{{\"com.apple.CoreSimulator.SimRuntime.iOS-27-0\":[{{\"udid\":\"FAKE-UDID\",\"name\":\"%s\",\"state\":\"Shutdown\",\"isAvailable\":true}}]}}}}' \"$3\" > '{state}'; echo FAKE-UDID ;;\n  *) exit 1 ;;\nesac\n",
            log = sandbox.path("tools.log").display(),
            runtimes = runtimes.display(),
            state = state.display()
        ),
    );
    sandbox.set("ICM_TOOL_XCRUN", sandbox.path("fakebin/xcrun"));

    let missing = sandbox.json(&["doctor", "ios-sim"]);
    assert_eq!(missing["exit"], 4, "{missing}");
    assert_eq!(missing["errors"][0]["id"], "env.simulator_missing");
    assert_eq!(missing["errors"][0]["fix"]["by"], "doctor");
    assert_eq!(missing["tools"]["xcode"], "27.0 (27A266a)");

    let fixed = sandbox.json(&["doctor", "ios-sim", "--fix"]);
    assert_eq!(fixed["exit"], 0, "{fixed}");
    let log = tool_log(&sandbox);
    assert!(
        log.contains("xcrun simctl create icm-iPhone 17 (iOS 27.0) com.apple.CoreSimulator.SimDeviceType.iPhone-17 com.apple.CoreSimulator.SimRuntime.iOS-27-0"),
        "{log}"
    );
    assert!(log.contains(&format!("DEVELOPER_DIR={}", developer.display())));

    // No runtime at or above min_os: the 8 GB download needs --yes.
    std::fs::write(&runtimes, r#"{"runtimes":[]}"#).unwrap();
    let runtime = sandbox.json(&["doctor", "ios-sim", "--fix"]);
    assert_eq!(runtime["exit"], 4, "{runtime}");
    assert_eq!(runtime["errors"][0]["id"], "env.ios_runtime_missing");
    assert_eq!(runtime["errors"][0]["fix"]["by"], "doctor-yes");
}

#[test]
fn doctor_without_a_project_checks_the_named_platforms_only() {
    let mut sandbox = Sandbox::new();
    fake_rust(
        &mut sandbox,
        &[if cfg!(target_arch = "aarch64") {
            if cfg!(target_os = "macos") {
                "aarch64-apple-darwin"
            } else {
                "aarch64-unknown-linux-gnu"
            }
        } else if cfg!(target_os = "macos") {
            "x86_64-apple-darwin"
        } else {
            "x86_64-unknown-linux-gnu"
        }],
    );
    let desktop = sandbox.json(&["doctor", "desktop"]);
    assert_eq!(desktop["platforms"], serde_json::json!(["desktop"]));
    assert!(desktop["run_dir"].as_str().unwrap().contains("cache"));
    let requirements = desktop["requirements"].as_array().unwrap();
    assert!(
        requirements
            .iter()
            .all(|r| r["platform"].is_null() || r["platform"] == "desktop")
    );
    // Whatever this machine lacks, it is never an agent-only exit.
    assert!(
        [0, 4, 9].contains(&desktop["exit"].as_i64().unwrap()),
        "{desktop}"
    );
}

// ---- stop and ps ------------------------------------------------------------------------

/// Starts a process that outlives its parent; returns its pid.
fn orphan_sleep() -> i32 {
    let output = Command::new("/bin/sh")
        .args(["-c", "sleep 120 >/dev/null 2>&1 & echo $!"])
        .output()
        .unwrap();
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn alive(pid: i32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn wait_gone(pid: i32) -> bool {
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn stop_ends_sessions_and_ps_lists_them() {
    let sandbox = Sandbox::with_fixture("app");
    let sessions = sandbox.cwd.join("target/icm/sessions");
    std::fs::create_dir_all(&sessions).unwrap();

    let pid = orphan_sleep();
    let marker = sandbox.path("stopped.txt");
    let shut = sandbox.path("shutdown.txt");
    std::fs::write(
        sessions.join("web.json"),
        serde_json::json!({
            "v": 1, "platform": "web", "run": "20261006T000000Z-run-web-0000",
            "pid": pid, "url": "http://127.0.0.1:9/",
            "device": {"kind": "simulator", "id": "X", "name": "icm-iPhone 17 (iOS 27.0)", "managed": true},
            "stop": [["/bin/sh", "-c", format!("echo stopped > '{}'", marker.display())]],
            "shutdown": [["/bin/sh", "-c", format!("echo shut > '{}'", shut.display())]],
            "ports": {"http": 9}
        })
        .to_string(),
    )
    .unwrap();

    let listed = sandbox.json(&["ps"]);
    assert_eq!(listed["exit"], 0, "{listed}");
    assert_eq!(listed["sessions"][0]["platform"], "web");
    assert_eq!(listed["sessions"][0]["running"], true);
    assert_eq!(listed["sessions"][0]["alive"][0], pid);
    assert_eq!(listed["next"][0]["cmd"], "icm stop --all --json -q");

    let stopped = sandbox.json(&["stop", "web", "--shutdown"]);
    assert_eq!(stopped["exit"], 0, "{stopped}");
    assert!(wait_gone(pid), "the session's process is still running");
    assert_eq!(stopped["stopped"][0]["processes"][0], pid);
    assert!(marker.is_file(), "the stop command did not run");
    assert!(shut.is_file(), "the shutdown command did not run");
    assert!(!sessions.join("web.json").exists());

    // Nothing left: still ok.
    let again = sandbox.json(&["stop", "--all"]);
    assert_eq!(again["exit"], 0);
    assert!(
        again["summary"]
            .as_str()
            .unwrap()
            .contains("nothing was running")
    );
    assert_eq!(sandbox.json(&["ps"])["sessions"], serde_json::json!([]));

    // A platform or --all is required.
    assert_eq!(sandbox.json(&["stop"])["exit"], 2);
}

#[test]
fn stop_never_signals_a_reused_pid_or_shuts_down_foreign_devices() {
    let mut sandbox = Sandbox::with_fixture("app");
    // `--shutdown` also looks for the managed emulator: through a fake adb
    // that sees no devices, never the real one.
    let adb = sandbox.path("fakebin/adb");
    write_exe(&adb, "#!/bin/sh\necho 'List of devices attached'\n");
    sandbox.set("ICM_TOOL_ADB", &adb);
    let sessions = sandbox.cwd.join("target/icm/sessions");
    std::fs::create_dir_all(&sessions).unwrap();

    // A session written long before this process started: its pid was
    // reused, so it must survive.
    let pid = orphan_sleep();
    let shut = sandbox.path("shutdown.txt");
    let file = sessions.join("android.json");
    std::fs::write(
        &file,
        serde_json::json!({
            "platform": "android", "pid": pid,
            "device": {"kind": "emulator", "serial": "emulator-5554", "name": "cn_api36", "managed": true},
            "shutdown": [["/bin/sh", "-c", format!("echo shut > '{}'", shut.display())]]
        })
        .to_string(),
    )
    .unwrap();
    assert!(
        Command::new("touch")
            .args(["-t", "200001010000"])
            .arg(&file)
            .status()
            .unwrap()
            .success()
    );

    let listed = sandbox.json(&["ps"]);
    assert_eq!(listed["sessions"][0]["running"], false, "{listed}");

    let stopped = sandbox.json(&["stop", "android", "--shutdown"]);
    assert_eq!(stopped["exit"], 0, "{stopped}");
    assert_eq!(stopped["stopped"][0]["already_gone"][0], pid);
    assert!(alive(pid), "a pid the session does not own was signalled");
    assert!(!shut.exists(), "a device icm did not create was shut down");
    let _ = Command::new("/bin/kill").arg(pid.to_string()).status();
}
