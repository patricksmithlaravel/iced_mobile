//! The desktop release pipelines (design §11.4 to §11.6): macOS, Windows
//! and Linux, on a copy of `tests/fixtures/release`.
//!
//! - Every host refuses the other desktop targets (`env.unsupported_host`).
//! - macOS (on macOS only): an ad-hoc-signed release and its DMG with the
//!   real tools (cargo, dsymutil, iconutil, codesign, ditto, hdiutil), and
//!   the signed two-stage flow with fake `security`, `codesign`, `spctl`
//!   and `xcrun stapler` standing in for the owner's identity and Apple's
//!   notary service. Nothing touches the user's keychains: `ICM_KEYCHAIN`
//!   always names a file of the test.
//! - Windows and Linux run on any Unix host with `ICM_HOST_OS` and fake
//!   tools (cargo's build step, rc, wix, makensis, signtool, the signing
//!   command; dpkg-deb, dpkg-shlibdeps, appimagetool): the generated files,
//!   the gates, the dist directory and `icm verify`. Their real-host runs
//!   are the CI jobs of `.github/workflows/icm-desktop.yml`.

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
    "ICM_HOST_OS",
    "ASC_KEY_ID",
    "ASC_ISSUER_ID",
    "RELEASE_TAG",
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
];

struct App {
    root: tempfile::TempDir,
    env: Vec<(String, String)>,
}

impl App {
    fn new() -> App {
        let root = tempfile::tempdir().unwrap();
        copy_dir(&fixtures().join("release"), &root.path().join("app"));
        let app = App {
            root,
            env: vec![("ICM_TODAY".into(), "2026-10-07".into())],
        };
        // A real icon (the placeholder is the owner's to replace).
        let icon = icm::release::desktop::icons::encode(&blue_disc());
        std::fs::create_dir_all(app.dir().join("assets")).unwrap();
        std::fs::write(app.dir().join("assets/icon.png"), icon).unwrap();
        let path = app.dir().join("icm.toml");
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("[app]\n", "[app]\nicon = \"assets/icon.png\"\npublisher = \"Acme Ltd\"\ncopyright = \"© 2026 Acme Ltd\"\ndescription = \"A release fixture.\"\ncategory = \"utilities\"\nresources = [\"data/*.txt\"]\n");
        std::fs::write(&path, text).unwrap();
        std::fs::create_dir_all(app.dir().join("data")).unwrap();
        std::fs::write(app.dir().join("data/hello.txt"), "hello\n").unwrap();
        app
    }

    fn dir(&self) -> PathBuf {
        self.root.path().join("app")
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.path().join(relative)
    }

    fn set(&mut self, key: &str, value: impl AsRef<std::ffi::OsStr>) {
        self.env.retain(|(k, _)| k != key);
        self.env
            .push((key.into(), value.as_ref().to_string_lossy().into_owned()));
    }

    fn config(&self, extra: &str) {
        let path = self.dir().join("icm.toml");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(extra);
        std::fs::write(path, text).unwrap();
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

    /// The `check` events of a run: `(id, status, detail)`.
    fn checks(&self, result: &Value) -> Vec<(String, String, String)> {
        let events = self.abs(&result["run_dir"]).join("events.ndjson");
        std::fs::read_to_string(events)
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|event| event["type"] == "check")
            .map(|event| {
                (
                    event["id"].as_str().unwrap_or("").to_string(),
                    event["status"].as_str().unwrap_or("").to_string(),
                    event["detail"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect()
    }

    fn status_of(&self, result: &Value, id: &str) -> Vec<String> {
        self.checks(result)
            .into_iter()
            .filter(|(check, _, _)| check == id)
            .map(|(_, status, _)| status)
            .collect()
    }

    /// A fake tool in `fakebin/`, used through `ICM_TOOL_<NAME>`.
    fn fake(&mut self, name: &str, script: &str) -> PathBuf {
        let path = self.path(&format!("fakebin/{name}"));
        write_exe(&path, script);
        let var = format!(
            "ICM_TOOL_{}",
            name.to_ascii_uppercase().replace(['-', '.'], "_")
        );
        self.set(&var, &path);
        path
    }
}

fn blue_disc() -> icm::raster::Image {
    let mut image = icm::raster::Image::filled(1024, 1024, [0, 0, 0, 0]);
    for y in 0..1024u32 {
        for x in 0..1024u32 {
            let (dx, dy) = (f64::from(x) - 512.0, f64::from(y) - 512.0);
            if dx * dx + dy * dy < 400.0 * 400.0 {
                image.set_pixel(x, y, [30, 90, 200, 255]);
            }
        }
    }
    image
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

fn real_cargo() -> String {
    String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v cargo"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string()
}

/// A cargo that writes `artifact` where `subcommand` would put the
/// executable (`<target-dir>/<relative>`) and runs real cargo for
/// everything else (`metadata` for the project and the notices).
fn fake_cargo(app: &mut App, subcommand: &str, relative: &str, artifact: &Path) {
    let log = app.path("cargo.log");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "{subcommand}" ]; then
    echo "$*" >> '{log}'
    dir=
    previous=
    for arg in "$@"; do
        if [ "$previous" = "--target-dir" ]; then dir=$arg; fi
        previous=$arg
    done
    mkdir -p "$(dirname "$dir/{relative}")"
    cp '{artifact}' "$dir/{relative}"
    exit 0
fi
exec '{cargo}' "$@"
"#,
        log = log.display(),
        artifact = artifact.display(),
        cargo = real_cargo(),
    );
    let _ = app.fake("cargo", &script);
}

// ---- hosts -------------------------------------------------------------------------

#[test]
fn desktop_targets_are_refused_on_other_hosts() {
    let mut app = App::new();
    for (host, target, needs) in [
        ("macos", "windows", "built on Windows"),
        ("macos", "linux", "built on Linux"),
        ("linux", "macos", "built on macOS"),
        ("linux", "windows", "built on Windows"),
        ("windows", "linux", "built on Linux"),
    ] {
        app.set("ICM_HOST_OS", host);
        let result = app.json(&["release", target, "--sign", "none", "--allow-dirty"]);
        assert_eq!(result["exit"], 4, "{host} {target}: {result}");
        assert_eq!(result["errors"][0]["id"], "env.unsupported_host");
        let detail = result["errors"][0]["detail"].as_str().unwrap();
        assert!(detail.contains(needs), "{detail}");
        assert!(!app.dir().join("target/icm/release-target").exists());
    }
    // The plan still prints anywhere, and says which host it needs.
    app.set("ICM_HOST_OS", "macos");
    let plan = app.json(&["release", "windows", "--sign", "none", "--dry-run"]);
    assert_eq!(plan["exit"], 0, "{plan}");
    let names: Vec<&str> = plan["plan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["name"].as_str().unwrap())
        .collect();
    for name in ["windows.host", "rc", "cargo.rustc", "wix.build", "makensis"] {
        assert!(names.contains(&name), "{name} not in {names:?}");
    }
    let rustc = plan["plan"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["name"] == "cargo.rustc")
        .unwrap();
    let display = rustc["display"].as_str().unwrap();
    assert!(display.contains("target-feature=+crt-static"), "{display}");
    assert!(display.contains("link-arg="), "{display}");
    assert!(!app.dir().join("target/icm/dist").exists());
}

// ---- macOS ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
#[test]
fn an_unsigned_macos_release_builds_a_real_app_and_dmg() {
    let mut app = App::new();
    // A keychain path that does not exist: icm never looks at the user's.
    app.set("ICM_KEYCHAIN", app.path("no.keychain-db"));

    let plan = app.json(&["release", "macos", "--sign", "none", "--dry-run"]);
    assert_eq!(plan["exit"], 0, "{plan}");

    let result = app.json(&["release", "macos", "--sign", "none", "--allow-dirty"]);
    assert_eq!(result["exit"], 0, "{result}");
    assert_eq!(result["release"]["uploadable"], false);
    for (id, status) in [
        ("macos.arch", "pass"),
        ("macos.min_os", "pass"),
        ("macos.dsym", "pass"),
        ("macos.bundle", "pass"),
        ("macos.sign.verify", "pass"),
        ("macos.hardened_runtime", "pass"),
        ("macos.gatekeeper", "info"),
    ] {
        assert_eq!(app.status_of(&result, id), [status], "{id}");
    }
    assert!(ids(&result, "warnings").contains(&"macos.sign.no_developer_id".to_string()));
    let bundle = app.abs(&result["artifacts"]["app"]);
    assert!(bundle.ends_with("target/icm/dist/0.3.0+7/macos/Fixture.app"));
    for file in [
        "Contents/Info.plist",
        "Contents/PkgInfo",
        "Contents/MacOS/release-app",
        "Contents/Resources/AppIcon.icns",
        "Contents/Resources/THIRD_PARTY_NOTICES.txt",
        "Contents/Resources/data/hello.txt",
        "Contents/_CodeSignature/CodeResources",
    ] {
        assert!(bundle.join(file).is_file(), "{file}");
    }
    let plist = std::fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap();
    assert!(plist.contains("<string>public.app-category.utilities</string>"));
    assert!(plist.contains("<key>LSMinimumSystemVersion</key>\n\t<string>12.0</string>"));
    let versions =
        icm::platform::ios_sim::macho::build_versions(&bundle.join("Contents/MacOS/release-app"))
            .unwrap();
    assert_eq!(versions[0].minos_string(), "12.0");
    assert_eq!(
        versions[0].platform,
        icm::platform::ios_sim::macho::PLATFORM_MACOS
    );
    let zip = app.abs(&result["artifacts"]["app_zip"]);
    let names = icm::release::notices::zip_names(&zip).unwrap();
    assert!(names.contains(&"Fixture.app/Contents/Resources/THIRD_PARTY_NOTICES.txt".to_string()));
    let md = std::fs::read_to_string(app.abs(&result["artifacts"]["upload_md"])).unwrap();
    assert!(md.contains("notarytool"), "{md}");
    assert!(app.abs(&result["artifacts"]["dsym"]).is_file());

    // Stage 2: the DMG of the (unstapled, ad-hoc) app.
    let dmg = app.json(&[
        "release",
        "macos",
        "--dmg",
        "--sign",
        "none",
        "--allow-dirty",
    ]);
    assert_eq!(dmg["exit"], 0, "{dmg}");
    assert!(ids(&dmg, "warnings").contains(&"macos.not_stapled".to_string()));
    assert_eq!(app.status_of(&dmg, "macos.dmg"), ["pass"]);
    let image = app.abs(&dmg["artifacts"]["dmg"]);
    assert!(image.ends_with("Fixture-0.3.0.dmg"));
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(app.abs(&dmg["artifacts"]["manifest"])).unwrap(),
    )
    .unwrap();
    let roles: Vec<(String, String)> = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["kind"].as_str().unwrap().to_string(),
                f["role"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    for pair in [
        ("dmg", "upload"),
        ("app", "stage"),
        ("app_zip", "stage"),
        ("dsym", "symbols"),
    ] {
        assert!(
            roles.contains(&(pair.0.to_string(), pair.1.to_string())),
            "{pair:?} in {roles:?}"
        );
    }

    // verify mounts the DMG and checks the app inside.
    let verify = app.json(&["verify", "macos"]);
    assert_eq!(verify["exit"], 0, "{verify}");
    assert_eq!(app.status_of(&verify, "macos.dmg"), ["pass"]);
    assert_eq!(app.status_of(&verify, "macos.bundle"), ["pass"]);
    assert!(
        !app.dir()
            .join("target/icm/tmp")
            .read_dir()
            .unwrap()
            .any(|_| true)
    );
    let zip_verify = app.json(&["verify", "macos", "--artifact", zip.to_str().unwrap()]);
    assert_eq!(zip_verify["exit"], 0, "{zip_verify}");

    // The app's and the DMG's signatures are recorded by their cdhash, as
    // codesign shows it.
    let app_entry = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["kind"] == "app")
        .unwrap()
        .clone();
    let cdhash = app_entry["cdhash"]
        .as_str()
        .unwrap_or_else(|| panic!("{app_entry}"));
    assert_eq!(cdhash.len(), 40, "{cdhash}");
    let shown = Command::new("codesign")
        .args(["-d", "-vvv"])
        .arg(&bundle)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&shown.stderr).contains(&format!("CDHash={cdhash}")),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    // A changed app without a stapled ticket is still a change.
    std::fs::write(bundle.join("Contents/CodeResources"), b"not a ticket").unwrap();
    let changed = app.json(&["verify", "macos"]);
    assert_eq!(changed["exit"], 1, "{changed}");
    assert_eq!(changed["errors"][0]["id"], "release.artifact_changed");
    let detail = changed["errors"][0]["detail"].as_str().unwrap();
    assert!(
        detail.starts_with("Fixture.app changed since the release"),
        "{detail}"
    );
    assert!(
        detail.contains("no notarization ticket is stapled"),
        "{detail}"
    );
}

#[cfg(target_os = "macos")]
fn signing_fakes(app: &mut App, identities: &str) {
    let state = app.path("state");
    std::fs::create_dir_all(&state).unwrap();
    let _ = app.fake(
        "security",
        &format!("#!/bin/sh\ncat <<'EOF'\n{identities}EOF\n"),
    );
    let log = app.path("codesign.log");
    let _ = app.fake(
        "codesign",
        &format!(
            r#"#!/bin/sh
echo "$*" >> '{log}'
case " $* " in
    *" -d "*)
        echo "CodeDirectory v=20500 size=1 flags=0x10000(runtime) hashes=1+1 location=embedded" >&2
        echo "Authority=Developer ID Application: Acme Ltd (ABCDE12345)" >&2
        echo "Authority=Developer ID Certification Authority" >&2
        echo "Timestamp=Oct 7, 2026 at 12:00:00" >&2
        echo "TeamIdentifier=ABCDE12345" >&2
        echo "CDHash=$(cat '{state}/cdhash' 2>/dev/null || echo {CDHASH})" >&2
        exit 0;;
esac
exit 0
"#,
            log = log.display(),
            state = state.display(),
        ),
    );
    let _ = app.fake(
        "spctl",
        &format!(
            r#"#!/bin/sh
if [ -f '{state}/notarized' ]; then
    echo "$5: accepted" >&2; echo "source=Notarized Developer ID" >&2; exit 0
fi
echo "x: rejected" >&2
echo "source=Unnotarized Developer ID" >&2
echo "origin=Developer ID Application: Acme Ltd (ABCDE12345)" >&2
exit 3
"#,
            state = state.display()
        ),
    );
    let _ = app.fake(
        "xcrun",
        &format!(
            r#"#!/bin/sh
if [ "$1" = stapler ] && [ "$2" = validate ]; then
    if [ -f '{state}/notarized' ]; then echo "The validate action worked!"; exit 0; fi
    echo "$3 does not have a ticket stapled to it."; exit 65
fi
exec /usr/bin/xcrun "$@"
"#,
            state = state.display()
        ),
    );
}

/// The cdhash the fake codesign reports, until `state/cdhash` says
/// otherwise (a re-signed file).
#[cfg(target_os = "macos")]
const CDHASH: &str = "0123456789abcdef0123456789abcdef01234567";

/// What the owner's `xcrun stapler staple` does, as the fake tools see it:
/// a ticket in `Contents/CodeResources` of an app, or a bigger signature
/// before a disk image's UDIF trailer (so hdiutil still reads it), and the
/// `state/notarized` that the fake spctl and stapler read.
#[cfg(target_os = "macos")]
fn staple(app: &App, path: &Path) {
    if path.is_dir() {
        std::fs::write(path.join("Contents/CodeResources"), b"fake ticket").unwrap();
    } else {
        let bytes = std::fs::read(path).unwrap();
        let (data, trailer) = bytes.split_at(bytes.len() - 512);
        assert_eq!(&trailer[..4], b"koly", "a UDIF disk image");
        let mut stapled = data.to_vec();
        stapled.extend_from_slice(&[0u8; 4096]);
        stapled.extend_from_slice(trailer);
        std::fs::write(path, stapled).unwrap();
    }
    std::fs::write(app.path("state/notarized"), "").unwrap();
}

#[cfg(target_os = "macos")]
const DEVELOPER_ID: &str = r#"Policy: Code Signing
  Matching identities
  1) 1111111111111111111111111111111111111111 "Developer ID Application: Acme Ltd (ABCDE12345)"
  2) 2222222222222222222222222222222222222222 "icm-test Code Signing" (CSSMERR_TP_NOT_TRUSTED)
     2 identities found

  Valid identities only
  1) 1111111111111111111111111111111111111111 "Developer ID Application: Acme Ltd (ABCDE12345)"
     1 valid identities found
"#;

#[cfg(target_os = "macos")]
#[test]
fn a_signed_macos_release_goes_through_both_stages() {
    let mut app = App::new();
    let keychain = app.path("build.keychain-db");
    std::fs::write(&keychain, "").unwrap();
    app.set("ICM_KEYCHAIN", &keychain);

    // No Developer ID: the owner's, before anything is built.
    signing_fakes(
        &mut app,
        "  1) 2222222222222222222222222222222222222222 \"icm-test Code Signing\" (CSSMERR_TP_NOT_TRUSTED)\n     1 identities found\n",
    );
    let refused = app.json(&["release", "macos", "--allow-dirty"]);
    assert_eq!(refused["exit"], 9, "{refused}");
    assert!(ids(&refused, "errors").contains(&"macos.sign.no_developer_id".to_string()));
    assert!(!app.dir().join("target/icm/dist").exists());

    // A named, untrusted identity signs, without a timestamp, and the
    // release ends with the owner's item once written.
    signing_fakes(&mut app, DEVELOPER_ID);
    app.config("\n[desktop.macos]\nidentity = \"icm-test Code Signing\"\n");
    let test_signed = app.json(&["release", "macos", "--allow-dirty"]);
    assert_eq!(test_signed["exit"], 9, "{test_signed}");
    assert_eq!(test_signed["release"]["uploadable"], false);
    assert!(app.abs(&test_signed["artifacts"]["app_zip"]).is_file());
    let log = std::fs::read_to_string(app.path("codesign.log")).unwrap();
    assert!(log.contains("--timestamp=none"), "{log}");
    assert!(
        log.contains("--sign 2222222222222222222222222222222222222222"),
        "{log}"
    );

    // auto: the Developer ID, with a secure timestamp and the keychain.
    let path = app.dir().join("icm.toml");
    let text = std::fs::read_to_string(&path).unwrap().replace(
        "identity = \"icm-test Code Signing\"",
        "identity = \"auto\"",
    );
    std::fs::write(&path, text).unwrap();
    std::fs::remove_file(app.path("codesign.log")).unwrap();
    let stage1 = app.json(&["release", "macos", "--allow-dirty"]);
    assert_eq!(stage1["exit"], 0, "{stage1}");
    assert_eq!(stage1["release"]["signed"], true);
    assert_eq!(stage1["release"]["uploadable"], true);
    assert_eq!(
        app.status_of(&stage1, "macos.sign.no_developer_id"),
        ["pass"]
    );
    let log = std::fs::read_to_string(app.path("codesign.log")).unwrap();
    let sign_line = log.lines().find(|l| l.starts_with("--force")).unwrap();
    assert!(
        sign_line.starts_with(&format!(
            "--force --options runtime --timestamp --keychain {} --sign 1111111111111111111111111111111111111111 ",
            keychain.display()
        )),
        "{sign_line}"
    );
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(app.abs(&stage1["artifacts"]["manifest"])).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["signing"]["developer_id"], true);
    assert_eq!(manifest["signing"]["team"], "ABCDE12345");
    let steps: Vec<String> = stage1["owner_steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["argv"].as_array())
        .map(|argv| {
            argv.iter()
                .map(|w| w.as_str().unwrap_or(""))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    assert!(
        steps.iter().any(|s| s.contains("notarytool submit")),
        "{steps:?}"
    );

    // Stage 2 before the owner stapled the app: the owner's.
    let early = app.json(&["release", "macos", "--dmg", "--allow-dirty"]);
    assert_eq!(early["exit"], 9, "{early}");
    assert_eq!(early["errors"][0]["id"], "macos.not_stapled");

    // artifacts.json names the signature of the app it recorded.
    let bundle = app.abs(&stage1["artifacts"]["app"]);
    let recorded = |manifest: &Value, kind: &str| -> Value {
        manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["kind"] == kind)
            .cloned()
            .unwrap_or_else(|| panic!("no {kind} in {manifest}"))
    };
    assert_eq!(recorded(&manifest, "app")["cdhash"], CDHASH);

    // The owner notarized and stapled the app, which changes its files but
    // not its signature: verify accepts it.
    staple(&app, &bundle);
    let stapled_app = app.json(&["verify", "macos"]);
    assert_eq!(stapled_app["exit"], 0, "{stapled_app}");
    assert!(
        app.checks(&stapled_app)
            .iter()
            .any(|(id, status, detail)| id == "release.artifact_changed"
                && status == "pass"
                && detail.starts_with("Fixture.app: stapled since the release")),
        "{:?}",
        app.checks(&stapled_app)
    );

    // The DMG of the stapled app.
    let stage2 = app.json(&["release", "macos", "--dmg", "--allow-dirty"]);
    assert_eq!(stage2["exit"], 0, "{stage2}");
    assert_eq!(app.status_of(&stage2, "macos.not_stapled"), ["pass"]);
    let dmg = app.abs(&stage2["artifacts"]["dmg"]);
    assert!(dmg.is_file());
    assert_eq!(stage2["release"]["uploadable"], true);
    let manifest2: Value = serde_json::from_str(
        &std::fs::read_to_string(app.abs(&stage2["artifacts"]["manifest"])).unwrap(),
    )
    .unwrap();
    assert_eq!(recorded(&manifest2, "dmg")["cdhash"], CDHASH);
    assert_eq!(recorded(&manifest2, "app")["cdhash"], CDHASH);
    assert!(
        stage2["owner_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step["title"] == "Staple the ticket to the DMG"
                && step["note"]
                    .as_str()
                    .is_some_and(|note| note.contains("records the stapled DMG's sha256"))),
        "{stage2}"
    );

    // The owner notarized and stapled the DMG: its bytes changed, its
    // signature did not, so stage 2's last steps go through.
    staple(&app, &dmg);
    let before = recorded(&manifest2, "dmg")["sha256"].clone();
    let now = icm::hash::sha256_hex(&std::fs::read(&dmg).unwrap());
    assert_ne!(before, now);
    let verify = app.json(&["verify", "macos", "--after-notarize"]);
    assert_eq!(verify["exit"], 0, "{verify}");
    let gatekeeper = app.status_of(&verify, "macos.gatekeeper");
    assert!(
        !gatekeeper.is_empty() && gatekeeper.iter().all(|s| s == "pass"),
        "{gatekeeper:?}"
    );
    assert!(
        app.checks(&verify)
            .iter()
            .any(|(id, status, detail)| id == "release.artifact_changed"
                && status == "pass"
                && detail.starts_with("Fixture-0.3.0.dmg: stapled since the release")),
        "{:?}",
        app.checks(&verify)
    );
    // The ledger records the DMG as it shipped, stapled.
    let marked = app.json(&["ledger", "mark-uploaded", "macos"]);
    assert_eq!(marked["exit"], 0, "{marked}");
    assert_eq!(marked["upload"]["sha256"], now.as_str(), "{marked}");
    assert_eq!(app.status_of(&marked, "release.artifact_changed"), ["info"]);

    // A file signed again since the release is a change verify rejects.
    std::fs::write(app.path("state/cdhash"), "f".repeat(40)).unwrap();
    let resigned = app.json(&["verify", "macos", "--after-notarize"]);
    assert_eq!(resigned["exit"], 1, "{resigned}");
    assert_eq!(resigned["errors"][0]["id"], "release.artifact_changed");
    let detail = resigned["errors"][0]["detail"].as_str().unwrap();
    assert!(detail.contains("not the recorded"), "{detail}");
    std::fs::remove_file(app.path("state/cdhash")).unwrap();

    // diagnose notarytool.
    let accepted = app.path("notary-app.json");
    std::fs::write(
        &accepted,
        r#"{"id":"abc","message":"Processing complete","status":"Accepted"}"#,
    )
    .unwrap();
    let ok = app.json(&["diagnose", "notarytool", accepted.to_str().unwrap()]);
    assert_eq!(ok["exit"], 0, "{ok}");
    let invalid = app.path("notary-log.json");
    std::fs::write(
        &invalid,
        r#"{"status":"Invalid","issues":[{"severity":"error","path":"a.zip/A.app/Contents/MacOS/a","message":"The executable does not have the hardened runtime enabled.","architecture":"arm64"}]}"#,
    )
    .unwrap();
    let bad = app.json(&["diagnose", "notarytool", invalid.to_str().unwrap()]);
    assert_eq!(bad["exit"], 1, "{bad}");
    assert!(
        ids(&bad, "errors").contains(&"macos.hardened_runtime".to_string())
            || bad["checks"]["failed"]
                .as_array()
                .unwrap()
                .iter()
                .any(|id| id == "macos.hardened_runtime"),
        "{bad}"
    );
}

// ---- Windows -------------------------------------------------------------------------

fn windows_fakes(app: &mut App, imports: &[&str]) {
    app.set("ICM_HOST_OS", "windows");
    let pe = app.path("built.exe");
    std::fs::write(
        &pe,
        icm::release::desktop::pe::synthetic(
            icm::release::desktop::pe::MACHINE_AMD64,
            icm::release::desktop::pe::SUBSYSTEM_GUI,
            imports,
        ),
    )
    .unwrap();
    fake_cargo(
        app,
        "rustc",
        "x86_64-pc-windows-msvc/release/release-app.exe",
        &pe,
    );
    let _ = app.fake(
        "rc",
        "#!/bin/sh\n[ \"$1\" = /nologo ] && [ \"$2\" = /fo ] || exit 2\n[ -f \"$4\" ] || exit 3\necho RES > \"$3\"\n",
    );
    let _ = app.fake(
        "wix",
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 5.0.2+fake; exit 0; fi\n[ \"$1\" = build ] || exit 2\ncp \"$6\" \"$5\" && printf 'MSI\\n' >> \"$5\"\n",
    );
    let _ = app.fake(
        "makensis",
        "#!/bin/sh\nif [ \"$1\" = -VERSION ]; then echo v3.11; exit 0; fi\nfor arg; do nsi=$arg; done\nout=$(sed -n 's/^OutFile \"\\(.*\\)\"$/\\1/p' \"$nsi\")\nprintf 'MZ setup\\n' > \"$out\"\n",
    );
    let _ = app.fake(
        "signtool",
        "#!/bin/sh\nfor arg; do file=$arg; done\ngrep -q ICM-TEST-SIGNED \"$file\"\n",
    );
    let sign = app.path("fakebin/sign-it");
    write_exe(
        &sign,
        "#!/bin/sh\n[ -n \"$ICM_TEST_SIGN_TOKEN\" ] || exit 7\nprintf '\\nICM-TEST-SIGNED\\n' >> \"$1\"\n",
    );
    app.set("ICM_TEST_SIGN_TOKEN", "fake-test-token-not-a-secret");
    app.config(&format!(
        "\n[desktop.windows]\nsign_command = \"{} {{file}}\"\nsign_env = [\"ICM_TEST_SIGN_TOKEN\"]\n",
        sign.display()
    ));
}

#[test]
fn a_windows_release_builds_signs_and_gates_both_installers() {
    let mut app = App::new();
    windows_fakes(&mut app, &["KERNEL32.dll", "USER32.dll", "ntdll.dll"]);
    let result = app.json(&["release", "windows", "--allow-dirty"]);
    assert_eq!(result["exit"], 0, "{result}");
    assert_eq!(result["release"]["signed"], true);
    assert_eq!(result["release"]["uploadable"], true);
    for id in [
        "windows.msi_version",
        "windows.pe_imports",
        "windows.subsystem",
    ] {
        assert_eq!(app.status_of(&result, id), ["pass"], "{id}");
    }
    assert_eq!(
        app.status_of(&result, "windows.signed"),
        ["pass", "pass", "pass"]
    );
    let msi = app.abs(&result["artifacts"]["msi"]);
    let exe = app.abs(&result["artifacts"]["exe"]);
    assert!(msi.ends_with("target/icm/dist/0.3.0+7/windows/Fixture-0.3.0.msi"));
    assert!(exe.ends_with("target/icm/dist/0.3.0+7/windows/Fixture-0.3.0-setup.exe"));
    for file in [&msi, &exe] {
        assert!(
            std::fs::read_to_string(file)
                .unwrap()
                .contains("ICM-TEST-SIGNED")
        );
    }

    // The cargo line: the static C runtime and the resource file.
    let cargo = std::fs::read_to_string(app.path("cargo.log")).unwrap();
    assert!(
        cargo.contains("--target x86_64-pc-windows-msvc --release"),
        "{cargo}"
    );
    assert!(cargo.contains("target-feature=+crt-static"), "{cargo}");
    assert!(cargo.contains("--locked"), "{cargo}");
    let generated = app.dir().join("target/icm/gen/windows/release/installer");
    let res = cargo
        .split_whitespace()
        .find_map(|word| word.strip_prefix("link-arg="))
        .unwrap();
    assert!(Path::new(res).starts_with(&generated), "{res}");
    assert!(Path::new(res).is_file());

    // The generated sources.
    let rc = std::fs::read_to_string(generated.join("app.rc")).unwrap();
    assert!(rc.contains("FILEVERSION 0,3,0,7"), "{rc}");
    assert!(rc.contains("VALUE \"CompanyName\", \"Acme Ltd\""));
    let wxs = std::fs::read_to_string(generated.join("app.wxs")).unwrap();
    assert!(wxs.contains("Version=\"0.3.0\""));
    assert!(wxs.contains("Name=\"THIRD_PARTY_NOTICES.txt\""));
    assert!(wxs.contains("Name=\"hello.txt\""));
    let nsi = std::fs::read_to_string(generated.join("installer.nsi")).unwrap();
    assert!(nsi.contains("RequestExecutionLevel user"));
    let ico = std::fs::read(generated.join("app.ico")).unwrap();
    assert_eq!(
        icm::release::desktop::icons::ico_frames(&ico)
            .unwrap()
            .len(),
        7
    );

    let md = std::fs::read_to_string(app.abs(&result["artifacts"]["upload_md"])).unwrap();
    assert!(md.contains("gh release upload"), "{md}");

    // verify reads the PE anywhere and leaves signtool to Windows.
    app.set("ICM_HOST_OS", "macos");
    let built = app
        .dir()
        .join("target/icm/gen/windows/release/installer/release-app.exe");
    let verify = app.json(&["verify", "windows", "--artifact", built.to_str().unwrap()]);
    assert_eq!(verify["exit"], 0, "{verify}");
    assert_eq!(app.status_of(&verify, "windows.pe_imports"), ["pass"]);
    assert_eq!(app.status_of(&verify, "windows.signed"), ["skip"]);
}

#[test]
fn a_windows_build_that_needs_the_vc_runtime_fails_its_gate() {
    let mut app = App::new();
    windows_fakes(&mut app, &["KERNEL32.dll", "VCRUNTIME140.dll"]);
    let result = app.json(&["release", "windows", "--sign", "none", "--allow-dirty"]);
    assert_eq!(result["exit"], 1, "{result}");
    assert_eq!(result["errors"][0]["id"], "windows.pe_imports");
    assert!(
        result["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("vcruntime140.dll")
    );
    assert_eq!(result["release"]["uploadable"], false);
    // --sign none signed nothing.
    assert!(ids(&result, "warnings").contains(&"windows.signed".to_string()));
    let msi = app.abs(&result["artifacts"]["msi"]);
    assert!(
        !std::fs::read_to_string(msi)
            .unwrap()
            .contains("ICM-TEST-SIGNED")
    );
}

#[test]
fn windows_versions_must_fit_its_fields() {
    let mut app = App::new();
    windows_fakes(&mut app, &["KERNEL32.dll"]);
    let path = app.dir().join("icm.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("build = 7", "build = 70000");
    std::fs::write(&path, text).unwrap();
    let result = app.json(&["release", "windows", "--sign", "none", "--allow-dirty"]);
    assert_eq!(result["exit"], 3, "{result}");
    assert_eq!(result["errors"][0]["id"], "windows.msi_version");
}

// ---- Linux ---------------------------------------------------------------------------

/// A dpkg-deb that builds a real `.deb` (ar, with xz tarballs) from the
/// root it is given, and prints its control file for `--info`.
const FAKE_DPKG_DEB: &str = r#"#!/bin/sh
state=$(dirname "$0")/../state
mkdir -p "$state"
case "$1" in
--version) echo "Debian dpkg-deb 1.21.1 (fake)"; exit 0;;
--info) cat "$(cat "$state/root")/DEBIAN/control"; exit 0;;
esac
[ "$1" = -Zxz ] && [ "$2" = --build ] && [ "$3" = --root-owner-group ] || exit 2
root=$4
out=$5
echo "$root" > "$state/root"
tmp=$state/deb
rm -rf "$tmp"; mkdir -p "$tmp"
(cd "$root" && tar -cJf "$tmp/data.tar.xz" ./usr) || exit 4
(cd "$root/DEBIAN" && tar -cJf "$tmp/control.tar.xz" .) || exit 4
printf '2.0\n' > "$tmp/debian-binary"
member() {
    size=$(wc -c < "$tmp/$1" | tr -d ' ')
    printf '%-16s%-12s%-6s%-6s%-8s%-10s`\n' "$1" 0 0 0 100644 "$size"
    cat "$tmp/$1"
    if [ $((size % 2)) -eq 1 ]; then printf '\n'; fi
}
{ printf '!<arch>\n'; member debian-binary; member control.tar.xz; member data.tar.xz; } > "$out"
"#;

fn linux_fakes(app: &mut App, glibc: &str) {
    app.set("ICM_HOST_OS", "linux");
    let elf = app.path("built-elf");
    std::fs::write(
        &elf,
        icm::release::desktop::elf::synthetic(
            icm::release::desktop::elf::EM_X86_64,
            &[
                ("libc.so.6", &["GLIBC_2.2.5", glibc]),
                ("libgcc_s.so.1", &["GCC_3.0"]),
            ],
        ),
    )
    .unwrap();
    fake_cargo(app, "build", "release/release-app", &elf);
    let _ = app.fake("dpkg-deb", FAKE_DPKG_DEB);
    let _ = app.fake(
        "dpkg-shlibdeps",
        "#!/bin/sh\n[ -f debian/control ] || exit 2\necho 'shlibs:Depends=libc6 (>= 2.34), libgcc-s1 (>= 4.2)'\n",
    );
    let listing = app.path("appdir.txt");
    let _ = app.fake(
        "appimagetool",
        &format!(
            "#!/bin/sh\n[ \"$1\" = --appimage-extract-and-run ] && [ \"$2\" = --runtime-file ] || exit 2\n[ -f \"$3\" ] || exit 3\n[ \"$ARCH\" = x86_64 ] || exit 4\n(cd \"$4\" && find . | sort) > '{}'\nprintf 'AppImage\\n' > \"$5\"\n",
            listing.display()
        ),
    );
    let runtime = app.path("fakebin/runtime");
    std::fs::write(&runtime, "runtime").unwrap();
    app.set("ICM_TOOL_APPIMAGE_RUNTIME", &runtime);
    // Two of the four libraries the AppImage bundles.
    let libs = app.path("libs");
    std::fs::create_dir_all(&libs).unwrap();
    std::fs::write(libs.join("libxkbcommon.so.0.0.0"), "xkb").unwrap();
    std::os::unix::fs::symlink("libxkbcommon.so.0.0.0", libs.join("libxkbcommon.so.0")).unwrap();
    std::fs::write(libs.join("libwayland-client.so.0"), "wl").unwrap();
    app.set("ICM_LINUX_LIB_DIRS", &libs);
    let _ = app.fake(
        "desktop-file-validate",
        "#!/bin/sh\ngrep -q '^Type=Application$' \"$1\" || { echo \"$1: error: no Type\"; exit 1; }\n",
    );
    let _ = app.fake(
        "lintian",
        "#!/bin/sh\nfor arg; do deb=$arg; done\n[ -f \"$deb\" ] || exit 2\necho 'W: release-app: no-manual-page usr/bin/release-app'\n",
    );
}

#[test]
fn a_linux_release_packages_a_deb_and_an_appimage() {
    let mut app = App::new();
    linux_fakes(&mut app, "GLIBC_2.34");

    // The maintainer is the owner's to name.
    let refused = app.json(&["release", "linux", "--allow-dirty"]);
    assert_eq!(refused["exit"], 9, "{refused}");
    assert_eq!(refused["errors"][0]["id"], "config.owner_decision");
    assert!(
        refused["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("maintainer")
    );

    app.config("\n[desktop.linux]\nmaintainer = \"Acme Ltd <dev@acme.example>\"\ndeb_depends = [\"libssl3\"]\n");
    let result = app.json(&["release", "linux", "--allow-dirty"]);
    assert_eq!(result["exit"], 0, "{result}");
    assert_eq!(result["release"]["uploadable"], true);
    assert_eq!(app.status_of(&result, "linux.glibc_floor"), ["pass"]);
    assert_eq!(
        app.status_of(&result, "linux.desktop_file"),
        ["pass", "pass"]
    );
    assert_eq!(app.status_of(&result, "linux.deb.lint"), ["warn"]);
    assert_eq!(app.status_of(&result, "linux.appimage_libs"), ["warn"]);

    let deb = app.abs(&result["artifacts"]["deb"]);
    assert!(deb.ends_with("target/icm/dist/0.3.0+7/linux/release-app_0.3.0-7_amd64.deb"));
    let appimage = app.abs(&result["artifacts"]["appimage"]);
    assert!(appimage.ends_with("Fixture-0.3.0-x86_64.AppImage"));
    let root = app.dir().join("target/icm/gen/linux/release/deb/root");
    let control = std::fs::read_to_string(root.join("DEBIAN/control")).unwrap();
    assert!(control.contains("Package: release-app\nVersion: 0.3.0-7\nArchitecture: amd64\nMaintainer: Acme Ltd <dev@acme.example>\n"), "{control}");
    assert!(
        control.contains("Depends: libc6 (>= 2.34), libgcc-s1 (>= 4.2), libssl3\n"),
        "{control}"
    );
    assert!(control.contains("Section: utils\n"));
    for file in [
        "usr/bin/release-app",
        "usr/share/applications/com.acme.fixture.desktop",
        "usr/share/icons/hicolor/512x512/apps/com.acme.fixture.png",
        "usr/share/doc/release-app/THIRD_PARTY_NOTICES.txt",
        "usr/share/doc/release-app/copyright",
        "usr/share/release-app/data/hello.txt",
    ] {
        assert!(root.join(file).is_file(), "{file}");
    }
    let listing = std::fs::read_to_string(app.path("appdir.txt")).unwrap();
    for entry in [
        "./AppRun",
        "./com.acme.fixture.desktop",
        "./com.acme.fixture.png",
        "./.DirIcon",
        "./usr/bin/release-app",
        "./usr/lib/libxkbcommon.so.0",
        "./usr/lib/libwayland-client.so.0",
    ] {
        assert!(listing.lines().any(|l| l == entry), "{entry} in {listing}");
    }

    // verify unpacks the .deb itself.
    app.set("ICM_HOST_OS", "macos");
    let verify = app.json(&["verify", "linux"]);
    assert_eq!(verify["exit"], 0, "{verify}");
    assert_eq!(app.status_of(&verify, "linux.glibc_floor"), ["pass"]);
    assert_eq!(
        app.status_of(&verify, "linux.desktop_file"),
        ["pass", "pass"]
    );
    assert!(
        app.status_of(&verify, "release.notices")
            .contains(&"pass".to_string())
    );
    // An AppImage unpacks only on Linux.
    let skipped = app.json(&["verify", "linux", "--artifact", appimage.to_str().unwrap()]);
    assert_eq!(skipped["exit"], 0, "{skipped}");
    assert_eq!(app.status_of(&skipped, "linux.glibc_floor"), ["skip"]);
}

#[test]
fn a_linux_binary_above_the_glibc_floor_is_not_uploadable() {
    let mut app = App::new();
    linux_fakes(&mut app, "GLIBC_2.39");
    let result = app.json(&["release", "linux", "--sign", "none", "--allow-dirty"]);
    assert_eq!(result["exit"], 1, "{result}");
    assert_eq!(result["errors"][0]["id"], "linux.glibc_floor");
    // Unset maintainer under --sign none: a WARN and a placeholder.
    assert!(ids(&result, "warnings").contains(&"config.owner_decision".to_string()));
    let control = std::fs::read_to_string(
        app.dir()
            .join("target/icm/gen/linux/release/deb/root/DEBIAN/control"),
    )
    .unwrap();
    assert!(
        control.contains("Maintainer: Acme Ltd <maintainer-unset@invalid>"),
        "{control}"
    );
}

// ---- the real packaging tools, where installed -----------------------------------------

/// A tool on `PATH`, if installed.
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

#[test]
fn real_makensis_compiles_the_generated_installer() {
    let Some(makensis) = on_path("makensis") else {
        eprintln!("skipped: makensis is not installed");
        return;
    };
    let mut app = App::new();
    windows_fakes(&mut app, &["KERNEL32.dll", "USER32.dll"]);
    app.set("ICM_TOOL_MAKENSIS", &makensis);
    let result = app.json(&["release", "windows", "--sign", "none", "--allow-dirty"]);
    assert_eq!(result["exit"], 0, "{result}");
    let setup = app.abs(&result["artifacts"]["exe"]);
    let pe = icm::release::desktop::pe::read(&setup).unwrap();
    assert_eq!(pe.subsystem, icm::release::desktop::pe::SUBSYSTEM_GUI);
    assert!(pe.runtime_dlls().is_empty(), "{:?}", pe.dlls());
    assert!(std::fs::metadata(&setup).unwrap().len() > 30_000);
    assert!(
        result["tools"]["makensis"]
            .as_str()
            .unwrap()
            .starts_with('v')
    );
}

#[test]
fn real_dpkg_deb_builds_the_package() {
    let Some(dpkg_deb) = on_path("dpkg-deb") else {
        eprintln!("skipped: dpkg-deb is not installed");
        return;
    };
    let mut app = App::new();
    linux_fakes(&mut app, "GLIBC_2.34");
    app.set("ICM_TOOL_DPKG_DEB", &dpkg_deb);
    app.config("\n[desktop.linux]\nmaintainer = \"Acme Ltd <dev@acme.example>\"\n");
    let result = app.json(&["release", "linux", "--allow-dirty"]);
    assert_eq!(result["exit"], 0, "{result}");
    let deb = app.abs(&result["artifacts"]["deb"]);
    let info = Command::new(&dpkg_deb)
        .arg("--info")
        .arg(&deb)
        .output()
        .unwrap();
    let info = String::from_utf8_lossy(&info.stdout);
    assert!(info.contains(" Package: release-app"), "{info}");
    assert!(info.contains(" Version: 0.3.0-7"), "{info}");
    assert!(
        info.contains(" Depends: libc6 (>= 2.34), libgcc-s1 (>= 4.2)"),
        "{info}"
    );
    let contents = Command::new(&dpkg_deb)
        .arg("--contents")
        .arg(&deb)
        .output()
        .unwrap();
    let contents = String::from_utf8_lossy(&contents.stdout);
    let binary = contents
        .lines()
        .find(|line| line.ends_with("./usr/bin/release-app"))
        .unwrap_or_else(|| panic!("{contents}"));
    assert!(binary.starts_with("-rwxr-xr-x root/root"), "{binary}");
    assert!(contents.contains("./usr/share/doc/release-app/THIRD_PARTY_NOTICES.txt"));
    // icm reads the real package on any host.
    app.set("ICM_HOST_OS", "macos");
    let verify = app.json(&["verify", "linux"]);
    assert_eq!(verify["exit"], 0, "{verify}");
    assert_eq!(app.status_of(&verify, "linux.glibc_floor"), ["pass"]);
}
