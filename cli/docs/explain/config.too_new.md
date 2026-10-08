## How icm detects it

- `min_icm = "0.14.1-mobile.3"` names the oldest icm that may read the file.
  icm compares by semver *ordering*, so `0.14.2-mobile.1` and
  `0.15.0-mobile.1` satisfy `0.14.1-mobile.3` (a `>=` version requirement would
  not, because of semver's pre-release rule). The older spelling
  `icm = ">=0.14.1-mobile.3"` is read the same way. The version must name a
  release, `X.Y.Z-mobile.N`: semver orders every `0.14.1-mobile.N` below
  `0.14.1`, so `min_icm = "0.14.1"` names no icm and is `config.invalid`.
- `schema = 2` (or higher) was written by a newer icm.

icm reads `schema` and `min_icm` (or `icm`) before the rest of the file and
ignores every other key while it does. A file for a newer icm may hold keys
and tables this icm does not know, or values of a type it does not take. They
are not read, so this is the only finding, not `config.unknown_key` or
`config.invalid` (exit 3). icm 0.14.1-mobile.1 read the whole file first, so
it reports the first key it does not know as `config.unknown_key`.

## Fix

Install the icm the file asks for; the fix command names the tag:

```sh
cargo install --locked --git https://github.com/patricksmithlaravel/iced_mobile --tag v<version> icm
```

For a newer `schema` the file names no version, so the fix command lists the
release tags, newest first; install the first one with the command above:

```sh
git ls-remote --tags --refs --sort=-v:refname https://github.com/patricksmithlaravel/iced_mobile 'v*-mobile.*'
```

Keep the keys this icm does not know: the newer icm reads them.
