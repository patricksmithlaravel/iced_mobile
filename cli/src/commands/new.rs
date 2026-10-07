//! `icm new <dir>`: creates an app from the template (design §6 `new`,
//! §8, Appendix C items 3 and 12).
//!
//! It copies the embedded `examples/app`, fills in the names and id,
//! rewrites the iced lines to the pinned framework source, writes
//! AGENTS.md, and runs `git init` unless `--no-git`. It does no network
//! work: the first cargo call resolves `Cargo.lock`.
//!
//! The framework source is `--framework`, else the pin this icm was built
//! with ([`crate::buildinfo::FRAMEWORK`]): its tag, its full rev when a
//! remote has that commit, else `path:<checkout>`. A build without git
//! metadata exits 2 (`new.framework_unknown`).

use crate::catalogue::CheckId;
use crate::cli::NewArgs;
use crate::context::Ctx;
use crate::error::{Check, Evidence, IcmError, Result, Status};
use crate::plan::{Plan, Step};
use crate::process::Cmd;
use crate::template::{self, Framework, Names};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Runs `icm new`.
pub fn run(ctx: &mut Ctx, args: &NewArgs) -> Result<()> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let dir = crate::paths::normalize(&if args.dir.is_absolute() {
        args.dir.clone()
    } else {
        cwd.join(&args.dir)
    });
    let dir_name = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| {
            IcmError::new(
                CheckId::UsageBadArgs,
                format!("`{}` names no directory", args.dir.display()),
            )
        })?;

    let names =
        Names::new(&dir_name, args.name.as_deref(), args.id.as_deref()).map_err(|message| {
            let id = if message.starts_with("--id") {
                CheckId::ConfigIdInvalid
            } else {
                CheckId::UsageBadArgs
            };
            IcmError::new(id, message).fix(
                "Choose another directory name, or pass a valid --name/--id.",
                &["icm new <dir> --id com.yourcompany.yourapp"],
            )
        })?;

    let framework = framework(args.framework.as_deref(), &cwd)?;

    // The directory: new, or empty, or --force.
    if dir.exists() && !dir.is_dir() {
        return Err(IcmError::new(
            CheckId::UsageBadArgs,
            format!(
                "{} exists and is not a directory",
                crate::paths::display(&dir)
            ),
        ));
    }
    let existing = existing_entries(&dir);
    if !existing.is_empty() && !args.force {
        return Err(IcmError::new(
            CheckId::NewDirNotEmpty,
            format!(
                "{} already holds {} ({}{})",
                crate::paths::display(&dir),
                plural(existing.len(), "entry", "entries"),
                existing
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                if existing.len() > 5 { ", …" } else { "" }
            ),
        )
        .evidence(Evidence::file(&dir)));
    }

    let mut files = template::render(&names, &framework)
        .map_err(|message| IcmError::new(CheckId::InternalBug, message))?;

    // Inside another cargo workspace the app would be claimed by it; make
    // the app its own workspace root instead.
    let enclosing = enclosing_workspace(&dir);
    if let Some(root) = &enclosing
        && let Some((_, cargo)) = files.iter_mut().find(|(path, _)| path == "Cargo.toml")
    {
        cargo.extend_from_slice(
            format!(
                "\n# Its own workspace: {} has a [workspace] that would otherwise claim this app.\n[workspace]\n",
                root.display()
            )
            .as_bytes(),
        );
    }

    // The rendered icm.toml must load like any other.
    if let Some((_, bytes)) = files.iter().find(|(path, _)| path == "icm.toml") {
        let text = String::from_utf8_lossy(bytes);
        if let Err(problems) = crate::config::parse(&dir.join("icm.toml"), &text) {
            let first = problems.into_iter().next().map(|p| p.to_string());
            return Err(IcmError::new(
                CheckId::InternalBug,
                format!(
                    "the rendered icm.toml does not validate: {}",
                    first.unwrap_or_default()
                ),
            ));
        }
    }

    let git = !args.no_git && !dir.join(".git").exists();
    if ctx.dry_run() {
        let mut plan = Plan::new();
        plan.push(Step::internal(
            "new.write",
            &format!(
                "write {} files into {}",
                files.len(),
                crate::paths::display(&dir)
            ),
        ));
        if git {
            plan.push(Step::exec("git.init", git_init(&dir)));
        }
        plan.report(ctx);
        ctx.rep
            .set("project", project_json(&dir, &names, &framework, &files));
        return Ok(());
    }

    let started = std::time::Instant::now();
    let _ = ctx.rep.step_log("new.write");
    let written = write_files(&dir, &files);
    ctx.rep.step_end_internal(
        "new.write",
        written.is_ok(),
        started.elapsed().as_millis() as u64,
    );
    written?;

    if git {
        match ctx.step("git.init", &git_init(&dir)) {
            Ok(outcome) if outcome.success() => {}
            Ok(outcome) => ctx.rep.check(Check::from_error(
                ctx.step_failure("git.init", CheckId::ToolFailed, &outcome),
                Status::Warn,
            )),
            Err(error) => ctx.rep.check(Check::from_error(
                IcmError::new(
                    CheckId::ToolFailed,
                    format!("git init did not run: {}", error.detail),
                ),
                Status::Warn,
            )),
        }
    }

    if names.id.starts_with("com.example.") {
        ctx.rep.check(
            Check::warn(
                CheckId::AppIdPlaceholder,
                format!(
                    "the app id {} is a placeholder; the owner chooses the permanent one",
                    names.id
                ),
            )
            .evidence(Evidence::file(dir.join("icm.toml"))),
        );
    }

    ctx.rep.set(
        "app",
        json!({"id": names.id, "name": names.display, "version": "0.1.0", "build": 1}),
    );
    ctx.rep
        .set("project", project_json(&dir, &names, &framework, &files));
    ctx.rep.artifact("project", &dir);
    ctx.rep.artifact("agents_md", &dir.join("AGENTS.md"));

    let shown = crate::paths::display(&dir);
    let cd = format!("cd {}", crate::process::shell_quote(&shown));
    ctx.rep.next(
        format!("{cd} && icm doctor --fix --yes --json -q"),
        "check this machine for every platform and install what is missing",
    );
    ctx.rep.next(
        format!("{cd} && icm check --all --json -q"),
        "compile every platform",
    );
    ctx.rep.next(
        format!("{cd} && icm run desktop --json -q"),
        "build, launch and screenshot the app",
    );
    ctx.rep.summary(format!(
        "created {} ({}) in {shown}, iced from {}",
        names.display,
        names.id,
        framework.spec()
    ));
    Ok(())
}

/// The framework source: `--framework`, else the build's default pin.
fn framework(explicit: Option<&str>, cwd: &Path) -> Result<Framework> {
    let url = if crate::buildinfo::GIT_URL.is_empty() {
        crate::version::DEFAULT_GIT_URL
    } else {
        crate::buildinfo::GIT_URL
    };

    if let Some(spec) = explicit {
        return Framework::parse(spec, url, cwd).map_err(|message| {
            IcmError::new(CheckId::UsageBadArgs, format!("--framework: {message}")).fix(
                "Pass --framework tag:<tag>, rev:<full sha> or path:<checkout of iced_mobile>.",
                &[],
            )
        });
    }

    let default = crate::buildinfo::FRAMEWORK;
    if default.is_empty() {
        return Err(IcmError::new(
            CheckId::NewFrameworkUnknown,
            format!(
                "this icm ({}) was built without git metadata, so it cannot pin a framework revision",
                crate::buildinfo::VERSION_LINE
            ),
        )
        .fix_commands(["icm new <dir> --framework path:<checkout of iced_mobile>"]));
    }
    Framework::parse(default, url, cwd).map_err(|message| {
        IcmError::new(
            CheckId::NewFrameworkUnknown,
            format!("this icm's default framework source {default} is unusable: {message}"),
        )
        .fix_commands([
            "icm new <dir> --framework tag:<tag>",
            "icm new <dir> --framework path:<checkout of iced_mobile>",
        ])
    })
}

/// The names in a directory, sorted (none when it does not exist).
fn existing_entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|read| {
            read.flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name != ".DS_Store")
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The nearest ancestor of `dir` whose Cargo.toml has a `[workspace]`.
fn enclosing_workspace(dir: &Path) -> Option<PathBuf> {
    let mut current = dir.parent();
    while let Some(ancestor) = current {
        let manifest = ancestor.join("Cargo.toml");
        if let Ok(text) = std::fs::read_to_string(&manifest)
            && let Ok(table) = toml::from_str::<toml::Table>(&text)
            && table.contains_key("workspace")
        {
            return Some(ancestor.to_path_buf());
        }
        current = ancestor.parent();
    }
    None
}

fn write_files(dir: &Path, files: &[(String, Vec<u8>)]) -> Result<()> {
    for (relative, bytes) in files {
        let path = dir.join(relative);
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, bytes));
        if let Err(error) = written {
            return Err(IcmError::new(
                CheckId::UsageBadArgs,
                format!("cannot write {}: {error}", crate::paths::display(&path)),
            )
            .evidence(Evidence::file(&path)));
        }
    }
    Ok(())
}

fn git_init(dir: &Path) -> Cmd {
    Cmd::tool("git")
        .args(["init", "-q"])
        .arg(dir)
        .cwd(dir)
        .timeout(Duration::from_secs(60))
}

fn project_json(
    dir: &Path,
    names: &Names,
    framework: &Framework,
    files: &[(String, Vec<u8>)],
) -> serde_json::Value {
    json!({
        "dir": crate::paths::display(dir),
        "package": names.package,
        "lib": names.lib,
        "bin": names.package,
        "name": names.display,
        "id": names.id,
        "framework": framework.spec(),
        "files": files.iter().map(|(path, _)| path.as_str()).collect::<Vec<_>>(),
    })
}

fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}
