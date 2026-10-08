//! Behaves like a desktop app for `icm run desktop`, without a window.

use std::time::Duration;

fn event(json: &str) {
    if std::env::var("ICM_EVENTS").is_ok_and(|value| value == "1") {
        eprintln!("ICM_EVENT {json}");
    }
}

/// A text inside a JSON string.
fn escaped(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// A text percent-encoded for a URL: letters, digits and `keep` stay, a
/// space is `space`, other bytes are `%XX`, or `%xx` when `lower`.
fn encoded(text: &str, keep: &str, space: &str, lower: bool) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || keep.as_bytes().contains(&byte) {
            out.push(char::from(byte));
        } else if byte == b' ' && !space.is_empty() {
            out.push_str(space);
        } else if lower {
            out.push_str(&format!("%{byte:02x}"));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Logs the secret it was given (`ICM_TEST_API_TOKEN`) as apps do: plain,
/// in a JSON line, in an `ICM_EVENT` warning, in URLs (form-urlencoded as
/// the `form_urlencoded` crate writes it, and with lowercase hex) and as
/// JSON with `\u` escapes after a log prefix.
fn log_token() -> Option<String> {
    let token = std::env::var("ICM_TEST_API_TOKEN").ok()?;
    println!("signed in with {token}");
    eprintln!("{{\"token\":\"{}\"}}", escaped(&token));
    eprintln!(
        "GET https://api.example.com/v1?token={}&lower={}",
        encoded(&token, "*-._", "+", false),
        encoded(&token, "-_.~", "", true)
    );
    eprintln!(
        "INFO fixture: payload {{\"t\":\"{}\"}}",
        escaped(&token).replace('/', "\\u002f")
    );
    event(&format!(
        r#"{{"v":1,"kind":"warning","code":"fixture.token","message":"token {}"}}"#,
        escaped(&token)
    ));
    Some(token)
}

/// Replaces this process by another program, as a launcher's `exec` does:
/// the pid and the start time stay, and the environment that can be read
/// back from the process is the new program's own.
///
/// - `exec`: this program again, in `idle` mode, started with an empty
///   environment and the same `argv[0]`.
/// - `exec-restricted`: the system's `sleep`, whose environment macOS keeps
///   from other processes (it is a platform binary): reading it back gives
///   the arguments and no variables.
fn replace_process(mode: &str) -> ! {
    use std::os::unix::process::CommandExt;
    let error = if mode == "exec" {
        let argv0 = std::env::args_os().next().unwrap_or_default();
        std::process::Command::new(&argv0)
            .arg0(&argv0)
            .arg("idle")
            .env_clear()
            .exec()
    } else {
        std::process::Command::new("/bin/sleep").arg("600").exec()
    };
    eprintln!("error: cannot replace the process: {error}");
    std::process::exit(4);
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("idle") {
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    let start = format!(
        r#"{{"v":1,"kind":"start","protocol":1,"framework":"0.14.1","pid":{},"platform":"test","bridge":null}}"#,
        std::process::id()
    );
    match std::env::var("ICM_FIXTURE").as_deref().unwrap_or("ready") {
        "panic" => {
            event(&start);
            let items: Vec<u32> = Vec::new();
            println!("{}", items[7]);
        }
        "leak" => {
            event(&start);
            let token = log_token().unwrap_or_default();
            panic!("rejected token {token}");
        }
        "exit" => {
            eprintln!("error: the fixture gives up");
            std::process::exit(3);
        }
        "hang" => {}
        mode => {
            event(&start);
            println!("hello from stdout");
            eprintln!("warning: a fixture warning");
            let token = log_token();
            // The protocol's fields, then what an app may add: the secret
            // in a field of its own, in the window and in a free-form name.
            let carried = escaped(token.as_deref().unwrap_or_default());
            event(&format!(
                r#"{{"v":1,"kind":"ready","ms":1,"window":{{"size":[400,300],"physical":[800,600],"scale":2,"title":"{carried}"}},"backend":"tiny-skia","adapter":"none{carried}","api":"none","account":{{"token":"{carried}"}}}}"#
            ));
            if mode == "quit" {
                // Ends by itself after the run, once the project has a
                // `quit` file, logging the secret once more.
                while !std::path::Path::new("quit").exists() {
                    std::thread::sleep(Duration::from_millis(50));
                }
                println!("after the run: {}", token.unwrap_or_default());
                return;
            }
            if mode.starts_with("exec") {
                // It keeps its pid and start time, and gives out no
                // environment: the secret it logged was inherited.
                replace_process(mode);
            }
        }
    }
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}
