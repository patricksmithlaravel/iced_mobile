## How icm detects it

`icm release android` signs the bundle with the upload key that
`[android.signing] upload` names. Before signing it runs
`keytool -list -v -keystore <keystore> -alias <alias> -storepass:env <VAR>`
to read the key's algorithm and certificate, then
`jarsigner … -storepass:env <VAR> -keypass:env <VAR>`. The passwords reach
both tools only through the environment variables, never on a command line
or in a file.

This check fails when either tool cannot use the key: the store password is
wrong, the key has its own password and `key_pass_env` is not set to it, the
alias is not in the keystore, or the file is not a keystore keytool can read.
The step log in the evidence has keytool's or jarsigner's own message (the
passwords themselves never appear in it).

The release still writes the unsigned bundle
(`<package>-<version>-<build>-unsigned.aab`) and `UPLOAD.md`, whose first step
is the jarsigner line, then exits 9.

## Fix

Only the owner can fix it, since the key and its passwords are theirs:

- check `keystore` and `alias` in `[android.signing] upload` against
  `keytool -list -keystore <keystore>`;
- export the right password in the variable `store_pass_env` names (and in
  `key_pass_env`, when the key has its own password);
- release again, or sign the unsigned bundle with the jarsigner line in
  `UPLOAD.md`.

An agent never guesses, creates or replaces an upload key: Google Play ties
the app to it after the first upload.
