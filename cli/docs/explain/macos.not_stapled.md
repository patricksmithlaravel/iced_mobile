## How icm detects it

A macOS release has two stages around the owner's notarization (design
§11.4). `icm release macos --dmg` packages the `.app` of stage 1, and the
app must already carry its notarization ticket so that Gatekeeper accepts
it offline. icm runs `xcrun stapler validate` on the app. `icm verify macos
--after-notarize` runs it on the app and on the DMG.

icm never notarizes or staples: those are the owner's commands, in the
stage's `UPLOAD.md` and `upload.sh`.

## Fix

The owner runs the commands of stage 1 (`icm upload-commands macos` prints
them):

1. `xcrun notarytool store-credentials …`, once per Mac.
2. `xcrun notarytool submit <Name>-<version>.app.zip --keychain-profile … --wait`.
3. `icm diagnose notarytool notary-app.json`, to check the answer.
4. `xcrun stapler staple <Name>.app`.
5. `icm release macos --dmg`.

Under `--sign none` this is a WARN, and the DMG is built from the
unnotarized app for local testing.
