//! The dev APK (design §10.4 steps 2–8): `cargo rustc --lib --crate-type
//! cdylib` with the NDK's linker, the ELF gates, `llvm-strip
//! --strip-debug`, the generated resources and manifest, `aapt2 compile`
//! and `aapt2 link`, a stored ZIP, `zipalign -P 16`, `apksigner` with
//! icm's debug key, and verification of the signed file.

use super::elf;
use super::manifest;
use super::res;
use super::zip;
use super::{DEBUG_KEY_ALIAS, DEBUG_KEYSTORE_PASS, Toolset};
use crate::cargo::{self, Artifact, Invocation, Message, Select};
use crate::catalogue::CheckId;
use crate::config::Abi;
use crate::context::{Ctx, Project};
use crate::error::{Check, Evidence, IcmError, Result};
use crate::process::Cmd;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What [`build`] produced.
#[derive(Clone, Debug)]
pub struct Built {
    /// The signed APK.
    pub apk: PathBuf,
    /// Its ABI.
    pub abi: Abi,
    /// The library name (`lib<lib>.so`).
    pub lib: String,
    /// The unstripped library cargo built.
    pub library: PathBuf,
    /// `dev` or `release`.
    pub profile: String,
}

/// The APK's file name for a package.
pub fn apk_name(package: &str) -> String {
    format!("{package}.apk")
}

/// The signed APK a build of `profile` leaves.
pub fn apk_path(project: &Project, package: &str, profile: &str) -> PathBuf {
    project
        .build_dir("android", profile)
        .join(apk_name(package))
}

fn io_error(what: &str, path: &Path, error: &std::io::Error) -> IcmError {
    IcmError::new(
        CheckId::InternalBug,
        format!("cannot {what} {}: {error}", path.display()),
    )
}

/// Builds, packages, signs and verifies the APK for one ABI.
pub fn build(
    ctx: &Ctx,
    project: &Project,
    tools: &Toolset,
    abi: Abi,
    release: bool,
) -> Result<Built> {
    let profile = if release { "release" } else { "dev" };
    let config = &project.config.config;
    let package = project.package_for("android")?.clone();
    let lib = project.lib_name()?;
    let triple = abi.triple();

    // Fail before a long build when a piece is missing.
    let toolchain = crate::toolchain::active(project.dir())?;
    let target_checks = crate::toolchain::check_targets(&toolchain, &[triple.to_string()]);
    if let Some(failed) = target_checks.iter().find(|check| check.failed()).cloned() {
        return Err(failed.into_error());
    }
    let ndk = tools.ndk.clone()?;
    let _ = tools.jdk.clone()?;
    let (build_tools_version, _) = tools.build_tools()?;
    let jar = tools.platform_jar(config.android.target_sdk)?;
    ctx.rep.set(
        "tools",
        serde_json::json!({
            "rustc": toolchain.rustc_version(),
            "ndk": ndk.version,
            "build_tools": build_tools_version,
            "jdk": tools.jdk.as_ref().ok().map(|jdk| jdk.version.clone()),
            "android_sdk": crate::paths::display(&tools.sdk.root),
        }),
    );

    // 1. The library.
    let mut env: Vec<(String, String)> = tools.child_env().to_vec();
    env.extend(crate::tools::ndk_env(&ndk, triple, config.android.min_sdk));
    let library = cargo_cdylib(ctx, project, &package, &lib, triple, profile, &env)?;

    // 2. ELF gates.
    gate_library(ctx, &library, abi)?;

    let gen_dir = project.gen_dir("android", profile);
    let build_dir = project.build_dir("android", profile);
    for dir in [&gen_dir, &build_dir] {
        std::fs::create_dir_all(dir).map_err(|e| io_error("create", dir, &e))?;
    }

    // 3. Strip debug info into the APK's lib dir.
    let lib_dir = gen_dir.join("lib").join(abi.as_str());
    if gen_dir.join("lib").exists() {
        std::fs::remove_dir_all(gen_dir.join("lib"))
            .map_err(|e| io_error("clean", &gen_dir.join("lib"), &e))?;
    }
    std::fs::create_dir_all(&lib_dir).map_err(|e| io_error("create", &lib_dir, &e))?;
    let stripped = lib_dir.join(format!("lib{lib}.so"));
    let strip = tools
        .llvm_strip()?
        .arg("--strip-debug")
        .arg("-o")
        .arg(&stripped)
        .arg(&library)
        .timeout(Duration::from_secs(300));
    let outcome = ctx.step("llvm-strip", &strip)?;
    if !outcome.success() {
        return Err(ctx.step_failure("llvm-strip", CheckId::ToolFailed, &outcome));
    }

    // 4. Resources and manifest; 5. link.
    let resources = resources(ctx, project, tools, &gen_dir, &package.version, &lib)?;
    for check in resources.checks.clone() {
        ctx.rep.check(check);
    }
    let linked = gen_dir.join("apk");
    link(
        ctx,
        tools,
        &jar,
        &resources,
        &linked,
        config.app.build,
        &package.version,
        if release { Link::Release } else { Link::Debug },
    )?;

    // 6. Package (all stored), align, sign, verify.
    let mut entries = linked_entries(&linked)?;
    entries.push(zip::Entry {
        name: format!("lib/{}/lib{lib}.so", abi.as_str()),
        source: zip::Source::File(stripped.clone()),
    });
    for (path, relative) in collect_resources(project.dir(), &config.app.resources) {
        entries.push(zip::Entry {
            name: format!("assets/{relative}"),
            source: zip::Source::File(path),
        });
    }
    let unaligned = gen_dir.join("unaligned.apk");
    zip::write(&unaligned, &entries).map_err(|e| io_error("write", &unaligned, &e))?;

    let aligned = gen_dir.join("aligned.apk");
    let align = tools
        .build_tool("zipalign")?
        .args(["-f", "-P", "16", "4"])
        .arg(&unaligned)
        .arg(&aligned)
        .timeout(Duration::from_secs(180));
    run_tool(ctx, "zipalign", &align, CheckId::AndroidApkZipalign)?;

    let keystore = ensure_debug_keystore(ctx, tools)?;
    let apk = build_dir.join(apk_name(&package.name));
    let _ = std::fs::remove_file(&apk);
    let sign = tools
        .apksigner()?
        .args(["sign", "--ks"])
        .arg(&keystore)
        .arg("--ks-pass")
        .arg(format!("pass:{DEBUG_KEYSTORE_PASS}"))
        .arg("--key-pass")
        .arg(format!("pass:{DEBUG_KEYSTORE_PASS}"))
        .args(["--ks-key-alias", DEBUG_KEY_ALIAS])
        .args(["--v4-signing-enabled", "false"])
        .arg("--out")
        .arg(&apk)
        .arg(&aligned)
        .timeout(Duration::from_secs(300));
    run_tool(ctx, "apksigner.sign", &sign, CheckId::AndroidApkSignature)?;

    let verify = tools
        .apksigner()?
        .arg("verify")
        .arg(&apk)
        .timeout(Duration::from_secs(300));
    let outcome = ctx.step("apksigner.verify", &verify)?;
    if !outcome.success() {
        return Err(ctx.step_failure("apksigner.verify", CheckId::AndroidApkSignature, &outcome));
    }
    ctx.rep.check(Check::pass(
        CheckId::AndroidApkSignature,
        format!("{} verifies (debug key)", crate::paths::display(&apk)),
    ));

    let check = tools
        .build_tool("zipalign")?
        .args(["-c", "-P", "16", "4"])
        .arg(&apk)
        .timeout(Duration::from_secs(180));
    let outcome = ctx.step("zipalign.check", &check)?;
    if !outcome.success() {
        return Err(ctx.step_failure("zipalign.check", CheckId::AndroidApkZipalign, &outcome));
    }
    ctx.rep.check(Check::pass(
        CheckId::AndroidApkZipalign,
        "native libraries are aligned to 16 KB pages",
    ));

    let _ = std::fs::remove_file(&unaligned);
    let _ = std::fs::remove_file(&aligned);

    Ok(Built {
        apk,
        abi,
        lib,
        library,
        profile: profile.to_string(),
    })
}

/// The generated manifest and resources, compiled for `aapt2 link`.
#[derive(Clone, Debug)]
pub struct Resources {
    /// `AndroidManifest.xml` (text).
    pub manifest: PathBuf,
    /// The compiled resources, in `-R` order: icm's, then `[android] res`.
    pub overlays: Vec<PathBuf>,
    /// Findings about the icon (WARN `app.icon.invalid`), for the caller to
    /// report.
    pub checks: Vec<Check>,
}

/// Writes the manifest and the resource tree into `gen_dir` (design §9.4)
/// and compiles them with `aapt2 compile` (icm's tree only when it
/// changed; `[android] res` each time).
pub fn resources(
    ctx: &Ctx,
    project: &Project,
    tools: &Toolset,
    gen_dir: &Path,
    version: &str,
    lib: &str,
) -> Result<Resources> {
    let config = &project.config.config;
    std::fs::create_dir_all(gen_dir).map_err(|e| io_error("create", gen_dir, &e))?;
    let icon = config
        .app
        .icon
        .as_ref()
        .map(|icon| project.dir().join(icon));
    let generated = res::generate(
        gen_dir,
        &res::Inputs {
            icon,
            background: config.app.background.clone(),
        },
    )?;
    let manifest_text = manifest::manifest(&manifest::Inputs {
        config,
        version,
        lib,
    })?;
    let manifest_path = gen_dir.join("AndroidManifest.xml");
    write_if_changed(&manifest_path, manifest_text.as_bytes())?;

    let res_zip = gen_dir.join("res.zip");
    if generated.changed || !res_zip.is_file() {
        let compile = tools
            .build_tool("aapt2")?
            .arg("compile")
            .arg("--dir")
            .arg(&generated.dir)
            .arg("-o")
            .arg(&res_zip)
            .timeout(Duration::from_secs(180));
        run_tool(ctx, "aapt2.compile", &compile, CheckId::AndroidAapt2Failed)?;
    }
    let mut overlays = vec![res_zip];
    if let Some(user) = config.android.res.as_deref() {
        let user_dir = project.dir().join(user);
        if user_dir.is_dir() {
            let user_zip = gen_dir.join("user-res.zip");
            let compile = tools
                .build_tool("aapt2")?
                .arg("compile")
                .arg("--dir")
                .arg(&user_dir)
                .arg("-o")
                .arg(&user_zip)
                .timeout(Duration::from_secs(180));
            run_tool(
                ctx,
                "aapt2.compile.user",
                &compile,
                CheckId::AndroidAapt2Failed,
            )?;
            overlays.push(user_zip);
        }
    }
    Ok(Resources {
        manifest: manifest_path,
        overlays,
        checks: generated.checks,
    })
}

/// What `aapt2 link` produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// A dev APK's binary XML, with `--debug-mode` (android:debuggable).
    Debug,
    /// A release APK's binary XML.
    Release,
    /// The protobuf format an App Bundle's module takes
    /// (`--proto-format`), never debuggable.
    Proto,
}

/// `aapt2 link --output-to-dir -o <out>` (emptied first) with
/// `android.jar`, the manifest and the overlays, `--version-code` and
/// `--version-name`.
#[allow(clippy::too_many_arguments)]
pub fn link(
    ctx: &Ctx,
    tools: &Toolset,
    jar: &Path,
    resources: &Resources,
    out: &Path,
    build: u64,
    version: &str,
    mode: Link,
) -> Result<()> {
    if out.exists() {
        std::fs::remove_dir_all(out).map_err(|e| io_error("clean", out, &e))?;
    }
    std::fs::create_dir_all(out).map_err(|e| io_error("create", out, &e))?;
    let mut link = tools.build_tool("aapt2")?.arg("link");
    if mode == Link::Proto {
        link = link.arg("--proto-format");
    }
    link = link
        .args(["--output-to-dir", "-o"])
        .arg(out)
        .arg("-I")
        .arg(jar)
        .arg("--manifest")
        .arg(&resources.manifest);
    for overlay in &resources.overlays {
        link = link.arg("-R").arg(overlay);
    }
    link = link
        .args(["--auto-add-overlay", "--replace-version", "--version-code"])
        .arg(build.to_string())
        .arg("--version-name")
        .arg(version);
    if mode == Link::Debug {
        link = link.arg("--debug-mode");
    }
    let name = if mode == Link::Proto {
        "aapt2.link.proto"
    } else {
        "aapt2.link"
    };
    run_tool(
        ctx,
        name,
        &link.timeout(Duration::from_secs(180)),
        CheckId::AndroidAapt2Failed,
    )
}

/// Runs a tool step; a non-zero exit is `id`.
pub fn run_tool(ctx: &Ctx, name: &str, cmd: &Cmd, id: CheckId) -> Result<()> {
    let outcome = ctx.step(name, cmd)?;
    if outcome.success() {
        Ok(())
    } else {
        Err(ctx.step_failure(name, id, &outcome))
    }
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<()> {
    if std::fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    std::fs::write(path, bytes).map_err(|e| io_error("write", path, &e))
}

/// `cargo rustc -p <pkg> --lib --crate-type cdylib --target <triple>`,
/// reported like [`Ctx::cargo`]: diagnostics become events, a failure is
/// `build.compile_error`, `build.link_error` or `build.cargo_failed`.
/// Returns the `.so`.
/// `cargo rustc -p <pkg> --lib --crate-type cdylib --target <triple>`: the
/// command the build runs and `--dry-run` prints.
pub fn cdylib_cmd(
    offline: bool,
    package: &cargo::Package,
    triple: &str,
    profile: &str,
    env: &[(String, String)],
) -> Cmd {
    let mut invocation = Invocation::new("rustc", &package.manifest_path, &package.name);
    invocation.select = Select::Lib;
    invocation.triple = Some(triple.to_string());
    invocation.profile = profile.to_string();
    invocation.offline = offline;
    let mut cmd = invocation
        .cmd()
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    // `--crate-type` is a `cargo rustc` flag; it goes after the subcommand.
    cmd.args.insert(1, "--crate-type".into());
    cmd.args.insert(2, "cdylib".into());
    cmd
}

fn cargo_cdylib(
    ctx: &Ctx,
    project: &Project,
    package: &cargo::Package,
    lib: &str,
    triple: &str,
    profile: &str,
    env: &[(String, String)],
) -> Result<PathBuf> {
    let _ = project;
    let cmd = cdylib_cmd(ctx.global.offline, package, triple, profile, env);

    let rep = ctx.rep.clone();
    let mut artifacts: Vec<Artifact> = Vec::new();
    let mut on_line = |line: &str| match cargo::parse_message(line) {
        Some(Message::Artifact(artifact)) => artifacts.push(artifact),
        Some(Message::Diagnostic(diagnostic)) => rep.diagnostic(diagnostic),
        _ => {}
    };
    let name = "cargo.rustc";
    let outcome = ctx.step_with(name, &cmd, Some(&mut on_line))?;

    if !outcome.success() {
        let diagnostics = ctx.rep.error_diagnostics();
        let stderr = outcome.stderr_text();
        let id = if cargo::is_link_failure(&stderr, &diagnostics) {
            CheckId::BuildLinkError
        } else if diagnostics.is_empty() {
            CheckId::BuildCargoFailed
        } else {
            CheckId::BuildCompileError
        };
        let mut error = ctx.step_failure(name, id, &outcome);
        if let Some(first) = diagnostics.first() {
            error.detail = format!(
                "{name}: {} error(s); first: {}{}",
                diagnostics.len(),
                first.message,
                match (&first.file, first.line) {
                    (Some(file), Some(line)) => format!(" at {file}:{line}"),
                    _ => String::new(),
                }
            );
        }
        if id == CheckId::BuildLinkError {
            error = error.cause(
                "the NDK's linker failed: check `icm print env android` and the NDK (r28 or newer)",
            );
        }
        return Err(error);
    }

    let crate_name = lib.replace('-', "_");
    artifacts
        .iter()
        .filter(|artifact| artifact.target_name.replace('-', "_") == crate_name)
        .flat_map(|artifact| artifact.filenames.iter())
        .find(|path| path.extension().is_some_and(|ext| ext == "so"))
        .cloned()
        .ok_or_else(|| {
            IcmError::new(
                CheckId::ToolFailed,
                format!("cargo built no lib{crate_name}.so for {triple}"),
            )
            .evidence(Evidence::file(&package.manifest_path))
        })
}

/// The ELF gates (design §10.4 step 5): the activity's entry point is
/// exported, the machine matches the ABI, and every `PT_LOAD` segment is
/// aligned to 16 KB pages (Google Play's rule since November 2025).
fn gate_library(ctx: &Ctx, library: &Path, abi: Abi) -> Result<()> {
    let facts = elf::read(library).map_err(|error| {
        IcmError::new(
            CheckId::AndroidSoAbis,
            format!("cannot read {}: {error}", library.display()),
        )
        .evidence(Evidence::file(library))
    })?;
    let shown = crate::paths::display(library);

    let expected = elf::machine_for_abi(abi.as_str());
    if expected != Some(facts.machine) {
        return Err(IcmError::new(
            CheckId::AndroidSoAbis,
            format!(
                "{shown} is {}, but the device needs {}",
                elf::machine_name(facts.machine),
                abi.as_str()
            ),
        )
        .evidence(Evidence::file(library)));
    }

    let entry = "ANativeActivity_onCreate";
    if !facts.exports(entry) {
        return Err(IcmError::new(
            CheckId::AndroidSoExport,
            format!("{shown} does not export {entry}, so NativeActivity cannot start it"),
        )
        .evidence(Evidence::file(library))
        .cause("src/lib.rs lacks `iced::android_main!(run);`, or iced's `android-native-activity` feature is off"));
    }
    ctx.rep.check(Check::pass(
        CheckId::AndroidSoExport,
        format!(
            "{shown} exports {entry} ({})",
            elf::machine_name(facts.machine)
        ),
    ));

    let align = facts.min_load_align();
    if align >= 0x4000 {
        ctx.rep.check(Check::pass(
            CheckId::AndroidSoAlign16k,
            format!("every PT_LOAD segment is aligned to {align:#x}"),
        ));
    } else {
        // A 4 KB library still runs on 4 KB devices: report, do not block.
        ctx.rep.check(
            Check::fail(
                CheckId::AndroidSoAlign16k,
                format!("a PT_LOAD segment of {shown} is aligned to {align:#x}, below 16 KB"),
            )
            .evidence(Evidence::file(library)),
        );
    }
    Ok(())
}

/// The linked files as entries: `AndroidManifest.xml` first, then
/// `resources.arsc`, then the rest in sorted order.
fn linked_entries(dir: &Path) -> Result<Vec<zip::Entry>> {
    let mut files = Vec::new();
    walk(dir, dir, &mut files).map_err(|e| io_error("read", dir, &e))?;
    files.sort_by(|(_, a), (_, b)| {
        let rank = |name: &str| match name {
            "AndroidManifest.xml" => 0,
            "resources.arsc" => 1,
            _ => 2,
        };
        rank(a).cmp(&rank(b)).then_with(|| a.cmp(b))
    });
    if !files.iter().any(|(_, name)| name == "AndroidManifest.xml") {
        return Err(IcmError::new(
            CheckId::AndroidAapt2Failed,
            format!(
                "aapt2 link left no AndroidManifest.xml in {}",
                dir.display()
            ),
        ));
    }
    Ok(files
        .into_iter()
        .map(|(path, name)| zip::Entry {
            name,
            source: zip::Source::File(path),
        })
        .collect())
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, String)>) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            // Build output and VCS data are never resources.
            let name = entry.file_name();
            if dir == root && (name == "target" || name.to_string_lossy().starts_with('.')) {
                continue;
            }
            walk(root, &path, out)?;
        } else if let Ok(relative) = path.strip_prefix(root) {
            let name = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.push((path, name));
        }
    }
    Ok(())
}

/// Whether a path component matches a pattern with `*` and `?`.
fn component_matches(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Whether a `/`-separated relative path matches a glob (`*`, `?` within a
/// component, `**` across components).
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    fn go(pattern: &[&str], path: &[&str]) -> bool {
        match (pattern.first(), path.first()) {
            (None, None) => true,
            (Some(&"**"), _) => {
                go(&pattern[1..], path) || (!path.is_empty() && go(pattern, &path[1..]))
            }
            (Some(p), Some(n)) => component_matches(p, n) && go(&pattern[1..], &path[1..]),
            _ => false,
        }
    }
    let pattern: Vec<&str> = pattern.split('/').filter(|p| !p.is_empty()).collect();
    let path: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    go(&pattern, &path)
}

/// The files `[app] resources` names, relative to the project directory
/// (they go to `assets/<relative path>`). A plain directory takes every
/// file under it.
pub fn collect_resources(project_dir: &Path, patterns: &[String]) -> Vec<(PathBuf, String)> {
    if patterns.is_empty() {
        return Vec::new();
    }
    let mut all = Vec::new();
    let _ = walk(project_dir, project_dir, &mut all);
    let mut out: Vec<(PathBuf, String)> = all
        .into_iter()
        .filter(|(_, relative)| {
            !relative.starts_with("target/")
                && patterns.iter().any(|pattern| {
                    let pattern = pattern.trim_start_matches("./").trim_end_matches('/');
                    glob_matches(pattern, relative) || relative.starts_with(&format!("{pattern}/"))
                })
        })
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out.dedup_by(|a, b| a.1 == b.1);
    out
}

/// Creates icm's debug keystore once (`keytool -genkeypair`, the Android
/// debug key's conventional alias and passwords) and returns its path.
pub fn ensure_debug_keystore(ctx: &Ctx, tools: &Toolset) -> Result<PathBuf> {
    let keystore = super::debug_keystore();
    if keystore.is_file() {
        return Ok(keystore);
    }
    let dir = keystore.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| io_error("create", dir, &e))?;
    // Another icm may create it at the same time: write aside, then rename.
    let temporary = dir.join(format!("debug.keystore.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&temporary);
    let cmd = tools
        .keytool()?
        .args(["-genkeypair", "-keystore"])
        .arg(&temporary)
        .args(["-storetype", "PKCS12", "-storepass", DEBUG_KEYSTORE_PASS])
        .args(["-alias", DEBUG_KEY_ALIAS, "-keypass", DEBUG_KEYSTORE_PASS])
        .args(["-keyalg", "RSA", "-keysize", "2048", "-validity", "10000"])
        .args(["-dname", "CN=Android Debug,O=Android,C=US"])
        .timeout(Duration::from_secs(120));
    let outcome = ctx.step("keytool.debug_keystore", &cmd)?;
    if !outcome.success() {
        let _ = std::fs::remove_file(&temporary);
        return Err(ctx.step_failure("keytool.debug_keystore", CheckId::ToolFailed, &outcome));
    }
    if keystore.is_file() {
        let _ = std::fs::remove_file(&temporary);
    } else {
        std::fs::rename(&temporary, &keystore).map_err(|e| io_error("move", &keystore, &e))?;
    }
    ctx.rep.progress(format!(
        "created the debug keystore {}",
        crate::paths::display(&keystore)
    ));
    Ok(keystore)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_matches("assets/*.ttf", "assets/a.ttf"));
        assert!(!glob_matches("assets/*.ttf", "assets/fonts/a.ttf"));
        assert!(glob_matches("assets/**/*.ttf", "assets/fonts/a.ttf"));
        assert!(glob_matches("assets/**/*.ttf", "assets/a.ttf"));
        assert!(glob_matches("data/?.json", "data/x.json"));
        assert!(!glob_matches("data/?.json", "data/xy.json"));
        assert!(glob_matches("README.md", "README.md"));
    }

    #[test]
    fn resources_are_collected_relative_to_the_project() {
        let dir = tempfile::tempdir().unwrap();
        for file in [
            "assets/a.txt",
            "assets/sub/b.txt",
            "other/c.txt",
            "target/x.txt",
        ] {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"x").unwrap();
        }
        let found: Vec<String> =
            collect_resources(dir.path(), &["assets".into(), "**/c.txt".into()])
                .into_iter()
                .map(|(_, relative)| relative)
                .collect();
        assert_eq!(
            found,
            vec!["assets/a.txt", "assets/sub/b.txt", "other/c.txt"]
        );
        assert!(collect_resources(dir.path(), &[]).is_empty());
    }

    #[test]
    fn linked_files_put_the_manifest_first() {
        let dir = tempfile::tempdir().unwrap();
        for file in ["res/a.png", "resources.arsc", "AndroidManifest.xml"] {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"x").unwrap();
        }
        let names: Vec<String> = linked_entries(dir.path())
            .unwrap()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(
            names,
            vec!["AndroidManifest.xml", "resources.arsc", "res/a.png"]
        );
    }

    #[test]
    fn library_gates() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.so");
        std::fs::write(
            &good,
            elf::synthetic(elf::EM_AARCH64, 0x4000, &["ANativeActivity_onCreate"]),
        )
        .unwrap();
        let facts = elf::read(&good).unwrap();
        assert!(facts.exports("ANativeActivity_onCreate"));
        assert_eq!(
            elf::machine_for_abi(Abi::Arm64V8a.as_str()),
            Some(facts.machine)
        );
    }
}
