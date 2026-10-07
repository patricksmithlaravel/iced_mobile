## How icm detects it

Before `screencapture -l <window id>`, icm asks macOS
(`CGPreflightScreenCaptureAccess`) whether the process that started it may
record the screen. macOS answers for the app responsible for icm: the
terminal, the editor or the agent app. Without that permission
`screencapture` cannot read the window ("could not create image from
window"), so icm renders the view headlessly instead, through the app's
`tests/icm.rs` harness at the window's size, scale and system appearance.

The run is still ok. The screenshot shows the view's initial state, not the
running window (a counter you clicked shows 0); `screen.source` in the
result is `headless` and `screen.note` says why.

## Fix

Nothing is required. For real window captures, the owner opens System
Settings > Privacy & Security > Screen & System Audio Recording, allows the
app that runs icm, and restarts that app. Agents never change this setting.
