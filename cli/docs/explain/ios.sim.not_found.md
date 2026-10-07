## How icm detects it

`icm run ios-sim` uses, in order: `--sim` or `--device` (a simulator name or
UDID), host.toml `[ios] simulator_udid`, else icm's managed simulator
`icm-<type>-ios-<version>` (for example `icm-iphone-17-ios-27.0`), which it
creates when missing. This fails when the name or UDID matches no simulator
in `xcrun simctl list devices`, when that simulator's runtime is gone, or
when host.toml `[ios] simulator_type` names a device type the chosen runtime
does not support.

icm only creates, shuts down or deletes simulators whose names start with
`icm-`; one you name with `--sim` is only booted.

## Fix

```sh
xcrun simctl list devices available     # pick a name or UDID
icm run ios-sim --sim "<name or UDID>"
icm run ios-sim                          # or let icm use its managed simulator
```
