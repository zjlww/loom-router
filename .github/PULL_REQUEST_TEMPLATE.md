## What changed, and why

<!--
Lead with the behaviour a user would notice, then the reason. If this fixes
something broken, describe the failure mode and keep that context at the fix
site as well.
-->

Fixes #

## How it was verified

<!--
Name the check that would have failed before this change. For provider-facing
changes, identify the provider and endpoint you exercised.
-->

## Checklist

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test --locked`
- [ ] No request or response body is logged or persisted
- [ ] Provider-specific payload fields stay inside `src/translate`
- [ ] Version changes update `Cargo.toml` and `CHANGELOG.md` together
