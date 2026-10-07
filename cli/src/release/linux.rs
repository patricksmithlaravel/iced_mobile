//! The Linux release pipeline (design §11.6, §9.6): a `.deb` and an
//! AppImage.
//!
//! It runs on a Linux host only (`env.unsupported_host` elsewhere): the
//! binary links against the host's glibc, and dpkg-deb, dpkg-shlibdeps and
//! appimagetool are Linux tools. The release workflow builds in the
//! `ubuntu:22.04` container (glibc 2.35), the default `[desktop.linux]
//! glibc_floor`.
//!
//! 1. Preconditions: `[desktop.linux] maintainer` for a `.deb` (the
//!    owner's decision, `config.owner_decision`; under `--sign none` the
//!    package says `<publisher> <maintainer-unset@invalid>`); dpkg-deb and
//!    dpkg-shlibdeps for `deb`; for `appimage`, appimagetool and the
//!    AppImage runtime pinned in `tools.toml` (downloaded only with
//!    `--yes`).
//! 2. `cargo build --release --locked` for the host in the release target
//!    directory, then `linux.glibc_floor`: the highest `GLIBC_x.y` in the
//!    executable's `.gnu.version_r` is at most `glibc_floor` (a FAIL keeps
//!    the artifacts but makes them not uploadable).
//! 3. The `.deb`: `usr/bin/<bin>`, `usr/share/applications/<id>.desktop`,
//!    hicolor icons from 16 to 512 px, `usr/share/doc/<package>/` with
//!    THIRD_PARTY_NOTICES.txt and `copyright`, `[app] resources` under
//!    `usr/share/<package>/`; `DEBIAN/control` with Depends from
//!    `dpkg-shlibdeps -O` plus `deb_depends`, and Recommends for the
//!    libraries winit and wgpu load with dlopen plus `deb_recommends`;
//!    `dpkg-deb -Zxz --build --root-owner-group` (xz, which every
//!    Debian-based dpkg reads), `dpkg-deb --info`, `linux.desktop_file`
//!    (icm's own checks, and `desktop-file-validate` when installed) and
//!    `linux.deb.lint` (lintian, a WARN, when installed).
//! 4. The AppImage: an AppDir with `AppRun`, the `.desktop` entry, the
//!    icon, the executable, the notices and the bundled libxkbcommon(-x11)
//!    and libwayland-cursor (`linux.appimage_libs` WARN when the host lacks
//!    one; libwayland-client is the host's, as the AppImage excludelist
//!    says, [`files::BUNDLED`]), then
//!    `appimagetool --appimage-extract-and-run --runtime-file <pinned
//!    runtime>`, so the build downloads nothing.
//!
//! `icm verify linux` unpacks a `.deb` itself (ar and tar, on any host)
//! and an AppImage with `--appimage-extract` (on Linux), then runs the
//! glibc, `.desktop` and notices gates.

pub mod files;

use super::desktop::{self, ar, elf, icons};
use super::verify::Verify;
use super::{Pipeline, Release, owner_plans};
use crate::cargo::Select;
use crate::catalogue::{By, CheckId};
use crate::cli::ReleaseTarget;
use crate::context::Ctx;
use crate::error::{Check, Evidence, IcmError, Result};
use crate::plan::{Plan, Step};
use crate::process::{Cmd, Outcome};
use crate::tools::Env;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The Linux pipeline.
pub struct Linux;

fn formats(rel: &Release) -> Vec<String> {
    rel.config().desktop.linux.formats.clone()
}

fn wants(rel: &Release, format: &str) -> bool {
    formats(rel).iter().any(|f| f == format)
}

/// The Debian package name: `[desktop.linux] deb_package`, else the Cargo
/// package's name made a Debian one.
pub fn deb_package(rel: &Release) -> String {
    rel.config()
        .desktop
        .linux
        .deb_package
        .clone()
        .unwrap_or_else(|| files::deb_package_name(&rel.package.name))
}

fn maintainer(rel: &Release) -> String {
    rel.config()
        .desktop
        .linux
        .maintainer
        .clone()
        .unwrap_or_else(|| {
            format!(
                "{} <maintainer-unset@invalid>",
                desktop::publisher(&rel.project)
            )
        })
}

fn which(env: &Env, name: &str) -> Option<PathBuf> {
    if let Some(path) = env.tool_override(name) {
        return Some(path);
    }
    let path = env.var("PATH")?;
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn apt_missing(name: &str, package: &str, why: &str) -> IcmError {
    IcmError::new(
        CheckId::EnvToolMissing,
        format!("{name} is not on PATH; {why}"),
    )
    .fix(
        format!("Install {package} (Debian and Ubuntu), or build in the ubuntu:22.04 container of the release workflow."),
        &[&format!("sudo apt-get install --no-install-recommends {package}")],
    )
    .by(By::Agent)
}

/// The directories bundled libraries are looked for in:
/// `ICM_LINUX_LIB_DIRS` (`:`-separated; icm's tests), else the host's
/// multiarch and plain library directories.
fn lib_dirs(env: &Env, arch: &str) -> Vec<PathBuf> {
    if let Some(dirs) = env.var("ICM_LINUX_LIB_DIRS") {
        return std::env::split_paths(dirs).collect();
    }
    let multiarch = format!("{arch}-linux-gnu");
    [
        format!("/usr/lib/{multiarch}"),
        format!("/lib/{multiarch}"),
        "/usr/lib64".to_string(),
        "/usr/lib".to_string(),
        "/lib64".to_string(),
        "/lib".to_string(),
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

fn run(ctx: &Ctx, name: &str, cmd: &Cmd) -> Result<Outcome> {
    desktop::run(ctx, name, cmd, CheckId::ToolFailed)
}

/// `linux.glibc_floor` on an executable.
pub fn glibc_check(exe: &Path, floor: &str) -> Check {
    let shown = crate::paths::display(exe);
    let image = match elf::read(exe) {
        Ok(image) => image,
        Err(error) => {
            return Check::fail(
                CheckId::LinuxGlibcFloor,
                format!("cannot read the ELF: {error}"),
            )
            .evidence(Evidence::file(exe));
        }
    };
    let floor_version = elf::parse_version(floor).unwrap_or_default();
    match image.max_glibc() {
        None => Check::pass(
            CheckId::LinuxGlibcFloor,
            format!("{shown} needs no versioned glibc symbol"),
        ),
        Some(needed) if needed <= floor_version => Check::pass(
            CheckId::LinuxGlibcFloor,
            format!(
                "{shown} needs glibc {} at most, within [desktop.linux] glibc_floor {floor}",
                elf::version_string(&needed)
            ),
        ),
        Some(needed) => Check::fail(
            CheckId::LinuxGlibcFloor,
            format!(
                "{shown} needs glibc {}, above [desktop.linux] glibc_floor {floor}: it would not start on older distributions",
                elf::version_string(&needed)
            ),
        )
        .evidence(Evidence::file(exe))
        .fix(
            "Build in the ubuntu:22.04 container (the release workflow does), or raise [desktop.linux] glibc_floor if the app need not run on older distributions.",
            &[],
        ),
    }
}

/// `linux.desktop_file` on a `.desktop` file: icm's checks, then
/// `desktop-file-validate` when installed.
fn desktop_file_checks(ctx: &Ctx, path: &Path) -> Result<Vec<Check>> {
    let shown = crate::paths::display(path);
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let problems = files::desktop_problems(&text);
    let mut checks = vec![if problems.is_empty() {
        Check::pass(
            CheckId::LinuxDesktopFile,
            format!("{shown} is a valid application entry"),
        )
    } else {
        Check::fail(
            CheckId::LinuxDesktopFile,
            format!("{shown}: {}", problems.join("; ")),
        )
        .evidence(Evidence::file(path))
    }];
    if let Some(validate) = which(&ctx.env, "desktop-file-validate") {
        let outcome = ctx.step(
            "desktop-file-validate",
            &Cmd::new(validate)
                .arg(path)
                .timeout(Duration::from_secs(60)),
        )?;
        let output = format!("{}{}", outcome.stdout_text(), outcome.stderr_text());
        let errors: Vec<&str> = output.lines().filter(|l| l.contains("error:")).collect();
        checks.push(if outcome.success() && errors.is_empty() {
            Check::pass(
                CheckId::LinuxDesktopFile,
                format!("{shown}: desktop-file-validate passes"),
            )
        } else {
            let mut check = Check::fail(
                CheckId::LinuxDesktopFile,
                format!("{shown}: desktop-file-validate: {}", errors.join(" / ")),
            );
            if let Some(log) = &outcome.log {
                check = check.evidence(Evidence::file(log));
            }
            check
        });
    }
    Ok(checks)
}

fn facts(rel: &Release, arch: &str) -> Result<files::Facts> {
    let config = rel.config();
    Ok(files::Facts {
        name: config.app.name.clone(),
        id: config.app.id.clone(),
        package: deb_package(rel),
        deb_version: format!("{}-{}", rel.version, rel.build),
        arch: arch.to_string(),
        maintainer: maintainer(rel),
        description: config.app.description.clone(),
        category: config.app.category.clone(),
        copyright: config
            .app
            .copyright
            .clone()
            .unwrap_or_else(|| desktop::publisher(&rel.project)),
        homepage: config.store.marketing_url.clone(),
        bin: rel
            .project
            .bin_for(super::ledger::platform_key(ReleaseTarget::Linux))?,
    })
}

/// The files both packages share, under `root` (`usr/...`).
fn lay_out_usr(
    rel: &Release,
    facts: &files::Facts,
    root: &Path,
    exe: &Path,
    icon: &icons::Icon,
    notices: &Path,
) -> Result<()> {
    let usr = root.join("usr");
    desktop::copy_file(exe, &usr.join("bin").join(&facts.bin), 0o755)?;
    desktop::write_file(
        &usr.join("share/applications")
            .join(format!("{}.desktop", facts.id)),
        files::desktop_entry(facts).as_bytes(),
        0o644,
    )?;
    for size in icons::HICOLOR {
        desktop::write_file(
            &usr.join(format!(
                "share/icons/hicolor/{size}x{size}/apps/{}.png",
                facts.id
            )),
            &icons::png(icon, *size),
            0o644,
        )?;
    }
    let doc = usr.join("share/doc").join(&facts.package);
    desktop::copy_file(notices, &doc.join(super::notices::FILE), 0o644)?;
    desktop::write_file(
        &doc.join("copyright"),
        files::copyright(facts, &rel.version).as_bytes(),
        0o644,
    )?;
    for relative in desktop::resources(&rel.project)? {
        desktop::copy_file(
            &rel.project.dir().join(&relative),
            &usr.join("share").join(&facts.package).join(&relative),
            0o644,
        )?;
    }
    Ok(())
}

fn fix_modes(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        std::fs::set_permissions(&current, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| desktop::io_error("chmod", &current, e))?;
        for entry in std::fs::read_dir(&current).into_iter().flatten().flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                stack.push(entry.path());
            }
        }
    }
    Ok(())
}

fn build_deb(
    ctx: &mut Ctx,
    rel: &mut Release,
    facts: &files::Facts,
    exe: &Path,
    icon: &icons::Icon,
    notices: &Path,
) -> Result<PathBuf> {
    let work = rel.gen_dir.join("deb");
    desktop::fresh_dir(&work)?;
    let root = work.join("root");
    lay_out_usr(rel, facts, &root, exe, icon, notices)?;

    // Depends from dpkg-shlibdeps, run against a stub debian/control.
    let shlibs = work.join("shlibdeps");
    desktop::write_file(
        &shlibs.join("debian/control"),
        format!(
            "Source: {0}\n\nPackage: {0}\nArchitecture: any\n",
            facts.package
        )
        .as_bytes(),
        0o644,
    )?;
    let installed = root.join("usr/bin").join(&facts.bin);
    let outcome = run(
        ctx,
        "dpkg-shlibdeps",
        &Cmd::new(which(&ctx.env, "dpkg-shlibdeps").unwrap_or_else(|| "dpkg-shlibdeps".into()))
            .arg("-O")
            .arg(format!("-e{}", installed.display()))
            .cwd(&shlibs)
            .timeout(Duration::from_secs(300)),
    )?;
    let linux = &rel.config().desktop.linux;
    let mut depends = files::shlibs_depends(&outcome.stdout_text());
    for extra in &linux.deb_depends {
        if !depends.contains(extra) {
            depends.push(extra.clone());
        }
    }
    let installed_kib = desktop::size_under(&root).div_ceil(1024);
    desktop::write_file(
        &root.join("DEBIAN/control"),
        files::control(facts, &depends, &linux.deb_recommends, installed_kib).as_bytes(),
        0o644,
    )?;
    fix_modes(&root)?;

    let deb = rel.dist.join(format!(
        "{}_{}_{}.deb",
        facts.package, facts.deb_version, facts.arch
    ));
    let dpkg_deb = which(&ctx.env, "dpkg-deb").unwrap_or_else(|| "dpkg-deb".into());
    let _ = run(
        ctx,
        "dpkg-deb.build",
        &Cmd::new(&dpkg_deb)
            .args(["-Zxz", "--build", "--root-owner-group"])
            .arg(&root)
            .arg(&deb)
            .timeout(Duration::from_secs(600)),
    )?;
    let info = run(
        ctx,
        "dpkg-deb.info",
        &Cmd::new(&dpkg_deb).arg("--info").arg(&deb),
    )?;
    if !info
        .stdout_text()
        .contains(&format!("Package: {}", facts.package))
    {
        return Err(IcmError::new(
            CheckId::ToolFailed,
            format!(
                "dpkg-deb --info {} does not show Package: {}",
                crate::paths::display(&deb),
                facts.package
            ),
        ));
    }
    let entry = root
        .join("usr/share/applications")
        .join(format!("{}.desktop", facts.id));
    for check in desktop_file_checks(ctx, &entry)? {
        rel.check(ctx, check);
    }
    let check = match which(&ctx.env, "lintian") {
        Some(lintian) => {
            let outcome = ctx.step(
                "lintian",
                &Cmd::new(lintian)
                    .args(["--no-tag-display-limit"])
                    .arg(&deb)
                    .timeout(Duration::from_secs(600)),
            )?;
            let text = format!("{}{}", outcome.stdout_text(), outcome.stderr_text());
            let tags: Vec<&str> = text
                .lines()
                .filter(|l| l.starts_with("E: ") || l.starts_with("W: "))
                .collect();
            if tags.is_empty() {
                Check::pass(
                    CheckId::LinuxDebLint,
                    "lintian reports no errors or warnings",
                )
            } else {
                let mut check = Check::warn(
                    CheckId::LinuxDebLint,
                    format!("lintian: {} tag(s), the first: {}", tags.len(), tags[0]),
                );
                if let Some(log) = &outcome.log {
                    check = check.evidence(Evidence::file(log));
                }
                check
            }
        }
        None => Check::skip(CheckId::LinuxDebLint, "lintian is not installed"),
    };
    rel.check(ctx, check);
    rel.tool(
        "dpkg-deb",
        ctx.probe(&Cmd::new(&dpkg_deb).arg("--version"))
            .ok()
            .and_then(|o| o.stdout_text().lines().next().map(str::to_string))
            .unwrap_or_default(),
    );
    rel.embed_notices(
        &deb,
        &format!("usr/share/doc/{}/{}", facts.package, super::notices::FILE),
    )?;
    Ok(deb)
}

fn build_appimage(
    ctx: &mut Ctx,
    rel: &mut Release,
    facts: &files::Facts,
    exe: &Path,
    icon: &icons::Icon,
    notices: &Path,
    arch: &str,
) -> Result<PathBuf> {
    let appimagetool = crate::pinned::require(ctx, "appimagetool")?;
    let runtime = crate::pinned::require(ctx, "appimage-runtime")?;
    let work = rel.gen_dir.join("appimage");
    desktop::fresh_dir(&work)?;
    let appdir = work.join("AppDir");
    lay_out_usr(rel, facts, &appdir, exe, icon, notices)?;
    desktop::write_file(
        &appdir.join("AppRun"),
        files::app_run(&facts.bin).as_bytes(),
        0o755,
    )?;
    desktop::write_file(
        &appdir.join(format!("{}.desktop", facts.id)),
        files::desktop_entry(facts).as_bytes(),
        0o644,
    )?;
    let icon_file = format!("{}.png", facts.id);
    desktop::write_file(&appdir.join(&icon_file), &icons::png(icon, 256), 0o644)?;
    std::os::unix::fs::symlink(&icon_file, appdir.join(".DirIcon"))
        .map_err(|e| desktop::io_error("link", &appdir.join(".DirIcon"), e))?;

    // The libraries winit loads, from the build host.
    let dirs = lib_dirs(&ctx.env, arch);
    let mut missing = Vec::new();
    let doc = appdir
        .join("usr/share/doc")
        .join(&facts.package)
        .join("bundled");
    for (lib, package) in files::BUNDLED {
        match dirs
            .iter()
            .map(|dir| dir.join(lib))
            .find(|path| path.exists())
        {
            Some(path) => {
                let real = std::fs::canonicalize(&path).unwrap_or(path);
                desktop::copy_file(&real, &appdir.join("usr/lib").join(lib), 0o644)?;
                let copyright = PathBuf::from("/usr/share/doc")
                    .join(package)
                    .join("copyright");
                if copyright.is_file() {
                    desktop::copy_file(
                        &copyright,
                        &doc.join(format!("{package}.copyright")),
                        0o644,
                    )?;
                }
            }
            None => missing.push(*lib),
        }
    }
    rel.check(
        ctx,
        if missing.is_empty() {
            Check::pass(
                CheckId::LinuxAppimageLibs,
                format!(
                    "bundled {}",
                    files::BUNDLED.iter().map(|(l, _)| *l).collect::<Vec<_>>().join(", ")
                ),
            )
        } else {
            Check::warn(
                CheckId::LinuxAppimageLibs,
                format!(
                    "the build host lacks {} (looked in {}); the AppImage uses the running system's copy",
                    missing.join(", "),
                    dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", ")
                ),
            )
        },
    );
    fix_modes(&appdir)?;

    let appimage = rel.dist.join(format!(
        "{}-{}-{arch}.AppImage",
        desktop::file_stem(&facts.name).replace(' ', "_"),
        rel.version
    ));
    let _ = run(
        ctx,
        "appimagetool",
        &Cmd::new(&appimagetool.path)
            .arg("--appimage-extract-and-run")
            .arg("--runtime-file")
            .arg(&runtime.path)
            .arg(&appdir)
            .arg(&appimage)
            .env("ARCH", arch)
            .timeout(Duration::from_secs(900)),
    )?;
    if !appimage.is_file() {
        return Err(IcmError::new(
            CheckId::ToolFailed,
            format!(
                "appimagetool exited 0 but wrote no {}",
                crate::paths::display(&appimage)
            ),
        ));
    }
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&appimage, std::fs::Permissions::from_mode(0o755));
    }
    if let Some(version) = appimagetool.version {
        rel.tool("appimagetool", version);
    }
    rel.embed_notices(
        &appimage,
        &format!("usr/share/doc/{}/{}", facts.package, super::notices::FILE),
    )?;
    Ok(appimage)
}

fn build(ctx: &mut Ctx, rel: &mut Release) -> Result<()> {
    let bin = rel
        .project
        .bin_for(super::ledger::platform_key(ReleaseTarget::Linux))?;
    let invocation = rel.invocation("build", Select::Bin(bin.clone()), None);
    let output = rel.cargo(ctx, "cargo.build", &invocation, &[], None)?;
    let exe = output
        .executable(&bin)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| rel.artifacts_dir(None).join(&bin));
    let floor = rel.config().desktop.linux.glibc_floor.clone();
    rel.check(ctx, glibc_check(&exe, &floor));
    let image = elf::read(&exe).map_err(|error| {
        IcmError::new(
            CheckId::BuildWrongPlatform,
            format!("cannot read the built executable: {error}"),
        )
        .evidence(Evidence::file(&exe))
    })?;
    let (Some(deb_arch), Some(arch)) = (image.deb_arch(), image.appimage_arch()) else {
        return Err(IcmError::new(
            CheckId::BuildWrongPlatform,
            format!(
                "{} is for {}, which the Linux packages do not support (x86-64 and AArch64 only)",
                crate::paths::display(&exe),
                elf::machine_name(image.machine)
            ),
        ));
    };
    let facts = facts(rel, deb_arch)?;
    let icon = icons::load(&rel.project)?;
    let notices = rel.notices(ctx, None)?;

    let mut built = Vec::new();
    if wants(rel, "deb") {
        let deb = build_deb(ctx, rel, &facts, &exe, &icon, &notices)?;
        rel.add_file("upload", "deb", &deb)?;
        built.push(deb);
    }
    if wants(rel, "appimage") {
        let appimage = build_appimage(ctx, rel, &facts, &exe, &icon, &notices, arch)?;
        rel.add_file("upload", "appimage", &appimage)?;
        built.push(appimage);
    }
    let names: Vec<String> = built
        .iter()
        .map(|path| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    rel.owner_plan = Some(owner_plans::desktop(&rel.common(), &refs));
    // Linux packages carry no signature: under --sign auto they are as
    // signed as their distribution needs.
    rel.signed = rel.sign() == crate::cli::SignMode::Auto;
    Ok(())
}

fn plan(rel: &Release) -> Plan {
    let mut plan = Plan::new();
    let bin = rel
        .project
        .bin_for(super::ledger::platform_key(ReleaseTarget::Linux))
        .unwrap_or_else(|_| rel.package.name.clone());
    plan.push(Step::internal(
        "linux.host",
        "a Linux host (else env.unsupported_host); [desktop.linux] maintainer; dpkg-deb, dpkg-shlibdeps; the pinned appimagetool and runtime",
    ));
    plan.push(
        Step::exec(
            "cargo.build",
            rel.invocation("build", Select::Bin(bin.clone()), None)
                .cmd(),
        )
        .gate(CheckId::LinuxGlibcFloor)
        .on_fail(CheckId::BuildCompileError),
    );
    let package = deb_package(rel);
    if wants(rel, "deb") {
        let root = rel.gen_dir.join("deb/root");
        plan.push(Step::internal(
            "linux.deb.layout",
            &format!(
                "lay out {}: usr/bin/{bin}, the .desktop entry, hicolor icons, usr/share/doc/{package}/THIRD_PARTY_NOTICES.txt, DEBIAN/control",
                crate::paths::display(&root)
            ),
        ));
        plan.push(Step::exec(
            "dpkg-shlibdeps",
            Cmd::tool("dpkg-shlibdeps")
                .arg("-O")
                .arg(format!("-e{}", root.join("usr/bin").join(&bin).display())),
        ));
        plan.push(
            Step::exec(
                "dpkg-deb.build",
                Cmd::tool("dpkg-deb")
                    .args(["-Zxz", "--build", "--root-owner-group"])
                    .arg(&root)
                    .arg(rel.dist.join(format!(
                        "{package}_{}-{}_<arch>.deb",
                        rel.version, rel.build
                    ))),
            )
            .gate(CheckId::LinuxDesktopFile)
            .gate(CheckId::LinuxDebLint),
        );
    }
    if wants(rel, "appimage") {
        let appdir = rel.gen_dir.join("appimage/AppDir");
        plan.push(
            Step::internal(
                "linux.appimage.layout",
                &format!(
                    "lay out {}: AppRun, the .desktop entry and icon, usr/, the bundled libxkbcommon(-x11) and libwayland-cursor",
                    crate::paths::display(&appdir)
                ),
            )
            .gate(CheckId::LinuxAppimageLibs),
        );
        plan.push(Step::exec(
            "appimagetool",
            Cmd::tool("appimagetool")
                .arg("--appimage-extract-and-run")
                .arg("--runtime-file")
                .arg("<pinned appimage-runtime>")
                .arg(&appdir)
                .arg(rel.dist.join(format!(
                    "{}-{}-<arch>.AppImage",
                    rel.config().app.name,
                    rel.version
                ))),
        ));
    }
    plan
}

impl Pipeline for Linux {
    fn plan(&self, _ctx: &Ctx, rel: &Release) -> Result<Plan> {
        Ok(plan(rel))
    }

    fn preconditions(&self, ctx: &mut Ctx, rel: &mut Release) -> Result<()> {
        desktop::require_host(&ctx.env, ReleaseTarget::Linux, "release linux")?;
        if wants(rel, "deb") {
            if rel.config().desktop.linux.maintainer.is_none() {
                let error = IcmError::new(
                    CheckId::ConfigOwnerDecision,
                    format!(
                        "{}: [desktop.linux] maintainer is unset; a .deb names who maintains it (`Name <email>`)",
                        rel.project.config.source.location_for("desktop.linux")
                    ),
                )
                .evidence(rel.project.config.evidence("desktop.linux"))
                .fix(
                    "The owner sets [desktop.linux] maintainer = \"Name <email>\" in icm.toml.",
                    &[],
                );
                rel.needs_owner(ctx, error);
            }
            let package = deb_package(rel);
            if !files::is_deb_package_name(&package) {
                return Err(IcmError::new(
                    CheckId::ConfigInvalid,
                    format!("the Debian package name `{package}` is invalid: lower-case letters, digits, `+`, `-` and `.`, starting with a letter or digit"),
                )
                .evidence(rel.project.config.evidence("desktop.linux")));
            }
            for (tool, package) in [("dpkg-deb", "dpkg"), ("dpkg-shlibdeps", "dpkg-dev")] {
                if which(&ctx.env, tool).is_none() {
                    return Err(apt_missing(
                        tool,
                        package,
                        "[desktop.linux] formats has \"deb\"",
                    ));
                }
            }
        }
        if wants(rel, "appimage") {
            let _ = crate::pinned::require(ctx, "appimagetool")?;
            let _ = crate::pinned::require(ctx, "appimage-runtime")?;
        }
        Ok(())
    }

    fn build(&self, ctx: &mut Ctx, rel: &mut Release) -> Result<()> {
        build(ctx, rel)
    }

    fn verify(&self, ctx: &mut Ctx, verify: &mut Verify) -> Result<()> {
        let Some(artifact) = verify.artifact.clone() else {
            return Ok(());
        };
        let name = artifact
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let base = verify
            .project
            .as_ref()
            .map(|project| project.icm_dir.join("tmp"))
            .unwrap_or_else(|| crate::paths::cache_dir().join("tmp"));
        let scratch = base.join(format!("verify-linux-{}", std::process::id()));
        desktop::fresh_dir(&scratch)?;
        let result = verify_unpacked(ctx, verify, &artifact, &name, &scratch);
        let _ = std::fs::remove_dir_all(&scratch);
        result
    }
}

fn verify_unpacked(
    ctx: &mut Ctx,
    verify: &mut Verify,
    artifact: &Path,
    name: &str,
    scratch: &Path,
) -> Result<()> {
    let root = if name.ends_with(".deb") {
        let members = ar::read(artifact).map_err(|error| {
            IcmError::new(
                CheckId::UsageBadArgs,
                format!("{name} is not a .deb: {error}"),
            )
            .evidence(Evidence::file(artifact))
        })?;
        let Some(data) = members.iter().find(|m| m.name.starts_with("data.tar")) else {
            return Err(IcmError::new(
                CheckId::UsageBadArgs,
                format!("{name} has no data.tar member"),
            )
            .evidence(Evidence::file(artifact)));
        };
        let tarball = scratch.join(&data.name);
        desktop::write_file(&tarball, &data.data, 0o644)?;
        let root = scratch.join("root");
        std::fs::create_dir_all(&root).map_err(|e| desktop::io_error("create", &root, e))?;
        let _ = run(
            ctx,
            "tar.extract",
            &Cmd::tool("tar")
                .arg("-xf")
                .arg(&tarball)
                .arg("-C")
                .arg(&root),
        )?;
        root
    } else if name.ends_with(".AppImage") {
        if desktop::host_os(&ctx.env) != "linux" {
            verify.check(
                ctx,
                Check::skip(
                    CheckId::LinuxGlibcFloor,
                    format!(
                        "{name}: an AppImage unpacks only on Linux (--appimage-extract); this host is {}",
                        desktop::host_os(&ctx.env)
                    ),
                ),
            );
            return Ok(());
        }
        let _ = run(
            ctx,
            "appimage.extract",
            &Cmd::new(artifact)
                .arg("--appimage-extract")
                .cwd(scratch)
                .timeout(Duration::from_secs(300)),
        )?;
        scratch.join("squashfs-root")
    } else {
        return Err(IcmError::new(
            CheckId::UsageBadArgs,
            format!("{name} is not a .deb or an .AppImage"),
        )
        .evidence(Evidence::file(artifact)));
    };

    let floor = verify
        .project
        .as_ref()
        .map(|project| project.config.config.desktop.linux.glibc_floor.clone())
        .unwrap_or_else(|| "2.35".to_string());
    let bin_dir = root.join("usr/bin");
    let executables: Vec<PathBuf> = std::fs::read_dir(&bin_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    if executables.is_empty() {
        verify.check(
            ctx,
            Check::fail(
                CheckId::LinuxGlibcFloor,
                format!("{name} has no executable in usr/bin"),
            )
            .evidence(Evidence::file(artifact)),
        );
    }
    for exe in &executables {
        verify.check(ctx, glibc_check(exe, &floor));
    }
    let entries: Vec<PathBuf> = std::fs::read_dir(root.join("usr/share/applications"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "desktop"))
        .collect();
    if entries.is_empty() {
        verify.check(
            ctx,
            Check::fail(
                CheckId::LinuxDesktopFile,
                format!("{name} installs no .desktop entry"),
            )
            .evidence(Evidence::file(artifact)),
        );
    }
    for entry in entries {
        for check in desktop_file_checks(ctx, &entry)? {
            verify.check(ctx, check);
        }
    }
    let notices = desktop::files_under(&root.join("usr/share/doc"))
        .into_iter()
        .any(|path| path.file_name().is_some_and(|f| f == super::notices::FILE));
    verify.check(
        ctx,
        if notices {
            Check::pass(
                CheckId::ReleaseNotices,
                format!("{name} carries {}", super::notices::FILE),
            )
        } else {
            Check::fail(
                CheckId::ReleaseNotices,
                format!(
                    "{name} carries no {} under usr/share/doc",
                    super::notices::FILE
                ),
            )
            .evidence(Evidence::file(artifact))
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Status;

    #[test]
    fn the_glibc_floor_is_a_ceiling_on_needs() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        std::fs::write(
            &old,
            elf::synthetic(
                elf::EM_X86_64,
                &[("libc.so.6", &["GLIBC_2.2.5", "GLIBC_2.34"])],
            ),
        )
        .unwrap();
        assert_eq!(glibc_check(&old, "2.35").status, Status::Pass);
        let new = dir.path().join("new");
        std::fs::write(
            &new,
            elf::synthetic(elf::EM_X86_64, &[("libc.so.6", &["GLIBC_2.39"])]),
        )
        .unwrap();
        let check = glibc_check(&new, "2.35");
        assert_eq!(check.status, Status::Fail);
        assert!(check.error.detail.contains("needs glibc 2.39, above"));
        assert_eq!(glibc_check(&new, "2.39").status, Status::Pass);
        let none = dir.path().join("none");
        std::fs::write(&none, elf::synthetic(elf::EM_AARCH64, &[])).unwrap();
        assert_eq!(glibc_check(&none, "2.35").status, Status::Pass);
        let garbage = dir.path().join("garbage");
        std::fs::write(&garbage, "#!/bin/sh\n").unwrap();
        assert_eq!(glibc_check(&garbage, "2.35").status, Status::Fail);
    }
}
