## How icm detects it

The harness prints `ICM_HARNESS {"protocol":N}` first; this icm speaks
protocol 1. A different `N`, or a harness that answers an `icm-*` command
or option with "unknown command" / "unknown option", means the app's
iced_mobile framework pin and this icm come from different tags.

## Fix

Use the icm built from the same iced_mobile tag as the app's iced
dependency (`icm --version` shows the rev this icm was built from), or move
the app's framework pin to this icm's tag. Every iced line, including
`iced_test` in `[dev-dependencies]`, must name the same source.
