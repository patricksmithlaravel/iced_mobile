//! `icm test [--host] [--filter <pattern>]`: the app's unit tests and its
//! `.ice` flows, headless (design §13.2).
//!
//! 1. `cargo test -p <pkg> --no-run` builds every test target; compiler
//!    errors are diagnostics and exit 5.
//! 2. `cargo test -p <pkg> --no-fail-fast [-- <pattern>]` runs them with
//!    `ICED_TEST_BACKEND=tiny-skia`: the unit tests, the doc tests and the
//!    harness, which runs `tests/flows/*.ice`.
//! 3. Each test binary becomes a `test.passed` check, each failing test or
//!    flow a `test.failed` check with its evidence (the panic location, the
//!    flow's file and line) and likely causes. Flows in a `[test] flows`
//!    directory other than `tests/flows` run through `icm-ice` one by one.
//!
//! `--on <platform>` and `--lifecycle` (tests on a device) come in a later
//! phase.

use crate::cargo::Select;
use crate::catalogue::CheckId;
use crate::cli::TestArgs;
use crate::context::Ctx;
use crate::error::{Check, Evidence, IcmError, Result};
use crate::harness::{self, Harness, libtest};
use crate::signatures::{self, Facts};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long the test binaries may run together.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Runs `icm test`.
pub fn run(ctx: &mut Ctx, args: &TestArgs) -> Result<()> {
    if args.on.is_some() || args.lifecycle {
        return Err(IcmError::new(
            CheckId::UsageNotImplemented,
            "`icm test --on <platform>` and `--lifecycle` (tests on a device) come in a later phase; this icm runs host tests",
        )
        .fix(
            "Run the host tests, and check a device with `icm run <platform>`.",
            &["icm test --json -q"],
        ));
    }

    let project = ctx.project()?.clone();
    let package = project.package.clone();
    let backend = harness::backend(&ctx.env);
    let has_harness = harness::has_target(&package);
    if !has_harness {
        let error = harness::missing(&package);
        ctx.rep.check(Check::from_error(
            IcmError {
                detail: format!("{}; the .ice flows were not run", error.detail),
                ..error
            },
            crate::error::Status::Warn,
        ));
    }

    // 1. Build.
    let build = harness::invocation(&project, Select::All, &["--no-run"]);
    let built = ctx.cargo("cargo.test.build", &build, &[])?;
    let harness_exe = built
        .artifacts
        .iter()
        .filter(|artifact| {
            artifact.target_name == harness::TARGET
                && artifact.target_kind.iter().any(|kind| kind == "test")
        })
        .find_map(|artifact| artifact.executable.clone());

    // 2. Run.
    let mut invocation = harness::invocation(&project, Select::All, &["--no-fail-fast"]);
    invocation.offline |= ctx.global.offline;
    if let Some(filter) = &args.filter {
        invocation.trailing.push(filter.clone());
    }
    let cmd = invocation
        .cmd()
        .env("ICED_TEST_BACKEND", &backend)
        .timeout(RUN_TIMEOUT);
    let outcome = ctx.step("cargo.test", &cmd)?;
    let stdout = outcome.stdout_text();
    let stderr = outcome.stderr_text();
    let suites = libtest::parse(&stdout, &stderr);
    let facts = Facts {
        platform: Some("headless"),
        ..Facts::default()
    };

    // 3. Report.
    let mut blocking: Option<IcmError> = None;
    let mut problems = 0usize;
    let mut summaries = Vec::new();
    let (mut passed, mut failed, mut ignored) = (0usize, 0usize, 0usize);
    let mut flows_json = Value::Null;

    for suite in &suites {
        passed += suite.passed;
        failed += suite.failed;
        ignored += suite.ignored;
        summaries.push(json!({
            "name": suite.name,
            "harness": suite.harness,
            "passed": suite.passed,
            "failed": suite.failed,
            "ignored": suite.ignored,
            "filtered": suite.filtered,
            "finished": suite.finished,
        }));

        if suite.harness {
            if let Some(found) = suite.protocol
                && found != harness::PROTOCOL
            {
                blocking.get_or_insert(harness::mismatch(found, &outcome));
                continue;
            }
            if let Some(result) = &suite.result {
                problems += suite.failed;
                let flows = result
                    .get("flows")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for flow in &flows {
                    if let Some(error) = harness::report_flow(ctx, flow) {
                        blocking.get_or_insert(error);
                    }
                }
                flows_json = json!({
                    "passed": suite.passed,
                    "failed": suite.failed,
                    "flows": flows.iter().map(harness::flow_summary).collect::<Vec<_>>(),
                });
                continue;
            }
        }

        if !suite.finished {
            problems += 1;
            let text = format!("{stdout}\n{stderr}");
            // The app's panic is why a harness suite stops early: name it,
            // and the flows that never reported, as `shot --headless` does.
            let panic = signatures::first_panic(&text);
            let mut detail = format!("{} stopped before it finished", suite.name);
            match &panic {
                Some(panic) => detail.push_str(&format!(": the app {}", panic.describe())),
                None => {
                    if let Some(crash) = libtest::crashed_processes(&stderr).first() {
                        detail.push_str(&format!(": {crash}"));
                    }
                }
            }
            if suite.harness {
                let unreported = flow_names(&package.dir().join("tests").join("flows"));
                if !unreported.is_empty() {
                    detail.push_str(&format!(
                        "; no flow reported a result ({})",
                        unreported.join(", ")
                    ));
                }
            }
            let mut error = IcmError::new(CheckId::TestFailed, detail);
            if let Some(panic) = &panic
                && let Some((file, line)) = panic.file_line()
            {
                let path = [project.metadata.workspace_root.as_path(), package.dir()]
                    .into_iter()
                    .map(|base| base.join(&file))
                    .find(|candidate| candidate.is_file())
                    .unwrap_or_else(|| PathBuf::from(&file));
                error = error
                    .evidence(Evidence::line(&path, line, panic.message.clone()))
                    .fix(
                        format!(
                            "Fix the panic at {}:{line}, then rerun.",
                            crate::paths::display(&path)
                        ),
                        &["icm test --json -q"],
                    );
            }
            let error = signatures::annotate(error, &text, &facts);
            let error = with_log(error, &outcome);
            ctx.rep
                .check(Check::from_error(error, crate::error::Status::Fail));
            continue;
        }

        problems += suite.failures.len().max(suite.failed);
        for failure in &suite.failures {
            ctx.rep.check(failure_check(
                &suite.name,
                failure,
                &project.metadata.workspace_root,
                package.dir(),
                &facts,
                &outcome,
            ));
        }
        let ran_any = suite.passed + suite.ignored + suite.filtered > 0;
        if suite.failures.is_empty() && suite.failed == 0 && ran_any {
            let mut detail = format!("{}: {} passed", suite.name, suite.passed);
            if suite.ignored > 0 {
                detail.push_str(&format!(", {} ignored", suite.ignored));
            }
            if suite.filtered > 0 {
                detail.push_str(&format!(", {} filtered out", suite.filtered));
            }
            ctx.rep.check(Check::pass(CheckId::TestPassed, detail));
        }
    }

    // Flows kept outside tests/flows run one by one.
    let flows_dir = project.dir().join(&project.config.config.test.flows);
    let default_dir = package.dir().join("tests").join("flows");
    if has_harness
        && let Some(exe) = harness_exe.clone()
        && !same_dir(&flows_dir, &default_dir)
    {
        let harness = Harness {
            exe,
            package_dir: package.dir().to_path_buf(),
            workspace_root: project.metadata.workspace_root.clone(),
            backend: backend.clone(),
        };
        let extra = run_extra_flows(ctx, &harness, &flows_dir, args.filter.as_deref())?;
        passed += extra.0;
        failed += extra.1;
        problems += extra.1;
        if let Some(error) = extra.2 {
            blocking.get_or_insert(error);
        }
    }

    if !outcome.success() && problems == 0 && blocking.is_none() {
        let mut error = ctx.step_failure("cargo.test", CheckId::TestFailed, &outcome);
        error = signatures::annotate(error, &format!("{stdout}\n{stderr}"), &facts);
        blocking = Some(error);
    }

    ctx.rep.set(
        "tests",
        json!({
            "passed": passed,
            "failed": failed,
            "ignored": ignored,
            "suites": summaries,
            "flows": flows_json,
            "backend": backend,
            "filter": args.filter,
        }),
    );
    ctx.rep.set("profile", json!("test"));

    let flows_note = flows_json
        .get("passed")
        .and_then(Value::as_u64)
        .map(|n| format!(" (including {n} flow(s))"))
        .unwrap_or_default();
    if failed == 0 && blocking.is_none() && outcome.success() {
        ctx.rep.summary(format!(
            "{passed} test(s) passed{flows_note}{}",
            if ignored > 0 {
                format!(", {ignored} ignored")
            } else {
                String::new()
            }
        ));
        ctx.rep.next(
            "icm shot --headless --all-viewports --json -q",
            "see the layout at phone and desktop sizes",
        );
    } else {
        ctx.rep.next(
            "icm ui --headless tree --json -q",
            "what the view shows, to fix a flow",
        );
        if failed > 0 && blocking.is_none() {
            ctx.rep.summary(format!(
                "{failed} test(s) failed, {passed} passed{flows_note}"
            ));
        }
    }

    match blocking {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let canonical =
        |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| crate::paths::normalize(p));
    canonical(a) == canonical(b)
}

/// The flows (`<name>.ice`) in a directory, by name, sorted.
fn flow_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "ice"))
        .filter_map(|path| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    names.sort();
    names
}

fn with_log(mut error: IcmError, outcome: &crate::process::Outcome) -> IcmError {
    if let Some(log) = &outcome.log {
        error = error.evidence(Evidence::file(log));
    }
    error
}

/// The `test.failed` check for one failed test.
fn failure_check(
    suite: &str,
    failure: &libtest::Failure,
    workspace_root: &Path,
    package_dir: &Path,
    facts: &Facts,
    outcome: &crate::process::Outcome,
) -> Check {
    let panic = signatures::first_panic(&failure.output);
    let mut detail = format!("{} ({suite})", failure.name);
    match &panic {
        Some(panic) => detail.push_str(&format!(" {}", panic.describe())),
        None if !failure.output.is_empty() => {
            let first = failure
                .output
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("");
            detail.push_str(&format!(" failed: {first}"));
        }
        None => detail.push_str(" failed"),
    }
    // The lines after the panic's own (assertion values, notes from the
    // test), without the message the detail already has.
    let message = panic.as_ref().map(|p| p.message.trim()).unwrap_or("");
    let more: Vec<&str> = failure
        .output
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty()
                && !line.starts_with("note: run with")
                && !line.contains(" panicked at ")
                && line != message
        })
        .take(8)
        .collect();
    if panic.is_some() && !more.is_empty() {
        detail.push('\n');
        detail.push_str(&more.join("\n"));
    }

    let mut error = IcmError::new(CheckId::TestFailed, detail);
    if let Some(panic) = &panic
        && let Some((file, line)) = panic.file_line()
    {
        let path = [workspace_root, package_dir]
            .into_iter()
            .map(|base| base.join(&file))
            .find(|candidate| candidate.is_file())
            .unwrap_or_else(|| PathBuf::from(&file));
        error = error.evidence(Evidence::line(path, line, panic.message.clone()));
    }
    error = with_log(error, outcome);
    error = error.fix(
        "Fix the code or the test at the evidence, then rerun just it.",
        &[&format!("icm test --filter {} --json -q", failure.name)],
    );
    error = signatures::annotate(error, &failure.output, facts);
    Check::from_error(error, crate::error::Status::Fail)
}

/// Runs each `.ice` file of a flows directory through `icm-ice`. Returns
/// passed and failed counts and a parse error to block on.
fn run_extra_flows(
    ctx: &Ctx,
    harness: &Harness,
    dir: &Path,
    filter: Option<&str>,
) -> Result<(usize, usize, Option<IcmError>)> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter(|path| path.extension().is_some_and(|ext| ext == "ice"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();

    let (mut passed, mut failed, mut blocking) = (0, 0, None);
    for file in files {
        let stem = file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if filter.is_some_and(|filter| !format!("flows::{stem}").contains(filter)) {
            continue;
        }
        let report = ctx
            .rep
            .run_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join(format!("flow-{stem}.json"));
        let args = vec![
            "icm-ice".to_string(),
            file.display().to_string(),
            "--report".to_string(),
            report.display().to_string(),
        ];
        let reply = harness.call(ctx, "harness.ice", &args, harness::COMMAND_TIMEOUT)?;
        if reply.ok {
            passed += 1;
        } else {
            failed += 1;
        }
        if let Some(error) = harness::report_flow(ctx, &reply.result) {
            blocking.get_or_insert(error);
        }
    }
    Ok((passed, failed, blocking))
}
