//! Tier list parsing and import.

use pokemon_draft_site::db::Db;
use pokemon_draft_site::tiers::{parse, Entry, ImportError};

const HEADER: &str = r#""ID","Name","Variation","Generation","Draftable","Tier","Status""#;

fn sheet(rows: &[&str]) -> String {
    std::iter::once(HEADER).chain(rows.iter().copied()).collect::<Vec<_>>().join("\n")
}

async fn season() -> (Db, i64) {
    let db = Db::connect("sqlite::memory:").await.expect("connect");
    let id = sqlx::query_scalar!(
        "INSERT INTO season (name, min_roster, max_roster, is_active) VALUES ('S1', 8, 12, 1) RETURNING id"
    )
    .fetch_one(db.pool())
    .await
    .expect("season");
    (db, id)
}

#[test]
fn parses_draftable_rows_and_skips_the_rest() {
    let csv = sheet(&[
        r#""1","Bulbasaur","","1","false","0","excluded""#,
        r#""3","Venusaur","","1","true","17","tiered""#,
        r#""4","Venusaur","Mega","1","true","20","tiered""#,
    ]);
    let entries = parse(&csv).expect("parse");
    assert_eq!(
        entries,
        vec![Entry { pokemon_id: 3, points: 17 }, Entry { pokemon_id: 4, points: 20 }],
        "only draftable rows are priced"
    );
}

#[test]
fn reports_every_bad_row_at_once() {
    let csv = sheet(&[
        r#""x","Bad Id","","1","true","5","tiered""#,
        r#""3","Venusaur","","1","true","17","tiered""#,
        r#""5","Bad Tier","","1","true","free","tiered""#,
        r#""6","Zero","","1","true","0","tiered""#,
        r#""3","Dupe","","1","true","9","tiered""#,
    ]);
    let errors = parse(&csv).expect_err("must reject");
    // One upload, one fix-it list -- not one error per re-upload.
    assert_eq!(errors.len(), 4, "got: {errors:?}");
    assert_eq!(errors.iter().map(|e| e.line).collect::<Vec<_>>(), vec![2, 4, 5, 6]);
    assert!(errors[3].message.contains("twice"), "got: {}", errors[3].message);
}

#[test]
fn a_zero_cost_draftable_row_is_an_error_not_a_skip() {
    // The seed sheet writes tier 0 on excluded rows. Marked draftable, that is
    // a mistake worth surfacing rather than importing an unpriced entry.
    let csv = sheet(&[r#""3","Venusaur","","1","true","0","tiered""#]);
    let errors = parse(&csv).expect_err("must reject");
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("costs start at 1"), "got: {}", errors[0].message);
}

#[test]
fn handles_quoted_commas_and_a_byte_order_mark() {
    let csv = format!(
        "\u{feff}{HEADER}\n{}",
        r#""3","Farfetch'd, Galar","","1","true","7","tiered""#
    );
    let entries = parse(&csv).expect("parse");
    assert_eq!(entries, vec![Entry { pokemon_id: 3, points: 7 }]);
}

#[tokio::test]
async fn import_replaces_the_seasons_prices() {
    let (db, season_id) = season().await;

    let n = db
        .import_tiers(season_id, &season_id_csv(&[(3, 17), (4, 20)]))
        .await
        .expect("first import");
    assert_eq!(n, 2);

    // A re-import is a replacement: the dropped entry must not linger.
    let n = db.import_tiers(season_id, &season_id_csv(&[(3, 12)])).await.expect("reimport");
    assert_eq!(n, 1);

    let rows: Vec<(i64, i64)> =
        sqlx::query_as("SELECT pokemon_id, points FROM cost WHERE season_id = ? ORDER BY pokemon_id")
            .bind(season_id)
            .fetch_all(db.pool())
            .await
            .expect("costs");
    assert_eq!(rows, vec![(3, 12)], "re-import replaces rather than merges");
}

#[tokio::test]
async fn import_refuses_unknown_ids() {
    let (db, season_id) = season().await;
    // 99999 is not in the seeded pokemon list.
    let err = db
        .import_tiers(season_id, &season_id_csv(&[(3, 17), (99999, 5)]))
        .await
        .expect_err("must refuse");
    assert!(matches!(err, ImportError::UnknownIds(ref s) if s.contains("99999")), "got: {err:?}");

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cost WHERE season_id = ?")
        .bind(season_id)
        .fetch_one(db.pool())
        .await
        .expect("count");
    assert_eq!(count, 0, "a refused import must leave no rows behind");
}

#[tokio::test]
async fn import_refuses_a_season_that_has_picks() {
    let (db, season_id) = season().await;
    db.import_tiers(season_id, &season_id_csv(&[(3, 17)])).await.expect("import");

    let person = db.upsert_discord_person("111", "ann").await.expect("person");
    let coach_id: i64 = sqlx::query_scalar(
        "INSERT INTO coach (person_id, season_id, budget, draft_position) VALUES (?, ?, 100, 1) RETURNING id",
    )
    .bind(person.id)
    .bind(season_id)
    .fetch_one(db.pool())
    .await
    .expect("coach");
    sqlx::query(
        "INSERT INTO pick (coach_id, season_id, pokemon_id, pick_number, points_paid)
         VALUES (?, ?, 3, 1, 17)",
    )
    .bind(coach_id)
    .bind(season_id)
    .execute(db.pool())
    .await
    .expect("pick");

    // Repricing mid-draft would rewrite what someone already paid.
    let err = db.import_tiers(season_id, &season_id_csv(&[(3, 1)])).await.expect_err("must refuse");
    assert!(matches!(err, ImportError::DraftStarted(1)), "got: {err:?}");

    let points: i64 = sqlx::query_scalar("SELECT points FROM cost WHERE season_id = ? AND pokemon_id = 3")
        .bind(season_id)
        .fetch_one(db.pool())
        .await
        .expect("cost");
    assert_eq!(points, 17, "the original price must survive a refused import");
}

/// Builds a minimal sheet pricing each `(id, cost)` pair.
fn season_id_csv(rows: &[(i64, i64)]) -> String {
    let rows: Vec<String> = rows
        .iter()
        .map(|(id, cost)| format!(r#""{id}","Name","","1","true","{cost}","tiered""#))
        .collect();
    sheet(&rows.iter().map(String::as_str).collect::<Vec<_>>())
}

#[tokio::test]
async fn the_shipped_format_sheet_imports_whole() {
    // The end-to-end check: the real export, not a hand-built fixture.
    let (db, season_id) = season().await;
    let csv = std::fs::read_to_string("format.csv").expect("read format.csv");
    let n = db.import_tiers(season_id, &csv).await.expect("import the real sheet");
    assert_eq!(n, 338, "every draftable row in format.csv must price");
}
