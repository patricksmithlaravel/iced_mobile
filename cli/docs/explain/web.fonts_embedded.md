## How icm detects it

A web page has no system fonts iced can load, so a web build must carry
its font inside the `.wasm`. iced's `fira-sans` feature embeds Fira Sans.
`icm run web` reads iced's features as resolved for `wasm32` (`cargo
metadata --filter-platform wasm32-unknown-unknown`) and reports a WARN when
`fira-sans` is off; a release makes it a FAIL. An app that loads its own
font (`iced::Application::font`) can ignore the warning.

Without a font, text draws as nothing: the screenshot shows buttons and
fields with no labels.

## Fix

In the app's Cargo.toml:

```toml
[target.'cfg(target_arch = "wasm32")'.dependencies]
iced = { …, features = ["fira-sans", "webgl"] }
```
