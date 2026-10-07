## How icm detects it

`icm release web` loads the finished site the way a visitor's browser does
before it calls the release uploadable. It serves `dist/…/web/site/` on
`127.0.0.1` (at the path `[web] public_url` names), opens it in headless
Chrome with `?icm_events=1`, and requires three things:

- the app sends `ICM_EVENT ready` within 60 s (an app that never announces
  itself passes when a canvas with a size is drawn after 5 s);
- the screenshot taken a second later is not blank (99.5 % or more of one
  colour is blank);
- the console logged no error: no `console.error`, uncaught exception,
  failed load or crashed renderer.

A panic, a page that never draws or a console error fails the gate, and the
release is not uploadable. The run directory's `smoke/` holds
`console.ndjson`, `screen.png` and Chrome's log; the result's `smoke` sums
them up. `icm verify web` runs the same check on a site directory, and
`icm verify web --url <deployed url>` on the owner's host.

## Fix

Read the console records the evidence names, then reproduce the page with
its logs:

```sh
icm run web --release --json -q
icm logs web --level warn --json -q
```

A blank page with no error is usually a missing font
(`web.fonts_embedded`) or a renderer without a WebGL fallback
(`web.renderer_fallback`).
