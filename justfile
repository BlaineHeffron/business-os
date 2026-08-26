fmt:
    cargo fmt --all

lint:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

check:
    cargo check --workspace --all-targets

code-shape:
    ./scripts/code_shape.sh

repo-map:
    cargo run -q -p bos-server -- repo-map > REPO_MAP.md
    @echo "REPO_MAP.md regenerated"

referrer-spam-domains:
    ./scripts/update_referrer_spam_domains.sh

slice-ids:
    mkdir -p frontend/src/lib/generated
    cargo run -q -p bos-server -- slice-ids-json > frontend/src/lib/generated/slice_ids.json
    @echo "frontend/src/lib/generated/slice_ids.json regenerated"

gate: fmt lint test code-shape
    @echo "gate green"

# Changed-diff Rust mutation testing (cargo-mutants 0.27.x).
# Install: cargo install cargo-mutants --version 0.27.1 --locked
# -j is compatible with --in-diff; cargo-mutants 0.27 rejects -j with --in-place.
mutants-diff:
    node scripts/repo-quality-check.mjs --mutation-only

# Weekly per-slice cargo-mutants rotation. Not part of `just gate`.
# GNU `timeout` only (same 30-minute cap as `just coverage`).
# Example: just mutants-slice client_profile
# Exit 124 from GNU timeout means the wall-clock cap fired, not a mutants crash.
mutants-slice name:
    #!/usr/bin/env bash
    set -euo pipefail
    name='{{name}}'
    if [[ ! "$name" =~ ^[a-z0-9_]+$ ]]; then
      echo "mutants-slice: invalid slice name: $name" >&2
      exit 1
    fi
    if [[ ! -d "crates/bos-app/src/slices/$name" ]]; then
      echo "mutants-slice: no such slice: crates/bos-app/src/slices/$name" >&2
      exit 1
    fi
    mkdir -p "target/mutants-$name"
    timeout 1800 cargo mutants --file "crates/bos-app/src/slices/$name/**" -j 4 --timeout-multiplier 3 --output "target/mutants-$name"

# Regenerate TypeScript bindings from bos-contracts into the frontend.
ts-types:
    #!/usr/bin/env bash
    set -euo pipefail
    tmp_dir="$(mktemp -d)"
    trap 'rm -rf "$tmp_dir"' EXIT
    TS_RS_EXPORT_DIR="$tmp_dir" cargo test -p bos-contracts --features ts -q
    find "$tmp_dir" -type f -name '*.ts' -exec sed -i 's/[[:space:]]*$//' {} +
    rm -rf frontend/src/types/generated
    mkdir -p frontend/src/types
    mv "$tmp_dir" frontend/src/types/generated
    echo "frontend/src/types/generated refreshed"

fe-install:
    npm --prefix frontend install

fe-dev:
    npm --prefix frontend run dev

fe-check:
    npm --prefix frontend run check

fe-quality:
    npm --prefix frontend run quality:crap

# Rust llvm-cov JSON for CRAP. Must stay identical to rust.coverage.command
# in .quality-gates.json (minus `{outputDir}` mkdir/path). GNU `timeout` only;
# on macOS install coreutils or run `just crap` which uses the same cap in-config.
# Not part of `just gate`.
coverage:
    mkdir -p target/coverage/repo-quality
    timeout 1800 cargo llvm-cov --workspace --json --ignore-run-fail --output-path target/coverage/repo-quality/llvm-cov.json --ignore-filename-regex '(^|/)(tests\.rs$|tests/)'

# Rust CRAP ratchet against scripts/quality/crap-baseline-rust.json. Not part of `just gate`.
crap:
    node scripts/repo-quality-check.mjs --section rust

# Build the SPA bundle (embedded into bos-server at next cargo build).
fe-build: ts-types
    npm --prefix frontend run build

# Start the development server on 127.0.0.1:4400.
server:
    cargo run -p bos-server
