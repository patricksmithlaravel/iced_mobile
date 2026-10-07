//! Headless Chrome over the DevTools protocol on a pipe (design §10.2 step
//! 5): Chrome is started with `--remote-debugging-pipe`, reads commands on
//! its fd 3 and writes replies and events on its fd 4, each message one
//! JSON object followed by a NUL byte. No WebSocket and no port.
//!
//! [`Conn`] is the connection: [`Conn::call`] sends a command and waits for
//! its reply; a reader thread hands every event (a message without `id`)
//! to the callback given at launch. Page commands go to a flattened target
//! session (`sessionId` on the message).

use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// An event callback.
pub type OnEvent = Box<dyn Fn(Value) + Send + 'static>;

/// A DevTools connection.
pub struct Conn {
    writer: Mutex<Box<dyn Write + Send>>,
    next: AtomicU64,
    pending: Mutex<HashMap<u64, Sender<Value>>>,
    closed: AtomicBool,
}

impl Conn {
    /// A connection over a writer and a reader; starts the reader thread.
    pub fn new(
        writer: Box<dyn Write + Send>,
        reader: Box<dyn Read + Send>,
        on_event: OnEvent,
    ) -> Arc<Conn> {
        let conn = Arc::new(Conn {
            writer: Mutex::new(writer),
            next: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        });
        let reading = Arc::clone(&conn);
        let _ = std::thread::Builder::new()
            .name("icm-cdp-reader".into())
            .spawn(move || reading.read_loop(reader, on_event));
        conn
    }

    fn read_loop(&self, reader: Box<dyn Read + Send>, on_event: OnEvent) {
        let mut reader = BufReader::new(reader);
        loop {
            let mut message = Vec::new();
            match reader.read_until(0, &mut message) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if message.last() == Some(&0) {
                let _ = message.pop();
            }
            let Ok(value) = serde_json::from_slice::<Value>(&message) else {
                continue;
            };
            match value.get("id").and_then(Value::as_u64) {
                Some(id) => {
                    let waiter = self.pending.lock().ok().and_then(|mut p| p.remove(&id));
                    if let Some(waiter) = waiter {
                        let _ = waiter.send(value);
                    }
                }
                None => on_event(value),
            }
        }
        self.closed.store(true, Ordering::SeqCst);
        // Wake every waiter: dropping the senders ends their recv.
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
    }

    /// Whether the other end has gone.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Sends a command (to a target session when `session` is given) and
    /// waits up to `timeout` for its reply's `result`.
    pub fn call(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        if self.is_closed() {
            return Err(format!("{method}: Chrome has closed the DevTools pipe"));
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = mpsc::channel();
        if let Ok(mut pending) = self.pending.lock() {
            let _ = pending.insert(id, sender);
        }

        let bytes = encode(id, session, method, &params);
        let sent = self
            .writer
            .lock()
            .map_err(|_| "the DevTools writer is poisoned".to_string())
            .and_then(|mut writer| {
                writer
                    .write_all(&bytes)
                    .and_then(|()| writer.flush())
                    .map_err(|error| format!("{method}: cannot write to Chrome: {error}"))
            });
        if let Err(error) = sent {
            self.forget(id);
            return Err(error);
        }

        match receiver.recv_timeout(timeout) {
            Ok(reply) => match reply.get("error") {
                Some(error) => Err(format!(
                    "{method}: {}",
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("error")
                )),
                None => Ok(reply.get("result").cloned().unwrap_or(Value::Null)),
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.forget(id);
                Err(format!(
                    "{method}: no reply from Chrome within {}",
                    crate::time::format_duration(timeout)
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(format!("{method}: Chrome closed the DevTools pipe"))
            }
        }
    }

    fn forget(&self, id: u64) {
        if let Ok(mut pending) = self.pending.lock() {
            let _ = pending.remove(&id);
        }
    }
}

/// One message on the wire: the JSON command and a NUL.
pub fn encode(id: u64, session: Option<&str>, method: &str, params: &Value) -> Vec<u8> {
    let mut message = json!({"id": id, "method": method, "params": params});
    if let Some(session) = session {
        message["sessionId"] = json!(session);
    }
    let mut bytes = serde_json::to_vec(&message).unwrap_or_default();
    bytes.push(0);
    bytes
}

/// Chrome's arguments (design §10.2 step 5) for a page at `scale` device
/// pixels per CSS pixel, plus the switches that keep a headless page
/// rendering at full speed and keep Chrome away from the keychain, sync and
/// first-run UI.
pub fn chrome_args(user_data_dir: &Path, scale: f64) -> Vec<String> {
    vec![
        "--headless=new".to_string(),
        // The emulated device pixel ratio alone is not enough: under
        // `Emulation.setDeviceMetricsOverride` Chrome still reports
        // `devicePixelContentBoxSize` at the real ratio (1 when headless),
        // so winit sized the canvas at CSS pixels and iced drew everything
        // `scale` times too large. A real ratio keeps the two consistent.
        format!("--force-device-scale-factor={scale}"),
        "--remote-debugging-pipe".to_string(),
        "--use-angle=swiftshader".to_string(),
        "--enable-unsafe-swiftshader".to_string(),
        format!("--user-data-dir={}", user_data_dir.display()),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--hide-scrollbars".to_string(),
        "--mute-audio".to_string(),
        "--use-mock-keychain".to_string(),
        "--password-store=basic".to_string(),
        "--disable-sync".to_string(),
        "--disable-extensions".to_string(),
        "--disable-component-update".to_string(),
        "--disable-background-networking".to_string(),
        "--disable-background-timer-throttling".to_string(),
        "--disable-backgrounding-occluded-windows".to_string(),
        "--disable-renderer-backgrounding".to_string(),
        "--disable-features=Translate,MediaRouter,OptimizationHints".to_string(),
        "about:blank".to_string(),
    ]
}

/// A running Chrome and its connection.
pub struct Browser {
    /// The browser process.
    pub child: Child,
    /// The DevTools connection.
    pub conn: Arc<Conn>,
}

impl Browser {
    /// The browser's pid.
    pub fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    /// Whether Chrome is still running (reaps it when it is not).
    pub fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Closes the browser: `Browser.close`, then SIGTERM and SIGKILL if it
    /// lingers.
    pub fn close(&mut self) {
        let _ = self
            .conn
            .call(None, "Browser.close", json!({}), Duration::from_secs(2));
        for (signal, wait) in [(libc::SIGTERM, 1_000), (libc::SIGKILL, 2_000)] {
            let deadline = std::time::Instant::now() + Duration::from_millis(wait);
            while std::time::Instant::now() < deadline {
                if !self.running() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            // SAFETY: kill(2) on our own child's pid.
            unsafe {
                let _ = libc::kill(self.pid(), signal);
            }
        }
        let _ = self.child.wait();
    }
}

fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: pipe(2) writes two descriptors into the array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    for fd in fds {
        // SAFETY: fcntl on descriptors we just created.
        unsafe {
            let _ = libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
    // SAFETY: both descriptors are open and owned by nobody else.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Starts Chrome with the DevTools pipe on its fds 3 and 4, output to
/// `log`, in the caller's process group.
pub fn launch(
    chrome: &Path,
    args: &[String],
    log: &Path,
    on_event: OnEvent,
) -> io::Result<Browser> {
    let (to_chrome_read, to_chrome_write) = pipe()?;
    let (from_chrome_read, from_chrome_write) = pipe()?;
    let chrome_in = to_chrome_read.as_raw_fd();
    let chrome_out = from_chrome_write.as_raw_fd();

    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)?;

    let mut command = Command::new(chrome);
    let _ = command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file.try_clone()?))
        .stderr(Stdio::from(log_file));

    // SAFETY: only async-signal-safe calls (fcntl, dup2, close) on
    // descriptors that are open in the child.
    unsafe {
        let _ = command.pre_exec(move || {
            // Move both ends above 9 first, so neither dup2 below can
            // clobber the other.
            let input = libc::fcntl(chrome_in, libc::F_DUPFD, 10);
            let output = libc::fcntl(chrome_out, libc::F_DUPFD, 10);
            if input < 0 || output < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::dup2(input, 3) < 0 || libc::dup2(output, 4) < 0 {
                return Err(io::Error::last_os_error());
            }
            let _ = libc::close(input);
            let _ = libc::close(output);
            Ok(())
        });
    }

    let child = command.spawn()?;
    drop(to_chrome_read);
    drop(from_chrome_write);

    let conn = Conn::new(
        Box::new(File::from(to_chrome_write)),
        Box::new(File::from(from_chrome_read)),
        on_event,
    );
    Ok(Browser { child, conn })
}

/// Finds the first page target, or creates one, and attaches to it
/// (flattened); returns the session id.
pub fn attach_page(conn: &Conn, timeout: Duration) -> Result<String, String> {
    let deadline = std::time::Instant::now() + timeout;
    let short = Duration::from_secs(10);
    let target = loop {
        let targets = conn.call(None, "Target.getTargets", json!({}), short)?;
        let page = targets
            .get("targetInfos")
            .and_then(Value::as_array)
            .and_then(|infos| {
                infos
                    .iter()
                    .find(|info| info.get("type").and_then(Value::as_str) == Some("page"))
            })
            .and_then(|info| info.get("targetId"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(page) = page {
            break page;
        }
        if std::time::Instant::now() >= deadline {
            let created = conn.call(
                None,
                "Target.createTarget",
                json!({"url": "about:blank"}),
                short,
            )?;
            break created
                .get("targetId")
                .and_then(Value::as_str)
                .ok_or("Target.createTarget returned no targetId")?
                .to_string();
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    let attached = conn.call(
        None,
        "Target.attachToTarget",
        json!({"targetId": target, "flatten": true}),
        short,
    )?;
    attached
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "Target.attachToTarget returned no sessionId".to_string())
}

/// Decodes standard base64 (as in `Page.captureScreenshot`'s `data`).
pub fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    let value = |byte: u8| -> Option<u32> {
        Some(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };

    let bytes: Vec<u8> = text
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut accumulator = 0u32;
        for (index, byte) in chunk.iter().enumerate() {
            let v = value(*byte).ok_or_else(|| format!("invalid base64 byte {byte:#x}"))?;
            accumulator |= v << (18 - 6 * index as u32);
        }
        let produced = match chunk.len() {
            4 => 3,
            3 => 2,
            2 => 1,
            _ => return Err("truncated base64".into()),
        };
        for index in 0..produced {
            out.push((accumulator >> (16 - 8 * index as u32)) as u8);
        }
    }
    Ok(out)
}

/// Where a session's Chrome profile lives.
pub fn profile_dir(session_dir: &Path) -> PathBuf {
    session_dir.join("chrome-profile")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn messages_are_nul_terminated_json() {
        let bytes = encode(7, Some("S1"), "Page.navigate", &json!({"url": "x"}));
        assert_eq!(bytes.last(), Some(&0));
        let value: Value = serde_json::from_slice(&bytes[..bytes.len() - 1]).unwrap();
        assert_eq!(value["id"], 7);
        assert_eq!(value["sessionId"], "S1");
        assert_eq!(value["params"]["url"], "x");
    }

    #[test]
    fn base64_decodes() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVsbG8gd29ybGQ").unwrap(), b"hello world");
        assert_eq!(base64_decode("").unwrap(), b"");
        assert!(base64_decode("a$==").is_err());
    }

    #[test]
    fn calls_get_their_replies_and_events_go_to_the_callback() {
        // A fake Chrome on the other end of two socket pairs.
        let (ours_write, theirs_read) = UnixStream::pair().unwrap();
        let (theirs_write, ours_read) = UnixStream::pair().unwrap();
        let (events_tx, events_rx) = mpsc::channel();
        let conn = Conn::new(
            Box::new(ours_write),
            Box::new(ours_read),
            Box::new(move |event| {
                let _ = events_tx.send(event);
            }),
        );

        let fake = std::thread::spawn(move || {
            let mut reader = BufReader::new(theirs_read);
            let mut writer = theirs_write;
            let mut message = Vec::new();
            let _ = reader.read_until(0, &mut message).unwrap();
            let request: Value = serde_json::from_slice(&message[..message.len() - 1]).unwrap();
            let id = request["id"].as_u64().unwrap();
            // An event first, then the reply, then an error reply.
            writer
                .write_all(
                    b"{\"method\":\"Runtime.consoleAPICalled\",\"params\":{\"type\":\"log\"}}\0",
                )
                .unwrap();
            writer
                .write_all(
                    format!("{{\"id\":{id},\"result\":{{\"product\":\"HeadlessChrome/1\"}}}}\0")
                        .as_bytes(),
                )
                .unwrap();
            message.clear();
            let _ = reader.read_until(0, &mut message).unwrap();
            let request: Value = serde_json::from_slice(&message[..message.len() - 1]).unwrap();
            let id = request["id"].as_u64().unwrap();
            writer
                .write_all(
                    format!("{{\"id\":{id},\"error\":{{\"message\":\"no such method\"}}}}\0")
                        .as_bytes(),
                )
                .unwrap();
        });

        let timeout = Duration::from_secs(5);
        let reply = conn
            .call(None, "Browser.getVersion", json!({}), timeout)
            .unwrap();
        assert_eq!(reply["product"], "HeadlessChrome/1");
        let event = events_rx.recv_timeout(timeout).unwrap();
        assert_eq!(event["method"], "Runtime.consoleAPICalled");
        let error = conn
            .call(None, "Nope.nope", json!({}), timeout)
            .unwrap_err();
        assert!(error.contains("no such method"), "{error}");
        fake.join().unwrap();

        // The fake hung up: calls fail fast instead of waiting.
        let started = std::time::Instant::now();
        let error = loop {
            match conn.call(None, "Browser.getVersion", json!({}), timeout) {
                Err(error) if conn.is_closed() => break error,
                _ if started.elapsed() > timeout => panic!("the closed pipe was not noticed"),
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        assert!(error.contains("pipe"), "{error}");
    }
}
