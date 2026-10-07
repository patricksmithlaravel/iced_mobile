## How icm detects it

`icm ui --headless find <selector>` renders the view through the app's
harness (`icm-tree`) and keeps the widgets that match:

- `#name` (or `id:name`): the widget whose id is `name`
  (`.id("name")` in the view);
- any other text: widgets whose text is exactly that, or, when none is,
  those whose text contains it (ignoring case).

None matched. The detail lists the texts that are on screen.

## Fix

Run `icm ui --headless tree` and pick a widget's exact text or `#id`. A
widget that is scrolled or clipped away is listed with no on-screen
rectangle; `--viewport` changes the screen size.
