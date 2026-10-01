//! Coach roster administration: budgets, draft order, and member fields.

use crate::db::{Db, Person};

/// A coach in the active season, joined to the person behind them.
#[derive(Debug, Clone)]
pub struct Coach {
    pub id: i64,
    pub person_id: i64,
    pub discord_id: String,
    pub discord_username: String,
    pub showdown_username: Option<String>,
    pub is_admin: bool,
    pub budget: i64,
    pub draft_position: i64,
    /// Tier-list cost of the roster, pending moves included.
    pub spent: i64,
    /// When this coach declared themselves finished, if they have.
    pub done_at: Option<String>,
    /// Name the schedule import matches on.
    pub team_name: Option<String>,
}

impl Coach {
    /// Budget left to spend on the rest of the roster.
    #[must_use]
    pub fn remaining(&self) -> i64 {
        self.budget - self.spent
    }
}

/// Why a coach edit was refused.
#[derive(Debug, thiserror::Error)]
pub enum CoachError {
    #[error("budget must be at least 1")]
    Budget,
    #[error("draft position must be at least 1")]
    Position,
    #[error("draft position {0} is already taken")]
    PositionTaken(i64),
    #[error("team name {0:?} is already taken")]
    TeamTaken(String),
    #[error("that person is already a coach this season")]
    Duplicate,
    #[error("a Discord ID is 17-20 digits")]
    DiscordId,
    #[error("budget {budget} is below the {spent} already spent")]
    BelowSpent { budget: i64, spent: i64 },
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

/// Discord snowflakes are 17-20 digits today and only grow.
fn valid_discord_id(s: &str) -> bool {
    (17..=20).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}

/// Maps the unique-constraint violations we can name back to a useful error.
///
/// SQLite names every column of the violated index, so the two constraints
/// must be matched whole: `person_id, season_id` contains `season_id`, and
/// testing for that substring first would report a duplicate person as a
/// position clash.
fn classify(e: sqlx::Error, position: i64, team_name: Option<&str>) -> CoachError {
    let Some(db) = e.as_database_error() else { return CoachError::Sqlx(e) };
    let columns = db.message().rsplit(": ").next().unwrap_or_default().trim().to_owned();
    match columns.as_str() {
        "coach.person_id, coach.season_id" => CoachError::Duplicate,
        "coach.season_id, coach.draft_position" => CoachError::PositionTaken(position),
        "coach.season_id, coach.team_name" => {
            CoachError::TeamTaken(team_name.unwrap_or_default().to_owned())
        }
        _ => CoachError::Sqlx(e),
    }
}

impl Db {
    /// Every coach in a season, in draft order.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn coaches(&self, season_id: i64) -> Result<Vec<Coach>, sqlx::Error> {
        season_coaches(self.pool(), season_id).await
    }

    /// People who could still be added as a coach this season.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn people_without_coach(&self, season_id: i64) -> Result<Vec<Person>, sqlx::Error> {
        sqlx::query_as!(
            Person,
            r#"SELECT id, discord_id, discord_username, showdown_username,
                      is_admin AS "is_admin!: bool"
               FROM person
               WHERE id NOT IN (SELECT person_id FROM coach WHERE season_id = ?)
               ORDER BY discord_username"#,
            season_id
        )
        .fetch_all(self.pool())
        .await
    }

    /// Adds an existing person to a season as a coach.
    ///
    /// # Errors
    /// Refuses a non-positive budget or position, a taken draft position, or a
    /// person who already coaches this season.
    pub async fn add_coach(
        &self,
        season_id: i64,
        person_id: i64,
        budget: i64,
        draft_position: i64,
    ) -> Result<(), CoachError> {
        if budget < 1 {
            return Err(CoachError::Budget);
        }
        if draft_position < 1 {
            return Err(CoachError::Position);
        }
        sqlx::query!(
            "INSERT INTO coach (person_id, season_id, budget, draft_position) VALUES (?, ?, ?, ?)",
            person_id,
            season_id,
            budget,
            draft_position
        )
        .execute(self.pool())
        .await
        .map_err(|e| classify(e, draft_position, None))?;
        tracing::info!(season_id, person_id, budget, draft_position, "coach added");
        Ok(())
    }

    /// Creates a person from a hand-entered Discord ID, or returns the existing one.
    ///
    /// The ID cannot be verified the way an OAuth login verifies it: a typo
    /// produces a person nobody can ever log in as. Kept for pre-seeding a
    /// roster before everyone has logged in.
    ///
    /// # Errors
    /// Refuses an ID that is not 17-20 digits.
    pub async fn person_by_hand(
        &self,
        discord_id: &str,
        discord_username: &str,
    ) -> Result<i64, CoachError> {
        let discord_id = discord_id.trim();
        if !valid_discord_id(discord_id) {
            return Err(CoachError::DiscordId);
        }
        let name = discord_username.trim();
        let name = if name.is_empty() { discord_id } else { name };
        // A later OAuth login for this ID updates the placeholder username.
        let id = sqlx::query_scalar!(
            r#"INSERT INTO person (discord_id, discord_username) VALUES (?, ?)
               ON CONFLICT (discord_id) DO UPDATE SET discord_id = excluded.discord_id
               RETURNING id"#,
            discord_id,
            name
        )
        .fetch_one(self.pool())
        .await?;
        Ok(id)
    }

    /// Updates a coach's budget, draft position, and team name.
    ///
    /// A blank team name clears it.
    ///
    /// # Errors
    /// Refuses a budget below what the coach has already spent, a non-positive
    /// value, or a draft position or team name another coach holds.
    pub async fn update_coach(
        &self,
        coach_id: i64,
        budget: i64,
        draft_position: i64,
        team_name: &str,
    ) -> Result<(), CoachError> {
        // Stored trimmed because the schedule import compares trimmed names.
        let team_name = Some(team_name.trim()).filter(|s| !s.is_empty());
        if budget < 1 {
            return Err(CoachError::Budget);
        }
        if draft_position < 1 {
            return Err(CoachError::Position);
        }
        // Cutting the budget under what is already spent would leave a roster
        // that no longer satisfies the cap rule.
        let spent: i64 = sqlx::query_scalar!(
            r#"SELECT COALESCE(SUM(ct.points), 0) AS "s!: i64"
               FROM roster_entry re
               JOIN cost ct ON ct.season_id = re.season_id AND ct.pokemon_id = re.pokemon_id
               WHERE re.coach_id = ? AND re.until_week IS NULL"#,
            coach_id
        )
        .fetch_one(self.pool())
        .await?;
        if budget < spent {
            return Err(CoachError::BelowSpent { budget, spent });
        }
        sqlx::query!(
            "UPDATE coach SET budget = ?, draft_position = ?, team_name = ? WHERE id = ?",
            budget,
            draft_position,
            team_name,
            coach_id
        )
        .execute(self.pool())
        .await
        .map_err(|e| classify(e, draft_position, team_name))?;
        tracing::info!(coach_id, budget, draft_position, "coach updated");
        Ok(())
    }

    /// Removes a coach from the season.
    ///
    /// # Errors
    /// Fails if the coach has picks, which foreign keys refuse to orphan.
    pub async fn remove_coach(&self, coach_id: i64) -> Result<(), CoachError> {
        sqlx::query!("DELETE FROM queue_slot WHERE coach_id = ?", coach_id)
            .execute(self.pool())
            .await?;
        sqlx::query!("DELETE FROM coach WHERE id = ?", coach_id).execute(self.pool()).await?;
        tracing::info!(coach_id, "coach removed");
        Ok(())
    }

    /// Sets the member fields an admin is allowed to change.
    ///
    /// Discord ID and username are omitted deliberately: they come from OAuth
    /// and editing them would break the owner's login.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn set_member_fields(
        &self,
        person_id: i64,
        showdown_username: Option<&str>,
        is_admin: bool,
    ) -> Result<(), sqlx::Error> {
        let showdown = showdown_username.map(str::trim).filter(|s| !s.is_empty());
        sqlx::query!(
            "UPDATE person SET showdown_username = ?, is_admin = ? WHERE id = ?",
            showdown,
            is_admin,
            person_id
        )
        .execute(self.pool())
        .await?;
        tracing::info!(person_id, is_admin, "member fields updated");
        Ok(())
    }

    /// Sets a person's own Showdown username.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn set_showdown_username(
        &self,
        person_id: i64,
        showdown_username: &str,
    ) -> Result<(), sqlx::Error> {
        let showdown = Some(showdown_username.trim()).filter(|s| !s.is_empty());
        sqlx::query!(
            "UPDATE person SET showdown_username = ? WHERE id = ?",
            showdown,
            person_id
        )
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

/// Every coach in a season, on any connection. `Db::coaches` wraps this.
pub(crate) async fn season_coaches<'e>(
    ex: impl sqlx::SqliteExecutor<'e>,
    season_id: i64,
) -> Result<Vec<Coach>, sqlx::Error> {
    sqlx::query_as!(
        Coach,
        r#"SELECT c.id, c.person_id, c.budget, c.draft_position, c.done_at, c.team_name,
                  p.discord_id, p.discord_username, p.showdown_username,
                  p.is_admin AS "is_admin!: bool",
                  COALESCE((SELECT SUM(ct.points) FROM roster_entry re
                            JOIN cost ct ON ct.season_id = re.season_id
                                        AND ct.pokemon_id = re.pokemon_id
                            WHERE re.coach_id = c.id AND re.until_week IS NULL), 0)
                      AS "spent!: i64"
           FROM coach c JOIN person p ON p.id = c.person_id
           WHERE c.season_id = ?
           ORDER BY c.draft_position"#,
        season_id
    )
    .fetch_all(ex)
    .await
}
