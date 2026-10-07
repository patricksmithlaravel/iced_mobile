## How icm detects it

`[app] id` still starts with `com.example.`, the template's placeholder. It is
a WARN for development and for `release --sign none`, and exit 9 for a signed
release: the id becomes permanent at the first store upload, so only the
owner chooses it.

## Fix (owner)

Choose the reverse-DNS id the app will keep forever and set it in
`[app] id`.
