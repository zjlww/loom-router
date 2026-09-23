# AGENTS.md - rules for code agents in this repo

LoomRouter is a headless Rust project. Keep it a single portable CLI binary;
do not add a graphical runtime or installer toolchain.

## Documentation

- Do not create loose research, plan, design, or decision files.
- Keep `README.md`, `CHANGELOG.md`, `CONTRIBUTING.md`, and `SECURITY.md`
  current when a change makes them wrong.
- Explain non-obvious implementation decisions in code comments instead of a
  separate document.

## Quality gate

Run these before committing:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
```

CI runs the same checks on Linux and builds the release executable on Linux,
macOS arm64, and macOS x64.

## Multi-platform

- Every change must compile on Linux and macOS.
- Use `dirs`, `std::env`, and platform-gated modules instead of hardcoded
  `/Users`, `/home`, or `/Applications` paths.
- New `#[cfg(target_os)]`, `#[cfg(unix)]`, or `#[cfg(windows)]` branches need
  a `// why` comment explaining the platform difference.
- Service management must keep the shared CLI and put launchd/systemd details
  behind the platform backend in `src/service.rs`.

## Git

- Direct pushes to `main` are blocked. Work on a `codex/...` branch and open a
  pull request.
- Use conventional commit subjects such as `feat(proxy):`, `fix(cli):`,
  `refactor(service):`, or `chore(ci):`.
- Do not amend commits already pushed to a pull request.
- Do not commit credentials, tokens, private keys, runtime databases, or
  `.env` files.

## House style

- Comments explain why, not what.
- Keep protocol knowledge in `src/translate`.
- Keep security-sensitive filesystem writes in `src/secure_fs`.
- Keep subprocess spawning off async worker threads with `spawn_blocking`.
- Add unit tests beside the module and end-to-end tests under `tests/`.
- Prefer deleting obsolete code over carrying compatibility layers with no
  caller.

## Text style

Never use em dashes or en dashes in user-facing text. The rule applies to CLI
output and documentation.
