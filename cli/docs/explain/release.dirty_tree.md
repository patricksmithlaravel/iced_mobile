## How icm detects it

A release records the commit it was built from (`source.git_rev` in
`artifacts.json`). `git status --porcelain --untracked-files=no` in the
project lists changed tracked files: then the artifacts would not match any
commit, and `icm release` stops with exit 1. With `--allow-dirty` it goes
on and records `dirty: true`. A project without a git commit is a WARN: the
release records no revision.

## Fix

Commit the changes and rerun, or pass `--allow-dirty` for a build that is
not meant for the stores (CI smoke builds use `--sign none --allow-dirty`).
