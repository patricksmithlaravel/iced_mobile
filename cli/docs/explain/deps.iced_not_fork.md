## How icm detects it

A framework crate in `Cargo.lock` comes from crates.io
(`registry+https://github.com/rust-lang/crates.io-index`) or from upstream
`github.com/iced-rs/iced`. Those lack the Android and iOS support icm builds
on. Path dependencies (a local fork checkout) and other git URLs pass.

## Fix

```toml
iced = { git = "https://github.com/patricksmithlaravel/iced_mobile", tag = "v0.14.1-mobile.N" }
```
