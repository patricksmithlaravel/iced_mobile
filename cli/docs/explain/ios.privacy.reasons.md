## How icm detects it

Apple rejects uploads (ITMS-91053) that call a "required reason" API
without declaring why in `PrivacyInfo.xcprivacy`. icm scans the release
executable: its undefined symbols (`_stat`, `_fstat`, `_fstatat`, `_lstat`,
`_getattrlist*` for FileTimestamp; `_mach_absolute_time` for
SystemBootTime; `_statfs*`, `_statvfs*` and the volume-capacity keys for
DiskSpace; `_OBJC_CLASS_$_NSUserDefaults` for UserDefaults) and the
Objective-C names in its bytes (`systemUptime`, `activeInputModes`,
`NSUserDefaults`, ...), which objc2 looks up by name at run time.

Every category found must have at least one reason in `[ios.privacy]
api_reasons`, which icm writes into the manifest. Rust's standard library
alone needs FileTimestamp and SystemBootTime, which the template declares.
`icm verify ios` reads the manifest inside the `.ipa` instead.

## Fix

The detail names each category and what gave it away, and the fix prints
the whole `api_reasons` line with a common reason for each new category.
Check every reason against Apple's list ("Describing use of required reason
API") before using it: the reason is a declaration to App Review.

```toml
[ios.privacy]
api_reasons = { FileTimestamp = ["C617.1"], SystemBootTime = ["35F9.1"], DiskSpace = ["E174.1"] }
```
