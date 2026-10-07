//! The process runner (design §3 "Process hygiene", Appendix C items 2, 8, 24).
//!
//! Every child icm starts:
//! - has stdin on `/dev/null`, so a tool that wants to prompt fails fast;
//! - runs in its own process group, which a timeout or a signal kills whole
//!   (SIGTERM, then SIGKILL after a grace period);
//! - writes to files, never to pipes, so a daemon that inherits stdout (the
//!   adb server) cannot hang icm;
//! - gets `GIT_TERMINAL_PROMPT=0`, `GIT_SSH_COMMAND="ssh -oBatchMode=yes"`
//!   (unless set), `RUSTUP_AUTO_INSTALL=0`, `CARGO_TERM_COLOR=never` and,
//!   unless the step opts out, `LC_ALL=C`;
//! - is logged with its argv and environment delta, secrets redacted.
//!
//! The secret values icm knows ([`secret_values`]) are also what the
//! reporter redacts from every event and result, and what every file icm
//! keeps from a tool's or an app's output is redacted of
//! ([`write_redacted`], [`copy_redacted`], [`redact_in_place`]).

use crate::signals;
use crate::time::{Utc, format_duration};
use serde_json::Value;
use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::FileExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};

/// The placeholder secrets are replaced with.
pub const REDACTED: &str = "<redacted>";

/// A command to run.
#[derive(Clone, Debug)]
pub struct Cmd {
    /// The program: a path, or a name looked up on `PATH`.
    pub program: OsString,
    /// The arguments.
    pub args: Vec<OsString>,
    /// Environment changes: `Some` sets, `None` removes.
    pub env: Vec<(OsString, Option<OsString>)>,
    /// The working directory.
    pub cwd: Option<PathBuf>,
    /// The time limit.
    pub timeout: Option<Duration>,
    /// Whether stdout is captured separately from the log.
    pub capture_stdout: bool,
    /// Whether `LC_ALL=C` is set.
    pub locale_c: bool,
    /// How long a killed group gets between SIGTERM and SIGKILL.
    pub kill_grace: Duration,
    /// Whether the log's output is copied to icm's stderr as it arrives
    /// (`-v`).
    pub mirror: bool,
}

impl Cmd {
    /// A command for a program path or name.
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Cmd {
            program: program.as_ref().to_os_string(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
            timeout: None,
            capture_stdout: true,
            locale_c: true,
            kill_grace: Duration::from_secs(2),
            mirror: false,
        }
    }

    /// A command for an external tool, honouring `ICM_TOOL_<NAME>`
    /// (e.g. `ICM_TOOL_XCRUN=/path/to/fake-xcrun`).
    pub fn tool(name: &str) -> Self {
        Cmd::new(tool_path(name))
    }

    /// Adds an argument.
    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    /// Adds arguments.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_os_string()));
        self
    }

    /// Sets an environment variable.
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env.push((
            key.as_ref().to_os_string(),
            Some(value.as_ref().to_os_string()),
        ));
        self
    }

    /// Sets several environment variables.
    pub fn envs<I, K, V>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        for (key, value) in vars {
            self = self.env(key, value);
        }
        self
    }

    /// Removes an environment variable.
    pub fn env_remove(mut self, key: impl AsRef<OsStr>) -> Self {
        self.env.push((key.as_ref().to_os_string(), None));
        self
    }

    /// Sets the working directory.
    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// Sets the time limit.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Sends stdout to the log with stderr instead of capturing it.
    pub fn stdout_to_log(mut self) -> Self {
        self.capture_stdout = false;
        self
    }

    /// Keeps the caller's locale instead of `LC_ALL=C`.
    pub fn keep_locale(mut self) -> Self {
        self.locale_c = false;
        self
    }

    /// Copies the output to icm's stderr as it arrives.
    pub fn mirrored(mut self, mirror: bool) -> Self {
        self.mirror = mirror;
        self
    }

    /// The program name for display (`xcrun`, `cargo`).
    pub fn program_name(&self) -> String {
        Path::new(&self.program)
            .file_name()
            .unwrap_or(&self.program)
            .to_string_lossy()
            .into_owned()
    }

    /// argv with secrets redacted.
    pub fn display_argv(&self) -> Vec<String> {
        let mut argv = vec![self.program.to_string_lossy().into_owned()];
        argv.extend(
            self.args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned()),
        );
        redact_argv(&argv)
    }

    /// The command line, shell-quoted and redacted.
    pub fn display(&self) -> String {
        self.display_argv()
            .iter()
            .map(|arg| shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The environment delta with secret values redacted. Removals show as
    /// an empty value.
    pub fn display_env(&self) -> Vec<(String, String)> {
        self.env
            .iter()
            .map(|(key, value)| {
                let key = key.to_string_lossy().into_owned();
                let value = match value {
                    None => String::new(),
                    Some(_) if is_secret_name(&key) => REDACTED.to_string(),
                    Some(value) => redact_values(&value.to_string_lossy()),
                };
                (key, value)
            })
            .collect()
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        let _ = command.args(&self.args).stdin(Stdio::null());

        let _ = command
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("RUSTUP_AUTO_INSTALL", "0")
            .env("CARGO_TERM_COLOR", "never");
        if std::env::var_os("GIT_SSH_COMMAND").is_none() {
            let _ = command.env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes");
        }
        if self.locale_c {
            let _ = command.env("LC_ALL", "C");
        }

        for (key, value) in &self.env {
            let _ = match value {
                Some(value) => command.env(key, value),
                None => command.env_remove(key),
            };
        }

        if let Some(cwd) = &self.cwd {
            let _ = command.current_dir(cwd);
        }

        command
    }
}

/// The path for an external tool: `ICM_TOOL_<NAME>` (upper case, `-` as
/// `_`) when set, else the name itself.
pub fn tool_path(name: &str) -> OsString {
    let var = format!(
        "ICM_TOOL_{}",
        name.to_ascii_uppercase().replace(['-', '.'], "_")
    );
    std::env::var_os(&var)
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| OsString::from(name))
}

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// The process exited with a code.
    Exited(i32),
    /// The process was killed by a signal it did not get from icm.
    Signaled(i32),
    /// icm killed it at its time limit.
    TimedOut(Duration),
    /// icm received a signal and killed it.
    Interrupted(i32),
}

/// The result of a run.
#[derive(Clone, Debug)]
pub struct Outcome {
    /// How it ended.
    pub end: End,
    /// Captured stdout (empty when stdout went to the log), not redacted
    /// (see [`run`]).
    pub stdout: Vec<u8>,
    /// stderr (and stdout when not captured separately).
    pub stderr: Vec<u8>,
    /// How long it ran.
    pub duration: Duration,
    /// The step log, if one was written.
    pub log: Option<PathBuf>,
    /// The file stdout was captured into, if it was kept.
    pub stdout_path: Option<PathBuf>,
}

impl Outcome {
    /// Whether it exited 0.
    pub fn success(&self) -> bool {
        self.end == End::Exited(0)
    }

    /// The exit code, if it exited.
    pub fn code(&self) -> Option<i32> {
        match self.end {
            End::Exited(code) => Some(code),
            _ => None,
        }
    }

    /// stdout as text.
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// stderr as text.
    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }

    /// The last `lines` lines of stderr, for error details.
    pub fn stderr_tail(&self, lines: usize) -> String {
        let text = self.stderr_text();
        let all: Vec<&str> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        all[all.len().saturating_sub(lines)..].join("\n")
    }

    /// A one-line description: `exit 1`, `timed out after 30s`, ...
    pub fn describe(&self) -> String {
        match self.end {
            End::Exited(code) => format!("exit {code}"),
            End::Signaled(signal) => format!("killed by {}", signals::name(signal)),
            End::TimedOut(limit) => format!("timed out after {}", format_duration(limit)),
            End::Interrupted(signal) => format!("interrupted by {}", signals::name(signal)),
        }
    }
}

/// Runs a command to completion (or its time limit, or a signal).
///
/// With `log`, the log gets a header (argv, cwd, redacted env delta), the
/// child's stderr (and stdout unless captured) live, and a footer; captured
/// stdout is kept next to it as `<log stem>.stdout`. `on_stdout_line` sees
/// each stdout line as it arrives.
///
/// The log files and `Outcome::stderr` are redacted. Captured stdout and
/// the lines `on_stdout_line` sees are not: they are data for icm's parsers
/// (a cargo artifact path must stay a path). Whatever of them icm reports
/// is redacted by the reporter ([`redact_values`], [`secret_values`]).
pub fn run(
    cmd: &Cmd,
    log: Option<&Path>,
    mut on_stdout_line: Option<&mut dyn FnMut(&str)>,
) -> io::Result<Outcome> {
    let started = Instant::now();

    let (stderr_file, log_offset) = match log {
        Some(log) => {
            if let Some(parent) = log.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = File::create(log)?;
            write_header(&mut file, cmd)?;
            drop(file);
            let file = OpenOptions::new().read(true).append(true).open(log)?;
            let offset = file.metadata()?.len();
            (file, offset)
        }
        None => (anonymous_file("stderr")?, 0),
    };

    let (stdout_file, stdout_path) = if cmd.capture_stdout {
        match log {
            Some(log) => {
                let path = log.with_extension("stdout");
                drop(File::create(&path)?);
                let file = OpenOptions::new().read(true).append(true).open(&path)?;
                (file, Some(path))
            }
            None => (anonymous_file("stdout")?, None),
        }
    } else {
        (stderr_file.try_clone()?, None)
    };

    let mut command = cmd.command();
    let _ = command
        .stdout(Stdio::from(stdout_file.try_clone()?))
        .stderr(Stdio::from(stderr_file.try_clone()?))
        .process_group(0);

    let mut child = command.spawn().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot start `{}`: {error}", cmd.program.to_string_lossy()),
        )
    })?;

    let pid = child.id() as i32;
    signals::register_group(pid);

    let deadline = cmd.timeout.map(|timeout| started + timeout);
    let secrets = secrets_for(cmd);
    let mut tail = Tail::default();
    let mut mirror_offsets = (0, log_offset);
    let mut poll = Duration::from_millis(5);

    let end = loop {
        if let Some(signal) = signals::pending() {
            kill_tree(&mut child, pid, cmd.kill_grace);
            break End::Interrupted(signal);
        }

        if let Some(status) = child.try_wait()? {
            // A child that died because icm got a signal (the watchdog
            // may have stopped its group) was interrupted, not failed.
            if let Some(signal) = signals::pending() {
                signals::kill_group(pid, libc::SIGKILL);
                break End::Interrupted(signal);
            }
            break match (status.code(), status.signal()) {
                (Some(code), _) => End::Exited(code),
                (None, Some(signal)) => End::Signaled(signal),
                (None, None) => End::Exited(-1),
            };
        }

        if let Some(callback) = on_stdout_line.as_deref_mut()
            && cmd.capture_stdout
        {
            tail.drain(&stdout_file, callback, false);
        }
        if cmd.mirror {
            mirror_offsets = mirror_both(&stdout_file, &stderr_file, cmd, mirror_offsets, &secrets);
        }

        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            kill_tree(&mut child, pid, cmd.kill_grace);
            break End::TimedOut(cmd.timeout.unwrap_or_default());
        }

        std::thread::sleep(poll);
        poll = (poll * 2).min(Duration::from_millis(50));
    };

    signals::unregister_group(pid);

    if let Some(callback) = on_stdout_line
        && cmd.capture_stdout
    {
        tail.drain(&stdout_file, callback, true);
    }
    if cmd.mirror {
        let _ = mirror_both(&stdout_file, &stderr_file, cmd, mirror_offsets, &secrets);
    }

    let duration = started.elapsed();
    let stdout = if cmd.capture_stdout {
        read_from(&stdout_file, 0)?
    } else {
        Vec::new()
    };
    let stderr = redact_bytes(read_from(&stderr_file, log_offset)?, &secrets);

    let outcome = Outcome {
        end,
        stdout,
        stderr,
        duration,
        log: log.map(Path::to_path_buf),
        stdout_path,
    };

    if let Some(log) = log {
        let mut file = &stderr_file;
        let _ = writeln!(
            file,
            "\n--- {} after {}",
            outcome.describe(),
            format_duration(duration)
        );
        // A tool that echoed a secret it was given must not leave it in
        // the run directory.
        redact_file(log, &secrets);
        if let Some(path) = &outcome.stdout_path {
            redact_file(path, &secrets);
        }
    }

    Ok(outcome)
}

/// The secret values a command's output must not keep: the values of
/// secret-named variables it was given (remembered from now on, so the
/// reporter redacts them too) and those in icm's environment.
fn secrets_for(cmd: &Cmd) -> Vec<String> {
    remember_secrets(cmd);
    secret_values()
}

/// [`redact_text`] for bytes: bytes that are not UTF-8 stay as they are
/// unless the text holds a secret.
fn redact_bytes(bytes: Vec<u8>, secrets: &[String]) -> Vec<u8> {
    if secrets.is_empty() {
        return bytes;
    }
    let text = String::from_utf8_lossy(&bytes);
    match redact_text(&text, secrets) {
        Cow::Borrowed(_) => bytes,
        Cow::Owned(text) => text.into_bytes(),
    }
}

/// Replaces each of `secrets` (longest first) in a text.
pub fn redact_with(text: &str, secrets: &[String]) -> String {
    let mut text = text.to_string();
    for secret in secrets {
        if text.contains(secret.as_str()) {
            text = text.replace(secret.as_str(), REDACTED);
        }
    }
    text
}

/// Replaces each of `secrets` in the text of a file icm keeps: everywhere
/// as text (in each form [`secret_values`] holds: raw, JSON-escaped,
/// percent-encoded), then, in a line that is a JSON document with escapes
/// (an NDJSON record, a line of `log show --style ndjson`), in its decoded
/// strings, whatever escapes its encoder used. Such a line is written again
/// as compact JSON.
pub fn redact_text<'a>(text: &'a str, secrets: &[String]) -> Cow<'a, str> {
    if secrets.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut text = if secrets.iter().any(|secret| text.contains(secret.as_str())) {
        Cow::Owned(redact_with(text, secrets))
    } else {
        Cow::Borrowed(text)
    };
    if text.contains('\\') {
        let mut out = String::with_capacity(text.len());
        let mut changed = false;
        for line in text.split_inclusive('\n') {
            let body = line.trim_end_matches(['\n', '\r']);
            match redact_json_line(body, secrets) {
                Some(json) => {
                    out.push_str(&json);
                    out.push_str(&line[body.len()..]);
                    changed = true;
                }
                None => out.push_str(line),
            }
        }
        if changed {
            text = Cow::Owned(out);
        }
    }
    text
}

/// A JSON document with escapes (a line of a file, or a string holding
/// one), redacted; `None` when it is not one or holds no secret.
fn redact_json_line(line: &str, secrets: &[String]) -> Option<String> {
    let line = line.trim();
    if !line.contains('\\') || !(line.starts_with('{') || line.starts_with('[')) {
        return None;
    }
    let mut value: Value = serde_json::from_str(line).ok()?;
    redact_json(&mut value, secrets).then(|| value.to_string())
}

/// Replaces each of `secrets` in every string of a JSON value, and says
/// whether it found one. A string that is itself a JSON document with
/// escapes (a raw `log` line that `icm logs ios-sim --raw` reports) is
/// redacted in its decoded strings too, and written again as compact JSON.
pub fn redact_json(value: &mut Value, secrets: &[String]) -> bool {
    match value {
        Value::String(text) => {
            let mut found = secrets.iter().any(|secret| text.contains(secret.as_str()));
            if found {
                *text = redact_with(text, secrets);
            }
            if let Some(json) = redact_json_line(text, secrets) {
                *text = json;
                found = true;
            }
            found
        }
        Value::Array(items) => {
            let mut found = false;
            for item in items {
                found |= redact_json(item, secrets);
            }
            found
        }
        Value::Object(map) => {
            let mut found = false;
            for item in map.values_mut() {
                found |= redact_json(item, secrets);
            }
            found
        }
        _ => false,
    }
}

/// Writes a text file icm keeps for reading later (a log, a copy of a
/// device's output) with the secret values icm knows ([`secret_values`])
/// replaced ([`redact_text`]), as the reporter replaces them in every event
/// and result: a file in the run directory must not keep what stdout hides.
pub fn write_redacted(path: &Path, text: &str) -> io::Result<()> {
    std::fs::write(path, redact_text(text, &secret_values()).as_bytes())
}

/// Copies a file into the run directory as [`write_redacted`] writes one:
/// the copy of an app's live output, a crash report. A file that is not
/// UTF-8 is copied byte for byte unless its text holds a secret.
pub fn copy_redacted(from: &Path, to: &Path) -> io::Result<()> {
    let bytes = std::fs::read(from)?;
    std::fs::write(to, redact_bytes(bytes, &secret_values()))
}

/// Replaces the secret values icm knows in a file that a tool or a browser
/// wrote into the run directory itself (devicectl's `--json-output`,
/// Chrome's log).
pub fn redact_in_place(path: &Path) {
    redact_file(path, &secret_values());
}

fn redact_file(path: &Path, secrets: &[String]) {
    if secrets.is_empty() {
        return;
    }
    if let Ok(bytes) = std::fs::read(path) {
        let redacted = redact_bytes(bytes.clone(), secrets);
        if redacted != bytes {
            let _ = std::fs::write(path, redacted);
        }
    }
}

/// Starts a long-lived process in its own session (setsid), with stdin on
/// `/dev/null` and its output appended to files. Returns its pid. icm does
/// not wait for it; `icm stop` or the caller ends it.
///
/// The files are the process's own output, unredacted; the secret-named
/// values it is given are remembered ([`secret_values`]), so the copies icm
/// keeps of that output ([`copy_redacted`]) and the reporter leave them out.
pub fn spawn_detached(cmd: &Cmd, stdout: &Path, stderr: &Path) -> io::Result<u32> {
    remember_secrets(cmd);
    for path in [stdout, stderr] {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let out = OpenOptions::new().create(true).append(true).open(stdout)?;
    let err = if stdout == stderr {
        out.try_clone()?
    } else {
        OpenOptions::new().create(true).append(true).open(stderr)?
    };

    let mut command = cmd.command();
    let _ = command.stdout(Stdio::from(out)).stderr(Stdio::from(err));

    // SAFETY: setsid is async-signal-safe and touches no Rust state.
    unsafe {
        let _ = command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = command.spawn().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot start `{}`: {error}", cmd.program.to_string_lossy()),
        )
    })?;

    Ok(child.id())
}

/// SIGTERM to the group, a grace period for the leader, then SIGKILL to the
/// group (stragglers included).
fn kill_tree(child: &mut std::process::Child, pgid: i32, grace: Duration) {
    signals::kill_group(pgid, libc::SIGTERM);

    let until = Instant::now() + grace;
    while Instant::now() < until {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    signals::kill_group(pgid, libc::SIGKILL);
    let _ = child.wait();
}

fn write_header(file: &mut File, cmd: &Cmd) -> io::Result<()> {
    writeln!(file, "$ {}", cmd.display())?;
    let cwd = match &cmd.cwd {
        Some(cwd) => cwd.display().to_string(),
        None => std::env::current_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default(),
    };
    writeln!(file, "# cwd: {cwd}")?;
    for (key, value) in cmd.display_env() {
        writeln!(file, "# env: {key}={value}")?;
    }
    writeln!(
        file,
        "# hygiene: stdin=/dev/null GIT_TERMINAL_PROMPT=0 RUSTUP_AUTO_INSTALL=0 CARGO_TERM_COLOR=never{}",
        if cmd.locale_c { " LC_ALL=C" } else { "" }
    )?;
    if let Some(timeout) = cmd.timeout {
        writeln!(file, "# timeout: {}", format_duration(timeout))?;
    }
    writeln!(
        file,
        "# started: {}",
        Utc::from_system(SystemTime::now()).rfc3339()
    )?;
    writeln!(file, "---")?;
    Ok(())
}

/// A file nobody else can see: created in the temp dir and unlinked at once.
fn anonymous_file(label: &str) -> io::Result<File> {
    let dir = std::env::temp_dir();
    for attempt in 0..100u32 {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default();
        let path = dir.join(format!(
            ".icm-{label}-{}-{nanos}-{attempt}",
            std::process::id()
        ));
        match OpenOptions::new()
            .create_new(true)
            .read(true)
            .append(true)
            .open(&path)
        {
            Ok(file) => {
                let _ = std::fs::remove_file(&path);
                return Ok(file);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("cannot create a temporary file"))
}

/// Mirrors captured stdout (when it is separate) and the log; returns the
/// new offsets.
fn mirror_both(
    stdout: &File,
    stderr: &File,
    cmd: &Cmd,
    (out, err): (u64, u64),
    secrets: &[String],
) -> (u64, u64) {
    let out = if cmd.capture_stdout {
        mirror(stdout, out, secrets)
    } else {
        out
    };
    (out, mirror(stderr, err, secrets))
}

/// Copies what a file gained since `offset` to icm's stderr (`-v`),
/// secrets redacted.
fn mirror(file: &File, offset: u64, secrets: &[String]) -> u64 {
    let mut buffer = vec![0u8; 64 * 1024];
    let mut position = offset;
    let mut stderr = io::stderr();
    while let Ok(read) = file.read_at(&mut buffer, position) {
        if read == 0 {
            break;
        }
        let _ = stderr.write_all(&redact_bytes(buffer[..read].to_vec(), secrets));
        position += read as u64;
    }
    position
}

fn read_from(file: &File, offset: u64) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut position = offset;
    loop {
        let read = file.read_at(&mut buffer, position)?;
        if read == 0 {
            break;
        }
        out.extend_from_slice(&buffer[..read]);
        position += read as u64;
    }
    Ok(out)
}

#[derive(Default)]
struct Tail {
    offset: u64,
    partial: Vec<u8>,
}

impl Tail {
    fn drain(&mut self, file: &File, callback: &mut dyn FnMut(&str), last: bool) {
        let mut buffer = vec![0u8; 64 * 1024];
        while let Ok(read) = file.read_at(&mut buffer, self.offset) {
            if read == 0 {
                break;
            }
            self.offset += read as u64;
            self.partial.extend_from_slice(&buffer[..read]);
        }

        while let Some(newline) = self.partial.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=newline).collect();
            let text = String::from_utf8_lossy(&line[..line.len() - 1]);
            callback(text.trim_end_matches('\r'));
        }

        if last && !self.partial.is_empty() {
            let text = String::from_utf8_lossy(&self.partial).into_owned();
            self.partial.clear();
            callback(&text);
        }
    }
}

/// Whether an environment variable name holds a secret:
/// `*PASS*|*SECRET*|*TOKEN*|*KEY*|*PRIVATE*` (design §1 principle 5).
pub fn is_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["PASS", "SECRET", "TOKEN", "KEY", "PRIVATE"]
        .iter()
        .any(|pattern| upper.contains(pattern))
}

/// Flags whose next argument is a password.
const PASSWORD_FLAGS: &[&str] = &[
    "-storepass",
    "-keypass",
    "-srcstorepass",
    "-deststorepass",
    "-srckeypass",
    "-destkeypass",
    "--ks-pass",
    "--key-pass",
    "--password",
    "-password",
    "--keystore-pass",
];

/// Android's debug keystore password, which Android publishes: redacting
/// it hides nothing and turns a printed keytool or apksigner command into
/// one that makes a keystore the build cannot open.
const PUBLIC_PASSWORD: &str = crate::android::DEBUG_KEYSTORE_PASS;

/// Redacts passwords in an argv: `pass:<x>` values, the argument after a
/// password flag (unless it is an `env:`/`file:` reference), `NAME=value`
/// for secret names, and the values of secret variables in icm's own
/// environment wherever they appear. Android's public debug keystore
/// password is left as it is.
pub fn redact_argv(argv: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len());
    let mut redact_next = false;

    for arg in argv {
        if redact_next {
            redact_next = false;
            if arg == PUBLIC_PASSWORD || arg.strip_prefix("pass:") == Some(PUBLIC_PASSWORD) {
                out.push(arg.clone());
                continue;
            }
            if arg.starts_with("pass:") {
                out.push(format!("pass:{REDACTED}"));
                continue;
            }
            if !(arg.starts_with("env:") || arg.starts_with("file:")) {
                out.push(REDACTED.to_string());
                continue;
            }
        }

        if PASSWORD_FLAGS.contains(&arg.as_str()) {
            redact_next = true;
            out.push(arg.clone());
            continue;
        }

        if let Some(_password) = arg.strip_prefix("pass:") {
            out.push(format!("pass:{REDACTED}"));
            continue;
        }

        if let Some((name, _)) = arg.split_once('=')
            && !name.is_empty()
            && !name.starts_with('-')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && is_secret_name(name)
        {
            out.push(format!("{name}={REDACTED}"));
            continue;
        }

        out.push(redact_values(arg));
    }

    out
}

/// Replaces the secret values icm knows ([`secret_values`]).
pub fn redact_values(text: &str) -> String {
    redact_with(text, &secret_values())
}

/// The secret values icm knows, longest first (so a secret containing
/// another is replaced whole): those of secret-named variables in its own
/// environment (at least 6 bytes, not a path), and those it has handed to
/// a child under a secret name (at least 4 bytes). A multi-line value also
/// counts line by line, since output is read and reported by the line.
/// Each also counts JSON-escaped, once and twice (a JSON line inside a JSON
/// record), as serde and JavaScript write it and as Apple's `log` writes it
/// (`/` as `\/`), since a line of an app's output or a tool's NDJSON can
/// carry it that way, and percent-encoded, as a URL carries it
/// ([`url_encoded`]).
pub fn secret_values() -> Vec<String> {
    secret_registry()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

fn secret_registry() -> &'static Mutex<Vec<String>> {
    static SECRETS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    SECRETS.get_or_init(|| {
        let mut values = Vec::new();
        for (name, value) in std::env::vars_os() {
            let value = value.to_string_lossy();
            if is_secret_name(&name.to_string_lossy())
                && !value.starts_with('/')
                && !value.starts_with('~')
            {
                add_secret(&mut values, &value, 6);
            }
        }
        Mutex::new(values)
    })
}

/// Remembers the secret-named values a command is given.
fn remember_secrets(cmd: &Cmd) {
    let given: Vec<String> = cmd
        .env
        .iter()
        .filter(|(key, _)| is_secret_name(&key.to_string_lossy()))
        .filter_map(|(_, value)| value.as_ref())
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    if given.is_empty() {
        return;
    }
    let mut values = secret_registry()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    for value in &given {
        add_secret(&mut values, value, 4);
    }
}

/// Remembers a value icm hands to an app some other way than its
/// environment (the web page's query) as a secret, when its name is a
/// secret's (at least 4 bytes, as for a child's environment).
pub fn remember_secret(name: &str, value: &str) {
    if is_secret_name(name) {
        let mut values = secret_registry()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        add_secret(&mut values, value, 4);
    }
}

/// Adds a secret (and the lines of a multi-line one) at least `min` bytes
/// long, with their JSON-escaped and percent-encoded forms, keeping the
/// list longest first.
fn add_secret(values: &mut Vec<String>, value: &str, min: usize) {
    let lines = value
        .lines()
        .map(str::trim)
        .filter(|_| value.contains('\n'));
    for text in std::iter::once(value).chain(lines) {
        if text.len() < min {
            continue;
        }
        // Escaped once (a JSON string) and twice (a JSON line inside a JSON
        // record, a raw `log` line in a log), each also with `\/`.
        let once = json_escaped(text);
        let twice = json_escaped(&once);
        let forms = [
            text.to_string(),
            once.replace('/', "\\/"),
            twice.replace('/', "\\/"),
            once,
            twice,
            url_encoded(text),
        ];
        for form in forms {
            if !values.contains(&form) {
                values.push(form);
            }
        }
    }
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
}

/// A text as it stands inside a JSON string (serde's escapes).
fn json_escaped(text: &str) -> String {
    let quoted = Value::String(text.to_string()).to_string();
    quoted[1..quoted.len() - 1].to_string()
}

/// A text percent-encoded as a URL's query component (every byte but
/// letters, digits and `-_.~,`), as icm writes the web page's query: a
/// secret in a URL a result, a tool or an app's log names is in this form.
pub fn url_encoded(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~,".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Quotes an argument for display in a POSIX shell.
pub fn shell_quote(arg: &str) -> String {
    if !arg.is_empty()
        && arg.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | ',' | '+' | '@' | '%')
        })
    {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Cmd {
        Cmd::new("/bin/sh").arg("-c").arg(script)
    }

    #[test]
    fn captures_stdout_and_stderr_separately() {
        let outcome = run(&sh("echo out; echo err >&2; exit 3"), None, None).unwrap();
        assert_eq!(outcome.end, End::Exited(3));
        assert_eq!(outcome.stdout_text(), "out\n");
        assert_eq!(outcome.stderr_text(), "err\n");
        assert!(!outcome.success());
    }

    #[test]
    fn stdin_is_closed() {
        // `cat` would wait forever on an open stdin.
        let outcome = run(
            &Cmd::new("cat").timeout(Duration::from_secs(10)),
            None,
            None,
        )
        .unwrap();
        assert!(outcome.success(), "{outcome:?}");
        assert!(outcome.duration < Duration::from_secs(5));
    }

    #[test]
    fn hygiene_variables_are_set() {
        let outcome = run(
            &sh("echo \"$GIT_TERMINAL_PROMPT $RUSTUP_AUTO_INSTALL $LC_ALL $CARGO_TERM_COLOR\""),
            None,
            None,
        )
        .unwrap();
        assert_eq!(outcome.stdout_text().trim(), "0 0 C never");

        let kept = run(
            &sh("echo \"${LC_ALL:-unset}\"")
                .keep_locale()
                .env_remove("LC_ALL"),
            None,
            None,
        )
        .unwrap();
        assert_eq!(kept.stdout_text().trim(), "unset");
    }

    #[test]
    fn timeout_kills_the_whole_group() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("grandchild.pid");
        let script = format!("sleep 30 & echo $! > {}; wait", pidfile.display());
        let started = Instant::now();
        let outcome = run(&sh(&script).timeout(Duration::from_millis(500)), None, None).unwrap();
        assert!(matches!(outcome.end, End::TimedOut(_)), "{outcome:?}");
        assert!(started.elapsed() < Duration::from_secs(10));

        let grandchild: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // The grandchild was in the group, so it is gone (or a zombie being reaped).
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", &grandchild.to_string()])
                .output()
                .unwrap();
            let stat = String::from_utf8_lossy(&state.stdout).trim().to_string();
            if stat.is_empty() || stat.starts_with('Z') {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "grandchild {grandchild} survived: {stat}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn a_daemon_holding_stdout_does_not_hang_the_runner() {
        // The background job keeps the output file open after `sh` exits,
        // like the adb server does. With pipes this would block.
        let started = Instant::now();
        let outcome = run(
            &sh("(sleep 5 >/dev/null 2>&1 &) ; (sleep 5 &) ; echo done"),
            None,
            None,
        )
        .unwrap();
        assert!(outcome.success());
        assert_eq!(outcome.stdout_text().trim(), "done");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn logs_have_a_redacted_header_and_live_output() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("steps").join("01-demo.log");
        let cmd = sh("echo to-stdout; echo to-stderr >&2")
            .env("ICM_ANDROID_STORE_PASS", "hunter22")
            .env("PLAIN", "visible");
        let mut lines = Vec::new();
        let mut callback = |line: &str| lines.push(line.to_string());
        let outcome = run(&cmd, Some(&log), Some(&mut callback)).unwrap();
        assert!(outcome.success());
        assert_eq!(lines, vec!["to-stdout".to_string()]);

        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.starts_with("$ /bin/sh -c "), "{text}");
        assert!(
            text.contains("# env: ICM_ANDROID_STORE_PASS=<redacted>"),
            "{text}"
        );
        assert!(text.contains("# env: PLAIN=visible"), "{text}");
        assert!(!text.contains("hunter22"), "{text}");
        assert!(text.contains("to-stderr"), "{text}");
        assert!(text.contains("--- exit 0 after"), "{text}");
        assert_eq!(outcome.stderr_text(), "to-stderr\n");

        let stdout = std::fs::read_to_string(log.with_extension("stdout")).unwrap();
        assert_eq!(stdout, "to-stdout\n");
    }

    #[test]
    fn stdout_can_share_the_log() {
        let outcome = run(&sh("echo a; echo b >&2").stdout_to_log(), None, None).unwrap();
        assert!(outcome.stdout.is_empty());
        let text = outcome.stderr_text();
        assert!(text.contains('a') && text.contains('b'), "{text}");
    }

    #[test]
    fn missing_programs_are_reported_by_name() {
        let error = run(&Cmd::new("/nonexistent/icm-tool"), None, None).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(error.to_string().contains("/nonexistent/icm-tool"));
    }

    #[test]
    fn argv_redaction() {
        let argv: Vec<String> = [
            "apksigner",
            "sign",
            "--ks-pass",
            "pass:hunter22",
            "--key-pass",
            "env:ICM_ANDROID_KEY_PASS",
            "keytool",
            "-storepass",
            "s3cret",
            "-keypass:env",
            "NAME",
            "ICM_TOKEN=abc",
            "--out=x",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        let redacted = redact_argv(&argv);
        assert_eq!(redacted[3], "pass:<redacted>");
        assert_eq!(redacted[5], "env:ICM_ANDROID_KEY_PASS");
        assert_eq!(redacted[8], "<redacted>");
        assert_eq!(redacted[10], "NAME");
        assert_eq!(redacted[11], "ICM_TOKEN=<redacted>");
        assert_eq!(redacted[12], "--out=x");

        // The debug keystore's public password stays readable.
        let debug: Vec<String> = [
            "keytool",
            "-storepass",
            "android",
            "--ks-pass",
            "pass:android",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(redact_argv(&debug), debug);
    }

    #[test]
    fn secret_names() {
        for name in [
            "ICM_ANDROID_STORE_PASS",
            "ASC_PRIVATE_KEY",
            "github_token",
            "AWS_SECRET",
        ] {
            assert!(is_secret_name(name), "{name}");
        }
        for name in ["PATH", "JAVA_HOME", "ANDROID_HOME", "ICM_RUN_ID"] {
            assert!(!is_secret_name(name), "{name}");
        }
    }

    #[test]
    fn secrets_handed_to_a_child_are_redacted_everywhere_after() {
        let echo = "echo \"$ICM_UNIT_KEY_PASS\"; echo \"$ICM_UNIT_KEY_PASS\" >&2";
        let outcome = run(
            &sh(echo).env("ICM_UNIT_KEY_PASS", "child-only-pw"),
            None,
            None,
        )
        .unwrap();
        // stderr is redacted; captured stdout stays raw for parsers.
        assert_eq!(outcome.stderr_text(), "<redacted>\n");
        assert_eq!(outcome.stdout_text(), "child-only-pw\n");
        // The reporter's redaction knows it from now on.
        assert!(secret_values().iter().any(|s| s == "child-only-pw"));
        assert_eq!(
            redact_values("detail: child-only-pw!"),
            "detail: <redacted>!"
        );
        // So do the files icm keeps.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        write_redacted(&log, "1 I app: child-only-pw\n2 I app: fine\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "1 I app: <redacted>\n2 I app: fine\n"
        );
    }

    #[test]
    fn multi_line_secrets_count_line_by_line() {
        let mut values = Vec::new();
        add_secret(
            &mut values,
            "-----BEGIN KEY-----\nAAAABBBBCCCC\r\nab\n-----END KEY-----",
            6,
        );
        // The value, escaped once and twice and percent-encoded, and its
        // three long lines (two of them percent-encoded too).
        assert_eq!(values.len(), 9, "{values:?}");
        assert!(values.contains(&"-----BEGIN%20KEY-----".to_string()));
        assert!(values.iter().any(|value| value.contains('\n')));
        assert!(values.contains(
            &"-----BEGIN KEY-----\\nAAAABBBBCCCC\\r\\nab\\n-----END KEY-----".to_string()
        ));
        assert!(values.contains(&"AAAABBBBCCCC".to_string()));
        assert!(!values.contains(&"ab".to_string()));
        assert_eq!(
            redact_with("line: AAAABBBBCCCC", &values),
            "line: <redacted>"
        );

        // Longest first: a secret containing another is replaced whole.
        let mut values = Vec::new();
        add_secret(&mut values, "abcdef", 6);
        add_secret(&mut values, "abcdefghij", 6);
        add_secret(&mut values, "abcdef", 6);
        assert_eq!(values, ["abcdefghij", "abcdef"]);
        assert_eq!(redact_with("abcdefghij", &values), "<redacted>");
    }

    /// A secret that JSON escapes is found raw, escaped as serde and
    /// JavaScript escape it, with `/` as `\/` (Apple's `log`), in a URL, and
    /// in a JSON line under any other escape; other lines and bytes stay as
    /// they are.
    #[test]
    fn escaped_secrets_are_redacted_in_files() {
        let mut secrets = Vec::new();
        add_secret(&mut secrets, "tok/se\"kr\\it-123456", 6);
        assert_eq!(secrets.len(), 6, "{secrets:?}");
        assert!(secrets.contains(&"tok\\/se\\\\\\\"kr\\\\\\\\it-123456".to_string()));
        let text = concat!(
            "raw: tok/se\"kr\\it-123456\n",
            "ICM_EVENT {\"msg\":\"tok/se\\\"kr\\\\it-123456\"}\n",
            "{\"eventMessage\":\"a tok\\/se\\\"kr\\\\it-123456 b\",\"n\":1}\n",
            "{\"msg\":\"tok\\u002fse\\u0022kr\\u005cit-123456\"}\r\n",
            "{\"msg\":\"fine \\/ here\"}\n",
            "GET /?api_token=tok%2Fse%22kr%5Cit-123456&x=1\n",
            "raw log line: {\"msg\":\"{\\\"t\\\":\\\"tok\\/se\\\\\\\"kr\\\\\\\\it-123456\\\"}\"}\n",
            "the end",
        );
        let redacted = redact_text(text, &secrets);
        assert_eq!(
            redacted,
            concat!(
                "raw: <redacted>\n",
                "ICM_EVENT {\"msg\":\"<redacted>\"}\n",
                "{\"eventMessage\":\"a <redacted> b\",\"n\":1}\n",
                "{\"msg\":\"<redacted>\"}\r\n",
                "{\"msg\":\"fine \\/ here\"}\n",
                "GET /?api_token=<redacted>&x=1\n",
                "raw log line: {\"msg\":\"{\\\"t\\\":\\\"<redacted>\\\"}\"}\n",
                "the end",
            )
        );
        assert!(matches!(
            redact_text("nothing here \\/", &secrets),
            Cow::Borrowed(_)
        ));

        // A record whose string is a raw `log` line (`logs ios-sim --raw`)
        // of a message in which the app wrote the secret as JSON: escaped
        // three times in the file.
        let app = format!(
            "{{\"token\":{}}}",
            Value::String("tok/se\"kr\\it-123456".into())
        );
        let line = serde_json::json!({ "eventMessage": app })
            .to_string()
            .replace('/', "\\/");
        let record = format!("{}\n", serde_json::json!({ "msg": line }));
        let redacted = redact_text(&record, &secrets);
        assert!(!redacted.contains("it-123456"), "{redacted}");
        let msg: Value = serde_json::from_str(&redacted).unwrap();
        let line: Value = serde_json::from_str(msg["msg"].as_str().unwrap()).unwrap();
        assert_eq!(line["eventMessage"], "{\"token\":\"<redacted>\"}");

        let bytes = b"\xff\xfe binary \x00".to_vec();
        assert_eq!(redact_bytes(bytes.clone(), &secrets), bytes);
        let mut with = bytes.clone();
        with.extend_from_slice(b" tok/se\"kr\\it-123456");
        assert!(String::from_utf8_lossy(&redact_bytes(with, &secrets)).ends_with(" <redacted>"));
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("plain-arg"), "plain-arg");
        assert_eq!(shell_quote("two words"), "'two words'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn tool_overrides() {
        // An unset override falls back to the name.
        assert_eq!(
            tool_path("icm-test-no-such-tool"),
            OsString::from("icm-test-no-such-tool")
        );
    }

    #[test]
    fn detached_processes_get_their_own_session() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.log");
        let pid = spawn_detached(
            &sh("echo started; ps -o sess= -p $$ >/dev/null; exit 0"),
            &out,
            &out,
        )
        .unwrap();
        assert!(pid > 0);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !std::fs::read_to_string(&out)
            .unwrap_or_default()
            .contains("started")
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
