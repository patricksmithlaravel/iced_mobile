## How icm detects it

Search order: host.toml `android_sdk`, `$ANDROID_HOME`, `$ANDROID_SDK_ROOT`,
`~/Library/Android/sdk`, `/opt/homebrew/share/android-commandlinetools`,
`/usr/local/share/android-commandlinetools`, `~/Android/Sdk`. A directory
counts when it holds `platform-tools`, `cmdline-tools`, `build-tools` or
`platforms`.

## Fix (owner)

Install the command-line tools (`brew install --cask
android-commandlinetools`), accept the licences yourself (`sdkmanager
--licenses`), or set `android_sdk` in `~/.config/icm/host.toml`. Then
`icm doctor android --fix --yes` installs the packages icm needs.
