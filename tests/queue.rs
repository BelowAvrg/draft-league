//! Positional queue slots: auto-pick, sniping, and who may see what.

use pokemon_draft_site::db::Db;
use pokemon_draft_site::draft::PickError;
use pokemon_draft_site::picks::{DraftError, QueueError};

/// Two coaches in an active season, priced at 1 point per Pokémon.
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

    sqlx::query("INSERT INTO cost (season_id, pokemon_id, points) SELECT 1, id, 1 FROM pokemon")
        .execute(db.pool())
        .await
        .expect("costs");

    let coaches = db.coaches(1).await.expect("list").into_iter().map(|c| c.id).collect();
    (db, coaches)
}

/// Pokémon ids from the seed migration, used as arbitrary draftable entries.
const MONS: [i64; 8] = [3, 4, 7, 8, 9, 12, 13, 19];

/// The slugs of [`MONS`], for asserting which entry a pick landed on.
const SLUGS: [&str; 8] = [
    "venusaur",
    "venusaur-mega",
    "charizard",
    "charizard-mega-x",
    "charizard-mega-y",
    "blastoise",
    "blastoise-mega",
    "beedrill",
];

/// Whoever the draft is waiting on.
async fn on_the_clock(db: &Db) -> Option<i64> {
    db.board().await.expect("board").turn.coach_id
}

#[tokio::test]
async fn a_queued_slot_picks_itself_when_the_draft_reaches_it() {
    let (db, coaches) = fixture().await;
    let (ash, gary) = (coaches[0], coaches[1]);

    // Gary queues his first pick while ash is still on the clock.
    db.set_queue_slot(gary, 1, MONS[1]).await.expect("queue slot 1");
    assert_eq!(on_the_clock(&db).await, Some(ash), "queuing does not take a turn");

    // Ash picks; the draft passes to gary, whose slot 1 resolves on its own.
    db.make_pick(ash, MONS[0]).await.expect("ash picks");

    let roster = db.roster_of(gary).await.expect("roster");
    assert_eq!(roster.len(), 1, "gary's queued pick landed without him acting");
    assert_eq!(roster[0].slug, SLUGS[1]);

    // Round 2 snakes back to gary, who has nothing queued for slot 2.
    assert_eq!(on_the_clock(&db).await, Some(gary), "the draft stops at the empty slot");
    assert!(db.queue_of(gary).await.expect("queue").is_empty(), "the spent slot is gone");
}

#[tokio::test]
async fn a_sniped_slot_stalls_rather_than_promoting_the_next() {
    let (db, coaches) = fixture().await;
    let (ash, gary) = (coaches[0], coaches[1]);

    // Gary queues two picks. Ash takes the one in slot 1 out from under him.
    db.set_queue_slot(gary, 1, MONS[0]).await.expect("slot 1");
    db.set_queue_slot(gary, 2, MONS[1]).await.expect("slot 2");
    db.make_pick(ash, MONS[0]).await.expect("ash snipes gary's slot 1");

    // Slot 1 is emptied by the snipe; slot 2 is NOT promoted into its place.
    let queue = db.queue_of(gary).await.expect("queue");
    assert_eq!(queue.len(), 1, "only the sniped slot is emptied");
    assert_eq!(queue[0].slot_number, 2, "slot 2 stays bound to pick 2");

    // The draft waits for gary to pick his first manually.
    assert_eq!(on_the_clock(&db).await, Some(gary), "a sniped slot stalls the draft");
    assert!(db.roster_of(gary).await.expect("roster").is_empty(), "nothing auto-picked");

    // Once he picks by hand, slot 2 is now his pick 2 -- and round 2 snakes
    // straight back to him, so it fires immediately.
    db.make_pick(gary, MONS[2]).await.expect("gary picks manually");
    let roster = db.roster_of(gary).await.expect("roster");
    assert_eq!(roster.len(), 2, "slot 2 fired once the draft reached pick 2");
    assert_eq!(roster[1].slug, SLUGS[1]);
}

#[tokio::test]
async fn auto_picks_cascade_across_coaches() {
    let (db, coaches) = fixture().await;
    let (ash, gary) = (coaches[0], coaches[1]);

    // Gary queues picks 1 and 2. Round 2 snakes back to him, so both should
    // fire off a single manual pick by ash.
    db.set_queue_slot(gary, 1, MONS[1]).await.expect("slot 1");
    db.set_queue_slot(gary, 2, MONS[2]).await.expect("slot 2");

    db.make_pick(ash, MONS[0]).await.expect("ash picks");

    assert_eq!(db.roster_of(gary).await.expect("roster").len(), 2, "both slots cascaded");
    assert_eq!(on_the_clock(&db).await, Some(ash), "the cascade stops at ash's empty queue");
}

#[tokio::test]
async fn queuing_the_slot_you_are_on_picks_immediately() {
    let (db, coaches) = fixture().await;
    let ash = coaches[0];

    // Ash is on the clock for pick 1, so queuing slot 1 resolves at once
    // rather than waiting for some later event to notice it.
    db.set_queue_slot(ash, 1, MONS[0]).await.expect("queue the live slot");
    assert_eq!(db.roster_of(ash).await.expect("roster").len(), 1, "picked on queue");
}

#[tokio::test]
async fn passed_slots_cannot_be_queued_but_future_ones_can() {
    let (db, coaches) = fixture().await;
    let (ash, gary) = (coaches[0], coaches[1]);

    db.make_pick(ash, MONS[0]).await.expect("ash picks");

    // Ash has one pick, so slot 1 is dead and slot 2 is his next.
    let err = db.set_queue_slot(ash, 1, MONS[1]).await.expect_err("slot 1 has passed");
    assert!(
        matches!(err, DraftError::Queue(QueueError::SlotPassed { slot: 1, next: 2 })),
        "got {err:?}"
    );

    // The next three picks are all queueable -- the point of the feature.
    for (slot, mon) in [(2, MONS[1]), (3, MONS[2]), (4, MONS[3])] {
        db.set_queue_slot(ash, slot, mon).await.expect("queue a future pick");
    }
    assert_eq!(db.queue_of(ash).await.expect("queue").len(), 3);

    // Past the roster maximum there is no pick to bind to.
    let err = db.set_queue_slot(gary, 13, MONS[4]).await.expect_err("13 is past the maximum");
    assert!(
        matches!(err, DraftError::Queue(QueueError::PastMaximum { slot: 13, max: 12 })),
        "got {err:?}"
    );
}

#[tokio::test]
async fn a_taken_pokemon_cannot_be_queued_and_duplicates_are_refused() {
    let (db, coaches) = fixture().await;
    let (ash, gary) = (coaches[0], coaches[1]);

    db.make_pick(ash, MONS[0]).await.expect("ash picks");

    let err = db.set_queue_slot(gary, 1, MONS[0]).await.expect_err("already drafted");
    assert!(matches!(err, DraftError::Pick(PickError::Taken)), "got {err:?}");

    // The same Pokémon in two slots guarantees one of them is dead on arrival.
    db.set_queue_slot(gary, 2, MONS[1]).await.expect("slot 2");
    let err = db.set_queue_slot(gary, 3, MONS[1]).await.expect_err("queued twice");
    assert!(
        matches!(err, DraftError::Queue(QueueError::AlreadyQueued { slot: 2 })),
        "got {err:?}"
    );

    // Re-setting the slot it already occupies is just a no-op overwrite.
    db.set_queue_slot(gary, 2, MONS[1]).await.expect("same slot, same mon");
}

#[tokio::test]
async fn counts_are_public_and_contents_are_not() {
    let (db, coaches) = fixture().await;
    let (ash, gary) = (coaches[0], coaches[1]);

    db.set_queue_slot(gary, 1, MONS[1]).await.expect("slot 1");
    db.set_queue_slot(gary, 2, MONS[2]).await.expect("slot 2");

    // The count is the public half of the split.
    let counts = db.queue_counts(1).await.expect("counts");
    assert_eq!(counts.iter().find(|(id, _)| *id == gary).expect("gary").1, 2);
    assert_eq!(counts.iter().find(|(id, _)| *id == ash).expect("ash").1, 0);

    // The contents come only from the owner-keyed read. There is no call that
    // takes a viewer and returns someone else's slots -- not even for admins.
    let mine = db.queue_of(gary).await.expect("gary's own queue");
    assert_eq!(mine.len(), 2);
    assert!(db.queue_of(ash).await.expect("ash's queue").is_empty());
}

#[tokio::test]
async fn a_slot_that_breaks_the_reserve_stalls_instead_of_picking() {
    let (db, coaches) = fixture().await;
    let (ash, gary) = (coaches[0], coaches[1]);

    // Price one Pokémon out of gary's reach: a 95-point mon against a budget
    // of 100 leaves 5, short of the 7 needed to reach the 8-pick minimum.
    sqlx::query("UPDATE cost SET points = 95 WHERE season_id = 1 AND pokemon_id = ?")
        .bind(MONS[1])
        .execute(db.pool())
        .await
        .expect("reprice");

    db.set_queue_slot(gary, 1, MONS[1]).await.expect("queuing does not check budget");
    db.make_pick(ash, MONS[0]).await.expect("ash picks");

    // The slot is unaffordable under the reserve rule, so the draft stops for
    // gary rather than spending his points or silently dropping the slot.
    assert!(db.roster_of(gary).await.expect("roster").is_empty(), "no auto-pick");
    assert_eq!(on_the_clock(&db).await, Some(gary), "gary is left to decide");
    assert_eq!(db.queue_of(gary).await.expect("queue").len(), 1, "the slot stays put");
}
