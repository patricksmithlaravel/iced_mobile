## How icm detects it

A release build inherits icm's environment, so an app that reads a variable
at build time (`option_env!("API_TOKEN")`, `env!`, a build script) bakes its
value into the binary, the wasm or the site. Before it writes
`artifacts.json`, `icm release` searches every file it ships (the files
`artifacts.json` lists, inside a `.app` or the web `site/` too) and the
binaries cargo built for the release (an `.ipa`, `.aab`, `.deb`, `.msi` or
AppImage compresses them) for the value of each secret-named variable in its
environment (`*TOKEN*`,
`*KEY*`, `*SECRET*`, `*PASS*`, `*PRIVATE*`, 6 bytes or more, not a path):
raw, JSON-escaped and percent-encoded. A value found is a FAIL, and the
release is not uploadable; under `--sign none` it is a WARN. The detail names
the variable and the files, never the value.

`icm verify` searches the files `artifacts.json` lists, or the artifact, again
with the variables of its own environment: run without the variable, it
cannot know the value.

## Fix

Stop the app reading the variable at build time, or unset it for the
release:

```sh
env -u API_TOKEN icm release <target> --json -q
```

A value the app must ship, such as a public client key, goes under a name
without TOKEN, KEY, SECRET, PASS or PRIVATE, so icm does not take it for a
secret.
