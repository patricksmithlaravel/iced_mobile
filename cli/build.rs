//! Embeds what icm needs to know about its own source:
//!
//! - `ICM_GIT_REV`, `ICM_GIT_DIRTY`: the commit icm was built from;
//! - `ICM_FRAMEWORK`, `ICM_GIT_URL`: the framework pin `icm new` defaults to
//!   (`tag:`, `rev:` or `path:`; see `src/gitinfo.rs`);
//! - `ICM_VERSION_LINE`: what `icm --version` prints after `icm `;
//! - `$OUT_DIR/explain_docs.rs`: the hand-written `docs/explain/*.md`;
//! - `$OUT_DIR/template.rs`: the template `icm new` copies (`examples/app`
//!   of the fork) and `docs/agents/limitations.md`, which AGENTS.md embeds;
//! - `$OUT_DIR/ofl.txt`: Fira Sans's licence (`graphics/fonts/OFL.txt`),
//!   for releases' THIRD_PARTY_NOTICES.
//!
//! `policy/stores.toml` and `tools.toml` are embedded with `include_str!`.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "src/gitinfo.rs"]
mod gitinfo;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/gitinfo.rs");
    println!("cargo:rerun-if-env-changed=ICM_BUILD_FRAMEWORK");

    let facts = git_facts(&manifest_dir, &version);
    let mut pin = gitinfo::decide(&facts, &version);

    // Escape hatch for packagers: force the default framework pin.
    if let Ok(forced) = env::var("ICM_BUILD_FRAMEWORK")
        && !forced.is_empty()
    {
        pin.framework = forced;
    }

    let rev = facts.rev.clone().unwrap_or_default();
    let short = rev.get(..12).unwrap_or(&rev);
    let mut version_line = version.clone();
    if rev.is_empty() {
        version_line.push_str(" (no git metadata)");
    } else {
        version_line.push_str(&format!(
            " (rev {short}{})",
            if facts.dirty { ", dirty" } else { "" }
        ));
    }

    println!("cargo:rustc-env=ICM_GIT_REV={rev}");
    println!(
        "cargo:rustc-env=ICM_GIT_DIRTY={}",
        if facts.dirty { "1" } else { "0" }
    );
    println!("cargo:rustc-env=ICM_FRAMEWORK={}", pin.framework);
    println!("cargo:rustc-env=ICM_GIT_URL={}", pin.url);
    println!("cargo:rustc-env=ICM_VERSION_LINE={version_line}");

    write_explain_docs(&manifest_dir, &out_dir);
    write_template(&manifest_dir, &out_dir);
    write_ofl(&manifest_dir, &out_dir);
}

/// Copies the fork's `graphics/fonts/OFL.txt` (Fira Sans's licence) to
/// `$OUT_DIR/ofl.txt`, the fallback THIRD_PARTY_NOTICES uses when a
/// release cannot find the framework's own copy. Empty outside the fork.
fn write_ofl(manifest_dir: &Path, out_dir: &Path) {
    let fork = manifest_dir.parent().unwrap_or(manifest_dir);
    let ofl = fork.join("graphics").join("fonts").join("OFL.txt");
    println!("cargo:rerun-if-changed={}", ofl.display());
    let text = fs::read_to_string(&ofl).unwrap_or_default();
    fs::write(out_dir.join("ofl.txt"), text).expect("write ofl.txt");
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(String::from_utf8(output.stdout).ok()?.trim().to_string())
}

fn rerun_if_exists(path: &Path) {
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn git_facts(manifest_dir: &Path, version: &str) -> gitinfo::GitFacts {
    let mut facts = gitinfo::GitFacts::default();

    let Some(rev) = git(manifest_dir, &["rev-parse", "--verify", "HEAD"]) else {
        return facts;
    };
    facts.rev = Some(rev.clone());

    // Rebuild when HEAD moves, refs change or the index changes.
    if let Some(git_dir) = git(manifest_dir, &["rev-parse", "--absolute-git-dir"]) {
        let git_dir = PathBuf::from(git_dir);
        rerun_if_exists(&git_dir.join("HEAD"));
        rerun_if_exists(&git_dir.join("index"));
    }
    if let Some(common) = git(manifest_dir, &["rev-parse", "--git-common-dir"]) {
        let common = if Path::new(&common).is_absolute() {
            PathBuf::from(common)
        } else {
            manifest_dir.join(common)
        };
        rerun_if_exists(&common.join("refs").join("heads"));
        rerun_if_exists(&common.join("refs").join("remotes"));
        rerun_if_exists(&common.join("refs").join("tags"));
        rerun_if_exists(&common.join("packed-refs"));
    }

    facts.dirty = git(
        manifest_dir,
        &["status", "--porcelain", "--untracked-files=no"],
    )
    .is_some_and(|status| !status.is_empty());

    let Some(toplevel) = git(manifest_dir, &["rev-parse", "--show-toplevel"]) else {
        return facts;
    };
    let toplevel = PathBuf::from(toplevel);
    facts.toplevel = Some(toplevel.display().to_string());

    // `cargo install --git` builds in a checkout it marks with `.cargo-ok`,
    // cloned from its git database (the checkout's `origin`).
    if toplevel.join(".cargo-ok").exists() {
        facts.cargo_checkout = true;

        if let Some(origin) = git(&toplevel, &["config", "--get", "remote.origin.url"]) {
            let db = PathBuf::from(origin.trim_start_matches("file://"));

            facts.checkout_url = fs::read_to_string(db.join("FETCH_HEAD"))
                .ok()
                .and_then(|contents| gitinfo::fetch_head_url(&contents));

            let tag = format!("v{version}");
            for reference in [
                format!("refs/remotes/origin/tags/{tag}"),
                format!("refs/tags/{tag}"),
            ] {
                let spec = format!("{reference}^{{commit}}");
                let resolved = Command::new("git")
                    .arg("--git-dir")
                    .arg(&db)
                    .args(["rev-parse", "--verify", "-q", &spec])
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .output()
                    .ok()
                    .filter(|output| output.status.success())
                    .and_then(|output| String::from_utf8(output.stdout).ok())
                    .map(|s| s.trim().to_string());

                if let Some(commit) = resolved {
                    facts.tag_commit = Some(commit);
                    break;
                }
            }
        }

        return facts;
    }

    // A local checkout: is HEAD on a configured, non-local remote?
    let remotes = git(&toplevel, &["remote"]).unwrap_or_default();
    for remote in remotes.lines().map(str::trim).filter(|r| !r.is_empty()) {
        let Some(url) = git(
            &toplevel,
            &["config", "--get", &format!("remote.{remote}.url")],
        ) else {
            continue;
        };
        if gitinfo::is_local_url(&url) {
            continue;
        }

        let pattern = format!("refs/remotes/{remote}/");
        let containing = git(
            &toplevel,
            &[
                "for-each-ref",
                "--contains",
                "HEAD",
                "--format=%(refname)",
                &pattern,
            ],
        )
        .unwrap_or_default();

        if !containing.trim().is_empty() {
            facts.pushed_remote_url = Some(url);
            break;
        }
    }

    facts
}

/// Generates `explain_docs.rs`: `(id, markdown)` for each `docs/explain/<id>.md`.
fn write_explain_docs(manifest_dir: &Path, out_dir: &Path) {
    let docs_dir = manifest_dir.join("docs").join("explain");
    println!("cargo:rerun-if-changed={}", docs_dir.display());

    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(read_dir) = fs::read_dir(&docs_dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "md") {
                let id = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .expect("utf-8 doc name")
                    .to_string();
                println!("cargo:rerun-if-changed={}", path.display());
                entries.push((id, path));
            }
        }
    }
    entries.sort();

    let mut source = String::from("/// Hand-written explain docs, generated by build.rs.\n");
    source.push_str("pub static EXPLAIN_DOCS: &[(&str, &str)] = &[\n");
    for (id, path) in &entries {
        source.push_str(&format!(
            "    ({id:?}, include_str!({:?})),\n",
            path.display().to_string()
        ));
    }
    source.push_str("];\n");

    fs::write(out_dir.join("explain_docs.rs"), source).expect("write explain_docs.rs");
}

/// Generates `template.rs`: every file of `../examples/app` (the template
/// `icm new` copies; `target/` and editor droppings left out) as
/// `(relative path, bytes)`, and `../docs/agents/limitations.md`. Both live
/// in the fork next to `cli/`, which `cargo install --git` and
/// `cargo install --path cli` both build from. Without them (a source tree
/// that is not the fork) the lists are empty and `icm new` says so.
fn write_template(manifest_dir: &Path, out_dir: &Path) {
    let fork = manifest_dir.parent().unwrap_or(manifest_dir);
    let app = fork.join("examples").join("app");
    let limitations = fork.join("docs").join("agents").join("limitations.md");
    println!("cargo:rerun-if-changed={}", app.display());
    println!("cargo:rerun-if-changed={}", limitations.display());

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_template(&app, &app, &mut files);
    files.sort();

    let mut source = String::from("/// The template's files, generated by build.rs.\n");
    source.push_str("pub static TEMPLATE_FILES: &[(&str, &[u8])] = &[\n");
    for (relative, path) in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        source.push_str(&format!(
            "    ({relative:?}, include_bytes!({:?})),\n",
            path.display().to_string()
        ));
    }
    source.push_str("];\n\n");

    source.push_str("/// docs/agents/limitations.md, generated by build.rs.\n");
    if limitations.is_file() {
        source.push_str(&format!(
            "pub static LIMITATIONS: &str = include_str!({:?});\n",
            limitations.display().to_string()
        ));
    } else {
        source.push_str("pub static LIMITATIONS: &str = \"\";\n");
    }

    fs::write(out_dir.join("template.rs"), source).expect("write template.rs");
}

fn collect_template(root: &Path, dir: &Path, files: &mut Vec<(String, PathBuf)>) {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "target" || name == ".DS_Store" || name.ends_with('~') || name.ends_with(".swp")
        {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect_template(root, &path, files);
        } else if kind.is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("inside the template")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            files.push((relative, path));
        }
    }
}
