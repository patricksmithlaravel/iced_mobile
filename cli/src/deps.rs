//! Lockfile checks (design §2.4 item 4, §12.1): one iced, from the fork,
//! pinned; one winit at or above the floors the fork needs.

use crate::cargo::{GitRef, Lock, LockPackage, Source};
use crate::catalogue::CheckId;
use crate::error::Check;
use semver::Version;
use std::collections::BTreeMap;

/// Every crate the fork publishes. Third-party `iced_*` crates (iced_aw,
/// iced_fonts, ...) are not the framework and are not checked.
pub const ICED_CRATES: &[&str] = &[
    "iced",
    "iced_beacon",
    "iced_core",
    "iced_debug",
    "iced_devtools",
    "iced_futures",
    "iced_graphics",
    "iced_highlighter",
    "iced_program",
    "iced_renderer",
    "iced_runtime",
    "iced_selector",
    "iced_test",
    "iced_tester",
    "iced_tiny_skia",
    "iced_wgpu",
    "iced_widget",
    "iced_winit",
];

/// The oldest winit the fork works with.
pub const WINIT_FLOOR: &str = "0.30.13";

/// The oldest softbuffer that works on Android.
pub const SOFTBUFFER_FLOOR: &str = "0.4.7";

fn iced_packages(lock: &Lock) -> Vec<&LockPackage> {
    lock.packages
        .iter()
        .filter(|package| ICED_CRATES.contains(&package.name.as_str()))
        .collect()
}

/// A short, comparable description of where a package comes from.
fn origin(package: &LockPackage) -> String {
    match Source::parse(package.source.as_deref()) {
        Source::Path => format!("path dependency (version {})", package.version),
        Source::Git { url, commit, .. } => format!(
            "{}#{} (version {})",
            crate::gitinfo::normalize_git_url(&url),
            commit,
            package.version
        ),
        Source::Registry { url } => format!("registry {url} (version {})", package.version),
        Source::Other(other) => format!("{other} (version {})", package.version),
    }
}

/// `deps.single_iced`: every iced crate from one source, revision and version.
pub fn single_iced(lock: &Lock) -> Check {
    let packages = iced_packages(lock);
    if packages.is_empty() {
        return Check::fail(
            CheckId::DepsSingleIced,
            format!("{} has no iced crate", crate::paths::display(&lock.path)),
        )
        .fix(
            "Depend on iced from the iced_mobile fork, then run `cargo generate-lockfile`.",
            &[],
        );
    }

    let mut origins: BTreeMap<String, Vec<&LockPackage>> = BTreeMap::new();
    for package in &packages {
        origins.entry(origin(package)).or_default().push(package);
    }

    if origins.len() == 1 {
        let (only, _) = origins.into_iter().next().expect("one origin");
        return Check::pass(
            CheckId::DepsSingleIced,
            format!("{} iced crates from {only}", packages.len()),
        );
    }

    let mut check = Check::fail(
        CheckId::DepsSingleIced,
        format!(
            "iced crates come from {} sources: {}",
            origins.len(),
            origins.keys().cloned().collect::<Vec<_>>().join("; ")
        ),
    );
    for group in origins.values() {
        for package in group.iter().take(3) {
            check = check.evidence(lock.evidence(package));
        }
    }
    check
}

/// `deps.iced_not_fork`: no iced crate from crates.io or upstream.
pub fn iced_not_fork(lock: &Lock) -> Check {
    let offenders: Vec<&LockPackage> = iced_packages(lock)
        .into_iter()
        .filter(|package| {
            let source = Source::parse(package.source.as_deref());
            source.is_crates_io() || source.is_upstream_iced()
        })
        .collect();

    if offenders.is_empty() {
        return Check::pass(
            CheckId::DepsIcedNotFork,
            "iced comes from the fork (or a local path)",
        );
    }

    let mut check = Check::fail(
        CheckId::DepsIcedNotFork,
        format!(
            "{} come from crates.io or upstream iced: {}",
            offenders.len(),
            offenders
                .iter()
                .map(|p| format!("{} {}", p.name, p.version))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    for package in offenders.iter().take(5) {
        check = check.evidence(lock.evidence(package));
    }
    check
}

/// `deps.iced_unpinned`: git iced pinned by tag or rev, not a branch.
pub fn iced_unpinned(lock: &Lock) -> Check {
    let offenders: Vec<&LockPackage> = iced_packages(lock)
        .into_iter()
        .filter(|package| {
            matches!(
                Source::parse(package.source.as_deref()),
                Source::Git {
                    reference: GitRef::Branch(_) | GitRef::DefaultBranch,
                    ..
                }
            )
        })
        .collect();

    if offenders.is_empty() {
        return Check::pass(
            CheckId::DepsIcedUnpinned,
            "iced is pinned by tag, rev or path",
        );
    }

    let mut check = Check::fail(
        CheckId::DepsIcedUnpinned,
        format!(
            "{} follow a branch: {}",
            offenders.len(),
            offenders
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    for package in offenders.iter().take(5) {
        check = check.evidence(lock.evidence(package));
    }
    check
}

/// `deps.cli_framework_skew`: the app's iced revision equals this icm's.
pub fn cli_framework_skew(lock: &Lock, cli_rev: &str, cli_version: &str) -> Check {
    let Some(iced) = lock.named("iced").next() else {
        return Check::skip(CheckId::DepsCliFrameworkSkew, "no iced in Cargo.lock");
    };
    if cli_rev.is_empty() {
        return Check::skip(
            CheckId::DepsCliFrameworkSkew,
            "this icm was built without git metadata",
        );
    }

    match Source::parse(iced.source.as_deref()) {
        Source::Git {
            reference, commit, ..
        } => {
            let same_tag =
                matches!(&reference, GitRef::Tag(tag) if *tag == format!("v{cli_version}"));
            if commit == cli_rev || same_tag {
                Check::pass(
                    CheckId::DepsCliFrameworkSkew,
                    format!("iced {} matches this icm", short(&commit)),
                )
            } else {
                Check::warn(
                    CheckId::DepsCliFrameworkSkew,
                    format!(
                        "the app's iced is at {} but this icm was built from {}",
                        short(&commit),
                        short(cli_rev)
                    ),
                )
                .evidence(lock.evidence(iced))
            }
        }
        Source::Path => Check::skip(
            CheckId::DepsCliFrameworkSkew,
            "iced is a path dependency; its revision is not known",
        ),
        _ => Check::skip(
            CheckId::DepsCliFrameworkSkew,
            "iced is not a git dependency",
        ),
    }
}

fn short(rev: &str) -> &str {
    rev.get(..12).unwrap_or(rev)
}

/// `deps.single_winit`: one winit in the graph.
pub fn single_winit(lock: &Lock) -> Check {
    let winits: Vec<&LockPackage> = lock.named("winit").collect();
    match winits.len() {
        0 => Check::skip(CheckId::DepsSingleWinit, "no winit in Cargo.lock"),
        1 => Check::pass(
            CheckId::DepsSingleWinit,
            format!("winit {} ({})", winits[0].version, origin_kind(winits[0])),
        ),
        n => {
            let mut check = Check::fail(
                CheckId::DepsSingleWinit,
                format!(
                    "{n} winits: {}",
                    winits
                        .iter()
                        .map(|p| format!("{} from {}", p.version, origin_kind(p)))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
            for package in &winits {
                check = check.evidence(lock.evidence(package));
            }
            check
        }
    }
}

fn origin_kind(package: &LockPackage) -> String {
    match Source::parse(package.source.as_deref()) {
        Source::Path => "path".to_string(),
        Source::Git { url, .. } => crate::gitinfo::normalize_git_url(&url),
        source if source.is_crates_io() => "crates.io".to_string(),
        Source::Registry { url } => url,
        Source::Other(other) => other,
    }
}

fn floor_check(lock: &Lock, name: &str, floor: &str, id: CheckId) -> Check {
    let floor_version = Version::parse(floor).expect("valid floor");
    let packages: Vec<&LockPackage> = lock.named(name).collect();
    if packages.is_empty() {
        return Check::skip(id, format!("no {name} in Cargo.lock"));
    }

    let low: Vec<&LockPackage> = packages
        .iter()
        .copied()
        .filter(|package| {
            Version::parse(&package.version).is_ok_and(|version| version < floor_version)
        })
        .collect();

    if low.is_empty() {
        return Check::pass(
            id,
            format!(
                "{name} {} (floor {floor})",
                packages
                    .iter()
                    .map(|p| p.version.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }

    let mut check = Check::fail(
        id,
        format!(
            "{name} {} is below {floor}",
            low.iter()
                .map(|p| p.version.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
    .fix(format!("Run `cargo update -p {name}`."), &[]);
    for package in low {
        check = check.evidence(lock.evidence(package));
    }
    check
}

/// `deps.winit_floor`.
pub fn winit_floor(lock: &Lock) -> Check {
    floor_check(lock, "winit", WINIT_FLOOR, CheckId::DepsWinitFloor)
}

/// `deps.softbuffer_floor` (relevant when building for Android).
pub fn softbuffer_floor(lock: &Lock) -> Check {
    floor_check(
        lock,
        "softbuffer",
        SOFTBUFFER_FLOOR,
        CheckId::DepsSoftbufferFloor,
    )
}

/// Every lockfile check, in order. The skew check needs this icm's rev and
/// version; `android` adds the softbuffer floor.
pub fn check_lock(lock: &Lock, android: bool) -> Vec<Check> {
    let mut checks = vec![
        single_iced(lock),
        iced_not_fork(lock),
        iced_unpinned(lock),
        cli_framework_skew(lock, crate::buildinfo::GIT_REV, crate::buildinfo::VERSION),
        single_winit(lock),
        winit_floor(lock),
    ];
    if android {
        checks.push(softbuffer_floor(lock));
    }
    checks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Status;
    use std::path::Path;

    const FORK: &str = "git+https://github.com/patricksmithlaravel/iced_mobile?tag=v0.14.1-mobile.1#e8bc51b5f0e3a1c2d4b5a6978877665544332211";
    const FORK_OTHER_REV: &str = "git+https://github.com/patricksmithlaravel/iced_mobile?rev=71f00e8#71f00e8475815532b25e7e07083abb73bd845cde";
    const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

    fn lock(packages: &[(&str, &str, Option<&str>)]) -> Lock {
        let mut text = String::from("version = 4\n");
        for (name, version, source) in packages {
            text.push_str(&format!(
                "\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n"
            ));
            if let Some(source) = source {
                text.push_str(&format!("source = \"{source}\"\n"));
            }
        }
        Lock::parse(Path::new("/app/Cargo.lock"), &text).unwrap()
    }

    fn healthy() -> Lock {
        lock(&[
            ("app", "0.1.0", None),
            ("iced", "0.14.1", Some(FORK)),
            ("iced_core", "0.14.1", Some(FORK)),
            ("iced_test", "0.14.1", Some(FORK)),
            ("iced_aw", "0.12.0", Some(CRATES_IO)),
            ("winit", "0.30.13", Some(CRATES_IO)),
            ("softbuffer", "0.4.8", Some(CRATES_IO)),
        ])
    }

    #[test]
    fn a_healthy_lock_passes() {
        for check in check_lock(&healthy(), true) {
            assert!(
                matches!(check.status, Status::Pass | Status::Skip | Status::Warn),
                "{}: {}",
                check.id(),
                check.error.detail
            );
        }
        assert_eq!(single_iced(&healthy()).status, Status::Pass);
        assert_eq!(
            cli_framework_skew(&healthy(), "x", "0.14.1-mobile.1").status,
            Status::Pass
        );
    }

    #[test]
    fn two_copies_of_iced_fail() {
        // The `twocopies` probe: a widget crate pulls iced from crates.io.
        let lock = lock(&[
            ("iced", "0.14.1", Some(FORK)),
            ("iced_core", "0.14.1", Some(FORK)),
            ("iced_core", "0.13.2", Some(CRATES_IO)),
        ]);
        let check = single_iced(&lock);
        assert_eq!(check.status, Status::Fail);
        assert_eq!(check.error.exit, crate::exit::Exit::Config);
        assert!(
            check.error.detail.contains("2 sources"),
            "{}",
            check.error.detail
        );
        assert_eq!(check.error.evidence.len(), 3);
        assert!(check.error.evidence.iter().all(|e| e.line.is_some()));
        assert_eq!(iced_not_fork(&lock).status, Status::Fail);
    }

    #[test]
    fn two_revisions_of_the_fork_fail() {
        let lock = lock(&[
            ("iced", "0.14.1", Some(FORK)),
            ("iced_winit", "0.14.1", Some(FORK_OTHER_REV)),
        ]);
        assert_eq!(single_iced(&lock).status, Status::Fail);
        assert_eq!(iced_not_fork(&lock).status, Status::Pass);
    }

    #[test]
    fn upstream_and_crates_io_are_not_the_fork() {
        let upstream = lock(&[(
            "iced",
            "0.14.0",
            Some("git+https://github.com/iced-rs/iced?tag=0.14.0#abc"),
        )]);
        assert_eq!(iced_not_fork(&upstream).status, Status::Fail);
        let registry = lock(&[("iced", "0.14.0", Some(CRATES_IO))]);
        assert_eq!(iced_not_fork(&registry).status, Status::Fail);
        let path = lock(&[("iced", "0.14.1", None)]);
        assert_eq!(iced_not_fork(&path).status, Status::Pass);
    }

    #[test]
    fn branches_are_unpinned() {
        let branch = lock(&[(
            "iced",
            "0.14.1",
            Some("git+https://github.com/patricksmithlaravel/iced_mobile?branch=main#abc"),
        )]);
        assert_eq!(iced_unpinned(&branch).status, Status::Fail);
        let default_branch = lock(&[(
            "iced",
            "0.14.1",
            Some("git+https://github.com/patricksmithlaravel/iced_mobile#abc"),
        )]);
        assert_eq!(iced_unpinned(&default_branch).status, Status::Fail);
        assert_eq!(iced_unpinned(&healthy()).status, Status::Pass);
    }

    #[test]
    fn skew_is_a_warning() {
        let lock = lock(&[("iced", "0.14.1", Some(FORK_OTHER_REV))]);
        let check = cli_framework_skew(
            &lock,
            "e8bc51b5f0e3a1c2d4b5a6978877665544332211",
            "0.14.1-mobile.1",
        );
        assert_eq!(check.status, Status::Warn);
        assert!(check.error.detail.contains("71f00e847581"));
        let same = cli_framework_skew(
            &lock,
            "71f00e8475815532b25e7e07083abb73bd845cde",
            "0.14.1-mobile.9",
        );
        assert_eq!(same.status, Status::Pass);
        assert_eq!(cli_framework_skew(&lock, "", "x").status, Status::Skip);
    }

    #[test]
    fn winit_and_softbuffer_floors() {
        let two = lock(&[
            ("winit", "0.30.13", Some(CRATES_IO)),
            (
                "winit",
                "0.30.13",
                Some("git+https://github.com/x/winit?rev=1#1"),
            ),
        ]);
        assert_eq!(single_winit(&two).status, Status::Fail);

        let old = lock(&[
            ("winit", "0.30.9", Some(CRATES_IO)),
            ("softbuffer", "0.4.6", Some(CRATES_IO)),
        ]);
        assert_eq!(winit_floor(&old).status, Status::Fail);
        assert_eq!(softbuffer_floor(&old).status, Status::Fail);
        assert_eq!(winit_floor(&healthy()).status, Status::Pass);
        assert_eq!(softbuffer_floor(&lock(&[])).status, Status::Skip);
    }

    #[test]
    fn an_empty_lock_has_no_iced() {
        assert_eq!(
            single_iced(&lock(&[("app", "0.1.0", None)])).status,
            Status::Fail
        );
    }
}
