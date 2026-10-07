## How icm detects it

`icm release web` compresses the optimized `.wasm` with `gzip -9` and
compares the result with `[web] size_budget_kb` in icm.toml (4096 KB by
default). Hosts serve the module compressed, so the gzip size is roughly
what a visitor downloads before the app can start. The size report,
`size.json` beside the site (and the result's `size`), lists the `.wasm`
before and after `wasm-opt`, its gzip size, the JavaScript glue and the
whole site.

The template app is about 4 MB, 1.6 MB gzipped.

## Fix

Make the module smaller: drop dependencies and features the web build does
not need (a `[target.'cfg(target_arch = "wasm32")'.dependencies]` section
can trim them for the web alone), and avoid large embedded assets (load them
from the site instead, with `[app] resources`). If the size is what the app
needs, raise the budget:

```toml
[web]
size_budget_kb = 6144
```
