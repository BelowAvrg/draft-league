//! Match results: the schedule as pages show it, manual entry, and the edit trail.

use std::ops::RangeInclusive;

use sqlx::SqliteConnection;

use crate::coaches::season_coaches;
use crate::db::{Db, Person};
use crate::replays::{Replay, to_id};
use crate::standings::standings;

/// Pokémon each side brings to a VGC game: the most a game's winner can have left.
const BROUGHT: i64 = 4;

/// Differentials a winner can post in a best-of-`best_of` match.
///
/// The top is a sweep that loses nothing. The bottom is winning each needed
/// game by one and losing every other game to a full team.
///
/// # Panics
/// Panics if `best_of` is not a positive odd number, which the schema forbids.
#[must_use]
pub fn differential_range(best_of: i64) -> RangeInclusive<i64> {
    assert!(best_of > 0 && best_of % 2 == 1, "best_of must be positive and odd, got {best_of}");
    let wins = best_of / 2 + 1;
    let losses = wins - 1;
    (wins - losses * BROUGHT)..=(wins * BROUGHT)
}

/// A forfeit scores as the biggest possible win: a clean sweep.
#[must_use]
pub fn forfeit_differential(best_of: i64) -> i64 {
    *differential_range(best_of).end()
}

/// How a hand-entered result was won.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Score {
    /// Played out; the winner's differential.
    Differential(i64),
    /// The loser forfeited; scored as a sweep.
    Forfeit,
}

/// A hand-entered result for one match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// Coach id of the winner.
    pub winner: i64,
    pub score: Score,
}

/// Why a result write was refused.
#[derive(Debug, thiserror::Error)]
pub enum ResultError {
    #[error("no such match")]
    NoMatch,
    #[error("only coaches in this season can enter results")]
    NotACoach,
    #[error("this playoff match has no teams yet")]
    Unseeded,
    #[error("the winner must be one of the two coaches in this match")]
    NotInMatch,
    #[error("a differential of {got} is impossible in a best of {best_of}; it must be {min} to {max}")]
    Differential { got: i64, best_of: i64, min: i64, max: i64 },
    #[error("the replay was played in {got}, but this season plays {want}")]
    WrongFormat { got: String, want: String },
    #[error("{0} isn't either coach's Showdown username; set it on the coach's profile and try again, or enter the result by hand")]
    UnknownPlayer(String),
    #[error("that replay is already attached to a match")]
    AlreadyAttached,
    #[error("this match's replays already decide it")]
    Decided,
    #[error("this would change who plays in {0}, which already has a result or a replay; clear that first")]
    Reseed(&'static str),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

/// Renders a result the way every page shows it.
fn describe(winner: Option<&str>, differential: Option<i64>, is_forfeit: bool) -> String {
    match (winner, differential) {
        (Some(w), Some(d)) if is_forfeit => format!("{w} by forfeit ({d:+})"),
        (Some(w), Some(d)) => format!("{w} {d:+}"),
        _ => "no result".to_owned(),
    }
}

/// One replayed game, scored from the match's coach slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameScore {
    /// Whether the coach in slot A won.
    pub a_won: bool,
    pub a_remaining: i64,
    pub b_remaining: i64,
}

/// The result a match's games produce, once someone has won enough of them.
///
/// Returns whether slot A won, and the winner's differential: KOs dealt minus
/// KOs taken, summed over games. Both sides bring the same number, so a game
/// counts as the winner's Pokémon left minus the loser's.
#[must_use]
pub fn tally(best_of: i64, games: &[GameScore]) -> Option<(bool, i64)> {
    let needed = best_of / 2 + 1;
    let a_wins: i64 = games.iter().map(|g| i64::from(g.a_won)).sum();
    let b_wins: i64 = games.iter().map(|g| i64::from(!g.a_won)).sum();
    let a_won = match (a_wins >= needed, b_wins >= needed) {
        (true, _) => true,
        (_, true) => false,
        _ => return None,
    };
    let differential = games
        .iter()
        .map(|g| if a_won { g.a_remaining - g.b_remaining } else { g.b_remaining - g.a_remaining })
        .sum();
    Some((a_won, differential))
}

/// One replay attached to a match, as the match page lists it.
#[derive(Debug, Clone)]
pub struct Game {
    pub id: i64,
    pub replay_url: String,
    /// Team name of the game's winner, falling back to their Discord name.
    pub winner: String,
    /// Pokémon the winner had left standing.
    pub winner_remaining: i64,
}

/// A match result as stored: winner, differential, forfeit.
#[derive(Debug, Clone, Copy)]
struct Outcome {
    winner: Option<i64>,
    differential: Option<i64>,
    is_forfeit: bool,
}

/// One scheduled match with its teams and result.
#[derive(Debug, Clone)]
pub struct Match {
    pub id: i64,
    pub season_id: i64,
    pub week: i64,
    pub is_playoff: bool,
    /// Coach ids; `None` on a playoff match not yet seeded.
    pub coach_a: Option<i64>,
    pub coach_b: Option<i64>,
    /// Team name, falling back to the coach's Discord name.
    pub team_a: Option<String>,
    pub team_b: Option<String>,
    pub winner: Option<i64>,
    pub differential: Option<i64>,
    pub is_forfeit: bool,
    /// The season's games per match.
    pub best_of: i64,
}

impl Match {
    /// The result in words, or "no result".
    #[must_use]
    pub fn result(&self) -> String {
        let winner = match self.winner {
            Some(w) if Some(w) == self.coach_a => self.team_a.as_deref(),
            Some(_) => self.team_b.as_deref(),
            None => None,
        };
        describe(winner, self.differential, self.is_forfeit)
    }

    /// Whether this coach plays in the match. `None` never does.
    #[must_use]
    pub fn involves(&self, coach: Option<i64>) -> bool {
        coach.is_some() && (self.coach_a == coach || self.coach_b == coach)
    }
}

/// One write to a match result, as the history lists it.
#[derive(Debug, Clone)]
pub struct Edit {
    /// Discord name of whoever made the write.
    pub by: String,
    pub created_at: String,
    /// `manual` or `replay`.
    pub source: String,
    pub old_winner: Option<String>,
    pub old_differential: Option<i64>,
    pub old_is_forfeit: bool,
    pub new_winner: Option<String>,
    pub new_differential: Option<i64>,
    pub new_is_forfeit: bool,
}

impl Edit {
    /// The result before this write.
    #[must_use]
    pub fn before(&self) -> String {
        describe(self.old_winner.as_deref(), self.old_differential, self.old_is_forfeit)
    }

    /// The result this write set.
    #[must_use]
    pub fn after(&self) -> String {
        describe(self.new_winner.as_deref(), self.new_differential, self.new_is_forfeit)
    }
}

impl Db {
    /// Every match in a season, by week.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn matches(&self, season_id: i64) -> Result<Vec<Match>, sqlx::Error> {
        season_matches(self.pool(), season_id).await
    }

    /// One match by id.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn match_by_id(&self, id: i64) -> Result<Option<Match>, sqlx::Error> {
        sqlx::query_as!(
            Match,
            r#"SELECT m.id, m.season_id, m.week, m.is_playoff AS "is_playoff!: bool",
                      m.coach_a_id AS coach_a, m.coach_b_id AS coach_b,
                      COALESCE(ca.team_name, pa.discord_username) AS "team_a?: String",
                      COALESCE(cb.team_name, pb.discord_username) AS "team_b?: String",
                      m.winner_coach_id AS winner, m.differential,
                      m.is_forfeit AS "is_forfeit!: bool", s.best_of
               FROM match m JOIN season s ON s.id = m.season_id
               LEFT JOIN coach ca ON ca.id = m.coach_a_id LEFT JOIN person pa ON pa.id = ca.person_id
               LEFT JOIN coach cb ON cb.id = m.coach_b_id LEFT JOIN person pb ON pb.id = cb.person_id
               WHERE m.id = ?"#,
            id
        )
        .fetch_optional(self.pool())
        .await
    }

    /// Every write to a match's result, oldest first.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn result_history(&self, match_id: i64) -> Result<Vec<Edit>, sqlx::Error> {
        sqlx::query_as!(
            Edit,
            r#"SELECT p.discord_username AS by, e.created_at, e.source,
                      COALESCE(co.team_name, po.discord_username) AS "old_winner?: String",
                      e.old_differential, e.old_is_forfeit AS "old_is_forfeit!: bool",
                      COALESCE(cn.team_name, pn.discord_username) AS "new_winner?: String",
                      e.new_differential, e.new_is_forfeit AS "new_is_forfeit!: bool"
               FROM match_result_edit e
               JOIN person p ON p.id = e.person_id
               LEFT JOIN coach co ON co.id = e.old_winner_coach_id LEFT JOIN person po ON po.id = co.person_id
               LEFT JOIN coach cn ON cn.id = e.new_winner_coach_id LEFT JOIN person pn ON pn.id = cn.person_id
               WHERE e.match_id = ?
               ORDER BY e.id"#,
            match_id
        )
        .fetch_all(self.pool())
        .await
    }

    /// Sets or clears a match result by hand, and logs the write.
    ///
    /// `None` clears the result. Any coach in the match's season may do this,
    /// as may an admin; the edit trail is the safeguard.
    ///
    /// # Errors
    /// Refuses a caller who is neither, an unseeded playoff match, a winner
    /// outside the match, or a differential the season's best-of cannot produce.
    /// Also refuses a write that would re-seed a playoff match that already
    /// has a result.
    pub async fn set_result(
        &self,
        match_id: i64,
        by: &Person,
        entry: Option<Entry>,
    ) -> Result<(), ResultError> {
        let mut tx = self.pool().begin().await?;

        let m = sqlx::query!(
            r#"SELECT m.season_id, m.coach_a_id, m.coach_b_id, m.winner_coach_id,
                      m.differential, m.is_forfeit AS "is_forfeit!: bool", s.best_of
               FROM match m JOIN season s ON s.id = m.season_id
               WHERE m.id = ?"#,
            match_id
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ResultError::NoMatch)?;

        may_edit(&mut tx, m.season_id, by).await?;

        let (Some(a), Some(b)) = (m.coach_a_id, m.coach_b_id) else {
            return Err(ResultError::Unseeded);
        };
        let (winner, differential, is_forfeit) = match entry {
            None => (None, None, false),
            Some(e) if e.winner != a && e.winner != b => return Err(ResultError::NotInMatch),
            Some(Entry { winner, score: Score::Forfeit }) => {
                (Some(winner), Some(forfeit_differential(m.best_of)), true)
            }
            Some(Entry { winner, score: Score::Differential(d) }) => {
                let range = differential_range(m.best_of);
                if !range.contains(&d) {
                    return Err(ResultError::Differential {
                        got: d,
                        best_of: m.best_of,
                        min: *range.start(),
                        max: *range.end(),
                    });
                }
                (Some(winner), Some(d), false)
            }
        };
        let old = Outcome { winner: m.winner_coach_id, differential: m.differential, is_forfeit: m.is_forfeit };
        let new = Outcome { winner, differential, is_forfeit };
        record(&mut tx, m.season_id, match_id, by.id, "manual", old, new).await?;

        tx.commit().await?;
        tracing::info!(match_id, by = by.id, ?winner, ?differential, is_forfeit, "match result set");
        Ok(())
    }

    /// Every replay attached to a match, in upload order.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn games(&self, match_id: i64) -> Result<Vec<Game>, sqlx::Error> {
        sqlx::query_as!(
            Game,
            r#"SELECT g.id, g.replay_url, COALESCE(c.team_name, p.discord_username) AS "winner!: String",
                      CASE WHEN g.winner_coach_id = m.coach_a_id THEN g.a_remaining ELSE g.b_remaining END AS "winner_remaining!: i64"
               FROM game g JOIN match m ON m.id = g.match_id
               JOIN coach c ON c.id = g.winner_coach_id JOIN person p ON p.id = c.person_id
               WHERE g.match_id = ?
               ORDER BY g.id"#,
            match_id
        )
        .fetch_all(self.pool())
        .await
    }

    /// Attaches a replay to a match as a game, and scores the match once its games decide it.
    ///
    /// A match its games don't decide yet keeps whatever result it has. Once
    /// they do, the computed result replaces it and the write is logged.
    ///
    /// # Errors
    /// Refuses a caller who isn't a coach in the season or an admin, an
    /// unseeded match, a replay in the wrong format, a player who isn't one of
    /// the two coaches, a replay already attached, and a match already decided
    /// by its replays.
    pub async fn add_game(&self, match_id: i64, by: &Person, replay: &Replay) -> Result<(), ResultError> {
        let mut tx = self.pool().begin().await?;

        let m = sqlx::query!(
            r#"SELECT m.season_id, m.coach_a_id, m.coach_b_id, m.winner_coach_id,
                      m.differential, m.is_forfeit AS "is_forfeit!: bool", s.best_of, s.format,
                      pa.showdown_username AS "showdown_a?: String",
                      pb.showdown_username AS "showdown_b?: String"
               FROM match m JOIN season s ON s.id = m.season_id
               LEFT JOIN coach ca ON ca.id = m.coach_a_id LEFT JOIN person pa ON pa.id = ca.person_id
               LEFT JOIN coach cb ON cb.id = m.coach_b_id LEFT JOIN person pb ON pb.id = cb.person_id
               WHERE m.id = ?"#,
            match_id
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ResultError::NoMatch)?;

        may_edit(&mut tx, m.season_id, by).await?;
        let (Some(a), Some(b)) = (m.coach_a_id, m.coach_b_id) else {
            return Err(ResultError::Unseeded);
        };
        if let Some(want) = m.format.filter(|f| *f != replay.format) {
            return Err(ResultError::WrongFormat { got: replay.format.clone(), want });
        }

        // Which player is coach A. Both names must match, one to each coach.
        let is = |player: &str, username: &Option<String>| username.as_deref().is_some_and(|u| to_id(u) == to_id(player));
        let [p1, p2] = &replay.players;
        let p1_is_a = match (is(p1, &m.showdown_a), is(p1, &m.showdown_b)) {
            (true, _) if is(p2, &m.showdown_b) => true,
            (_, true) if is(p2, &m.showdown_a) => false,
            (false, false) => return Err(ResultError::UnknownPlayer(p1.clone())),
            _ => return Err(ResultError::UnknownPlayer(p2.clone())),
        };
        let (a_side, b_side) = if p1_is_a { (0, 1) } else { (1, 0) };
        let score = GameScore {
            a_won: replay.winner == a_side,
            a_remaining: replay.remaining[a_side],
            b_remaining: replay.remaining[b_side],
        };

        let mut games: Vec<GameScore> = sqlx::query!(
            "SELECT winner_coach_id, a_remaining, b_remaining FROM game WHERE match_id = ? ORDER BY id",
            match_id
        )
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|g| GameScore { a_won: g.winner_coach_id == a, a_remaining: g.a_remaining, b_remaining: g.b_remaining })
        .collect();
        if tally(m.best_of, &games).is_some() {
            return Err(ResultError::Decided);
        }

        let winner = if score.a_won { a } else { b };
        let inserted = sqlx::query!(
            "INSERT INTO game (match_id, replay_id, replay_url, winner_coach_id, a_remaining, b_remaining, created_by)
             VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT (replay_id) DO NOTHING",
            match_id,
            replay.id,
            replay.url,
            winner,
            score.a_remaining,
            score.b_remaining,
            by.id
        )
        .execute(&mut *tx)
        .await?;
        if inserted.rows_affected() == 0 {
            return Err(ResultError::AlreadyAttached);
        }

        games.push(score);
        if let Some((a_won, differential)) = tally(m.best_of, &games) {
            let old = Outcome { winner: m.winner_coach_id, differential: m.differential, is_forfeit: m.is_forfeit };
            let new = Outcome { winner: Some(if a_won { a } else { b }), differential: Some(differential), is_forfeit: false };
            record(&mut tx, m.season_id, match_id, by.id, "replay", old, new).await?;
        }

        tx.commit().await?;
        tracing::info!(match_id, by = by.id, replay = %replay.id, winner, "replay attached");
        Ok(())
    }

    /// Detaches a replay from a match, and re-scores a result the replays set.
    ///
    /// A replay-set result follows the games that are left, which usually
    /// means clearing it; the change is logged. A hand-entered result stays.
    ///
    /// # Errors
    /// Refuses a caller who isn't a coach in the season or an admin, a game
    /// not on this match, and a change that would re-seed a playoff match that
    /// already has a result.
    pub async fn remove_game(&self, match_id: i64, game_id: i64, by: &Person) -> Result<(), ResultError> {
        let mut tx = self.pool().begin().await?;

        let m = sqlx::query!(
            r#"SELECT m.season_id, m.coach_a_id, m.coach_b_id, m.winner_coach_id, m.differential,
                      m.is_forfeit AS "is_forfeit!: bool", m.result_source, s.best_of
               FROM match m JOIN season s ON s.id = m.season_id
               WHERE m.id = ?"#,
            match_id
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ResultError::NoMatch)?;
        may_edit(&mut tx, m.season_id, by).await?;

        let replay = sqlx::query_scalar!(
            "DELETE FROM game WHERE id = ? AND match_id = ? RETURNING replay_id",
            game_id,
            match_id
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ResultError::NoMatch)?;

        if let (Some("replay"), Some(a), Some(b)) = (m.result_source.as_deref(), m.coach_a_id, m.coach_b_id) {
            let games: Vec<GameScore> = sqlx::query!(
                "SELECT winner_coach_id, a_remaining, b_remaining FROM game WHERE match_id = ? ORDER BY id",
                match_id
            )
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .map(|g| GameScore { a_won: g.winner_coach_id == a, a_remaining: g.a_remaining, b_remaining: g.b_remaining })
            .collect();
            let (winner, differential) = match tally(m.best_of, &games) {
                Some((a_won, d)) => (Some(if a_won { a } else { b }), Some(d)),
                None => (None, None),
            };
            if (winner, differential) != (m.winner_coach_id, m.differential) {
                let old = Outcome { winner: m.winner_coach_id, differential: m.differential, is_forfeit: m.is_forfeit };
                let new = Outcome { winner, differential, is_forfeit: false };
                record(&mut tx, m.season_id, match_id, by.id, "replay", old, new).await?;
            }
        }

        tx.commit().await?;
        tracing::info!(match_id, by = by.id, %replay, "replay removed");
        Ok(())
    }
}

/// Refuses anyone who is neither a coach in the season nor an admin.
async fn may_edit(conn: &mut SqliteConnection, season_id: i64, by: &Person) -> Result<(), ResultError> {
    let is_coach: bool = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM coach WHERE person_id = ? AND season_id = ?) AS "e!: bool""#,
        by.id,
        season_id
    )
    .fetch_one(conn)
    .await?;
    if is_coach || by.is_admin { Ok(()) } else { Err(ResultError::NotACoach) }
}

/// Every match in a season, by week, on any connection. `Db::matches` wraps this.
async fn season_matches<'e>(
    ex: impl sqlx::SqliteExecutor<'e>,
    season_id: i64,
) -> Result<Vec<Match>, sqlx::Error> {
    sqlx::query_as!(
        Match,
        r#"SELECT m.id, m.season_id, m.week, m.is_playoff AS "is_playoff!: bool",
                  m.coach_a_id AS coach_a, m.coach_b_id AS coach_b,
                  COALESCE(ca.team_name, pa.discord_username) AS "team_a?: String",
                  COALESCE(cb.team_name, pb.discord_username) AS "team_b?: String",
                  m.winner_coach_id AS winner, m.differential,
                  m.is_forfeit AS "is_forfeit!: bool", s.best_of
           FROM match m JOIN season s ON s.id = m.season_id
           LEFT JOIN coach ca ON ca.id = m.coach_a_id LEFT JOIN person pa ON pa.id = ca.person_id
           LEFT JOIN coach cb ON cb.id = m.coach_b_id LEFT JOIN person pb ON pb.id = cb.person_id
           WHERE m.season_id = ?
           ORDER BY m.week, m.id"#,
        season_id
    )
    .fetch_all(ex)
    .await
}

/// Writes a match result, logs the write, and re-seeds the playoffs.
async fn record(
    conn: &mut SqliteConnection,
    season_id: i64,
    match_id: i64,
    by: i64,
    source: &'static str,
    old: Outcome,
    new: Outcome,
) -> Result<(), ResultError> {
    let result_source = new.winner.map(|_| source);
    sqlx::query!(
        "UPDATE match SET winner_coach_id = ?, differential = ?, is_forfeit = ?, result_source = ?
         WHERE id = ?",
        new.winner,
        new.differential,
        new.is_forfeit,
        result_source,
        match_id
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "INSERT INTO match_result_edit
             (match_id, person_id, source,
              old_winner_coach_id, old_differential, old_is_forfeit,
              new_winner_coach_id, new_differential, new_is_forfeit)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        match_id,
        by,
        source,
        old.winner,
        old.differential,
        old.is_forfeit,
        new.winner,
        new.differential,
        new.is_forfeit
    )
    .execute(&mut *conn)
    .await?;
    seed_playoffs(conn, season_id).await
}

/// Fills the playoff slots from the results as they now stand.
///
/// The semifinals are #1 v #4 and #2 v #3 once every regular-season match has
/// a result, and empty before that. The final is the two semifinal winners
/// once both are in. Runs after every result write, so an edit that changes
/// the standings or a semifinal winner re-seeds what depends on it.
///
/// # Errors
/// Refuses to move a playoff match that already has a result.
async fn seed_playoffs(conn: &mut SqliteConnection, season_id: i64) -> Result<(), ResultError> {
    let matches = season_matches(&mut *conn, season_id).await?;
    let playoffs: Vec<&Match> = matches.iter().filter(|m| m.is_playoff).collect();
    // Only the shape the schedule import produces: two semifinals, then a final.
    let [semi_1, semi_2, last] = playoffs[..] else {
        if !playoffs.is_empty() {
            tracing::warn!(season_id, playoff_matches = playoffs.len(), "unexpected playoff shape; not seeding");
        }
        return Ok(());
    };

    let season_done = matches.iter().all(|m| m.is_playoff || m.winner.is_some());
    let table = standings(&season_coaches(&mut *conn, season_id).await?, &matches);
    let seed = |n: usize| table.get(n).filter(|_| season_done).map(|r| r.coach_id);
    fill(conn, semi_1, (seed(0), seed(3)), "a semifinal").await?;
    fill(conn, semi_2, (seed(1), seed(2)), "a semifinal").await?;
    // A semifinal that just moved had no result, so its old winner is None.
    fill(conn, last, (semi_1.winner, semi_2.winner), "the final").await?;
    Ok(())
}

/// Puts `teams` in a playoff match's slots, unless they are there already.
async fn fill(
    conn: &mut SqliteConnection,
    m: &Match,
    teams: (Option<i64>, Option<i64>),
    round: &'static str,
) -> Result<(), ResultError> {
    // Both slots or neither: a half-known pairing stays empty.
    let (a, b) = if teams.0.is_some() && teams.1.is_some() { teams } else { (None, None) };
    if (m.coach_a, m.coach_b) == (a, b) {
        return Ok(());
    }
    let has_games: bool =
        sqlx::query_scalar!(r#"SELECT EXISTS(SELECT 1 FROM game WHERE match_id = ?) AS "e!: bool""#, m.id)
            .fetch_one(&mut *conn)
            .await?;
    if m.winner.is_some() || has_games {
        return Err(ResultError::Reseed(round));
    }
    sqlx::query!("UPDATE match SET coach_a_id = ?, coach_b_id = ? WHERE id = ?", a, b, m.id)
        .execute(&mut *conn)
        .await?;
    tracing::info!(match_id = m.id, ?a, ?b, round, "playoff match seeded");
    Ok(())
}
