## How icm detects it

The `wasm-bindgen` CLI must be exactly the version of the `wasm-bindgen`
crate in the app's `Cargo.lock`; any other version fails with a schema
mismatch. icm uses `<cache>/tools/wasm-bindgen/<version>/bin/wasm-bindgen`
and accepts a copy on `PATH` only when `wasm-bindgen --version` matches.

## Fix

```sh
icm doctor web --fix --yes
# which runs:
cargo install wasm-bindgen-cli --version <lock version> --locked --root <cache>/tools/wasm-bindgen/<lock version>
```
