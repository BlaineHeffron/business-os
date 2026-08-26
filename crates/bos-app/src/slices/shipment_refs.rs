//! Shared shipment-reference conversion helpers for StockForge-backed slices.

use bos_contracts::claim_drafts::{ClaimShipmentDocumentRef, ClaimShipmentRefs};
use bos_integrations::stockforge_read::SfShipmentRefs;

use crate::store_core::StoreError;

pub(crate) fn claim_refs_from_sf(refs: Option<&SfShipmentRefs>) -> Option<ClaimShipmentRefs> {
    refs.map(|refs| ClaimShipmentRefs {
        shipping_platform: refs.shipping_platform.clone(),
        platform_shipment_id: refs.platform_shipment_id.clone(),
        carrier: refs.carrier.clone(),
        carrier_service: refs.carrier_service.clone(),
        mode: refs.mode.clone(),
        tracking_number: refs.tracking_number.clone(),
        pro_number: refs.pro_number.clone(),
        bol_number: refs.bol_number.clone(),
        tracking_url: refs.tracking_url.clone(),
        document_refs: refs
            .document_refs
            .iter()
            .map(|doc| ClaimShipmentDocumentRef {
                kind: doc.kind.clone(),
                url: doc.url.clone(),
            })
            .collect(),
        claim_platform: refs.claim_platform.clone(),
        claim_api_supported: refs.claim_api_supported,
    })
}

pub(crate) fn serialize_refs(
    refs: Option<&ClaimShipmentRefs>,
) -> Result<Option<String>, StoreError> {
    refs.map(|refs| {
        serde_json::to_string(refs)
            .map_err(|err| StoreError::Domain(format!("serialize shipment refs: {err}")))
    })
    .transpose()
}

pub(crate) fn deserialize_refs(raw: Option<&str>) -> Option<ClaimShipmentRefs> {
    raw.and_then(|raw| serde_json::from_str(raw).ok())
}
