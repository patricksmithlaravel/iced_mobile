//! What the docs agents read say agrees with icm. Exit 4 means the
//! environment is not ready, and `errors[].fix.by` says who acts: some
//! exit-4 errors need the agent or the owner, so "exit 4: run `icm doctor
//! --fix --yes`" sends an agent the wrong way. And the fork's AGENTS.md
//! says who shuts down a throwaway `icm-test-*` device.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_icm");

fn fork() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn icm(args: &[&str]) -> String {
    let cache = tempfile::tempdir().unwrap();
    let output = Command::new(BIN)
        .args(args)
        .current_dir(cache.path())
        .env("ICM_CACHE_DIR", cache.path())
        .env("ICM_HOST_CONFIG", cache.path().join("no-host.toml"))
        .env_remove("ICM_JSON")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "icm {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// What `text` says about exit 4 in its exit-code list: the 200 bytes
/// from each `4 environment` or `| 4 |` on.
fn exit_four(text: &str) -> Vec<&str> {
    ["4 environment", "| 4 |"]
        .iter()
        .flat_map(|needle| text.match_indices(needle))
        .map(|(at, _)| {
            let mut end = (at + 200).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            &text[at..end]
        })
        .collect()
}

#[test]
fn exit_four_says_to_follow_fix_by() {
    // The premise: exit-4 errors are not all doctor's to fix.
    let list: Value = serde_json::from_str(
        icm(&["explain", "--list", "--json", "-q"])
            .lines()
            .last()
            .unwrap(),
    )
    .unwrap();
    let by: Vec<&str> = list["catalogue"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["exit"] == 4)
        .map(|entry| entry["by"].as_str().unwrap())
        .collect();
    assert!(by.contains(&"agent") && by.contains(&"owner"), "{by:?}");

    let readme = std::fs::read_to_string(fork().join("README.md")).unwrap();
    let template = std::fs::read_to_string(fork().join("examples/app/AGENTS.md")).unwrap();
    let explained = icm(&["explain", "exit-codes"]);
    let help = icm(&["--help"]);
    for (name, text) in [
        ("README.md", &readme),
        ("examples/app/AGENTS.md", &template),
        ("icm explain exit-codes", &explained),
        ("icm --help", &help),
    ] {
        let said = exit_four(text);
        assert!(!said.is_empty(), "{name} lists no exit 4");
        for words in said {
            assert!(words.contains("fix.by"), "{name}: {words}");
        }
    }

    // Nor does doctor's exit 4 mean "more to install".
    let flat = readme.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        !flat.contains("Exit 4 from doctor means `--fix --yes` has more to install"),
        "README.md"
    );
    let doctor = &flat[flat.find("- **doctor:**").unwrap()..];
    let item = &doctor[..doctor[1..].find("- **").map_or(doctor.len(), |end| end + 1)];
    assert!(item.contains("fix.by"), "README.md: {item}");
    assert!(
        explained.contains("else the first remaining error's own exit"),
        "{explained}"
    );
}

/// The fork's AGENTS.md says what icm does with a throwaway `icm-test-*`
/// device (DESIGN.md Appendix D item 19, `tests/ios_sim.rs`
/// a_test_simulator_the_run_booted_stays_booted): every setting that names
/// one makes icm boot it, only Android's `stop --shutdown` shuts one down
/// (the emulator its own run booted), and a test simulator is shut down and
/// deleted by hand. It once said `stop --shutdown` shuts down any it
/// booted, so an agent that trusted it left simulators booted.
#[test]
fn agents_md_says_who_shuts_down_a_test_device() {
    let agents = std::fs::read_to_string(fork().join("AGENTS.md")).unwrap();
    let devices = agents
        .split("\n## Devices\n")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .expect("AGENTS.md has a Devices section");
    let devices = devices.split_whitespace().collect::<Vec<_>>().join(" ");
    for said in [
        "`--avd`",
        "`[android] avd`",
        "`--sim`",
        "`[ios] simulator_udid`",
        "Only Android's `stop --shutdown` shuts one down, and only the emulator its own run booted.",
        "`xcrun simctl shutdown <udid>`",
        "`xcrun simctl delete <udid>`",
    ] {
        assert!(
            devices.contains(said),
            "AGENTS.md's Devices section no longer says {said}"
        );
    }
}
