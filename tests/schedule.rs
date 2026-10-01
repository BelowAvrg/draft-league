//! Schedule parsing and import, and setting match times.

use std::collections::HashMap;

use pokemon_draft_site::db::Db;
use pokemon_draft_site::results::ResultError;
use pokemon_draft_site::schedule::{ImportError, parse};
use time::OffsetDateTime;

const HEADER: &str = r#""Team_One_ID","Team_One_Name","Team_Two_ID","Team_Two_Name","Week""#;

fn export(rows: &[&str]) -> String {
    std::iter::once(HEADER).chain(rows.iter().copied()).collect::<Vec<_>>().join("\n")
}

fn teams(names: &[&str]) -> HashMap<String, i64> {
    names.iter().zip(1..).map(|(n, id)| ((*n).to_owned(), id)).collect()
}

/// A season whose coaches carry the real export's twelve team names.
async fn season() -> (Db, i64) {
    let db = Db::connect("sqlite::memory:").await.expect("connect");
    let season = sqlx::query_scalar!(
        "INSERT INTO season (name, min_roster, max_roster, is_active) VALUES ('S1', 8, 12, 1) RETURNING id"
    )
    .fetch_one(db.pool())
    .await
    .expect("season");
    let csv = include_str!("../draft_league_schedule.csv");
    let mut names: Vec<&str> = csv
        .lines()
        .skip(1)
        .flat_map(|l| {
            let f: Vec<&str> = l.split("\",\"").collect();
            [f[1], f[3]]
        })
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 12, "the export has twelve teams");
    for (pos, name) in (1..).zip(names) {
        let person = db.person_by_hand(&format!("1000000000000000{pos:02}"), name).await.expect("person");
        db.add_coach(season, person, 100, pos).await.expect("coach");
        let coach = db.coaches(season).await.expect("coaches").into_iter().find(|c| c.person_id == person);
        db.update_coach(coach.expect("added").id, 100, pos, name).await.expect("team name");
    }
    (db, season)
}

#[test]
fn trailing_spaces_match_and_blank_rows_are_playoffs() {
    let csv = export(&[
        r#""4812","winsome","4806","socksinthedryer ","7""#,
        r#""","","","","12""#,
    ]);
    let fixtures = parse(&csv, &teams(&["winsome", "socksinthedryer"])).expect("parse");
    assert_eq!(fixtures[0].coaches, Some((1, 2)));
    assert_eq!((fixtures[1].week, fixtures[1].coaches), (12, None));
}

#[test]
fn reports_every_bad_row_at_once() {
    let csv = export(&[
        r#""1","nobody","2","winsome","1""#,
        r#""1","winsome","","","2""#,
        r#""1","winsome","2","winsome","3""#,
        r#""1","winsome","2","josh","zero""#,
        r#""1","winsome","2","josh""#,
    ]);
    let errors = parse(&csv, &teams(&["winsome", "josh"])).expect_err("must reject");
    assert_eq!(errors.iter().map(|e| e.line).collect::<Vec<_>>(), vec![2, 3, 4, 5, 6], "got: {errors:?}");
    assert!(errors[0].message.contains("nobody"), "got: {}", errors[0].message);
}

#[tokio::test]
async fn imports_the_real_export() {
    let (db, season) = season().await;
    let n = db.import_schedule(season, include_str!("../draft_league_schedule.csv")).await.expect("import");
    // 11 round-robin weeks of 6, two semifinals, one final.
    assert_eq!(n, 11 * 6 + 3);

    let playoff: Vec<i64> =
        sqlx::query_scalar!("SELECT week FROM match WHERE is_playoff = 1 ORDER BY week")
            .fetch_all(db.pool())
            .await
            .expect("playoff");
    assert_eq!(playoff, vec![12, 12, 13]);

    // Round robin: every pair of coaches meets exactly once.
    let pairs: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(DISTINCT MIN(coach_a_id, coach_b_id) || '-' || MAX(coach_a_id, coach_b_id)) AS "n!: i64"
           FROM match WHERE is_playoff = 0"#
    )
    .fetch_one(db.pool())
    .await
    .expect("pairs");
    assert_eq!(pairs, 12 * 11 / 2);

    // Re-import is allowed until a result exists, then refused.
    db.import_schedule(season, include_str!("../draft_league_schedule.csv")).await.expect("re-import");
    sqlx::query!(
        "UPDATE match SET winner_coach_id = coach_a_id, differential = 3, result_source = 'manual'
         WHERE id = (SELECT MIN(id) FROM match)"
    )
    .execute(db.pool())
    .await
    .expect("result");
    let err = db.import_schedule(season, include_str!("../draft_league_schedule.csv")).await;
    assert!(matches!(err, Err(ImportError::ResultsRecorded(1))), "got: {err:?}");
}

#[tokio::test]
async fn only_the_match_coaches_or_an_admin_set_its_time() {
    let (db, season) = season().await;
    let coaches = db.coaches(season).await.expect("coaches");
    let (a, b) = (coaches[0].id, coaches[1].id);
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
        "INSERT INTO match (season_id, week, is_playoff) VALUES (?, 2, 1) RETURNING id AS \"id!\"",
        season
    )
    .fetch_one(db.pool())
    .await
    .expect("playoff");
    let person = |n: usize| {
        let db = db.clone();
        let id = coaches[n].person_id;
        async move { db.person(id).await.expect("read").expect("exists") }
    };
    let (coach_b, outsider) = (person(1).await, person(2).await);
    let admin = pokemon_draft_site::db::Person { is_admin: true, ..outsider.clone() };
    let first = OffsetDateTime::from_unix_timestamp(1_790_000_000).expect("time");
    let second = OffsetDateTime::from_unix_timestamp(1_790_003_600).expect("time");

    sqlx::query("UPDATE person SET showdown_username = 'bob' WHERE id = ?")
        .bind(coach_b.id)
        .execute(db.pool())
        .await
        .expect("showdown name");
    sqlx::query("UPDATE season SET format = 'gen9championsvgc2026regmc' WHERE id = ?")
        .bind(season)
        .execute(db.pool())
        .await
        .expect("format");

    let s = db.set_schedule(game, &coach_b, Some(first)).await.expect("coach sets");
    assert_eq!((s.label.as_str(), s.coaches, s.previous, s.at), ("Week 1", [a, b], None, Some(first)));
    assert_eq!(
        (s.opponent_showdown.as_deref(), s.format.as_deref(), s.best_of),
        (Some("bob"), Some("gen9championsvgc2026regmc"), 3)
    );
    let m = db.match_by_id(game).await.expect("read").expect("exists");
    assert_eq!(m.scheduled_at.as_deref(), Some("2026-09-21T14:13:20Z"));

    let err = db.set_schedule(game, &outsider, Some(second)).await;
    assert!(matches!(err, Err(ResultError::NotACoach)), "got: {err:?}");

    let s = db.set_schedule(game, &admin, Some(second)).await.expect("admin reschedules");
    assert_eq!((s.previous, s.at), (Some(first), Some(second)));

    let err = db.set_schedule(playoff, &admin, Some(first)).await;
    assert!(matches!(err, Err(ResultError::Unseeded)), "got: {err:?}");

    let s = db.set_schedule(game, &coach_b, None).await.expect("coach clears");
    assert_eq!((s.previous, s.at), (Some(second), None));
    let m = db.match_by_id(game).await.expect("read").expect("exists");
    assert_eq!(m.scheduled_at, None);
}
