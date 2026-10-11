//! The desktop log collector (`icm __session desktop`, hidden): the process
//! that reads an app's stdout and stderr from the pipes the launcher gives
//! it, and writes the live files `app.stdout` and `app.stderr` with the
//! secret values icm knows redacted. What the app writes never reaches
//! those files raw, so a value it inherits from icm's environment does not
//! reach the disk.
//!
//! The launcher starts the helper with the read ends of the app's two pipes
//! on file descriptors 3 (stdout) and 4 (stderr), with the app's environment
//! (so the helper's registry holds the values the app inherits and is
//! given), and with stdin, stdout and stderr on `/dev/null`. Its argv
//! carries names only: `--handed NAME` for a secret-named value the launcher
//! gave the app explicitly, whose value the helper reads from its own
//! environment ([`argv`]).
//!
//! The helper must never hold the app up. It drains both pipes until both
//! reach end of file, or until the app is gone (`procid::check` says `Gone`
//! or `Other`) and nothing has come from its pipes for [`GONE_QUIET`]. A
//! write that fails (a full disk, a directory that is gone) is counted and
//! dropped, and draining goes on. It prints nothing: its only output is the
//! usage error of a bad argv, which goes nowhere.
//!
//! A complete line is redacted as [`process::redact_bytes`] redacts a file:
//! every form of every secret, and the JSON strings of a line that has
//! escapes. An `ICM_EVENT <json>` line keeps its JSON valid
//! ([`redact_line`]). A partial line waits for its newline. When it has
//! waited [`IDLE`], or grown past [`PARTIAL_LIMIT`], its text is redacted and
//! written up to its last [`holdback`] bytes, which stay pending: the rest of
//! a secret may not have arrived yet. At end of file the partial line is
//! written whole.

use crate::catalogue::CheckId;
use crate::error::{IcmError, Result};
use crate::process;
use crate::procid::{self, Identity, Verdict};
use serde_json::Value;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The read end of the app's stdout pipe, as the launcher leaves it.
const STDOUT_FD: RawFd = 3;
/// The read end of the app's stderr pipe.
const STDERR_FD: RawFd = 4;
/// The live files in the session directory.
const STDOUT_FILE: &str = "app.stdout";
const STDERR_FILE: &str = "app.stderr";

/// How much one read takes from a pipe.
const CHUNK: usize = 64 * 1024;
/// How long one wait for the pipes lasts before the helper checks the app
/// and the clock again.
const TICK: Duration = Duration::from_millis(100);
/// How long a partial line may wait for its newline before what it can
/// spare is written.
const IDLE: Duration = Duration::from_millis(500);
/// How long the app must be gone, with nothing read from its pipes, before
/// the helper stops.
const GONE_QUIET: Duration = Duration::from_millis(300);
/// The length a partial line may reach before its head is written.
const PARTIAL_LIMIT: usize = 64 * 1024;

/// The usage of the hidden subcommand.
const USAGE: &str = "usage: icm __session desktop --app-pid N --app-identity <json> --files <dir> [--handed NAME]... (internal; started by `icm run desktop`)";

/// The helper's argv, after the program's name: what the launcher starts it
/// with. It carries the names of the secret-named values the app was handed
/// explicitly, never their values.
#[allow(dead_code)] // the launcher that starts the helper comes in a later change
pub fn argv(app_pid: i32, identity: &Identity, files: &Path, handed: &[String]) -> Vec<OsString> {
    let identity = serde_json::to_string(identity).unwrap_or_default();
    let mut args = vec![
        OsString::from("__session"),
        OsString::from("desktop"),
        OsString::from("--app-pid"),
        OsString::from(app_pid.to_string()),
        OsString::from("--app-identity"),
        OsString::from(identity),
        OsString::from("--files"),
        files.as_os_str().to_os_string(),
    ];
    for name in handed {
        args.push(OsString::from("--handed"));
        args.push(OsString::from(name));
    }
    args
}

/// What the launcher passes the helper.
#[derive(Debug, PartialEq)]
struct Args {
    app_pid: i32,
    identity: Identity,
    files: PathBuf,
    handed: Vec<String>,
}

fn parse(args: &[String]) -> Option<Args> {
    let (mut app_pid, mut identity, mut files) = (None, None, None);
    let mut handed = Vec::new();
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let value = rest.next()?;
        match flag.as_str() {
            "--app-pid" => app_pid = value.parse().ok(),
            "--app-identity" => identity = serde_json::from_str(value).ok(),
            "--files" => files = Some(PathBuf::from(value)),
            "--handed" => handed.push(value.clone()),
            _ => return None,
        }
    }
    Some(Args {
        app_pid: app_pid?,
        identity: identity?,
        files: files?,
        handed,
    })
}

/// Runs the collector: the arguments are those after `desktop`. Draining
/// ends when [`drain`] does. The arguments are checked before anything is
/// opened.
pub fn main(args: &[String]) -> Result<()> {
    let args = parse(args).ok_or_else(|| IcmError::new(CheckId::UsageBadArgs, USAGE))?;
    let (Some(stdout), Some(stderr)) = (inherited(STDOUT_FD), inherited(STDERR_FD)) else {
        return Err(IcmError::new(
            CheckId::UsageBadArgs,
            "the log collector needs the app's pipes on file descriptors 3 and 4 (internal; started by `icm run desktop`)",
        ));
    };
    reset_signals();
    let secrets = secrets_for(&args.handed, |name| std::env::var_os(name));
    let outputs = [
        open_output(&args.files.join(STDOUT_FILE)),
        open_output(&args.files.join(STDERR_FILE)),
    ];
    let (pid, identity) = (args.app_pid, args.identity);
    let app_gone = move || {
        matches!(
            procid::check(pid, &identity),
            Verdict::Gone | Verdict::Other(_)
        )
    };
    let _ = drain([stdout, stderr], outputs, &secrets, app_gone);
    Ok(())
}

/// The pipe the launcher left on `fd`, or `None` when nothing is open there
/// (a helper started by hand has no pipes).
fn inherited(fd: RawFd) -> Option<File> {
    // SAFETY: F_GETFD reads the descriptor's flags and touches no memory.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
        return None;
    }
    // SAFETY: the launcher passed this descriptor to the helper alone, so
    // the File is its only owner, and it closes the descriptor once.
    Some(unsafe { File::from_raw_fd(fd) })
}

/// The helper ends when it is signalled, as any process does. icm's own
/// handlers, installed before the command ran, would only note the signal.
fn reset_signals() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: SIG_DFL installs no handler and touches no memory.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
        }
    }
}

/// The secret values the helper redacts with: the registry of its own
/// environment (the app's inherited values and its `--env` values), and the
/// values of the names the launcher handed the app. [`process::remember_secret`]
/// takes a handed value only when its name is a secret's, and at least 4
/// bytes long.
fn secrets_for(handed: &[String], value_of: impl Fn(&str) -> Option<OsString>) -> Vec<String> {
    for name in handed {
        if let Some(value) = value_of(name) {
            process::remember_secret(name, &value.to_string_lossy());
        }
    }
    process::secret_values()
}

/// The live file at `path`, created or appended to, never truncated. When it
/// cannot be opened, a writer that refuses everything: the helper counts
/// what it drops and keeps draining, so the app never blocks on a full pipe.
fn open_output(path: &Path) -> Box<dyn Write> {
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(file) => Box::new(file),
        Err(error) => Box::new(Refused(error.kind())),
    }
}

/// A writer that refuses every write, with the kind of the error that
/// refused it.
struct Refused(io::ErrorKind);

impl Write for Refused {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(self.0))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One of the app's output streams: the file it goes to, and the bytes not
/// written yet (a partial line, or the held-back end of one).
struct Stream<W> {
    out: W,
    /// Bytes not written yet: a partial line, or the held-back end of one.
    /// A complete line is written with its newline, so none is held here.
    pending: Vec<u8>,
    /// The secret forms, longest first.
    secrets: Vec<String>,
    /// When input last arrived.
    last_input: Instant,
    /// Whether the idle write has already happened for the current wait.
    settled: bool,
    /// Bytes that could not be written.
    dropped: u64,
}

impl<W: Write> Stream<W> {
    fn new(out: W, secrets: Vec<String>) -> Self {
        Stream {
            out,
            pending: Vec::new(),
            secrets,
            last_input: Instant::now(),
            settled: false,
            dropped: 0,
        }
    }

    /// Takes what the app wrote. Its complete lines are written as one
    /// batch, redacted; a partial line is written only once it passes
    /// [`PARTIAL_LIMIT`].
    fn push(&mut self, bytes: &[u8]) {
        self.last_input = Instant::now();
        self.settled = false;
        let mut pending = std::mem::take(&mut self.pending);
        pending.extend_from_slice(bytes);
        let mut lines = Vec::new();
        let mut start = 0;
        while let Some(at) = pending[start..].iter().position(|byte| *byte == b'\n') {
            let end = start + at + 1;
            lines.extend(redact_line(&pending[start..end], &self.secrets));
            start = end;
        }
        pending.drain(..start);
        self.pending = pending;
        self.emit(&lines);
        if self.pending.len() > PARTIAL_LIMIT {
            self.write_partial();
        }
    }

    /// Writes what the partial line can spare, once per wait: called when no
    /// input has come for [`IDLE`].
    fn idle(&mut self, now: Instant) {
        if !self.settled
            && !self.pending.is_empty()
            && now.saturating_duration_since(self.last_input) >= IDLE
        {
            self.write_partial();
            self.settled = true;
        }
    }

    /// The end of the stream: what is pending is written whole, redacted,
    /// since no more of it will come.
    fn finish(&mut self) {
        let rest = process::redact_bytes(std::mem::take(&mut self.pending), &self.secrets);
        self.emit(&rest);
    }

    /// Writes the pending bytes redacted, all but the [`holdback`] bytes at
    /// the end, which stay pending. Redaction runs over the whole pending
    /// text first, so no complete secret is split by the cut, and a secret
    /// that is still incomplete at the end is among the held bytes.
    ///
    /// A UTF-8 character cut short at the end is kept out of the redaction
    /// (which would replace its bytes), and the cut never falls inside a
    /// character.
    fn write_partial(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        let whole = incomplete_utf8_start(&pending);
        let (body, tail) = pending.split_at(whole);
        let mut text = process::redact_bytes(body.to_vec(), &self.secrets);
        text.extend_from_slice(tail);
        let mut cut = text.len().saturating_sub(holdback(&self.secrets));
        for _ in 0..3 {
            if cut == 0 || cut >= text.len() || !is_continuation(text[cut]) {
                break;
            }
            cut -= 1;
        }
        self.emit(&text[..cut]);
        self.pending = text[cut..].to_vec();
    }

    /// Writes `bytes`. A failed write is counted, not fatal: the app must
    /// never block on a pipe the helper has stopped reading.
    fn emit(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() && self.out.write_all(bytes).is_err() {
            self.dropped += bytes.len() as u64;
        }
    }

    /// The writer and the number of bytes dropped.
    fn into_parts(self) -> (W, u64) {
        (self.out, self.dropped)
    }
}

/// Drains the app's two pipes into their outputs until both reach end of
/// file, or until `app_gone` says the app is gone and nothing has come from
/// the pipes for [`GONE_QUIET`]. Returns the outputs and the bytes each
/// dropped on write errors.
pub(crate) fn drain<R, W>(
    inputs: [R; 2],
    outputs: [W; 2],
    secrets: &[String],
    mut app_gone: impl FnMut() -> bool,
) -> ([W; 2], [u64; 2])
where
    R: Read + AsRawFd,
    W: Write,
{
    let mut secrets = secrets.to_vec();
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    let mut inputs = inputs.map(Some);
    let mut streams = outputs.map(|out| Stream::new(out, secrets.clone()));
    let mut buf = vec![0u8; CHUNK];
    let mut quiet_since = Instant::now();
    loop {
        if inputs.iter().all(Option::is_none) {
            break;
        }
        let watched: Vec<usize> = (0..2).filter(|index| inputs[*index].is_some()).collect();
        let mut fds: Vec<libc::pollfd> = watched
            .iter()
            .filter_map(|index| inputs[*index].as_ref())
            .map(|input| libc::pollfd {
                fd: input.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let ready: Vec<usize> = match wait_readable(&mut fds, TICK) {
            Ok(()) => watched
                .iter()
                .zip(&fds)
                .filter(|(_, fd)| fd.revents != 0)
                .map(|(index, _)| *index)
                .collect(),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Vec::new(),
            // Each read then says what is wrong with its pipe.
            Err(_) => watched,
        };
        for index in ready {
            let Some(input) = inputs[index].as_mut() else {
                continue;
            };
            match input.read(&mut buf) {
                Ok(0) => {
                    inputs[index] = None;
                    streams[index].finish();
                }
                Ok(read) => {
                    streams[index].push(&buf[..read]);
                    quiet_since = Instant::now();
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) => {}
                Err(_) => {
                    inputs[index] = None;
                    streams[index].finish();
                }
            }
        }
        let now = Instant::now();
        for stream in &mut streams {
            stream.idle(now);
        }
        if now.saturating_duration_since(quiet_since) >= GONE_QUIET && app_gone() {
            for (index, input) in inputs.iter_mut().enumerate() {
                if input.take().is_some() {
                    streams[index].finish();
                }
            }
            break;
        }
    }
    let [first, second] = streams.map(Stream::into_parts);
    ([first.0, second.0], [first.1, second.1])
}

/// Waits up to `timeout` for any of `fds` to have events, filling in their
/// `revents`.
fn wait_readable(fds: &mut [libc::pollfd], timeout: Duration) -> io::Result<()> {
    let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: poll reads and writes only the `fds` it is given, which is a
    // slice of exactly `fds.len()` entries.
    let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) };
    if ready < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// One line of the app's output, with its newline. An `ICM_EVENT <json>` line
/// is redacted in the JSON's strings, so it stays valid JSON for the desktop
/// module's `Watch`, and written again only when a string held a secret. The
/// text is then redacted as any line is, which catches a secret in a key or a
/// number too, where the JSON pass cannot. Any other line, and an
/// `ICM_EVENT` that does not parse, is redacted as text.
fn redact_line(line: &[u8], secrets: &[String]) -> Vec<u8> {
    const PREFIX: &[u8] = b"ICM_EVENT ";
    if let Some(json) = line.strip_prefix(PREFIX)
        && let Ok(mut event) = serde_json::from_slice::<Value>(json)
        && process::redact_json(&mut event, secrets)
    {
        let mut out = PREFIX.to_vec();
        out.extend_from_slice(event.to_string().as_bytes());
        out.push(b'\n');
        return process::redact_bytes(out, secrets);
    }
    process::redact_bytes(line.to_vec(), secrets)
}

/// How many bytes at the end of a partial line may begin a secret, so they
/// are held back until more of the line arrives. A match is a form of a
/// secret, possibly with some of its bytes percent-escaped
/// ([`process::redact_text`]), and every form is at most as long as the
/// longest in `secrets`. An escaped byte takes three bytes, so a match is at
/// most three times that length.
fn holdback(secrets: &[String]) -> usize {
    secrets
        .iter()
        .map(String::len)
        .max()
        .map_or(0, |longest| (3 * longest).saturating_sub(1))
}

/// Where a UTF-8 character that the end of `bytes` cuts short begins, or the
/// length of `bytes` when none does.
fn incomplete_utf8_start(bytes: &[u8]) -> usize {
    match std::str::from_utf8(bytes) {
        Err(error) if error.error_len().is_none() => error.valid_up_to(),
        _ => bytes.len(),
    }
}

/// Whether a byte continues a UTF-8 character rather than starting one.
fn is_continuation(byte: u8) -> bool {
    byte & 0xC0 == 0x80
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{PipeReader, PipeWriter};

    /// The secret the tests use: the bytes every form escapes.
    const VALUE: &str = "tok/se\"kr\\it-123456";

    /// Forms of [`VALUE`] as output carries them, each as (before, form,
    /// after) on a line of its own.
    const FORMS: [(&str, &str, &str); 10] = [
        ("", r#"tok/se"kr\it-123456"#, ""),
        ("", r#"tok/se\"kr\\it-123456"#, ""),
        ("", r#"tok\/se\"kr\\it-123456"#, ""),
        ("", r#"tok/se\\\"kr\\\\it-123456"#, ""),
        ("", r#"tok\/se\\\"kr\\\\it-123456"#, ""),
        ("", "tok%2Fse%22kr%5Cit-123456", ""),
        ("", "tok%2fse%22kr%5cit-123456", ""),
        ("", "tok/se%22kr%5cit-123456", ""),
        (
            r#"{"msg":""#,
            r"tok\u002fse\u0022kr\u005cit-123456",
            r#""}"#,
        ),
        ("GET /?t=", "tok%2Fse%22kr%5Cit-123456", "&x=1"),
    ];

    /// The secret list for [`VALUE`]: every form, longest first.
    fn secrets() -> Vec<String> {
        let mut forms = process::secret_forms(VALUE);
        forms.sort_by_key(|form| std::cmp::Reverse(form.len()));
        forms
    }

    /// Feeds two pipes as an app would, and returns what was written to
    /// each live file.
    fn run(stdout: &[u8], stderr: &[u8], secrets: &[String]) -> (Vec<u8>, Vec<u8>) {
        let (out_r, out_w) = std::io::pipe().unwrap();
        let (err_r, err_w) = std::io::pipe().unwrap();
        let feed = |mut writer: PipeWriter, data: Vec<u8>| {
            std::thread::spawn(move || writer.write_all(&data).unwrap())
        };
        let out_feed = feed(out_w, stdout.to_vec());
        let err_feed = feed(err_w, stderr.to_vec());
        let ([out, err], _) = drain([out_r, err_r], sinks(), secrets, || false);
        out_feed.join().unwrap();
        err_feed.join().unwrap();
        (out, err)
    }

    fn pipe() -> (PipeReader, PipeWriter) {
        std::io::pipe().unwrap()
    }

    /// Two empty live files, as the helper's outputs start out.
    fn sinks() -> [Vec<u8>; 2] {
        [Vec::new(), Vec::new()]
    }

    /// Every form of a secret, on its own line, is redacted.
    #[test]
    fn every_form_is_redacted_per_line() {
        let text: String = FORMS
            .iter()
            .map(|(before, form, after)| format!("{before}{form}{after}\n"))
            .collect();
        let expected: String = FORMS
            .iter()
            .map(|(before, _, after)| format!("{before}<redacted>{after}\n"))
            .collect();
        let (out, err) = run(text.as_bytes(), b"", &secrets());
        assert_eq!(String::from_utf8_lossy(&out), expected);
        assert!(err.is_empty());
    }

    /// Cut at any byte, with or without an idle write (or an idle write
    /// after every byte) between the reads, a secret is redacted whole.
    #[test]
    fn a_secret_split_at_every_offset_is_redacted() {
        let secrets = secrets();
        for (before, form, after) in FORMS {
            let line = format!("é€ {before}{form}{after} end\n");
            let expected = format!("é€ {before}<redacted>{after} end\n");
            for split in 0..=line.len() {
                for idle in [false, true] {
                    let mut stream = Stream::new(Vec::new(), secrets.clone());
                    stream.push(&line.as_bytes()[..split]);
                    if idle {
                        stream.idle(Instant::now() + IDLE);
                    }
                    stream.push(&line.as_bytes()[split..]);
                    stream.finish();
                    let (out, dropped) = stream.into_parts();
                    assert_eq!(dropped, 0);
                    assert_eq!(out, expected.as_bytes(), "{form} split at {split}");
                }
            }
            let mut stream = Stream::new(Vec::new(), secrets.clone());
            for byte in line.as_bytes() {
                stream.push(std::slice::from_ref(byte));
                stream.idle(Instant::now() + IDLE);
            }
            stream.finish();
            let (out, _) = stream.into_parts();
            assert_eq!(out, expected.as_bytes(), "{form} byte by byte");
        }
    }

    /// A percent-escaped match can be longer than every form of its secret:
    /// `%61bcdef` is 8 bytes for the 6-byte secret `abcdef`. Cut anywhere
    /// after an idle write, it is still redacted whole, so the holdback is
    /// three times the longest form, not one less than it.
    #[test]
    fn a_percent_escape_longer_than_every_form_is_held_back_whole() {
        let secret = "abcdef";
        let mut secrets = process::secret_forms(secret);
        secrets.sort_by_key(|form| std::cmp::Reverse(form.len()));
        assert_eq!(secrets[0].len(), 6, "{secrets:?}");
        let line = "x %61bcdef y\n";
        for split in 0..=line.len() {
            let mut stream = Stream::new(Vec::new(), secrets.clone());
            stream.push(&line.as_bytes()[..split]);
            stream.idle(Instant::now() + IDLE);
            stream.push(&line.as_bytes()[split..]);
            stream.finish();
            let (out, _) = stream.into_parts();
            assert_eq!(out, b"x <redacted> y\n", "split at {split}");
        }
    }

    /// A partial line with no newline is written, redacted, at end of file.
    #[test]
    fn a_partial_line_is_flushed_redacted_on_eof() {
        let text = format!("waiting {VALUE} for the token");
        let (out, _) = run(text.as_bytes(), b"", &secrets());
        assert_eq!(out, b"waiting <redacted> for the token");
    }

    /// A line longer than the partial limit, with a secret across the
    /// point where its head is written: the secret is redacted whole, and
    /// nothing of it is written before it is complete.
    #[test]
    fn a_long_line_keeps_the_holdback() {
        let mut data = "a".repeat(65530);
        data.push_str(VALUE);
        data.push_str(&"b".repeat(100 * 1024 - data.len()));
        let expected = data.replace(VALUE, "<redacted>");
        let secrets = secrets();
        let mut stream = Stream::new(Vec::new(), secrets);
        let bytes = data.as_bytes();
        stream.push(&bytes[..65540]);
        assert!(stream.out.len() <= 65530, "a secret's head was written");
        for chunk in bytes[65540..].chunks(CHUNK) {
            stream.push(chunk);
        }
        stream.finish();
        let (out, dropped) = stream.into_parts();
        assert_eq!(dropped, 0);
        assert_eq!(out, expected.as_bytes());
    }

    /// Bytes that are not UTF-8 are written as they came unless their line
    /// holds a secret, which the text redaction makes valid UTF-8.
    #[test]
    fn non_utf8_bytes_are_kept_unless_a_secret_is_in_the_line() {
        let text: Vec<u8> = [
            &b"\xff\xfe plain\n"[..],
            &b"\xff "[..],
            VALUE.as_bytes(),
            &b"\n"[..],
        ]
        .concat();
        let (out, _) = run(&text, b"", &secrets());
        let expected: Vec<u8> =
            [&b"\xff\xfe plain\n"[..], "\u{FFFD} <redacted>\n".as_bytes()].concat();
        assert_eq!(out, expected);
    }

    /// An `ICM_EVENT` line stays valid JSON: a secret in its strings is
    /// redacted there, a line without one is written as it was, and one that
    /// is not JSON is redacted as text.
    #[test]
    fn icm_event_lines_stay_valid_json() {
        let ready = r#"ICM_EVENT {"v":1,"kind":"ready","ms":812}"#;
        let panic = r#"ICM_EVENT {"v":1,"kind":"panic","message":"boom tok/se\"kr\\it-123456","location":"src/lib.rs:4:1"}"#;
        let broken = format!(r#"ICM_EVENT {{not json {VALUE}"#);
        let text = format!("{ready}\n{panic}\n{broken}\n");
        let (out, _) = run(text.as_bytes(), b"", &secrets());
        let out = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "{out}");
        assert_eq!(lines[0], ready);
        let event: Value =
            serde_json::from_str(lines[1].strip_prefix("ICM_EVENT ").unwrap()).unwrap();
        assert_eq!(event["kind"], "panic");
        assert_eq!(event["message"], "boom <redacted>");
        assert_eq!(event["location"], "src/lib.rs:4:1");
        assert_eq!(lines[2], "ICM_EVENT {not json <redacted>");
        assert!(!out.contains("123456"), "{out}");
    }

    /// A value the launcher handed the app under a secret name counts from 4
    /// bytes; the same value under a name that is not a secret's does not.
    #[test]
    fn a_handed_secret_of_four_bytes_counts_and_a_plain_name_does_not() {
        let handed = secrets_for(
            &["API_TOKEN".to_string(), "PLAIN_NAME".to_string()],
            |name| {
                let value = if name == "API_TOKEN" { "Qz#8" } else { "Yp#9" };
                Some(OsString::from(value))
            },
        );
        assert!(handed.iter().any(|secret| secret == "Qz#8"), "{handed:?}");
        assert!(handed.iter().all(|secret| secret != "Yp#9"), "{handed:?}");
        let (out, _) = run(b"a Qz#8 b Yp#9 c\n", b"", &handed);
        assert_eq!(out, b"a <redacted> b Yp#9 c\n");
    }

    /// The helper's argv carries names: the arguments are exactly these,
    /// and the parse reads them back.
    #[test]
    fn the_helper_is_started_with_names_only() {
        let identity = Identity {
            start: "1234".into(),
            exe: "/tmp/app".into(),
            unavailable: None,
        };
        let args = argv(
            4242,
            &identity,
            Path::new("/tmp/session"),
            &["API_TOKEN".into(), "DB_PASSWORD".into()],
        );
        let text: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let identity_json = serde_json::to_string(&identity).unwrap();
        assert_eq!(
            text,
            [
                "__session",
                "desktop",
                "--app-pid",
                "4242",
                "--app-identity",
                identity_json.as_str(),
                "--files",
                "/tmp/session",
                "--handed",
                "API_TOKEN",
                "--handed",
                "DB_PASSWORD",
            ]
        );
        let parsed = parse(&text[2..]).unwrap();
        assert_eq!(parsed.app_pid, 4242);
        assert_eq!(parsed.identity, identity);
        assert_eq!(parsed.files, PathBuf::from("/tmp/session"));
        assert_eq!(parsed.handed, ["API_TOKEN", "DB_PASSWORD"]);
    }

    /// The helper stops at end of file at once. With its pipes still open
    /// it stops once the app is gone and nothing has come for
    /// [`GONE_QUIET`], and not before; an app that runs keeps it draining.
    #[test]
    fn the_helper_stops_at_eof_or_once_the_app_is_gone_and_quiet() {
        let (out_r, out_w) = pipe();
        let (err_r, err_w) = pipe();
        drop((out_w, err_w));
        let started = Instant::now();
        let _ = drain([out_r, err_r], sinks(), &[], || false);
        assert!(started.elapsed() < Duration::from_secs(5));

        let (out_r, out_w) = pipe();
        let (err_r, err_w) = pipe();
        let started = Instant::now();
        let _ = drain([out_r, err_r], sinks(), &[], || true);
        assert!(started.elapsed() >= GONE_QUIET);
        assert!(started.elapsed() < Duration::from_secs(5));
        drop((out_w, err_w));

        let (out_r, out_w) = pipe();
        let (err_r, err_w) = pipe();
        let running = std::thread::spawn(move || drain([out_r, err_r], sinks(), &[], || false));
        std::thread::sleep(Duration::from_millis(600));
        assert!(
            !running.is_finished(),
            "stopped while the app's pipes were open"
        );
        drop((out_w, err_w));
        running.join().unwrap();
    }

    /// Output that cannot be written is counted and dropped, and the helper
    /// drains all of it: an output directory that does not exist, and 1 MiB
    /// from the app.
    #[test]
    fn the_helper_keeps_draining_after_write_errors() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone");
        let (out_r, out_w) = pipe();
        let (err_r, err_w) = pipe();
        let data = format!("{}\n", "x".repeat(1023)).repeat(1024);
        let feed = std::thread::spawn(move || {
            let mut out_w = out_w;
            out_w.write_all(data.as_bytes()).unwrap();
        });
        drop(err_w);
        let outputs: [Box<dyn Write>; 2] = [
            open_output(&missing.join(STDOUT_FILE)),
            open_output(&missing.join(STDERR_FILE)),
        ];
        let (_, dropped) = drain([out_r, err_r], outputs, &[], || false);
        feed.join().unwrap();
        assert_eq!(dropped, [1u64 << 20, 0]);
    }
}
