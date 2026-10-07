//! How a build of icm decides which framework source `icm new` pins.
//!
//! This file is compiled twice: by `build.rs` (through `#[path]`) and by the
//! library, where its unit tests run. It may use only `std`.
//!
//! Appendix C item 3 of the design: `git describe` finds no tag in the
//! checkout `cargo install --git --tag` builds from, because cargo keeps the
//! tag only as `refs/remotes/origin/tags/<tag>` in its git database. So the
//! tag is derived from the package version (CI forces the two to be equal)
//! and confirmed against that database; and a build from a local checkout
//! pins its revision only when a configured remote already has it, and
//! otherwise the checkout's path.

/// What `build.rs` learned from git about the source being built.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitFacts {
    /// The full commit hash of `HEAD`, if the source is a git checkout.
    pub rev: Option<String>,
    /// Whether tracked files differ from `HEAD`.
    pub dirty: bool,
    /// The checkout's top-level directory.
    pub toplevel: Option<String>,
    /// Whether this is a checkout cargo made for `cargo install --git`.
    pub cargo_checkout: bool,
    /// The repository URL cargo fetched from (its database's `FETCH_HEAD`).
    pub checkout_url: Option<String>,
    /// The commit `refs/remotes/origin/tags/v<version>` names in cargo's
    /// database, when that ref exists.
    pub tag_commit: Option<String>,
    /// The URL of a configured, non-local remote whose remote-tracking
    /// branches contain `HEAD`.
    pub pushed_remote_url: Option<String>,
}

/// The framework source `icm new` pins by default, and the git URL to pin it
/// with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameworkPin {
    /// `tag:<tag>`, `rev:<sha>`, `path:<dir>`, or empty when unknown.
    pub framework: String,
    /// The repository URL for `tag:` and `rev:` pins; may be empty.
    pub url: String,
}

/// Decides the default framework pin for a build of icm `version`.
pub fn decide(facts: &GitFacts, version: &str) -> FrameworkPin {
    let Some(rev) = facts.rev.as_deref() else {
        return FrameworkPin {
            framework: String::new(),
            url: String::new(),
        };
    };

    if facts.cargo_checkout {
        let url = facts
            .checkout_url
            .as_deref()
            .map(normalize_git_url)
            .unwrap_or_default();
        let framework = if facts.tag_commit.as_deref() == Some(rev) {
            format!("tag:v{version}")
        } else {
            // Cargo fetched this rev from `url`, so the remote has it.
            format!("rev:{rev}")
        };
        return FrameworkPin { framework, url };
    }

    let url = facts
        .pushed_remote_url
        .as_deref()
        .map(normalize_git_url)
        .unwrap_or_default();

    if facts.pushed_remote_url.is_some() && !facts.dirty {
        return FrameworkPin {
            framework: format!("rev:{rev}"),
            url,
        };
    }

    FrameworkPin {
        framework: facts
            .toplevel
            .as_deref()
            .map(|dir| format!("path:{dir}"))
            .unwrap_or_default(),
        url,
    }
}

/// Turns a git remote URL into the `https://host/owner/repo` form Cargo.toml
/// git dependencies use: no trailing `.git` or `/`, and SSH forms rewritten.
pub fn normalize_git_url(url: &str) -> String {
    let url = url.trim();
    let mut out = if let Some(rest) = url.strip_prefix("git@") {
        // git@github.com:owner/repo(.git)
        match rest.split_once(':') {
            Some((host, path)) => format!("https://{host}/{path}"),
            None => url.to_string(),
        }
    } else if let Some(rest) = url.strip_prefix("ssh://") {
        let rest = rest.split_once('@').map_or(rest, |(_, r)| r);
        format!("https://{rest}")
    } else {
        url.to_string()
    };

    while out.ends_with('/') {
        let _ = out.pop();
    }

    if let Some(stripped) = out.strip_suffix(".git") {
        out = stripped.to_string();
    }

    out
}

/// Whether a remote URL points at this machine rather than a server.
pub fn is_local_url(url: &str) -> bool {
    let url = url.trim();
    url.starts_with('/')
        || url.starts_with("file:")
        || url.starts_with('.')
        || url.starts_with('~')
        || (!url.contains("://") && !url.contains('@'))
}

/// Reads the source URL from a cargo git database's `FETCH_HEAD`, whose
/// lines end with `... of <url>`.
pub fn fetch_head_url(contents: &str) -> Option<String> {
    let line = contents.lines().find(|line| line.contains(" of "))?;
    let (_, url) = line.rsplit_once(" of ")?;
    let url = url.trim().trim_matches('\'');

    if url.is_empty() {
        None
    } else {
        Some(url.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REV: &str = "e8bc51b5f0e3a1c2d4b5a6978877665544332211";

    #[test]
    fn cargo_checkout_with_matching_tag_pins_the_tag() {
        let facts = GitFacts {
            rev: Some(REV.into()),
            cargo_checkout: true,
            checkout_url: Some("https://github.com/patricksmithlaravel/iced_mobile".into()),
            tag_commit: Some(REV.into()),
            ..GitFacts::default()
        };
        let pin = decide(&facts, "0.14.1-mobile.3");
        assert_eq!(pin.framework, "tag:v0.14.1-mobile.3");
        assert_eq!(
            pin.url,
            "https://github.com/patricksmithlaravel/iced_mobile"
        );
    }

    #[test]
    fn cargo_checkout_of_an_untagged_rev_pins_the_rev() {
        let facts = GitFacts {
            rev: Some(REV.into()),
            cargo_checkout: true,
            checkout_url: Some("https://github.com/x/iced_mobile.git".into()),
            tag_commit: None,
            ..GitFacts::default()
        };
        let pin = decide(&facts, "0.14.1-mobile.3");
        assert_eq!(pin.framework, format!("rev:{REV}"));
        assert_eq!(pin.url, "https://github.com/x/iced_mobile");
    }

    #[test]
    fn local_checkout_pins_rev_only_when_pushed_and_clean() {
        let mut facts = GitFacts {
            rev: Some(REV.into()),
            toplevel: Some("/src/iced_mobile".into()),
            pushed_remote_url: Some("git@github.com:x/iced_mobile.git".into()),
            ..GitFacts::default()
        };
        assert_eq!(decide(&facts, "0.1.0").framework, format!("rev:{REV}"));
        assert_eq!(
            decide(&facts, "0.1.0").url,
            "https://github.com/x/iced_mobile"
        );

        facts.dirty = true;
        assert_eq!(decide(&facts, "0.1.0").framework, "path:/src/iced_mobile");

        facts.dirty = false;
        facts.pushed_remote_url = None;
        assert_eq!(decide(&facts, "0.1.0").framework, "path:/src/iced_mobile");
    }

    #[test]
    fn no_git_means_no_pin() {
        let pin = decide(&GitFacts::default(), "0.1.0");
        assert!(pin.framework.is_empty());
    }

    #[test]
    fn urls_are_normalized() {
        assert_eq!(
            normalize_git_url("https://github.com/a/b.git"),
            "https://github.com/a/b"
        );
        assert_eq!(
            normalize_git_url("https://github.com/a/b/"),
            "https://github.com/a/b"
        );
        assert_eq!(
            normalize_git_url("git@github.com:a/b.git"),
            "https://github.com/a/b"
        );
        assert_eq!(
            normalize_git_url("ssh://git@github.com/a/b"),
            "https://github.com/a/b"
        );
        assert!(is_local_url("/Users/me/repo"));
        assert!(is_local_url("file:///tmp/db"));
        assert!(!is_local_url("https://github.com/a/b"));
        assert!(!is_local_url("git@github.com:a/b"));
    }

    #[test]
    fn fetch_head_url_is_read() {
        let contents = "71f00e8475815532b25e7e07083abb73bd845cde\t\t'71f00e8' of https://github.com/patricksmithlaravel/iced_mobile\n";
        assert_eq!(
            fetch_head_url(contents).as_deref(),
            Some("https://github.com/patricksmithlaravel/iced_mobile")
        );
        assert_eq!(fetch_head_url(""), None);
    }
}
