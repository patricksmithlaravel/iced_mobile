## How icm detects it

The web session starts headless Chrome with `--remote-debugging-pipe` and
drives it over the DevTools protocol on that pipe. This fails when Chrome
cannot start, does not answer `Browser.getVersion`, cannot load the page,
exits while the session runs, or cannot carry out a request (a screenshot,
an input event).

The evidence names `chrome.log` (Chrome's own output) and `session.log`
(the session host's), both in `target/icm/sessions/web/`.

## Fix

```sh
icm print tools                  # which Chrome icm found
icm run web --json -q            # a fresh session
```

Point icm at another Chrome with `chrome = "<path>"` in
`~/.config/icm/host.toml` or `ICM_CHROME=<path>`.
