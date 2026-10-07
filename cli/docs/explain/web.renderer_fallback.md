## How icm detects it

iced's `wgpu` renderer prefers WebGPU in the browser. Headless Chrome
exposes `navigator.gpu` but has no WebGPU adapter ("No available
adapters"), and many real browsers have none either; wgpu then needs its
WebGL2 backend, which iced's `webgl` feature compiles in. Without it the
app does not draw: in headless Chrome 154, wgpu takes the canvas for WebGPU,
finds no adapter, and iced's tiny-skia fallback then panics with
"Create softbuffer surface for window: ... A canvas context other than
`CanvasRenderingContext2d` was already created" (`run.app_panicked`).

`icm run web` reads iced's features as resolved for `wasm32` (`cargo
metadata --filter-platform wasm32-unknown-unknown`) and warns when `wgpu` is
on but `webgl` is off. The result's `device.renderer` names what the app
drew with (`api: "Gl"` is WebGL2 through SwiftShader in headless Chrome).

## Fix

In the app's Cargo.toml:

```toml
[target.'cfg(target_arch = "wasm32")'.dependencies]
iced = { …, features = ["fira-sans", "webgl"] }
```
