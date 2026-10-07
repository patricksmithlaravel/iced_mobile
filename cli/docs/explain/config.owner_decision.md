## How icm detects it

`icm release` needs answers only the owner can give, and icm.toml has no
default for them:

- iOS: `[ios] team_id` (the Apple Developer team) and
  `[ios] uses_non_exempt_encryption` (export compliance, asked for every
  build);
- Android: `[android.signing] upload`, the owner's upload key (by path,
  alias and the names of the variables holding its passwords).

Under `icm release --sign auto` (the default) each is a FAIL and the
release stops with exit 9 before it builds anything (Android still builds
the unsigned bundle first). `owner_steps` in the result lists every item.
Under `--sign none` they are WARNs and the artifacts are unsigned and not
uploadable.

## Fix

Stop and hand `errors[0].fix` (and `owner_steps`) to the owner. An agent
does not invent a team id, an encryption answer or a key. To keep working
without them, use `icm release <target> --sign none`.
