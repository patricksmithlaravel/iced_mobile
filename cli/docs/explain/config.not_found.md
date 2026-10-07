## How icm finds icm.toml

1. `--config <path>` (or `ICM_CONFIG`): a file, or a directory holding `icm.toml`.
2. Otherwise the nearest `icm.toml` in the current directory or any parent.

The project directory is the directory of that file; the app package is the
workspace package whose `Cargo.toml` sits next to it, unless `[app] package`
names another.

## Fix

- Run icm from the app's directory (or a subdirectory of it).
- Pass `--config path/to/icm.toml`.
- No app yet: `icm new <dir> --id com.example.<name>`.
