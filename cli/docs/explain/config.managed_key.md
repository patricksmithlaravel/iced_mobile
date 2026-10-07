## Why

icm generates Info.plist and AndroidManifest.xml on every build from icm.toml,
so one value has one home. Overlays (`[ios.info_plist]`,
`[android.manifest] application` / `activity`) may only *add* keys. Setting a
generated key (e.g. `CFBundleIdentifier`, `MinimumOSVersion`, any `DT*` key,
`android:label`, `android:icon`, `android:configChanges`) is refused; the
detail names the icm.toml key that sets it instead (for example `[app] id`,
`[ios] min_os`), or, for a value only icm writes (`android:configChanges`,
`android:launchMode`, `android:exported`), why it is fixed. In the manifest
such an overlay would be a duplicate attribute, which aapt2 reports only when
it links the APK (`android.aapt2_failed`).
