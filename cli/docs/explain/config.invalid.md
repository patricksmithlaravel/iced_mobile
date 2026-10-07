## How icm detects it

icm.toml is parsed with spans, so every problem points at `file:line:col`:

- TOML syntax errors (an unclosed `[table`, a missing quote);
- a value of the wrong type (`build = "1"` instead of `build = 1`);
- a missing required key (`[app] name`, `[app] id`);
- a value outside its range (`build = 0`, `min_sdk` above `target_sdk`,
  `background = "white"` instead of `"#FFFFFF"`).

`errors[0].evidence[0]` names the file, the line and the line's text. When
several keys are wrong, each one is its own entry in `errors[]`.

The same id covers `~/.config/icm/host.toml`, and a `Cargo.toml` that
`cargo metadata` rejects.

## Fix

Edit the value at the evidence's line; `icm print config` shows the
resolved file with every default once it parses.
