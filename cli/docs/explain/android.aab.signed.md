## How icm detects it

Google Play takes only an App Bundle signed with the app's upload key.
After signing, `icm release android` (and `icm verify android`) checks the
bundle by reading what the tools print, never their exit codes:

- `jarsigner -verify -verbose -certs <aab>` must say `jar verified.` and not
  `jar is unsigned.` (an unsigned bundle exits 0). icm does not pass
  `-strict`, which older JDKs fail on every self-signed upload key;
- `keytool -printcert -jarfile <aab>` must show the upload key's certificate
  (its SHA-256, which `artifacts.json` records under `signing`).

A signature without a timestamp is reported as INFO: Google Play re-signs
the APKs it serves and does not need one.

This check fails when the bundle is unsigned, does not verify, or is signed
by another certificate than the upload key's. A `--sign none` release is
unsigned on purpose: its bundle gets the WARN `android.aab.unsigned`
instead.

## Fix

- Unsigned: release with `[android.signing] upload` configured and its
  password variables exported, or have the owner run the jarsigner line in
  `UPLOAD.md`.
- Another certificate: the bundle was signed with a different key than the
  one `[android.signing] upload` names; release again rather than signing by
  hand.
- Does not verify: the bundle changed after signing (`icm verify android`
  also reports `release.artifact_changed`); release again.
