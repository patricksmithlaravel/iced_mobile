## How icm detects it

The harness could not parse an `.ice` flow: it reported `invalid flow: …`
for the file in the evidence. icm stops with exit 3 after reporting the
other tests.

## Fix

An `.ice` file is a header, a line of dashes, then one instruction per line:

```
viewport: 402x874
mode: Immediate
-----
click "Increment"
type "Milk"
type enter
expect "Count: 1"
```

`click "<text>"` targets a widget by its exact text (a text field by its
placeholder, or by its value once typed into); `type "<text>"` types into
the focused field; `type enter|tab|escape|backspace` presses a key;
`expect "<text>"` passes when some widget shows exactly that text. Fix the
line the detail names and rerun `icm ui --headless ice <file>`.
