# Contributing to LoomRouter

LoomRouter is a headless Rust gateway. Contributions should preserve the
single-binary CLI boundary.

## Quality gate

Run the complete gate before pushing:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
```

CI also builds the release executable on Linux x64, macOS arm64, and macOS
x64. Platform-specific code must keep all three builds working.

## House style

- Comments explain why, never what.
- Record the failure mode that caused a fix near the code that owns it.
- Name tests as the sentence they assert, for example
  `normalize_usage_reads_kimi_per_choice_placement`.
- Keep unit tests beside their module and cross-module behavior under `tests/`.
- Log failures instead of discarding them with `let _ =`.
- Delete obsolete code instead of leaving unused abstraction layers.

## Ownership boundaries

### Protocol translation

Provider payload shapes belong in `src/translate`. Do not read usage or stream
fields with provider-specific literals outside that module.

### Secure filesystem writes

Every write with a security property goes through `src/secure_fs`. Do not add
another atomic-write helper or weaken owner-only permissions.

### Platform differences

Keep `cfg` branches in the module that owns the platform behavior. Shell,
service-manager, path, and permission differences belong in dedicated helpers
or in `src/service.rs`, not scattered through callers.

### Versioning and release notes

The version lives only in `Cargo.toml`. A release tag must match it, and
`CHANGELOG.md` must contain a matching `## <version>` section.

## Credentials

- Provider keys stay in `~/.loomrouter/config.json`.
- The local proxy token is generated at runtime and stored in
  `~/.loomrouter/local-token`.
- Never log request or response bodies.
- Never commit credentials, runtime databases, or generated model catalogs.
