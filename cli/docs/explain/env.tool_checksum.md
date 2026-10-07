## How icm detects it

icm downloads a few tools itself (bundletool, binaryen's `wasm-opt`,
appimagetool): `cli/tools.toml` pins each one's URL, size and sha256 for
every host. After `curl` finishes, icm compares the file's size and sha256
with the pin before it unpacks anything. When they differ, the download is
deleted and nothing is installed. The detail names both hashes.

## Fix

Rerun `icm doctor <platform> --fix --yes`: a truncated or corrupted download
fails this check and a retry fetches it again. If the hash keeps differing,
the file behind the URL changed or the pin is wrong; do not install the tool
by hand from that URL. Report it, or point `ICM_TOOL_<NAME>` at a copy you
trust.
