## How icm detects it

icm lists the code-signing identities with `security find-identity -p
codesigning`, in the keychain host.toml `signing_keychain` (or
`ICM_KEYCHAIN`) names, else in the user's keychain search list. It only
reads: it never imports, unlocks or trusts a certificate.

- `[ios.signing] distribution.identity = "auto"` takes the valid
  `Apple Distribution` (or older `iPhone Distribution`) identities of
  `[ios] team_id` whose certificate the App Store profile includes; device
  runs take `Apple Development` ones.
- A SHA-1 or a common name picks that identity even when the system does
  not trust it. The release is then still built and signed, but ends with
  exit 9 naming the problem `security` reported (an expired certificate, a
  missing Apple intermediate certificate, `CSSMERR_TP_NOT_TRUSTED`).
- `icm verify ios` checks the signature's leaf authority is an Apple
  Distribution certificate.

Under `--sign none` this is a WARN and the bundle is signed ad hoc.

## Fix

The owner installs their distribution certificate with its private key:
Xcode > Settings > Accounts > Manage Certificates > "+" > Apple
Distribution, or imports the `.p12` they exported earlier. A CI keychain is
named by `signing_keychain` in host.toml so it never has to join the search
list.
