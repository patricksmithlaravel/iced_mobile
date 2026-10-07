## How icm detects it

icm reads `Cargo.lock` and groups every framework crate (`iced`,
`iced_core`, `iced_winit`, `iced_test`, ... but not third-party `iced_*`
crates) by source, revision and version. More than one group means the build
holds two copies of iced: types do not match across them, and on Android the
shell `set_android_app` fills is not the one the app runs.

Common causes:

- a widget crate depends on crates.io `iced`;
- two `Cargo.toml` lines name the fork with different tags or revs, or one
  with `.git` and one without;
- `iced` from git and `iced_test` from a path.

The evidence lists the lockfile lines of each copy.

## Fix

Put the same git URL and `tag`/`rev` on every iced line of every crate
(character for character), align or drop the third-party crate, then
`cargo update -p iced`.
