# Changelog

## 0.2.19

### Changed

- LoomRouter is now a headless-only Rust project. The previous graphical
  application surface and its build toolchain are removed.
- The crate lives at the repository root and produces a single `loom-router`
  executable.

### Added

- Per-user systemd service management on Linux alongside launchd on macOS.
- Standalone Linux x64, macOS arm64, and macOS x64 release archives with
  SHA-256 checksums.

### Fixed

- Provider-specific proxy routing remains available through the
  `provider_proxies` configuration map.
