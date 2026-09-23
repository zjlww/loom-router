# LoomRouter

LoomRouter is a headless local gateway for Codex and other OpenAI-compatible
agents. It publishes models from multiple providers under one local endpoint,
translates between the Responses, Chat Completions, and Anthropic protocols,
and keeps provider credentials on the local machine.

The project produces one `loom-router` executable for macOS and Linux.

## Features

- One local provider endpoint on `127.0.0.1:4180`.
- Routing across DeepSeek, OpenRouter, Kimi, Anthropic, OpenCode, and custom
  OpenAI-compatible providers.
- Responses, Chat Completions, and Anthropic Messages translation.
- Streaming, WebSocket transport, tool calls, vision, and reasoning summaries.
- Codex catalog and `config.toml` integration.
- Per-provider upstream proxy configuration.
- Request and token accounting in a local SQLite database.
- Per-user launchd on macOS and systemd on Linux.

## Build

Prerequisites:

- Rust stable
- A C toolchain for the bundled SQLite build

```bash
git clone https://github.com/zjlww/loom-router.git
cd loom-router
cargo build --locked --release
./target/release/loom-router --version
```

## Run

Run the gateway in the foreground:

```bash
./target/release/loom-router serve
```

Install it as a per-user background service:

```bash
./target/release/loom-router service install \
  --link ~/.local/bin/loom-router
./target/release/loom-router status
```

On macOS, `service install` manages the LaunchAgent
`dev.loomrouter.agent`. On Linux, it manages the user unit
`~/.config/systemd/user/loom-router.service` through `systemctl --user`.

Service commands:

```bash
loom-router service install [--link PATH]
loom-router service restart
loom-router service uninstall
loom-router service status
```

## Configuration

The authoritative configuration file is:

```text
~/.loomrouter/config.json
```

It stores the listen port, providers, API keys, enabled models, provider
proxies, and Codex integration state. Keep it private; it contains provider
credentials.

The local proxy token is generated at startup and written into the managed
Codex configuration block. Codex, not the operator, reads that token
automatically.

Relevant state:

| Path | Purpose |
| --- | --- |
| `~/.loomrouter/config.json` | Providers, credentials, and gateway settings |
| `~/.loomrouter/loom.db` | Request and token accounting |
| `~/.loomrouter/local-token` | Local proxy bearer token |
| `~/.codex/loom-router/merged-models.json` | Generated model catalog |
| `~/.codex/config.toml` | Contains the managed LoomRouter block |

## Other agents

Any OpenAI-compatible client can use the local endpoint:

```json
{
  "baseURL": "http://127.0.0.1:4180/v1",
  "apiKey": "loom"
}
```

Model IDs are listed by:

```bash
TOKEN="$(cat ~/.loomrouter/local-token)"
curl -s -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:4180/v1/models
```

## Development

The quality gate is:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
```

Layout:

```text
src/                  Rust library and CLI
  proxy/              HTTP, WebSocket, routing, and streaming
  translate/          Responses, Chat Completions, and Anthropic translation
  codex/              Codex config and catalog integration
  service.rs          launchd and systemd service management
tests/                Cross-module and end-to-end tests
```

## Releases

Pushing a tag such as `v0.2.19` starts the release workflow. It publishes
standalone archives with SHA-256 checksums for:

- Linux x64
- macOS arm64
- macOS x64

Each archive contains the `loom-router` executable.

## Environment overrides

These variables exist for development and debugging. They can execute code or
redirect credentials, so use them only when the target is fully trusted.

| Variable | Effect |
| --- | --- |
| `CODEX_BIN` | Codex CLI used for catalog capture |
| `CODEX_NATIVE_BASE_URL` | Overrides the native OpenAI/ChatGPT backend URL |
| `CODEX_HOME` | Overrides the Codex configuration directory |

## Security

- Provider API keys remain in `~/.loomrouter/config.json`.
- The local proxy requires the bearer token in
  `~/.loomrouter/local-token`.
- The gateway binds to loopback by default.
- Request and response bodies are not written to the accounting database.

See `SECURITY.md` for reporting and scope.

## License

MIT
