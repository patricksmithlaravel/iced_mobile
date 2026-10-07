## How icm detects it

`icm release ios` builds with line tables
(`profile.release.debug="line-tables-only"`), runs `xcrun dsymutil` on the
executable before the bundled copy is stripped, and zips the dSYM next to
the `.ipa` for crash symbolication. Two gates check it:

- `ios.dsym.uuid`: the dSYM's `LC_UUID` equals the executable's.
- `ios.dsym.line_tables`: `xcrun dwarfdump --debug-line` on the dSYM names
  at least one source file of the app's own package. An empty dSYM (no
  debug info reached dsymutil) has the right UUID too, so only the line
  table proves crash reports will show the app's files and lines.

## Fix

Build releases through `icm release ios`, which sets `debug` on the
command line (it wins over the app's Cargo.toml). It does not set `strip`:
a `strip = true` or `strip = "debuginfo"` in the app's `[profile.release]`
removes the debug map dsymutil follows, so remove it. dsymutil reads the
object files cargo keeps in
`target/icm/release-target/aarch64-apple-ios/release/deps/`, so do not clean
them between the build and the dSYM.
