## How icm detects it

A release builds with `cargo --locked`: exactly the lock the project has, so
the artifact matches a commit. `icm new` writes no `Cargo.lock`; `icm check`,
`icm run` and `icm doctor web --fix --yes` resolve it. Before the build,
`icm release` checks that the lock `cargo metadata` names exists, so a new
app gets this precondition (exit 1) instead of cargo's "cannot create the
lock file because --locked was passed" deep in the build.

## Fix

Create the lock and commit it:

```sh
icm check <platform> --json -q   # ios-device, android or desktop; the web: icm doctor web --fix --yes
git add Cargo.lock
git commit -m "Add Cargo.lock"
```

Then rerun the release. An uncommitted lock is `release.dirty_tree`.
