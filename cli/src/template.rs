//! The template `icm new` copies (design §8, as built in §8.5).
//!
//! `build.rs` embeds the fork's `examples/app` and
//! `docs/agents/limitations.md`. [`render`] fills the template's
//! placeholders:
//!
//! - the package, library and binary name `app` (Cargo.toml, `app::` in
//!   `src/main.rs` and `tests/icm.rs`, icm.toml `package`/`lib`/`bin`);
//! - the display name `App` (icm.toml `name`, `.title("App")`);
//! - the id `com.example.app`;
//! - the path dependencies `path = "../.."` and `path = "../../test"`,
//!   which become the pinned framework source ([`Framework`]);
//! - AGENTS.md's `{{name}}`, `{{id}}`, `{{framework_tag}}`,
//!   `{{icm_version}}` and `{{limitations}}`.
//!
//! Every substitution must happen at least once; a template that changed
//! under icm's feet is an internal error, and the unit tests below catch it
//! first.

use crate::config;
use std::path::{Path, PathBuf};

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/template.rs"));
}

/// The marker line in `limitations.md` after which AGENTS.md embeds it.
const LIMITATIONS_MARKER: &str = "<!-- icm:";

/// The template's files, `(relative path, bytes)`, sorted by path.
pub fn files() -> &'static [(&'static str, &'static [u8])] {
    embedded::TEMPLATE_FILES
}

/// One template file.
pub fn file(path: &str) -> Option<&'static [u8]> {
    files()
        .iter()
        .find(|(name, _)| *name == path)
        .map(|(_, bytes)| *bytes)
}

/// The sha256 of the template's placeholder icon (`assets/icon.png`), which
/// `app.icon.placeholder` compares against (Appendix C item 12).
pub fn placeholder_icon_sha256() -> Option<String> {
    file("assets/icon.png").map(crate::hash::sha256_hex)
}

/// The known-limitations list AGENTS.md embeds: `limitations.md` after its
/// `<!-- icm: … -->` marker line.
pub fn limitations() -> &'static str {
    let text = embedded::LIMITATIONS;
    match text.find(LIMITATIONS_MARKER) {
        Some(start) => {
            let after = &text[start..];
            let body = after.split_once('\n').map_or("", |(_, rest)| rest);
            body.trim()
        }
        None => text.trim(),
    }
}

/// The framework source a new app pins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Framework {
    /// A release tag of the fork.
    Tag {
        /// The fork's git URL.
        url: String,
        /// The tag, e.g. `v0.14.1-mobile.1`.
        tag: String,
    },
    /// A commit of the fork.
    Rev {
        /// The fork's git URL.
        url: String,
        /// The commit hash.
        rev: String,
    },
    /// A local checkout of the fork.
    Path(PathBuf),
}

impl Framework {
    /// Parses `tag:<t>`, `rev:<sha>` or `path:<dir>`; `url` is the fork's
    /// git URL for the first two. A relative `path:` is taken from `cwd`.
    pub fn parse(spec: &str, url: &str, cwd: &Path) -> Result<Framework, String> {
        let spec = spec.trim();
        let (kind, value) = spec.split_once(':').ok_or_else(|| {
            format!("`{spec}` is not a framework source; use tag:<tag>, rev:<sha> or path:<dir>")
        })?;
        let value = value.trim();
        match kind {
            "tag" => {
                let ok = !value.is_empty()
                    && value
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'));
                if !ok {
                    return Err(format!("`{value}` is not a tag name"));
                }
                Ok(Framework::Tag {
                    url: url.to_string(),
                    tag: value.to_string(),
                })
            }
            "rev" => {
                let ok =
                    (7..=40).contains(&value.len()) && value.chars().all(|c| c.is_ascii_hexdigit());
                if !ok {
                    return Err(format!(
                        "`{value}` is not a commit hash (7 to 40 hex digits; prefer the full hash)"
                    ));
                }
                Ok(Framework::Rev {
                    url: url.to_string(),
                    rev: value.to_ascii_lowercase(),
                })
            }
            "path" => {
                if value.is_empty() {
                    return Err("path: needs a directory".to_string());
                }
                let expanded = crate::paths::expand_tilde(value);
                let absolute = if expanded.is_absolute() {
                    expanded
                } else {
                    cwd.join(expanded)
                };
                let dir = std::fs::canonicalize(&absolute)
                    .unwrap_or_else(|_| crate::paths::normalize(&absolute));
                check_checkout(&dir)?;
                Ok(Framework::Path(dir))
            }
            other => Err(format!(
                "`{other}:` is not a framework source kind; use tag:<tag>, rev:<sha> or path:<dir>"
            )),
        }
    }

    /// The `tag:`/`rev:`/`path:` form.
    pub fn spec(&self) -> String {
        match self {
            Framework::Tag { tag, .. } => format!("tag:{tag}"),
            Framework::Rev { rev, .. } => format!("rev:{rev}"),
            Framework::Path(dir) => format!("path:{}", dir.display()),
        }
    }

    /// How AGENTS.md names the framework version.
    pub fn label(&self) -> String {
        match self {
            Framework::Tag { tag, .. } => tag.clone(),
            Framework::Rev { rev, .. } => format!("rev {}", rev.get(..12).unwrap_or(rev)),
            Framework::Path(dir) => format!("from {}", dir.display()),
        }
    }

    /// The source keys of a dependency on the fork's root crate (`iced`) or
    /// one of its directories (`test` for `iced_test`).
    fn source(&self, subdir: Option<&str>) -> String {
        match self {
            Framework::Tag { url, tag } => {
                format!("git = {}, tag = {}", toml_string(url), toml_string(tag))
            }
            Framework::Rev { url, rev } => {
                format!("git = {}, rev = {}", toml_string(url), toml_string(rev))
            }
            Framework::Path(dir) => {
                let dir = match subdir {
                    Some(sub) => dir.join(sub),
                    None => dir.clone(),
                };
                format!("path = {}", toml_string(&dir.display().to_string()))
            }
        }
    }
}

/// Whether a directory is a checkout of the fork: the `iced` package at its
/// root and `iced_test` in `test/`.
fn check_checkout(dir: &Path) -> Result<(), String> {
    let package_name = |manifest: &Path| -> Option<String> {
        let text = std::fs::read_to_string(manifest).ok()?;
        let value: toml::Table = toml::from_str(&text).ok()?;
        value
            .get("package")?
            .get("name")?
            .as_str()
            .map(str::to_string)
    };
    match (
        package_name(&dir.join("Cargo.toml")).as_deref(),
        package_name(&dir.join("test").join("Cargo.toml")).as_deref(),
    ) {
        (Some("iced"), Some("iced_test")) => Ok(()),
        _ => Err(format!(
            "{} is not a checkout of iced_mobile (no `iced` package there, or no `iced_test` in test/)",
            dir.display()
        )),
    }
}

/// A TOML basic string.
pub fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The names a new app gets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Names {
    /// The Cargo package (and binary) name, e.g. `my-notes`.
    pub package: String,
    /// The library's crate name, e.g. `my_notes`.
    pub lib: String,
    /// The display name, e.g. `My Notes`.
    pub display: String,
    /// The reverse-DNS id, e.g. `com.example.my_notes`.
    pub id: String,
}

/// Package names that would clash with the app's own dependencies or with
/// Rust's built-in crates, or that Cargo refuses.
const RESERVED: &[&str] = &[
    "iced",
    "iced_test",
    "log",
    "std",
    "core",
    "alloc",
    "test",
    "proc_macro",
    "proc-macro",
    "build",
    "deps",
    "examples",
    "incremental",
    "as",
    "async",
    "await",
    "break",
    "const",
    "continue",
    "crate",
    "dyn",
    "else",
    "enum",
    "extern",
    "false",
    "fn",
    "for",
    "if",
    "impl",
    "in",
    "let",
    "loop",
    "match",
    "mod",
    "move",
    "mut",
    "pub",
    "ref",
    "return",
    "self",
    "static",
    "struct",
    "super",
    "trait",
    "true",
    "type",
    "unsafe",
    "use",
    "where",
    "while",
    "abstract",
    "become",
    "box",
    "do",
    "final",
    "gen",
    "macro",
    "override",
    "priv",
    "try",
    "typeof",
    "unsized",
    "virtual",
    "yield",
];

impl Names {
    /// The names for an app in a directory called `dir_name`, with an
    /// optional display name and id.
    pub fn new(dir_name: &str, display: Option<&str>, id: Option<&str>) -> Result<Names, String> {
        let package = package_name(dir_name)?;
        let lib = package.replace('-', "_");

        let display = match display {
            Some(name) => {
                let name = name.trim();
                if name.is_empty() {
                    return Err("--name must not be empty".to_string());
                }
                if name.chars().any(char::is_control) {
                    return Err("--name must not contain control characters".to_string());
                }
                name.to_string()
            }
            None => display_name(dir_name),
        };

        let id = match id {
            Some(id) => id.trim().to_string(),
            None => format!("com.example.{lib}"),
        };
        config::validate_app_id(&id).map_err(|message| format!("--id `{id}` {message}"))?;

        Ok(Names {
            package,
            lib,
            display,
            id,
        })
    }
}

/// A Cargo package name from a directory name: lower case, runs of
/// anything but letters, digits, `-` and `_` become one `-`.
pub fn package_name(dir_name: &str) -> Result<String, String> {
    let mut name = String::new();
    for c in dir_name.trim().chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            name.push(c);
        } else if !name.ends_with('-') {
            name.push('-');
        }
    }
    let name = name.trim_matches(['-', '_']).to_string();

    if name.is_empty() {
        return Err(format!(
            "cannot make a package name from the directory name `{dir_name}`; use a name with letters"
        ));
    }
    if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(format!(
            "the package name `{name}` (from the directory name) must start with a letter"
        ));
    }
    if RESERVED.contains(&name.as_str()) || RESERVED.contains(&name.replace('-', "_").as_str()) {
        return Err(format!(
            "`{name}` cannot be the app's package name (it clashes with a Rust keyword, a built-in \
             crate or one of the app's dependencies); choose another directory name"
        ));
    }
    Ok(name)
}

/// A display name from a directory name: words split at `-`, `_`, `.` and
/// spaces, each capitalised.
pub fn display_name(dir_name: &str) -> String {
    let words: Vec<String> = dir_name
        .split(['-', '_', '.', ' '])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect();
    if words.is_empty() {
        "App".to_string()
    } else {
        words.join(" ")
    }
}

/// Fills the template. Returns `(relative path, bytes)` for every file.
pub fn render(names: &Names, framework: &Framework) -> Result<Vec<(String, Vec<u8>)>, String> {
    if files().is_empty() {
        return Err(
            "this icm was built without the template (examples/app was not next to cli/ at build time)"
                .to_string(),
        );
    }

    let mut out = Vec::with_capacity(files().len());
    for (path, bytes) in files() {
        let rendered = match *path {
            "Cargo.toml" => cargo_toml(text(path, bytes)?, names, framework)?.into_bytes(),
            "icm.toml" => icm_toml(text(path, bytes)?, names)?.into_bytes(),
            "src/main.rs" | "tests/icm.rs" => replace_all(
                path,
                text(path, bytes)?,
                "app::",
                &format!("{}::", names.lib),
            )?
            .into_bytes(),
            "src/lib.rs" => replace_all(
                path,
                text(path, bytes)?,
                ".title(\"App\")",
                &format!(".title({:?})", names.display),
            )?
            .into_bytes(),
            "AGENTS.md" => agents_md(text(path, bytes)?, names, framework)?.into_bytes(),
            _ => bytes.to_vec(),
        };
        out.push(((*path).to_string(), rendered));
    }
    Ok(out)
}

fn text<'a>(path: &str, bytes: &'a [u8]) -> Result<&'a str, String> {
    std::str::from_utf8(bytes).map_err(|_| format!("template file {path} is not UTF-8"))
}

/// Replaces every occurrence, and fails when there is none.
fn replace_all(path: &str, text: &str, from: &str, to: &str) -> Result<String, String> {
    if !text.contains(from) {
        return Err(format!(
            "template file {path} no longer contains `{from}`; icm new cannot fill it"
        ));
    }
    Ok(text.replace(from, to))
}

fn cargo_toml(text: &str, names: &Names, framework: &Framework) -> Result<String, String> {
    let text = replace_all(
        "Cargo.toml",
        text,
        "name = \"app\"",
        &format!("name = {}", toml_string(&names.package)),
    )?;
    let text = replace_all(
        "Cargo.toml",
        &text,
        "path = \"../../test\"",
        &framework.source(Some("test")),
    )?;
    let text = replace_all(
        "Cargo.toml",
        &text,
        "path = \"../..\"",
        &framework.source(None),
    )?;

    // Nothing may still point into the fork's tree.
    if let Some(line) = text.lines().find(|line| line.contains("path = \"../")) {
        return Err(format!(
            "the template's Cargo.toml has a path dependency icm does not rewrite: {line}"
        ));
    }
    let parsed: toml::Table = toml::from_str(&text)
        .map_err(|error| format!("the rendered Cargo.toml does not parse: {error}"))?;
    if parsed
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(toml::Value::as_str)
        != Some(names.package.as_str())
    {
        return Err("the rendered Cargo.toml has the wrong package name".to_string());
    }
    Ok(text)
}

fn icm_toml(text: &str, names: &Names) -> Result<String, String> {
    let mut seen = [false; 5];
    let keys = [
        ("name", toml_string(&names.display)),
        ("id", toml_string(&names.id)),
        ("package", toml_string(&names.package)),
        ("lib", toml_string(&names.lib)),
        ("bin", toml_string(&names.package)),
    ];

    let mut section = String::new();
    let mut out = String::with_capacity(text.len() + 64);
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            section = trimmed
                .split(']')
                .next()
                .unwrap_or("")
                .trim_start_matches('[')
                .trim()
                .to_string();
        }
        let mut replaced = None;
        if section == "app" {
            for (index, (key, value)) in keys.iter().enumerate() {
                if let Some(new_line) = set_value(line, key, value) {
                    seen[index] = true;
                    replaced = Some(new_line);
                    break;
                }
            }
        }
        out.push_str(replaced.as_deref().unwrap_or(line));
    }

    if let Some(index) = seen.iter().position(|seen| !seen) {
        return Err(format!(
            "the template's icm.toml has no `[app] {} = …` line; icm new cannot fill it",
            keys[index].0
        ));
    }
    Ok(out)
}

/// Replaces the value of `key = "…"` on a line, keeping a trailing comment
/// in its column when the new value leaves room.
fn set_value(line: &str, key: &str, value: &str) -> Option<String> {
    let rest = line.strip_prefix(&format!("{key} = "))?;
    if !rest.starts_with('"') {
        return None;
    }
    let close = rest[1..].find('"')? + 2;
    let old_value = &rest[..close];
    let after = &rest[close..];
    let prefix_len = key.len() + 3;

    let trimmed = after.trim_start_matches(' ');
    if trimmed.starts_with('#') {
        let column = prefix_len + old_value.len() + (after.len() - trimmed.len());
        let used = prefix_len + value.len();
        let pad = if column > used { column - used } else { 1 };
        Some(format!("{key} = {value}{}{trimmed}", " ".repeat(pad)))
    } else {
        Some(format!("{key} = {value}{after}"))
    }
}

fn agents_md(text: &str, names: &Names, framework: &Framework) -> Result<String, String> {
    let limitations = limitations();
    let limitations = if limitations.is_empty() {
        "(this icm was built without docs/agents/limitations.md)"
    } else {
        limitations
    };
    let mut text = text.to_string();
    for (placeholder, value) in [
        ("{{name}}", names.display.clone()),
        ("{{id}}", names.id.clone()),
        ("{{framework_tag}}", framework.label()),
        ("{{icm_version}}", crate::buildinfo::VERSION.to_string()),
        ("{{limitations}}", limitations.to_string()),
    ] {
        text = replace_all("AGENTS.md", &text, placeholder, &value)?;
    }
    if let Some(start) = text.find("{{") {
        let end = text[start..]
            .find("}}")
            .map_or(text.len(), |e| start + e + 2);
        return Err(format!(
            "AGENTS.md has a placeholder icm does not know: {}",
            &text[start..end]
        ));
    }
    Ok(text)
}

/// One key of icm.toml as the template documents it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigKey {
    /// The dotted key, e.g. `app.id`.
    pub key: String,
    /// The template's line for it (commented out when the template leaves
    /// it unset), without its comment.
    pub line: String,
    /// The template's comment for it.
    pub comment: String,
    /// Whether the template leaves it commented out.
    pub commented_out: bool,
    /// Whether it is a table (`[app.permissions]`).
    pub table: bool,
}

/// Every key the template's icm.toml documents, in file order: its tables
/// and their keys, commented-out ones included (`icm explain config.<key>`).
pub fn config_keys() -> Vec<ConfigKey> {
    let Some(text) = file("icm.toml").and_then(|bytes| std::str::from_utf8(bytes).ok()) else {
        return Vec::new();
    };
    // The template pads every comment with at least two spaces.
    let split_comment = |line: &str| -> (String, String) {
        match line.find("  #") {
            Some(index) => (
                line[..index].trim_end().to_string(),
                line[index..]
                    .trim()
                    .trim_start_matches('#')
                    .trim()
                    .to_string(),
            ),
            None => (line.trim_end().to_string(), String::new()),
        }
    };
    let key_of = |text: &str| -> Option<String> {
        let (key, _) = text.split_once(" = ")?;
        let key = key.trim();
        (!key.is_empty()
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .then(|| key.to_string())
    };

    let mut section = String::new();
    let mut keys: Vec<ConfigKey> = Vec::new();
    for raw in text.lines() {
        let trimmed = raw.trim();
        if trimmed.starts_with('[') && !trimmed.starts_with("[[") {
            let (header, comment) = split_comment(trimmed);
            section = header.trim_matches(['[', ']']).trim().to_string();
            keys.push(ConfigKey {
                key: section.clone(),
                line: header,
                comment,
                commented_out: false,
                table: true,
            });
            continue;
        }
        let (body, commented_out) = match trimmed.strip_prefix("# ") {
            Some(rest) => (rest, true),
            None => (trimmed, false),
        };
        let Some(key) = key_of(body) else {
            continue;
        };
        let (line, comment) = split_comment(body);
        let dotted = if section.is_empty() {
            key
        } else {
            format!("{section}.{key}")
        };
        if !keys.iter().any(|k| k.key == dotted) {
            keys.push(ConfigKey {
                key: dotted,
                line: if commented_out {
                    format!("# {line}")
                } else {
                    line
                },
                comment,
                commented_out,
                table: false,
            });
        }
    }
    keys
}

/// The `icm explain config.<key>` doc for an icm.toml key, from the
/// template's annotated icm.toml.
pub fn config_key_doc(key: &str) -> Option<String> {
    let keys = config_keys();
    let entry = keys.iter().find(|k| k.key == key)?;
    let mut doc = format!("# config.{key}\n\n");
    match entry.key.rsplit_once('.') {
        _ if entry.table => {
            doc.push_str(&format!("The `[{}]` table of icm.toml.\n\n", entry.key));
        }
        Some((table, name)) => {
            doc.push_str(&format!(
                "`{name}` in the `[{table}]` table of icm.toml.\n\n"
            ));
        }
        None => doc.push_str(&format!(
            "The top-level `{}` key of icm.toml.\n\n",
            entry.key
        )),
    }
    if !entry.comment.is_empty() {
        let mut chars = entry.comment.trim_end_matches('.').chars();
        let sentence: String = chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default();
        doc.push_str(&format!("{sentence}.\n\n"));
    }
    doc.push_str(if entry.commented_out {
        "A new app leaves it unset:\n\n"
    } else {
        "In a new app:\n\n"
    });
    doc.push_str(&format!("```toml\n{}\n```\n", entry.line));
    if entry.table {
        let inner: Vec<String> = keys
            .iter()
            .filter(|k| {
                !k.table
                    && k.key
                        .rsplit_once('.')
                        .is_some_and(|(table, _)| table == entry.key)
            })
            .map(|k| format!("`config.{}`", k.key))
            .collect();
        if !inner.is_empty() {
            doc.push_str(&format!("\nKeys: {}\n", inner.join(", ")));
        }
    }
    doc.push_str(
        "\n`icm print config` shows the resolved values. An unknown or mistyped key is \
         `config.unknown_key` or `config.invalid` (exit 3) at its file:line.\n",
    );
    Some(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_keys_are_documented_from_the_template() {
        let keys = config_keys();
        for key in [
            "schema",
            "min_icm",
            "app",
            "app.id",
            "app.permissions",
            "app.permissions.internet",
            "ios.min_os",
            "ios.team_id",
            "android.target_sdk",
            "web.public_url",
            "desktop.macos.min_os",
            "test.viewports",
        ] {
            assert!(keys.iter().any(|k| k.key == key), "{key} is not documented");
        }
        let doc = config_key_doc("app.id").unwrap();
        assert!(doc.starts_with("# config.app.id\n"), "{doc}");
        assert!(doc.contains("placeholder"), "{doc}");
        assert!(doc.contains("id = \"com.example.app\""), "{doc}");
        let team = config_key_doc("ios.team_id").unwrap();
        assert!(team.contains("leaves it unset"), "{team}");
        let table = config_key_doc("app.permissions").unwrap();
        assert!(
            table.contains("`config.app.permissions.internet`"),
            "{table}"
        );
        assert!(config_key_doc("app.nope").is_none());
    }

    fn fork_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn rendered(
        names: &Names,
        framework: &Framework,
    ) -> std::collections::BTreeMap<String, String> {
        render(names, framework)
            .unwrap()
            .into_iter()
            .filter_map(|(path, bytes)| String::from_utf8(bytes).ok().map(|text| (path, text)))
            .collect()
    }

    #[test]
    fn the_template_is_embedded() {
        for path in [
            "Cargo.toml",
            "icm.toml",
            "src/lib.rs",
            "src/main.rs",
            "tests/icm.rs",
            "AGENTS.md",
            "assets/icon.png",
            "rust-toolchain.toml",
            ".gitignore",
        ] {
            assert!(file(path).is_some(), "{path} is not embedded");
        }
        assert!(!files().iter().any(|(path, _)| path.starts_with("target/")));
        assert_eq!(placeholder_icon_sha256().unwrap().len(), 64);
        assert!(limitations().starts_with("- **"), "{}", limitations());
        assert!(!limitations().contains("<!-- icm"));
    }

    /// Rendering with the template's own names and `path:../..` gives the
    /// template back, byte for byte, except AGENTS.md's placeholders and the
    /// absolute paths: every substitution is complete and nothing else
    /// changes (CI's template-drift check, design §17 item 9).
    #[test]
    fn rendering_with_the_template_names_is_the_identity() {
        let names = Names {
            package: "app".into(),
            lib: "app".into(),
            display: "App".into(),
            id: "com.example.app".into(),
        };
        let root = fork_root();
        let framework = Framework::Path(root.clone());
        for (path, bytes) in render(&names, &framework).unwrap() {
            let original = file(&path).unwrap();
            match path.as_str() {
                "Cargo.toml" => {
                    let text = String::from_utf8(bytes).unwrap();
                    let normalized = text
                        .replace(
                            &format!(
                                "path = {}",
                                toml_string(&root.join("test").display().to_string())
                            ),
                            "path = \"../../test\"",
                        )
                        .replace(
                            &format!("path = {}", toml_string(&root.display().to_string())),
                            "path = \"../..\"",
                        );
                    assert_eq!(normalized.as_bytes(), original, "Cargo.toml");
                }
                "AGENTS.md" => {
                    let text = String::from_utf8(bytes).unwrap();
                    assert!(
                        text.starts_with("# AGENTS.md — App (com.example.app) · iced_mobile from ")
                    );
                    assert!(!text.contains("{{"));
                }
                _ => assert_eq!(bytes, original, "{path}"),
            }
        }
    }

    #[test]
    fn a_tagged_app_pins_every_iced_line() {
        let names = Names::new("my-notes", None, None).unwrap();
        assert_eq!(names.package, "my-notes");
        assert_eq!(names.lib, "my_notes");
        assert_eq!(names.display, "My Notes");
        assert_eq!(names.id, "com.example.my_notes");

        let framework = Framework::Tag {
            url: "https://github.com/patricksmithlaravel/iced_mobile".into(),
            tag: "v0.14.1-mobile.3".into(),
        };
        let files = rendered(&names, &framework);

        let cargo = &files["Cargo.toml"];
        let parsed: toml::Table = toml::from_str(cargo).unwrap();
        assert_eq!(parsed["package"]["name"].as_str(), Some("my-notes"));
        assert_eq!(parsed["bin"][0]["name"].as_str(), Some("my-notes"));
        assert_eq!(parsed["test"][0]["name"].as_str(), Some("icm"));
        let iced_lines: Vec<&str> = cargo
            .lines()
            .filter(|line| line.starts_with("iced"))
            .collect();
        assert_eq!(iced_lines.len(), 3, "{cargo}");
        for line in iced_lines {
            assert!(
                line.contains(
                    "git = \"https://github.com/patricksmithlaravel/iced_mobile\", tag = \"v0.14.1-mobile.3\""
                ),
                "{line}"
            );
            assert!(!line.contains("path"), "{line}");
        }
        assert_eq!(
            parsed["dependencies"]["iced"]["features"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        assert!(files["src/main.rs"].contains("my_notes::run()"));
        assert!(files["tests/icm.rs"].contains("my_notes::application()"));
        assert!(files["src/lib.rs"].contains(".title(\"My Notes\")"));

        let icm = &files["icm.toml"];
        let loaded = crate::config::parse(Path::new("/x/icm.toml"), icm).unwrap();
        assert_eq!(loaded.config.app.name, "My Notes");
        assert_eq!(loaded.config.app.id, "com.example.my_notes");
        assert_eq!(loaded.config.app.package.as_deref(), Some("my-notes"));
        assert_eq!(loaded.config.app.lib.as_deref(), Some("my_notes"));
        assert_eq!(loaded.config.app.bin.as_deref(), Some("my-notes"));
        // Comments stay in their column.
        let line = icm.lines().find(|l| l.starts_with("name = ")).unwrap();
        assert_eq!(line.find('#'), Some(39), "{line}");

        let agents = &files["AGENTS.md"];
        assert!(agents.starts_with(&format!(
            "# AGENTS.md — My Notes (com.example.my_notes) · iced_mobile v0.14.1-mobile.3 · icm {}",
            crate::buildinfo::VERSION
        )));
        assert!(agents.contains("No safe-area insets"));
        assert!(!agents.contains("{{"));
    }

    #[test]
    fn revs_and_odd_names_render() {
        let names = Names::new(
            "Demo App",
            Some("Fancy \"Quoted\" \\ App"),
            Some("dev.acme.demo"),
        )
        .unwrap();
        assert_eq!(names.package, "demo-app");
        let framework = Framework::Rev {
            url: "https://github.com/x/iced_mobile".into(),
            rev: "2571bdd35a1b2c3d4e5f60718293a4b5c6d7e8f9".into(),
        };
        let files = rendered(&names, &framework);
        assert!(files["Cargo.toml"].contains(
            "git = \"https://github.com/x/iced_mobile\", rev = \"2571bdd35a1b2c3d4e5f60718293a4b5c6d7e8f9\""
        ));
        assert!(files["src/lib.rs"].contains(r#".title("Fancy \"Quoted\" \\ App")"#));
        let loaded = crate::config::parse(Path::new("/x/icm.toml"), &files["icm.toml"]).unwrap();
        assert_eq!(loaded.config.app.name, "Fancy \"Quoted\" \\ App");
        assert_eq!(loaded.config.app.id, "dev.acme.demo");
        assert!(files["AGENTS.md"].contains("iced_mobile rev 2571bdd35a1b ·"));
    }

    #[test]
    fn framework_sources_parse() {
        let cwd = fork_root();
        assert_eq!(
            Framework::parse("tag:v0.14.1-mobile.1", "u", &cwd).unwrap(),
            Framework::Tag {
                url: "u".into(),
                tag: "v0.14.1-mobile.1".into()
            }
        );
        assert!(Framework::parse("rev:2571bdd", "u", &cwd).is_ok());
        assert!(Framework::parse("rev:xyz", "u", &cwd).is_err());
        assert!(Framework::parse("tag:", "u", &cwd).is_err());
        assert!(Framework::parse("branch:main", "u", &cwd).is_err());
        assert!(Framework::parse("v0.14", "u", &cwd).is_err());

        let path = Framework::parse("path:.", "u", &cwd).unwrap();
        assert_eq!(path, Framework::Path(std::fs::canonicalize(&cwd).unwrap()));
        assert!(path.spec().starts_with("path:/"));
        let not_fork = Framework::parse("path:cli", "u", &cwd).unwrap_err();
        assert!(not_fork.contains("not a checkout"), "{not_fork}");
    }

    #[test]
    fn package_names_come_from_directories() {
        assert_eq!(package_name("demo").unwrap(), "demo");
        assert_eq!(package_name("My Notes!").unwrap(), "my-notes");
        assert_eq!(package_name("notes_app").unwrap(), "notes_app");
        assert!(package_name("123").is_err());
        assert!(package_name("iced").is_err());
        assert!(package_name("test").is_err());
        assert!(package_name("---").is_err());
        assert_eq!(display_name("my-notes"), "My Notes");
        assert_eq!(display_name("demo"), "Demo");
        assert!(Names::new("demo", None, Some("not an id")).is_err());
        assert!(Names::new("demo", Some("  "), None).is_err());
    }

    #[test]
    fn values_keep_their_comment_column() {
        let line = "name = \"App\"                           # the name\n";
        assert_eq!(
            set_value(line, "name", "\"Notes\""),
            Some("name = \"Notes\"                         # the name\n".to_string())
        );
        let long = set_value(line, "name", &format!("\"{}\"", "x".repeat(40))).unwrap();
        assert!(
            long.contains(&format!("{}\" # the name", "x".repeat(40))),
            "{long}"
        );
        assert_eq!(
            set_value("id = \"a\"\n", "id", "\"b\""),
            Some("id = \"b\"\n".to_string())
        );
        assert_eq!(set_value("identity = \"auto\"", "id", "\"b\""), None);
    }
}
