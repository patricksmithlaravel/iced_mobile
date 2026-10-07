## How icm detects it

App Store Connect requires a privacy policy URL and a support URL for every
app, and Google Play requires a privacy policy URL (the Data safety
section). Neither is in the binary, so icm cannot check the store; it
checks that icm.toml has them in `[store]` (`privacy_policy_url`,
`support_url`), so `UPLOAD.md` can list them for the owner. A missing one
is a WARN: the release is still built.

## Fix

The owner publishes the pages and sets the URLs:

```toml
[store]
privacy_policy_url = "https://example.com/privacy"
support_url = "https://example.com/support"
```
