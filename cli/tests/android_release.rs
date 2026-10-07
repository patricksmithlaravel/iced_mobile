//! `icm release android`, `icm verify android` and `icm diagnose play`
//! against fake tools: a fake cargo that "builds" synthetic ELF libraries,
//! and stand-ins for aapt2, bundletool (`java -jar`), the JDK's jar,
//! keytool and jarsigner, apksigner and zipalign that record their argv and
//! produce real zip files with `zip`/`unzip`. They pin the pipeline's
//! sequence, the signing through `-storepass:env`, the unsigned and owner
//! paths, the gates on the bundle and the dist layout. Nothing is built,
//! signed or installed for real; the real pipeline is verified on the
//! template (docs/icm/DESIGN.md Appendix D, "Android release").

use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_icm");

/// The upload key's certificate in the fake keytool's answers.
const SHA: &str = "09:05:5F:DE:21:70:CC:1F:3C:61:7B:50:A5:80:00:87:D3:23:08:82:2D:8F:B4:DB:34:64:16:F3:ED:92:44:69";

/// A password value that must never appear anywhere icm writes.
const PASSWORD: &str = "correct-horse-battery-staple";

const FAKE_CARGO: &str = r#"#!/bin/sh
# icm's tests: `cargo rustc --crate-type cdylib` copies a synthetic ELF
# library for the target and reports it; the rest is cargo.
if [ "$1" = rustc ]; then
  echo "cargo $*" >> "$FAKE_LOG"
  target=''; dir=''
  while [ $# -gt 0 ]; do
    case "$1" in
      --target) target="$2"; shift ;;
      --target-dir) dir="$2"; shift ;;
    esac
    shift
  done
  out="$dir/$target/release"
  mkdir -p "$out"
  cp "$FAKE_ELF/$target.so" "$out/librelease_app.so"
  echo "{\"reason\":\"compiler-artifact\",\"package_id\":\"release-app 0.3.0\",\"target\":{\"name\":\"release_app\",\"kind\":[\"cdylib\"]},\"filenames\":[\"$out/librelease_app.so\"],\"executable\":null,\"fresh\":false}"
  echo '{"reason":"build-finished","success":true}'
  exit 0
fi
exec cargo "$@"
"#;

/// `-o <path>` (or another flag's value) of the arguments.
const VALUE_OF: &str = r#"value_of() { flag="$1"; shift; next=''; for a in "$@"; do [ -n "$next" ] && { echo "$a"; return; }; [ "$a" = "$flag" ] && next=1; done; }
"#;

const FAKE_AAPT2: &str = r#"echo "aapt2 $*" >> "$FAKE_LOG"
out=$(value_of -o "$@")
case "$1" in
  compile) echo res > "$out" ;;
  link) mkdir -p "$out/res/mipmap-mdpi" && echo manifest > "$out/AndroidManifest.xml" && echo table > "$out/resources.pb" && echo png > "$out/res/mipmap-mdpi/ic_launcher.png" ;;
esac
"#;

const FAKE_STRIP: &str = r#"echo "llvm-strip $*" >> "$FAKE_LOG"
out=$(value_of -o "$@")
for a in "$@"; do in="$a"; done
cp "$in" "$out"
"#;

const FAKE_JAVA: &str = r#"echo "java $*" >> "$FAKE_LOG"
[ "$1" = -jar ] || exit 2
shift 2
cmd="$1"; shift
modules=''; output=''; bundle=''
for a in "$@"; do
  case "$a" in
    --modules=*) modules="${a#--modules=}" ;;
    --output=*) output="${a#--output=}" ;;
    --bundle=*) bundle="${a#--bundle=}" ;;
  esac
done
case "$cmd" in
  build-bundle)
    tmp="$output.tmp"; rm -rf "$tmp"; mkdir -p "$tmp/base"
    (cd "$tmp/base" && unzip -q "$modules") || exit 1
    echo config > "$tmp/BundleConfig.pb"
    rm -f "$output"; (cd "$tmp" && zip -qr "$output" BundleConfig.pb base) || exit 1
    rm -rf "$tmp" ;;
  validate)
    [ -n "$FAKE_VALIDATE_FAIL" ] && { echo "[BT:1.18.3] Error: bad bundle" >&2; exit 1; }
    echo "App Bundle information" ;;
  dump)
    case "$1" in
      manifest) cat "$FAKE_MANIFEST" ;;
      config) echo '{"optimizations":{"uncompressNativeLibraries":{"enabled":true,"alignment":"PAGE_ALIGNMENT_16K"}}}' ;;
    esac ;;
  build-apks)
    mkdir -p "$output/.x"
    (cd "$output/.x" && unzip -q "$bundle") || exit 1
    (cd "$output/.x/base" && zip -qr "$output/universal.apk" lib assets) || exit 1
    rm -rf "$output/.x" ;;
  *) exit 3 ;;
esac
"#;

const FAKE_JAR: &str = r#"echo "jar $*" >> "$FAKE_LOG"
[ "$1" = xf ] || exit 2
shift; jar="$1"; shift
unzip -qo "$jar" "$@"
"#;

const FAKE_KEYTOOL: &str = r#"echo "keytool $*" >> "$FAKE_LOG"
case " $* " in
  *" -genkeypair "*) echo key > "$(value_of -keystore "$@")" ;;
  *" -list "*)
    [ -n "$FAKE_KEY_FAIL" ] && { echo "keytool error: java.io.IOException: keystore password was incorrect"; exit 1; }
    var=$(value_of -storepass:env "$@")
    eval "value=\${$var:-}"
    [ -n "$value" ] || { echo "Cannot find environment variable: $var"; exit 1; }
    printf 'Alias name: upload\nOwner: CN=test\nCertificate fingerprints:\n\t SHA256: %s\nSubject Public Key Algorithm: 2048-bit RSA key\n' "$FAKE_SHA" ;;
  *" -printcert "*)
    for a in "$@"; do jar="$a"; done
    if unzip -l "$jar" | grep -q META-INF/UPLOAD.SF; then printf 'Owner: CN=test\n\t SHA256: %s\n' "$FAKE_SHA"; else echo 'Not a signed jar file'; fi ;;
esac
"#;

const FAKE_JARSIGNER: &str = r#"echo "jarsigner $*" >> "$FAKE_LOG"
case " $* " in
  *" -verify "*)
    for a in "$@"; do jar="$a"; done
    if unzip -l "$jar" | grep -q META-INF/UPLOAD.SF; then
      printf -- '- Signed by "CN=test"\n    Digest algorithm: SHA-256\n\njar verified.\n\nThis jar contains signatures that do not include a timestamp.\n'
    else
      printf 'no manifest.\n\njar is unsigned.\n'
    fi ;;
  *)
    for var in "$(value_of -storepass:env "$@")" "$(value_of -keypass:env "$@")"; do
      eval "value=\${$var:-}"
      [ -n "$value" ] || { echo "Cannot find environment variable: $var" >&2; exit 1; }
    done
    out=$(value_of -signedjar "$@")
    n=$#; i=0; for a in "$@"; do i=$((i+1)); [ $i -eq $((n-1)) ] && in="$a"; done
    cp "$in" "$out" && mkdir -p "$out.d/META-INF" && echo sig > "$out.d/META-INF/UPLOAD.SF" \
      && (cd "$out.d" && zip -q "$out" META-INF/UPLOAD.SF) && rm -rf "$out.d" && echo 'jar signed.' ;;
esac
"#;

const FAKE_APKSIGNER: &str = r#"echo "apksigner $*" >> "$FAKE_LOG"
case "$1" in
  sign) out=$(value_of --out "$@"); for a in "$@"; do in="$a"; done; cp "$in" "$out" ;;
  verify) echo 'Signer #1 certificate DN: CN=test' ;;
esac
"#;

const FAKE_ZIPALIGN: &str = r#"echo "zipalign $*" >> "$FAKE_LOG"
"#;

const FAKE_ADB: &str = r#"echo "adb $*" >> "$FAKE_LOG"
[ "$1" = devices ] && echo "List of devices attached"
exit 0
"#;

/// The template's `bundletool dump manifest`, for the release fixture.
const MANIFEST: &str = r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" android:compileSdkVersion="36" android:versionCode="7" android:versionName="0.3.0" package="com.acme.fixture">
  <uses-sdk android:minSdkVersion="26" android:targetSdkVersion="36"/>
  <uses-permission android:name="android.permission.INTERNET"/>
  <application android:allowBackup="true" android:extractNativeLibs="false" android:hasCode="false" android:label="Fixture">
    <activity android:configChanges="0xd000ffff" android:exported="true" android:launchMode="2" android:name="android.app.NativeActivity">
      <meta-data android:name="android.app.lib_name" android:value="release_app"/>
    </activity>
  </application>
</manifest>
"#;

struct Sandbox {
    root: tempfile::TempDir,
    env: Vec<(String, String)>,
}

fn write_exe(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn shell(body: &str) -> String {
    format!("#!/bin/sh\n{VALUE_OF}{body}")
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

/// A real 1024x1024 PNG (not the template's icon).
fn write_icon(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::File::create(path).unwrap();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), 1024, 1024);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    let data: Vec<u8> = (0..1024 * 1024)
        .flat_map(|i: u32| [(i % 251) as u8, 90, 200, 255])
        .collect();
    writer.write_image_data(&data).unwrap();
}

impl Sandbox {
    /// `None` when `zip`/`unzip` are missing (the fakes need them).
    fn new() -> Option<Sandbox> {
        for tool in ["zip", "unzip"] {
            let found = Command::new(tool)
                .arg("-v")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok();
            if !found {
                eprintln!("skipping: `{tool}` is not installed");
                return None;
            }
        }
        let root = tempfile::tempdir().unwrap();
        let path = |relative: &str| root.path().join(relative);
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        copy_dir(&fixtures.join("release"), &path("app"));
        write_icon(&path("app/assets/icon.png"));
        let config = std::fs::read_to_string(path("app/icm.toml"))
            .unwrap()
            .replace("[app]\n", "[app]\nicon = \"assets/icon.png\"\n");
        std::fs::write(
            path("app/icm.toml"),
            format!("{config}\n[store]\nprivacy_policy_url = \"https://acme.example/privacy\"\n"),
        )
        .unwrap();

        // The SDK, the NDK and a JDK, with fakes where icm runs a tool.
        let sdk = path("sdk");
        write_exe(&sdk.join("platform-tools/adb"), &shell(FAKE_ADB));
        std::fs::create_dir_all(sdk.join("build-tools/36.0.0")).unwrap();
        std::fs::create_dir_all(sdk.join("platforms/android-36")).unwrap();
        std::fs::write(sdk.join("platforms/android-36/android.jar"), b"").unwrap();
        std::fs::create_dir_all(sdk.join("ndk/29.0.14206865")).unwrap();
        std::fs::write(
            sdk.join("ndk/29.0.14206865/source.properties"),
            "Pkg.Revision = 29.0.14206865\n",
        )
        .unwrap();
        let jdk = path("jdk");
        std::fs::create_dir_all(&jdk).unwrap();
        std::fs::write(jdk.join("release"), "JAVA_VERSION=\"21.0.1\"\n").unwrap();
        for (name, body) in [
            ("java", FAKE_JAVA),
            ("jar", FAKE_JAR),
            ("keytool", FAKE_KEYTOOL),
            ("jarsigner", FAKE_JARSIGNER),
        ] {
            write_exe(&jdk.join("bin").join(name), &shell(body));
        }
        std::fs::write(
            path("host.toml"),
            format!(
                "android_sdk = \"{}\"\njava_home = \"{}\"\n",
                sdk.display(),
                jdk.display()
            ),
        )
        .unwrap();

        let bin = path("fakebin");
        write_exe(&bin.join("cargo"), FAKE_CARGO);
        for (name, body) in [
            ("aapt2", FAKE_AAPT2),
            ("llvm-strip", FAKE_STRIP),
            ("apksigner", FAKE_APKSIGNER),
            ("zipalign", FAKE_ZIPALIGN),
        ] {
            write_exe(&bin.join(name), &shell(body));
        }
        // rustc and rustup with both Android targets installed.
        let sysroot = path("sysroot");
        for target in ["aarch64-linux-android", "x86_64-linux-android"] {
            std::fs::create_dir_all(sysroot.join("lib/rustlib").join(target).join("lib")).unwrap();
        }
        write_exe(
            &bin.join("rustc"),
            &format!(
                "#!/bin/sh\ncase \"$*\" in\n  *sysroot*) echo '{}' ;;\n  *--version*) echo 'rustc 1.98.0 (fake 2026-08-18)' ;;\n  *) exit 1 ;;\nesac\n",
                sysroot.display()
            ),
        );
        write_exe(
            &bin.join("rustup"),
            "#!/bin/sh\nif [ \"$1 $2\" = 'show active-toolchain' ]; then echo '1.98.0-fake (default)'; exit 0; fi\nexit 1\n",
        );

        // The libraries the fake cargo "builds".
        let elf = path("elf");
        std::fs::create_dir_all(&elf).unwrap();
        for (target, machine) in [
            ("aarch64-linux-android", icm::android::elf::EM_AARCH64),
            ("x86_64-linux-android", icm::android::elf::EM_X86_64),
        ] {
            std::fs::write(
                elf.join(format!("{target}.so")),
                icm::android::elf::synthetic(machine, 0x4000, &["ANativeActivity_onCreate"]),
            )
            .unwrap();
        }
        std::fs::write(path("manifest.xml"), MANIFEST).unwrap();
        std::fs::write(path("bundletool.jar"), b"jar").unwrap();

        let show = |p: PathBuf| p.display().to_string();
        let env = vec![
            ("ICM_TODAY".into(), "2026-10-07".into()),
            ("ICM_CACHE_DIR".into(), show(path("cache"))),
            ("ICM_HOST_CONFIG".into(), show(path("host.toml"))),
            ("CARGO_TARGET_DIR".into(), show(path("app/target"))),
            ("ANDROID_USER_HOME".into(), show(path("android-home"))),
            ("ANDROID_AVD_HOME".into(), show(path("android-home/avd"))),
            ("ICM_TOOL_CARGO".into(), show(bin.join("cargo"))),
            ("ICM_TOOL_RUSTC".into(), show(bin.join("rustc"))),
            ("ICM_TOOL_RUSTUP".into(), show(bin.join("rustup"))),
            ("ICM_TOOL_AAPT2".into(), show(bin.join("aapt2"))),
            ("ICM_TOOL_LLVM_STRIP".into(), show(bin.join("llvm-strip"))),
            ("ICM_TOOL_APKSIGNER".into(), show(bin.join("apksigner"))),
            ("ICM_TOOL_ZIPALIGN".into(), show(bin.join("zipalign"))),
            ("ICM_TOOL_BUNDLETOOL".into(), show(path("bundletool.jar"))),
            ("FAKE_LOG".into(), show(path("tools.log"))),
            ("FAKE_ELF".into(), show(elf)),
            ("FAKE_MANIFEST".into(), show(path("manifest.xml"))),
            ("FAKE_SHA".into(), SHA.into()),
        ];
        Some(Sandbox { root, env })
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.path().join(relative)
    }

    fn dir(&self) -> PathBuf {
        self.path("app")
    }

    fn set(&mut self, key: &str, value: &str) {
        self.env.retain(|(k, _)| k != key);
        self.env.push((key.into(), value.into()));
    }

    fn unset(&mut self, key: &str) {
        self.env.retain(|(k, _)| k != key);
    }

    fn config(&self, extra: &str) {
        let path = self.dir().join("icm.toml");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(extra);
        std::fs::write(path, text).unwrap();
    }

    fn sign_with_test_key(&mut self) {
        std::fs::write(self.path("up.jks"), b"keystore").unwrap();
        self.config(&format!(
            "\n[android.signing]\nupload = {{ keystore = \"{}\", alias = \"upload\", store_pass_env = \"ICM_TEST_STOREPASS\" }}\n",
            self.path("up.jks").display()
        ));
        self.set("ICM_TEST_STOREPASS", PASSWORD);
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.path("tools.log")).unwrap_or_default()
    }

    fn clear_log(&self) {
        let _ = std::fs::remove_file(self.path("tools.log"));
    }

    fn output(&self, args: &[&str]) -> Output {
        let mut command = Command::new(BIN);
        let _ = command
            .args(args)
            .current_dir(self.dir())
            .stdin(Stdio::null());
        for var in [
            "ICM_JSON",
            "ICM_CONFIG",
            "ICM_TIMEOUT",
            "ICM_RUN_ID",
            "ICM_RUN_DIR",
            "ICM_RUN_ROOT",
            "ICM_DETACHED",
            "ICM_TOOLS_TOML",
            "ANDROID_SERIAL",
            "ANDROID_HOME",
            "ANDROID_SDK_ROOT",
            "ANDROID_NDK_HOME",
            "ANDROID_NDK_ROOT",
            "JAVA_HOME",
            "ICM_TEST_STOREPASS",
        ] {
            let _ = command.env_remove(var);
        }
        for (key, value) in &self.env {
            let _ = command.env(key, value);
        }
        command.output().unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut full = args.to_vec();
        full.extend(["--json", "-q"]);
        let output = self.output(&full);
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let last = text.lines().last().unwrap_or_else(|| {
            panic!(
                "no output; stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        let result: Value = serde_json::from_str(last).unwrap();
        assert_eq!(result["type"], "result");
        assert_eq!(output.status.code().map(i64::from), result["exit"].as_i64());
        result
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
}

/// `(id, status)` of every check event in a `--json` run's events.
fn checks(sandbox: &Sandbox, result: &Value) -> Vec<(String, String)> {
    let dir = sandbox.abs(&result["run_dir"]);
    std::fs::read_to_string(dir.join("events.ndjson"))
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event["type"] == "check")
        .map(|event| {
            (
                event["id"].as_str().unwrap().to_string(),
                event["status"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn has(checks: &[(String, String)], id: &str, status: &str) -> bool {
    checks.iter().any(|(i, s)| i == id && s == status)
}

fn zip_names(path: &Path) -> Vec<String> {
    let output = Command::new("unzip").arg("-Z1").arg(path).output().unwrap();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn an_unsigned_release_builds_the_bundle_and_every_gate() {
    let Some(sandbox) = Sandbox::new() else {
        return;
    };

    // --dry-run: the pipeline's steps, and nothing written.
    let plan = sandbox.json(&["release", "android", "--sign", "none", "--apk", "--dry-run"]);
    assert_eq!(plan["exit"], 0, "{plan}");
    let names: Vec<&str> = plan["plan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["name"].as_str().unwrap())
        .collect();
    for name in [
        "cargo.rustc.arm64-v8a",
        "cargo.rustc.x86_64",
        "aapt2.link.proto",
        "bundletool.build_bundle",
        "android.unsigned",
        "android.gates",
        "android.apk",
        "android.smoke",
    ] {
        assert!(names.contains(&name), "{name} not in {names:?}");
    }
    assert!(!sandbox.dir().join("target/icm/dist").exists());
    assert_eq!(sandbox.log(), "", "a dry run runs no tool");

    let result = sandbox.json(&[
        "release",
        "android",
        "--sign",
        "none",
        "--apk",
        "--allow-dirty",
    ]);
    assert_eq!(result["exit"], 0, "{result}");
    let found = checks(&sandbox, &result);
    for (id, status) in [
        ("android.aab.validate", "pass"),
        ("android.aab.unsigned", "warn"),
        ("android.manifest.target_sdk", "pass"),
        ("android.manifest.config_changes", "pass"),
        ("android.manifest.has_code", "pass"),
        ("android.manifest.debuggable", "pass"),
        ("android.manifest.lib_name", "pass"),
        ("android.manifest.version", "pass"),
        ("android.bundle.alignment", "pass"),
        ("android.so.abis", "pass"),
        ("android.so.export", "pass"),
        ("android.so.align16k", "pass"),
        ("store.no_agent_bridge", "pass"),
        ("android.apk.signature", "pass"),
        ("android.smoke", "skip"),
        ("release.notices", "pass"),
    ] {
        assert!(has(&found, id, status), "{id} {status} not in {found:?}");
    }
    assert_eq!(result["release"]["signed"], false);
    assert_eq!(result["release"]["uploadable"], false);

    // The dist: the unsigned bundle, the symbols, the listing icon, the
    // debug-signed universal APK, the documents.
    let aab = sandbox.abs(&result["artifacts"]["aab"]);
    assert!(
        aab.ends_with("release-app-0.3.0-7-unsigned.aab"),
        "{}",
        aab.display()
    );
    let names = zip_names(&aab);
    for entry in [
        "base/manifest/AndroidManifest.xml",
        "base/resources.pb",
        "base/res/mipmap-mdpi/ic_launcher.png",
        "base/lib/arm64-v8a/librelease_app.so",
        "base/lib/x86_64/librelease_app.so",
        "base/assets/THIRD_PARTY_NOTICES.txt",
    ] {
        assert!(names.iter().any(|n| n == entry), "{entry} not in {names:?}");
    }
    assert!(!names.iter().any(|n| n.starts_with("base/dex/")));
    let symbols = sandbox.abs(&result["artifacts"]["symbols"]);
    assert_eq!(
        zip_names(&symbols),
        ["arm64-v8a/librelease_app.so", "x86_64/librelease_app.so"]
    );
    let icon = std::fs::read(sandbox.abs(&result["artifacts"]["play_icon"])).unwrap();
    assert_eq!(&icon[16..24], &[0, 0, 2, 0, 0, 0, 2, 0], "a 512x512 PNG");
    let apk = sandbox.abs(&result["artifacts"]["apk"]);
    assert!(apk.ends_with("release-app-0.3.0-7-universal-debugkey.apk"));
    let dist = sandbox.abs(&result["artifacts"]["dist"]);
    let config = std::fs::read_to_string(
        sandbox
            .dir()
            .join("target/icm/gen/android/release/bundle/BundleConfig.json"),
    )
    .unwrap();
    assert!(config.contains("PAGE_ALIGNMENT_16K"));

    // The tools ran in order, with release flags and no signing.
    let log = sandbox.log();
    let at = |needle: &str| {
        log.find(needle)
            .unwrap_or_else(|| panic!("{needle} not run:\n{log}"))
    };
    assert!(at("cargo rustc") < at("aapt2 link --proto-format"));
    assert!(at("aapt2 link --proto-format") < at("build-bundle"));
    assert!(at("build-bundle") < at(" validate "));
    assert!(at(" validate ") < at("dump manifest"));
    assert!(log.contains("--crate-type cdylib"), "{log}");
    assert!(
        log.contains("--release") && log.contains("--locked"),
        "{log}"
    );
    assert!(log.contains("llvm-strip --strip-unneeded"), "{log}");
    assert!(!log.contains("--debug-mode"), "{log}");
    assert!(!log.contains("jarsigner -J"), "nothing is signed: {log}");
    assert!(
        log.contains("build-apks") && log.contains("--mode=universal"),
        "{log}"
    );
    assert!(
        log.contains("adb devices"),
        "the smoke test looked for a device: {log}"
    );

    // UPLOAD.md: the first release goes through the Play Console; the
    // jarsigner line is there for the owner.
    let upload = std::fs::read_to_string(dist.join("UPLOAD.md")).unwrap();
    assert!(upload.contains("first Android release"), "{upload}");
    assert!(upload.contains("Internal testing"), "{upload}");
    assert!(upload.contains("12 testers"), "{upload}");
    assert!(upload.contains("fastlane supply"), "{upload}");
    assert!(upload.contains("edits.commit"), "{upload}");
    let upload_sh = Command::new("bash")
        .arg(dist.join("upload.sh"))
        .output()
        .unwrap();
    assert_eq!(upload_sh.status.code(), Some(9));

    // icm verify android: the same gates on the dist's bundle, with the
    // release's --sign none.
    sandbox.clear_log();
    let verify = sandbox.json(&["verify", "android"]);
    assert_eq!(verify["exit"], 0, "{verify}");
    let found = checks(&sandbox, &verify);
    assert!(has(&found, "android.aab.unsigned", "warn"), "{found:?}");
    assert!(
        has(&found, "android.manifest.lib_name", "pass"),
        "{found:?}"
    );
    assert!(has(&found, "release.artifact_changed", "pass"), "{found:?}");
    assert!(sandbox.log().contains("dump manifest"));

    // A bundle built elsewhere and unsigned fails `android.aab.signed`.
    let elsewhere = sandbox.path("other.aab");
    std::fs::copy(&aab, &elsewhere).unwrap();
    let other = sandbox.json(&[
        "verify",
        "android",
        "--artifact",
        elsewhere.to_str().unwrap(),
    ]);
    assert_eq!(other["exit"], 1, "{other}");
    assert!(has(&checks(&sandbox, &other), "android.aab.signed", "fail"));

    let not_aab = sandbox.json(&["verify", "android", "--artifact", "Cargo.toml"]);
    assert_eq!(not_aab["exit"], 2, "{not_aab}");
}

#[test]
fn a_signed_release_signs_through_environment_variables() {
    let Some(mut sandbox) = Sandbox::new() else {
        return;
    };
    sandbox.sign_with_test_key();
    let result = sandbox.json(&["release", "android", "--apk", "--allow-dirty"]);
    assert_eq!(result["exit"], 0, "{result}");
    assert_eq!(result["release"]["signed"], true);
    assert_eq!(result["release"]["uploadable"], true, "{result}");
    let aab = sandbox.abs(&result["artifacts"]["aab"]);
    assert!(
        aab.ends_with("release-app-0.3.0-7.aab"),
        "{}",
        aab.display()
    );
    let found = checks(&sandbox, &result);
    assert!(has(&found, "android.aab.signed", "pass"), "{found:?}");
    assert!(
        has(&found, "android.aab.signed", "info"),
        "the missing timestamp: {found:?}"
    );

    // keytool reads the key, jarsigner signs with the algorithm it named,
    // the passwords by variable name only.
    let log = sandbox.log();
    assert!(
        log.contains("keytool -J-Duser.language=en -list -v -keystore"),
        "{log}"
    );
    assert!(
        log.contains("-storepass:env ICM_TEST_STOREPASS -keypass:env ICM_TEST_STOREPASS -sigalg SHA256withRSA -digestalg SHA-256 -signedjar"),
        "{log}"
    );
    assert!(
        log.contains("jarsigner -J-Duser.language=en -verify -verbose -certs"),
        "{log}"
    );
    assert!(!log.contains("-strict"), "{log}");
    assert!(
        log.contains("keytool -J-Duser.language=en -printcert -jarfile"),
        "{log}"
    );
    assert!(
        log.contains("--ks-pass env:ICM_TEST_STOREPASS --key-pass env:ICM_TEST_STOREPASS"),
        "{log}"
    );
    assert!(
        sandbox
            .abs(&result["artifacts"]["apk"])
            .ends_with("release-app-0.3.0-7-universal.apk")
    );

    // The password is never written anywhere.
    let dist = sandbox.abs(&result["artifacts"]["dist"]);
    let run = sandbox.abs(&result["run_dir"]);
    let mut texts = vec![log, result.to_string()];
    for dir in [dist, run] {
        let mut stack = vec![dir];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                if entry.path().is_dir() {
                    stack.push(entry.path());
                } else if let Ok(text) = std::fs::read_to_string(entry.path()) {
                    texts.push(text);
                }
            }
        }
    }
    assert!(texts.iter().all(|text| !text.contains(PASSWORD)));

    // artifacts.json records the certificate; verify compares with it.
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(sandbox.abs(&result["artifacts"]["manifest"])).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["signing"]["certificate_sha256"], SHA);
    let verify = sandbox.json(&["verify", "android"]);
    assert_eq!(verify["exit"], 0, "{verify}");
    assert!(has(
        &checks(&sandbox, &verify),
        "android.aab.signed",
        "pass"
    ));
}

#[test]
fn key_problems_are_the_owners_and_still_ship_the_unsigned_bundle() {
    let Some(mut sandbox) = Sandbox::new() else {
        return;
    };
    sandbox.sign_with_test_key();

    // A wrong password: keytool says so, the bundle ships unsigned, exit 9.
    sandbox.set("FAKE_KEY_FAIL", "1");
    let wrong = sandbox.json(&["release", "android", "--allow-dirty", "--no-smoke"]);
    assert_eq!(wrong["exit"], 9, "{wrong}");
    assert_eq!(wrong["errors"][0]["id"], "android.keystore.unreadable");
    assert!(
        wrong["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("password is wrong")
    );
    assert!(
        sandbox
            .abs(&wrong["artifacts"]["aab"])
            .ends_with("release-app-0.3.0-7-unsigned.aab")
    );
    assert_eq!(wrong["release"]["uploadable"], false);
    // The owner's plan starts with the jarsigner line.
    let first_run = wrong["owner_steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["kind"] == "once")
        .unwrap();
    assert!(
        first_run["command"]
            .as_str()
            .unwrap()
            .starts_with("jarsigner -J-Duser.language=en -keystore"),
        "{first_run}"
    );

    // The variable unset: the core's owner item, and nothing is signed.
    sandbox.unset("FAKE_KEY_FAIL");
    sandbox.unset("ICM_TEST_STOREPASS");
    sandbox.clear_log();
    let unset = sandbox.json(&["release", "android", "--allow-dirty", "--no-smoke"]);
    assert_eq!(unset["exit"], 9, "{unset}");
    assert_eq!(
        unset["errors"][0]["id"],
        "android.keystore.password_env_unset"
    );
    assert!(
        !sandbox
            .log()
            .contains("jarsigner -J-Duser.language=en -keystore")
    );
}

#[test]
fn gates_fail_on_what_the_linked_bundle_says() {
    let Some(mut sandbox) = Sandbox::new() else {
        return;
    };
    let bad = MANIFEST
        .replace(
            "android:hasCode=\"false\"",
            "android:hasCode=\"false\" android:debuggable=\"true\"",
        )
        .replace("0xd000ffff", "0x40003fff")
        .replace("android:versionCode=\"7\"", "android:versionCode=\"8\"");
    std::fs::write(sandbox.path("bad.xml"), bad).unwrap();
    let path = sandbox.path("bad.xml").display().to_string();
    sandbox.set("FAKE_MANIFEST", &path);
    let result = sandbox.json(&[
        "release",
        "android",
        "--sign",
        "none",
        "--allow-dirty",
        "--no-smoke",
    ]);
    assert_eq!(result["exit"], 1, "{result}");
    let found = checks(&sandbox, &result);
    for id in [
        "android.manifest.debuggable",
        "android.manifest.config_changes",
        "android.manifest.version",
    ] {
        assert!(has(&found, id, "fail"), "{id}: {found:?}");
    }
    assert_eq!(result["release"]["uploadable"], false);
    assert!(
        result["release"]["not_uploadable"]
            .as_str()
            .unwrap()
            .contains("unsigned")
    );

    // A target below Google Play's floor stops before the build.
    sandbox.config("\n[android]\ntarget_sdk = 34\n");
    sandbox.clear_log();
    let low = sandbox.json(&["release", "android", "--sign", "none", "--allow-dirty"]);
    assert_eq!(low["exit"], 1, "{low}");
    assert_eq!(low["errors"][0]["id"], "android.manifest.target_sdk");
    assert!(!sandbox.log().contains("cargo rustc"));
}

#[test]
fn diagnose_play_maps_google_plays_answers() {
    let Some(sandbox) = Sandbox::new() else {
        return;
    };
    std::fs::write(
        sandbox.path("supply.log"),
        "[12:00:01]: Preparing to upload for language 'en-US'...\n[!] Google Api Error: Invalid request - APK specifies a version code that has already been used.\n",
    )
    .unwrap();
    let path = sandbox.path("supply.log").display().to_string();
    let used = sandbox.json(&["diagnose", "play", &path]);
    assert_eq!(used["exit"], 1, "{used}");
    assert_eq!(used["errors"][0]["id"], "version.build_not_increased");
    assert_eq!(used["errors"][0]["evidence"][0]["line"], 2);

    std::fs::write(
        sandbox.path("denied.json"),
        "{\"error\": {\"code\": 403, \"message\": \"The caller does not have permission\", \"status\": \"PERMISSION_DENIED\"}}",
    )
    .unwrap();
    let path = sandbox.path("denied.json").display().to_string();
    let denied = sandbox.json(&["diagnose", "play", &path]);
    assert_eq!(denied["exit"], 9, "{denied}");
    assert_eq!(denied["errors"][0]["id"], "android.play.permission");

    std::fs::write(
        sandbox.path("ok.log"),
        "[12:01:00]: Successfully finished the upload to Google Play\n",
    )
    .unwrap();
    let path = sandbox.path("ok.log").display().to_string();
    let ok = sandbox.json(&["diagnose", "play", &path]);
    assert_eq!(ok["exit"], 0, "{ok}");
    assert_eq!(ok["diagnosis"]["uploaded"], true);
}
