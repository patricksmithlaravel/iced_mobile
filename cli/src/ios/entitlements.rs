//! Entitlements (design §9.3): the minimal set icm signs with, plus
//! `[ios.entitlements]`, checked against the profile's `Entitlements`
//! (wildcards resolved). The profile's entitlements are never copied
//! wholesale.

use super::profile::wildcard_matches;
use crate::config::IcmToml;
use crate::platform::ios_sim::plist::from_toml;
use serde_json::{Map, Value, json};

/// The entitlements of a development build (device runs).
pub fn development(config: &IcmToml, team: &str) -> Map<String, Value> {
    let mut map = base(config, team);
    let _ = map.insert("get-task-allow".into(), json!(true));
    overlay(config, map)
}

/// The entitlements of an App Store build.
pub fn distribution(config: &IcmToml, team: &str) -> Map<String, Value> {
    let mut map = base(config, team);
    let _ = map.insert("get-task-allow".into(), json!(false));
    let _ = map.insert("beta-reports-active".into(), json!(true));
    overlay(config, map)
}

fn base(config: &IcmToml, team: &str) -> Map<String, Value> {
    let mut map = Map::new();
    let _ = map.insert(
        "application-identifier".into(),
        json!(format!("{team}.{}", config.app.id)),
    );
    let _ = map.insert("com.apple.developer.team-identifier".into(), json!(team));
    map
}

fn overlay(config: &IcmToml, mut map: Map<String, Value>) -> Map<String, Value> {
    for (key, value) in &config.ios.entitlements {
        let _ = map.insert(key.clone(), from_toml(value));
    }
    map
}

/// Whether the profile allows one value of an entitlement.
fn allowed(signed: &Value, allowed_by: &Value) -> bool {
    match (signed, allowed_by) {
        // A capability that is off needs nothing from the profile.
        (Value::Bool(false), _) => true,
        (Value::Bool(true), Value::Bool(true)) => true,
        (Value::String(value), Value::String(pattern)) => wildcard_matches(pattern, value),
        (Value::String(value), Value::Array(patterns)) => patterns
            .iter()
            .filter_map(Value::as_str)
            .any(|pattern| wildcard_matches(pattern, value)),
        (Value::Array(values), _) => values.iter().all(|value| allowed(value, allowed_by)),
        (value, pattern) => value == pattern,
    }
}

/// The signed entitlements the profile does not allow, as `key` or
/// `key = value` descriptions.
pub fn not_in_profile(signed: &Map<String, Value>, profile: &Map<String, Value>) -> Vec<String> {
    let mut missing = Vec::new();
    for (key, value) in signed {
        match profile.get(key) {
            None if *value == Value::Bool(false) => {}
            None => missing.push(format!("{key} (the profile does not have it)")),
            Some(allowed_by) if !allowed(value, allowed_by) => {
                missing.push(format!("{key} = {value} (the profile allows {allowed_by})"));
            }
            Some(_) => {}
        }
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn config(extra: &str) -> IcmToml {
        crate::config::parse(
            Path::new("/p/icm.toml"),
            &format!(
                "schema = 1\n[app]\nname = \"Notes\"\nid = \"com.acme.notes\"\nbuild = 3\n{extra}"
            ),
        )
        .unwrap_or_else(|errors| panic!("{errors:?}"))
        .config
    }

    #[test]
    fn the_minimal_sets() {
        let config = config(
            "[ios.entitlements]\n\"keychain-access-groups\" = [\"ABCDE12345.com.acme.notes\"]\n",
        );
        let dist = distribution(&config, "ABCDE12345");
        assert_eq!(
            Value::Object(dist.clone()),
            json!({
                "application-identifier": "ABCDE12345.com.acme.notes",
                "com.apple.developer.team-identifier": "ABCDE12345",
                "get-task-allow": false,
                "beta-reports-active": true,
                "keychain-access-groups": ["ABCDE12345.com.acme.notes"],
            })
        );
        let dev = development(&config, "ABCDE12345");
        assert_eq!(dev["get-task-allow"], true);
        assert!(!dev.contains_key("beta-reports-active"));
    }

    #[test]
    fn the_profile_must_allow_each_one() {
        let profile: Map<String, Value> = serde_json::from_value(json!({
            "application-identifier": "ABCDE12345.*",
            "com.apple.developer.team-identifier": "ABCDE12345",
            "get-task-allow": false,
            "beta-reports-active": true,
            "keychain-access-groups": ["ABCDE12345.*", "com.apple.token"],
        }))
        .unwrap();
        let config = config(
            "[ios.entitlements]\n\"keychain-access-groups\" = [\"ABCDE12345.com.acme.notes\"]\n",
        );
        assert!(not_in_profile(&distribution(&config, "ABCDE12345"), &profile).is_empty());

        // Debugging is not allowed by a store profile; push is not in it.
        let mut dev = development(&config, "ABCDE12345");
        let _ = dev.insert("aps-environment".into(), json!("production"));
        let missing = not_in_profile(&dev, &profile);
        assert_eq!(missing.len(), 2, "{missing:?}");
        assert!(missing[0].starts_with("aps-environment"));
        assert!(missing[1].starts_with("get-task-allow = true"));

        // Another team's app id.
        let other = distribution(&config, "ZZZZZ99999");
        assert!(
            not_in_profile(&other, &profile)
                .iter()
                .any(|m| m.starts_with("application-identifier"))
        );
    }
}
