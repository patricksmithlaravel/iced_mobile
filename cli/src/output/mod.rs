//! The output contract (design §4, Appendix C item 11).
//!
//! - Human mode: protocol lines on stdout (see [`human`]), progress on
//!   stderr. `-q` keeps only `CHECK FAIL`/`CHECK WARN` and `RESULT`.
//! - `--json`: NDJSON on stdout, schema `icm.output/1`; every event has `v`,
//!   `type`, `run` and `t`, and **the last line is always the result
//!   object**. `-q` prints only that line.
//! - Either way, a command that does work writes `runs/<id>/events.ndjson`,
//!   `runs/<id>/result.json` and `last.json` under its icm root
//!   (`<target>/icm`, or the cache dir outside a project).
//! - No secret values (design §1 principle 5): every string in every event
//!   and in the result, progress and content has the secret values icm
//!   knows replaced with `<redacted>` ([`crate::process::secret_values`]),
//!   whatever produced it (a hook's CHECK line, a tool's output, an app's
//!   log). Step logs are redacted by the runner, and the logs a pipeline
//!   saves as artifacts go through [`crate::process::write_redacted`] (and
//!   [`redact`] for JSON records).
//!
//! Exit-code rules (§4.4): the first blocking failure (the error a command
//! returns) sets the exit code and is `errors[0]`; non-blocking FAILs set
//! exit 1 only when nothing blocked; WARN never changes the exit code except
//! under `--strict`; a panic is exit 70; `ok == (exit == 0)`.

pub mod human;
pub mod rundir;

use crate::buildinfo;
use crate::catalogue::CheckId;
use crate::error::{Check, Diagnostic, Evidence, IcmError, Status};
use crate::exit::Exit;
use crate::process::{End, Outcome};
use human::Stream;
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, TryLockError};
use std::time::{Duration, Instant};

/// How many rendered compiler diagnostics `errors[0]` carries.
pub const MAX_DIAGNOSTICS: usize = 10;

/// Output options.
#[derive(Clone, Copy, Debug, Default)]
pub struct Mode {
    /// NDJSON on stdout.
    pub json: bool,
    /// Fewer lines (see the module docs).
    pub quiet: bool,
    /// Echo step argv to stderr.
    pub verbose: bool,
    /// WARN becomes FAIL.
    pub strict: bool,
    /// A content command (`print`, `explain`): in human mode its content is
    /// stdout, and protocol lines go to stderr (only on failure).
    pub content: bool,
}

/// What an invocation is.
#[derive(Clone, Debug)]
pub struct RunInfo {
    /// The run id.
    pub run: String,
    /// The command, e.g. `run`.
    pub command: String,
    /// The platform or target, e.g. `ios-sim`.
    pub target: Option<String>,
    /// The argv, as given.
    pub argv: Vec<String>,
    /// Whether this invocation keeps a run directory.
    pub save: bool,
}

/// The reporter: one per invocation, shared by handle.
#[derive(Clone)]
pub struct Reporter {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    mode: Mode,
    info: RunInfo,
    started: Instant,
    stdout: Box<dyn Write + Send>,
    stderr: Box<dyn Write + Send>,
    root: Option<PathBuf>,
    run_dir: Option<PathBuf>,
    events: Option<File>,
    pending: Vec<String>,
    steps: u32,
    counts: Counts,
    emitted: BTreeSet<(String, String)>,
    blocking: Option<IcmError>,
    failures: Vec<IcmError>,
    warnings: Vec<IcmError>,
    diagnostics: Vec<Diagnostic>,
    diagnostic_keys: BTreeSet<String>,
    workspace_root: Option<PathBuf>,
    artifacts: Map<String, Value>,
    next: Vec<Value>,
    fields: Map<String, Value>,
    summary: Option<String>,
    latest: Option<String>,
    finished: Option<Exit>,
}

#[derive(Default)]
struct Counts {
    pass: u32,
    fail: u32,
    warn: u32,
    skip: u32,
    info: u32,
    failed: Vec<String>,
}

static ACTIVE: OnceLock<Reporter> = OnceLock::new();

impl Reporter {
    /// A reporter writing to the process's stdout and stderr.
    pub fn new(mode: Mode, info: RunInfo) -> Reporter {
        Reporter::with_writers(
            mode,
            info,
            Box::new(std::io::stdout()),
            Box::new(std::io::stderr()),
        )
    }

    /// A reporter writing to the given streams (tests).
    pub fn with_writers(
        mode: Mode,
        info: RunInfo,
        stdout: Box<dyn Write + Send>,
        stderr: Box<dyn Write + Send>,
    ) -> Reporter {
        Reporter {
            inner: Arc::new(Mutex::new(Inner {
                mode,
                info,
                started: Instant::now(),
                stdout,
                stderr,
                root: None,
                run_dir: None,
                events: None,
                pending: Vec::new(),
                steps: 0,
                counts: Counts::default(),
                emitted: BTreeSet::new(),
                blocking: None,
                failures: Vec::new(),
                warnings: Vec::new(),
                diagnostics: Vec::new(),
                diagnostic_keys: BTreeSet::new(),
                workspace_root: None,
                artifacts: Map::new(),
                next: Vec::new(),
                fields: Map::new(),
                summary: None,
                latest: None,
                finished: None,
            })),
        }
    }

    /// Makes this the reporter the panic hook and the signal watchdog use.
    pub fn make_active(&self) {
        let _ = ACTIVE.set(self.clone());
    }

    /// The active reporter, if any.
    pub fn active() -> Option<&'static Reporter> {
        ACTIVE.get()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The run id.
    pub fn run_id(&self) -> String {
        self.lock().info.run.clone()
    }

    /// The output mode.
    pub fn mode(&self) -> Mode {
        self.lock().mode
    }

    /// The run directory, once attached.
    pub fn run_dir(&self) -> Option<PathBuf> {
        self.lock().run_dir.clone()
    }

    /// The icm root (`<target>/icm` or the cache dir), once attached.
    pub fn root(&self) -> Option<PathBuf> {
        self.lock().root.clone()
    }

    /// Whether the result has been written.
    pub fn finished(&self) -> Option<Exit> {
        self.lock().finished
    }

    /// Creates `<root>/runs/<id>/` and starts writing events there. Events
    /// emitted before this are written first. Does nothing if attached.
    pub fn attach(&self, root: &Path) -> std::io::Result<PathBuf> {
        let run = self.run_id();
        let dir = rundir::runs_dir(root).join(&run);
        self.attach_dir(root, &dir)?;
        Ok(dir)
    }

    /// Like [`Reporter::attach`] with an explicit run directory (a detached
    /// child continues the directory its parent created).
    pub fn attach_dir(&self, root: &Path, dir: &Path) -> std::io::Result<()> {
        let mut inner = self.lock();
        if inner.run_dir.is_some() || !inner.info.save {
            return Ok(());
        }
        std::fs::create_dir_all(dir.join("steps"))?;
        let _ = rundir::write_owner(dir);
        let mut events = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("events.ndjson"))?;
        for line in inner.pending.drain(..) {
            let _ = events.write_all(line.as_bytes());
        }
        inner.events = Some(events);
        inner.run_dir = Some(dir.to_path_buf());
        inner.root = Some(root.to_path_buf());
        Ok(())
    }

    /// Emits the `start` event. Every command but `wait` (which replays
    /// another run's events) calls it first.
    pub fn start(&self) {
        let (info, cwd) = {
            let inner = self.lock();
            (
                inner.info.clone(),
                std::env::current_dir()
                    .map(|d| d.display().to_string())
                    .unwrap_or_default(),
            )
        };
        self.emit(json!({
            "type": "start",
            "command": info.command,
            "target": info.target,
            "argv": crate::process::redact_argv(&info.argv),
            "cwd": cwd,
            "icm": buildinfo::json(),
        }));
    }

    /// Emits an event: written to `events.ndjson`, printed per the mode.
    pub fn emit(&self, event: Value) {
        let mut inner = self.lock();
        inner.emit(event);
    }

    /// A progress message: stderr in human mode (not with `-q`).
    pub fn progress(&self, message: impl AsRef<str>) {
        let mut inner = self.lock();
        if !inner.mode.json && !inner.mode.quiet {
            let message = crate::process::redact_values(message.as_ref());
            let _ = writeln!(inner.stderr, "{message}");
        }
    }

    /// Content for a content command (`print`, `explain`): stdout in human
    /// mode; ignored in JSON mode (put it in a result field instead).
    pub fn content(&self, text: impl AsRef<str>) {
        let mut inner = self.lock();
        if !inner.mode.json {
            let text = crate::process::redact_values(text.as_ref());
            let _ = inner.stdout.write_all(text.as_bytes());
            if !text.ends_with('\n') {
                let _ = inner.stdout.write_all(b"\n");
            }
            let _ = inner.stdout.flush();
        }
    }

    /// Reports a check. FAIL is non-blocking here (exit 1 unless something
    /// blocks); return the error from the command to block. Under
    /// `--strict`, WARN becomes FAIL.
    pub fn check(&self, check: Check) {
        let mut inner = self.lock();
        inner.check(check);
    }

    /// Reports an artifact; it also lands in the result's `artifacts`.
    pub fn artifact(&self, kind: &str, path: &Path) {
        self.artifact_with(kind, path, Map::new());
    }

    /// Reports an artifact with extra event fields (`bytes`, `blank`, ...).
    pub fn artifact_with(&self, kind: &str, path: &Path, extra: Map<String, Value>) {
        let display = crate::paths::display(path);
        let mut event = json!({"type": "artifact", "kind": kind, "path": display});
        if let Some(object) = event.as_object_mut() {
            object.extend(extra);
        }
        let mut inner = self.lock();
        let _ = inner
            .artifacts
            .insert(kind.to_string(), Value::String(display));
        inner.emit(event);
    }

    /// Reports an artifact that is not a file (the web app's `url`): the
    /// value is kept as given, not shown as a path.
    pub fn artifact_value(&self, kind: &str, value: &str) {
        let mut inner = self.lock();
        let _ = inner
            .artifacts
            .insert(kind.to_string(), Value::String(value.to_string()));
        inner.emit(json!({"type": "artifact", "kind": kind, "path": value}));
    }

    /// Reports that the app is ready (`url` or `session`, `source`, ...).
    pub fn ready(&self, fields: Value) {
        let mut event = json!({"type": "ready"});
        if let (Some(object), Some(extra)) = (event.as_object_mut(), fields.as_object()) {
            object.extend(extra.clone());
        }
        self.emit(event);
    }

    /// Reports a compiler diagnostic, de-duplicated across targets. The
    /// first [`MAX_DIAGNOSTICS`] errors are attached to `errors[0]` when the
    /// command fails with a build error.
    pub fn diagnostic(&self, mut diagnostic: Diagnostic) {
        let mut inner = self.lock();
        if let Some(root) = &inner.workspace_root {
            diagnostic.dependency = diagnostic.outside(root);
        }
        let key = format!(
            "{}|{:?}|{:?}|{:?}|{:?}|{}",
            diagnostic.level,
            diagnostic.code,
            diagnostic.file,
            diagnostic.line,
            diagnostic.col,
            diagnostic.message
        );
        if !inner.diagnostic_keys.insert(key) {
            return;
        }
        if diagnostic.level == "error" && inner.diagnostics.len() < MAX_DIAGNOSTICS {
            inner.diagnostics.push(diagnostic.clone());
        }
        let mut event = json!(diagnostic);
        event["type"] = json!("diagnostic");
        inner.emit(event);
    }

    /// The app's workspace root: diagnostics from files outside it are
    /// marked `dependency` (and their warnings not printed in human mode).
    pub fn set_workspace_root(&self, root: &Path) {
        self.lock().workspace_root = Some(root.to_path_buf());
    }

    /// The first error-level diagnostics seen.
    pub fn error_diagnostics(&self) -> Vec<Diagnostic> {
        self.lock().diagnostics.clone()
    }

    /// Adds a `next` suggestion (`NEXT` line in human mode).
    pub fn next(&self, cmd: impl Into<String>, why: impl Into<String>) {
        let mut inner = self.lock();
        inner
            .next
            .push(json!({"cmd": cmd.into(), "why": why.into()}));
    }

    /// Sets a result field (`app`, `device`, `process`, `session`, `tools`,
    /// `inputs`, `profile`, `status`, `screen`, ...).
    pub fn set(&self, key: &str, value: Value) {
        let mut inner = self.lock();
        let _ = inner.fields.insert(key.to_string(), value);
    }

    /// A result field set so far.
    pub fn field(&self, key: &str) -> Option<Value> {
        self.lock().fields.get(key).cloned()
    }

    /// Sets the result's one-line summary.
    pub fn summary(&self, summary: impl Into<String>) {
        self.lock().summary = Some(summary.into());
    }

    /// Forgets the summary set so far, so the result's summary comes from
    /// the error (an `--attach` that ends in a crash after "is running").
    pub fn clear_summary(&self) {
        self.lock().summary = None;
    }

    /// Points `latest/<platform>` at this run when it finishes.
    pub fn latest(&self, platform: &str) {
        self.lock().latest = Some(platform.to_string());
    }

    /// The next step log path (`steps/NN-<name>.log`), if a run directory
    /// is attached.
    pub fn step_log(&self, name: &str) -> Option<PathBuf> {
        let mut inner = self.lock();
        inner.steps += 1;
        let number = inner.steps;
        inner
            .run_dir
            .as_ref()
            .map(|dir| dir.join("steps").join(format!("{number:02}-{name}.log")))
    }

    /// Emits the `step` begin event.
    pub fn step_begin(
        &self,
        name: &str,
        argv: &[String],
        env: &[(String, String)],
        cwd: Option<&Path>,
    ) {
        let env: Map<String, Value> = env
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        self.emit(json!({
            "type": "step",
            "name": name,
            "phase": "begin",
            "argv": argv,
            "env": env,
            "cwd": cwd.map(crate::paths::display),
        }));
    }

    /// Emits the `step` end event for a finished process.
    pub fn step_end(&self, name: &str, outcome: &Outcome) {
        let end = match outcome.end {
            End::Exited(_) => "exited",
            End::Signaled(_) => "signaled",
            End::TimedOut(_) => "timed_out",
            End::Interrupted(_) => "interrupted",
        };
        self.emit(json!({
            "type": "step",
            "name": name,
            "phase": "end",
            "ok": outcome.success(),
            "end": end,
            "code": outcome.code(),
            "ms": outcome.duration.as_millis() as u64,
            "log": outcome.log.as_deref().map(crate::paths::display),
        }));
    }

    /// Emits a `step` end event for an internal step (no process).
    pub fn step_end_internal(&self, name: &str, ok: bool, ms: u64) {
        self.emit(json!({
            "type": "step",
            "name": name,
            "phase": "end",
            "ok": ok,
            "end": "internal",
            "ms": ms,
        }));
    }

    /// Writes the result and returns the exit code. Only the first call
    /// does anything; later calls return the same code.
    pub fn finish(&self, outcome: Result<(), IcmError>) -> Exit {
        let mut inner = self.lock();
        if let Some(exit) = inner.finished {
            return exit;
        }
        if let Err(error) = outcome {
            inner.block(error);
        }
        inner.finish()
    }

    /// Finishes with `run.interrupted` for `signal`; the command's own error
    /// (often a child it saw fail because icm killed it) follows in
    /// `errors[]`.
    pub fn finish_interrupted(&self, signal: i32, error: IcmError) -> Exit {
        let mut inner = self.lock();
        if let Some(exit) = inner.finished {
            return exit;
        }
        let interrupted = interrupted(signal);
        let same = error.id == interrupted.id;
        inner.block(interrupted);
        if !same {
            inner.block(error);
        }
        inner.finish()
    }

    /// Finishes with exit 70 for a panic.
    pub fn finish_panic(&self, message: &str) -> Exit {
        let error = IcmError::new(CheckId::InternalBug, format!("icm panicked: {message}")).fix(
            "Report this with the run_dir; rerunning may work around a transient cause.",
            &[],
        );
        let mut inner = self.lock();
        if let Some(exit) = inner.finished {
            return exit;
        }
        // A panic overrides everything.
        if let Some(previous) = inner.blocking.take() {
            inner.failures.insert(0, previous);
        }
        inner.block(error);
        inner.finish()
    }

    /// The watchdog's finish: like [`Reporter::finish_interrupted`], but
    /// gives up (returns `None`) if the reporter stays locked for `patience`.
    pub fn try_finish_interrupted(&self, signal: i32, patience: Duration) -> Option<Exit> {
        let until = Instant::now() + patience;
        loop {
            let mut inner = match self.inner.try_lock() {
                Ok(inner) => inner,
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                Err(TryLockError::WouldBlock) if Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                Err(TryLockError::WouldBlock) => return None,
            };
            if let Some(exit) = inner.finished {
                return Some(exit);
            }
            inner.block(interrupted(signal));
            return Some(inner.finish());
        }
    }

    /// Prints another run's NDJSON lines (`icm wait`) as if they were this
    /// invocation's, and finishes with that run's result: its exit code,
    /// and no result object of this invocation's own.
    pub fn replay(&self, lines: &[String]) -> Exit {
        let mut inner = self.lock();
        if let Some(exit) = inner.finished {
            return exit;
        }
        let mut exit = Exit::Internal;
        for line in lines {
            let Ok(event) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let kind = event
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if kind == "result" {
                exit = event
                    .get("exit")
                    .and_then(Value::as_i64)
                    .and_then(Exit::from_code)
                    .unwrap_or(Exit::Internal);
            }
            inner.print(&kind, line);
        }
        inner.finished = Some(exit);
        exit
    }
}

/// The error for a signal.
pub fn interrupted(signal: i32) -> IcmError {
    IcmError::new(
        CheckId::RunInterrupted,
        format!(
            "received {}; child processes were stopped",
            crate::signals::name(signal)
        ),
    )
}

impl Inner {
    fn emit(&mut self, event: Value) {
        let (kind, line) = self.line(event);
        self.record(&line);
        self.print(&kind, &line);
    }

    /// The NDJSON line for an event: `v`, `type`, `run` and `t` first,
    /// secrets redacted.
    fn line(&self, mut event: Value) -> (String, String) {
        redact(&mut event);
        let t = self.started.elapsed().as_millis() as u64;
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("event")
            .to_string();

        if let Some(object) = event.as_object_mut() {
            let _ = object.remove("type");
            let _ = object.remove("v");
            let _ = object.remove("run");
            let _ = object.remove("t");
        }

        // `v`, `type`, `run` and `t` first, then the rest.
        let rest = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
        let rest = rest.trim_start_matches('{');
        let head = format!(
            "{{\"v\":1,\"type\":{},\"run\":{},\"t\":{t}",
            json!(kind),
            json!(self.info.run)
        );
        let line = if rest == "}" {
            format!("{head}}}\n")
        } else {
            format!("{head},{rest}\n")
        };
        (kind, line)
    }

    /// Writes a line to `events.ndjson` (or keeps it until attached).
    fn record(&mut self, line: &str) {
        if self.info.save {
            match self.events.as_mut() {
                Some(file) => {
                    let _ = file.write_all(line.as_bytes());
                }
                None => self.pending.push(line.to_string()),
            }
        }
    }

    /// Prints a line per the mode.
    fn print(&mut self, kind: &str, line: &str) {
        let is_result = kind == "result";
        if self.mode.json {
            if !self.mode.quiet || is_result {
                let _ = self.stdout.write_all(line.as_bytes());
                if !line.ends_with('\n') {
                    let _ = self.stdout.write_all(b"\n");
                }
                let _ = self.stdout.flush();
            }
            return;
        }

        let full: Value = serde_json::from_str(line).unwrap_or(Value::Null);
        let rendered = human::render(&full, self.mode.quiet, self.mode.verbose);

        if self.mode.content {
            // Content commands keep stdout for their content. Protocol lines
            // go to stderr, and only when something failed.
            let failing = match kind {
                "check" => matches!(full.get("status").and_then(Value::as_str), Some("fail")),
                "result" => !full.get("ok").and_then(Value::as_bool).unwrap_or(false),
                _ => false,
            };
            if failing {
                for (_, text) in rendered {
                    let _ = writeln!(self.stderr, "{text}");
                }
            }
            return;
        }

        for (stream, text) in rendered {
            match stream {
                Stream::Stdout => {
                    let _ = writeln!(self.stdout, "{text}");
                }
                Stream::Stderr => {
                    let _ = writeln!(self.stderr, "{text}");
                }
            }
        }
        let _ = self.stdout.flush();
    }

    fn check(&mut self, mut check: Check) {
        if self.mode.strict && check.status == Status::Warn {
            check.status = Status::Fail;
            check.error.exit = Exit::CheckFailed;
            check.error.detail = format!("{} (WARN made FAIL by --strict)", check.error.detail);
        }

        let key = (check.error.id.to_string(), check.error.detail.clone());
        let _ = self.emitted.insert(key);

        match check.status {
            Status::Pass => self.counts.pass += 1,
            Status::Fail => {
                self.counts.fail += 1;
                self.counts.failed.push(check.error.id.to_string());
                self.failures.push(check.error.clone());
            }
            Status::Warn => {
                self.counts.warn += 1;
                self.warnings.push(check.error.clone());
            }
            Status::Skip => self.counts.skip += 1,
            Status::Info => self.counts.info += 1,
        }

        self.emit(check.to_event());
    }

    /// Records the blocking error (once) and emits its check event unless
    /// the same finding was already reported.
    fn block(&mut self, error: IcmError) {
        if self.blocking.is_some() {
            self.failures.push(error);
            return;
        }

        let key = (error.id.to_string(), error.detail.clone());
        if self.emitted.contains(&key) {
            // Reported as a non-blocking FAIL first; it moves to errors[0].
            self.failures
                .retain(|failure| (failure.id.to_string(), failure.detail.clone()) != key);
        } else {
            let check = Check::from_error(error.clone(), Status::Fail);
            self.counts.fail += 1;
            self.counts.failed.push(error.id.to_string());
            let _ = self.emitted.insert(key);
            self.emit(check.to_event());
        }

        self.blocking = Some(error);
    }

    fn exit(&self) -> Exit {
        match &self.blocking {
            Some(error) => error.exit,
            None if !self.failures.is_empty() => Exit::CheckFailed,
            None => Exit::Ok,
        }
    }

    fn finish(&mut self) -> Exit {
        let exit = self.exit();

        // Attach build diagnostics to a build failure (Appendix C item 11).
        if let Some(blocking) = self.blocking.as_mut()
            && blocking.exit == Exit::Build
            && blocking.diagnostics.is_empty()
        {
            blocking.diagnostics = self.diagnostics.clone();
            for diagnostic in self.diagnostics.iter().take(3) {
                if let (Some(file), Some(line)) = (&diagnostic.file, diagnostic.line) {
                    blocking.evidence.push(Evidence {
                        path: file.clone(),
                        line: Some(line),
                        excerpt: Some(diagnostic.message.clone()),
                    });
                }
            }
        }

        // A command that does work but never found its project keeps its
        // run in the cache dir. A usage error did no work: no run dir.
        if self.info.save && self.run_dir.is_none() && exit != Exit::Usage {
            let root = crate::paths::cache_dir();
            let dir = rundir::runs_dir(&root).join(&self.info.run);
            if std::fs::create_dir_all(dir.join("steps")).is_ok()
                && let Ok(mut events) = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join("events.ndjson"))
            {
                for line in self.pending.drain(..) {
                    let _ = events.write_all(line.as_bytes());
                }
                self.events = Some(events);
                self.run_dir = Some(dir);
                self.root = Some(root);
            }
        }

        let result = self.result(exit);

        // events.ndjson gets the result line before result.json exists, so
        // a reader that sees result.json (`icm wait`) finds the whole
        // stream; stdout gets it last, once the files are in place.
        let (kind, line) = self.line(result.clone());
        self.record(&line);

        if let (Some(dir), Some(root)) = (self.run_dir.clone(), self.root.clone()) {
            let mut text = serde_json::to_string_pretty(&result).unwrap_or_default();
            text.push('\n');
            let _ = rundir::write_atomic(&dir.join("result.json"), text.as_bytes());
            let _ = rundir::write_atomic(&root.join("last.json"), text.as_bytes());
            if let Some(platform) = self.latest.clone() {
                let _ = rundir::link_latest(&root, &platform, &dir);
            }
        }

        self.finished = Some(exit);
        self.print(&kind, &line);

        if let Some(root) = self.root.clone() {
            let _ = rundir::prune(&root, rundir::KEEP_RUNS, &self.info.run);
        }

        exit
    }

    fn result(&self, exit: Exit) -> Value {
        let errors: Vec<Value> = self
            .blocking
            .iter()
            .chain(self.failures.iter())
            .map(IcmError::to_json)
            .collect();
        let warnings: Vec<Value> = self.warnings.iter().map(IcmError::to_json).collect();

        let summary = self.summary.clone().unwrap_or_else(|| {
            match self.blocking.as_ref().or(self.failures.first()) {
                Some(error) if !error.detail.is_empty() => {
                    format!("{}: {}", error.id, first_line(&error.detail))
                }
                Some(error) => format!("{}: {}", error.id, error.title),
                None => {
                    let mut text = format!("icm {}", self.info.command);
                    if let Some(target) = &self.info.target {
                        text.push(' ');
                        text.push_str(target);
                    }
                    text.push_str(" ok");
                    if !self.warnings.is_empty() {
                        text.push_str(&format!(" with {} warning(s)", self.warnings.len()));
                    }
                    text
                }
            }
        });

        let mut result = json!({
            "v": 1,
            "type": "result",
            "run": self.info.run,
            "schema": "icm.result/1",
            "command": self.info.command,
            "target": self.info.target,
            "profile": null,
            "ok": exit.is_ok(),
            "exit": exit.code(),
            "summary": summary,
            "app": null,
            "device": null,
            "process": null,
            "checks": {
                "pass": self.counts.pass,
                "fail": self.counts.fail,
                "warn": self.counts.warn,
                "skip": self.counts.skip,
                "info": self.counts.info,
                "failed": self.counts.failed,
            },
            "errors": errors,
            "warnings": warnings,
            "artifacts": self.artifacts,
            "session": null,
            "owner_steps": [],
            "inputs": {},
            "tools": {},
            "next": self.next,
            "ms": self.started.elapsed().as_millis() as u64,
            "run_dir": self.run_dir.as_deref().map(crate::paths::display),
        });

        if let Some(object) = result.as_object_mut() {
            for (key, value) in &self.fields {
                let _ = object.insert(key.clone(), value.clone());
            }
        }

        redact(&mut result);
        result
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

/// Replaces the secret values icm knows in every string of a JSON value.
/// A file of JSON records is redacted this way before it is written, since
/// JSON escaping can hide a secret from a search of the text.
pub fn redact(value: &mut Value) {
    let secrets = crate::process::secret_values();
    if !secrets.is_empty() {
        redact_strings(value, &secrets);
    }
}

fn redact_strings(value: &mut Value, secrets: &[String]) {
    match value {
        Value::String(text) => {
            if secrets.iter().any(|secret| text.contains(secret.as_str())) {
                *text = crate::process::redact_with(text, secrets);
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_strings(item, secrets);
            }
        }
        Value::Object(map) => {
            for (_, item) in map.iter_mut() {
                redact_strings(item, secrets);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::CheckId;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Buffer {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    fn reporter(mode: Mode, save: bool) -> (Reporter, Buffer, Buffer) {
        let out = Buffer::default();
        let err = Buffer::default();
        let info = RunInfo {
            run: rundir::new_run_id("run", Some("web")),
            command: "run".into(),
            target: Some("web".into()),
            argv: vec!["icm".into(), "run".into(), "web".into()],
            save,
        };
        let reporter =
            Reporter::with_writers(mode, info, Box::new(out.clone()), Box::new(err.clone()));
        reporter.start();
        (reporter, out, err)
    }

    fn json_mode() -> Mode {
        Mode {
            json: true,
            ..Mode::default()
        }
    }

    fn lines(buffer: &Buffer) -> Vec<Value> {
        buffer
            .text()
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid JSON line"))
            .collect()
    }

    #[test]
    fn an_interrupted_command_exits_130_and_keeps_its_own_error() {
        let (rep, out, _) = reporter(json_mode(), false);
        let exit = rep.finish_interrupted(
            libc::SIGTERM,
            IcmError::new(CheckId::ConfigInvalid, "cargo metadata failed: "),
        );
        assert_eq!(exit, Exit::Interrupted);
        let result = lines(&out).pop().unwrap();
        assert_eq!(result["exit"], 130);
        assert_eq!(result["errors"][0]["id"], "run.interrupted");
        assert_eq!(result["errors"][1]["id"], "config.invalid");

        let (rep, out, _) = reporter(json_mode(), false);
        let exit = rep.finish_interrupted(libc::SIGINT, interrupted(libc::SIGINT));
        assert_eq!(exit, Exit::Interrupted);
        let result = lines(&out).pop().unwrap();
        assert_eq!(result["errors"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn json_lines_have_the_common_fields_and_end_with_the_result() {
        let (rep, out, _) = reporter(json_mode(), false);
        rep.check(Check::pass(CheckId::DepsSingleIced, "one iced"));
        rep.artifact("preview", Path::new("/tmp/screen.preview.png"));
        rep.next("icm stop web", "stop the session");
        let exit = rep.finish(Ok(()));
        assert_eq!(exit, Exit::Ok);

        let events = lines(&out);
        assert_eq!(events.first().unwrap()["type"], "start");
        for event in &events {
            assert_eq!(event["v"], 1);
            assert!(event["run"].is_string());
            assert!(event["t"].is_u64());
            assert!(event["type"].is_string());
        }
        let result = events.last().unwrap();
        assert_eq!(result["type"], "result");
        assert_eq!(result["schema"], "icm.result/1");
        assert_eq!(result["ok"], true);
        assert_eq!(result["exit"], 0);
        assert_eq!(result["checks"]["pass"], 1);
        assert_eq!(result["artifacts"]["preview"], "/tmp/screen.preview.png");
        assert_eq!(result["next"][0]["cmd"], "icm stop web");

        // The raw line starts with the ordered head.
        assert!(
            out.text()
                .lines()
                .last()
                .unwrap()
                .starts_with("{\"v\":1,\"type\":\"result\"")
        );
    }

    #[test]
    fn quiet_json_prints_only_the_result() {
        let (rep, out, _) = reporter(
            Mode {
                json: true,
                quiet: true,
                ..Mode::default()
            },
            false,
        );
        rep.check(Check::warn(CheckId::RunScreenBlank, "all black"));
        let _ = rep.finish(Ok(()));
        let events = lines(&out);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "result");
        assert_eq!(events[0]["warnings"][0]["id"], "run.screen_blank");
        assert_eq!(events[0]["ok"], true);
    }

    #[test]
    fn the_blocking_error_is_errors_zero_and_sets_the_exit() {
        let (rep, out, _) = reporter(json_mode(), false);
        rep.check(Check::fail(CheckId::WebSizeBudget, "5000 KB > 4096 KB"));
        let exit = rep.finish(Err(IcmError::new(
            CheckId::RunAppPanicked,
            "panicked at src/lib.rs:41:9",
        )));
        assert_eq!(exit, Exit::AppDied);
        let result = lines(&out).pop().unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["exit"], 10);
        assert_eq!(result["errors"][0]["id"], "run.app_panicked");
        assert_eq!(result["errors"][1]["id"], "web.size_budget");
        assert_eq!(result["checks"]["fail"], 2);
        assert!(result["summary"].as_str().unwrap().contains("panicked"));
    }

    #[test]
    fn non_blocking_failures_exit_one() {
        let (rep, _, _) = reporter(json_mode(), false);
        rep.check(Check::fail(CheckId::DepsSingleIced, "two sources"));
        assert_eq!(rep.finish(Ok(())), Exit::CheckFailed);
    }

    #[test]
    fn a_reported_failure_returned_as_blocking_is_not_duplicated() {
        let (rep, out, _) = reporter(json_mode(), false);
        let check = Check::fail(CheckId::DepsSingleIced, "two sources");
        rep.check(check.clone());
        rep.check(Check::fail(CheckId::DepsWinitFloor, "0.30.9"));
        let exit = rep.finish(Err(check.into_error()));
        assert_eq!(exit, Exit::Config);
        let events = lines(&out);
        let result = events.last().unwrap();
        assert_eq!(result["errors"][0]["id"], "deps.single_iced");
        assert_eq!(result["errors"].as_array().unwrap().len(), 2);
        let check_events = events.iter().filter(|e| e["type"] == "check").count();
        assert_eq!(check_events, 2);
    }

    #[test]
    fn strict_turns_warnings_into_failures() {
        let (rep, _, _) = reporter(
            Mode {
                json: true,
                strict: true,
                ..Mode::default()
            },
            false,
        );
        rep.check(Check::warn(CheckId::RunScreenBlank, "all black"));
        assert_eq!(rep.finish(Ok(())), Exit::CheckFailed);
    }

    #[test]
    fn panics_override_everything() {
        let (rep, out, _) = reporter(json_mode(), false);
        rep.check(Check::fail(CheckId::WebSizeBudget, "too big"));
        assert_eq!(rep.finish_panic("boom at src/x.rs:1"), Exit::Internal);
        let result = lines(&out).pop().unwrap();
        assert_eq!(result["exit"], 70);
        assert_eq!(result["errors"][0]["id"], "internal.bug");
        // Finishing again changes nothing.
        assert_eq!(rep.finish(Ok(())), Exit::Internal);
        assert_eq!(
            lines(&out).iter().filter(|e| e["type"] == "result").count(),
            1
        );
    }

    #[test]
    fn dependency_warnings_are_marked_and_not_printed() {
        let (rep, _out, err) = reporter(Mode::default(), false);
        rep.set_workspace_root(Path::new("/work/app"));
        let warning = |file: &str| Diagnostic {
            level: "warning".into(),
            code: None,
            message: "use of deprecated method".into(),
            rendered: format!("warning: use of deprecated method\n --> {file}:1:1\n"),
            file: Some(file.into()),
            line: Some(1),
            col: Some(1),
            targets: vec![],
            dependency: false,
        };
        rep.diagnostic(warning("/fork/winit/src/lib.rs"));
        rep.diagnostic(warning("src/lib.rs"));
        rep.diagnostic(warning("/work/app/src/main.rs"));
        let text = err.text();
        assert!(!text.contains("/fork/winit"), "{text}");
        assert!(text.contains("--> src/lib.rs"), "{text}");
        assert!(text.contains("/work/app/src/main.rs"), "{text}");
    }

    #[test]
    fn build_failures_carry_the_diagnostics() {
        let (rep, out, _) = reporter(json_mode(), false);
        let diagnostic = Diagnostic {
            level: "error".into(),
            code: Some("E0308".into()),
            message: "mismatched types".into(),
            rendered: "error[E0308]: mismatched types\n --> src/lib.rs:4:5\n".into(),
            file: Some("src/lib.rs".into()),
            line: Some(4),
            col: Some(5),
            targets: vec!["app".into()],
            dependency: false,
        };
        rep.diagnostic(diagnostic.clone());
        rep.diagnostic(diagnostic); // de-duplicated
        let _ = rep.finish(Err(IcmError::new(CheckId::BuildCompileError, "1 error")));
        let events = lines(&out);
        assert_eq!(
            events.iter().filter(|e| e["type"] == "diagnostic").count(),
            1
        );
        let result = events.last().unwrap();
        assert_eq!(result["exit"], 5);
        assert_eq!(result["errors"][0]["diagnostics"][0]["line"], 4);
        assert_eq!(result["errors"][0]["evidence"][0]["path"], "src/lib.rs");
    }

    #[test]
    fn human_mode_prints_protocol_lines_only() {
        let (rep, out, err) = reporter(Mode::default(), false);
        rep.progress("building...");
        rep.check(Check::pass(CheckId::DepsSingleIced, "one iced"));
        rep.check(Check::warn(CheckId::RunScreenBlank, "all black"));
        rep.artifact("preview", Path::new("/tmp/p.png"));
        rep.next("icm stop web", "stop");
        let _ = rep.finish(Ok(()));
        let text = out.text();
        for line in text.lines() {
            let keyword = line.split_whitespace().next().unwrap_or("");
            assert!(
                line.starts_with("  ")
                    || ["CHECK", "ARTIFACT", "NEXT", "RESULT", "STEP", "READY"].contains(&keyword),
                "not a protocol line: {line}"
            );
        }
        assert!(text.contains("CHECK PASS deps.single_iced: one iced"));
        assert!(
            text.lines()
                .last()
                .unwrap()
                .starts_with("RESULT ok run web exit=0 run=")
        );
        assert_eq!(err.text(), "building...\n");
    }

    #[test]
    fn quiet_human_mode_keeps_failures_and_the_result() {
        let (rep, out, _) = reporter(
            Mode {
                quiet: true,
                ..Mode::default()
            },
            false,
        );
        rep.check(Check::pass(CheckId::DepsSingleIced, "one iced"));
        rep.check(Check::warn(CheckId::RunScreenBlank, "all black"));
        let _ = rep.finish(Err(IcmError::new(CheckId::ConfigNotFound, "no icm.toml")));
        let text = out.text();
        assert!(!text.contains("CHECK PASS"));
        assert!(text.contains("CHECK WARN run.screen_blank"));
        assert!(text.contains("CHECK FAIL config.not_found: no icm.toml"));
        assert!(text.contains("RESULT fail run web exit=3"));
    }

    #[test]
    fn run_directories_get_events_result_last_and_latest() {
        let root = tempfile::tempdir().unwrap();
        let (rep, out, _) = reporter(json_mode(), true);
        rep.check(Check::pass(CheckId::DepsSingleIced, "before attach"));
        let dir = rep.attach(root.path()).unwrap();
        let log = rep.step_log("cargo.build").unwrap();
        assert!(log.ends_with("steps/01-cargo.build.log"));
        rep.latest("web");
        let _ = rep.finish(Ok(()));

        let events = std::fs::read_to_string(dir.join("events.ndjson")).unwrap();
        let events: Vec<Value> = events
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(events[0]["type"], "start");
        assert_eq!(events[1]["type"], "check");
        assert_eq!(events.last().unwrap()["type"], "result");
        assert_eq!(lines(&out).len(), events.len());

        let result: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("result.json")).unwrap())
                .unwrap();
        assert_eq!(result["exit"], 0);
        let last: Value =
            serde_json::from_str(&std::fs::read_to_string(root.path().join("last.json")).unwrap())
                .unwrap();
        assert_eq!(last["run"], result["run"]);
        assert!(root.path().join("latest/web/result.json").exists());
    }

    #[test]
    fn secrets_are_redacted_in_events_results_and_files() {
        // A secret icm handed to a child under a secret name.
        let given = crate::process::Cmd::new("/usr/bin/true")
            .env("ICM_UNIT_REPORT_TOKEN", "rep-unit-secret");
        let _ = crate::process::run(&given, None, None).unwrap();

        let root = tempfile::tempdir().unwrap();
        let (rep, out, err) = reporter(Mode::default(), true);
        let dir = rep.attach(root.path()).unwrap();
        rep.progress("using rep-unit-secret");
        rep.check(Check::warn(
            CheckId::RunScreenBlank,
            "the hook was given rep-unit-secret",
        ));
        rep.set(
            "hooks",
            json!([{"log": "x", "nested": ["rep-unit-secret"]}]),
        );
        let _ = rep.finish(Ok(()));

        for text in [
            out.text(),
            err.text(),
            std::fs::read_to_string(dir.join("events.ndjson")).unwrap(),
            std::fs::read_to_string(dir.join("result.json")).unwrap(),
            std::fs::read_to_string(root.path().join("last.json")).unwrap(),
        ] {
            assert!(!text.contains("rep-unit-secret"), "{text}");
        }
        assert!(
            out.text()
                .contains("CHECK WARN run.screen_blank: the hook was given <redacted>")
        );
        assert_eq!(err.text(), "using <redacted>\n");
        let result: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("result.json")).unwrap())
                .unwrap();
        assert_eq!(
            result["warnings"][0]["detail"],
            "the hook was given <redacted>"
        );
        assert_eq!(result["hooks"][0]["nested"][0], "<redacted>");
    }

    #[test]
    fn content_commands_keep_stdout_for_content() {
        let (rep, out, err) = reporter(
            Mode {
                content: true,
                ..Mode::default()
            },
            false,
        );
        rep.content("export JAVA_HOME='/jdk'");
        let _ = rep.finish(Ok(()));
        assert_eq!(out.text(), "export JAVA_HOME='/jdk'\n");
        assert_eq!(err.text(), "");

        let (rep, out, err) = reporter(
            Mode {
                content: true,
                ..Mode::default()
            },
            false,
        );
        let _ = rep.finish(Err(IcmError::new(CheckId::EnvJdkMissing, "no JDK 17+")));
        assert_eq!(out.text(), "");
        assert!(
            err.text()
                .contains("CHECK FAIL env.jdk_missing: no JDK 17+")
        );
        assert!(err.text().contains("RESULT fail"));
    }
}
