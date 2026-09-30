//! Schedule CSV parsing and per-season import.
//!
//! The upload is the draftleague.net export: `Team_One_ID, Team_One_Name,
//! Team_Two_ID, Team_Two_Name, Week`. Teams are matched to coaches by team
//! name; the draftleague.net ids are dropped.

use std::collections::HashMap;

use crate::db::Db;
use crate::tiers::{RowError, split_row};

/// Column count of the schedule export.
const COLUMNS: usize = 5;
/// Zero-based column positions in the export.
const COL_TEAM_A: usize = 1;
const COL_TEAM_B: usize = 3;
const COL_WEEK: usize = 4;

/// One scheduled match. Both coaches absent means an unseeded playoff match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fixture {
    pub week: i64,
    /// Coach ids, or `None` for a playoff slot awaiting seeding.
    pub coaches: Option<(i64, i64)>,
}

/// Parses a schedule export, resolving team names through `teams`.
///
/// `teams` maps a trimmed team name to its coach id. Names in the file are
/// trimmed before lookup, since the export carries trailing spaces. Every bad
/// row is reported, not just the first.
///
/// # Errors
/// Returns every malformed row: wrong width, a week that is not a positive
/// number, exactly one team blank, an unknown team, or a team against itself.
#[expect(clippy::implicit_hasher, reason = "callers only ever build the default map")]
pub fn parse(csv: &str, teams: &HashMap<String, i64>) -> Result<Vec<Fixture>, Vec<RowError>> {
    let mut fixtures = Vec::new();
    let mut errors = Vec::new();

    for (i, line) in csv.trim_start_matches('\u{feff}').lines().enumerate().skip(1) {
        let line_no = i + 1;
        if line.trim().is_empty() {
            continue;
        }
        let mut fail = |message: String| errors.push(RowError { line: line_no, message });

        let fields = split_row(line);
        if fields.len() != COLUMNS {
            fail(format!("expected {COLUMNS} columns, found {}", fields.len()));
            continue;
        }
        let week = match fields[COL_WEEK].trim().parse::<i64>() {
            Ok(w) if w > 0 => w,
            _ => {
                fail(format!("week {:?} is not a positive number", fields[COL_WEEK]));
                continue;
            }
        };
        let (a, b) = (fields[COL_TEAM_A].trim(), fields[COL_TEAM_B].trim());
        let coaches = match (a.is_empty(), b.is_empty()) {
            (true, true) => None,
            (false, false) => {
                let (Some(&a_id), Some(&b_id)) = (teams.get(a), teams.get(b)) else {
                    let unknown: Vec<_> =
                        [a, b].into_iter().filter(|t| !teams.contains_key(*t)).collect();
                    fail(format!("no coach has team name {}", unknown.join(" or ")));
                    continue;
                };
                if a_id == b_id {
                    fail(format!("{a} is scheduled against itself"));
                    continue;
                }
                Some((a_id, b_id))
            }
            _ => {
                fail("one team is blank; a playoff row leaves both blank".to_owned());
                continue;
            }
        };
        fixtures.push(Fixture { week, coaches });
    }

    if errors.is_empty() { Ok(fixtures) } else { Err(errors) }
}

/// Why a schedule import was refused before touching the database.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("the uploaded file has problems")]
    Rows(Vec<RowError>),
    /// Replacing the schedule would throw away recorded results.
    #[error("{0} matches have results or result history; re-importing would discard them")]
    ResultsRecorded(i64),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

impl Db {
    /// Replaces a season's schedule with the matches in an uploaded export.
    ///
    /// # Errors
    /// Refuses a season with any match result or result history, or a file with
    /// malformed rows or team names no coach holds.
    pub async fn import_schedule(&self, season_id: i64, csv: &str) -> Result<usize, ImportError> {
        let mut tx = self.pool().begin().await?;

        // A cleared result still has history, and the trail must not be lost.
        let results: i64 = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM match m WHERE m.season_id = ?
               AND (m.winner_coach_id IS NOT NULL
                    OR EXISTS (SELECT 1 FROM match_result_edit e WHERE e.match_id = m.id))",
            season_id
        )
        .fetch_one(&mut *tx)
        .await?;
        if results > 0 {
            return Err(ImportError::ResultsRecorded(results));
        }

        let teams: HashMap<String, i64> = sqlx::query!(
            r#"SELECT id AS "id!", team_name AS "team_name!" FROM coach
               WHERE season_id = ? AND team_name IS NOT NULL"#,
            season_id
        )
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|r| (r.team_name, r.id))
        .collect();

        let fixtures = parse(csv, &teams).map_err(ImportError::Rows)?;

        sqlx::query!("DELETE FROM match WHERE season_id = ?", season_id).execute(&mut *tx).await?;
        for f in &fixtures {
            let (a, b) = f.coaches.unzip();
            let is_playoff = f.coaches.is_none();
            sqlx::query!(
                "INSERT INTO match (season_id, week, is_playoff, coach_a_id, coach_b_id)
                 VALUES (?, ?, ?, ?, ?)",
                season_id,
                f.week,
                is_playoff,
                a,
                b
            )
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        tracing::info!(season_id, matches = fixtures.len(), "schedule imported");
        Ok(fixtures.len())
    }
}
