## How icm detects it

Every table in icm.toml (and host.toml) rejects keys it does not define, so a
typo such as `[andriod]` or `colour = "#FFF"` fails loudly instead of being
ignored. The detail lists the keys allowed in that table.

`[checks]` keys must be dev platforms: `desktop`, `web`, `ios-sim`,
`ios-device`, `android`.

## A key from a newer icm

icm reads icm.toml's `min_icm` before its other keys. When it names a newer
icm, the finding is `config.too_new` (exit 4) instead, whatever keys the file
holds. So an unknown key in icm.toml is either a typo or a key from an icm
newer than the file says it needs: the file names no `min_icm` this icm can
read, or one this icm meets. The fix says which.

## Fix

Rename or remove the key at the evidence's line. Keys for values icm
generates (Info.plist, AndroidManifest.xml) are refused separately as
`config.managed_key`.

If a newer icm added the key, keep it: install that icm and set `min_icm` to
its version (raise it if the file has one), so an older icm reports
`config.too_new` with the install command.
