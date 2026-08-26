//! bos-app: registries + spine + slices. All product logic lives in `slices/`.
//!
//! Top-level modules are the spine ONLY (code-shape enforces the allowlist):
//! - `env_registry` — the single env::var call site
//! - `persistence`  — sqlite + the migration registry
//! - `store_core`   — the single mutation path (revision + idempotency + receipt)
//! - `http`         — shared state, operator auth, router composition
//! - `llm`          — typed-LLM config/routing/execute (api | harness backends)
//! - `outbox`       — the single external-effect path (atomic enqueue + leased delivery)
//! - `operator_visibility` — shared operator-specific slice visibility
//! - `overlay`      — client deployment profile loader (identity, enabled slices, seeds)
//! - `produce`      — the shared produce-stage flow, generic over packet kinds
//! - `slices`       — the slice registry and all feature slices

pub mod env_registry;
pub mod http;
pub mod llm;
pub mod operator_visibility;
pub mod outbox;
pub mod overlay;
pub mod persistence;
pub mod produce;
pub mod slices;
pub mod store_core;

/// Generate REPO_MAP.md content from the registries.
pub fn repo_map_markdown() -> String {
    format!(
        "# REPO_MAP (generated — do not edit; run `just repo-map`)\n\n\
         ## Slices\n\n{}\n## Environment variables\n\n{}",
        slices::markdown(),
        env_registry::markdown_table()
    )
}

/// Generate the machine-readable slice id artifact consumed by frontend gates.
pub fn slice_ids_json() -> String {
    let ids: Vec<&str> = slices::registry().iter().map(|slice| slice.id).collect();
    format!(
        "{}\n",
        serde_json::to_string_pretty(&ids).expect("slice ids serialize")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_slice_ids_artifact_is_current() {
        let expected = slice_ids_json();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../frontend/src/lib/generated/slice_ids.json");
        let actual = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        assert_eq!(
            actual, expected,
            "frontend slice id artifact is stale; run `just slice-ids`"
        );
    }
}
