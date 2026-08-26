//! Client profile: per-client company background, seeded from the overlay and
//! read at produce time to ground outward-facing LLM tasks. Store-only (no
//! routes, no produce flavor of its own).

pub mod store;

#[cfg(test)]
mod tests;

use crate::slices::SliceSpec;

pub const SLICE: SliceSpec = SliceSpec {
    id: "client_profile",
    title: "Client Profile",
    summary: "Per-client company background seeded from the overlay and read by outward-facing LLM tasks.",
    routes: &[],
    tables: &[store::ENTITY_KIND],
    env_vars: &[],
    read_models: &[],
};
