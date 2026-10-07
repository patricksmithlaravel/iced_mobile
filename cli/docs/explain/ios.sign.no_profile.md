## How icm detects it

An App Store build embeds an App Store provisioning profile
(`embedded.mobileprovision`); a device run needs a development profile that
lists the device. icm finds the profile by reference:
`[ios.signing] distribution.profile` (releases) or `development.profile`
(`icm run ios-device`) is `auto`, a UUID or a path.

- `auto` searches `~/Library/Developer/Xcode/UserData/Provisioning Profiles`
  and `~/Library/MobileDevice/Provisioning Profiles` (or the directories in
  `ICM_PROVISIONING_PROFILES`, `:`-separated) for a profile of the right
  kind for `[ios] team_id` and `[app] id` (exactly, or through a wildcard
  App ID), that includes the signing identity's certificate, lists the
  device (development) and has not expired. An exact App ID wins over a
  wildcard, then the latest expiry.
- A UUID picks that profile from the same directories; a path reads that
  file. Either is then checked the same way, and the reason it does not fit
  is reported as `ios.sign.profile_mismatch` or `ios.sign.profile_expired`.

icm reads the plist inside the profile directly, so the check runs on any
host. Under `icm release ios --sign none` a missing profile is a WARN.

## Fix

The owner creates the profile in the Apple Developer portal (Certificates,
Identifiers & Profiles > Profiles: "App Store Connect" for releases, "iOS
App Development" for device runs), for the App ID `<team>.<app id>` and
their certificate, downloads it, and either double-clicks it (Xcode copies
it into the search directory) or sets its path:

```toml
[ios.signing]
distribution = { identity = "auto", profile = "~/profiles/Notes_App_Store.mobileprovision" }
```
