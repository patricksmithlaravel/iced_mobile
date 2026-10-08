//! `--detach` (Appendix C item 24): builds outlast an agent's command
//! timeout (agent shells commonly allow at most 10 minutes), so a
//! command can run in the background and be collected with `icm wait`.
//!
//! The parent creates the run directory, re-executes icm without
//! `--detach` in a new session (so killing the agent's shell does not kill
//! it), records `detached.json`, and returns `{run, status: "running"}` at
//! once. The child continues in the same run directory (`ICM_RUN_ID`,
//! `ICM_RUN_DIR`, `ICM_RUN_ROOT`) and writes `events.ndjson` and
//! `result.json` there as usual.

use crate::catalogue::CheckId;
use crate::context::Ctx;
use crate::error::{IcmError, Result};
use crate::output::rundir;
use crate::process::{self, Cmd};
use serde_json::json;

/// Set in the detached child.
pub const DETACHED_ENV: &str = "ICM_DETACHED";

/// Starts the command in the background.
pub fn run(ctx: &mut Ctx) -> Result<()> {
    // The child resolves the same project; fail fast here on a broken
    // config instead of in the background. Outside a project the run goes
    // to the cache dir.
    let root = match ctx.project() {
        Ok(project) => project.icm_dir.clone(),
        Err(error) if error.id == CheckId::ConfigNotFound.id() => crate::paths::cache_dir(),
        Err(error) => return Err(error),
    };

    let run = ctx.rep.run_id();
    let dir = rundir::runs_dir(&root).join(&run);
    std::fs::create_dir_all(dir.join("steps")).map_err(|error| {
        IcmError::new(
            CheckId::InternalBug,
            format!("cannot create {}: {error}", dir.display()),
        )
    })?;

    let exe = std::env::current_exe().map_err(|error| {
        IcmError::new(
            CheckId::InternalBug,
            format!("cannot find icm's own executable: {error}"),
        )
    })?;
    let args: Vec<&String> = ctx
        .argv
        .iter()
        .skip(1)
        .filter(|arg| arg.as_str() != "--detach")
        .collect();

    let cmd = Cmd::new(&exe)
        .args(&args)
        .env("ICM_RUN_ID", &run)
        .env("ICM_RUN_DIR", &dir)
        .env("ICM_RUN_ROOT", &root)
        .env(DETACHED_ENV, "1")
        .keep_locale();

    let pid = process::spawn_detached(
        &cmd,
        &dir.join("detached.stdout"),
        &dir.join("detached.stderr"),
    )
    .map_err(|error| {
        IcmError::new(
            CheckId::InternalBug,
            format!("cannot start the detached icm: {error}"),
        )
    })?;

    let record = json!({
        "pid": pid,
        // What the process is now: `icm wait` and `prune` tell it from any
        // process that has the pid after it exits.
        "identity": crate::procid::capture(pid as i32),
        "argv": process::redact_argv(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>()),
        "cwd": std::env::current_dir().ok(),
        "started": crate::time::Utc::now().rfc3339(),
    });
    let _ = rundir::write_atomic(
        &dir.join("detached.json"),
        serde_json::to_string_pretty(&record)
            .unwrap_or_default()
            .as_bytes(),
    );

    ctx.rep.set("status", json!("running"));
    ctx.rep.set("pid", json!(pid));
    ctx.rep.set("run_dir", json!(crate::paths::display(&dir)));
    ctx.rep.summary(format!(
        "started in the background as run {run} (pid {pid})"
    ));
    ctx.rep.next(
        format!("icm wait {run} --timeout 9m --json -q"),
        "wait for the result; call it again while it is still running",
    );
    Ok(())
}
