## How icm detects it

icm resolves the toolchain *the project* uses (its `rust-toolchain.toml`
included, via `rustc --print sysroot` in the project directory) and looks for
the target's standard library under `<sysroot>/lib/rustlib/<target>/lib`. A
target installed only on another toolchain (often `stable`) does not count.

Children run with `RUSTUP_AUTO_INSTALL=0`, so nothing is downloaded behind
your back.

## Fix

```sh
icm doctor <platform> --fix --yes
# or by hand, with the toolchain from the detail:
rustup target add --toolchain <toolchain> <target>
```
