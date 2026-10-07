## How icm detects it

codesign needs the private key of the signing identity. The first time a
tool uses a key, macOS asks in a dialog whether to allow it; icm runs
codesign with stdin closed and a 60 s limit (`ICM_CODESIGN_TIMEOUT` sets
another), so a codesign still running then is waiting for a dialog nobody
will answer. icm stops it and exits 9: only the owner can grant access.

## Fix

The owner runs the release once in a terminal on the Mac and answers the
dialog with "Always Allow". For a CI keychain the owner grants Apple's
tools access once, with the keychain's password:

```sh
security set-key-partition-list -S apple-tool:,apple: -s -k "$KEYCHAIN_PASSWORD" ci.keychain-db
```

and names the keychain in host.toml (`signing_keychain`), so it never joins
the user's search list.
