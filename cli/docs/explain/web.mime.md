## How icm detects it

Browsers compile a `.wasm` while it downloads only when it is served as
`application/wasm`. wasm-bindgen's loader falls back to a slower path on any
other type, so the app may still start, which is why icm checks the type
itself:

- `icm release web` checks that the site's `_headers` (which Netlify and
  Cloudflare Pages apply) declares `Content-Type: application/wasm` for the
  content-hashed module, and records the type the page received in the
  serve check (`web.serve_smoke`);
- `icm verify web --url <deployed url>` loads the deployed site in headless
  Chrome and fails when the host served the `.wasm` with another type or
  status.

## Fix

Configure the host to serve `.wasm` as `application/wasm`, then deploy
again and rerun `icm verify web --url`:

- Netlify, Cloudflare Pages: `_headers` in the site does it.
- Amazon S3: the `aws s3 cp … --content-type application/wasm` line in
  UPLOAD.md.
- nginx, Apache, Caddy: the snippets in `hosting/` beside the site.
- GitHub Pages serves `.wasm` correctly.

```sh
icm upload-commands web
```
