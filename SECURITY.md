# Security Policy

## Reporting a vulnerability

Open a normal issue. LoomRouter runs entirely on the operator's machine and is
bound to loopback by default; there is no shared hosted instance.

Include the version, operating system, and the smallest reproduction you can.
Do not paste working credentials. If a proof of concept needs a provider key,
describe the request shape instead.

## Supported versions

Security fixes ship in the latest release. There are no backports.

## Attack surface

The security-sensitive areas are:

- **Credential storage.** Provider keys live in
  `~/.loomrouter/config.json`; the local proxy token lives in
  `~/.loomrouter/local-token` and the managed Codex configuration block.
  Security-sensitive writes go through `src/secure_fs`.
- **Local proxy access.** The gateway binds to `127.0.0.1` and requires a
  random bearer token. A path that lets an unauthenticated local process or
  browser page spend stored provider credentials is in scope.
- **Credential routing.** Sending provider A's key to provider B is in scope.
- **Logs and accounting.** Request and response bodies contain prompts and
  source code and must never be persisted or logged.
- **Service installation.** A service command must not overwrite unrelated
  user files, remove the wrong service, or start a binary other than the one
  explicitly installed.

## Known limitations

- On Windows, `src/secure_fs` cannot match Unix owner-only permissions with an
  equivalent ACL. Windows is not a supported service target.
- Service management supports macOS launchd and Linux systemd only.

## Out of scope

- An attacker who already has code execution as the LoomRouter user.
- Vulnerabilities in upstream model providers.
