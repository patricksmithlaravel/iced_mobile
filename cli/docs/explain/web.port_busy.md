## How icm detects it

`icm run web` serves the site on `127.0.0.1:<port>` (8787 unless `--port`
says otherwise). This project's own earlier web session is stopped first
(a new `run web` replaces it), so a busy port belongs to something else:
another project's web session, or an unrelated server. The session's
`session.log` holds the operating system's error.

## Fix

```sh
icm run web --port 0 --json -q    # any free port; the URL is in artifacts.url
lsof -nP -iTCP:8787 -sTCP:LISTEN  # what holds the port
```
