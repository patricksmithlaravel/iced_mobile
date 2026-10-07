//! The session's static server (design §10.2 step 5): `std::net` only,
//! loopback only, one thread per connection, `Connection: close`.
//!
//! - `GET`/`HEAD` serve files under the site root: paths are
//!   percent-decoded, `..` and hidden tricks are refused, symlinks may not
//!   leave the root, directories serve `index.html`, `.wasm` is
//!   `application/wasm`, and every response is `Cache-Control: no-store`.
//! - `POST /__icm/log` takes a console record from the page's forwarder
//!   (used when `--show` opens a system browser).
//! - `POST /__icm/control` is icm's own channel to the session (screenshot,
//!   input, status, stop). It needs the session's token in `X-Icm-Token`.
//! - A POST whose `Origin` is not this server (another site open in the
//!   user's browser) is refused.

use serde_json::{Value, json};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// The control endpoint.
pub const CONTROL_PATH: &str = "/__icm/control";
/// The console forwarder's endpoint.
pub const LOG_PATH: &str = "/__icm/log";
/// The header that carries the control token.
pub const TOKEN_HEADER: &str = "x-icm-token";

const MAX_HEADER: usize = 16 * 1024;
const MAX_BODY: usize = 1024 * 1024;

/// What the session does with the non-file endpoints.
pub trait Handler: Send + Sync + 'static {
    /// The control token.
    fn token(&self) -> &str;
    /// A control request; returns the HTTP status and the JSON reply.
    fn control(&self, request: &Value) -> (u16, Value);
    /// A record from the page's console forwarder.
    fn forwarded(&self, record: &Value);
}

/// Binds `127.0.0.1:<port>` (0: any free port).
pub fn bind(port: u16) -> io::Result<TcpListener> {
    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
}

/// Serves `root` on `listener` from a background thread.
pub fn serve(listener: TcpListener, root: PathBuf, handler: Arc<dyn Handler>) {
    let _ = std::thread::Builder::new()
        .name("icm-web-server".into())
        .spawn(move || {
            let root = Arc::new(root);
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    continue;
                };
                let root = Arc::clone(&root);
                let handler = Arc::clone(&handler);
                let _ = std::thread::Builder::new()
                    .name("icm-web-conn".into())
                    .spawn(move || {
                        let _ = handle(stream, &root, handler.as_ref());
                    });
            }
        });
}

/// A parsed request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    /// `GET`, `HEAD`, `POST`, ...
    pub method: String,
    /// The path, without the query string, still percent-encoded.
    pub path: String,
    /// The query string, without `?`.
    pub query: String,
    /// Header names in lower case, with their values.
    pub headers: Vec<(String, String)>,
    /// The body (`Content-Length` bytes).
    pub body: Vec<u8>,
}

impl Request {
    /// A header's value.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Reads one request.
pub fn read_request(reader: &mut impl BufRead) -> Result<Request, String> {
    let mut head = Vec::new();
    loop {
        let mut line = Vec::new();
        let read = reader
            .take((MAX_HEADER - head.len().min(MAX_HEADER)) as u64 + 1)
            .read_until(b'\n', &mut line)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            return Err("the connection closed".into());
        }
        head.extend_from_slice(&line);
        if head.len() > MAX_HEADER {
            return Err("the request head is too large".into());
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
    }

    let text = String::from_utf8_lossy(&head);
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    if method.is_empty() || !target.starts_with('/') {
        return Err(format!("bad request line `{first}`"));
    }
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (target, String::new()),
    };

    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();

    let length = headers
        .iter()
        .find(|(key, _)| key == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    if length > MAX_BODY {
        return Err("the request body is too large".into());
    }
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .map_err(|error| error.to_string())?;

    Ok(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

/// Decodes `%XX` escapes; `None` for a malformed escape or invalid UTF-8.
pub fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The file a request path names under `root`, or the HTTP status to
/// answer instead (400 for a hostile path, 404 when missing).
pub fn resolve(root: &Path, path: &str) -> Result<PathBuf, u16> {
    let decoded = percent_decode(path).ok_or(400u16)?;
    if decoded.contains('\0') || decoded.contains('\\') {
        return Err(400);
    }

    let mut file = root.to_path_buf();
    for part in decoded.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(400),
            part => {
                let component = Path::new(part);
                if component.components().count() != 1
                    || !matches!(component.components().next(), Some(Component::Normal(_)))
                {
                    return Err(400);
                }
                file.push(part);
            }
        }
    }

    if file.is_dir() {
        file.push("index.html");
    }
    if !file.is_file() {
        return Err(404);
    }

    // Symlinks may not lead out of the root.
    let real_root = std::fs::canonicalize(root).map_err(|_| 404u16)?;
    let real = std::fs::canonicalize(&file).map_err(|_| 404u16)?;
    if !real.starts_with(&real_root) {
        return Err(404);
    }
    Ok(file)
}

/// Whether a request comes from the served page itself or from a client
/// that sends no `Origin` (icm, curl): a browser sends `Origin` on every
/// cross-origin POST, and it must then name this server's own host.
pub fn same_origin(request: &Request) -> bool {
    match (request.header("origin"), request.header("host")) {
        (None, _) => true,
        (Some(origin), Some(host)) => origin == format!("http://{host}"),
        (Some(_), None) => false,
    }
}

/// The `Content-Type` for a file.
pub fn mime(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

fn respond(
    stream: &mut impl Write,
    status: u16,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        reason(status),
        body.len()
    )?;
    if !head_only {
        stream.write_all(body)?;
    }
    stream.flush()
}

fn respond_json(stream: &mut impl Write, status: u16, value: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(value).unwrap_or_default();
    respond(stream, status, "application/json", &body, false)
}

fn handle(stream: TcpStream, root: &Path, handler: &dyn Handler) -> io::Result<()> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(60)));
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    let request = match read_request(&mut reader) {
        Ok(request) => request,
        Err(error) => {
            return respond(
                &mut writer,
                400,
                "text/plain; charset=utf-8",
                error.as_bytes(),
                false,
            );
        }
    };

    // Another site open in the user's browser may POST here; only the
    // page itself (same origin) and icm (no Origin) may.
    if request.method == "POST" && !same_origin(&request) {
        return respond_json(&mut writer, 403, &json!({"error": "cross-origin request"}));
    }

    match (request.method.as_str(), request.path.as_str()) {
        ("POST", CONTROL_PATH) => {
            let token = request.header(TOKEN_HEADER).unwrap_or("");
            if token.is_empty() || token != handler.token() {
                return respond_json(&mut writer, 403, &json!({"error": "bad token"}));
            }
            let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
            let (status, reply) = handler.control(&body);
            respond_json(&mut writer, status, &reply)
        }
        ("POST", LOG_PATH) => {
            if let Ok(record) = serde_json::from_slice::<Value>(&request.body) {
                handler.forwarded(&record);
            }
            respond(&mut writer, 204, "text/plain", b"", false)
        }
        (_, path) if path.starts_with("/__icm/") => {
            respond(&mut writer, 404, "text/plain; charset=utf-8", b"", false)
        }
        ("GET" | "HEAD", path) => {
            let head_only = request.method == "HEAD";
            match resolve(root, path) {
                Ok(file) => match std::fs::read(&file) {
                    Ok(bytes) => respond(&mut writer, 200, mime(&file), &bytes, head_only),
                    Err(_) => respond(&mut writer, 404, "text/plain", b"not found", head_only),
                },
                Err(status) => respond(
                    &mut writer,
                    status,
                    "text/plain; charset=utf-8",
                    reason(status).as_bytes(),
                    head_only,
                ),
            }
        }
        _ => respond(
            &mut writer,
            405,
            "text/plain; charset=utf-8",
            b"GET and HEAD only",
            false,
        ),
    }
}

/// Sends one HTTP request to `127.0.0.1:<port>` and returns the status and
/// body (icm's client side of the control channel, and tests).
pub fn request(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    timeout: Duration,
) -> io::Result<(u16, Vec<u8>)> {
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let mut stream = TcpStream::connect_timeout(&address.into(), Duration::from_secs(5))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;

    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (key, value) in headers {
        head.push_str(&format!("{key}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;

    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response)?;
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no HTTP response head"))?;
    let head = String::from_utf8_lossy(&response[..split]);
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no HTTP status"))?;
    Ok((status, response[split + 4..].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Fake {
        logs: Mutex<Vec<Value>>,
    }

    impl Handler for Fake {
        fn token(&self) -> &str {
            "secret-token"
        }
        fn control(&self, request: &Value) -> (u16, Value) {
            (200, json!({"echo": request}))
        }
        fn forwarded(&self, record: &Value) {
            self.logs.lock().unwrap().push(record.clone());
        }
    }

    fn site() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("pkg")).unwrap();
        std::fs::write(dir.path().join("index.html"), "<!doctype html>hi").unwrap();
        std::fs::write(dir.path().join("pkg/app_bg.wasm"), b"\0asm").unwrap();
        std::fs::write(dir.path().join("pkg/a b.js"), "x").unwrap();
        dir
    }

    #[test]
    fn requests_parse() {
        let raw = b"POST /__icm/log?x=1 HTTP/1.1\r\nHost: h\r\nContent-Length: 4\r\nX-Icm-Token: t\r\n\r\nbodyEXTRA";
        let request = read_request(&mut &raw[..]).unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/__icm/log");
        assert_eq!(request.query, "x=1");
        assert_eq!(request.header("x-icm-token"), Some("t"));
        assert_eq!(request.body, b"body");
        assert!(read_request(&mut &b"nonsense\r\n\r\n"[..]).is_err());
    }

    #[test]
    fn only_the_page_itself_may_post() {
        let request = |headers: &[(&str, &str)]| Request {
            method: "POST".into(),
            path: LOG_PATH.into(),
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            ..Request::default()
        };
        assert!(same_origin(&request(&[("host", "127.0.0.1:8787")])));
        assert!(same_origin(&request(&[
            ("host", "127.0.0.1:8787"),
            ("origin", "http://127.0.0.1:8787")
        ])));
        assert!(!same_origin(&request(&[
            ("host", "127.0.0.1:8787"),
            ("origin", "https://evil.example")
        ])));
        assert!(!same_origin(&request(&[(
            "origin",
            "http://127.0.0.1:8787"
        )])));
    }

    #[test]
    fn paths_stay_inside_the_root() {
        let dir = site();
        let root = dir.path();
        assert_eq!(resolve(root, "/").unwrap(), root.join("index.html"));
        assert_eq!(
            resolve(root, "/pkg/app_bg.wasm").unwrap(),
            root.join("pkg/app_bg.wasm")
        );
        assert_eq!(
            resolve(root, "/pkg/a%20b.js").unwrap(),
            root.join("pkg/a b.js")
        );
        assert_eq!(resolve(root, "/../etc/passwd"), Err(400));
        assert_eq!(resolve(root, "/pkg/%2e%2e/%2e%2e/x"), Err(400));
        assert_eq!(resolve(root, "/a%5cb"), Err(400));
        assert_eq!(resolve(root, "/%zz"), Err(400));
        assert_eq!(resolve(root, "/missing.js"), Err(404));

        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), root.join("link")).unwrap();
        assert_eq!(resolve(root, "/link"), Err(404));
    }

    #[test]
    fn wasm_has_its_mime_type() {
        assert_eq!(mime(Path::new("a/app_bg.wasm")), "application/wasm");
        assert_eq!(mime(Path::new("app.js")), "text/javascript; charset=utf-8");
        assert_eq!(mime(Path::new("index.html")), "text/html; charset=utf-8");
        assert_eq!(
            mime(Path::new("manifest.webmanifest")),
            "application/manifest+json"
        );
        assert_eq!(mime(Path::new("x.unknown")), "application/octet-stream");
    }

    #[test]
    fn the_server_serves_files_and_guards_control() {
        let dir = site();
        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let fake = Arc::new(Fake {
            logs: Mutex::new(Vec::new()),
        });
        serve(listener, dir.path().to_path_buf(), fake.clone());
        let timeout = Duration::from_secs(5);

        let (status, body) = request(port, "GET", "/", &[], b"", timeout).unwrap();
        assert_eq!((status, body.as_slice()), (200, &b"<!doctype html>hi"[..]));

        let (status, body) = request(port, "HEAD", "/pkg/app_bg.wasm", &[], b"", timeout).unwrap();
        assert_eq!(status, 200);
        assert!(body.is_empty());

        let (status, _) = request(port, "GET", "/../x", &[], b"", timeout).unwrap();
        assert_eq!(status, 400);
        let (status, _) = request(port, "DELETE", "/", &[], b"", timeout).unwrap();
        assert_eq!(status, 405);

        let (status, _) = request(port, "POST", CONTROL_PATH, &[], b"{}", timeout).unwrap();
        assert_eq!(status, 403);
        let (status, body) = request(
            port,
            "POST",
            CONTROL_PATH,
            &[("X-Icm-Token", "secret-token")],
            br#"{"op":"status"}"#,
            timeout,
        )
        .unwrap();
        assert_eq!(status, 200);
        let reply: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(reply["echo"]["op"], "status");

        let (status, _) = request(
            port,
            "POST",
            LOG_PATH,
            &[],
            br#"{"level":"warn","msg":"hi"}"#,
            timeout,
        )
        .unwrap();
        assert_eq!(status, 204);
        assert_eq!(fake.logs.lock().unwrap()[0]["msg"], "hi");

        let (status, _) = request(
            port,
            "POST",
            LOG_PATH,
            &[("Origin", "https://evil.example")],
            br#"{"level":"warn","msg":"spoofed"}"#,
            timeout,
        )
        .unwrap();
        assert_eq!(status, 403);
        assert_eq!(fake.logs.lock().unwrap().len(), 1);
    }
}
