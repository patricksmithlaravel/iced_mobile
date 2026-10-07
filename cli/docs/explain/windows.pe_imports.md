## How icm detects it

Rust's `x86_64-pc-windows-msvc` target links the Visual C++ runtime
(`vcruntime140.dll`) dynamically unless the crates are built with
`+crt-static`. A machine with Visual Studio has the runtime, so the app
starts there. A clean Windows does not have it, so the installed app fails
with "VCRUNTIME140.dll was not found" (design Appendix C item 20).

`icm release windows` builds with the static C runtime for every crate
(`--config target.x86_64-pc-windows-msvc.rustflags=["-C",
"target-feature=+crt-static"]`). It then reads the executable's PE import
and delay-import tables, and fails when they name a runtime Windows does not
ship: `vcruntime*`, `msvcp*`, `concrt*`, `vccorlib*`, the old `msvcr*`, or
MinGW's `libgcc_s*`, `libstdc++*` and `libwinpthread*`. The Universal CRT
(`ucrtbase.dll`, `api-ms-win-crt-*`) is part of Windows 10 and later, so it
passes. `icm verify windows` reads the imports on any host.

## Fix

- Look for a `RUSTFLAGS` or `CARGO_ENCODED_RUSTFLAGS` in the environment
  that runs the release. Either one replaces the target rustflags icm sets.
  Unset it, or add `-C target-feature=+crt-static` to it.
- A dependency that ships its own DLL needs that DLL installed next to the
  executable. icm's installers do not do that yet, so report the case.
