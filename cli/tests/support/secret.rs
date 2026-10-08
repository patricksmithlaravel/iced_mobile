//! The secret of the redaction tests, and a search of the files a command
//! kept for it in every form a log, an encoder or a URL can give it.
//! Test files include it with `#[path = "support/secret.rs"] mod secret;`.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// The variable icm's environment holds it in (the redaction rule's
/// `*TOKEN*`).
pub const NAME: &str = "ICM_TEST_API_TOKEN";

/// The value: JSON escapes its `"` and `\`, and Apple's `log` its `/`.
pub const TOKEN: &str = "tok/se\"kr\\it-123456";

/// The end of the value, which no escape or encoding changes: a file that
/// holds it holds the secret in some form, however many times escaped.
pub const TAIL: &str = "it-123456";

/// A secret-named variable of the user's shell that icm inherits (and a
/// desktop app with it) but that no app logs: another tool's token. No
/// command may write its value to any file under `target/`.
pub const INHERITED_NAME: &str = "ICM_TEST_SHELL_TOKEN";

/// Its value, which JSON escapes too.
pub const INHERITED: &str = "sh/ell\"to\\ken-only-987654";

/// The end of [`INHERITED`], which no escape or encoding changes.
pub const INHERITED_TAIL: &str = "only-987654";

/// A text inside a JSON string, as serde and JavaScript write it.
fn escaped(text: &str) -> String {
    let quoted = serde_json::to_string(text).unwrap();
    quoted[1..quoted.len() - 1].to_string()
}

/// The forms a file could hold the secret in: raw, JSON-escaped (with `/`
/// as `\/` too), escaped twice (a JSON line in a JSON record),
/// percent-encoded (a URL's query), and any other that keeps [`TAIL`].
pub fn forms() -> Vec<String> {
    forms_of(TOKEN, TAIL)
}

/// [`forms`] of a value whose end is `tail`.
fn forms_of(value: &str, tail: &str) -> Vec<String> {
    let once = escaped(value);
    let percent: String = value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect();
    vec![
        value.to_string(),
        once.replace('/', "\\/"),
        escaped(&once),
        once,
        percent,
        tail.to_string(),
    ]
}

/// The files under `dir` (recursively) that hold the secret, with the form
/// each holds.
pub fn leaks(dir: &Path) -> Vec<(PathBuf, String)> {
    leaks_of(dir, &forms())
}

/// The files under `dir` (recursively) that hold one of `forms`, with the
/// form each holds.
fn leaks_of(dir: &Path, forms: &[String]) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                stack.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                let text = String::from_utf8_lossy(&bytes);
                if let Some(form) = forms.iter().find(|form| text.contains(form.as_str())) {
                    found.push((path, form.clone()));
                }
            }
        }
    }
    found
}

/// Panics, naming the files, when a file under `dir` holds the secret.
pub fn assert_kept_nowhere(dir: &Path) {
    assert_none("the secret", &leaks(dir));
}

/// Panics, naming the files, when a file under `dir` (a project's whole
/// `target/`) holds [`INHERITED`] in any form: what icm only inherited
/// from its environment, no command may write.
pub fn assert_inherited_nowhere(dir: &Path) {
    assert!(dir.is_dir(), "{} is not a directory", dir.display());
    let forms = forms_of(INHERITED, INHERITED_TAIL);
    assert_none(
        &format!("the inherited {INHERITED_NAME}"),
        &leaks_of(dir, &forms),
    );
}

fn assert_none(what: &str, leaks: &[(PathBuf, String)]) {
    assert!(
        leaks.is_empty(),
        "{what} is in {}",
        leaks
            .iter()
            .map(|(path, form)| format!("{} (as {form})", path.display()))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// Whether a file holds the secret in one of its forms (an app's own live
/// output does).
pub fn holds(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .is_ok_and(|text| forms().iter().any(|form| text.contains(form.as_str())))
}
