//! Tier list CSV parsing and per-season import.
//!
//! The upload is the format sheet as exported: seven quoted columns, one row
//! per dex entry, where `Tier` is the cost and `Draftable` selects the rows
//! that belong in the season.

use crate::db::Db;

/// Column count of the format sheet. A row with any other width is malformed.
const COLUMNS: usize = 7;
/// Zero-based column positions in the format sheet.
const COL_ID: usize = 0;
const COL_DRAFTABLE: usize = 4;
const COL_TIER: usize = 5;

/// One priced entry destined for a season's tier list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// Row id from the sheet, which is also `pokemon.id`.
    pub pokemon_id: i64,
    pub points: i64,
}

/// A problem with one row, reported with its line number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowError {
    /// 1-based line number in the uploaded text, counting the header.
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for RowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// Splits one CSV line, honouring quotes and doubled-quote escapes.
pub(crate) fn split_row(line: &str) -> Vec<String> {
    let mut fields = Vec::with_capacity(COLUMNS);
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(std::mem::take(&mut field)),
            _ => field.push(c),
        }
    }
    fields.push(field);
    fields
}

/// Parses an uploaded tier sheet into the entries it prices.
///
/// Non-draftable rows are skipped rather than rejected: the sheet lists the
/// whole dex and marks what is legal. Every bad row is reported, not just the
/// first, so an admin fixes one upload instead of ten.
///
/// # Errors
/// Returns every malformed row: wrong width, unparseable id or tier, a cost
/// below 1, or an id repeated within the file.
pub fn parse(csv: &str) -> Result<Vec<Entry>, Vec<RowError>> {
    let mut entries = Vec::new();
    let mut errors = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // A spreadsheet export opens with a byte-order mark; the header row is
    // whatever comes first, and is skipped whether or not it is labelled.
    for (i, line) in csv.trim_start_matches('\u{feff}').lines().enumerate().skip(1) {
        let line_no = i + 1;
        if line.trim().is_empty() {
            continue;
        }
        let fields = split_row(line);
        if fields.len() != COLUMNS {
            errors.push(RowError {
                line: line_no,
                message: format!("expected {COLUMNS} columns, found {}", fields.len()),
            });
            continue;
        }
        if !fields[COL_DRAFTABLE].trim().eq_ignore_ascii_case("true") {
            continue;
        }
        let Ok(pokemon_id) = fields[COL_ID].trim().parse::<i64>() else {
            errors.push(RowError {
                line: line_no,
                message: format!("id {:?} is not a number", fields[COL_ID]),
            });
            continue;
        };
        let Ok(points) = fields[COL_TIER].trim().parse::<i64>() else {
            errors.push(RowError {
                line: line_no,
                message: format!("tier {:?} is not a number", fields[COL_TIER]),
            });
            continue;
        };
        if points < 1 {
            errors.push(RowError {
                line: line_no,
                message: format!("draftable entry priced at {points}; costs start at 1"),
            });
            continue;
        }
        if !seen.insert(pokemon_id) {
            errors.push(RowError { line: line_no, message: format!("id {pokemon_id} appears twice") });
            continue;
        }
        entries.push(Entry { pokemon_id, points });
    }

    if errors.is_empty() { Ok(entries) } else { Err(errors) }
}

/// Why an import was refused before touching the database.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("the uploaded file has problems")]
    Rows(Vec<RowError>),
    /// Repricing a draft in progress would rewrite what people already paid.
    #[error("season already has {0} picks; repricing it would corrupt the draft")]
    DraftStarted(i64),
    #[error("unknown pokemon ids: {0}")]
    UnknownIds(String),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

impl Db {
    /// Replaces a season's tier list with the prices in an uploaded sheet.
    ///
    /// # Errors
    /// Refuses a season that already has picks, a sheet with malformed rows,
    /// or ids that are not in the pokemon list.
    pub async fn import_tiers(&self, season_id: i64, csv: &str) -> Result<usize, ImportError> {
        let entries = parse(csv).map_err(ImportError::Rows)?;

        let mut tx = self.pool().begin().await?;

        let picks: i64 = sqlx::query_scalar!("SELECT COUNT(*) FROM pick WHERE season_id = ?", season_id)
            .fetch_one(&mut *tx)
            .await?;
        if picks > 0 {
            return Err(ImportError::DraftStarted(picks));
        }

        // Validated up front so an unknown id is reported as such, rather than
        // surfacing as an opaque foreign-key failure.
        let known: std::collections::HashSet<i64> =
            sqlx::query_scalar!("SELECT id FROM pokemon").fetch_all(&mut *tx).await?.into_iter().collect();
        let mut unknown: Vec<i64> =
            entries.iter().map(|e| e.pokemon_id).filter(|id| !known.contains(id)).collect();
        if !unknown.is_empty() {
            unknown.sort_unstable();
            let listed =
                unknown.iter().map(i64::to_string).collect::<Vec<_>>().join(", ");
            return Err(ImportError::UnknownIds(listed));
        }

        // Import replaces the season's list rather than merging into it, so a
        // removed entry actually disappears.
        sqlx::query!("DELETE FROM cost WHERE season_id = ?", season_id).execute(&mut *tx).await?;
        for e in &entries {
            sqlx::query!(
                "INSERT INTO cost (season_id, pokemon_id, points) VALUES (?, ?, ?)",
                season_id,
                e.pokemon_id,
                e.points
            )
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        tracing::info!(season_id, entries = entries.len(), "tier list imported");
        Ok(entries.len())
    }
}
