//! Playoff seeding follows the standings and refuses to move a played match.

use pokemon_draft_site::db::{Db, Person};
use pokemon_draft_site::results::{Entry, ResultError, Score};

struct Season {
    db: Db,
    by: Person,
    /// Coach ids in draft order.
    c: [i64; 4],
    /// Regular-season match id for each pairing, keyed by (lower, higher) index into `c`.
    games: Vec<((usize, usize), i64)>,
    semis: [i64; 2],
    last: i64,
}

impl Season {
    fn game(&self, x: usize, y: usize) -> i64 {
        self.games.iter().find(|(k, _)| *k == (x, y)).expect("pairing").1
    }

    async fn win(&self, match_id: i64, winner: i64) -> Result<(), ResultError> {
        self.db.set_result(match_id, &self.by, Some(Entry { winner, score: Score::Differential(2) })).await
    }

    async fn teams(&self, match_id: i64) -> (Option<i64>, Option<i64>) {
        let m = self.db.match_by_id(match_id).await.expect("read").expect("exists");
        (m.coach_a, m.coach_b)
    }
}

async fn insert(db: &Db, season: i64, week: i64, a: Option<i64>, b: Option<i64>) -> i64 {
    let playoff = a.is_none();
    sqlx::query_scalar!(
        "INSERT INTO match (season_id, week, is_playoff, coach_a_id, coach_b_id) VALUES (?, ?, ?, ?, ?)
         RETURNING id AS \"id!\"",
        season,
        week,
        playoff,
        a,
        b
    )
    .fetch_one(db.pool())
    .await
    .expect("match")
}

/// Four coaches, a three-week round robin, semifinals in week 4, final in week 5.
async fn season() -> Season {
    let db = Db::connect("sqlite::memory:").await.expect("connect");
    let season = sqlx::query_scalar!(
        "INSERT INTO season (name, min_roster, max_roster, is_active) VALUES ('S1', 8, 12, 1) RETURNING id"
    )
    .fetch_one(db.pool())
    .await
    .expect("season");
    for n in 1..=4u8 {
        let id = db.person_by_hand(&format!("1000000000000000{n:02}"), &format!("p{n}")).await.expect("person");
        db.add_coach(season, id, 100, i64::from(n)).await.expect("coach");
    }
    let by = db.person(1).await.expect("read").expect("exists");
    let ids: Vec<i64> = db.coaches(season).await.expect("coaches").iter().map(|c| c.id).collect();
    let c = [ids[0], ids[1], ids[2], ids[3]];

    let mut games = Vec::new();
    for (week, pairs) in [(1, [(0, 1), (2, 3)]), (2, [(0, 2), (1, 3)]), (3, [(0, 3), (1, 2)])] {
        for (x, y) in pairs {
            games.push(((x, y), insert(&db, season, week, Some(c[x]), Some(c[y])).await));
        }
    }
    let semis = [insert(&db, season, 4, None, None).await, insert(&db, season, 4, None, None).await];
    let last = insert(&db, season, 5, None, None).await;
    Season { db, by, c, games, semis, last }
}

#[tokio::test]
async fn seeding_tracks_results_until_a_playoff_match_is_played() {
    let s = season().await;
    let [c1, c2, c3, c4] = s.c;

    // The lower draft position wins every game except the last one entered.
    for &((x, y), id) in &s.games[..5] {
        s.win(id, s.c[x.min(y)]).await.expect("regular result");
    }
    assert_eq!(s.teams(s.semis[0]).await, (None, None), "no seeding until the season is done");

    s.win(s.game(1, 2), c2).await.expect("last regular result");
    assert_eq!(s.teams(s.semis[0]).await, (Some(c1), Some(c4)), "1 v 4");
    assert_eq!(s.teams(s.semis[1]).await, (Some(c2), Some(c3)), "2 v 3");

    // Before any semifinal is played, an override re-seeds: 4 now beats 3.
    s.win(s.game(2, 3), c4).await.expect("override");
    assert_eq!(s.teams(s.semis[0]).await, (Some(c1), Some(c3)));
    assert_eq!(s.teams(s.semis[1]).await, (Some(c2), Some(c4)));

    s.win(s.semis[0], c1).await.expect("semi 1");
    assert_eq!(s.teams(s.last).await, (None, None), "the final waits for both semifinals");
    s.win(s.semis[1], c4).await.expect("semi 2");
    assert_eq!(s.teams(s.last).await, (Some(c1), Some(c4)));

    // Now a seeding-changing override is refused and rolled back.
    let err = s.win(s.game(2, 3), c3).await;
    assert!(matches!(err, Err(ResultError::Reseed("a semifinal"))), "got: {err:?}");
    let kept = s.db.match_by_id(s.game(2, 3)).await.expect("read").expect("exists");
    assert_eq!(kept.winner, Some(c4), "refused write left the old result");

    // An override that keeps the seeding is still fine.
    s.db.set_result(s.game(2, 3), &s.by, Some(Entry { winner: c4, score: Score::Forfeit }))
        .await
        .expect("same winner, new differential");

    s.win(s.last, c1).await.expect("final");
    let err = s.win(s.semis[1], c2).await;
    assert!(matches!(err, Err(ResultError::Reseed("the final"))), "got: {err:?}");
}
