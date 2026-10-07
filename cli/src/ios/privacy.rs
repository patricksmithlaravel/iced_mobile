//! The required-reason API scan (design §11.1 step 4, §12.2
//! `ios.privacy.reasons`, ITMS-91053): which of Apple's required-reason
//! categories an executable uses, from its undefined symbols and the
//! Objective-C class and selector names in its bytes (objc2 looks classes
//! up by name, so they are strings, not symbols).
//!
//! Every category found needs a reason in `[ios.privacy] api_reasons`;
//! the gate's failure prints the exact line to add.

use std::collections::BTreeMap;

/// One required-reason category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Category {
    /// The short name `api_reasons` uses (`FileTimestamp`).
    pub name: &'static str,
    /// Undefined symbols that mean the category is used.
    pub symbols: &'static [&'static str],
    /// Names in the binary's bytes that mean it is used.
    pub strings: &'static [&'static str],
    /// The reason most apps declare, offered in the fix (the owner of the
    /// code confirms it against Apple's list).
    pub common_reason: &'static str,
}

/// Apple's categories (Privacy manifest files, "Describing use of
/// required reason API").
pub const CATEGORIES: &[Category] = &[
    Category {
        name: "FileTimestamp",
        symbols: &[
            "_stat",
            "_fstat",
            "_fstatat",
            "_lstat",
            "_getattrlist",
            "_fgetattrlist",
            "_getattrlistat",
            "_getattrlistbulk",
            "_NSFileCreationDate",
            "_NSFileModificationDate",
            "_NSURLContentModificationDateKey",
            "_NSURLCreationDateKey",
        ],
        strings: &["fileModificationDate", "contentModificationDateKey"],
        common_reason: "C617.1",
    },
    Category {
        name: "SystemBootTime",
        symbols: &["_mach_absolute_time"],
        strings: &["systemUptime"],
        common_reason: "35F9.1",
    },
    Category {
        name: "DiskSpace",
        symbols: &[
            "_statfs",
            "_statvfs",
            "_fstatfs",
            "_fstatvfs",
            "_NSFileSystemFreeSize",
            "_NSFileSystemSize",
            "_NSURLVolumeAvailableCapacityKey",
            "_NSURLVolumeAvailableCapacityForImportantUsageKey",
            "_NSURLVolumeAvailableCapacityForOpportunisticUsageKey",
            "_NSURLVolumeTotalCapacityKey",
        ],
        strings: &[
            "NSURLVolumeAvailableCapacity",
            "NSURLVolumeTotalCapacityKey",
            "NSFileSystemFreeSize",
        ],
        common_reason: "E174.1",
    },
    Category {
        name: "ActiveKeyboards",
        symbols: &[],
        strings: &["activeInputModes"],
        common_reason: "54BD.1",
    },
    Category {
        name: "UserDefaults",
        symbols: &["_OBJC_CLASS_$_NSUserDefaults"],
        strings: &["NSUserDefaults"],
        common_reason: "CA92.1",
    },
];

/// What the scan found for one category.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The category's short name.
    pub category: &'static str,
    /// What gave it away (`_stat`, `"systemUptime"`).
    pub evidence: Vec<String>,
}

/// The categories an executable uses.
pub fn scan(undefined: &[String], bytes: &[u8]) -> Vec<Found> {
    let mut found = Vec::new();
    for category in CATEGORIES {
        let mut evidence: Vec<String> = category
            .symbols
            .iter()
            .filter(|symbol| undefined.iter().any(|u| u == *symbol))
            .map(|symbol| (*symbol).to_string())
            .collect();
        for string in category.strings {
            if super::macho::contains(bytes, string.as_bytes()) {
                evidence.push(format!("\"{string}\""));
            }
        }
        if !evidence.is_empty() {
            found.push(Found {
                category: category.name,
                evidence,
            });
        }
    }
    found
}

/// A category name as `api_reasons` may spell it (`FileTimestamp` or
/// `NSPrivacyAccessedAPICategoryFileTimestamp`), short.
pub fn short_name(name: &str) -> &str {
    name.strip_prefix("NSPrivacyAccessedAPICategory")
        .unwrap_or(name)
}

/// The categories found without a declared reason.
pub fn undeclared<'a>(
    found: &'a [Found],
    declared: &BTreeMap<String, Vec<String>>,
) -> Vec<&'a Found> {
    found
        .iter()
        .filter(|f| {
            !declared
                .iter()
                .any(|(name, reasons)| short_name(name) == f.category && !reasons.is_empty())
        })
        .collect()
}

/// The `api_reasons = { … }` line with every declared reason plus the
/// common reason of each missing category.
pub fn suggested_line(declared: &BTreeMap<String, Vec<String>>, missing: &[&Found]) -> String {
    let mut all: BTreeMap<String, Vec<String>> = declared
        .iter()
        .map(|(name, reasons)| (short_name(name).to_string(), reasons.clone()))
        .collect();
    for found in missing {
        let reason = CATEGORIES
            .iter()
            .find(|c| c.name == found.category)
            .map_or("<reason>", |c| c.common_reason);
        let reasons = all.entry(found.category.to_string()).or_default();
        if reasons.is_empty() {
            reasons.push(reason.to_string());
        }
    }
    let parts: Vec<String> = all
        .iter()
        .map(|(name, reasons)| {
            let quoted: Vec<String> = reasons.iter().map(|r| format!("\"{r}\"")).collect();
            format!("{name} = [{}]", quoted.join(", "))
        })
        .collect();
    format!("api_reasons = {{ {} }}", parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(name, reasons)| {
                (
                    (*name).to_string(),
                    reasons.iter().map(|r| (*r).to_string()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn the_template_binary_needs_what_it_declares() {
        // What `nm -u` lists for the template's release build.
        let undefined: Vec<String> = [
            "_fstat",
            "_fstatat",
            "_mach_absolute_time",
            "_stat",
            "_write",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        let found = scan(&undefined, b"no objc names");
        let names: Vec<&str> = found.iter().map(|f| f.category).collect();
        assert_eq!(names, ["FileTimestamp", "SystemBootTime"]);
        assert_eq!(found[0].evidence, ["_stat", "_fstat", "_fstatat"]);
        let template = declared(&[
            ("FileTimestamp", &["C617.1"]),
            ("SystemBootTime", &["35F9.1"]),
        ]);
        assert!(undeclared(&found, &template).is_empty());
    }

    #[test]
    fn missing_reasons_get_the_exact_line() {
        let undefined = vec!["_statfs".to_string(), "_stat".to_string()];
        let found = scan(&undefined, b"..NSUserDefaults..activeInputModes..");
        let names: Vec<&str> = found.iter().map(|f| f.category).collect();
        assert_eq!(
            names,
            [
                "FileTimestamp",
                "DiskSpace",
                "ActiveKeyboards",
                "UserDefaults"
            ]
        );
        let declared = declared(&[("NSPrivacyAccessedAPICategoryFileTimestamp", &["C617.1"])]);
        let missing = undeclared(&found, &declared);
        assert_eq!(missing.len(), 3);
        assert_eq!(
            suggested_line(&declared, &missing),
            "api_reasons = { ActiveKeyboards = [\"54BD.1\"], DiskSpace = [\"E174.1\"], FileTimestamp = [\"C617.1\"], UserDefaults = [\"CA92.1\"] }"
        );
        // An empty reason list is not a declaration.
        let empty = super::tests::declared(&[("DiskSpace", &[])]);
        assert_eq!(undeclared(&found, &empty).len(), 4);
    }
}
