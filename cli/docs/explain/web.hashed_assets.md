## How icm detects it

A release names the app's modules by their content: `pkg/app-<h8>.js` and
`pkg/app_bg-<h8>.wasm`, where `<h8>` is the first 8 hex digits of the
file's sha256. `index.html` loads both names explicitly, so hosts can cache
the modules forever (`_headers` marks them immutable) while `index.html`
itself is revalidated on every visit, and a new release is never mixed with
an old cached module.

The gate reads `index.html`, finds the `import init from` and
`module_or_path` names, and checks that both files exist and that each
name's hash matches its content. It fails for a site that was edited after
the release, or that `icm release web` did not make.

## Fix

Make the release again rather than editing its files:

```sh
icm release web --json -q
```
