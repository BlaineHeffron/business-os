//! Operator-facing slice visibility for the browser shell.
//!
//! Slice enablement is client-level configuration; this module applies the
//! authenticated operator's visibility policy on top of it. It is intentionally
//! outside diagnostics so the app shell does not depend on the support-hub
//! health surface being enabled.

use crate::http::{AppState, OperatorScope};
use crate::store_core::StoreError;

pub fn visible_slice_ids(
    state: &AppState,
    scope: &OperatorScope,
    actor_id: &str,
) -> Result<Vec<String>, StoreError> {
    let enabled_slices = state.enabled_slice_ids();
    let report_config = crate::slices::owner_reports::service::config_from_sources(
        state.owner_reports_overlay.as_ref().as_ref(),
    );
    let mut visible = Vec::new();
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    for slice in enabled_slices {
        let allowed = match slice.as_str() {
            "admin_settings" => matches!(scope, OperatorScope::All),
            "accounting" => {
                crate::slices::accounting::service::cached_financial_visibility_allowed(
                    conn,
                    &state.client_id,
                    scope,
                    state.accounting_visibility_policy,
                )?
            }
            "owner_reports" => {
                crate::slices::owner_reports::service::operator_allowed(&report_config, actor_id)
            }
            _ => true,
        };
        if allowed {
            visible.push(slice);
        }
    }
    Ok(visible)
}
