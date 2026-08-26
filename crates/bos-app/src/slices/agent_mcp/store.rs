//! No local persistence.
//!
//! Agent MCP mutations call the owning slice stores (`operator_notes`,
//! `work_queue`, and draft slices) so receipts remain attributed to the domain
//! state they change.
