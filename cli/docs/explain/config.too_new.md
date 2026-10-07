## How icm detects it

- `min_icm = "0.14.1-mobile.3"` names the oldest icm that may read the file.
  icm compares by semver *ordering*, so `0.14.2-mobile.1` and
  `0.15.0-mobile.1` satisfy `0.14.1-mobile.3` (a `>=` version requirement would
  not, because of semver's pre-release rule). The older spelling
  `icm = ">=0.14.1-mobile.3"` is read the same way.
- `schema = 2` (or higher) was written by a newer icm.

## Fix

Install the icm the file asks for; the fix command names the tag:

```sh
cargo install --locked --git https://github.com/patricksmithlaravel/iced_mobile --tag v<version> icm
```
