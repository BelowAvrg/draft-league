//! Coach administration: budgets, draft order, and member fields.

use pokemon_draft_site::coaches::CoachError;
use pokemon_draft_site::db::Db;

/// A database with one active season and one person who has logged in.
async fn fixture() -> (Db, i64, i64) {
    let db = Db::connect("sqlite::memory:").await.expect("connect");
    sqlx::query("INSERT INTO season (id, name, min_roster, max_roster, is_active) VALUES (1, 'S1', 8, 12, 1)")
        .execute(db.pool())
        .await
        .expect("season");
    let person = db.upsert_discord_person("100000000000000001", "ash").await.expect("person");
    (db, 1, person.id)
}

#[tokio::test]
async fn budget_cannot_drop_below_what_is_spent() {
    let (db, season, person) = fixture().await;
    db.add_coach(season, person, 100, 1).await.expect("add");
    let coach = db.coaches(season).await.expect("list")[0].id;

    // pokemon 3 (venusaur) comes from the seed migration; ids are dex numbers.
    // Spent is the roster's tier-list cost, so it needs a price.
    sqlx::query("INSERT INTO cost (season_id, pokemon_id, points) VALUES (1, 3, 40)")
        .execute(db.pool())
        .await
        .expect("cost");
    sqlx::query(
        "INSERT INTO pick (coach_id, season_id, pokemon_id, pick_number, points_paid)
         VALUES (?, 1, 3, 1, 40)",
    )
    .bind(coach)
    .execute(db.pool())
    .await
    .expect("pick");

    let err = db.update_coach(coach, 30, 1, "").await.expect_err("must refuse");
    assert!(matches!(err, CoachError::BelowSpent { budget: 30, spent: 40 }), "got {err:?}");

    // Trimming to exactly what is spent is still a valid roster.
    db.update_coach(coach, 40, 1, "").await.expect("40 is allowed");

    let listed = db.coaches(season).await.expect("list");
    assert_eq!(listed[0].spent, 40);
    assert_eq!(listed[0].remaining(), 0);
}

#[tokio::test]
async fn draft_positions_are_unique_within_a_season() {
    let (db, season, first) = fixture().await;
    let second = db.upsert_discord_person("100000000000000002", "misty").await.expect("p2").id;

    db.add_coach(season, first, 100, 1).await.expect("add first");
    let err = db.add_coach(season, second, 100, 1).await.expect_err("same slot");
    assert!(matches!(err, CoachError::PositionTaken(1)), "got {err:?}");

    db.add_coach(season, second, 100, 2).await.expect("add second");
    let err = db.add_coach(season, first, 100, 3).await.expect_err("already a coach");
    assert!(matches!(err, CoachError::Duplicate), "got {err:?}");
}

#[tokio::test]
async fn hand_entered_discord_ids_are_shaped_like_snowflakes() {
    let (db, ..) = fixture().await;
    for bad in ["", "12345", "not-a-number", "1234567890123456789012345"] {
        let err = db.person_by_hand(bad, "x").await.expect_err("{bad} must be refused");
        assert!(matches!(err, CoachError::DiscordId), "{bad}: got {err:?}");
    }
    // Re-adding the same ID returns the existing person rather than duplicating.
    let a = db.person_by_hand("100000000000000009", "brock").await.expect("create");
    let b = db.person_by_hand("100000000000000009", "brock").await.expect("again");
    assert_eq!(a, b);
}

#[tokio::test]
async fn candidates_exclude_people_who_already_coach() {
    let (db, season, person) = fixture().await;
    assert_eq!(db.people_without_coach(season).await.expect("before").len(), 1);
    db.add_coach(season, person, 100, 1).await.expect("add");
    assert!(db.people_without_coach(season).await.expect("after").is_empty());
}

#[tokio::test]
async fn admin_can_set_another_members_fields() {
    let (db, _season, person) = fixture().await;
    db.set_member_fields(person, Some("  AshK  "), true).await.expect("set");
    let p = db.person(person).await.expect("read").expect("exists");
    assert_eq!(p.showdown_username.as_deref(), Some("AshK"), "whitespace is trimmed");
    assert!(p.is_admin);

    // Blanking the field clears it rather than storing an empty string.
    db.set_member_fields(person, Some("   "), false).await.expect("clear");
    let p = db.person(person).await.expect("read").expect("exists");
    assert_eq!(p.showdown_username, None);
    assert!(!p.is_admin);
}
