# An iced_mobile app

One Rust codebase, `src/lib.rs`, for the desktop, the web, iOS and Android, built with
[iced_mobile](https://github.com/patricksmithlaravel/iced_mobile) and run with `icm`.

```sh
icm doctor --fix                                # check the machine; --fix --yes also installs what is missing
icm run desktop                                 # or web, ios-sim, android: build, launch, screenshot
icm logs android --level warn                   # the app's live logs
icm test                                        # unit tests and tests/flows/*.ice, headless
icm shot --headless --all-viewports             # screenshots at phone and desktop sizes, no device
icm explain <id>                                # what a check or error id means and how to fix it
```

`icm.toml` holds the app's identity, icon, permissions and platform settings; icm generates
Info.plist, AndroidManifest.xml and index.html from it on every build. `AGENTS.md` is the guide for
coding agents, and lists the known limitations of this version of iced_mobile.
