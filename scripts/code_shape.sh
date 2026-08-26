#!/usr/bin/env bash
# Machine-checks the structural rules in AGENTS.md / docs/ARCHITECTURE.md.
# Run via `just code-shape`. CI runs this; a violation is a build failure.
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
err() { echo "code-shape: $1" >&2; fail=1; }

# Rule 1: process-env config reads stay in env_registry.rs; test env mutation
# stays isolated behind http::test_support::EnvGuard.
test_env_marker_ok=1
while IFS= read -r marker; do
    case "$marker" in
        crates/bos-app/src/http.rs:*) ;;
        *) err "test env access marker outside crates/bos-app/src/http.rs: $marker"; test_env_marker_ok=0 ;;
    esac
done < <(grep -rnE 'code-shape: test-env-access (begin|end)' crates apps --include='*.rs' || true)

mapfile -t test_env_starts < <(grep -n "code-shape: test-env-access begin" crates/bos-app/src/http.rs || true)
mapfile -t test_env_ends < <(grep -n "code-shape: test-env-access end" crates/bos-app/src/http.rs || true)
if [ "${#test_env_starts[@]}" -ne 1 ] || [ "${#test_env_ends[@]}" -ne 1 ]; then
    err "test env access markers must appear exactly once in crates/bos-app/src/http.rs"
    test_env_marker_ok=0
fi
test_env_start=0
test_env_end=0
if [ "${#test_env_starts[@]}" -eq 1 ]; then
    test_env_start="${test_env_starts[0]%%:*}"
fi
if [ "${#test_env_ends[@]}" -eq 1 ]; then
    test_env_end="${test_env_ends[0]%%:*}"
fi
if [ "$test_env_start" -ge "$test_env_end" ]; then
    err "test env access begin marker must appear before the end marker"
    test_env_marker_ok=0
fi

test_support_bounds="$(
    awk '
        /^[[:space:]]*#\[cfg\(test\)\][[:space:]]*$/ { cfg = NR; next }
        cfg == NR - 1 && /^[[:space:]]*pub mod test_support[[:space:]]*\{/ {
            start = NR
            active = 1
            depth = 0
        }
        active {
            line = $0
            opens = gsub(/\{/, "{", line)
            line = $0
            closes = gsub(/\}/, "}", line)
            depth += opens - closes
            if (depth == 0 && NR > start) {
                print cfg ":" start ":" NR
                exit
            }
        }
    ' crates/bos-app/src/http.rs
)"
if [ -z "$test_support_bounds" ]; then
    err "could not locate #[cfg(test)] http::test_support bounds"
    test_env_marker_ok=0
else
    test_support_cfg="${test_support_bounds%%:*}"
    rest="${test_support_bounds#*:}"
    test_support_start="${rest%%:*}"
    test_support_end="${rest#*:}"
    if [ "$test_env_start" -le "$test_support_start" ] || [ "$test_env_end" -ge "$test_support_end" ]; then
        err "test env access block must stay inside #[cfg(test)] http::test_support"
        test_env_marker_ok=0
    fi
    if [ "$test_support_cfg" -ge "$test_support_start" ]; then
        err "http::test_support must remain gated by #[cfg(test)]"
        test_env_marker_ok=0
    fi
fi
while IFS= read -r hit; do
    rest="${hit#*:}"
    line="${rest%%:*}"
    case "$hit" in
        crates/bos-app/src/env_registry.rs:*) ;;
        crates/bos-app/src/http.rs:*)
            if [ "$test_env_marker_ok" -eq 1 ] &&
                [ "$line" -gt "$test_env_start" ] && [ "$line" -lt "$test_env_end" ]; then
                continue
            fi
            err "process env access outside env_registry/test support: $hit"
            ;;
        *) err "process env access outside env_registry/test support: $hit" ;;
    esac
done < <(grep -rnE 'std::env::(var|var_os|set_var|remove_var)\(|[^[:alnum:]_]env::(var|var_os|set_var|remove_var)\(' crates apps --include='*.rs' || true)

# Rule 2: bos-app top level is spine-only.
allowlist="lib.rs env_registry.rs persistence.rs store_core.rs slices.rs http.rs llm.rs outbox.rs operator_visibility.rs overlay.rs produce.rs"
for f in crates/bos-app/src/*.rs; do
    base="$(basename "$f")"
    case " $allowlist " in
        *" $base "*) ;;
        *) err "unexpected top-level module in bos-app: $f (features go in src/slices/<name>/)" ;;
    esac
done

# Rule 3: every slice directory has the canonical shape and a registry entry.
if [ -d crates/bos-app/src/slices ]; then
    shared_slice_modules="async_kickoff.rs datetime_input.rs draft_store.rs mutation_context.rs oauth_state.rs shipment_refs.rs"
    shared_slice_dirs="grounding"
    for f in crates/bos-app/src/slices/*.rs; do
        [ -f "$f" ] || continue
        base="$(basename "$f")"
        case " $shared_slice_modules " in
            *" $base "*) ;;
            *) err "unexpected shared slice module: $f (document it in docs/ARCHITECTURE.md and scripts/code_shape.sh, or move it into a slice)" ;;
        esac
    done

    canonical_slice_files="mod.rs store.rs service.rs routes.rs worker.rs projection.rs tests.rs"
    allowed_extra_slice_files="$(cat <<'EOF'
email_triage/catalog.rs
email_triage/facts.rs
email_triage/legacy.rs
email_triage/subjects.rs
enrichment/research.rs
enrichment/research_finalize.rs
enrichment/web_tier.rs
quote_workflows/profiles.rs
work_queue/agent_launch.rs
EOF
)"

    for dir in crates/bos-app/src/slices/*/; do
        [ -d "$dir" ] || continue
        name="$(basename "$dir")"
        case " $shared_slice_dirs " in
            *" $name "*)
                while IFS= read -r f; do
                    rel="${f#crates/bos-app/src/slices/}"
                    case "$rel" in
                        grounding/lookup.rs|grounding/mod.rs|grounding/render.rs|grounding/store.rs|grounding/tests.rs|grounding/tools.rs|grounding/types.rs|grounding/util.rs) ;;
                        *) err "unexpected extra shared slice module file: $rel (document it in docs/ARCHITECTURE.md and scripts/code_shape.sh, or consolidate)" ;;
                    esac
                done < <(find "$dir" -type f -name '*.rs' | LC_ALL=C sort)
                continue
                ;;
        esac
        for required in mod.rs tests.rs; do
            [ -f "$dir$required" ] || err "slice '$name' missing $required"
        done
        grep -q "${name}::SLICE" crates/bos-app/src/slices.rs ||
            err "slice '$name' not registered in slices.rs registry()"

        while IFS= read -r f; do
            base="$(basename "$f")"
            rel="${f#crates/bos-app/src/slices/}"
            direct_rel="${name}/${base}"
            if [ "$rel" = "$direct_rel" ]; then
                case " $canonical_slice_files " in
                    *" $base "*) continue ;;
                esac
            fi
            if ! printf '%s\n' "$allowed_extra_slice_files" | grep -qx "$rel"; then
                err "unexpected extra slice file: $rel (document it in docs/ARCHITECTURE.md and scripts/code_shape.sh, or consolidate into the canonical slice files)"
            fi
        done < <(find "$dir" -type f -name '*.rs' | LC_ALL=C sort)
    done
fi

# Rule 4: no raw INSERT/UPDATE/DELETE in slice code outside store.rs
# (mutations must flow through store_core via the slice's store).
if [ -d crates/bos-app/src/slices ]; then
    while IFS= read -r hit; do
        case "$hit" in
            */store.rs:*) ;;
            */oauth_state.rs:*) ;;
            *) err "raw SQL mutation outside slice store.rs: $hit" ;;
        esac
    done < <(grep -rnE '"(INSERT|UPDATE|DELETE) ' crates/bos-app/src/slices --include='*.rs' || true)
fi

# Rule 5: generated TS bindings are current with bos-contracts.
if [ -d frontend/src/types/generated ] && command -v cargo >/dev/null; then
    tmp_ts="$(mktemp -d)"
    if TS_RS_EXPORT_DIR="$tmp_ts" cargo test -p bos-contracts --features ts -q >/dev/null 2>&1; then
        find "$tmp_ts" -type f -name '*.ts' -exec sed -i 's/[[:space:]]*$//' {} +
        diff -rq "$tmp_ts" frontend/src/types/generated >/dev/null 2>&1 ||
            err "frontend/src/types/generated is stale — run 'just ts-types'"
    fi
    rm -rf "$tmp_ts"
fi

# Rule 6: REPO_MAP.md is current.
if command -v cargo >/dev/null && [ -f REPO_MAP.md ]; then
    tmp_map="$(mktemp)"
    if cargo run -q -p bos-server -- repo-map > "$tmp_map" 2>/dev/null; then
        diff -q "$tmp_map" REPO_MAP.md >/dev/null 2>&1 ||
            err "REPO_MAP.md is stale — run 'just repo-map'"
    fi
    rm -f "$tmp_map"
fi

# Rule 7: every routed slice is mounted, and build_router stays registry-ordered.
# client_profile is registered for REPO_MAP/migrations/env ownership, but it
# intentionally has no axum router.
routeless="client_profile"
registry_ids="$(grep -oE '[a-z_0-9]+::SLICE,' crates/bos-app/src/slices.rs | sed -E 's#::SLICE,##' | LC_ALL=C sort -u)"
router_ids="$(grep -oE 'slices::[a-z_0-9]+::routes::router' crates/bos-app/src/http.rs | sed -E 's#slices::([a-z_0-9]+)::routes::router#\1#')"
router_ids_sorted="$(printf '%s\n' "$router_ids" | sed '/^$/d' | LC_ALL=C sort -u)"

while IFS= read -r id; do
    [ -n "$id" ] || continue
    case " $routeless " in
        *" $id "*) continue ;;
    esac
    if ! printf '%s\n' "$router_ids_sorted" | grep -qx "$id"; then
        err "slice '$id' registered but has no build_router entry (unreachable) — add it to http.rs::build_router or add to routeless allowlist"
    fi
done <<< "$registry_ids"

while IFS= read -r id; do
    [ -n "$id" ] || continue
    if ! printf '%s\n' "$registry_ids" | grep -qx "$id"; then
        err "build_router mounts '$id' with no registry entry"
    fi
done <<< "$router_ids_sorted"

if [ "$(printf '%s\n' "$router_ids" | sed '/^$/d')" != "$router_ids_sorted" ]; then
    err "build_router slice entries must stay sorted by registry id"
fi

# Rule 8: literal operator error codes need friendly frontend messages unless
# they are intentionally served by the generic fallback copy.
backend_error_sources="$(find crates/bos-app/src -name '*.rs' ! -name 'tests.rs' -print)"
backend_error_codes="$(
    {
        perl -0777 -ne 'while(/error_response\(\s*[^,]+,\s*"([a-z_0-9]+)"/g){print "$1\n"}' $backend_error_sources
        perl -0777 -ne 'while(/StoreError::Domain\(\s*"([a-z_0-9]+)"\.to_string\(\)/g){print "$1\n"}' $backend_error_sources
        perl -0777 -ne 'while(/Err\("((?:produce_)[a-z_0-9]+)"\)/g){print "$1\n"}' crates/bos-app/src/produce.rs
        perl -0777 -ne 'while(/Self::[A-Za-z0-9_]+\s*=>\s*"(email_triage_[a-z_0-9]+)"/g){print "$1\n"}' crates/bos-contracts/src/email_triage.rs
    } | LC_ALL=C sort -u
)"
frontend_error_codes="$(awk '/const ERROR_MESSAGES/{inside=1; next} inside && /^};/{inside=0} inside {print}' frontend/src/lib/api/core.ts | grep -oE '^[[:space:]]*[a-z_0-9]+:' | sed -E 's/^[[:space:]]*([a-z_0-9]+):/\1/' | LC_ALL=C sort -u)"
generic_fallback_codes="$(cat <<'EOF' | sed -E 's/[[:space:]]+#.*$//' | LC_ALL=C sort -u
# --- internal / server / infra: generic fallback is correct, keep permanently ---
auth_lookup_failed # auth
handler_panicked # panic
store_sqlite_error # storage
persistence_busy # storage
approval_job_build_failed # outbox
work_queue_agent_payload_build_failed # agent
work_queue_agent_spawn_join_failed # agent
debug_agent_spawn_join_failed # agent
work_queue_agent_monitor_delivery_failed # agent
work_queue_agent_job_not_claimable # agent
work_queue_agent_result_invalid # callback
agent_launch_already_requested # duplicate
agent_monitor_unconfigured # agent
debug_agent_monitor_unconfigured # agent
debug_row_not_found # debug
google_credential_unavailable # credential
source_user_credential_unavailable # credential
scope_forbidden # auth
operator_session_expired # auth
operator_session_invalid # auth
EOF
)"

while IFS= read -r code; do
    [ -n "$code" ] || continue
    if printf '%s\n' "$frontend_error_codes" | grep -qx "$code"; then
        continue
    fi
    if printf '%s\n' "$generic_fallback_codes" | grep -qx "$code"; then
        continue
    fi
    err "operator error code '$code' has no ERROR_MESSAGES entry and is not in the generic-fallback allowlist — add a friendly message in api/core.ts or allowlist it"
done <<< "$backend_error_codes"

# Rule 9: frontend localStorage use is allowlisted. Operator auth is
# cookie-backed; do not persist tokens or workflow state in browser storage.
while IFS= read -r hit; do
    file="${hit%%:*}"
    case "$file" in
        frontend/src/lib/theme.ts | frontend/index.html) ;;
        *) err "frontend localStorage access outside allowlist: $hit" ;;
    esac
done < <(grep -rnE '\blocalStorage\b' frontend --exclude-dir=dist --exclude-dir=node_modules || true)

while IFS= read -r hit; do
    case "$hit" in
        frontend/src/lib/theme.ts:*'localStorage.getItem(STORAGE_KEY)'*) ;;
        frontend/src/lib/theme.ts:*'localStorage.setItem(STORAGE_KEY, theme)'*) ;;
        frontend/index.html:*'localStorage.getItem("bos-theme")'*) ;;
        *) err "frontend localStorage key is not allowlisted: $hit" ;;
    esac
done < <(grep -rnE '\blocalStorage\b' frontend/src/lib/theme.ts frontend/index.html || true)

if [ "$fail" -ne 0 ]; then
    echo "code-shape: FAILED" >&2
    exit 1
fi
echo "code-shape: ok"
