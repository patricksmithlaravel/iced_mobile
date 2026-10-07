//! Dependency checks (design §2.4 item 4, §5, §12.1): from Cargo.lock, one
//! iced, from the fork, pinned, and one winit (the one iced brings) at or
//! above the floors the fork needs; from the Android build's resolved
//! features (cargo tree), one Android activity backend.

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

/// A short, comparable description of where a package comes from: the
/// source and revision, not the version (the fork's crates do not all share
/// one version: iced_widget is 0.14.2 next to iced 0.14.1).
fn origin(package: &LockPackage) -> String {
    match Source::parse(package.source.as_deref()) {
        Source::Path => "a path dependency".to_string(),
        Source::Git { url, commit, .. } => {
            format!("{}#{}", crate::gitinfo::normalize_git_url(&url), commit)
        }
        Source::Registry { url } => format!("registry {url}"),
        Source::Other(other) => other,
    }
}

/// `deps.single_iced`: every iced crate from one source and revision.
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
            origins
                .iter()
                .map(|(origin, group)| format!(
                    "{origin} ({})",
                    group
                        .iter()
                        .map(|p| format!("{} {}", p.name, p.version))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
                .collect::<Vec<_>>()
                .join("; ")
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

/// Where iced comes from, as [`origin`] names it: the `iced` package's
/// source, or the first framework crate's when the lock has no `iced`.
fn iced_origin(lock: &Lock) -> Option<String> {
    lock.named("iced")
        .next()
        .or_else(|| iced_packages(lock).into_iter().next())
        .map(origin)
}

/// `deps.single_winit`: one winit in the graph.
///
/// The fork vendors its winit (`vendor/winit`, a path dependency of its
/// workspace), so winit comes from iced's own source: in an app's lock, the
/// same git URL and revision as iced; in the fork's workspace, or with iced
/// as a path dependency, a path. An older fork takes winit from crates.io.
/// Either way there must be one: a crate that depends on winit itself (from
/// crates.io, another git source or path) adds a second copy, which iced
/// does not use, and whose Objective-C classes clash with iced's on iOS.
pub fn single_winit(lock: &Lock) -> Check {
    let winits: Vec<&LockPackage> = lock.named("winit").collect();
    let iced = iced_origin(lock);
    let from_iced = |package: &LockPackage| iced.as_deref() == Some(origin(package).as_str());
    let describe = |package: &LockPackage| {
        if from_iced(package) {
            format!("{} from iced's own source", package.version)
        } else {
            format!("{} from {}", package.version, origin_kind(package))
        }
    };
    match winits.len() {
        0 => Check::skip(CheckId::DepsSingleWinit, "no winit in Cargo.lock"),
        1 if from_iced(winits[0]) => Check::pass(
            CheckId::DepsSingleWinit,
            format!("winit {}", describe(winits[0])),
        ),
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
                        .map(|p| describe(p))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
            .fix(
                "Keep only the winit iced brings: drop direct winit dependencies (use iced::mobile::AndroidApp, iced_winit::winit and iced's activity features), and give a crate that needs winit itself iced's copy with [patch.crates-io] winit on iced's git URL and tag or rev, character for character. `cargo tree -d --target all` shows which crate pulls each copy.",
                &["cargo tree -d --target all"],
            );
            // iced's own first: the evidence of the others is what to fix.
            let mut winits = winits;
            winits.sort_by_key(|package| !from_iced(package));
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

/// The crate whose features choose the Java activity an Android app runs
/// in. winit depends on it on Android; iced's `android-native-activity` and
/// `android-game-activity` features reach it through iced_winit and winit.
pub const ANDROID_ACTIVITY: &str = "android-activity";

/// android-activity's backends: its feature, the `[android] activity` value
/// that runs it, and the iced feature that turns it on.
const ACTIVITY_BACKENDS: &[(&str, &str, &str)] = &[
    ("native-activity", "native", "android-native-activity"),
    ("game-activity", "game", "android-game-activity"),
];

/// One android-activity crate in an Android build: `{p}` as cargo tree
/// prints it (`android-activity v0.6.1`, plus the source when it is not
/// crates.io) and its features.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityCrate {
    /// The package, as cargo tree names it.
    pub package: String,
    /// Its enabled features.
    pub features: Vec<String>,
}

impl ActivityCrate {
    /// The backends among its features.
    fn backends(&self) -> Vec<&'static str> {
        ACTIVITY_BACKENDS
            .iter()
            .filter(|(feature, _, _)| self.features.iter().any(|f| f == feature))
            .map(|(feature, _, _)| *feature)
            .collect()
    }
}

/// The android-activity crates in `cargo tree -f '{p}|{f}' --prefix none`
/// output, each once (cargo marks repeats with ` (*)`).
pub fn activity_crates(tree: &str) -> Vec<ActivityCrate> {
    let mut crates: Vec<ActivityCrate> = Vec::new();
    for line in tree.lines() {
        let line = line.trim();
        let line = line.strip_suffix("(*)").map_or(line, str::trim_end);
        let Some((package, features)) = line.split_once('|') else {
            continue;
        };
        if !package.starts_with(&format!("{ANDROID_ACTIVITY} v")) {
            continue;
        }
        if crates.iter().any(|c| c.package == package) {
            continue;
        }
        crates.push(ActivityCrate {
            package: package.to_string(),
            features: features
                .split(',')
                .map(str::trim)
                .filter(|f| !f.is_empty())
                .map(str::to_string)
                .collect(),
        });
    }
    crates
}

/// `deps.android_activity_backend`: the app's Android build turns on
/// exactly one android-activity backend, the one `[android] activity`
/// names (the generated manifest starts that activity). `crates` are the
/// android-activity crates of `package`'s build for `triple`
/// ([`activity_crates`]); `manifest` is the package's Cargo.toml.
pub fn android_activity_backend(
    crates: &[ActivityCrate],
    activity: &str,
    package: &str,
    triple: &str,
    manifest: &std::path::Path,
) -> Check {
    let tree =
        format!("cargo tree -p {package} --target {triple} -e features -i {ANDROID_ACTIVITY}");
    let evidence = crate::error::Evidence::file(manifest);
    let wanted = ACTIVITY_BACKENDS
        .iter()
        .find(|(_, value, _)| *value == activity)
        .copied()
        .unwrap_or(ACTIVITY_BACKENDS[0]);
    let (wanted_feature, _, wanted_iced) = wanted;

    if crates.is_empty() {
        return Check::fail(
            CheckId::DepsAndroidActivityBackend,
            format!(
                "{package}'s Android build ({triple}) has no {ANDROID_ACTIVITY}, so it has no activity backend: nothing gives Android an `android_main` to start"
            ),
        )
        .fix(
            format!(
                "Depend on iced with its default features, or list `{wanted_iced}` among them, so that winit (and android-activity) are built for Android."
            ),
            &[],
        )
        .evidence(evidence);
    }

    let found: Vec<(&ActivityCrate, &'static str)> = crates
        .iter()
        .flat_map(|c| c.backends().into_iter().map(move |b| (c, b)))
        .collect();
    match found.as_slice() {
        [] => Check::fail(
            CheckId::DepsAndroidActivityBackend,
            format!(
                "{} is built for {triple} with neither native-activity nor game-activity: iced's default `android-native-activity` feature is off (`default-features = false` on iced drops it), and android-activity refuses to build without a backend",
                crates
                    .iter()
                    .map(|c| c.package.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
        .fix(
            format!(
                "Add \"{wanted_iced}\" to iced's features (or iced_winit's, which has the same feature); not to a winit of the app's own, which would be a second winit (deps.single_winit)."
            ),
            &[],
        )
        .evidence(evidence),
        [(c, backend)] if *backend == wanted_feature => Check::pass(
            CheckId::DepsAndroidActivityBackend,
            format!(
                "{} is built for {triple} with {backend} alone, the backend [android] activity = \"{activity}\" runs",
                c.package
            ),
        ),
        [(c, backend)] => Check::fail(
            CheckId::DepsAndroidActivityBackend,
            format!(
                "{} is built for {triple} with {backend}, but [android] activity = \"{activity}\" makes the manifest start the activity {wanted_feature} serves: the app would not start",
                c.package
            ),
        )
        .fix(
            format!(
                "Turn on iced's `{wanted_iced}` instead (the other backend's feature comes from the crate `{tree}` names)."
            ),
            &[tree.as_str()],
        )
        .evidence(evidence),
        several => Check::fail(
            CheckId::DepsAndroidActivityBackend,
            if let [only] = crates {
                format!(
                    "{} is built for {triple} with both native-activity and game-activity (iced's `android-native-activity`, a default feature, and `android-game-activity` are both on), and android-activity refuses to build with both",
                    only.package
                )
            } else {
                format!(
                    "{package}'s Android build ({triple}) holds {} android-activity crates with {} backends ({}): their activity entry points collide",
                    crates.len(),
                    several.len(),
                    several
                        .iter()
                        .map(|(c, backend)| format!("{} with {backend}", c.package))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
        )
        .fix(
            format!(
                "Keep only `{wanted_iced}`; the command below shows which crate turns on each backend. A GameActivity build needs `default-features = false` on every crate that depends on iced."
            ),
            &[tree.as_str()],
        )
        .evidence(evidence),
    }
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
    fn one_source_with_several_versions_passes() {
        // The fork's iced_widget is 0.14.2 next to iced 0.14.1.
        let git = lock(&[
            ("iced", "0.14.1", Some(FORK)),
            ("iced_widget", "0.14.2", Some(FORK)),
        ]);
        assert_eq!(single_iced(&git).status, Status::Pass);
        let path = lock(&[("iced", "0.14.1", None), ("iced_widget", "0.14.2", None)]);
        assert_eq!(single_iced(&path).status, Status::Pass);
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
    fn the_winit_iced_brings_is_the_one() {
        // The fork vendors winit: in an app's lock it comes from iced's
        // git source, as dpi does.
        let vendored = lock(&[
            ("iced", "0.14.1", Some(FORK)),
            ("iced_winit", "0.14.1", Some(FORK)),
            ("dpi", "0.1.1", Some(FORK)),
            ("winit", "0.30.13", Some(FORK)),
        ]);
        let check = single_winit(&vendored);
        assert_eq!(check.status, Status::Pass);
        assert!(
            check.error.detail.contains("from iced's own source"),
            "{}",
            check.error.detail
        );
        assert_eq!(winit_floor(&vendored).status, Status::Pass);

        // In the fork's own workspace both are paths.
        let workspace = lock(&[
            ("iced", "0.14.1", None),
            ("iced_winit", "0.14.1", None),
            ("winit", "0.30.13", None),
        ]);
        assert_eq!(single_winit(&workspace).status, Status::Pass);

        // A crate that still depends on crates.io winit adds a second one.
        let second = lock(&[
            ("iced", "0.14.1", Some(FORK)),
            ("winit", "0.30.13", Some(CRATES_IO)),
            ("winit", "0.30.13", Some(FORK)),
        ]);
        let check = single_winit(&second);
        assert_eq!(check.status, Status::Fail);
        assert!(
            check
                .error
                .detail
                .contains("0.30.13 from crates.io, 0.30.13 from iced's own source"),
            "{}",
            check.error.detail
        );
        assert!(check.error.fix.summary.contains("[patch.crates-io]"));
        // iced's copy first, then the one to remove.
        assert_eq!(check.error.evidence.len(), 2);
        let excerpt = |index: usize| {
            check.error.evidence[index]
                .excerpt
                .clone()
                .unwrap_or_default()
        };
        assert!(excerpt(0).contains("iced_mobile"), "{}", excerpt(0));
        assert!(excerpt(1).contains("crates.io-index"), "{}", excerpt(1));

        // So does winit from another revision of the fork.
        let other_rev = lock(&[
            ("iced", "0.14.1", Some(FORK)),
            ("winit", "0.30.13", Some(FORK)),
            ("winit", "0.30.13", Some(FORK_OTHER_REV)),
        ]);
        assert_eq!(single_winit(&other_rev).status, Status::Fail);
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

    // `cargo tree -e normal,build -f '{p}|{f}' --prefix none` for Tawara's
    // Android build (iced_winit without default features, winit with
    // `android-native-activity`), trimmed.
    const TAWARA_TREE: &str = "\
tawara-mobile v0.1.0 (/src/tawara/crates/mobile)|
iced_winit v0.14.1 (https://github.com/patricksmithlaravel/iced_mobile?rev=71f00e8#71f00e84)|linux-theme-detection,wayland,x11
winit v0.30.13|ahash,android-native-activity,bytemuck,rwh_06
android-activity v0.6.1|default,native-activity
ndk v0.9.0|default,rwh_06
winit v0.30.13|ahash,android-native-activity,bytemuck,rwh_06 (*)
android-activity v0.6.1|default,native-activity (*)
";

    fn backend(tree: &str, activity: &str) -> Check {
        android_activity_backend(
            &activity_crates(tree),
            activity,
            "app",
            "aarch64-linux-android",
            Path::new("/app/Cargo.toml"),
        )
    }

    #[test]
    fn one_native_activity_passes() {
        let crates = activity_crates(TAWARA_TREE);
        assert_eq!(
            crates,
            vec![ActivityCrate {
                package: "android-activity v0.6.1".to_string(),
                features: vec!["default".to_string(), "native-activity".to_string()],
            }]
        );
        let check = backend(TAWARA_TREE, "native");
        assert_eq!(check.status, Status::Pass, "{}", check.error.detail);
        assert!(check.error.detail.contains("native-activity alone"));
    }

    #[test]
    fn no_backend_or_no_android_activity_fails() {
        let none = backend(
            "app v0.1.0 (/app)|\nandroid-activity v0.6.1|default\n",
            "native",
        );
        assert_eq!(none.status, Status::Fail);
        assert_eq!(none.error.exit, crate::exit::Exit::Config);
        assert!(
            none.error.detail.contains("neither"),
            "{}",
            none.error.detail
        );
        assert!(none.error.fix.summary.contains("android-native-activity"));

        let absent = backend("app v0.1.0 (/app)|\niced v0.14.1 (/iced)|wgpu\n", "native");
        assert_eq!(absent.status, Status::Fail);
        assert!(absent.error.detail.contains("has no android-activity"));
    }

    #[test]
    fn two_backends_fail() {
        let both = backend(
            "app v0.1.0 (/app)|\nandroid-activity v0.6.1|default,game-activity,native-activity\n",
            "native",
        );
        assert_eq!(both.status, Status::Fail);
        assert!(
            both.error
                .detail
                .contains("with both native-activity and game-activity"),
            "{}",
            both.error.detail
        );
        assert!(
            both.error.fix.commands[0].contains("-e features -i android-activity"),
            "{:?}",
            both.error.fix.commands
        );

        // Two copies of android-activity, one backend each.
        let copies = backend(
            "android-activity v0.5.2|native-activity\nandroid-activity v0.6.1|native-activity\n",
            "native",
        );
        assert_eq!(copies.status, Status::Fail);
        assert!(
            copies.error.detail.contains("2 android-activity crates"),
            "{}",
            copies.error.detail
        );
    }

    #[test]
    fn the_backend_must_match_the_manifest() {
        let game = "android-activity v0.6.1|default,game-activity\n";
        assert_eq!(backend(game, "native").status, Status::Fail);
        assert_eq!(backend(game, "game").status, Status::Pass);
        assert_eq!(backend(TAWARA_TREE, "game").status, Status::Fail);
    }

    #[test]
    fn an_empty_lock_has_no_iced() {
        assert_eq!(
            single_iced(&lock(&[("app", "0.1.0", None)])).status,
            Status::Fail
        );
    }
}
