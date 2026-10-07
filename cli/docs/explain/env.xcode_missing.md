## How icm detects it

The developer directory comes from `$DEVELOPER_DIR`, else `xcode-select -p`.
Only the Command Line Tools being selected, or `xcodebuild -version` failing,
means no usable Xcode. All Xcode paths derive from that directory.

## Fix (owner)

Install Xcode from the App Store, then:

```sh
sudo xcode-select -s /Applications/Xcode.app
sudo xcodebuild -license   # if the licence was never accepted
```
