//! Free agency against a real database: timing, budget, limits, deadline.

use pokemon_draft_site::db::Db;
use pokemon_draft_site::draft::Roster;
use pokemon_draft_site::moves::{MAX_MOVES, MoveError, check_side};

/// Coach budget: 8 one-point picks leave 12 to play with.
const BUDGET: i64 = 20;

struct League {
    db: Db,
    ash: i64,
    gary: i64,
    /// Each coach's 8 drafted Pokémon.
    ash_mons: Vec<i64>,
    gary_mons: Vec<i64>,
    /// Undrafted Pokémon, all 1 point.
    free: Vec<i64>,
}

/// Two coaches drafted to 8 each and done, with ash v gary in weeks 1-3.
async fn league() -> League {
    let db = Db::connect("sqlite::memory:").await.expect("connect");
    sqlx::query(
        "INSERT INTO season (id, name, min_roster, max_roster, is_active)
         VALUES (1, 'S1', 8, 12, 1)",
    )
    .execute(db.pool())
    .await
    .expect("season");
    for (i, name) in ["ash", "gary"].iter().enumerate() {
        let person = db.upsert_discord_person(&format!("10000000000000000{i}"), name).await.expect("person");
        db.add_coach(1, person.id, BUDGET, i64::try_from(i).expect("fits") + 1).await.expect("coach");
    }
    sqlx::query("INSERT INTO cost (season_id, pokemon_id, points) SELECT 1, id, 1 FROM pokemon")
        .execute(db.pool())
        .await
        .expect("costs");
    let coaches: Vec<i64> = db.coaches(1).await.expect("coaches").iter().map(|c| c.id).collect();
    let (ash, gary) = (coaches[0], coaches[1]);

    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM pokemon ORDER BY id LIMIT 24")
        .fetch_all(db.pool())
        .await
        .expect("ids");
    let (mut ash_mons, mut gary_mons) = (Vec::new(), Vec::new());
    for round in 0..8 {
        let (a, g) = (ids[round * 2], ids[round * 2 + 1]);
        if round % 2 == 0 {
            db.make_pick(ash, a).await.expect("ash");
            db.make_pick(gary, g).await.expect("gary");
        } else {
            db.make_pick(gary, g).await.expect("gary");
            db.make_pick(ash, a).await.expect("ash");
        }
        ash_mons.push(a);
        gary_mons.push(g);
    }
    db.finish_drafting(ash).await.expect("ash done");
    db.finish_drafting(gary).await.expect("gary done");

    for week in 1..=3 {
        sqlx::query("INSERT INTO match (season_id, week, coach_a_id, coach_b_id) VALUES (1, ?, ?, ?)")
            .bind(week)
            .bind(ash)
            .bind(gary)
            .execute(db.pool())
            .await
            .expect("match");
    }
    League { db, ash, gary, ash_mons, gary_mons, free: ids[16..].to_vec() }
}

async fn play_week(l: &League, week: i64) {
    sqlx::query(
        "UPDATE match SET winner_coach_id = ?, differential = 1, result_source = 'manual'
         WHERE week = ?",
    )
    .bind(l.ash)
    .bind(week)
    .execute(l.db.pool())
    .await
    .expect("result");
}

async fn price(l: &League, pokemon: i64, points: i64) {
    sqlx::query("UPDATE cost SET points = ? WHERE pokemon_id = ?")
        .bind(points)
        .bind(pokemon)
        .execute(l.db.pool())
        .await
        .expect("price");
}

#[tokio::test]
async fn a_swap_is_pending_until_the_week_is_played() {
    let l = league().await;
    let (drop, pickup) = (l.ash_mons[0], l.free[0]);
    l.db.free_agency(l.ash, drop, pickup).await.expect("swap");

    let roster = l.db.roster(l.ash).await.expect("roster");
    let find = |id| roster.iter().find(|o| o.pokemon_id == id).expect("listed");
    assert_eq!(find(drop).leaves, Some(2), "still plays week 1, leaving");
    assert_eq!(find(pickup).joins, Some(2), "arrives for week 2");
    assert_eq!(roster.len(), 9, "both shown while pending");

    play_week(&l, 1).await;
    let roster = l.db.roster(l.ash).await.expect("roster");
    assert_eq!(roster.len(), 8, "the dropped one is gone");
    assert!(roster.iter().all(|o| o.joins.is_none() && o.leaves.is_none()), "nothing pending");
    assert!(roster.iter().any(|o| o.pokemon_id == pickup));

    // A pending arrival can't be dropped again the same week.
    let l2 = league().await;
    l2.db.free_agency(l2.ash, l2.ash_mons[0], l2.free[0]).await.expect("swap");
    let err = l2.db.free_agency(l2.ash, l2.free[0], l2.free[1]).await.expect_err("not here yet");
    assert!(matches!(err, MoveError::StillPending), "got {err:?}");
}

#[tokio::test]
async fn free_agency_checks_ownership_and_budget() {
    let l = league().await;

    let err = l.db.free_agency(l.ash, l.gary_mons[0], l.free[0]).await.expect_err("not ash's");
    assert!(matches!(err, MoveError::NotOwned), "got {err:?}");
    let err = l.db.free_agency(l.ash, l.ash_mons[0], l.gary_mons[0]).await.expect_err("gary's");
    assert!(matches!(err, MoveError::Unavailable), "got {err:?}");

    // 8 spent - 1 dropped + 14 = 21, one over the budget of 20.
    price(&l, l.free[0], 14).await;
    let err = l.db.free_agency(l.ash, l.ash_mons[0], l.free[0]).await.expect_err("over");
    assert!(matches!(err, MoveError::OverBudget { cost: 21, budget: 20, .. }), "got {err:?}");

    // Exactly at the budget is fine.
    price(&l, l.free[0], 13).await;
    l.db.free_agency(l.ash, l.ash_mons[0], l.free[0]).await.expect("at the cap");
    let spent = l.db.coaches(1).await.expect("coaches")[0].spent;
    assert_eq!(spent, BUDGET, "spent counts the pending pickup");
}

#[tokio::test]
async fn moves_are_limited_and_close_at_the_deadline() {
    let l = league().await;
    for i in 0..MAX_MOVES {
        let i = usize::try_from(i).expect("small");
        l.db.free_agency(l.gary, l.gary_mons[i], l.free[i]).await.expect("within the limit");
    }
    let err = l.db.free_agency(l.gary, l.gary_mons[5], l.free[5]).await.expect_err("limit");
    assert!(matches!(err, MoveError::NoMovesLeft { .. }), "got {err:?}");

    // In the last week, a move would play after the season.
    play_week(&l, 1).await;
    play_week(&l, 2).await;
    let err = l.db.free_agency(l.ash, l.ash_mons[0], l.free[6]).await.expect_err("deadline");
    assert!(matches!(err, MoveError::PastDeadline { week: 4, last: 3 }), "got {err:?}");
}

#[tokio::test]
async fn no_moves_while_the_draft_runs() {
    let db = Db::connect("sqlite::memory:").await.expect("connect");
    sqlx::query("INSERT INTO season (id, name, min_roster, max_roster, is_active) VALUES (1, 'S1', 8, 12, 1)")
        .execute(db.pool())
        .await
        .expect("season");
    let person = db.upsert_discord_person("100000000000000000", "ash").await.expect("person");
    db.add_coach(1, person.id, BUDGET, 1).await.expect("coach");
    sqlx::query("INSERT INTO cost (season_id, pokemon_id, points) SELECT 1, id, 1 FROM pokemon")
        .execute(db.pool())
        .await
        .expect("costs");
    let ash = db.coaches(1).await.expect("coaches")[0].id;
    db.make_pick(ash, 3).await.expect("pick");

    let err = db.free_agency(ash, 3, 4).await.expect_err("draft running");
    assert!(matches!(err, MoveError::DraftRunning), "got {err:?}");
}

#[test]
fn a_side_must_stay_in_range_and_under_budget() {
    let roster = Roster::new(8, 12).expect("range");
    assert!(check_side("ash", 8, 20, 20, roster).is_ok(), "at the minimum and the cap");
    assert!(check_side("ash", 12, 5, 20, roster).is_ok(), "at the maximum");
    assert!(matches!(check_side("ash", 7, 5, 20, roster), Err(MoveError::Size { size: 7, .. })));
    assert!(matches!(check_side("ash", 13, 5, 20, roster), Err(MoveError::Size { size: 13, .. })));
    assert!(matches!(check_side("ash", 8, 21, 20, roster), Err(MoveError::OverBudget { .. })));
}

async fn offer_status(l: &League, offer: i64) -> (String, Option<String>) {
    sqlx::query_as("SELECT status, reason FROM trade_offer WHERE id = ?")
        .bind(offer)
        .fetch_one(l.db.pool())
        .await
        .expect("offer")
}

#[tokio::test]
async fn an_accepted_trade_swaps_rosters_from_next_week() {
    let l = league().await;
    let (a, g) = (l.ash_mons[0], l.gary_mons[0]);
    l.db.propose_trade(l.ash, l.gary, &[a], &[g]).await.expect("offer 1");
    // A second offer for the same Pokémon dies when the first goes through.
    l.db.propose_trade(l.ash, l.gary, &[a], &[l.gary_mons[1]]).await.expect("offer 2");

    let err = l.db.accept_trade(1, l.ash).await.expect_err("only gary can accept");
    assert!(matches!(err, MoveError::NoOffer), "got {err:?}");
    l.db.accept_trade(1, l.gary).await.expect("accept");

    let ash = l.db.roster(l.ash).await.expect("roster");
    let find = |id| ash.iter().find(|o| o.pokemon_id == id).expect("listed");
    assert_eq!((find(a).leaves, find(g).joins), (Some(2), Some(2)), "pending until week 1 is played");
    assert_eq!(l.db.moves_used(l.ash).await.expect("used"), 1);
    assert_eq!(l.db.moves_used(l.gary).await.expect("used"), 1, "a trade costs both sides");
    assert_eq!(offer_status(&l, 1).await.0, "accepted");
    assert_eq!(offer_status(&l, 2).await.0, "declined", "its Pokémon has moved");

    let log = l.db.move_log(1).await.expect("log");
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].items.len(), 2, "one arrival on each side");
}

#[tokio::test]
async fn a_trade_that_breaks_a_rule_is_declined_on_accept() {
    let l = league().await;
    // 1-for-2 would leave gary at 7, under the minimum.
    l.db.propose_trade(l.ash, l.gary, &[l.ash_mons[0]], &[l.gary_mons[0], l.gary_mons[1]])
        .await
        .expect("offer");
    let err = l.db.accept_trade(1, l.gary).await.expect_err("gary too small");
    assert!(matches!(&err, MoveError::Size { who, size: 7, .. } if who == "gary"), "got {err:?}");

    let (status, reason) = offer_status(&l, 1).await;
    assert_eq!(status, "declined");
    assert_eq!(reason, Some(err.to_string()), "the coaches see why");
    assert!(l.db.move_log(1).await.expect("log").is_empty(), "nothing moved");
}

#[tokio::test]
async fn a_free_agency_drop_shows_in_the_log_and_kills_offers() {
    let l = league().await;
    l.db.propose_trade(l.gary, l.ash, &[l.gary_mons[0]], &[l.ash_mons[0]]).await.expect("offer");
    l.db.free_agency(l.ash, l.ash_mons[0], l.free[0]).await.expect("swap");
    assert_eq!(offer_status(&l, 1).await.0, "declined", "ash no longer has what gary wanted");

    let log = l.db.move_log(1).await.expect("log");
    let dropped: Vec<bool> = log[0].items.iter().map(|i| i.dropped).collect();
    assert_eq!(dropped.len(), 2);
    assert!(dropped.contains(&true) && dropped.contains(&false), "pickup and drop");
}
