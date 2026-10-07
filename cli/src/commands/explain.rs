//! `icm explain <id> | exit-codes | --list`.

use crate::catalogue::{self, CheckId};
use crate::cli::ExplainArgs;
use crate::context::Ctx;
use crate::error::{IcmError, Result};
use crate::exit::Exit;
use serde_json::{Value, json};

/// Runs `icm explain`.
pub fn run(ctx: &mut Ctx, args: &ExplainArgs) -> Result<()> {
    if args.list {
        let entries: Vec<Value> = CheckId::ALL
            .iter()
            .map(|check| {
                let mut entry = json!(check.entry());
                entry["hand_written"] = json!(catalogue::hand_written(check.id()).is_some());
                entry
            })
            .collect();

        let width = CheckId::ALL.iter().map(|c| c.id().len()).max().unwrap_or(0);
        let mut text = String::new();
        for check in CheckId::ALL {
            let entry = check.entry();
            text.push_str(&format!(
                "{:width$}  {:4}  exit {:>3}  {:10}  {}\n",
                entry.id,
                entry.level.name(),
                entry.exit.code(),
                entry.by.as_str(),
                entry.title,
            ));
        }

        ctx.rep.set("catalogue", Value::Array(entries));
        ctx.rep.summary(format!("{} ids", CheckId::ALL.len()));
        ctx.rep.content(text);
        return Ok(());
    }

    let Some(id) = args.id.as_deref() else {
        return Err(IcmError::new(
            CheckId::UsageBadArgs,
            "give an id (e.g. `icm explain config.invalid`), `exit-codes`, or --list",
        )
        .fix(
            "Pass an id from `icm explain --list`.",
            &["icm explain --list"],
        ));
    };

    if id == "exit-codes" {
        let doc = catalogue::exit_codes_doc();
        let codes: Vec<Value> = Exit::ALL
            .iter()
            .map(|exit| {
                json!({
                    "code": exit.code(),
                    "name": exit.name(),
                    "meaning": exit.meaning(),
                    "next": exit.next(),
                })
            })
            .collect();
        ctx.rep.set("exit_codes", Value::Array(codes));
        ctx.rep.set("doc", json!(doc));
        ctx.rep.summary("exit codes");
        ctx.rep.content(doc);
        return Ok(());
    }

    match catalogue::explain(id) {
        Some(doc) => {
            if let Some(check) = CheckId::from_id(id) {
                ctx.rep.set("entry", json!(check.entry()));
            }
            ctx.rep.set("id", json!(id));
            ctx.rep.set("doc", json!(doc));
            ctx.rep.summary(format!("explained {id}"));
            ctx.rep.content(doc);
            Ok(())
        }
        None => {
            let mut detail = format!("`{id}` is not a check or error id");
            let mut commands = vec!["icm explain --list".to_string()];
            if let Some(suggestion) = catalogue::suggest(id) {
                detail.push_str(&format!("; did you mean `{suggestion}`?"));
                commands.insert(0, format!("icm explain {suggestion}"));
            }
            Err(IcmError::new(CheckId::UsageBadArgs, detail)
                .fix("Use an id from `icm explain --list`.", &[])
                .fix_commands(commands))
        }
    }
}
