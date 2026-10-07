## How icm detects it

A release records the commit it was built from (`source.git_rev` in
`artifacts.json`), and it builds the project's `Cargo.lock` with `--locked`.
Two things make the artifacts match no commit, and `icm release` stops with
exit 1 on either:

- `git status --porcelain --untracked-files=no` in the project lists changed
  tracked files;
- `git ls-files --error-unmatch` says `Cargo.lock` is not tracked. A new app's
  lock is created by `icm check` or `icm run` after its first commit, so it
  starts out untracked, and a commit without it cannot rebuild the release.

With `--allow-dirty` the release goes on and records `dirty: true`. A project
without a git commit is a WARN: the release records no revision.

## Fix

Commit the changes and `Cargo.lock` (remove it from `.gitignore` if it is
listed there) and rerun, or pass `--allow-dirty` for a build that is not
meant for the stores (CI smoke builds use `--sign none --allow-dirty`).
