use bos_contracts::client_profile::ClientProfile;

use super::store;
use crate::persistence::Persistence;

const CLIENT: &str = "test-client";

#[test]
fn upsert_profile_round_trips_and_updates_via_store_core() {
    let mut persistence = Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();

    let profile = ClientProfile {
        client_id: CLIENT.to_string(),
        company_name: Some("Example Company".to_string()),
        bio: Some("Commercial painting contractor.".to_string()),
        industry: None,
        website: Some("https://example.test".to_string()),
        persona: Some("Plain-spoken and practical.".to_string()),
    };
    store::upsert_profile(conn, CLIENT, "seed", &profile, "seed-profile-1", 1_000).expect("insert");

    let stored = store::load_profile(conn, CLIENT)
        .expect("load")
        .expect("stored");
    assert_eq!(stored, profile);

    let updated = ClientProfile {
        bio: Some("Industrial coatings and commercial repainting.".to_string()),
        website: None,
        ..profile
    };
    store::upsert_profile(conn, CLIENT, "seed", &updated, "seed-profile-2", 2_000).expect("update");

    let stored = store::load_profile(conn, CLIENT)
        .expect("load")
        .expect("stored");
    assert_eq!(stored.bio, updated.bio);
    assert_eq!(stored.website, None);
}
