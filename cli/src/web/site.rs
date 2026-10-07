//! The dev site (design §9.5, §10.2 step 4): `index.html`,
//! `manifest.webmanifest`, the icon and `[app] resources`, next to the
//! `pkg/` wasm-bindgen writes. Fonts are not files here: the app embeds
//! them in the wasm (iced's `fira-sans` feature), because the web has no
//! system fonts; `icm run web` checks that the feature resolves for wasm32
//! (`web.fonts_embedded`).

use std::path::{Component, Path, PathBuf};

/// wasm-bindgen's `--out-name`: `pkg/app.js` and `pkg/app_bg.wasm`.
pub const OUT_NAME: &str = "app";

/// The query string icm's headless Chrome loads the page with: events on,
/// and the console forwarder off (the session reads the console over the
/// DevTools pipe instead).
pub const HEADLESS_QUERY: &str = "icm_events=1&icm_cdp=1";

/// The query string for a system browser (`--show`): events on, and the
/// forwarder posting the console to `/__icm/log`.
pub const SHOW_QUERY: &str = "icm_events=1";

/// What the site is made from.
#[derive(Clone, Debug)]
pub struct Inputs {
    /// `[app] name`.
    pub name: String,
    /// `[app] background`, `#RRGGBB`.
    pub background: String,
    /// The icon PNG, if `[app] icon` names one that exists.
    pub icon: Option<PathBuf>,
    /// The project directory, which `resources` globs are relative to.
    pub project_dir: PathBuf,
    /// `[app] resources`.
    pub resources: Vec<String>,
}

/// Escapes text for HTML.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// `#RRGGBB`, or white when the value is anything else.
pub fn color(value: &str) -> String {
    let valid = value.len() == 7
        && value.starts_with('#')
        && value[1..].chars().all(|c| c.is_ascii_hexdigit());
    if valid {
        value.to_ascii_uppercase()
    } else {
        "#FFFFFF".to_string()
    }
}

/// The dev script: forwards the console and uncaught errors to
/// `/__icm/log` when the page is not driven by icm's headless Chrome.
const FORWARDER: &str = r#"(function () {
  var query = new URLSearchParams(location.search);
  if (query.get("icm_cdp") === "1") return;
  function text(value) {
    if (typeof value === "string") return value;
    if (value instanceof Error) return value.stack || String(value);
    try { return JSON.stringify(value); } catch (e) { return String(value); }
  }
  function send(level, values) {
    var msg = Array.prototype.map.call(values, text).join(" ");
    try {
      fetch("/__icm/log", { method: "POST", keepalive: true,
        body: JSON.stringify({ level: level, msg: msg, epoch_ms: Date.now() }) });
    } catch (e) {}
  }
  ["log", "info", "warn", "error", "debug"].forEach(function (method) {
    var original = console[method];
    console[method] = function () {
      send(method, arguments);
      return original.apply(console, arguments);
    };
  });
  addEventListener("error", function (event) {
    send("error", ["uncaught: " + event.message + " at " + event.filename + ":" + event.lineno]);
  });
  addEventListener("unhandledrejection", function (event) {
    send("error", ["unhandled rejection: " + text(event.reason)]);
  });
})();"#;

/// The dev `index.html`.
pub fn index_html(name: &str, background: &str, icon: bool) -> String {
    // Without an icon, an empty one: the browser would otherwise ask for
    // /favicon.ico and log the 404 as an error.
    let icon = if icon {
        "<link rel=\"icon\" href=\"icon.png\">"
    } else {
        "<link rel=\"icon\" href=\"data:,\">"
    };
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>{title}</title>{icon}<link rel="manifest" href="manifest.webmanifest">
<meta name="theme-color" content="{background}">
<style>html,body{{margin:0;height:100%;background:{background}}}canvas{{display:block;width:100%;height:100%;outline:none}}</style>
<script>
{FORWARDER}
</script>
</head><body>
<script type="module">
import init from "./pkg/{OUT_NAME}.js";
init({{ module_or_path: "./pkg/{OUT_NAME}_bg.wasm" }}).catch(function (error) {{
  console.error("icm: the app failed to start: " + (error && error.stack || error));
}});
</script>
</body></html>
"#,
        title = escape(name),
    )
}

/// The dev `manifest.webmanifest`.
pub fn manifest(name: &str, background: &str, icon: Option<(u32, u32)>) -> String {
    let icons = match icon {
        Some((width, height)) => serde_json::json!([
            {"src": "icon.png", "sizes": format!("{width}x{height}"), "type": "image/png"}
        ]),
        None => serde_json::json!([]),
    };
    let manifest = serde_json::json!({
        "name": name,
        "short_name": name,
        "start_url": ".",
        "display": "standalone",
        "background_color": background,
        "theme_color": background,
        "icons": icons,
    });
    let mut text = serde_json::to_string_pretty(&manifest).unwrap_or_default();
    text.push('\n');
    text
}

/// The width and height in a PNG's header.
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((width, height))
}

/// Writes the site's files (not `pkg/`): returns the files written.
pub fn write(site: &Path, inputs: &Inputs) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(site)
        .map_err(|error| format!("cannot create {}: {error}", site.display()))?;
    let background = color(&inputs.background);
    let mut written = Vec::new();
    let mut put = |name: &str, bytes: &[u8]| -> Result<(), String> {
        let path = site.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        std::fs::write(&path, bytes)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        written.push(path);
        Ok(())
    };

    let icon = match &inputs.icon {
        Some(path) => {
            let bytes = std::fs::read(path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            let size = png_size(&bytes);
            put("icon.png", &bytes)?;
            size.or(Some((0, 0)))
        }
        None => None,
    };

    put(
        "index.html",
        index_html(&inputs.name, &background, icon.is_some()).as_bytes(),
    )?;
    put(
        "manifest.webmanifest",
        manifest(&inputs.name, &background, icon.filter(|s| s.0 > 0)).as_bytes(),
    )?;

    for file in resources(&inputs.project_dir, &inputs.resources)? {
        let relative = file
            .strip_prefix(&inputs.project_dir)
            .map_err(|_| format!("{} is outside the project", file.display()))?
            .to_path_buf();
        let target = site.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        let _ = std::fs::copy(&file, &target).map_err(|error| {
            format!(
                "cannot copy {} to {}: {error}",
                file.display(),
                target.display()
            )
        })?;
        written.push(target);
    }
    Ok(written)
}

/// The files `[app] resources` globs match, relative paths kept. A pattern
/// is a path relative to the project with `*` and `?` inside a component
/// and `**` for any number of directories; a directory matches everything
/// under it.
pub fn resources(project_dir: &Path, patterns: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for pattern in patterns {
        let path = Path::new(pattern);
        if path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
        {
            return Err(format!(
                "[app] resources: `{pattern}` must stay inside the project"
            ));
        }
        let parts: Vec<&str> = pattern
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
        let literal = parts
            .iter()
            .take_while(|part| !part.contains(['*', '?']))
            .count();
        let base = parts[..literal]
            .iter()
            .fold(project_dir.to_path_buf(), |dir, part| dir.join(part));

        if literal == parts.len() {
            if base.is_file() {
                files.push(base);
            } else if base.is_dir() {
                walk(&base, &mut files);
            }
            continue;
        }

        let mut candidates = Vec::new();
        walk(&base, &mut candidates);
        for file in candidates {
            let Ok(relative) = file.strip_prefix(&base) else {
                continue;
            };
            let names: Vec<String> = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            if glob_components(&parts[literal..], &names) {
                files.push(file);
            }
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = read_dir.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            walk(&entry.path(), files);
        } else if kind.is_file() {
            files.push(entry.path());
        }
    }
}

/// Matches path components against pattern components (`**` spans any
/// number of components).
pub fn glob_components(pattern: &[&str], names: &[&str]) -> bool {
    match (pattern.first(), names.first()) {
        (None, None) => true,
        (Some(&"**"), _) => {
            glob_components(&pattern[1..], names)
                || (!names.is_empty() && glob_components(pattern, &names[1..]))
        }
        (Some(part), Some(name)) => glob(part, name) && glob_components(&pattern[1..], &names[1..]),
        _ => false,
    }
}

/// Matches one component: `*` is any run of characters, `?` one character.
pub fn glob(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut p, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, n));
            p += 1;
        } else if let Some((star_p, star_n)) = star {
            p = star_p + 1;
            n = star_n + 1;
            star = Some((star_p, star_n + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_html_loads_the_app_and_escapes_the_title() {
        let html = index_html("Tom & <Jerry>", "#112233", true);
        assert!(html.contains("<title>Tom &amp; &lt;Jerry&gt;</title>"));
        assert!(html.contains("import init from \"./pkg/app.js\";"));
        assert!(html.contains("module_or_path: \"./pkg/app_bg.wasm\""));
        assert!(html.contains("background:#112233"));
        assert!(html.contains("href=\"icon.png\""));
        assert!(html.contains("/__icm/log"));
        let plain = index_html("A", "#FFFFFF", false);
        assert!(!plain.contains("icon.png"));
        assert!(plain.contains("href=\"data:,\""));
    }

    #[test]
    fn colors_are_validated() {
        assert_eq!(color("#a1b2c3"), "#A1B2C3");
        assert_eq!(color("red"), "#FFFFFF");
        assert_eq!(color("#12345"), "#FFFFFF");
    }

    #[test]
    fn globs_match() {
        assert!(glob("*.png", "icon.png"));
        assert!(glob("i?on.*", "icon.png"));
        assert!(!glob("*.png", "icon.jpg"));
        assert!(glob("*", ""));
        assert!(glob_components(&["**", "*.ttf"], &["a", "b", "x.ttf"]));
        assert!(glob_components(&["**", "*.ttf"], &["x.ttf"]));
        assert!(!glob_components(&["*.ttf"], &["a", "x.ttf"]));
    }

    #[test]
    fn the_site_gets_its_files_and_resources() {
        let project = tempfile::tempdir().unwrap();
        let assets = project.path().join("assets/fonts");
        std::fs::create_dir_all(&assets).unwrap();
        std::fs::write(assets.join("a.ttf"), "font").unwrap();
        std::fs::write(assets.join("notes.txt"), "no").unwrap();
        std::fs::write(project.path().join("data.json"), "{}").unwrap();
        let icon = project.path().join("icon.png");
        std::fs::write(
            &icon,
            crate::preview::encode(&crate::preview::Image::filled(4, 4, [1, 2, 3, 255])).unwrap(),
        )
        .unwrap();

        let site = tempfile::tempdir().unwrap();
        let inputs = Inputs {
            name: "Demo".into(),
            background: "#000000".into(),
            icon: Some(icon),
            project_dir: project.path().to_path_buf(),
            resources: vec!["assets/**/*.ttf".into(), "data.json".into()],
        };
        let written = write(site.path(), &inputs).unwrap();
        assert!(site.path().join("index.html").is_file());
        assert!(site.path().join("icon.png").is_file());
        assert!(site.path().join("assets/fonts/a.ttf").is_file());
        assert!(site.path().join("data.json").is_file());
        assert!(!site.path().join("assets/fonts/notes.txt").exists());
        assert_eq!(written.len(), 5);

        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(site.path().join("manifest.webmanifest")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["icons"][0]["sizes"], "4x4");
        assert_eq!(manifest["background_color"], "#000000");

        let escape_attempt = Inputs {
            resources: vec!["../secret".into()],
            ..inputs
        };
        assert!(write(site.path(), &escape_attempt).is_err());
    }
}
