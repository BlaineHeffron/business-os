# Architecture

BusinessOS uses one Rust server and an embedded React interface.

The server stores workflow state in SQLite.

Provider writes use an outbox and explicit approval controls.

## Dependency direction

- `bos-kernel` contains execution, idempotency, retry, and delivery types.
- `bos-contracts` contains shared request and response types.
- `bos-profile-api` contains public extension interfaces.
- `bos-integrations` contains external provider clients.
- `bos-app` contains workflow modules, persistence, and HTTP routes.
- `bos-server` starts the process and serves the application.

Lower crates do not depend on `bos-app` or `bos-server`.

## Workflow modules

Each workflow module has one directory under `crates/bos-app/src/slices`.

A module can contain routes, domain logic, storage, background work, projections, and tests.

Shared persistence functions provide revision checks, idempotency, and audit records.

External effects enter the outbox in the same transaction as the domain change.

## Frontend

The React application is in `frontend`.

Generated TypeScript types come from `bos-contracts`.

Run `just ts-types` after a shared contract changes.

## Configuration

The environment registry in `bos-app` defines runtime settings.

The default settings bind the server to the local host and disable provider writes.

Keep credentials and client overlays outside this repository.
