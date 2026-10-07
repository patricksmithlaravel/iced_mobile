## How icm detects it

A Linux binary records the newest glibc symbol version it calls
(`GLIBC_2.34`, for example) in its `.gnu.version_r` section. It does not
start on a system whose glibc is older. icm reads that section from the
release executable, and from `usr/bin/*` inside a `.deb` or AppImage for
`icm verify linux`. It compares the newest `GLIBC_x.y` with
`[desktop.linux] glibc_floor`, which defaults to 2.35 (Ubuntu 22.04 and
Debian 12).

A binary built on a newer distribution links against that distribution's
glibc, so a release built on, say, Ubuntu 24.04 (glibc 2.39) fails this gate.
The artifacts are still written, but they are not uploadable.

## Fix

- Build the release in the `ubuntu:22.04` container, as the Linux job of
  `.github/workflows/icm-desktop.yml` does.
- Or raise `glibc_floor` in icm.toml, if the app need not run on older
  distributions.
