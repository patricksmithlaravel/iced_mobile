## How icm detects it

`icm release ios` and `icm run ios-device` pass the signing keychain
(host.toml `signing_keychain`, or `ICM_KEYCHAIN`) to codesign with
`--keychain`. On macOS 26 codesign searches only the keychains of the user's
keychain search list (`security list-keychains -d user`), and `--keychain`
only narrows that search: it does not add the keychain to it. So for a
keychain outside the list codesign answers `<sha1>: no identity found`,
although `security find-identity -p codesigning <keychain>` lists the
identity.

icm reads the list only after codesign has failed that way, and only when
the keychain is not in it. It never checks the list before codesign runs, so
a Mac whose codesign finds the identity (macOS 27 did) is not affected. The
failure keeps its own id when the keychain is in the list (`ios.sign.no_identity`),
when no signing keychain is set, and when the list cannot be read or is
empty. A codesign failure that does not say `no identity found` keeps
`ios.sign.verify`, as before.

Exit 9: only the owner can put the keychain in the list, and the release
must run again after it does.

## Fix

The owner does this around the release. icm never changes the list. The
error's commands use the list as it is now:

```sh
security list-keychains -d user -s <each listed keychain> <signing keychain>
icm release ios
security list-keychains -d user -s <each listed keychain>
```

The first command appends the signing keychain for the release, the last
restores the list.

The same condition on macOS is `macos.sign.keychain_not_in_search_list`.
