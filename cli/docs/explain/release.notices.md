## How icm detects it

An app built with iced ships hundreds of open-source crates, most under
licences (MIT, Apache-2.0, ...) that require their notices to travel with
the binary, and Fira Sans, whose SIL Open Font License must ship with the
font. `icm release` writes `THIRD_PARTY_NOTICES.txt` from `cargo metadata`
for the build's target (the crates that end up in the artifact, their
licence texts, and Fira Sans's licence when it is embedded), and the
target's pipeline puts it inside the artifact: the `.app` bundle, Android's
`assets/`, the web site, the desktop packages.

This gate fails when no artifact carries it, or when the place a pipeline
recorded is not there: icm looks inside directories and zip archives
(`.ipa`, `.aab`, `.apk`, zipped sites and apps). `icm verify` looks again,
and for an artifact built elsewhere searches the whole archive.

## Fix

A FAIL here is a bug in the target's pipeline: report it with the run
directory. The file itself is also in the dist directory next to the
artifacts.
