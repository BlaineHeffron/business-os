# BusinessOS

BusinessOS is a local-first operations server for small businesses.

The server combines workflow modules, provider connectors, approval controls, an audit trail, and a React operator interface.

## Status

This repository is an early public release.

Review the code and security model before production use.

## Requirements

- Rust 1.85 or newer
- Node.js 22 or newer
- npm 10 or newer
- `just` for the documented commands

## Install and run

1. Clone the repository.
2. Install the frontend packages with `npm --prefix frontend ci`.
3. Build the frontend with `npm --prefix frontend run build`.
4. Run the server with `cargo run -p bos-server`.
5. Open `http://127.0.0.1:4400`.

The server stores local data in `./state` by default.

Use [config/example-settings.sh](config/example-settings.sh) for safe local settings.

The example contains no credentials.

## Development

Run these focused checks before a change:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
./scripts/code_shape.sh
npm --prefix frontend run check
npm --prefix frontend test
```

Run `just gate` for the Rust checks and the code shape check.

## Architecture

The Rust workspace has a small dependency direction.

- `bos-kernel` contains shared execution and delivery types.
- `bos-contracts` contains browser-safe data types.
- `bos-profile-api` defines extension interfaces.
- `bos-integrations` contains provider connectors.
- `bos-app` contains workflow modules and HTTP routes.
- `bos-server` starts the process and serves the embedded frontend.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the module rules.

## Client overlays

This repository does not contain client overlays.

Keep client names, identifiers, policies, seeds, credentials, and deployment settings in a separate private repository.

See [docs/CLIENT_OVERLAYS.md](docs/CLIENT_OVERLAYS.md) before you add a deployment profile.

## Contributions and security

Read [CONTRIBUTING.md](CONTRIBUTING.md) before you send a change.

Report a security problem with the process in [SECURITY.md](SECURITY.md).

Follow [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) in project spaces.

## License

BusinessOS uses the Apache License 2.0.

See [LICENSE](LICENSE).
