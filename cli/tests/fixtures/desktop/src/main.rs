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

/// Logs the secret it was given (`ICM_TEST_API_TOKEN`) as apps do: plain,
/// in a JSON line and in an `ICM_EVENT` warning.
fn log_token() -> Option<String> {
    let token = std::env::var("ICM_TEST_API_TOKEN").ok()?;
    println!("signed in with {token}");
    eprintln!("{{\"token\":\"{}\"}}", escaped(&token));
    event(&format!(
        r#"{{"v":1,"kind":"warning","code":"fixture.token","message":"token {}"}}"#,
        escaped(&token)
    ));
    Some(token)
}

fn main() {
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
        _ => {
            event(&start);
            println!("hello from stdout");
            eprintln!("warning: a fixture warning");
            let _ = log_token();
            event(
                r#"{"v":1,"kind":"ready","ms":1,"window":{"size":[400,300],"physical":[800,600],"scale":2},"backend":"tiny-skia","adapter":"none","api":"none"}"#,
            );
        }
    }
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}
