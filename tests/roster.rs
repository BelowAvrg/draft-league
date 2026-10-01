//! Ownership (`roster_entry`) tracking the draft's picks.

use pokemon_draft_site::db::Db;

/// One coach in an active season, every Pokémon priced at 1 point.
async fn fixture() -> (Db, i64) {
    let db = Db::connect("sqlite::memory:").await.expect("connect");
    sqlx::query(
        "INSERT INTO season (id, name, min_roster, max_roster, is_active)
         VALUES (1, 'S1', 8, 12, 1)",
    )
    .execute(db.pool())
    .await
    .expect("season");
    let person = db.upsert_discord_person("100000000000000000", "ash").await.expect("person");
    db.add_coach(1, person.id, 100, 1).await.expect("coach");
    sqlx::query("INSERT INTO cost (season_id, pokemon_id, points) SELECT 1, id, 1 FROM pokemon")
        .execute(db.pool())
        .await
        .expect("costs");
    let admin = db.upsert_discord_person("100000000000000001", "oak").await.expect("admin");
    (db, admin.id)
}

async fn owned(db: &Db) -> Vec<(i64, i64, Option<i64>)> {
    sqlx::query_as("SELECT pokemon_id, from_week, until_week FROM roster_entry ORDER BY id")
        .fetch_all(db.pool())
        .await
        .expect("entries")
}

#[tokio::test]
async fn picks_and_undos_move_ownership() {
    let (db, admin) = fixture().await;
    let ash = db.coaches(1).await.expect("coaches")[0].id;

    db.make_pick(ash, 3).await.expect("pick");
    db.make_pick(ash, 4).await.expect("pick");
    assert_eq!(owned(&db).await, [(3, 1, None), (4, 1, None)], "drafted = owned from week 1");

    db.undo_pick(ash, 2, admin).await.expect("undo");
    assert_eq!(owned(&db).await, [(3, 1, None)], "undo returns it to the pool");

    // Once a Pokémon has changed hands, undoing its pick would orphan the new
    // owner's entry, so the database refuses.
    sqlx::query("UPDATE roster_entry SET until_week = 3 WHERE pokemon_id = 3")
        .execute(db.pool())
        .await
        .expect("simulate a move");
    db.undo_pick(ash, 1, admin).await.expect_err("moved on");
    assert_eq!(owned(&db).await, [(3, 1, Some(3))], "refused undo changes nothing");
}

#[tokio::test]
async fn a_pokemon_has_one_owner_at_a_time() {
    let (db, _) = fixture().await;
    let ash = db.coaches(1).await.expect("coaches")[0].id;
    db.make_pick(ash, 3).await.expect("pick");

    let second = "INSERT INTO roster_entry (season_id, coach_id, pokemon_id, from_week)
                  VALUES (1, ?, 3, 2)";
    sqlx::query(second).bind(ash).execute(db.pool()).await.expect_err("already owned");

    // Closing the current entry frees it for the next owner.
    sqlx::query("UPDATE roster_entry SET until_week = 2 WHERE pokemon_id = 3")
        .execute(db.pool())
        .await
        .expect("close");
    sqlx::query(second).bind(ash).execute(db.pool()).await.expect("next owner");
}
