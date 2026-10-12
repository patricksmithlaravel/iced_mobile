//! Checks of the fork's CI in `.github/` (design §17 and Appendix D "CI"):
//! the release tag rule `.github/ci/tag.sh` enforces and the docs that name
//! the release, the workflows running the checks AGENTS.md lists, the
//! scripts they name, and nothing in them that formats path dependencies,
//! reads a secret or uploads, publishes or notarizes. Also the crate
//! versions the fork's `Cargo.lock`, which CI and the checkout's examples
//! build, must not fall below. They read the fork's files around `cli/` and
//! run no workflow.

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

/// The workflows the fork's CI consists of, and the per-platform ones that
/// run `.github/ci` scripts.
const WORKFLOWS: &[&str] = &[
    ".github/workflows/framework.yml",
    ".github/workflows/icm.yml",
    ".github/workflows/icm-android.yml",
    ".github/workflows/icm-ios.yml",
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

/// A release commit (AGENTS.md "Rules") names one release everywhere a
/// new user starts: README.md's install command, framework pins and status
/// line, AGENTS.md and the newest release's section of CHANGES-mobile.md
/// (under its Unreleased section, if any) all name `v` plus icm's version.
/// The template asks for no icm newer than this
/// one, or `icm new` would create apps it refuses, and phase0.sh's tag
/// step reads the version instead of naming a tag.
#[test]
fn the_docs_name_this_release() {
    let tag = format!("v{VERSION}");

    let readme = read("README.md");
    let pins: Vec<&str> = readme
        .lines()
        .filter(|line| line.contains("--tag v") || line.contains("tag = \"v"))
        .collect();
    assert!(
        pins.len() >= 3,
        "README.md has no install command or framework pins: {pins:?}"
    );
    for line in pins {
        assert!(
            line.contains(&format!("--tag {tag} ")) || line.contains(&format!("tag = \"{tag}\"")),
            "README.md pins another release than {tag}: {line}"
        );
    }
    let current = format!("The current release is `{tag}`");
    assert!(readme.contains(&current), "README.md: {current:?}");
    assert!(
        read("AGENTS.md").contains(&current),
        "AGENTS.md: {current:?}"
    );

    // Fixes on `main` since the release wait in an `## Unreleased` section
    // above it, which the next release commit renames.
    let changes = read("CHANGES-mobile.md");
    let mut sections = changes.lines().filter(|line| line.starts_with("## "));
    let mut newest = sections.next();
    if newest == Some("## Unreleased") {
        newest = sections.next();
    }
    let newest = newest.expect("CHANGES-mobile.md has a section per release");
    assert_eq!(newest, format!("## Changes in {tag}"));
    assert!(
        sections.all(|line| line != "## Unreleased"),
        "CHANGES-mobile.md's Unreleased section is above the newest release's"
    );
    assert!(
        changes.contains(&format!("--tag {tag} icm")),
        "CHANGES-mobile.md's install command"
    );

    let template = read("examples/app/icm.toml");
    let min_icm = template
        .lines()
        .find_map(|line| line.strip_prefix("min_icm = \""))
        .and_then(|rest| rest.split('"').next())
        .expect("the template's icm.toml has min_icm");
    let min_icm = semver::Version::parse(min_icm).expect("min_icm is a version");
    let version = semver::Version::parse(VERSION).unwrap();
    assert!(
        min_icm <= version,
        "the template asks for icm {min_icm}, newer than this icm {version}"
    );

    let phase0 = read("cli/tests/accept/phase0.sh");
    let literal = phase0
        .match_indices("-mobile.")
        .find(|(at, _)| phase0[at + "-mobile.".len()..].starts_with(|c: char| c.is_ascii_digit()));
    assert!(
        literal.is_none(),
        "phase0.sh names a release tag; derive it from cli/Cargo.toml"
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

/// The versions of `name` in a `Cargo.lock`.
fn locked_versions(lock: &str, name: &str) -> Vec<(u64, u64, u64)> {
    lock.split("[[package]]")
        .filter(|package| {
            package
                .lines()
                .any(|line| line.trim() == format!("name = \"{name}\""))
        })
        .filter_map(|package| {
            let version = package
                .lines()
                .find_map(|line| line.trim().strip_prefix("version = "))?
                .trim_matches('"');
            let mut numbers = version
                .split(['.', '-', '+'])
                .map(|part| part.parse::<u64>().unwrap_or(0));
            Some((
                numbers.next()?,
                numbers.next().unwrap_or(0),
                numbers.next().unwrap_or(0),
            ))
        })
        .collect()
}

/// android-activity 0.6.0's NativeActivity asks InputMethodManager to show
/// the soft keyboard for its NativeContentView, which is never the served
/// view on Android 16 (API 36, where the served view is the DecorView): the
/// request fails at PHASE_CLIENT_VIEW_SERVED and no keyboard shows, so a
/// text_input cannot be typed into on the screen. 0.6.1 passes the
/// DecorView. iced_winit requires 0.6.1 on Android, and the fork's lock,
/// which builds `examples/app` from the checkout, must hold it too.
#[test]
fn the_lock_has_an_android_activity_that_shows_the_keyboard() {
    let versions = locked_versions(&read("Cargo.lock"), "android-activity");
    assert!(!versions.is_empty(), "Cargo.lock has no android-activity");
    for version in versions {
        assert!(
            version >= (0, 6, 1),
            "Cargo.lock has android-activity {version:?}, whose soft keyboard never shows on API 36; run `cargo update -p android-activity` at the root"
        );
    }
}

/// What only a Mac can check: its `/bin/bash` is the bash 3.2 that GitHub's
/// macOS runners run `run:` steps and `#!/usr/bin/env bash` scripts with,
/// and `.github/ci/xcode.sh` reads Xcode's plists with `plutil`.
#[cfg(target_os = "macos")]
mod macos {
    use super::{Output, Path, PermissionsExt, ci_files, fork, read};
    use std::process::Command;

    /// `/bin/bash -n FILE`: bash's complaint when it cannot parse FILE.
    fn bash32_parse_error(file: &Path) -> Option<String> {
        let out = Command::new("/bin/bash")
            .arg("-n")
            .arg(file)
            .output()
            .expect("run /bin/bash");
        (!out.status.success()).then(|| String::from_utf8_lossy(&out.stderr).trim().to_string())
    }

    /// The script with GitHub's `${{ … }}` expressions, which the runner
    /// replaces before bash reads it, as a plain word.
    fn without_expressions(script: &str) -> String {
        let mut out = String::new();
        let mut rest = script;
        while let Some(start) = rest.find("${{") {
            let Some(end) = rest[start..].find("}}") else {
                break;
            };
            out.push_str(&rest[..start]);
            out.push_str("EXPR");
            rest = &rest[start + end + 2..];
        }
        out.push_str(rest);
        out
    }

    /// The bash `run:` steps of a workflow's jobs that run on macOS, as
    /// (the line of `run:`, the script). A job runs on macOS when its text
    /// names macos (its `runs-on` or its matrix). A step whose `shell:` is
    /// not bash (pwsh) is left out.
    fn macos_bash_steps(text: &str) -> Vec<(usize, String)> {
        let lines: Vec<&str> = text.lines().collect();
        let indent = |line: &str| line.len() - line.trim_start().len();
        let filled = |line: &str| !line.trim().is_empty();
        let Some(jobs) = lines.iter().position(|line| *line == "jobs:") else {
            return Vec::new();
        };
        // A job is a key two spaces in under `jobs:`, up to the next one.
        let mut starts: Vec<usize> = (jobs + 1..lines.len())
            .filter(|&i| {
                indent(lines[i]) == 2
                    && !lines[i].trim_start().starts_with('#')
                    && lines[i].trim_end().ends_with(':')
            })
            .collect();
        starts.push(lines.len());

        let mut steps = Vec::new();
        for job in starts.windows(2) {
            let (start, end) = (job[0], job[1]);
            if !lines[start..end]
                .join("\n")
                .to_ascii_lowercase()
                .contains("macos")
            {
                continue;
            }
            for i in start..end {
                let trimmed = lines[i].trim_start();
                let key = trimmed.strip_prefix("- ").unwrap_or(trimmed);
                let Some(value) = key.strip_prefix("run:") else {
                    continue;
                };
                // The step's keys sit in this column, after its `- `.
                let column = lines[i].len() - key.len();
                let first = (start..=i)
                    .rev()
                    .find(|&j| {
                        indent(lines[j]) + 2 == column && lines[j].trim_start().starts_with("- ")
                    })
                    .unwrap_or(i);
                let last = (i + 1..end)
                    .find(|&j| filled(lines[j]) && indent(lines[j]) < column)
                    .unwrap_or(end);
                let shell = (first..last).find_map(|j| {
                    let trimmed = lines[j].trim_start();
                    let key = trimmed.strip_prefix("- ").unwrap_or(trimmed);
                    (lines[j].len() - key.len() == column)
                        .then(|| key.strip_prefix("shell:"))
                        .flatten()
                });
                if shell.is_some_and(|shell| !shell.trim().starts_with("bash")) {
                    continue;
                }
                let value = value.trim();
                let script = if value.starts_with('|') || value.starts_with('>') {
                    let block_end = (i + 1..end)
                        .find(|&j| filled(lines[j]) && indent(lines[j]) <= column)
                        .unwrap_or(end);
                    let block = &lines[i + 1..block_end];
                    let margin = block
                        .iter()
                        .filter(|line| filled(line))
                        .map(|line| indent(line))
                        .min()
                        .unwrap_or(0);
                    block
                        .iter()
                        .map(|line| line.get(margin..).unwrap_or(""))
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    value.to_string()
                };
                steps.push((i + 1, without_expressions(&script)));
            }
        }
        steps
    }

    /// GitHub's macOS runners have no bash but /bin/bash, 3.2. It cannot
    /// parse a `case` inside `$(...)` (the pattern's `)` ends the command
    /// substitution), which failed icm-ios.yml's Xcode step on its first
    /// run, and other syntax newer bashes take. Every bash step of a job
    /// that runs on macOS, every script in .github/ci and every acceptance
    /// script (phase2.sh and phase5.sh run on macOS runners) must parse.
    #[test]
    fn macos_bash_parses_every_step_and_script() {
        let scratch = tempfile::tempdir().unwrap();
        let mut checked = Vec::new();
        let mut failures = Vec::new();
        let mut check = |label: String, file: &Path| {
            if let Some(error) = bash32_parse_error(file) {
                failures.push(format!("{label}: {error}"));
            }
            checked.push(label);
        };

        for (path, text) in ci_files() {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name.ends_with(".yml") {
                for (line, script) in macos_bash_steps(&text) {
                    let file = scratch.path().join(format!("{name}-{line}.sh"));
                    std::fs::write(&file, script).unwrap();
                    check(format!(".github/workflows/{name}:{line}"), &file);
                }
            } else if name.ends_with(".sh") {
                check(format!(".github/ci/{name}"), &path);
            }
        }
        let accept = fork().join("cli/tests/accept");
        let mut scripts: Vec<_> = std::fs::read_dir(&accept)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "sh"))
            .collect();
        scripts.sort();
        for path in scripts {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            check(format!("cli/tests/accept/{name}"), &path);
        }

        // The steps were found: icm-ios.yml's Xcode step, icm.yml's macOS
        // jobs, framework.yml's.
        for wanted in [
            ".github/workflows/icm-ios.yml:",
            ".github/workflows/icm.yml:",
            ".github/workflows/framework.yml:",
            ".github/ci/xcode.sh",
            "cli/tests/accept/phase2.sh",
        ] {
            assert!(
                checked.iter().any(|label| label.starts_with(wanted)),
                "nothing of {wanted} was checked: {checked:#?}"
            );
        }
        assert!(
            failures.is_empty(),
            "macOS's /bin/bash (3.2) cannot parse these:\n{}",
            failures.join("\n")
        );
    }

    /// A plist of string values, as an Xcode bundle's are.
    fn plist(entries: &[(&str, &str)]) -> String {
        let mut text = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n",
        );
        for (key, value) in entries {
            text.push_str(&format!("\t<key>{key}</key>\n\t<string>{value}</string>\n"));
        }
        text.push_str("</dict>\n</plist>\n");
        text
    }

    /// An Xcode bundle as the script reads it: its licence type (none: no
    /// LicenseInfo.plist) and its version.
    fn xcode_bundle(apps: &Path, name: &str, license: Option<&str>, version: &str) {
        let contents = apps.join(name).join("Contents");
        std::fs::create_dir_all(contents.join("Resources")).unwrap();
        std::fs::create_dir_all(contents.join("Developer")).unwrap();
        if let Some(license) = license {
            std::fs::write(
                contents.join("Resources/LicenseInfo.plist"),
                plist(&[("licenseID", "EA0000"), ("licenseType", license)]),
            )
            .unwrap();
        }
        std::fs::write(
            contents.join("version.plist"),
            plist(&[
                ("CFBundleShortVersionString", version),
                ("ProductBuildVersion", "17A000"),
            ]),
        )
        .unwrap();
    }

    fn xcode_sh(apps: &Path) -> Output {
        Command::new(fork().join(".github/ci/xcode.sh"))
            .arg(apps)
            .output()
            .expect("run .github/ci/xcode.sh")
    }

    /// The developer directory xcode.sh picks in `apps`.
    fn picked(apps: &Path) -> String {
        let out = xcode_sh(apps);
        assert!(
            out.status.success(),
            "xcode.sh failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// icm-ios.yml selects the Xcode `.github/ci/xcode.sh` picks: the
    /// newest release, judged by the bundle's licence type and version.
    /// GitHub's images keep each Xcode as Xcode_<version>.app with links
    /// under other names, and link a beta (Xcode_27.2_beta_2.app) under its
    /// plain version too (Xcode_27.2.app), so a name says nothing.
    #[test]
    fn xcode_sh_picks_the_newest_release_xcode() {
        let script = fork().join(".github/ci/xcode.sh");
        let mode = script.metadata().unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, ".github/ci/xcode.sh is not executable");
        assert!(
            read(".github/workflows/icm-ios.yml").contains("developer_dir=$(.github/ci/xcode.sh)"),
            "icm-ios.yml no longer selects the Xcode .github/ci/xcode.sh picks"
        );

        let root = tempfile::tempdir().unwrap();
        // The temporary directory is under a link (/var); the script prints
        // real paths.
        let apps = std::fs::canonicalize(root.path()).unwrap();
        let link = |target: &str, name: &str| {
            std::os::unix::fs::symlink(apps.join(target), apps.join(name)).unwrap();
        };
        let developer = |name: &str| format!("{}/{name}/Contents/Developer", apps.display());

        let out = xcode_sh(&apps);
        assert_eq!(out.status.code(), Some(1), "no Xcode at all");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("::error::no release Xcode"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        // The macOS 26 image's releases and their links.
        xcode_bundle(&apps, "Xcode_26.0.1.app", Some("GM"), "26.0.1");
        link("Xcode_26.0.1.app", "Xcode_26.0.app");
        xcode_bundle(&apps, "Xcode_26.4.1.app", Some("GM"), "26.4.1");
        link("Xcode_26.4.1.app", "Xcode_26.4.app");
        xcode_bundle(&apps, "Xcode_26.6.app", Some("GM"), "26.6");
        link("Xcode_26.6.app", "Xcode_26.6.0.app");
        // Newer, but none a release: a beta also linked under its plain
        // version, a beta under a release's name, a bundle without a
        // licence, a link to nothing.
        xcode_bundle(&apps, "Xcode_27.2_beta_2.app", Some("Beta"), "27.2");
        link("Xcode_27.2_beta_2.app", "Xcode_27.2.app");
        xcode_bundle(&apps, "Xcode_27.3.app", Some("Beta"), "27.3");
        xcode_bundle(&apps, "Xcode_28.0.app", None, "28.0");
        link("Xcode_gone.app", "Xcode_29.0.app");
        assert_eq!(picked(&apps), developer("Xcode_26.6.app"));

        // Versions compare by number.
        xcode_bundle(&apps, "Xcode_26.10.app", Some("GM"), "26.10");
        assert_eq!(picked(&apps), developer("Xcode_26.10.app"));

        // A release candidate is a GM build, which the App Store takes.
        xcode_bundle(
            &apps,
            "Xcode_27.1_Release_Candidate.app",
            Some("GM"),
            "27.1",
        );
        link("Xcode_27.1_Release_Candidate.app", "Xcode_27.1.app");
        assert_eq!(picked(&apps), developer("Xcode_27.1_Release_Candidate.app"));
    }
}
