use std::collections::BTreeMap;

use bos_integrations::accounting_read::{CustomerRecord, TierSource};
use bos_integrations::shopify::ShopifyWriteConfig;

use super::{service, store};
use crate::http::test_support::EnvGuard;
use crate::outbox;
use crate::persistence::Persistence;

const CLIENT: &str = "test-client";

fn customer(
    id: &str,
    name: &str,
    email: Option<&str>,
    tier: Option<&str>,
    active: bool,
) -> CustomerRecord {
    CustomerRecord {
        customer_id: id.to_string(),
        display_name: name.to_string(),
        company_name: None,
        email: email.map(str::to_string),
        phone: None,
        active,
        tier_raw: tier.map(str::to_string),
        tier_source: tier
            .map(|_| TierSource::CustomerTypeRefName)
            .unwrap_or(TierSource::NotProvided),
        updated_at: Some("2026-06-01T00:00:00Z".to_string()),
    }
}

fn mapping() -> BTreeMap<String, service::TierMappingTarget> {
    let mut mapping = BTreeMap::new();
    mapping.insert(
        "wholesale".to_string(),
        service::TierMappingTarget {
            tag: Some("Wholesale".to_string()),
            metafield_namespace: Some("customer".to_string()),
            metafield_key: Some("tier".to_string()),
            metafield_value: Some("Wholesale".to_string()),
            segment_query: Some("customer_tags CONTAINS 'Wholesale'".to_string()),
        },
    );
    mapping
}

#[test]
fn mapping_uses_overlay_when_env_is_unset() {
    let _env = EnvGuard::unset("BOS_SHOPIFY_TIER_MAPPING_JSON");
    let overlay = crate::overlay::CustomerTierSyncOverlay {
        tier_mappings: [(
            "Wholesale".to_string(),
            crate::overlay::CustomerTierSyncTargetOverlay {
                tag: Some("Wholesale".to_string()),
                ..Default::default()
            },
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    };

    let mapping = service::mapping_from_sources(&overlay).expect("mapping");

    assert_eq!(
        mapping
            .get("wholesale")
            .and_then(|target| target.tag.as_deref()),
        Some("Wholesale")
    );
}

#[test]
fn resolver_can_copy_any_qbo_tier_to_shopify_metafield_and_tag() {
    let _env = EnvGuard::unset("BOS_SHOPIFY_TIER_MAPPING_JSON");
    let overlay = crate::overlay::CustomerTierSyncOverlay {
        copy_qbo_tier: true,
        metafield_namespace: Some("customer".to_string()),
        metafield_key: Some("tier".to_string()),
        write_tag: true,
        tag_prefix: Some("tier:".to_string()),
        ..Default::default()
    };
    let resolver = service::target_resolver_from_sources(&overlay).expect("resolver");
    let mut persistence = Persistence::open_in_memory().expect("db");
    crate::slices::accounting::store::upsert_customer_snapshots(
        persistence.connection(),
        CLIENT,
        &[
            customer(
                "c1",
                "Contractor",
                Some("contractor@example.test"),
                Some("Contractor"),
                true,
            ),
            customer(
                "c2",
                "Distributor",
                Some("distributor@example.test"),
                Some("Distributor"),
                true,
            ),
        ],
        1_000,
    )
    .expect("seed customers");
    let customers =
        crate::slices::accounting::store::list_customers(persistence.connection_ref(), CLIENT)
            .expect("customers");

    let plan = service::build_plan_with_resolver(&customers, &resolver);

    assert_eq!(plan.actions.len(), 2);
    assert!(plan.skipped.is_empty());
    assert_eq!(plan.actions[0].qbo_tier, "Contractor");
    assert_eq!(
        plan.actions[0].shopify.metafield_namespace.as_deref(),
        Some("customer")
    );
    assert_eq!(
        plan.actions[0].shopify.metafield_key.as_deref(),
        Some("tier")
    );
    assert_eq!(
        plan.actions[0].shopify.metafield_value.as_deref(),
        Some("Contractor")
    );
    assert_eq!(
        plan.actions[0].shopify.tag.as_deref(),
        Some("tier:Contractor")
    );
    assert_eq!(
        plan.actions[0].shopify.exclusive_tag_prefix.as_deref(),
        Some("tier:")
    );
    assert_eq!(
        plan.actions[1].shopify.metafield_value.as_deref(),
        Some("Distributor")
    );
    assert_eq!(
        plan.actions[1].shopify.tag.as_deref(),
        Some("tier:Distributor")
    );
}

#[test]
fn mapping_version_is_source_neutral() {
    let version = service::mapping_version(&mapping());

    assert!(version.starts_with("mapping:"));
}

#[test]
fn plan_uses_qbo_tiers_and_skips_unmapped_or_ungrounded_customers() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    crate::slices::accounting::store::upsert_customer_snapshots(
        persistence.connection(),
        CLIENT,
        &[
            customer(
                "c1",
                "Corner Market",
                Some("buyer@example.test"),
                Some("Wholesale"),
                true,
            ),
            customer("c2", "No Email", None, Some("Wholesale"), true),
            customer(
                "c3",
                "Retail",
                Some("retail@example.test"),
                Some("Retail"),
                true,
            ),
            customer(
                "c4",
                "Inactive",
                Some("old@example.test"),
                Some("Wholesale"),
                false,
            ),
            customer("c5", "No Tier", Some("none@example.test"), None, true),
        ],
        1_000,
    )
    .expect("seed customers");
    let customers =
        crate::slices::accounting::store::list_customers(persistence.connection_ref(), CLIENT)
            .expect("customers");
    let plan = service::build_plan(&customers, &mapping());
    assert_eq!(plan.actions.len(), 1);
    assert_eq!(plan.actions[0].qbo_customer_id, "c1");
    assert_eq!(plan.actions[0].qbo_tier, "Wholesale");
    assert_eq!(plan.actions[0].shopify.tag.as_deref(), Some("Wholesale"));
    let reasons: Vec<&str> = plan
        .skipped
        .iter()
        .map(|skip| skip.reason.as_str())
        .collect();
    assert!(reasons.contains(&"shopify_match_email_missing"));
    assert!(reasons.contains(&"qbo_tier_unmapped"));
    assert!(reasons.contains(&"qbo_customer_inactive"));
    assert!(reasons.contains(&"qbo_tier_missing"));
}

#[test]
fn approval_enqueues_shopify_job_and_delivery_dry_runs_behind_gate() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    crate::slices::accounting::store::upsert_customer_snapshots(
        persistence.connection(),
        CLIENT,
        &[customer(
            "c1",
            "Corner Market",
            Some("buyer@example.test"),
            Some("Wholesale"),
            true,
        )],
        1_000,
    )
    .expect("seed customers");
    let customers =
        crate::slices::accounting::store::list_customers(persistence.connection_ref(), CLIENT)
            .expect("customers");
    let plan = service::build_plan(&customers, &mapping());
    store::stage_run(
        persistence.connection(),
        CLIENT,
        "user_example",
        "run_1",
        &plan,
        "stage_1",
        2_000,
    )
    .expect("stage");
    let run = store::get_run(persistence.connection_ref(), CLIENT, "run_1")
        .expect("get")
        .expect("run");
    assert_eq!(run.revision, 1);
    let job = service::build_approval_job(&run, "user_example", 3_000).expect("job");
    let payload: bos_integrations::shopify::ShopifyTierSyncOutboxPayload =
        serde_json::from_str(&job.payload_json).expect("payload");
    assert_eq!(payload.actions[0].target.exclusive_tag_prefix, None);
    store::approve_run(
        persistence.connection(),
        crate::slices::mutation_context::MutationContext {
            client_id: CLIENT,
            actor_id: "user_example",
            expected_revision: Some(1),
            idempotency_key: "approve_1",
            now_ms: 3_000,
        },
        "run_1",
        &job,
    )
    .expect("approve");
    let claimed = outbox::claim_due_jobs(
        persistence.connection(),
        CLIENT,
        Some(service::PROVIDER_SHOPIFY),
        60_000,
        10,
        4_000,
    )
    .expect("claim");
    assert_eq!(claimed.len(), 1);
    let config = ShopifyWriteConfig {
        shop_domain: Some("example.myshopify.com".to_string()),
        access_token: Some("token".to_string()),
        api_version: "2026-01".to_string(),
        write_enabled: false,
    };
    match service::execute_job(&claimed[0], &config, 5_000) {
        outbox::AttemptOutcome::Delivered { result_json } => {
            let result: serde_json::Value = serde_json::from_str(&result_json).expect("json");
            assert_eq!(result["dry_run"], serde_json::json!(true));
            assert_eq!(result["action_count"], serde_json::json!(1));
        }
        other => panic!("expected dry-run delivery, got {other:?}"),
    }
}
