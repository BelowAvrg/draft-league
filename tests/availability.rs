//! Recurring weekly availability storage.

use pokemon_draft_site::availability::{AvailabilityError, Slot, valid_time_zone};
use pokemon_draft_site::db::Db;

#[test]
fn time_zone_shape_rejects_markup_and_accepts_iana_names() {
    assert!(valid_time_zone("America/Denver"));
    assert!(valid_time_zone("Etc/GMT+7"));
    assert!(valid_time_zone("UTC"));
    assert!(!valid_time_zone(""));
    assert!(!valid_time_zone("America/<script>"));
    assert!(!valid_time_zone("/America/Denver"));
}

#[tokio::test]
async fn availability_replaces_and_deduplicates_weekly_slots() {
    let db = Db::connect("sqlite::memory:").await.expect("connect");
    let person = db
        .upsert_discord_person("100", "Misty")
        .await
        .expect("person");
    let monday_evening = Slot::new(1, 36).expect("slot");
    let saturday_morning = Slot::new(6, 18).expect("slot");

    db.set_availability(
        person.id,
        "America/Denver",
        &[monday_evening, saturday_morning, monday_evening],
    )
    .await
    .expect("save");
    let saved = db.availability(person.id).await.expect("read");
    assert_eq!(saved.time_zone.as_deref(), Some("America/Denver"));
    assert_eq!(saved.slots, [monday_evening, saturday_morning]);

    db.set_availability(person.id, "America/Chicago", &[saturday_morning])
        .await
        .expect("replace");
    let saved = db.availability(person.id).await.expect("read again");
    assert_eq!(saved.time_zone.as_deref(), Some("America/Chicago"));
    assert_eq!(saved.slots, [saturday_morning]);

    let error = db.set_availability(person.id, "bad zone!", &[]).await;
    assert!(matches!(error, Err(AvailabilityError::InvalidTimeZone(_))));
}
