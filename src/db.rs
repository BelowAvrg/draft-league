//! Database pool and person queries.

use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

/// A league member: the person, not their participation in a season.
#[derive(Debug, Clone)]
pub struct Person {
    pub id: i64,
    pub discord_id: String,
    pub discord_username: String,
    pub showdown_username: Option<String>,
    pub is_admin: bool,
}

/// The active season, with how much of its tier list is filled in.
#[derive(Debug, Clone)]
pub struct Season {
    pub id: i64,
    pub name: String,
    /// Fewest Pokémon a coach must draft.
    pub min_roster: i64,
    /// Most Pokémon a coach may draft.
    pub max_roster: i64,
    /// Number of pokemon priced for this season.
    pub priced: i64,
}

/// Shared handle to the SQLite database. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Db {
    pool: SqlitePool,
}

impl Db {
    /// Opens the database at `url` and runs pending migrations.
    ///
    /// # Errors
    /// Fails if the database cannot be opened or a migration fails.
    pub async fn connect(url: &str) -> Result<Self, sqlx::Error> {
        let pool = SqlitePoolOptions::new().max_connections(5).connect(url).await?;
        sqlx::query("PRAGMA foreign_keys = ON").execute(&pool).await?;
        sqlx::migrate!().run(&pool).await?;
        Ok(Self { pool })
    }

    /// The underlying pool, for the session store.
    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Finds the person with this Discord ID, if they have logged in before.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn person_by_discord_id(&self, discord_id: &str) -> Result<Option<Person>, sqlx::Error> {
        sqlx::query_as!(
            Person,
            r#"SELECT id, discord_id, discord_username, showdown_username,
                      is_admin AS "is_admin!: bool"
               FROM person WHERE discord_id = ?"#,
            discord_id
        )
        .fetch_optional(&self.pool)
        .await
    }

    /// Looks up a person by id.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn person(&self, id: i64) -> Result<Option<Person>, sqlx::Error> {
        sqlx::query_as!(
            Person,
            r#"SELECT id, discord_id, discord_username, showdown_username,
                      is_admin AS "is_admin!: bool"
               FROM person WHERE id = ?"#,
            id
        )
        .fetch_optional(&self.pool)
        .await
    }

    /// A Pokémon's display name.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn pokemon_name(&self, id: i64) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar!("SELECT display_name FROM pokemon WHERE id = ?", id)
            .fetch_optional(&self.pool)
            .await
    }

    /// The active season, if one is set up.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn active_season(&self) -> Result<Option<Season>, sqlx::Error> {
        sqlx::query_as!(
            Season,
            r#"SELECT s.id, s.name, s.min_roster, s.max_roster,
                      (SELECT COUNT(*) FROM cost WHERE season_id = s.id) AS "priced!: i64"
               FROM season s WHERE s.is_active = 1"#
        )
        .fetch_optional(&self.pool)
        .await
    }

    /// Records a Discord login, creating the person on first sight.
    ///
    /// The very first person to log in becomes an admin, since someone has to
    /// be able to set up the season. Later logins refresh the cached username.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn upsert_discord_person(
        &self,
        discord_id: &str,
        discord_username: &str,
    ) -> Result<Person, sqlx::Error> {
        let mut tx = self.pool.begin().await?;

        let is_first: bool = sqlx::query_scalar!(r#"SELECT NOT EXISTS(SELECT 1 FROM person) AS "e!: bool""#)
            .fetch_one(&mut *tx)
            .await?;

        let person = sqlx::query_as!(
            Person,
            r#"INSERT INTO person (discord_id, discord_username, is_admin)
               VALUES (?, ?, ?)
               ON CONFLICT (discord_id) DO UPDATE SET discord_username = excluded.discord_username
               RETURNING id, discord_id, discord_username, showdown_username,
                         is_admin AS "is_admin!: bool""#,
            discord_id,
            discord_username,
            is_first
        )
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(person)
    }
}
