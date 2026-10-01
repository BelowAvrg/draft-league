//! Post-draft roster moves: free agency, trades, and ownership by week.
//!
//! Ownership changes the moment a move is made; the move only *plays* from
//! its effective week. See TRADES.md.

use sqlx::SqliteConnection;

use crate::db::Db;
use crate::draft::Roster;
use crate::picks::DraftError;

/// Moves each coach may make in a season. League rule; a trade uses one from
/// each side.
pub const MAX_MOVES: i64 = 4;

/// Why a move was refused.
#[derive(Debug, thiserror::Error)]
pub enum MoveError {
    #[error("moves open once the draft is over")]
    DraftRunning,
    #[error("there is no schedule yet, so there is no week to move in")]
    NoSchedule,
    #[error("the regular season is over; no more moves")]
    SeasonOver,
    #[error("a move now would play from week {week}, after the week {last} deadline")]
    PastDeadline { week: i64, last: i64 },
    #[error("no moves left for {who}; each coach gets {MAX_MOVES} a season")]
    NoMovesLeft { who: String },
    #[error("that Pokémon is not on the roster it is being moved from")]
    NotOwned,
    #[error("that Pokémon has not joined its roster yet; it can move once it has")]
    StillPending,
    #[error("that Pokémon is not available")]
    Unavailable,
    #[error("{who} would end at {cost} points, over the budget of {budget}")]
    OverBudget { who: String, cost: i64, budget: i64 },
    #[error("{who} would end with {size} Pokémon; a roster holds {min} to {max}")]
    Size { who: String, size: i64, min: i64, max: i64 },
    #[error("no such open offer")]
    NoOffer,
    #[error("each side has to give at least one Pokémon")]
    EmptySide,
    #[error("you cannot trade with yourself")]
    SelfTrade,
    #[error(transparent)]
    Draft(#[from] DraftError),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

/// One Pokémon on a coach's roster page, including moves still pending.
#[derive(Debug, Clone)]
pub struct Owned {
    pub pokemon_id: i64,
    pub slug: String,
    pub display_name: String,
    /// Space-separated, primary type first.
    pub types: String,
    /// This season's tier-list price.
    pub points: i64,
    /// The pick that drafted it, if this coach drafted it.
    pub pick_number: Option<i64>,
    /// First week it plays for this coach, while that is still ahead.
    pub joins: Option<i64>,
    /// First week it no longer plays for this coach, when it is on the way out.
    pub leaves: Option<i64>,
}

/// Checks one side of a move once it lands: roster size and budget.
///
/// `who` names the side in the error, e.g. "you" or a coach's name.
///
/// # Errors
/// Refuses a size outside `roster` or a cost over `budget`.
pub fn check_side(who: &str, size: i64, cost: i64, budget: i64, roster: Roster) -> Result<(), MoveError> {
    if !(roster.min()..=roster.max()).contains(&size) {
        return Err(MoveError::Size { who: who.to_owned(), size, min: roster.min(), max: roster.max() });
    }
    if cost > budget {
        return Err(MoveError::OverBudget { who: who.to_owned(), cost, budget });
    }
    Ok(())
}

impl Db {
    /// The regular-season week a coach is playing now: their earliest match
    /// without a result. `None` when they have none left, or no schedule.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn current_week(&self, coach_id: i64) -> Result<Option<i64>, sqlx::Error> {
        current_week(&mut *self.pool().acquire().await?, coach_id).await
    }

    /// Moves a coach has made this season, trades included.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn moves_used(&self, coach_id: i64) -> Result<i64, sqlx::Error> {
        moves_used(&mut *self.pool().acquire().await?, coach_id).await
    }

    /// A coach's roster as their page shows it: what plays this week, plus
    /// pending arrivals and departures. Draft picks first, in pick order.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn roster(&self, coach_id: i64) -> Result<Vec<Owned>, sqlx::Error> {
        // With no week left to play, every move has landed.
        let now = self.current_week(coach_id).await?.unwrap_or(i64::MAX);
        let rows = sqlx::query!(
            r#"SELECT re.pokemon_id, m.slug, m.display_name, m.types,
                      ct.points AS "points!: i64", pk.pick_number AS "pick_number?: i64",
                      re.from_week, re.until_week
               FROM roster_entry re
               JOIN pokemon m ON m.id = re.pokemon_id
               JOIN cost ct ON ct.season_id = re.season_id AND ct.pokemon_id = re.pokemon_id
               LEFT JOIN pick pk ON pk.coach_id = re.coach_id AND pk.pokemon_id = re.pokemon_id
               WHERE re.coach_id = ?
               ORDER BY pk.pick_number IS NULL, pk.pick_number, re.id"#,
            coach_id
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .filter(|r| r.until_week.is_none_or(|u| u > now))
            .map(|r| Owned {
                pokemon_id: r.pokemon_id,
                slug: r.slug,
                display_name: r.display_name,
                types: r.types,
                points: r.points,
                pick_number: r.pick_number,
                joins: Some(r.from_week).filter(|&w| w > now),
                leaves: r.until_week,
            })
            .collect())
    }

    /// Drops one owned Pokémon and picks up one nobody owns.
    ///
    /// Returns the week the swap takes effect.
    ///
    /// The swap plays from the week after the coach's current one; until
    /// then the roster page shows it pending. Budget is checked against the
    /// roster with every pending move applied, which is what ownership
    /// already records.
    ///
    /// # Errors
    /// Refuses during the draft, with no schedule, after the deadline, past
    /// [`MAX_MOVES`], over budget, or when `drop` is not owned (or not yet
    /// arrived) or `pickup` is owned or unpriced.
    pub async fn free_agency(&self, coach_id: i64, drop: i64, pickup: i64) -> Result<i64, MoveError> {
        let board = self.board().await?;
        if board.seats.is_empty() || board.turn.coach_id.is_some() {
            return Err(MoveError::DraftRunning);
        }
        board.seat_of(coach_id).ok_or(DraftError::NotACoach)?;
        let season_id = board.season.id;

        let mut tx = self.pool().begin().await?;
        let week = effective_week(&mut tx, season_id, &[coach_id]).await?;
        if moves_used(&mut tx, coach_id).await? >= MAX_MOVES {
            return Err(MoveError::NoMovesLeft { who: "you".to_owned() });
        }

        let dropped = sqlx::query!(
            "SELECT id, from_week FROM roster_entry
             WHERE coach_id = ? AND pokemon_id = ? AND until_week IS NULL",
            coach_id,
            drop
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(MoveError::NotOwned)?;
        // Its entry would have to end before it starts.
        if dropped.from_week >= week {
            return Err(MoveError::StillPending);
        }

        let owned: bool = sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT 1 FROM roster_entry
                   WHERE season_id = ? AND pokemon_id = ? AND until_week IS NULL) AS "e!: bool""#,
            season_id,
            pickup
        )
        .fetch_one(&mut *tx)
        .await?;
        let price = |id| {
            sqlx::query_scalar!(
                r#"SELECT points AS "points!: i64" FROM cost WHERE season_id = ? AND pokemon_id = ?"#,
                season_id,
                id
            )
        };
        let pickup_cost = price(pickup).fetch_optional(&mut *tx).await?;
        let (false, Some(pickup_cost)) = (owned, pickup_cost) else {
            return Err(MoveError::Unavailable);
        };
        let drop_cost = price(drop).fetch_one(&mut *tx).await?;

        let side = side(&mut tx, coach_id).await?;
        check_side("you", side.size, side.cost - drop_cost + pickup_cost, side.budget, board.roster)?;

        let move_id = sqlx::query_scalar!(
            "INSERT INTO move (season_id, kind, coach_a_id, effective_week)
             VALUES (?, 'free_agency', ?, ?) RETURNING id",
            season_id,
            coach_id,
            week
        )
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query!(
            "UPDATE roster_entry SET until_week = ?, left_move_id = ? WHERE id = ?",
            week,
            move_id,
            dropped.id
        )
        .execute(&mut *tx)
        .await?;
        close_offers_with(&mut tx, &[drop], None).await?;
        // A race for the same free agent loses here, on the one-owner index.
        sqlx::query!(
            "INSERT INTO roster_entry (season_id, coach_id, pokemon_id, from_week, move_id)
             VALUES (?, ?, ?, ?, ?)",
            season_id,
            coach_id,
            pickup,
            week,
            move_id
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        tracing::info!(coach_id, drop, pickup, week, "free agency move");
        Ok(week)
    }
}

/// Declines every open offer, other than `except`, that includes any of
/// `pokemon`: it can no longer go through as offered.
pub(crate) async fn close_offers_with(
    conn: &mut SqliteConnection,
    pokemon: &[i64],
    except: Option<i64>,
) -> Result<(), sqlx::Error> {
    for &id in pokemon {
        sqlx::query!(
            "UPDATE trade_offer
             SET status = 'declined', decided_at = datetime('now'),
                 reason = 'A Pokémon in it has since moved.'
             WHERE status = 'open' AND id IS NOT ?
               AND id IN (SELECT offer_id FROM trade_offer_item WHERE pokemon_id = ?)",
            except,
            id
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// A coach's roster as it stands with pending moves applied.
pub(crate) struct Side {
    pub name: String,
    pub size: i64,
    pub cost: i64,
    pub budget: i64,
}

pub(crate) async fn side(conn: &mut SqliteConnection, coach_id: i64) -> Result<Side, sqlx::Error> {
    sqlx::query_as!(
        Side,
        r#"SELECT p.discord_username AS name, c.budget,
                  COUNT(re.id) AS "size!: i64", COALESCE(SUM(ct.points), 0) AS "cost!: i64"
           FROM coach c
           JOIN person p ON p.id = c.person_id
           LEFT JOIN roster_entry re ON re.coach_id = c.id AND re.until_week IS NULL
           LEFT JOIN cost ct ON ct.season_id = re.season_id AND ct.pokemon_id = re.pokemon_id
           WHERE c.id = ?
           GROUP BY c.id"#,
        coach_id
    )
    .fetch_one(conn)
    .await
}

async fn current_week(conn: &mut SqliteConnection, coach_id: i64) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT MIN(week) AS "w?: i64" FROM match
           WHERE is_playoff = 0 AND winner_coach_id IS NULL AND ? IN (coach_a_id, coach_b_id)"#,
        coach_id
    )
    .fetch_one(conn)
    .await
}

pub(crate) async fn moves_used(conn: &mut SqliteConnection, coach_id: i64) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "n!: i64" FROM move WHERE ? IN (coach_a_id, coach_b_id)"#,
        coach_id
    )
    .fetch_one(conn)
    .await
}

/// The week a move made now by `coaches` would first play: after the later
/// of their current weeks, so neither side's unplayed match changes.
pub(crate) async fn effective_week(
    conn: &mut SqliteConnection,
    season_id: i64,
    coaches: &[i64],
) -> Result<i64, MoveError> {
    let last: Option<i64> = sqlx::query_scalar!(
        r#"SELECT MAX(week) AS "w?: i64" FROM match WHERE season_id = ? AND is_playoff = 0"#,
        season_id
    )
    .fetch_one(&mut *conn)
    .await?;
    let last = last.ok_or(MoveError::NoSchedule)?;
    let mut now = 0;
    for &coach_id in coaches {
        now = now.max(current_week(conn, coach_id).await?.ok_or(MoveError::SeasonOver)?);
    }
    let week = now + 1;
    if week > last {
        return Err(MoveError::PastDeadline { week, last });
    }
    Ok(week)
}
