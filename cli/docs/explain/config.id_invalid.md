## Rule

`[app] id` must be a reverse-DNS identifier valid on every store:

- at least two dot-separated segments (`com.acme.notes`);
- each segment starts with a letter;
- only ASCII letters, digits and underscores (Android rejects `-`).

The id is permanent after the first store upload. `com.example.*` is
accepted but reported as `app.id.placeholder`.
