## How icm detects it

`[app] id` still starts with `com.example.`, the template's placeholder. It is
a WARN for development and for `release --sign none`, and exit 9 for a signed
release of the stores' and desktop targets (`ios`, `android`, `macos`,
`windows`, `linux`): the id becomes permanent at the first store upload, so
only the owner chooses it. `icm release web` always reports it as a WARN: a
static site has no store id, and the release goes on.

## Fix (owner)

Choose the reverse-DNS id the app will keep forever and set it in
`[app] id`.
