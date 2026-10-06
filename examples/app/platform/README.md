# platform/

icm generates every native file (Info.plist, AndroidManifest.xml, PrivacyInfo.xcprivacy,
index.html) from `icm.toml` on each build. Change `icm.toml`, not those files.

This directory holds what `icm.toml` points at, when the app needs it:

- `android/res/`: Android resources compiled over the generated ones (`[android] res`).
- `ios/resources/`: files copied into the root of the iOS app bundle.
- Scripts that `[checks]` runs after `icm run`, such as `ios/checks.sh`.

It is empty otherwise.
