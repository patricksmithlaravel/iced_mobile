//! Behaves like a desktop app for `icm run desktop`, without a window.

use std::time::Duration;

fn event(json: &str) {
    if std::env::var("ICM_EVENTS").is_ok_and(|value| value == "1") {
        eprintln!("ICM_EVENT {json}");
    }
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
        "exit" => {
            eprintln!("error: the fixture gives up");
            std::process::exit(3);
        }
        "hang" => {}
        _ => {
            event(&start);
            println!("hello from stdout");
            eprintln!("warning: a fixture warning");
            event(
                r#"{"v":1,"kind":"ready","ms":1,"window":{"size":[400,300],"physical":[800,600],"scale":2},"backend":"tiny-skia","adapter":"none","api":"none"}"#,
            );
        }
    }
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}
