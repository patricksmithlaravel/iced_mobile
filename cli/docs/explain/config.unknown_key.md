## How icm detects it

Every table in icm.toml (and host.toml) rejects keys it does not define, so a
typo such as `[andriod]` or `colour = "#FFF"` fails loudly instead of being
ignored. The detail lists the keys allowed in that table.

`[checks]` keys must be dev platforms: `desktop`, `web`, `ios-sim`,
`ios-device`, `android`.

## Fix

Rename or remove the key at the evidence's line. Keys for values icm
generates (Info.plist, AndroidManifest.xml) are refused separately as
`config.managed_key`.
