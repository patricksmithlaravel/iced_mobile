## How icm detects it

icm carries a dated table of store floors (`cli/policy/stores.toml`: the
App Store's minimum SDK and deployment target, Google Play's targetSdk, the
16 KB page-size rule, ...). The release gates read their floors from it.
`icm doctor` and `icm release` compare the table's `reviewed` date with
today: more than 90 days is this WARN, because the stores may have raised a
floor since. `icm print policy` shows the table and the value of each rule
in force today; floors that take effect within 60 days are reported as INFO
`store.policy_upcoming`.

A WARN never changes the exit code (except under `--strict`).

## Fix

Install a newer icm, whose table has been reviewed more recently. Until
then, check the stores' current requirements before an upload.
