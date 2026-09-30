//! Recording picks against a real database: turn enforcement and finishing early.

use pokemon_draft_site::db::Db;
use pokemon_draft_site::draft::{PickError, Standing};
use pokemon_draft_site::picks::DraftError;

/// Two coaches in an active season, priced at 1 point per Pokémon.
///
/// A flat tier list keeps the reserve arithmetic obvious: reaching the 8-pick
/// minimum always costs exactly the slots remaining.
async fn fixture() -> (Db, Vec<i64>) {
    let db = Db::connect("sqlite::memory:").await.expect("connect");
    sqlx::query(
        "INSERT INTO season (id, name, min_roster, max_roster, is_active)
         VALUES (1, 'S1', 8, 12, 1)",
    )
    .execute(db.pool())
    .await
    .expect("season");

    for (i, name) in ["ash", "gary"].iter().enumerate() {
        let discord = format!("10000000000000000{i}");
        let person = db.upsert_discord_person(&discord, name).await.expect("person");
        db.add_coach(1, person.id, 100, i64::try_from(i).expect("fits") + 1).await.expect("coach");
    }

    // Price every seeded Pokémon at 1 point.
    sqlx::query("INSERT INTO cost (season_id, pokemon_id, points) SELECT 1, id, 1 FROM pokemon")
        .execute(db.pool())
        .await
        .expect("costs");

    let coaches = db.coaches(1).await.expect("list").into_iter().map(|c| c.id).collect();
    (db, coaches)
}

/// Pokémon ids from the seed migration, used as arbitrary draftable entries.
const MONS: [i64; 14] = [3, 4, 7, 8, 9, 12, 13, 19, 20, 23, 24, 32, 33, 35];

/// The first `n` draftable ids, for tests that need more than [`MONS`] holds.
async fn draftable_ids(db: &Db, n: i64) -> Vec<i64> {
    sqlx::query_scalar::<_, i64>("SELECT id FROM pokemon ORDER BY id LIMIT ?")
        .bind(n)
        .fetch_all(db.pool())
        .await
        .expect("ids")
}

#[tokio::test]
async fn picking_out_of_turn_is_refused() {
    let (db, coaches) = fixture().await;
    let (ash, gary) = (coaches[0], coaches[1]);

    // Draft position 1 leads, so gary picking first is out of turn.
    let err = db.make_pick(gary, MONS[0]).await.expect_err("not gary's turn");
    assert!(matches!(err, DraftError::Pick(PickError::NotYourTurn)), "got {err:?}");

    db.make_pick(ash, MONS[0]).await.expect("ash leads");

    // Round 2 snakes back, so gary picks twice in a row across the turn.
    db.make_pick(gary, MONS[1]).await.expect("gary ends round 1");
    db.make_pick(gary, MONS[2]).await.expect("gary starts round 2");

    let err = db.make_pick(gary, MONS[3]).await.expect_err("three in a row");
    assert!(matches!(err, DraftError::Pick(PickError::NotYourTurn)), "got {err:?}");
}

#[tokio::test]
async fn a_pokemon_cannot_be_drafted_twice() {
    let (db, coaches) = fixture().await;
    let (ash, gary) = (coaches[0], coaches[1]);
    db.make_pick(ash, MONS[0]).await.expect("ash picks");

    let err = db.make_pick(gary, MONS[0]).await.expect_err("already gone");
    assert!(matches!(err, DraftError::Pick(PickError::Taken)), "got {err:?}");
}

#[tokio::test]
async fn finishing_early_takes_a_coach_out_of_the_order() {
    let (db, coaches) = fixture().await;
    let (ash, gary) = (coaches[0], coaches[1]);

    // Eight picks each, alternating with the snake. Each round consumes two
    // fresh entries so nothing is drafted twice.
    let mons = draftable_ids(&db, 16).await;
    for round in 0..8 {
        let (first, second) = if round % 2 == 0 { (ash, gary) } else { (gary, ash) };
        db.make_pick(first, mons[round * 2]).await.expect("first of the round");
        db.make_pick(second, mons[round * 2 + 1]).await.expect("second of the round");
    }

    let board = db.board().await.expect("board");
    assert_eq!(board.seat_of(ash).expect("seat").picks, 8);

    db.finish_drafting(ash).await.expect("8 is enough to stop");

    // Ash is out; gary is now alone on the clock however the snake turns.
    let board = db.board().await.expect("board");
    let ash_seat = board.seat_of(ash).expect("seat");
    assert_eq!(board.standing(ash_seat), Standing::Done);
    assert_eq!(board.turn.coach_id, Some(gary), "the order compressed to gary");

    let err = db.make_pick(ash, MONS[10]).await.expect_err("ash is finished");
    assert!(matches!(err, DraftError::Pick(PickError::NotYourTurn)), "got {err:?}");

    // The admin correction puts them back in.
    db.reopen_drafting(ash).await.expect("reopen");
    let board = db.board().await.expect("board");
    assert_eq!(board.standing(board.seat_of(ash).expect("seat")), Standing::Active);
}

#[tokio::test]
async fn the_reserve_rule_blocks_a_pick_that_strands_the_minimum() {
    let db = Db::connect("sqlite::memory:").await.expect("connect");
    sqlx::query(
        "INSERT INTO season (id, name, min_roster, max_roster, is_active)
         VALUES (1, 'S1', 8, 12, 1)",
    )
    .execute(db.pool())
    .await
    .expect("season");
    let person = db.upsert_discord_person("100000000000000001", "ash").await.expect("person");
    // 10 points for 8 Pokémon: two points of slack over the cheap tier.
    db.add_coach(1, person.id, 10, 1).await.expect("coach");
    let ash = db.coaches(1).await.expect("list")[0].id;

    sqlx::query("INSERT INTO cost (season_id, pokemon_id, points) SELECT 1, id, 1 FROM pokemon")
        .execute(db.pool())
        .await
        .expect("cheap costs");
    // One expensive entry: affordable within the cap, but not once the seven
    // slots behind it are reserved for.
    sqlx::query("UPDATE cost SET points = 4 WHERE pokemon_id = ? AND season_id = 1")
        .bind(MONS[0])
        .execute(db.pool())
        .await
        .expect("reprice");

    // 10 points, 8 slots: taking the 4 leaves 6 for 7 slots at 1 each. The cap
    // rule alone would allow this; the reserve rule is what refuses it.
    let err = db.make_pick(ash, MONS[0]).await.expect_err("strands the minimum");
    assert!(
        matches!(err, DraftError::Pick(PickError::BreaksReserve { reserve: 7, left: 6, .. })),
        "got {err:?}"
    );

    // Cheap picks do not help: every one spends a point and frees only a
    // 1-point slot, so the 4 stays out of reach on this budget.
    db.make_pick(ash, MONS[1]).await.expect("cheap pick");
    let err = db.make_pick(ash, MONS[0]).await.expect_err("still strands it");
    assert!(
        matches!(err, DraftError::Pick(PickError::BreaksReserve { reserve: 6, left: 5, .. })),
        "got {err:?}"
    );

    // Filling the roster on the cheap tier is always possible, which is the
    // guarantee the reserve rule exists to provide.
    let mons = draftable_ids(&db, 10).await;
    for m in mons.iter().filter(|m| **m != MONS[0] && **m != MONS[1]).take(7) {
        db.make_pick(ash, *m).await.expect("cheap pick");
    }
    let board = db.board().await.expect("board");
    let seat = board.seat_of(ash).expect("seat");
    assert_eq!(seat.picks, 8, "reached the minimum");
    assert_eq!(seat.remaining, 2, "with points to spare");
    // Past the minimum the reserve is gone, so the cap is the only limit left.
    let err = db.make_pick(ash, MONS[0]).await.expect_err("2 points cannot buy a 4");
    assert!(
        matches!(err, DraftError::Pick(PickError::OverBudget { cost: 4, remaining: 2 })),
        "got {err:?}"
    );
}

#[tokio::test]
async fn queue_counts_are_public_without_exposing_contents() {
    let (db, coaches) = fixture().await;
    let ash = coaches[0];
    sqlx::query("INSERT INTO queue_slot (coach_id, slot_number, pokemon_id) VALUES (?, 1, ?)")
        .bind(ash)
        .bind(MONS[5])
        .execute(db.pool())
        .await
        .expect("queue");

    let counts = db.queue_counts(1).await.expect("counts");
    assert_eq!(counts.iter().find(|(id, _)| *id == ash).expect("ash").1, 1);

    // Drafting a queued Pokémon breaks the binding rather than promoting the
    // next slot, so the count drops.
    db.make_pick(ash, MONS[5]).await.expect("ash drafts their own queued pick");
    let counts = db.queue_counts(1).await.expect("counts");
    assert_eq!(counts.iter().find(|(id, _)| *id == ash).expect("ash").1, 0);
}

#[tokio::test]
async fn undoing_the_latest_pick_refunds_it_and_leaves_a_trail() {
    let (db, coaches) = fixture().await;
    let (ash, gary) = (coaches[0], coaches[1]);
    let admin = db.coaches(1).await.expect("list")[0].person_id;

    // Same eight rounds as above; the snake makes ash's 8th the last pick.
    let mons = draftable_ids(&db, 16).await;
    for round in 0..8 {
        let (first, second) = if round % 2 == 0 { (ash, gary) } else { (gary, ash) };
        db.make_pick(first, mons[round * 2]).await.expect("first of the round");
        db.make_pick(second, mons[round * 2 + 1]).await.expect("second of the round");
    }
    db.finish_drafting(ash).await.expect("ash stops at 8");

    // Only the latest pick can go, so a stale page cannot undo a second one.
    let err = db.undo_pick(gary, 8, admin).await.expect_err("not the latest");
    assert!(matches!(err, DraftError::NotLatestPick), "got {err:?}");

    db.undo_pick(ash, 8, admin).await.expect("undo ash's 8th");
    let err = db.undo_pick(ash, 8, admin).await.expect_err("already undone");
    assert!(matches!(err, DraftError::NotLatestPick), "got {err:?}");

    // Points back, below the minimum so no longer done, and back on the clock.
    let board = db.board().await.expect("board");
    let seat = board.seat_of(ash).expect("seat");
    assert_eq!((seat.picks, seat.remaining, seat.done), (7, 93, false));
    assert_eq!(board.turn.coach_id, Some(ash));

    // The Pokémon is back in the pool.
    db.make_pick(ash, mons[15]).await.expect("redraft the undone Pokémon");

    let trail: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pick_correction")
        .fetch_one(db.pool())
        .await
        .expect("trail");
    assert_eq!(trail, 1);
}
