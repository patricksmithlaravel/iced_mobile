//! `icm release ios`, `icm verify ios` and `icm diagnose altool` end to end
//! against fake tools (`tests/fixtures/fake-ios/`): a fake `cargo` that
//! reports a synthetic device Mach-O, a fake `xcrun` (actool, dsymutil,
//! dwarfdump, strip, assetutil), `codesign`, `security`, `xcodebuild` and
//! `sw_vers`, a fake App Store profile and identity. The real `plutil`,
//! `ditto`, `zip` and `unzip` run, so these tests need macOS. Nothing is
//! signed for real and no keychain is touched.
#![cfg(target_os = "macos")]

use icm::ios::{macho, profile, sha1};
use icm::platform::ios_sim::image;
use icm::platform::ios_sim::macho::PLATFORM_IOS;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_icm");
const TEAM: &str = "ABCDE12345";
const CERT: &[u8] = b"the DER certificate of Acme's Apple Distribution identity";
const IDENTITY: &str = "Apple Distribution: Acme Ltd (ABCDE12345)";
const UUID: [u8; 16] = [
    0x02, 0xF0, 0xF6, 0x19, 0xEE, 0x0D, 0x37, 0x69, 0x89, 0xD5, 0xE3, 0x2E, 0xFA, 0x6B, 0xC5, 0x2C,
];

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
    "ICM_PROVISIONING_PROFILES",
    "ICM_CODESIGN_TIMEOUT",
    "SOURCE_DATE_EPOCH",
    "ASC_KEY_ID",
    "ASC_ISSUER_ID",
    "ASC_APP_ID",
];

struct Ios {
    root: tempfile::TempDir,
    env: Vec<(String, String)>,
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

fn write_plist(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, icm::platform::ios_sim::plist::to_xml(value)).unwrap();
}

/// A device executable: the template's imports, nothing else.
fn device_exe(undefined: &[&str], extra: &[u8], minos: (u32, u32, u32)) -> Vec<u8> {
    macho::synthetic(PLATFORM_IOS, minos, (27, 0, 0), UUID, undefined, extra)
}

impl Ios {
    /// The release fixture with the owner's answers, an RGBA icon, a valid
    /// identity and a matching App Store profile.
    fn new() -> Ios {
        let ios = Ios::bare();
        ios.config(&format!(
            "\n[ios]\nteam_id = \"{TEAM}\"\nuses_non_exempt_encryption = false\nasc_app_id = \"1234567890\"\n\n[store]\nprivacy_policy_url = \"https://acme.example/privacy\"\nsupport_url = \"https://acme.example/support\"\n"
        ));
        ios.identities(&format!("  1) {} \"{IDENTITY}\"\n", sha1::hex_upper(CERT)));
        ios.profile("store.mobileprovision", "2027-06-30T12:00:00Z", vec![]);
        ios
    }

    /// The fixture with an icon but none of the owner's answers.
    fn bare() -> Ios {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("app");
        copy_dir(&fixtures().join("release"), &app);
        let state = root.path().join("state");
        std::fs::create_dir_all(state.join("sysroot/lib/rustlib/aarch64-apple-ios/lib")).unwrap();
        let developer = state.join("Xcode.app/Contents/Developer");
        std::fs::create_dir_all(developer.join("usr/bin")).unwrap();
        write_plist(
            &state.join("Xcode.app/Contents/Info.plist"),
            &json!({"CFBundleShortVersionString": "27.0", "DTXcode": "2700"}),
        );
        write_plist(
            &developer.join("Platforms/iPhoneOS.platform/Info.plist"),
            &json!({"DefaultProperties": {"DEFAULT_COMPILER": "com.apple.compilers.llvm.clang.1_0"}}),
        );
        std::fs::create_dir_all(state.join("profiles")).unwrap();
        std::fs::write(
            state.join("exe"),
            device_exe(&["_stat", "_mach_absolute_time", "_write"], b"", (16, 0, 0)),
        )
        .unwrap();

        // An icon with transparent corners: icm flattens it.
        let mut icon = image::Rgba::filled(1024, 1024, [0, 0, 0, 0]);
        for y in 100..924 {
            for x in 100..924 {
                icon.set(x, y, [40, 140, 220, 255]);
            }
        }
        std::fs::create_dir_all(app.join("assets")).unwrap();
        image::write_png(&app.join("assets/icon.png"), &icon, true).unwrap();
        let config = app.join("icm.toml");
        let mut text = std::fs::read_to_string(&config)
            .unwrap()
            .replace("[app]\n", "[app]\nicon = \"assets/icon.png\"\n");
        text.push_str(
            "\n[ios.privacy]\napi_reasons = { FileTimestamp = [\"C617.1\"], SystemBootTime = [\"35F9.1\"] }\n",
        );
        std::fs::write(&config, text).unwrap();

        Ios {
            root,
            env: vec![("ICM_TODAY".into(), "2026-10-07".into())],
        }
    }

    fn dir(&self) -> PathBuf {
        self.root.path().join("app")
    }

    fn state(&self) -> PathBuf {
        self.root.path().join("state")
    }

    fn config(&self, extra: &str) {
        let path = self.dir().join("icm.toml");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(extra);
        std::fs::write(path, text).unwrap();
    }

    fn identities(&self, lines: &str) {
        std::fs::write(self.state().join("identities.txt"), lines).unwrap();
    }

    fn profile(&self, file: &str, expires: &str, devices: Vec<String>) {
        let fixture = profile::Fixture {
            name: "Fixture App Store".into(),
            uuid: "C11C296F-D9CC-48F3-8624-50ECE5C568E2".into(),
            team: TEAM.into(),
            app_id: "com.acme.fixture".into(),
            certificates: vec![CERT.to_vec()],
            get_task_allow: !devices.is_empty(),
            devices,
            expires: expires.into(),
        };
        std::fs::write(self.state().join("profiles").join(file), fixture.bytes()).unwrap();
    }

    fn exe(&self, bytes: Vec<u8>) {
        std::fs::write(self.state().join("exe"), bytes).unwrap();
    }

    fn set(&mut self, key: &str, value: &str) {
        self.env.push((key.into(), value.into()));
    }

    fn command(&self, args: &[&str]) -> Command {
        let fakes = fixtures().join("fake-ios");
        let home = std::env::var("HOME").unwrap_or_default();
        let state = self.state();
        let mut command = Command::new(BIN);
        for var in SCRUB {
            let _ = command.env_remove(var);
        }
        let _ = command
            .args(args)
            .current_dir(self.dir())
            .stdin(Stdio::null())
            .env("ICM_CACHE_DIR", self.root.path().join("cache"))
            .env("ICM_HOST_CONFIG", self.root.path().join("no-host.toml"))
            .env("CARGO_TARGET_DIR", self.dir().join("target"))
            .env("HOME", self.root.path().join("home"))
            .env(
                "CARGO_HOME",
                std::env::var("CARGO_HOME").unwrap_or(format!("{home}/.cargo")),
            )
            .env(
                "RUSTUP_HOME",
                std::env::var("RUSTUP_HOME").unwrap_or(format!("{home}/.rustup")),
            )
            .env(
                "ICM_REAL_CARGO",
                std::env::var("CARGO").unwrap_or("cargo".into()),
            )
            .env("ICM_FAKE_STATE", &state)
            .env("ICM_FAKE_BIN", "release-app")
            .env("ICM_FAKE_IDENTITY_NAME", IDENTITY)
            .env("ICM_FAKE_TEAM", TEAM)
            .env("DEVELOPER_DIR", state.join("Xcode.app/Contents/Developer"))
            .env("ICM_KEYCHAIN", state.join("ci.keychain-db"))
            .env("ICM_PROVISIONING_PROFILES", state.join("profiles"))
            .env("ICM_TOOL_XCRUN", fakes.join("xcrun"))
            .env("ICM_TOOL_CARGO", fakes.join("cargo"))
            .env("ICM_TOOL_RUSTC", fakes.join("rustc"))
            .env("ICM_TOOL_RUSTUP", fakes.join("rustup"))
            .env("ICM_TOOL_XCODEBUILD", fakes.join("xcodebuild"))
            .env("ICM_TOOL_CODESIGN", fakes.join("codesign"))
            .env("ICM_TOOL_SECURITY", fakes.join("security"))
            .env("ICM_TOOL_SW_VERS", fakes.join("sw_vers"))
            .env("ICM_TOOL_XATTR", fakes.join("ok"));
        for (key, value) in &self.env {
            let _ = command.env(key, value);
        }
        command
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut full: Vec<&str> = args.to_vec();
        full.extend(["--json", "-q"]);
        let output = self.command(&full).output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        let last = text.lines().last().unwrap_or_else(|| {
            panic!(
                "no output; stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        let result: Value = serde_json::from_str(last).unwrap();
        assert_eq!(result["type"], "result");
        assert_eq!(
            output.status.code().map(i64::from),
            result["exit"].as_i64(),
            "{result}"
        );
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

    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.state().join(name)).unwrap_or_default()
    }
}

fn ids(result: &Value, key: &str) -> Vec<String> {
    result["checks"][key]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn warnings(result: &Value) -> Vec<String> {
    result["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["id"].as_str().unwrap().to_string())
        .collect()
}

fn zip_entry(ipa: &Path, entry: &str) -> String {
    let output = Command::new("/usr/bin/unzip")
        .arg("-p")
        .arg(ipa)
        .arg(entry)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{entry} is not in {}",
        ipa.display()
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_signed_release_writes_an_uploadable_ipa_that_verifies() {
    let ios = Ios::new();
    let result = ios.json(&["release", "ios", "--allow-dirty"]);
    assert_eq!(result["exit"], 0, "{result}");
    assert_eq!(ids(&result, "failed"), Vec::<String>::new(), "{result}");
    assert_eq!(result["release"]["signed"], true);
    assert_eq!(result["release"]["uploadable"], true, "{result}");

    let ipa = ios.abs(&result["artifacts"]["ipa"]);
    assert!(
        ipa.ends_with("target/icm/dist/0.3.0+7/ios/Fixture.ipa"),
        "{}",
        ipa.display()
    );
    assert!(
        ios.abs(&result["artifacts"]["dsym"])
            .ends_with("Fixture.app.dSYM.zip")
    );
    let names = icm::release::notices::zip_names(&ipa).unwrap();
    for name in [
        "Payload/",
        "Payload/Fixture.app/release-app",
        "Payload/Fixture.app/Info.plist",
        "Payload/Fixture.app/PrivacyInfo.xcprivacy",
        "Payload/Fixture.app/Assets.car",
        "Payload/Fixture.app/embedded.mobileprovision",
        "Payload/Fixture.app/THIRD_PARTY_NOTICES.txt",
        "Payload/Fixture.app/_CodeSignature/CodeResources",
    ] {
        assert!(names.contains(&name.to_string()), "{name} not in {names:?}");
    }
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "entries are sorted");

    // The store plist: DT keys from the (fake) Xcode, the export answer.
    let info = zip_entry(&ipa, "Payload/Fixture.app/Info.plist");
    for needle in [
        "<key>DTXcodeBuild</key>\n\t<string>27A266a</string>",
        "<key>DTSDKName</key>\n\t<string>iphoneos27.0</string>",
        "<key>DTXcode</key>\n\t<string>2700</string>",
        "<key>BuildMachineOSBuild</key>\n\t<string>26A434</string>",
        "<key>ITSAppUsesNonExemptEncryption</key>\n\t<false/>",
        "<string>iPhoneOS</string>",
        "<key>CFBundleIconName</key>\n\t<string>AppIcon</string>",
    ] {
        assert!(info.contains(needle), "{needle} not in\n{info}");
    }
    assert!(zip_entry(&ipa, "Payload/Fixture.app/PrivacyInfo.xcprivacy").contains("C617.1"));

    // Signed with the identity, the distribution entitlements, the keychain.
    let codesign = ios.log("codesign.log");
    let sha = sha1::hex_upper(CERT);
    assert!(
        codesign.contains(&format!("--force --sign {sha} --entitlements ")),
        "{codesign}"
    );
    assert!(
        codesign.contains(&format!(
            "--generate-entitlement-der --timestamp=none --keychain {}",
            ios.state().join("ci.keychain-db").display()
        )),
        "{codesign}"
    );
    let signed = ios.log("signed-entitlements.plist");
    assert!(
        signed.contains("<string>ABCDE12345.com.acme.fixture</string>"),
        "{signed}"
    );
    assert!(
        signed.contains("<key>get-task-allow</key>\n\t<false/>"),
        "{signed}"
    );
    assert!(
        signed.contains("<key>beta-reports-active</key>\n\t<true/>"),
        "{signed}"
    );
    assert!(ios.log("security.log").contains(&format!(
        "find-identity -p codesigning {}",
        ios.state().join("ci.keychain-db").display()
    )));

    // The icon was flattened onto the background: an opaque RGB PNG.
    let flat = ios
        .dir()
        .join("target/icm/gen/ios/release/Assets.xcassets/AppIcon.appiconset/AppIcon.png");
    let pixels = image::read_png(&flat).unwrap();
    assert!(!pixels.has_transparency());

    // artifacts.json and UPLOAD.md.
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(ios.abs(&result["artifacts"]["manifest"])).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["signing"]["profile"]["type"], "app-store");
    assert_eq!(manifest["signing"]["identity_sha1"], sha);
    assert_eq!(manifest["tools"]["xcode"], "27.0 (27A266a)");
    assert_eq!(manifest["tools"]["sdk"], "iphoneos27.0");
    let kinds: Vec<&str> = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap())
        .collect();
    for kind in ["ipa", "dsym", "info_plist", "privacy", "notices"] {
        assert!(kinds.contains(&kind), "{kind} not in {kinds:?}");
    }
    let upload = std::fs::read_to_string(ios.abs(&result["artifacts"]["upload_md"])).unwrap();
    assert!(
        upload.contains("--build-status --apple-id 1234567890"),
        "{upload}"
    );
    assert!(upload.contains("Transporter"), "{upload}");
    assert!(upload.contains("icm shot ios-sim --store"), "{upload}");

    // verify reruns the gates on the IPA.
    let verify = ios.json(&["verify", "ios"]);
    assert_eq!(verify["exit"], 0, "{verify}");
    assert_eq!(ids(&verify, "failed"), Vec::<String>::new(), "{verify}");
    assert!(verify["checks"]["pass"].as_u64().unwrap() > 20, "{verify}");
}

#[test]
fn an_unsigned_release_warns_where_the_owner_must_act() {
    let ios = Ios::bare();
    let result = ios.json(&["release", "ios", "--sign", "none", "--allow-dirty"]);
    assert_eq!(result["exit"], 0, "{result}");
    assert_eq!(result["release"]["uploadable"], false);
    assert_eq!(result["release"]["signed"], false);
    let warned = warnings(&result);
    for id in [
        "config.owner_decision",
        "ios.sign.no_identity",
        "ios.plist.export_compliance",
        "store.metadata_missing",
    ] {
        assert!(warned.contains(&id.to_string()), "{id} not in {warned:?}");
    }
    // Ad hoc, without entitlements (no team yet).
    let codesign = ios.log("codesign.log");
    let sign = codesign
        .lines()
        .find(|l| l.starts_with("codesign --force"))
        .unwrap_or_else(|| panic!("{codesign}"));
    assert!(
        sign.starts_with("codesign --force --sign - --timestamp=none /"),
        "{sign}"
    );
    let ipa = ios.abs(&result["artifacts"]["ipa"]);
    let names = icm::release::notices::zip_names(&ipa).unwrap();
    assert!(
        !names
            .iter()
            .any(|n| n.ends_with("embedded.mobileprovision"))
    );
    assert!(
        !names
            .iter()
            .any(|n| n.contains("/._") || n.starts_with("__MACOSX"))
    );

    // Unsigned artifacts verify with WARNs, as their release saw them.
    let verify = ios.json(&["verify", "ios", "--artifact", ipa.to_str().unwrap()]);
    assert_eq!(verify["exit"], 0, "{verify}");
    let warned = warnings(&verify);
    assert!(
        warned.contains(&"ios.sign.no_profile".to_string()),
        "{warned:?}"
    );
    assert!(
        warned.contains(&"ios.sign.no_identity".to_string()),
        "{warned:?}"
    );

    // An unsigned IPA built twice is the same file (sorted entries, fixed
    // times, an ad-hoc signature).
    let first = icm::hash::sha256_file(&ipa).unwrap();
    let again = ios.json(&["release", "ios", "--sign", "none", "--allow-dirty"]);
    assert_eq!(again["exit"], 0, "{again}");
    assert_eq!(icm::hash::sha256_file(&ipa).unwrap(), first);
}

#[test]
fn missing_or_wrong_signing_assets_stop_for_the_owner() {
    // No profile at all.
    let ios = Ios::new();
    std::fs::remove_file(ios.state().join("profiles/store.mobileprovision")).unwrap();
    let result = ios.json(&["release", "ios", "--allow-dirty"]);
    assert_eq!(result["exit"], 9, "{result}");
    assert_eq!(result["errors"][0]["id"], "ios.sign.no_profile");
    assert_eq!(result["errors"][0]["fix"]["by"], "owner");
    assert!(
        !ios.dir()
            .join("target/icm/dist/0.3.0+7/ios/Fixture.ipa")
            .exists()
    );
    assert!(
        !ios.log("cargo.log").lines().any(|l| l.starts_with("build")),
        "nothing was built"
    );

    // An expired profile.
    let ios = Ios::new();
    ios.profile("store.mobileprovision", "2026-09-30T00:00:00Z", vec![]);
    let result = ios.json(&["release", "ios", "--allow-dirty"]);
    assert_eq!(
        result["errors"][0]["id"], "ios.sign.profile_expired",
        "{result}"
    );

    // Expiring within a week: still the owner's; within a month: a WARN.
    let ios = Ios::new();
    ios.profile("store.mobileprovision", "2026-10-10T00:00:00Z", vec![]);
    let result = ios.json(&["release", "ios", "--allow-dirty"]);
    assert_eq!(
        result["errors"][0]["id"], "ios.sign.profile_expired",
        "{result}"
    );
    let ios = Ios::new();
    ios.profile("store.mobileprovision", "2026-10-30T00:00:00Z", vec![]);
    let result = ios.json(&["release", "ios", "--allow-dirty"]);
    assert_eq!(result["exit"], 0, "{result}");
    assert!(warnings(&result).contains(&"ios.sign.profile_expired".to_string()));

    // A development profile is not an App Store profile.
    let ios = Ios::new();
    ios.profile(
        "store.mobileprovision",
        "2027-06-30T12:00:00Z",
        vec!["UDID-1".into()],
    );
    let result = ios.json(&["release", "ios", "--allow-dirty"]);
    assert_eq!(
        result["errors"][0]["id"], "ios.sign.profile_mismatch",
        "{result}"
    );

    // No identity in the keychain.
    let ios = Ios::new();
    ios.identities("");
    let result = ios.json(&["release", "ios", "--allow-dirty"]);
    assert_eq!(
        result["errors"][0]["id"], "ios.sign.no_identity",
        "{result}"
    );

    // A named identity the system does not trust: built and signed, then
    // exit 9 for the owner.
    let ios = Ios::new();
    ios.identities(&format!(
        "  1) {} \"{IDENTITY}\" (CSSMERR_TP_NOT_TRUSTED)\n",
        sha1::hex_upper(CERT)
    ));
    let text = std::fs::read_to_string(ios.dir().join("icm.toml"))
        .unwrap()
        .replace(
            "[ios]\n",
            &format!(
                "[ios]\nsigning = {{ distribution = {{ identity = \"{}\", profile = \"auto\" }} }}\n",
                sha1::hex_upper(CERT)
            ),
        );
    std::fs::write(ios.dir().join("icm.toml"), text).unwrap();
    let result = ios.json(&["release", "ios", "--allow-dirty"]);
    assert_eq!(result["exit"], 9, "{result}");
    assert_eq!(result["errors"][0]["id"], "ios.sign.no_identity");
    assert!(
        result["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("CSSMERR_TP_NOT_TRUSTED")
    );
    assert!(ios.abs(&result["artifacts"]["ipa"]).is_file(), "{result}");
    assert_eq!(result["release"]["uploadable"], false);
}

#[test]
fn broken_builds_fail_their_gates() {
    let mut ios = Ios::new();
    // DiskSpace without a reason, the agent bridge, the wrong minos, an
    // empty dSYM and a transparent icon.
    ios.exe(device_exe(
        &["_stat", "_mach_absolute_time", "_statfs"],
        b"..ICM_AGENT_BRIDGE_V1..",
        (15, 0, 0),
    ));
    ios.set("ICM_FAKE_DSYM", "empty");
    ios.set("ICM_FAKE_ICON", "clear");
    let result = ios.json(&["release", "ios", "--allow-dirty"]);
    assert_eq!(result["exit"], 1, "{result}");
    let failed = ids(&result, "failed");
    for id in [
        "ios.macho.minos",
        "store.no_agent_bridge",
        "ios.privacy.reasons",
        "ios.dsym.line_tables",
        "ios.icon.opaque_1024",
    ] {
        assert!(failed.contains(&id.to_string()), "{id} not in {failed:?}");
    }
    assert_eq!(result["release"]["uploadable"], false);
    // The privacy failure's fix is the line to add.
    let privacy = result["errors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == "ios.privacy.reasons")
        .unwrap_or_else(|| panic!("{result}"));
    assert!(
        privacy["fix"]["summary"]
            .as_str()
            .unwrap()
            .contains("DiskSpace = [\"E174.1\"]"),
        "{privacy}"
    );

    // A simulator binary is refused outright.
    let ios = Ios::new();
    ios.exe(icm::platform::ios_sim::macho::synthetic(
        7,
        (16, 0, 0),
        (27, 0, 0),
    ));
    let result = ios.json(&["release", "ios", "--allow-dirty"]);
    assert_eq!(result["errors"][0]["id"], "ios.macho.platform", "{result}");
}

#[test]
fn a_beta_xcode_warns_and_a_hanging_codesign_is_a_keychain_prompt() {
    let mut ios = Ios::new();
    ios.set("ICM_FAKE_XCODE_BUILD", "27A5230g");
    let result = ios.json(&["release", "ios", "--allow-dirty"]);
    assert_eq!(result["exit"], 0, "{result}");
    assert!(warnings(&result).contains(&"ios.xcode.not_beta".to_string()));

    let mut ios = Ios::new();
    ios.set("ICM_FAKE_CODESIGN", "hang");
    ios.set("ICM_CODESIGN_TIMEOUT", "1");
    let result = ios.json(&["release", "ios", "--allow-dirty"]);
    assert_eq!(result["exit"], 9, "{result}");
    assert_eq!(result["errors"][0]["id"], "ios.sign.keychain_prompt");
}

#[test]
fn verify_rejects_a_broken_ipa() {
    let ios = Ios::new();
    let staging = ios.root.path().join("bad");
    let app = staging.join("Payload/Fixture.app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("Info.plist"), "x").unwrap();
    std::fs::write(app.join("._Info.plist"), "x").unwrap();
    std::fs::create_dir_all(staging.join("__MACOSX")).unwrap();
    std::fs::write(staging.join("__MACOSX/._x"), "x").unwrap();
    let ipa = ios.root.path().join("bad.ipa");
    let status = Command::new("/usr/bin/zip")
        .args(["-qr"])
        .arg(&ipa)
        .args(["Payload", "__MACOSX"])
        .current_dir(&staging)
        .status()
        .unwrap();
    assert!(status.success());
    let result = ios.json(&["verify", "ios", "--artifact", ipa.to_str().unwrap()]);
    assert_ne!(result["exit"], 0, "{result}");
    let failed = ids(&result, "failed");
    assert!(failed.contains(&"ios.ipa.layout".to_string()), "{result}");
}

#[test]
fn diagnose_reads_altool_output() {
    let ios = Ios::new();
    let validate = ios.root.path().join("validate.json");
    std::fs::write(
        &validate,
        r#"{"tool-version":"27.0.5","product-errors":[{"code":-19208,"message":"Validation failed","userInfo":{"NSLocalizedFailureReason":"Invalid large app icon. The large app icon in the asset catalog can't be transparent or contain an alpha channel. (ITMS-90717)"}}]}"#,
    )
    .unwrap();
    let result = ios.json(&["diagnose", "altool", validate.to_str().unwrap()]);
    assert_eq!(result["exit"], 1, "{result}");
    assert_eq!(result["errors"][0]["id"], "ios.icon.opaque_1024");
    assert_eq!(
        result["errors"][0]["evidence"][0]["path"],
        validate.display().to_string()
    );

    let upload = ios.root.path().join("upload.json");
    std::fs::write(
        &upload,
        r#"{"success-message":"No errors uploading","details":{"delivery-uuid":"9f1c2b3a-0000-4000-8000-000000000001"}}"#,
    )
    .unwrap();
    let result = ios.json(&["diagnose", "altool", upload.to_str().unwrap()]);
    assert_eq!(result["exit"], 0, "{result}");
    assert_eq!(
        result["delivery_id"],
        "9f1c2b3a-0000-4000-8000-000000000001"
    );

    let auth = ios.root.path().join("auth.json");
    std::fs::write(
        &auth,
        r#"{"product-errors":[{"code":-19209,"message":"Failed to authenticate for session: (401) NOT_AUTHORIZED"}]}"#,
    )
    .unwrap();
    let result = ios.json(&["diagnose", "altool", auth.to_str().unwrap()]);
    assert_eq!(result["exit"], 9, "{result}");
    assert_eq!(result["errors"][0]["id"], "ios.asc.auth");
}

#[test]
fn the_dry_run_prints_the_pipeline_and_writes_nothing() {
    let ios = Ios::new();
    let result = ios.json(&["release", "ios", "--dry-run"]);
    assert_eq!(result["exit"], 0, "{result}");
    let steps: Vec<&str> = result["plan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    for step in [
        "release.preconditions",
        "cargo.build",
        "ios.dsymutil",
        "ios.dwarfdump",
        "ios.actool",
        "ios.codesign",
        "ios.ipa.zip",
        "release.manifest",
    ] {
        assert!(steps.contains(&step), "{step} not in {steps:?}");
    }
    assert!(!ios.dir().join("target/icm/dist").exists());
    assert!(
        ios.log("cargo.log")
            .lines()
            .all(|l| l.starts_with("metadata"))
    );
    assert!(ios.log("codesign.log").is_empty());

    // The xcodebuild export fallback is not built until the owner's first
    // upload shows it is needed.
    let export = ios.json(&["release", "ios", "--via-xcode-export", "--dry-run"]);
    assert_eq!(export["exit"], 2, "{export}");
    assert_eq!(export["errors"][0]["id"], "usage.not_implemented");
}
