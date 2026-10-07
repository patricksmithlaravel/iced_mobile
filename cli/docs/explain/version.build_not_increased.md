## How icm detects it

The stores refuse a build number they have seen before. icm records every
upload in `.icm/ledger.toml` (written by `icm ledger mark-uploaded`, the
last line of `upload.sh`), and `icm release <target>` compares `[app] build`
with the highest build recorded for that target. Not higher is exit 1
before anything is built; under `--sign none` it is a WARN, since unsigned
artifacts are not uploaded.

## Fix

Raise `[app] build` in icm.toml above the number in the detail, then rerun
the release. Keep `.icm/ledger.toml` in git so every checkout knows which
builds were used.
