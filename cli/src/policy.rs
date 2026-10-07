//! The dated store policy table (design §12.0): `cli/policy/stores.toml`,
//! embedded in icm.
//!
//! Each rule has values with the date they take effect; the value in force
//! on a day is the last one whose date has come. Gates read their floors
//! here instead of hard-coding them ([`Policy::int`], [`Policy::text`]).
//! `icm doctor` and `icm release` report [`Policy::checks`]: WARN
//! `env.policy_stale` once the table's review date is more than
//! `stale_after_days` old, and INFO `store.policy_upcoming` for each floor
//! that takes effect within `upcoming_days`.

use crate::catalogue::CheckId;
use crate::error::Check;
use crate::time::Day;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::OnceLock;

/// The embedded table.
pub const SOURCE: &str = include_str!("../policy/stores.toml");

/// The table's schema version this icm reads.
const SCHEMA: u32 = 1;

/// A rule's value from a date on.
#[derive(Clone, Debug, PartialEq)]
pub struct Dated {
    /// The value: a number, a version string or a flag.
    pub value: toml::Value,
    /// When it takes effect; `None`: always.
    pub effective: Option<Day>,
    /// A later deadline the store grants on request.
    pub extension: Option<Day>,
}

/// One policy rule.
#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    /// The rule id, e.g. `play.target_sdk`.
    pub id: String,
    /// What the rule requires.
    pub title: String,
    /// The store or platform (`app-store`, `google-play`, `android`).
    pub store: String,
    /// The catalogue ids that enforce it.
    pub gates: Vec<String>,
    /// Where the rule comes from.
    pub source: String,
    /// A remark.
    pub note: Option<String>,
    /// Its values, in date order.
    pub values: Vec<Dated>,
}

impl Rule {
    /// The value in force on `day`.
    pub fn on(&self, day: Day) -> Option<&Dated> {
        self.values
            .iter()
            .rev()
            .find(|value| value.effective.is_none_or(|effective| effective <= day))
    }
}

/// The whole table.
#[derive(Clone, Debug, PartialEq)]
pub struct Policy {
    /// When the table was last reviewed against the stores' announcements.
    pub reviewed: Day,
    /// After how many days the table counts as stale.
    pub stale_after_days: i64,
    /// How far ahead `upcoming` looks.
    pub upcoming_days: i64,
    /// The rules.
    pub rules: Vec<Rule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    schema: u32,
    reviewed: String,
    stale_after_days: i64,
    upcoming_days: i64,
    #[serde(default, rename = "rule")]
    rules: Vec<RawRule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    id: String,
    title: String,
    store: String,
    #[serde(default)]
    gates: Vec<String>,
    source: String,
    #[serde(default)]
    note: Option<String>,
    values: Vec<RawValue>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawValue {
    value: toml::Value,
    #[serde(default)]
    effective: Option<String>,
    #[serde(default)]
    extension: Option<String>,
}

/// Parses a policy table.
pub fn parse(text: &str) -> Result<Policy, String> {
    let file: File = toml::from_str(text).map_err(|error| error.to_string())?;
    if file.schema != SCHEMA {
        return Err(format!("schema {} (this icm reads {SCHEMA})", file.schema));
    }
    let day = |text: &str, what: &str| {
        Day::parse(text).ok_or_else(|| format!("{what}: `{text}` is not a YYYY-MM-DD date"))
    };
    let reviewed = day(&file.reviewed, "reviewed")?;

    let mut rules = Vec::new();
    for raw in file.rules {
        if raw.values.is_empty() {
            return Err(format!("rule {} has no values", raw.id));
        }
        let mut values = Vec::new();
        for value in raw.values {
            let what = format!("rule {}", raw.id);
            values.push(Dated {
                value: value.value,
                effective: value
                    .effective
                    .as_deref()
                    .map(|text| day(text, &what))
                    .transpose()?,
                extension: value
                    .extension
                    .as_deref()
                    .map(|text| day(text, &what))
                    .transpose()?,
            });
        }
        if values.windows(2).any(|pair| {
            pair[0]
                .effective
                .zip(pair[1].effective)
                .is_none_or(|(a, b)| a >= b)
        }) {
            return Err(format!(
                "rule {}: dated values must be in increasing date order",
                raw.id
            ));
        }
        if rules.iter().any(|rule: &Rule| rule.id == raw.id) {
            return Err(format!("rule {} appears twice", raw.id));
        }
        rules.push(Rule {
            id: raw.id,
            title: raw.title,
            store: raw.store,
            gates: raw.gates,
            source: raw.source,
            note: raw.note,
            values,
        });
    }

    Ok(Policy {
        reviewed,
        stale_after_days: file.stale_after_days,
        upcoming_days: file.upcoming_days,
        rules,
    })
}

/// The embedded table (parsed once; a unit test keeps it valid).
pub fn get() -> &'static Policy {
    static POLICY: OnceLock<Policy> = OnceLock::new();
    POLICY.get_or_init(|| parse(SOURCE).expect("cli/policy/stores.toml is valid"))
}

impl Policy {
    /// A rule by id.
    pub fn rule(&self, id: &str) -> Option<&Rule> {
        self.rules.iter().find(|rule| rule.id == id)
    }

    /// The value of a rule in force today.
    pub fn value(&self, id: &str) -> Option<&toml::Value> {
        self.rule(id)?.on(Day::today()).map(|dated| &dated.value)
    }

    /// An integer rule's value in force today.
    pub fn int(&self, id: &str) -> Option<i64> {
        self.value(id)?.as_integer()
    }

    /// A string rule's value in force today.
    pub fn text(&self, id: &str) -> Option<&str> {
        self.value(id)?.as_str()
    }

    /// How many days ago the table was reviewed.
    pub fn age_days(&self, today: Day) -> i64 {
        today.0 - self.reviewed.0
    }

    /// Whether the table is older than `stale_after_days`.
    pub fn stale(&self, today: Day) -> bool {
        self.age_days(today) > self.stale_after_days
    }

    /// The values that take effect after `today` and within
    /// `upcoming_days`.
    pub fn upcoming(&self, today: Day) -> Vec<(&Rule, &Dated)> {
        let until = today.plus(self.upcoming_days);
        self.rules
            .iter()
            .flat_map(|rule| rule.values.iter().map(move |value| (rule, value)))
            .filter(|(_, value)| {
                value
                    .effective
                    .is_some_and(|effective| effective > today && effective <= until)
            })
            .collect()
    }

    /// What `doctor` and `release` report: PASS or WARN `env.policy_stale`,
    /// and INFO `store.policy_upcoming` for each floor coming soon.
    pub fn checks(&self, today: Day) -> Vec<Check> {
        let age = self.age_days(today);
        let mut checks = vec![if self.stale(today) {
            Check::warn(
                CheckId::EnvPolicyStale,
                format!(
                    "icm's store policy table was reviewed on {} ({age} days ago, more than {}); the stores' floors may have moved since",
                    self.reviewed, self.stale_after_days
                ),
            )
            .fix(
                "Install a newer icm, whose table is current; `icm print policy` shows this one.",
                &[&crate::version::install_command(None)],
            )
        } else {
            Check::pass(
                CheckId::EnvPolicyStale,
                format!(
                    "store policy table reviewed on {} ({age} day(s) ago)",
                    self.reviewed
                ),
            )
        }];
        for (rule, value) in self.upcoming(today) {
            let effective = value.effective.expect("upcoming values are dated");
            checks.push(Check::info(
                CheckId::StorePolicyUpcoming,
                format!(
                    "{}: {} becomes {} on {effective} (in {} days){}",
                    rule.id,
                    rule.title,
                    show(&value.value),
                    effective.0 - today.0,
                    value
                        .extension
                        .map(|extension| format!("; extension on request to {extension}"))
                        .unwrap_or_default()
                ),
            ));
        }
        checks
    }

    /// The table as JSON for `icm print policy`, with each rule's value in
    /// force on `today`.
    pub fn to_json(&self, today: Day) -> Value {
        json!({
            "reviewed": self.reviewed.to_string(),
            "age_days": self.age_days(today),
            "stale_after_days": self.stale_after_days,
            "stale": self.stale(today),
            "upcoming_days": self.upcoming_days,
            "rules": self.rules.iter().map(|rule| json!({
                "id": rule.id,
                "title": rule.title,
                "store": rule.store,
                "gates": rule.gates,
                "source": rule.source,
                "note": rule.note,
                "in_force": rule.on(today).map(|dated| to_json(&dated.value)),
                "values": rule.values.iter().map(|dated| json!({
                    "value": to_json(&dated.value),
                    "effective": dated.effective.map(|day| day.to_string()),
                    "extension": dated.extension.map(|day| day.to_string()),
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }

    /// The table as text for `icm print policy`.
    pub fn to_text(&self, today: Day) -> String {
        let mut text = format!(
            "store policy table reviewed {} ({} days ago; stale after {})\n",
            self.reviewed,
            self.age_days(today),
            self.stale_after_days
        );
        for rule in &self.rules {
            let in_force = rule
                .on(today)
                .map(|dated| show(&dated.value))
                .unwrap_or_else(|| "(not yet)".to_string());
            text.push_str(&format!("{:28} {in_force:8} {}\n", rule.id, rule.title));
            for dated in &rule.values {
                if let Some(effective) = dated.effective {
                    text.push_str(&format!(
                        "{:28}   {} from {effective}{}\n",
                        "",
                        show(&dated.value),
                        dated
                            .extension
                            .map(|day| format!(" (extension to {day})"))
                            .unwrap_or_default()
                    ));
                }
            }
        }
        text
    }
}

/// A value as written in a sentence.
fn show(value: &toml::Value) -> String {
    match to_json(value) {
        Value::String(text) => text,
        other => other.to_string(),
    }
}

fn to_json(value: &toml::Value) -> Value {
    match value {
        toml::Value::String(text) => json!(text),
        toml::Value::Integer(number) => json!(number),
        toml::Value::Float(number) => json!(number),
        toml::Value::Boolean(flag) => json!(flag),
        other => json!(format!("{other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Status;

    fn day(text: &str) -> Day {
        Day::parse(text).unwrap()
    }

    #[test]
    fn the_embedded_table_parses_and_names_real_gates() {
        let policy = parse(SOURCE).unwrap();
        assert_eq!(policy.reviewed.to_string(), "2026-10-06");
        for rule in &policy.rules {
            assert!(!rule.gates.is_empty(), "{} names no gate", rule.id);
            for gate in &rule.gates {
                assert!(
                    CheckId::from_id(gate).is_some(),
                    "{}: `{gate}` is not a catalogue id",
                    rule.id
                );
            }
        }
        for id in [
            "app_store.min_sdk",
            "app_store.min_deployment",
            "play.target_sdk",
            "play.page_size_16k",
        ] {
            assert!(policy.rule(id).is_some(), "{id} is missing");
        }
    }

    #[test]
    fn values_follow_their_dates() {
        let policy = get();
        let target = policy.rule("play.target_sdk").unwrap();
        let on = |date: &str| target.on(day(date)).map(|d| d.value.as_integer().unwrap());
        assert_eq!(on("2025-01-01"), None);
        assert_eq!(on("2025-08-31"), Some(35));
        assert_eq!(on("2026-08-30"), Some(35));
        assert_eq!(on("2026-08-31"), Some(36));
        // Undated values are always in force.
        let back = policy.rule("android.back_optout").unwrap();
        assert_eq!(
            back.on(day("2000-01-01")).unwrap().value.as_integer(),
            Some(36)
        );
        assert_eq!(
            policy
                .rule("app_store.min_deployment")
                .unwrap()
                .on(day("2026-10-06"))
                .unwrap()
                .value
                .as_str(),
            Some("13.0")
        );
    }

    #[test]
    fn staleness_and_upcoming_floors_are_reported() {
        let policy = get();
        let fresh = policy.checks(day("2026-10-07"));
        assert_eq!(fresh[0].status, Status::Pass);
        assert_eq!(fresh[0].id(), "env.policy_stale");

        let stale = policy.checks(day("2027-01-05"));
        assert_eq!(stale[0].status, Status::Warn);
        assert!(
            stale[0].error.detail.contains("91 days ago"),
            "{}",
            stale[0].error.detail
        );

        // 57 days before Play's targetSdk 36 floor, 66 before the iOS 13 one.
        let before = policy.checks(day("2026-07-05"));
        let upcoming: Vec<&Check> = before
            .iter()
            .filter(|check| check.id() == "store.policy_upcoming")
            .collect();
        assert_eq!(upcoming.len(), 1, "{before:?}");
        assert_eq!(upcoming[0].status, Status::Info);
        assert!(
            upcoming[0]
                .error
                .detail
                .contains("becomes 36 on 2026-08-31 (in 57 days)"),
            "{}",
            upcoming[0].error.detail
        );
    }

    #[test]
    fn bad_tables_are_refused() {
        let base =
            "schema = 1\nreviewed = \"2026-10-06\"\nstale_after_days = 90\nupcoming_days = 60\n";
        assert!(parse(base).is_ok());
        assert!(parse(&base.replace("schema = 1", "schema = 2")).is_err());
        assert!(parse(&base.replace("2026-10-06", "soon")).is_err());
        let unordered = format!(
            "{base}[[rule]]\nid = \"x\"\ntitle = \"t\"\nstore = \"s\"\nsource = \"s\"\nvalues = [{{ value = 2, effective = \"2026-02-01\" }}, {{ value = 1, effective = \"2026-01-01\" }}]\n"
        );
        assert!(
            parse(&unordered)
                .unwrap_err()
                .contains("increasing date order")
        );
        assert!(parse(&format!("{base}colour = 1\n")).is_err());
    }

    #[test]
    fn the_table_prints() {
        let policy = get();
        let text = policy.to_text(day("2026-10-07"));
        assert!(text.contains("play.target_sdk"), "{text}");
        assert!(
            text.contains("36 from 2026-08-31 (extension to 2026-11-01)"),
            "{text}"
        );
        let value = policy.to_json(day("2026-10-07"));
        assert_eq!(value["stale"], false);
        let rules = value["rules"].as_array().unwrap();
        let play = rules
            .iter()
            .find(|rule| rule["id"] == "play.target_sdk")
            .unwrap();
        assert_eq!(play["in_force"], 36);
    }
}
