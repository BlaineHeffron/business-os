# Contributing

Thank you for contributing to BusinessOS.

## Before you start

Open an issue before a large change.

Describe the problem, the proposed result, and the affected modules.

Do not add client data or deployment settings.

## Development procedure

1. Install the tools listed in `README.md`.
2. Create a focused branch.
3. Add the smallest change that solves the problem.
4. Add or update focused tests.
5. Run `cargo fmt --all --check`.
6. Run `cargo clippy --workspace --all-targets -- -D warnings`.
7. Run `cargo test --workspace`.
8. Run `./scripts/code_shape.sh`.
9. Run `npm --prefix frontend run check` for frontend changes.
10. Run `npm --prefix frontend test` for frontend changes.

## Pull requests

Explain the user-visible result and the main design choice.

List the checks that you ran.

State any known limit or follow-up task.

Keep unrelated changes out of the pull request.

## Client overlay policy

Keep all client overlays outside this repository.

An overlay includes client names, identifiers, seeds, rules, credentials, provider accounts, hosts, and deployment settings.

Use invented names and values in tests and examples.
