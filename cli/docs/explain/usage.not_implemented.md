## Why

icm is built in phases. Phase 1 covers the dev loop (`new`, `doctor`,
`check`, `run`, `logs`, `shot`, `input`, `test`, `ui --headless`, `stop`);
releases, `init --adopt-*`, `framework`, `version`, `ledger`, `diagnose`
and `self update` come later. A command can also be listed but not yet
implemented in a particular build; `icm --version` names the build.

## Fix

Use a command this build implements (`icm --help`), or install a newer icm.
