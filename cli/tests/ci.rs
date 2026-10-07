//! Checks of the fork's CI in `.github/` (design §17 and Appendix D "CI"):
//! the release tag rule `.github/ci/tag.sh` enforces, the workflows running
//! the checks AGENTS.md lists, the scripts they name, and nothing in them
//! that formats path dependencies, reads a secret or uploads, publishes or
//! notarizes. They read the fork's files around `cli/` and run no workflow.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The fork's checkout (`cli/` is one of its directories).
fn fork() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli/ sits in the fork")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    std::fs::read_to_string(fork().join(relative))
        .unwrap_or_else(|error| panic!("cannot read {relative}: {error}"))
}

/// The workflows the fork's CI consists of.
const WORKFLOWS: &[&str] = &[
    ".github/workflows/framework.yml",
    ".github/workflows/icm.yml",
];

/// Every file under `.github/workflows` and `.github/ci`, with its text.
fn ci_files() -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    for dir in [".github/workflows", ".github/ci"] {
        for entry in std::fs::read_dir(fork().join(dir)).unwrap().flatten() {
            let path = entry.path();
            if path.is_file() {
                let text = std::fs::read_to_string(&path).unwrap();
                files.push((path, text));
            }
        }
    }
    files.sort();
    files
}

fn tag_sh(args: &[&str]) -> Output {
    Command::new(fork().join(".github/ci/tag.sh"))
        .args(args)
        .output()
        .expect("run .github/ci/tag.sh")
}

#[test]
fn a_release_tag_is_v_and_the_cli_version() {
    let tag = format!("v{VERSION}");
    let out = tag_sh(&[&tag]);
    assert!(
        out.status.success(),
        "{tag}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let (base, n) = VERSION.split_once("-mobile.").expect("a -mobile.N version");
    let next = format!("v{base}-mobile.{}", n.parse::<u32>().unwrap() + 1);
    let not_a_release = format!("v{base}");
    for bad in [
        VERSION,
        next.as_str(),
        not_a_release.as_str(),
        "v0.0.0-mobile.0",
    ] {
        let out = tag_sh(&[bad]);
        assert_eq!(out.status.code(), Some(1), "{bad} must be refused");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.starts_with("::error::"), "{bad}: {stdout}");
    }

    assert_eq!(
        tag_sh(&[]).status.code(),
        Some(2),
        "no tag is a usage error"
    );
}

/// The command lines of the first ```sh block after `marker` in `text`.
fn sh_block_after(text: &str, marker: &str) -> Vec<String> {
    let start = text
        .find(marker)
        .unwrap_or_else(|| panic!("AGENTS.md no longer says {marker:?}"));
    text[start..]
        .lines()
        .map(str::trim)
        .skip_while(|line| *line != "```sh")
        .skip(1)
        .take_while(|line| *line != "```")
        .filter(|line| !line.is_empty() && !line.starts_with("cd "))
        .map(String::from)
        .collect()
}

/// The commands a workflow runs: `run: <command>` lines and the lines of
/// `run: |` blocks, trimmed.
fn workflow_commands(workflow: &str) -> Vec<String> {
    read(workflow)
        .lines()
        .map(str::trim)
        .map(|line| {
            line.strip_prefix("- run: ")
                .or_else(|| line.strip_prefix("run: "))
                .unwrap_or(line)
                .to_string()
        })
        .collect()
}

#[test]
fn the_workflows_run_the_checks_agents_md_lists() {
    let agents = read("AGENTS.md");
    let framework = workflow_commands(".github/workflows/framework.yml");
    let icm = workflow_commands(".github/workflows/icm.yml");

    let groups = [
        ("At the root (the framework):", &framework, 6),
        ("enables one by default:", &framework, 2),
        ("In `cli/`, for any change to icm:", &icm, 3),
    ];
    for (marker, commands, at_least) in groups {
        let listed = sh_block_after(&agents, marker);
        assert!(
            listed.len() >= at_least,
            "AGENTS.md's block after {marker:?} has {} commands: {listed:?}",
            listed.len()
        );
        for command in listed {
            assert!(
                commands.contains(&command),
                "AGENTS.md lists `{command}` (after {marker:?}), but CI does not run it"
            );
        }
    }
}

#[test]
fn the_workflows_run_scripts_that_exist() {
    for workflow in WORKFLOWS {
        let text = read(workflow);
        let mut named = 0;
        for (index, _) in text.match_indices(".github/ci/") {
            let rest = &text[index..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || "./-_".contains(c)))
                .unwrap_or(rest.len());
            let relative = rest[..end].trim_end_matches('.');
            if !(relative.ends_with(".sh") || relative.ends_with(".ps1")) {
                continue; // the directory itself, in a comment
            }
            let path = fork().join(relative);
            assert!(
                path.is_file(),
                "{workflow} runs {relative}, which does not exist"
            );
            if relative.ends_with(".sh") {
                let mode = path.metadata().unwrap().permissions().mode();
                assert!(mode & 0o111 != 0, "{relative} is not executable");
            }
            named += 1;
        }
        assert!(named > 0, "{workflow} runs no .github/ci script");
    }
}

#[test]
fn ci_never_formats_path_dependencies_reads_secrets_or_uploads() {
    // `--all` would format vendor/winit with this repository's settings
    // (AGENTS.md); CI holds no credential; and only the owner uploads,
    // publishes or notarizes (design §11), so no upload argv either.
    let forbidden = [
        "cargo fmt --all",
        "secrets.",
        "--upload-package",
        "--upload-app",
        "notarytool submit",
        "stapler staple",
        "fastlane supply",
        "wrangler",
        "netlify deploy",
        "s3 sync",
        "release upload",
        "release create",
        "cargo publish",
        "gh-pages",
        "git push",
        "git tag",
    ];
    let mut offenders = Vec::new();
    for (path, text) in ci_files() {
        for pattern in forbidden {
            if text.contains(pattern) {
                offenders.push(format!("{}: {pattern}", path.display()));
            }
        }
    }
    assert!(offenders.is_empty(), "forbidden in .github: {offenders:#?}");
}
