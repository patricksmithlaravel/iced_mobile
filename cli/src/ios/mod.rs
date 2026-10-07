//! What iOS device builds share (design §9.1-§9.3, §10.5, §11.1): `icm
//! run ios-device` (`platform/ios_device/`) and `icm release ios`
//! (`release/ios.rs`).
//!
//! | Module | What it owns |
//! |---|---|
//! | [`bundle`] | the device `.app`: actool for `iphoneos`, the Info.plist with `DT*` keys, PrivacyInfo, resources; the plist, icon and usage-description gates |
//! | [`dt`] | the `DT*` keys from the selected Xcode |
//! | [`identity`] | `security find-identity`: signing identities by reference |
//! | [`profile`] | provisioning profiles: decode, search, match (kind, team, app id, certificate, device, expiry) |
//! | [`entitlements`] | the minimal development and distribution sets and the subset check against a profile |
//! | [`codesign`] | sign last under the keychain watchdog, verify, read the signature and entitlements back |
//! | [`macho`] | `LC_UUID`, architectures and undefined symbols |
//! | [`privacy`] | the required-reason API scan (ITMS-91053) |
//! | [`dsym`] | dsymutil and the dSYM's UUID and line-table gates |
//! | [`ipa`] | `Payload/` and `zip -X`; the layout gate |
//! | [`plist_xml`], [`sha1`] | XML plists (profiles, signed entitlements) and certificate fingerprints, on any host |

pub mod bundle;
pub mod codesign;
pub mod dsym;
pub mod dt;
pub mod entitlements;
pub mod identity;
pub mod ipa;
pub mod macho;
pub mod plist_xml;
pub mod privacy;
pub mod profile;
pub mod sha1;

/// Seconds since the epoch "now": midnight of `ICM_TODAY` when icm's tests
/// set it (profile expiry is judged against the same day as the policy
/// table), else the clock.
pub fn now_unix() -> i64 {
    if std::env::var("ICM_TODAY").is_ok() {
        return crate::time::Day::today().0 * 86_400;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}
