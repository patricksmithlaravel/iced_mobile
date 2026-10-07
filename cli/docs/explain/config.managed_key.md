## Why

icm generates Info.plist and AndroidManifest.xml on every build from icm.toml,
so one value has one home. Overlays (`[ios.info_plist]`,
`[android.manifest] application` / `activity`) may only *add* keys. Setting a
generated key (e.g. `CFBundleIdentifier`, `MinimumOSVersion`, any `DT*` key,
`android:label`, `android:icon`) is refused; the detail names the icm.toml
key that sets it instead (for example `[app] id`, `[ios] min_os`).
