//! Provisioning profiles (design §10.5 step 3, §11.1 preconditions, §12.2
//! `ios.sign.profile_*`).
//!
//! A `.mobileprovision` is a CMS envelope around a plain XML plist, so icm
//! reads the plist straight out of the bytes ([`super::plist_xml::embedded`])
//! on any host: no `security cms -D`, which needs macOS and a keychain.
//!
//! Profiles are found by reference (`[ios.signing] <kind>.profile`):
//! `auto` searches `ICM_PROVISIONING_PROFILES` (a `:`-separated list of
//! directories, for CI and icm's tests) or else Xcode's two directories;
//! a UUID picks that profile from them; a path reads that file. Every
//! candidate is matched against what the build needs: the kind, the team,
//! the app id (exact or wildcard), the identity's certificate, the device
//! (development) and the expiry.

use super::plist_xml;
use super::sha1;
use crate::catalogue::CheckId;
use crate::error::{Evidence, IcmError};
use crate::tools::Env;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// A profile FAILs when it expires within this many days...
pub const EXPIRY_FAIL_DAYS: i64 = 7;
/// ...and WARNs within this many.
pub const EXPIRY_WARN_DAYS: i64 = 30;

/// What a profile is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Development: listed devices, `get-task-allow`.
    Development,
    /// Ad hoc distribution: listed devices, no `get-task-allow`.
    AdHoc,
    /// App Store (and TestFlight): no device list.
    AppStore,
    /// In-house: every device.
    Enterprise,
}

impl Kind {
    /// `development`, `ad-hoc`, `app-store`, `enterprise`.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Development => "development",
            Kind::AdHoc => "ad-hoc",
            Kind::AppStore => "app-store",
            Kind::Enterprise => "enterprise",
        }
    }
}

/// A decoded profile.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    /// Where it was read from.
    pub path: PathBuf,
    /// `Name`.
    pub name: String,
    /// `UUID`.
    pub uuid: String,
    /// `TeamIdentifier`.
    pub teams: Vec<String>,
    /// `ExpirationDate`, as written (`2027-09-29T12:00:00Z`).
    pub expires: Option<String>,
    /// `ExpirationDate` in seconds since the Unix epoch.
    pub expires_unix: Option<i64>,
    /// `ProvisionedDevices`.
    pub devices: Vec<String>,
    /// `ProvisionsAllDevices`.
    pub all_devices: bool,
    /// The SHA-1 of each certificate in `DeveloperCertificates`, upper case.
    pub certificates: Vec<String>,
    /// `Entitlements`.
    pub entitlements: Map<String, Value>,
    /// `Platform`.
    pub platforms: Vec<String>,
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Seconds since the epoch of `YYYY-MM-DDTHH:MM:SSZ`.
pub fn parse_date(text: &str) -> Option<i64> {
    let (date, time) = text.trim().trim_end_matches('Z').split_once('T')?;
    let day = crate::time::Day::parse(date)?;
    let mut parts = time.split(':').map(|p| p.parse::<i64>().ok());
    let (hour, minute, second) = (parts.next()??, parts.next()??, parts.next()??);
    Some(day.0 * 86_400 + hour * 3_600 + minute * 60 + second)
}

impl Profile {
    /// Decodes a profile's bytes.
    pub fn decode(path: &Path, bytes: &[u8]) -> Result<Profile, String> {
        let plist = plist_xml::embedded(bytes)?;
        let dict = plist
            .as_object()
            .ok_or("the profile's plist is not a dictionary")?;
        let text = |key: &str| dict.get(key).and_then(Value::as_str).map(str::to_string);
        let uuid = text("UUID").ok_or("the profile has no UUID")?;
        let expires = text("ExpirationDate");
        let certificates = strings(dict.get("DeveloperCertificates"))
            .iter()
            .filter_map(|b64| sha1::base64_decode(b64))
            .map(|der| sha1::hex_upper(&der))
            .collect();
        Ok(Profile {
            path: path.to_path_buf(),
            name: text("Name").unwrap_or_default(),
            uuid,
            teams: strings(dict.get("TeamIdentifier")),
            expires_unix: expires.as_deref().and_then(parse_date),
            expires,
            devices: strings(dict.get("ProvisionedDevices")),
            all_devices: dict
                .get("ProvisionsAllDevices")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            certificates,
            entitlements: dict
                .get("Entitlements")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
            platforms: strings(dict.get("Platform")),
        })
    }

    /// Reads and decodes a profile file.
    pub fn read(path: &Path) -> Result<Profile, String> {
        let bytes = std::fs::read(path).map_err(|error| format!("cannot read it: {error}"))?;
        Profile::decode(path, &bytes)
    }

    /// What the profile is for.
    pub fn kind(&self) -> Kind {
        if self.all_devices {
            Kind::Enterprise
        } else if self.devices.is_empty() {
            Kind::AppStore
        } else if self.get_task_allow() {
            Kind::Development
        } else {
            Kind::AdHoc
        }
    }

    /// The profile's `get-task-allow`.
    pub fn get_task_allow(&self) -> bool {
        self.entitlements
            .get("get-task-allow")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// The profile's `application-identifier` (`TEAM.com.acme.notes`, or a
    /// wildcard `TEAM.*`).
    pub fn application_identifier(&self) -> Option<&str> {
        self.entitlements
            .get("application-identifier")
            .and_then(Value::as_str)
    }

    /// How the profile's app id covers `TEAM.<bundle id>`.
    pub fn app_match(&self, team: &str, bundle_id: &str) -> AppMatch {
        let wanted = format!("{team}.{bundle_id}");
        match self.application_identifier() {
            Some(id) if id == wanted => AppMatch::Exact,
            Some(id) if wildcard_matches(id, &wanted) => AppMatch::Wildcard,
            _ => AppMatch::No,
        }
    }

    /// Days until the profile expires (negative once it has), at `now`.
    pub fn days_left(&self, now: i64) -> Option<i64> {
        self.expires_unix
            .map(|expires| (expires - now).div_euclid(86_400))
    }

    /// Whether it has expired at `now`.
    pub fn expired(&self, now: i64) -> bool {
        self.expires_unix.is_some_and(|expires| expires <= now)
    }

    /// `artifacts.json` `signing.profile`.
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "uuid": self.uuid,
            "type": self.kind().as_str(),
            "team": self.teams.first(),
            "app_id": self.application_identifier(),
            "expires": self.expires,
        })
    }

    /// `Name (UUID)`.
    pub fn label(&self) -> String {
        format!("\"{}\" ({})", self.name, self.uuid)
    }
}

/// How a profile's app id covers the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AppMatch {
    /// Not at all.
    No,
    /// Through a wildcard (`TEAM.*`, `TEAM.com.acme.*`).
    Wildcard,
    /// Exactly.
    Exact,
}

/// Whether an entitlement pattern (`TEAM.*`, `*`) matches a value.
pub fn wildcard_matches(pattern: &str, value: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => value.starts_with(prefix),
        None => pattern == value,
    }
}

/// The directories `auto` and UUID references search.
pub fn search_dirs(env: &Env) -> Vec<PathBuf> {
    if let Some(list) = env.var("ICM_PROVISIONING_PROFILES") {
        return std::env::split_paths(list).collect();
    }
    let Some(home) = env.home() else {
        return Vec::new();
    };
    vec![
        home.join("Library/Developer/Xcode/UserData/Provisioning Profiles"),
        home.join("Library/MobileDevice/Provisioning Profiles"),
    ]
}

/// Every readable profile in `dirs` (`.mobileprovision` files), and the
/// files that could not be read.
pub fn scan(dirs: &[PathBuf]) -> (Vec<Profile>, Vec<(PathBuf, String)>) {
    let mut profiles = Vec::new();
    let mut unreadable = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|ext| ext == "mobileprovision" || ext == "provisionprofile")
            })
            .collect();
        paths.sort();
        for path in paths {
            match Profile::read(&path) {
                Ok(profile) => {
                    if !profiles.iter().any(|p: &Profile| p.uuid == profile.uuid) {
                        profiles.push(profile);
                    }
                }
                Err(error) => unreadable.push((path, error)),
            }
        }
    }
    (profiles, unreadable)
}

/// What a build needs from its profile.
#[derive(Clone, Debug)]
pub struct Want<'a> {
    /// The kind (App Store for releases, development for device runs).
    pub kind: Kind,
    /// `[ios] team_id`.
    pub team: &'a str,
    /// `[app] id`.
    pub bundle_id: &'a str,
    /// The SHA-1s of the identities that may sign (empty: any).
    pub certificates: &'a [String],
    /// The device UDID (development).
    pub device: Option<&'a str>,
    /// Now, in seconds since the epoch.
    pub now: i64,
}

/// Why a profile does not fit, or `None` when it does.
pub fn mismatch(profile: &Profile, want: &Want<'_>) -> Option<(CheckId, String)> {
    let kind = profile.kind();
    if kind != want.kind {
        return Some((
            CheckId::IosSignProfileMismatch,
            format!(
                "{} is a {} profile; this build needs {}",
                profile.label(),
                kind.as_str(),
                match want.kind {
                    Kind::AppStore => "an App Store profile",
                    Kind::Development => "a development profile",
                    Kind::AdHoc => "an ad hoc profile",
                    Kind::Enterprise => "an in-house profile",
                }
            ),
        ));
    }
    if !profile.teams.iter().any(|team| team == want.team) {
        return Some((
            CheckId::IosSignProfileMismatch,
            format!(
                "{} belongs to team {}, not [ios] team_id {}",
                profile.label(),
                if profile.teams.is_empty() {
                    "(none)".to_string()
                } else {
                    profile.teams.join(", ")
                },
                want.team
            ),
        ));
    }
    if profile.app_match(want.team, want.bundle_id) == AppMatch::No {
        return Some((
            CheckId::IosSignProfileMismatch,
            format!(
                "{} is for the app id {}, not {}.{}",
                profile.label(),
                profile.application_identifier().unwrap_or("(none)"),
                want.team,
                want.bundle_id
            ),
        ));
    }
    if !want.certificates.is_empty()
        && !profile.certificates.iter().any(|cert| {
            want.certificates
                .iter()
                .any(|c| c.eq_ignore_ascii_case(cert))
        })
    {
        return Some((
            CheckId::IosSignProfileMismatch,
            format!(
                "{} does not include the signing identity's certificate ({})",
                profile.label(),
                want.certificates.join(", ")
            ),
        ));
    }
    if let Some(device) = want.device
        && kind == Kind::Development
        && !profile
            .devices
            .iter()
            .any(|d| d.eq_ignore_ascii_case(device))
    {
        return Some((
            CheckId::IosSignProfileMismatch,
            format!("{} does not list the device {device}", profile.label()),
        ));
    }
    if profile.expired(want.now) {
        return Some((
            CheckId::IosSignProfileExpired,
            format!(
                "{} expired on {}",
                profile.label(),
                profile.expires.as_deref().unwrap_or("?")
            ),
        ));
    }
    None
}

/// The chosen profile.
#[derive(Clone, Debug)]
pub struct Chosen {
    /// The profile.
    pub profile: Profile,
    /// How it was found (`auto`, `UUID`, `path`).
    pub how: &'static str,
}

/// A reference's error: the id, the detail, the owner's fix.
fn owner_error(id: CheckId, detail: String, fix: &str, evidence: Option<&Path>) -> IcmError {
    let mut error = IcmError::new(id, detail).fix(fix, &[]);
    if let Some(path) = evidence {
        error = error.evidence(Evidence::file(path));
    }
    error
}

fn is_uuid(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&parts)
            .all(|(len, part)| part.len() == *len && part.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Finds the profile a reference names (`auto`, a UUID or a path, the
/// latter resolved by the caller) and checks it fits. The error is an
/// owner item: `ios.sign.no_profile`, `.profile_mismatch` or
/// `.profile_expired`.
pub fn choose(reference: &str, dirs: &[PathBuf], want: &Want<'_>) -> Result<Chosen, IcmError> {
    let what = match want.kind {
        Kind::AppStore => "App Store",
        Kind::Development => "development",
        Kind::AdHoc => "ad hoc",
        Kind::Enterprise => "in-house",
    };
    let create = match want.kind {
        Kind::Development => format!(
            "The owner creates (or downloads in Xcode > Settings > Accounts) a development profile for {}.{} with their Apple Development certificate and this device, or sets [ios.signing] development.profile to its path or UUID.",
            want.team, want.bundle_id
        ),
        _ => format!(
            "The owner creates an App Store profile for {}.{} with their Apple Distribution certificate in the Apple Developer portal, downloads it, and sets [ios.signing] distribution.profile to its path or UUID (or leaves \"auto\").",
            want.team, want.bundle_id
        ),
    };

    let reference = reference.trim();
    if reference != "auto" && !is_uuid(reference) {
        let path = Path::new(reference);
        let profile = Profile::read(path).map_err(|error| {
            owner_error(
                CheckId::IosSignNoProfile,
                format!("the profile {reference} cannot be used: {error}"),
                &create,
                None,
            )
        })?;
        return match mismatch(&profile, want) {
            None => Ok(Chosen {
                profile,
                how: "path",
            }),
            Some((id, detail)) => Err(owner_error(id, detail, &create, Some(path))),
        };
    }

    let (profiles, _) = scan(dirs);
    let searched = if dirs.is_empty() {
        "no profile directory".to_string()
    } else {
        dirs.iter()
            .map(|d| crate::paths::display(d))
            .collect::<Vec<_>>()
            .join(" and ")
    };

    if is_uuid(reference) {
        let Some(profile) = profiles
            .iter()
            .find(|p| p.uuid.eq_ignore_ascii_case(reference))
        else {
            return Err(owner_error(
                CheckId::IosSignNoProfile,
                format!("no profile with the UUID {reference} is in {searched}"),
                &create,
                None,
            ));
        };
        return match mismatch(profile, want) {
            None => Ok(Chosen {
                profile: profile.clone(),
                how: "UUID",
            }),
            Some((id, detail)) => Err(owner_error(id, detail, &create, Some(&profile.path))),
        };
    }

    // auto: the fitting profiles, an exact app id first, then the latest
    // expiry.
    let mut fitting: Vec<&Profile> = profiles
        .iter()
        .filter(|p| mismatch(p, want).is_none())
        .collect();
    fitting.sort_by_key(|p| {
        (
            std::cmp::Reverse(p.app_match(want.team, want.bundle_id)),
            std::cmp::Reverse(p.expires_unix.unwrap_or(0)),
        )
    });
    if let Some(best) = fitting.first() {
        return Ok(Chosen {
            profile: (*best).clone(),
            how: "auto",
        });
    }

    // Nothing fits: say why, from the profiles for this app.
    let for_app: Vec<&Profile> = profiles
        .iter()
        .filter(|p| p.app_match(want.team, want.bundle_id) != AppMatch::No)
        .collect();
    let near: Vec<(CheckId, String)> = for_app.iter().filter_map(|p| mismatch(p, want)).collect();
    if near.is_empty() {
        return Err(owner_error(
            CheckId::IosSignNoProfile,
            format!(
                "no {what} profile for {}.{} is in {searched} ({} profile(s) there, none for this app)",
                want.team,
                want.bundle_id,
                profiles.len()
            ),
            &create,
            None,
        ));
    }
    let id = if near
        .iter()
        .all(|(id, _)| *id == CheckId::IosSignProfileExpired)
    {
        CheckId::IosSignProfileExpired
    } else {
        CheckId::IosSignProfileMismatch
    };
    let details: Vec<String> = near.into_iter().map(|(_, detail)| detail).collect();
    Err(owner_error(
        id,
        format!(
            "no {what} profile in {searched} fits {}.{}: {}",
            want.team,
            want.bundle_id,
            details.join("; ")
        ),
        &create,
        None,
    ))
}

/// A test profile (icm's tests write `.mobileprovision` fixtures from it;
/// a real one is the same plist inside a CMS envelope).
#[derive(Clone, Debug)]
pub struct Fixture {
    /// `Name` and `AppIDName`.
    pub name: String,
    /// `UUID`.
    pub uuid: String,
    /// `TeamIdentifier`.
    pub team: String,
    /// The bundle id part of `application-identifier` (`*` for a wildcard).
    pub app_id: String,
    /// DER certificates.
    pub certificates: Vec<Vec<u8>>,
    /// `ProvisionedDevices` (empty: an App Store profile).
    pub devices: Vec<String>,
    /// `get-task-allow`.
    pub get_task_allow: bool,
    /// `ExpirationDate`.
    pub expires: String,
}

impl Fixture {
    /// The plist.
    pub fn plist(&self) -> Value {
        let team = &self.team;
        let mut entitlements = json!({
            "application-identifier": format!("{team}.{}", self.app_id),
            "com.apple.developer.team-identifier": team,
            "get-task-allow": self.get_task_allow,
            "keychain-access-groups": [format!("{team}.*"), "com.apple.token"],
        });
        if !self.get_task_allow && self.devices.is_empty() {
            entitlements["beta-reports-active"] = json!(true);
        }
        let mut plist = json!({
            "AppIDName": self.name,
            "ApplicationIdentifierPrefix": [team],
            "CreationDate": "2026-01-01T00:00:00Z",
            "Platform": ["iOS"],
            "DeveloperCertificates": self.certificates.iter().map(|c| sha1::base64_encode(c)).collect::<Vec<_>>(),
            "Entitlements": entitlements,
            "ExpirationDate": self.expires,
            "Name": self.name,
            "TeamIdentifier": [team],
            "TeamName": "icm test",
            "TimeToLive": 365,
            "UUID": self.uuid,
            "Version": 1,
        });
        if !self.devices.is_empty() {
            plist["ProvisionedDevices"] = json!(self.devices);
        }
        plist
    }

    /// The `.mobileprovision` bytes: the plist between bytes that stand in
    /// for the CMS envelope.
    pub fn bytes(&self) -> Vec<u8> {
        let mut bytes = vec![0x30, 0x80, 0x06, 0x09, 0x2a, 0x86, 0x48];
        bytes.extend_from_slice(crate::platform::ios_sim::plist::to_xml(&self.plist()).as_bytes());
        bytes.extend_from_slice(&[0x00, 0x00, 0xa0, 0x80]);
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEAM: &str = "ABCDE12345";
    const NOW: i64 = 1_791_331_200; // 2026-10-07

    fn write(dir: &Path, file: &str, fixture: &Fixture) -> PathBuf {
        let path = dir.join(file);
        std::fs::write(&path, fixture.bytes()).unwrap();
        path
    }

    fn want<'a>(kind: Kind, certs: &'a [String], device: Option<&'a str>) -> Want<'a> {
        Want {
            kind,
            team: TEAM,
            bundle_id: "com.acme.notes",
            certificates: certs,
            device,
            now: NOW,
        }
    }

    #[test]
    fn dates_parse() {
        assert_eq!(parse_date("2026-10-07T00:00:00Z"), Some(NOW));
        assert_eq!(parse_date("2026-10-07T01:02:03Z"), Some(NOW + 3723));
        assert_eq!(parse_date("yesterday"), None);
    }

    #[test]
    fn kinds_and_app_ids_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let store = write(
            dir.path(),
            "store.mobileprovision",
            &Fixture {
                name: "Notes Store".into(),
                uuid: "11111111-2222-3333-4444-555555555555".into(),
                team: TEAM.into(),
                app_id: "com.acme.notes".into(),
                certificates: vec![b"cert-a".to_vec()],
                devices: vec![],
                get_task_allow: false,
                expires: "2027-09-29T12:00:00Z".into(),
            },
        );
        let profile = Profile::read(&store).unwrap();
        assert_eq!(profile.kind(), Kind::AppStore);
        assert_eq!(profile.name, "Notes Store");
        assert_eq!(profile.teams, [TEAM]);
        assert_eq!(profile.certificates, [sha1::hex_upper(b"cert-a")]);
        assert_eq!(profile.app_match(TEAM, "com.acme.notes"), AppMatch::Exact);
        assert_eq!(profile.app_match(TEAM, "com.acme.other"), AppMatch::No);
        assert_eq!(profile.days_left(NOW), Some(357));
        assert_eq!(profile.to_json()["type"], "app-store");

        let dev = Profile::decode(
            Path::new("dev"),
            &Fixture {
                name: "Wildcard Dev".into(),
                uuid: "66666666-2222-3333-4444-555555555555".into(),
                team: TEAM.into(),
                app_id: "*".into(),
                certificates: vec![b"cert-d".to_vec()],
                devices: vec!["00008110-000A1B2C3D4E5F60".to_string()],
                get_task_allow: true,
                expires: "2027-01-01T00:00:00Z".into(),
            }
            .bytes(),
        )
        .unwrap();
        assert_eq!(dev.kind(), Kind::Development);
        assert_eq!(dev.app_match(TEAM, "com.acme.notes"), AppMatch::Wildcard);
        assert!(wildcard_matches(
            "ABCDE12345.com.acme.*",
            "ABCDE12345.com.acme.notes"
        ));
        assert!(!wildcard_matches(
            "ABCDE12345.com.acme.*",
            "ABCDE12345.com.other"
        ));
    }

    #[test]
    fn auto_picks_the_fitting_profile_and_explains_misses() {
        let dir = tempfile::tempdir().unwrap();
        let cert = sha1::hex_upper(b"cert-a");
        let certs = [cert.clone()];
        // A development profile, an expired store profile, a wildcard store
        // profile, an exact store profile for another certificate.
        let _ = write(
            dir.path(),
            "dev.mobileprovision",
            &Fixture {
                name: "Dev".into(),
                uuid: "00000000-0000-0000-0000-000000000001".into(),
                team: TEAM.into(),
                app_id: "com.acme.notes".into(),
                certificates: vec![b"cert-a".to_vec()],
                devices: vec!["UDID-1".to_string()],
                get_task_allow: true,
                expires: "2027-01-01T00:00:00Z".into(),
            },
        );
        let _ = write(
            dir.path(),
            "old.mobileprovision",
            &Fixture {
                name: "Old".into(),
                uuid: "00000000-0000-0000-0000-000000000002".into(),
                team: TEAM.into(),
                app_id: "com.acme.notes".into(),
                certificates: vec![b"cert-a".to_vec()],
                devices: vec![],
                get_task_allow: false,
                expires: "2026-01-01T00:00:00Z".into(),
            },
        );
        let _ = write(
            dir.path(),
            "wild.mobileprovision",
            &Fixture {
                name: "Wild".into(),
                uuid: "00000000-0000-0000-0000-000000000003".into(),
                team: TEAM.into(),
                app_id: "*".into(),
                certificates: vec![b"cert-a".to_vec()],
                devices: vec![],
                get_task_allow: false,
                expires: "2027-03-01T00:00:00Z".into(),
            },
        );
        let _ = write(
            dir.path(),
            "other-cert.mobileprovision",
            &Fixture {
                name: "Other cert".into(),
                uuid: "00000000-0000-0000-0000-000000000004".into(),
                team: TEAM.into(),
                app_id: "com.acme.notes".into(),
                certificates: vec![b"cert-b".to_vec()],
                devices: vec![],
                get_task_allow: false,
                expires: "2027-06-01T00:00:00Z".into(),
            },
        );
        std::fs::write(dir.path().join("broken.mobileprovision"), b"junk").unwrap();
        let dirs = [dir.path().to_path_buf()];

        let chosen = choose("auto", &dirs, &want(Kind::AppStore, &certs, None)).unwrap();
        assert_eq!(chosen.profile.name, "Wild");
        assert_eq!(chosen.how, "auto");

        // Any certificate: the exact app id wins over the wildcard.
        let chosen = choose("auto", &dirs, &want(Kind::AppStore, &[], None)).unwrap();
        assert_eq!(chosen.profile.name, "Other cert");

        let dev = choose(
            "auto",
            &dirs,
            &want(Kind::Development, &certs, Some("UDID-1")),
        )
        .unwrap();
        assert_eq!(dev.profile.name, "Dev");
        let error = choose(
            "auto",
            &dirs,
            &want(Kind::Development, &certs, Some("UDID-9")),
        )
        .unwrap_err();
        assert_eq!(error.id, "ios.sign.profile_mismatch");
        assert!(
            error.detail.contains("does not list the device UDID-9"),
            "{}",
            error.detail
        );

        // By UUID and by path, with the reason when it does not fit.
        let by_uuid = choose(
            "00000000-0000-0000-0000-000000000002",
            &dirs,
            &want(Kind::AppStore, &certs, None),
        )
        .unwrap_err();
        assert_eq!(by_uuid.id, "ios.sign.profile_expired");
        let path = dir.path().join("dev.mobileprovision");
        let by_path = choose(
            &path.display().to_string(),
            &dirs,
            &want(Kind::AppStore, &certs, None),
        )
        .unwrap_err();
        assert_eq!(by_path.id, "ios.sign.profile_mismatch");
        assert!(
            by_path.detail.contains("a development profile"),
            "{}",
            by_path.detail
        );
        let missing = choose(
            "/nowhere.mobileprovision",
            &dirs,
            &want(Kind::AppStore, &certs, None),
        )
        .unwrap_err();
        assert_eq!(missing.id, "ios.sign.no_profile");

        // Another app: none.
        let other = Want {
            bundle_id: "org.other.app",
            team: "ZZZZZ99999",
            ..want(Kind::AppStore, &certs, None)
        };
        let none = choose("auto", &dirs, &other).unwrap_err();
        assert_eq!(none.id, "ios.sign.no_profile");
        assert_eq!(none.fix.by.as_str(), "owner");
    }

    #[test]
    fn the_search_dirs_follow_the_env() {
        let env = Env::from_pairs(
            &[("ICM_PROVISIONING_PROFILES", "/a:/b")],
            Some(Path::new("/h")),
        );
        assert_eq!(
            search_dirs(&env),
            [PathBuf::from("/a"), PathBuf::from("/b")]
        );
        let env = Env::from_pairs(&[], Some(Path::new("/h")));
        assert_eq!(
            search_dirs(&env)[0],
            PathBuf::from("/h/Library/Developer/Xcode/UserData/Provisioning Profiles")
        );
    }
}
