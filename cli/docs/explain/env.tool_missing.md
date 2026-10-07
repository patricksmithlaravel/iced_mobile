## How icm detects it

A program icm runs was not found. Every external tool can be overridden
with `ICM_TOOL_<NAME>=/path/to/tool` (upper case, `-` as `_`), e.g.
`ICM_TOOL_XCRUN`, `ICM_TOOL_ADB`, `ICM_TOOL_CARGO`. `icm print tools` shows
what icm found and where.

## Fix

Run `icm doctor <platform> --fix --yes`, install the tool, or set the
override.
