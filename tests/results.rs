//! Match result bounds, entry, and the edit trail.

use pokemon_draft_site::db::{Db, Person};
use pokemon_draft_site::results::{
    Entry, ResultError, Score, differential_range, forfeit_differential,
};

#[test]
fn differential_bounds_follow_best_of() {
    // Bo1: win with 1 to 4 left.
    assert_eq!(differential_range(1), 1..=4);
    // Bo3: a 2-0 sweep tops out at 8; a 2-1 winning twice by 1 and losing by 4 bottoms at -2.
    assert_eq!(differential_range(3), -2..=8);
    assert_eq!(forfeit_differential(3), 8);
    assert_eq!(forfeit_differential(1), 4);
}

struct League {
    db: Db,
    /// One scheduled match between `a` and `b`.
    game: i64,
    /// An unseeded playoff match.
    playoff: i64,
    a: i64,
    b: i64,
    coach: Person,
    outsider: Person,
}

async fn person(db: &Db, n: u8) -> Person {
    let id = db.person_by_hand(&format!("1000000000000000{n:02}"), &format!("p{n}")).await.expect("person");
    db.person(id).await.expect("read").expect("exists")
}

async fn league() -> League {
    let db = Db::connect("sqlite::memory:").await.expect("connect");
    let season = sqlx::query_scalar!(
        "INSERT INTO season (name, min_roster, max_roster, is_active) VALUES ('S1', 8, 12, 1) RETURNING id"
    )
    .fetch_one(db.pool())
    .await
    .expect("season");
    let (coach, other, outsider) = (person(&db, 1).await, person(&db, 2).await, person(&db, 3).await);
    db.add_coach(season, coach.id, 100, 1).await.expect("a");
    db.add_coach(season, other.id, 100, 2).await.expect("b");
    let ids: Vec<i64> = db.coaches(season).await.expect("coaches").iter().map(|c| c.id).collect();
    let (a, b) = (ids[0], ids[1]);
    let game = sqlx::query_scalar!(
        "INSERT INTO match (season_id, week, coach_a_id, coach_b_id) VALUES (?, 1, ?, ?) RETURNING id AS \"id!\"",
        season,
        a,
        b
    )
    .fetch_one(db.pool())
    .await
    .expect("match");
    let playoff = sqlx::query_scalar!(
        "INSERT INTO match (season_id, week, is_playoff) VALUES (?, 12, 1) RETURNING id AS \"id!\"",
        season
    )
    .fetch_one(db.pool())
    .await
    .expect("playoff");
    League { db, game, playoff, a, b, coach, outsider }
}

#[tokio::test]
async fn refuses_what_cannot_be_a_result() {
    let l = league().await;
    let win = |d| Some(Entry { winner: l.a, score: Score::Differential(d) });

    let err = l.db.set_result(l.game, &l.outsider, win(3)).await;
    assert!(matches!(err, Err(ResultError::NotACoach)), "got: {err:?}");

    let err = l.db.set_result(l.game, &l.coach, win(9)).await;
    assert!(matches!(err, Err(ResultError::Differential { got: 9, min: -2, max: 8, .. })), "got: {err:?}");

    let err = l.db.set_result(l.game, &l.coach, Some(Entry { winner: 999, score: Score::Forfeit })).await;
    assert!(matches!(err, Err(ResultError::NotInMatch)), "got: {err:?}");

    let err = l.db.set_result(l.playoff, &l.coach, win(3)).await;
    assert!(matches!(err, Err(ResultError::Unseeded)), "got: {err:?}");

    assert!(l.db.result_history(l.game).await.expect("history").is_empty(), "refusals write nothing");
}

#[tokio::test]
async fn every_write_is_logged_including_a_clear() {
    let l = league().await;
    l.db.set_result(l.game, &l.coach, Some(Entry { winner: l.a, score: Score::Differential(3) }))
        .await
        .expect("set");
    l.db.set_result(l.game, &l.coach, Some(Entry { winner: l.b, score: Score::Forfeit })).await.expect("forfeit");

    let m = l.db.match_by_id(l.game).await.expect("read").expect("exists");
    assert_eq!((m.winner, m.differential, m.is_forfeit), (Some(l.b), Some(8), true), "Bo3 forfeit is +8");

    l.db.set_result(l.game, &l.coach, None).await.expect("clear");
    let m = l.db.match_by_id(l.game).await.expect("read").expect("exists");
    assert_eq!((m.winner, m.differential, m.is_forfeit), (None, None, false));

    let trail: Vec<(String, String)> =
        l.db.result_history(l.game).await.expect("history").iter().map(|e| (e.before(), e.after())).collect();
    assert_eq!(
        trail,
        vec![
            ("no result".to_owned(), "p1 +3".to_owned()),
            ("p1 +3".to_owned(), "p2 by forfeit (+8)".to_owned()),
            ("p2 by forfeit (+8)".to_owned(), "no result".to_owned()),
        ]
    );
}

#[tokio::test]
async fn an_admin_outside_the_season_may_still_correct() {
    let l = league().await;
    l.db.set_member_fields(l.outsider.id, None, true).await.expect("promote");
    let admin = l.db.person(l.outsider.id).await.expect("read").expect("exists");
    l.db.set_result(l.game, &admin, Some(Entry { winner: l.a, score: Score::Differential(-2) }))
        .await
        .expect("admin may enter a result");
}
