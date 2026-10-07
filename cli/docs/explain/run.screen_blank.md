## How icm detects it

At least 99.5 % of the screenshot's pixels are within a small distance of
one colour. It is a WARN by default and a FAIL with `--expect-content`.

Common causes: text with no font (keep iced's `fira-sans` feature), a theme
that draws text in the background colour, or (Android) a window with
`FLAG_SECURE`, which icm reports as `android.screen.secure` instead.

## Fix

Compare with `icm shot --headless` (the same view rendered without a
device) and read `icm logs <platform> --level warn`.
