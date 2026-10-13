//! The user's keychain search list, and codesign's `no identity found` for a
//! signing keychain outside it.
//!
//! codesign searches only the keychains of the user's keychain search list
//! (`security list-keychains -d user`). `--keychain` narrows that search to
//! one keychain of the list; it does not add the keychain to it (Apple DTS,
//! https://developer.apple.com/forums/thread/728073). So a signing keychain
//! (host.toml `signing_keychain` or `ICM_KEYCHAIN`) that is not in the list
//! makes codesign fail with `<sha1>: no identity found` on macOS 26, although
//! `security find-identity -p codesigning <keychain>` lists the identity.
//! macOS 27.0.1's codesign found it.
//!
//! icm reads the list only after codesign has failed that way, and only to
//! report it. No release checks the list before codesign runs, so a Mac whose
//! codesign finds the identity is never affected, and icm never changes the
//! list. The error ([`not_listed`]) names the keychain and gives the
//! `security list-keychains -d user -s` commands that append it for the
//! release and put the list back afterwards. The iOS and macOS signing paths
//! both use it (`ios/codesign.rs`, `release/macos.rs`).
//!
//! Every other outcome keeps the failure's own id: codesign failing for
//! another reason, a list that cannot be read, and an empty list. icm prints
//! the restore command from the paths it read, so an empty list has no
//! command to give, and icm does not guess one.

use crate::catalogue::CheckId;
use crate::context::Ctx;
use crate::error::{Evidence, IcmError};
use crate::process::{Cmd, Outcome, shell_quote};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// codesign's message when none of the keychains it searches holds the identity.
const NO_IDENTITY: &str = "no identity found";

/// `security list-keychains -d user`: the user's keychain search list. icm
/// only reads it; it never runs `-s`, which changes the list.
fn list_cmd() -> Cmd {
    Cmd::tool("security")
        .args(["list-keychains", "-d", "user"])
        .timeout(Duration::from_secs(30))
}

/// The paths `security list-keychains` prints: one quoted path per line.
/// Other lines are not paths and are skipped.
fn parse_list(text: &str) -> Vec<PathBuf> {
    text.lines()
        .filter_map(|line| {
            let path = line.trim().strip_prefix('"')?.strip_suffix('"')?;
            (!path.is_empty()).then(|| PathBuf::from(path))
        })
        .collect()
}

/// The user's keychain search list, with at least one path. `None` when
/// `security` fails or its listing has no quoted path, an empty list
/// included: icm cannot then say that a keychain is outside it, so the
/// failure keeps its own id.
fn search_list(ctx: &Ctx) -> Option<Vec<PathBuf>> {
    let outcome = ctx.probe(&list_cmd()).ok()?;
    if !outcome.success() {
        return None;
    }
    let list = parse_list(&outcome.stdout_text());
    (!list.is_empty()).then_some(list)
}

/// `path` as an absolute path, against icm's working directory. codesign and
/// `security` run in that directory (no `cwd` is set on them), so this is the
/// file they open for a relative path, and the commands the error prints work
/// from any directory. Nothing is resolved on disk, so `..` stays as written.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether `keychain` is one of `list`: the same path, or the same file once
/// symbolic links are followed. A relative `keychain` is taken against icm's
/// directory, as [`absolute`] does.
fn is_listed(list: &[PathBuf], keychain: &Path) -> bool {
    let keychain = absolute(keychain);
    let resolved = std::fs::canonicalize(&keychain).ok();
    list.iter().any(|entry| {
        *entry == keychain
            || resolved
                .as_ref()
                .is_some_and(|target| std::fs::canonicalize(entry).ok().as_ref() == Some(target))
    })
}

/// Whether codesign's output says that no identity was found.
fn says_no_identity(outcome: &Outcome) -> bool {
    outcome.stdout_text().contains(NO_IDENTITY) || outcome.stderr_text().contains(NO_IDENTITY)
}

/// The error for a codesign that failed with `no identity found` while
/// `keychain`, the signing keychain, is outside the user's search list.
///
/// `None` for every other failure: no signing keychain was given, codesign
/// named another cause, the search list could not be read or is empty, or
/// the keychain is listed (the failure then keeps its own id). `identity` is
/// the certificate's SHA-1; `id` is the owner's id for the platform.
pub fn outside_search_list(
    ctx: &Ctx,
    id: CheckId,
    keychain: Option<&Path>,
    identity: &str,
    outcome: &Outcome,
) -> Option<IcmError> {
    let keychain = keychain?;
    if !says_no_identity(outcome) {
        return None;
    }
    let list = search_list(ctx)?;
    if is_listed(&list, keychain) {
        return None;
    }
    Some(not_listed(id, keychain, identity, &list, outcome))
}

/// The owner's error for a signing keychain outside the search list `list`,
/// which holds at least one keychain ([`search_list`]). The keychain is named
/// by its absolute path ([`absolute`]), in the detail and in the commands, so
/// a relative `signing_keychain` or `ICM_KEYCHAIN` gives the same text from
/// any directory.
fn not_listed(
    id: CheckId,
    keychain: &Path,
    identity: &str,
    list: &[PathBuf],
    outcome: &Outcome,
) -> IcmError {
    let keychain = absolute(keychain);
    let shown = keychain.display().to_string();
    let holds: Vec<String> = list.iter().map(|path| path.display().to_string()).collect();
    let detail = format!(
        "codesign did not find the identity {identity} in {shown}. On this Mac codesign searches only the keychains of the user's keychain search list, and --keychain only narrows that search, so a keychain outside the list is never searched. {shown} is not in the list, which holds {}.",
        holds.join(", ")
    );

    // The commands use the paths as the list holds them, so restoring the
    // list is the same command without the keychain.
    let listed: Vec<String> = list
        .iter()
        .map(|path| shell_quote(&path.display().to_string()))
        .collect();
    let mut appended = listed.clone();
    appended.push(shell_quote(&shown));
    let append = format!("security list-keychains -d user -s {}", appended.join(" "));
    let restore = format!("security list-keychains -d user -s {}", listed.join(" "));

    let mut error = IcmError::new(id, detail).fix(
        "Append the keychain to the user's keychain search list (the first command), run the release again, then put the list back (the last command). icm never changes the list itself.",
        &[append.as_str(), restore.as_str()],
    );
    if let Some(log) = &outcome.log {
        error = error.evidence(Evidence::file(log).with_excerpt(outcome.stderr_tail(1)));
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::By;
    use crate::exit::Exit;
    use crate::process::End;

    const LIST: &str = "    \"/Users/acme/Library/Keychains/login.keychain-db\"\n    \"/Users/acme/Library/Keychains/Build Keys.keychain-db\"\n";

    fn failed(stdout: &str, stderr: &str, log: Option<&Path>) -> Outcome {
        Outcome {
            end: End::Exited(1),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
            duration: Duration::from_millis(5),
            log: log.map(Path::to_path_buf),
            stdout_path: None,
        }
    }

    #[test]
    fn the_list_is_read_from_its_quoted_lines() {
        assert_eq!(
            parse_list(LIST),
            vec![
                PathBuf::from("/Users/acme/Library/Keychains/login.keychain-db"),
                PathBuf::from("/Users/acme/Library/Keychains/Build Keys.keychain-db"),
            ]
        );
        // Text that is not a quoted path lists nothing, so an unreadable
        // listing is never taken for a list that lacks the keychain.
        assert!(parse_list("login.keychain-db\n\n   \n").is_empty());
        assert!(parse_list("    \"\"\n").is_empty());
    }

    #[test]
    fn a_keychain_is_listed_by_its_path_or_by_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("build.keychain-db");
        std::fs::write(&real, "").unwrap();
        let alias = dir.path().join("alias.keychain-db");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        assert!(is_listed(std::slice::from_ref(&alias), &real));
        assert!(is_listed(std::slice::from_ref(&real), &alias));
        assert!(!is_listed(
            &[PathBuf::from("/Users/acme/login.keychain-db")],
            &real
        ));
        // A keychain that does not exist is listed by the same path only.
        let missing = dir.path().join("missing.keychain-db");
        assert!(is_listed(std::slice::from_ref(&missing), &missing));
        assert!(!is_listed(std::slice::from_ref(&real), &missing));
    }

    #[test]
    fn only_codesign_no_identity_is_read_as_the_search_list() {
        assert!(says_no_identity(&failed(
            "",
            "2222222222222222222222222222222222222222: no identity found\n",
            None
        )));
        assert!(says_no_identity(&failed(
            "1: no identity found\n",
            "",
            None
        )));
        assert!(!says_no_identity(&failed(
            "",
            "codesign: resource fork, Finder information, or similar detritus not allowed\n",
            None
        )));
    }

    #[test]
    fn the_error_names_the_keychain_and_gives_the_commands() {
        let keychain = Path::new("/ci/Build Keys.keychain-db");
        let list = parse_list(LIST);
        let outcome = failed(
            "",
            "3333333333333333333333333333333333333333: no identity found\n",
            Some(Path::new("/run/codesign.log")),
        );
        let error = not_listed(
            CheckId::MacosSignKeychainNotInSearchList,
            keychain,
            "3333333333333333333333333333333333333333",
            &list,
            &outcome,
        );
        assert_eq!(error.id, "macos.sign.keychain_not_in_search_list");
        assert_eq!(error.exit, Exit::NeedsOwner);
        assert_eq!(error.fix.by, By::Owner);
        assert!(
            error.detail.contains("/ci/Build Keys.keychain-db"),
            "{}",
            error.detail
        );
        assert!(
            error
                .detail
                .contains("searches only the keychains of the user's keychain search list"),
            "{}",
            error.detail
        );
        assert!(
            error.detail.contains("--keychain only narrows that search"),
            "{}",
            error.detail
        );
        assert!(
            error
                .fix
                .summary
                .contains("icm never changes the list itself")
        );
        assert_eq!(
            error.fix.commands,
            [
                "security list-keychains -d user -s /Users/acme/Library/Keychains/login.keychain-db '/Users/acme/Library/Keychains/Build Keys.keychain-db' '/ci/Build Keys.keychain-db'",
                "security list-keychains -d user -s /Users/acme/Library/Keychains/login.keychain-db '/Users/acme/Library/Keychains/Build Keys.keychain-db'",
            ]
        );
        assert_eq!(error.evidence.len(), 1);
    }

    #[test]
    fn the_ios_error_has_its_own_id_and_no_evidence_without_a_log() {
        let outcome = failed("", "x: no identity found\n", None);
        let error = not_listed(
            CheckId::IosSignKeychainNotInSearchList,
            Path::new("/ci/ci.keychain-db"),
            "4444444444444444444444444444444444444444",
            &[PathBuf::from(
                "/Users/acme/Library/Keychains/login.keychain-db",
            )],
            &outcome,
        );
        assert_eq!(error.id, "ios.sign.keychain_not_in_search_list");
        assert_eq!(error.exit, Exit::NeedsOwner);
        assert!(
            error
                .detail
                .contains("which holds /Users/acme/Library/Keychains/login.keychain-db."),
            "{}",
            error.detail
        );
        assert_eq!(
            error.fix.commands,
            [
                "security list-keychains -d user -s /Users/acme/Library/Keychains/login.keychain-db /ci/ci.keychain-db",
                "security list-keychains -d user -s /Users/acme/Library/Keychains/login.keychain-db",
            ]
        );
        assert!(error.evidence.is_empty());
    }

    #[test]
    fn a_relative_keychain_is_named_and_matched_by_its_absolute_path() {
        // codesign and security resolve a relative keychain from icm's
        // directory, so the detail and the commands name it from there too.
        let shown = std::env::current_dir()
            .unwrap()
            .join("ci/build.keychain-db")
            .display()
            .to_string();
        let relative = Path::new("ci/build.keychain-db");
        assert!(is_listed(
            std::slice::from_ref(&PathBuf::from(&shown)),
            relative
        ));
        assert!(!is_listed(
            &[PathBuf::from(
                "/Users/acme/Library/Keychains/login.keychain-db"
            )],
            relative
        ));

        let outcome = failed(
            "",
            "6666666666666666666666666666666666666666: no identity found\n",
            None,
        );
        let error = not_listed(
            CheckId::MacosSignKeychainNotInSearchList,
            relative,
            "6666666666666666666666666666666666666666",
            &parse_list(LIST),
            &outcome,
        );
        assert!(
            error.detail.contains(&format!(
                "did not find the identity 6666666666666666666666666666666666666666 in {shown}. "
            )),
            "{}",
            error.detail
        );
        assert!(
            !error.detail.contains(" ci/build.keychain-db"),
            "{}",
            error.detail
        );
        assert_eq!(
            error.fix.commands,
            [
                format!(
                    "security list-keychains -d user -s /Users/acme/Library/Keychains/login.keychain-db '/Users/acme/Library/Keychains/Build Keys.keychain-db' {}",
                    shell_quote(&shown)
                ),
                "security list-keychains -d user -s /Users/acme/Library/Keychains/login.keychain-db '/Users/acme/Library/Keychains/Build Keys.keychain-db'".to_string(),
            ]
        );
    }
}
