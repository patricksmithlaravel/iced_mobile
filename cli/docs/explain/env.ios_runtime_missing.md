## How icm detects it

`icm run ios-sim` reads `xcrun simctl list -j runtimes available` and picks
the newest iOS runtime at or above `[ios] min_os`; `--runtime min` picks the
lowest such runtime (to test the minimum OS), `--runtime 18.3` a specific
one. The detail lists the runtimes that are installed.

## Fix

```sh
icm doctor ios-sim --fix --yes          # runs xcodebuild -downloadPlatform iOS (about 8 GB)
icm run ios-sim --runtime newest        # or pick an installed runtime
```

Lowering `[ios] min_os` below what the app needs is not a fix.
