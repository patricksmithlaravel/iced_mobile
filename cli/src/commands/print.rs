//! `icm print config | env <platform> | paths | tools | plan <cmd…> | commands`.
//!
//! In human mode the content is stdout (so `eval "$(icm print env android)"`
//! works); with `--json` it is a field of the result.

use crate::catalogue::CheckId;
use crate::cli::{Cli, Platform, PrintArgs, PrintWhat};
use crate::config::{self, Abi};
use crate::context::Ctx;
use crate::error::{IcmError, Result};
use crate::process::shell_quote;
use crate::tools;
use clap::{CommandFactory, Parser};
use serde_json::{Map, Value, json};

/// Runs `icm print`.
pub fn run(ctx: &mut Ctx, args: PrintArgs) -> Result<()> {
    match args.what {
        PrintWhat::Config => config(ctx),
        PrintWhat::Env { platform } => env(ctx, platform),
        PrintWhat::Paths => paths(ctx),
        PrintWhat::Tools => tools_report(ctx),
        PrintWhat::Plan { command } => plan(ctx, command),
        PrintWhat::Commands => commands(ctx),
        PrintWhat::Policy => policy(ctx),
    }
}

/// `icm print policy`: the embedded store policy table (design §12.0) with
/// the value of each rule in force today.
fn policy(ctx: &mut Ctx) -> Result<()> {
    let policy = crate::policy::get();
    let today = crate::time::Day::today();
    ctx.rep.set("policy", policy.to_json(today));
    for check in policy.checks(today) {
        if check.status != crate::error::Status::Pass {
            ctx.rep.check(check);
        }
    }
    ctx.rep.summary(format!(
        "store policy table reviewed {} ({} rules)",
        policy.reviewed,
        policy.rules.len()
    ));
    ctx.rep.content(policy.to_text(today));
    Ok(())
}

fn config(ctx: &mut Ctx) -> Result<()> {
    let project = ctx.project()?;
    let value = serde_json::to_value(&project.config.config).map_err(|error| {
        IcmError::new(
            CheckId::InternalBug,
            format!("cannot serialize the config: {error}"),
        )
    })?;
    let path = crate::paths::display(&project.config.path);
    let text = serde_json::to_string_pretty(&value).unwrap_or_default();
    ctx.rep.set("config", value);
    ctx.rep.set("config_path", json!(path));
    ctx.rep.summary(format!("{path} is valid"));
    ctx.rep.content(text);
    Ok(())
}

/// Whether there is an icm.toml to read (a broken one is still an error).
fn has_config(ctx: &Ctx) -> bool {
    let cwd = std::env::current_dir().unwrap_or_default();
    config::locate(ctx.global.config.as_deref(), &cwd).is_ok()
}

fn env(ctx: &mut Ctx, platform: Platform) -> Result<()> {
    let host = ctx.host()?.clone();
    let project = if has_config(ctx) {
        Some(ctx.project()?.clone())
    } else {
        None
    };
    let settings = project.as_ref().map(|p| p.config.config.clone());

    let mut vars: Vec<(String, String)> = vec![("RUSTUP_AUTO_INSTALL".into(), "0".into())];
    let mut problems: Vec<IcmError> = Vec::new();

    match platform {
        Platform::Android => {
            let jdk = tools::jdk(&host, &ctx.env, true)
                .map_err(|e| problems.push(e))
                .ok();
            let sdk = tools::android_sdk(&host, &ctx.env)
                .map_err(|e| problems.push(e))
                .ok();
            let ndk = tools::ndk(sdk.as_ref(), &host, &ctx.env)
                .map_err(|e| problems.push(e))
                .ok();
            let base_path = ctx.env.var("PATH").unwrap_or("").to_string();
            vars.extend(tools::android_env(
                jdk.as_ref(),
                sdk.as_ref(),
                ndk.as_ref(),
                &base_path,
            ));
            if let Some(ndk) = &ndk {
                let (abis, min_sdk) = match &settings {
                    Some(config) => (config.android.abis.clone(), config.android.min_sdk),
                    None => (vec![Abi::Arm64V8a, Abi::X86_64], 26),
                };
                for abi in abis {
                    for (key, value) in tools::ndk_env(ndk, abi.triple(), min_sdk) {
                        if !vars.iter().any(|(k, _)| *k == key) {
                            vars.push((key, value));
                        }
                    }
                }
            }
        }
        Platform::IosSim | Platform::IosDevice => {
            let min_os = settings
                .as_ref()
                .map_or_else(|| "16.0".to_string(), |c| c.ios.min_os.clone());
            vars.push(("IPHONEOS_DEPLOYMENT_TARGET".into(), min_os));
            match tools::xcode(&ctx.env) {
                Ok(xcode) => vars.push((
                    "DEVELOPER_DIR".into(),
                    xcode.developer_dir.display().to_string(),
                )),
                Err(error) => problems.push(error),
            }
        }
        Platform::Desktop => {
            if cfg!(target_os = "macos") {
                let min_os = settings
                    .as_ref()
                    .map_or_else(|| "12.0".to_string(), |c| c.desktop.macos.min_os.clone());
                vars.push(("MACOSX_DEPLOYMENT_TARGET".into(), min_os));
            }
        }
        Platform::Web => {
            if let Some(config) = &settings
                && !config.web.rustflags.is_empty()
            {
                vars.push((
                    "CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS".into(),
                    config.web.rustflags.join(" "),
                ));
            }
        }
    }

    let text: String = vars
        .iter()
        .map(|(key, value)| format!("export {key}={}\n", shell_quote(value)))
        .collect();
    let map: Map<String, Value> = vars
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    ctx.rep.set("env", Value::Object(map));
    ctx.rep.content(text);

    if problems.is_empty() {
        ctx.rep.summary(format!(
            "{} variables for {}",
            vars.len(),
            platform.as_str()
        ));
        Ok(())
    } else {
        Err(ctx.report_all(problems))
    }
}

fn paths(ctx: &mut Ctx) -> Result<()> {
    let project = ctx.project()?.clone();
    let show = |path: &std::path::Path| json!(crate::paths::display(path));
    let paths = json!({
        "config": show(&project.config.path),
        "project_dir": show(project.dir()),
        "workspace_root": show(&project.metadata.workspace_root),
        "target_dir": show(&project.target_dir),
        "icm_dir": show(&project.icm_dir),
        "runs": show(&project.runs_dir()),
        "latest": show(&project.icm_dir.join("latest")),
        "gen": show(&project.icm_dir.join("gen")),
        "build": show(&project.icm_dir.join("build")),
        "sessions": show(&project.sessions_dir()),
        "locks": show(&project.locks_dir()),
        "last": show(&project.icm_dir.join("last.json")),
        "cargo_lock": show(&project.lock_path()),
        "host_config": crate::paths::host_config().map(|p| crate::paths::display(&p)),
        "cache": show(&crate::paths::cache_dir()),
        "tools": show(&crate::paths::tools_dir()),
    });
    let text = paths
        .as_object()
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| format!("{key:15} {}\n", value.as_str().unwrap_or("-")))
                .collect::<String>()
        })
        .unwrap_or_default();
    ctx.rep.set("paths", paths);
    ctx.rep.content(text);
    Ok(())
}

fn tools_report(ctx: &mut Ctx) -> Result<()> {
    let host = ctx.host()?.clone();
    let env = ctx.env.clone();
    let mut report = Map::new();
    let mut lines = String::new();

    let mut record = |name: &str, found: std::result::Result<Value, IcmError>| match found {
        Ok(value) => {
            lines.push_str(&format!(
                "{name:13} {}{}{}\n",
                value["path"].as_str().unwrap_or(""),
                value["version"]
                    .as_str()
                    .map(|v| format!("  {v}"))
                    .unwrap_or_default(),
                value["source"]
                    .as_str()
                    .map(|s| format!("  ({s})"))
                    .unwrap_or_default(),
            ));
            let _ = report.insert(name.to_string(), value);
        }
        Err(error) => {
            lines.push_str(&format!(
                "{name:13} MISSING  {}: {}\n",
                error.id, error.detail
            ));
            let _ = report.insert(
                name.to_string(),
                json!({"missing": {"id": error.id, "detail": error.detail, "fix": error.fix}}),
            );
        }
    };

    let dir = ctx
        .try_project()
        .map(|p| p.dir().to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    record(
        "rust",
        crate::toolchain::active(&dir).map(|toolchain| {
            json!({
                "path": toolchain.sysroot,
                "version": toolchain.rustc_version(),
                "source": toolchain.name.clone().unwrap_or_else(|| "rustc".into()),
                "toolchain": toolchain.name,
                "reason": toolchain.reason,
                "targets": toolchain.installed_targets(),
            })
        }),
    );

    if cfg!(target_os = "macos") {
        record(
            "xcode",
            tools::xcode(&env).map(|xcode| {
                json!({
                    "path": xcode.developer_dir,
                    "version": xcode.display(),
                    "source": xcode.source,
                    "beta": xcode.beta,
                })
            }),
        );
    }

    let sdk = tools::android_sdk(&host, &env);
    record(
        "android_sdk",
        sdk.clone()
            .map(|sdk| json!({"path": sdk.root, "source": sdk.source})),
    );
    if let Ok(sdk) = &sdk {
        for (name, path) in [
            ("adb", Some(sdk.adb(&env))),
            ("emulator", Some(sdk.emulator(&env))),
            ("sdkmanager", sdk.sdkmanager(&env)),
            ("avdmanager", sdk.avdmanager(&env)),
        ] {
            record(
                name,
                match path.filter(|p| p.exists()) {
                    Some(path) => Ok(json!({"path": path, "source": "android sdk"})),
                    None => Err(IcmError::new(
                        CheckId::EnvAndroidPackageMissing,
                        format!("{name} is not in the SDK at {}", sdk.root.display()),
                    )),
                },
            );
        }
        record(
            "build_tools",
            sdk.build_tools(35)
                .map(|(version, dir)| json!({"path": dir, "version": version, "source": "android sdk"}))
                .ok_or_else(|| {
                    IcmError::new(
                        CheckId::EnvAndroidPackageMissing,
                        "no build-tools 35 or newer in the SDK",
                    )
                }),
        );
    }
    record(
        "ndk",
        tools::ndk(sdk.as_ref().ok(), &host, &env)
            .map(|ndk| json!({"path": ndk.root, "version": ndk.version, "source": ndk.source})),
    );
    record(
        "jdk",
        tools::jdk(&host, &env, true)
            .map(|jdk| json!({"path": jdk.home, "version": jdk.version, "source": jdk.source})),
    );
    record(
        "chrome",
        tools::chrome(&host, &env).map(|found| json!(found)),
    );
    // The pinned tools releases download (tools.toml).
    for (name, found) in crate::pinned::report(&env) {
        record(&name, found);
    }

    ctx.rep.set("tools", Value::Object(report));
    ctx.rep.content(lines);
    Ok(())
}

fn plan(ctx: &mut Ctx, command: Vec<String>) -> Result<()> {
    let mut argv = vec!["icm".to_string()];
    argv.extend(command);
    argv.push("--dry-run".to_string());
    let cli = Cli::try_parse_from(&argv).map_err(|error| {
        IcmError::new(
            CheckId::UsageBadArgs,
            crate::usage_detail(&error.render().to_string()),
        )
    })?;
    if cli.command.is_view() {
        return Err(IcmError::new(
            CheckId::UsageBadArgs,
            "`print plan` takes a command that does work (run, build, check, ...)",
        ));
    }
    ctx.global.dry_run = true;
    super::dispatch(ctx, cli.command)
}

fn commands(ctx: &mut Ctx) -> Result<()> {
    let command = Cli::command();
    let value = describe(&command);
    let mut lines = String::new();
    list(&command, "icm", &mut lines);
    ctx.rep.set("commands", value);
    ctx.rep.content(lines);
    Ok(())
}

fn describe(command: &clap::Command) -> Value {
    let args: Vec<Value> = command
        .get_arguments()
        .filter(|arg| !arg.is_hide_set() && arg.get_id() != "help" && arg.get_id() != "version")
        .map(|arg| {
            json!({
                "name": arg.get_id().as_str(),
                "long": arg.get_long(),
                "short": arg.get_short().map(|c| c.to_string()),
                "positional": arg.is_positional(),
                "required": arg.is_required_set(),
                "global": arg.is_global_set(),
                "takes_value": arg.get_action().takes_values(),
                "help": arg.get_help().map(ToString::to_string),
                "values": arg
                    .get_possible_values()
                    .iter()
                    .map(|v| v.get_name().to_string())
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    let subcommands: Vec<Value> = command
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set())
        .map(describe)
        .collect();
    json!({
        "name": command.get_name(),
        "about": command.get_about().map(ToString::to_string),
        "args": args,
        "subcommands": subcommands,
    })
}

fn list(command: &clap::Command, prefix: &str, out: &mut String) {
    for sub in command.get_subcommands().filter(|sub| !sub.is_hide_set()) {
        let name = format!("{prefix} {}", sub.get_name());
        let positionals: Vec<String> = sub
            .get_positionals()
            .map(|arg| format!("<{}>", arg.get_id().as_str()))
            .collect();
        out.push_str(&format!(
            "{name} {}  # {}\n",
            positionals.join(" "),
            sub.get_about().map(ToString::to_string).unwrap_or_default()
        ));
        list(sub, &name, out);
    }
}
