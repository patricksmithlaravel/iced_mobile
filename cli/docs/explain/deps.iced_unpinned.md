## How icm detects it

The lockfile source of a framework crate carries `?branch=...` or no query at
all (the default branch). A later `cargo update` would silently move the app
to another framework revision.

## Fix

Pin every iced line with `tag = "v0.14.1-mobile.N"` (or `rev = "<sha>"`).
