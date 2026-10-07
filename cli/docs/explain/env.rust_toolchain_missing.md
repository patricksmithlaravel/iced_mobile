## How icm detects it

The project's `rust-toolchain.toml` (or a rustup override) selects a
toolchain that is not installed. With `RUSTUP_AUTO_INSTALL=0`, rustup reports
it instead of downloading it during the first cargo call.

## Fix

```sh
icm doctor --fix --yes
# or:
rustup toolchain install <toolchain from the detail>
```
