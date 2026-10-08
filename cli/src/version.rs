//! Comparing icm versions (Appendix C item 5).
//!
//! A `VersionReq` like `>=0.14.1-mobile.1` matches `0.14.1-mobile.2` but not
//! `0.14.2-mobile.1` or `0.15.0-mobile.1` (semver's pre-release rule), so the
//! first upstream rebase would make every project "too new". icm therefore
//! compares with `Version`'s *ordering* against a plain minimum.

use semver::Version;

/// The fork's repository, when the build did not record one.
pub const DEFAULT_GIT_URL: &str = "https://github.com/patricksmithlaravel/iced_mobile";

/// Parses a minimum version: `0.14.1-mobile.1`, or the older `>=0.14.1-mobile.1`.
/// It must name a release, `X.Y.Z-mobile.N`: every icm is one, and semver
/// orders `0.14.1-mobile.N` below `0.14.1`, so a plain `0.14.1` would ask
/// for an icm that does not exist.
pub fn parse_min(raw: &str) -> Result<Version, String> {
    let trimmed = raw.trim();
    let plain = trimmed.strip_prefix(">=").map(str::trim).unwrap_or(trimmed);
    let plain = plain.strip_prefix('v').unwrap_or(plain);

    let version = Version::parse(plain).map_err(|error| {
        format!(
            "`{raw}` is not a version ({error}); write a plain minimum like `min_icm = \"0.14.1-mobile.1\"`"
        )
    })?;
    if !is_release(&version) {
        return Err(format!(
            "`{raw}` names no icm release: every icm is `X.Y.Z-mobile.N`, which semver orders below \
             `X.Y.Z`; write `min_icm = \"{}.{}.{}-mobile.N\"` with the N of the oldest icm that \
             reads the file",
            version.major, version.minor, version.patch
        ));
    }
    Ok(version)
}

/// Whether a version is a release's: `X.Y.Z-mobile.N`.
pub fn is_release(version: &Version) -> bool {
    version
        .pre
        .as_str()
        .strip_prefix("mobile.")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `current` is at least `min`, by version ordering.
pub fn meets(current: &Version, min: &Version) -> bool {
    current >= min
}

/// The repository icm installs from.
fn git_url() -> &'static str {
    if crate::buildinfo::GIT_URL.is_empty() {
        DEFAULT_GIT_URL
    } else {
        crate::buildinfo::GIT_URL
    }
}

/// The command that installs icm `min`: a release's tag.
pub fn install_command(min: &Version) -> String {
    format!(
        "cargo install --locked --git {} --tag v{min} icm",
        git_url()
    )
}

/// How to install the newest icm, for a fix whose commands must stay
/// runnable: the summary's words (the install command with `<tag>` in it)
/// and the command that lists the release tags, newest first.
pub fn newest_install() -> (String, String) {
    let url = git_url();
    (
        format!(
            "the fix command lists the release tags, newest first; install the first with \
             `cargo install --locked --git {url} --tag <tag> icm`"
        ),
        format!("git ls-remote --tags --refs --sort=-v:refname {url} 'v*-mobile.*'"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn ordering_survives_upstream_bumps() {
        let min = parse_min("0.14.1-mobile.1").unwrap();
        for newer in [
            "0.14.1-mobile.1",
            "0.14.1-mobile.2",
            "0.14.1-mobile.10",
            "0.14.2-mobile.1",
            "0.15.0-mobile.1",
            "0.14.1",
        ] {
            assert!(meets(&v(newer), &min), "{newer} should satisfy {min}");
        }
        for older in ["0.14.0-mobile.9", "0.14.1-mobile.0", "0.13.9"] {
            assert!(!meets(&v(older), &min), "{older} should not satisfy {min}");
        }

        // What VersionReq would have done: the bug this module avoids.
        let req = semver::VersionReq::parse(">=0.14.1-mobile.1").unwrap();
        assert!(!req.matches(&v("0.14.2-mobile.1")));
    }

    #[test]
    fn legacy_and_prefixed_forms_parse() {
        assert_eq!(
            parse_min(">=0.14.1-mobile.1").unwrap(),
            v("0.14.1-mobile.1")
        );
        assert_eq!(
            parse_min(">= 0.14.1-mobile.3").unwrap(),
            v("0.14.1-mobile.3")
        );
        assert_eq!(parse_min("v0.14.1-mobile.3").unwrap(), v("0.14.1-mobile.3"));
        assert!(parse_min("^0.14").is_err());
        assert!(parse_min("0.14").is_err());
    }

    #[test]
    fn install_commands_name_the_tag() {
        assert!(install_command(&v("0.14.1-mobile.4")).ends_with("--tag v0.14.1-mobile.4 icm"));
        let (words, list) = newest_install();
        assert!(words.contains("--tag <tag> icm"), "{words}");
        assert!(!list.contains('<'), "{list}");
        assert!(list.starts_with("git ls-remote --tags"), "{list}");
    }

    /// Every icm is `X.Y.Z-mobile.N`, which semver orders below `X.Y.Z`: a
    /// minimum without `-mobile.N` names no icm.
    #[test]
    fn minimums_name_a_release() {
        for raw in [
            "0.14.1",
            "v0.14.1",
            ">=0.15.0",
            "0.14.1-beta.1",
            "0.14.1-mobile",
            "0.14.1-mobile.x",
        ] {
            let error = parse_min(raw).unwrap_err();
            assert!(error.contains("names no icm release"), "{raw}: {error}");
        }
        assert!(
            parse_min("0.14.1")
                .unwrap_err()
                .contains("`min_icm = \"0.14.1-mobile.N\"`")
        );
        assert_eq!(
            parse_min("0.14.1-mobile.10").unwrap(),
            v("0.14.1-mobile.10")
        );
    }
}
