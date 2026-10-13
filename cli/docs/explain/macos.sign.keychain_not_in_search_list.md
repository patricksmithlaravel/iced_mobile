## How icm detects it

`icm release macos` passes the signing keychain (host.toml
`signing_keychain`, or `ICM_KEYCHAIN`) to codesign with `--keychain`. On
macOS 26 codesign searches only the keychains of the user's keychain search
list (`security list-keychains -d user`), and `--keychain` only narrows that
search: it does not add the keychain to it. So for a keychain outside the
list codesign answers `<sha1>: no identity found`, although `security
find-identity -p codesigning <keychain>` lists the identity.

icm reads the list only after codesign has failed that way, and only when
the keychain is not in it. It never checks the list before codesign runs, so
a Mac whose codesign finds the identity (macOS 27 did) is not affected, and
the release stops with this id only where codesign failed. The failure keeps
its own id (`tool.failed`) when the keychain is in the list, when no signing
keychain is set, when codesign fails for another reason, and when the list
cannot be read or is empty (icm cannot print the command that puts an empty
list back).

Exit 9: only the owner can put the keychain in the list, and the release
must run again after it does.

## Fix

The owner does this around the release. icm never changes the list. The
error's commands use the list as it is now:

```sh
security list-keychains -d user -s <each listed keychain> <signing keychain>
icm release macos
security list-keychains -d user -s <each listed keychain>
```

The first command appends the signing keychain for the release, the last
restores the list. The acceptance scripts (`phase2.sh` and `phase5.sh`) do
the same around their signed release only, and only on a CI runner
(`GITHUB_ACTIONS=true` with `ICM_ACCEPT_KEYCHAIN_SEARCH=1`); on a developer's
Mac they never change the list.

The same condition on iOS is `ios.sign.keychain_not_in_search_list`.
