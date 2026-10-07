## How icm detects it

`icm release macos` lists the code-signing identities with `security
find-identity -p codesigning`, which reads certificates only, never a
private key, so it never prompts. It searches host.toml `signing_keychain`
(or `ICM_KEYCHAIN`) when set, otherwise the user's keychain search list.

- With `[desktop.macos] identity = "auto"` it needs a *valid* (trusted)
  `Developer ID Application:` identity. Without one the release stops
  before anything is built.
- With a SHA-1 or a name, icm signs with that identity even when it is not
  a Developer ID, for example an Apple Distribution identity or a
  self-signed test certificate. The app is built and signed, without a
  secure timestamp, and the release then ends with this id, because Apple's
  notary service and Gatekeeper accept only Developer ID signatures.

`icm diagnose notarytool` reports it too, when the notary service says the
binary "is not signed with a valid Developer ID certificate".

## Fix

The owner acts:

1. In Apple Developer > Certificates, Identifiers & Profiles, create a
   *Developer ID Application* certificate. Only the team's Account Holder can.
2. Install it with its private key in the login keychain, or in a build
   keychain that host.toml `signing_keychain` names.
3. Leave `[desktop.macos] identity = "auto"`, or set it to the certificate's
   SHA-1 (`security find-identity -v -p codesigning` lists it).

Meanwhile `icm release macos --sign none` builds an ad-hoc-signed app and
DMG for local testing.
