//! Linux window lookup and capture (design §10.1 step 4; implemented, but
//! only macOS is verified in phase 1).
//!
//! - X11 (`DISPLAY` set): `xdotool search --pid <pid>` finds the window and
//!   ImageMagick's `import -window <id>` captures it. CI uses Xvfb.
//! - Wayland only (no `DISPLAY`): no portable capture without a portal and a
//!   prompt, so icm renders the view headlessly, like a missing X11 tool.
//!
//! These are plain process calls, compiled on every host so the parsing is
//! unit-tested on macOS too.

use crate::process::Cmd;
use std::path::Path;
use std::time::Duration;

/// The display server a capture can use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Display {
    /// X11 (or XWayland) at this `DISPLAY`.
    X11(String),
    /// Wayland without X11.
    Wayland,
    /// Neither: a headless host.
    None,
}

/// The display server from the environment.
pub fn display(env: &crate::tools::Env) -> Display {
    match (env.var("DISPLAY"), env.var("WAYLAND_DISPLAY")) {
        (Some(display), _) => Display::X11(display.to_string()),
        (None, Some(_)) => Display::Wayland,
        (None, None) => Display::None,
    }
}

/// `xdotool search --pid <pid> --onlyvisible`: the app's visible windows.
pub fn search_cmd(pid: i32) -> Cmd {
    Cmd::tool("xdotool")
        .args(["search", "--onlyvisible", "--pid"])
        .arg(pid.to_string())
        .timeout(Duration::from_secs(10))
}

/// The first window id in `xdotool search` output.
pub fn parse_search(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && line.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_string)
}

/// `import -window <id> png:<out>`: captures one window.
pub fn capture_cmd(window: &str, out: &Path) -> Cmd {
    let mut target = std::ffi::OsString::from("png:");
    target.push(out.as_os_str());
    Cmd::tool("import")
        .args(["-window", window])
        .arg(target)
        .timeout(Duration::from_secs(30))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_come_from_the_environment() {
        let env = |pairs: &[(&str, &str)]| crate::tools::Env::from_pairs(pairs, None);
        assert_eq!(
            display(&env(&[("DISPLAY", ":99")])),
            Display::X11(":99".into())
        );
        assert_eq!(
            display(&env(&[("DISPLAY", ":0"), ("WAYLAND_DISPLAY", "wayland-0")])),
            Display::X11(":0".into())
        );
        assert_eq!(
            display(&env(&[("WAYLAND_DISPLAY", "wayland-0")])),
            Display::Wayland
        );
        assert_eq!(display(&env(&[])), Display::None);
    }

    #[test]
    fn commands_and_parsing() {
        assert_eq!(
            search_cmd(42).display(),
            "xdotool search --onlyvisible --pid 42"
        );
        assert_eq!(
            capture_cmd("12345", Path::new("/r/screen.png")).display(),
            "import -window 12345 png:/r/screen.png"
        );
        assert_eq!(
            parse_search("\n41943041\n41943050\n").as_deref(),
            Some("41943041")
        );
        assert_eq!(parse_search("Defaulting to search window name\n"), None);
    }
}
