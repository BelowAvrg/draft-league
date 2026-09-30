//! SQLite-backed session store.
//!
//! The published `tower-sessions-sqlx-store` is pinned to sqlx 0.8 and
//! tower-sessions-core 0.14; this crate is on 0.9 and 0.15, which makes its
//! store type unusable here. The trait is three methods over one table.

use async_trait::async_trait;
use sqlx::SqlitePool;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tower_sessions::session::{Id, Record};
use tower_sessions::session_store::{Error, Result};

/// Stores sessions in the application's SQLite database.
#[derive(Debug, Clone)]
pub struct SqliteStore {
    pool: SqlitePool,
}

/// `Id` is an i128, which SQLite cannot hold natively; hex text sorts and
/// compares exactly.
fn id_text(id: &Id) -> String {
    format!("{:032x}", id.0.cast_unsigned())
}

fn backend<E: std::fmt::Display>(e: E) -> Error {
    Error::Backend(e.to_string())
}

fn encode<E: std::fmt::Display>(e: E) -> Error {
    Error::Encode(e.to_string())
}

fn decode<E: std::fmt::Display>(e: E) -> Error {
    Error::Decode(e.to_string())
}

impl SqliteStore {
    /// Builds a store over an existing pool.
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Removes sessions whose expiry has passed.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn delete_expired(&self) -> Result<u64> {
        let now = OffsetDateTime::now_utc().format(&Rfc3339).map_err(encode)?;
        let r = sqlx::query!("DELETE FROM session WHERE expiry_date < ?", now)
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(r.rows_affected())
    }
}

#[async_trait]
impl tower_sessions::SessionStore for SqliteStore {
    async fn create(&self, session_record: &mut Record) -> Result<()> {
        // Retry on id collision rather than overwriting a live session.
        loop {
            let id = id_text(&session_record.id);
            let data = serde_json::to_vec(&session_record.data).map_err(encode)?;
            let expiry = session_record.expiry_date.format(&Rfc3339).map_err(encode)?;
            let res = sqlx::query!(
                "INSERT INTO session (id, data, expiry_date) VALUES (?, ?, ?)
                 ON CONFLICT (id) DO NOTHING",
                id,
                data,
                expiry
            )
            .execute(&self.pool)
            .await
            .map_err(backend)?;

            if res.rows_affected() > 0 {
                return Ok(());
            }
            session_record.id = Id::default();
        }
    }

    async fn save(&self, session_record: &Record) -> Result<()> {
        let id = id_text(&session_record.id);
        let data = serde_json::to_vec(&session_record.data).map_err(encode)?;
        let expiry = session_record.expiry_date.format(&Rfc3339).map_err(encode)?;
        sqlx::query!(
            "INSERT INTO session (id, data, expiry_date) VALUES (?, ?, ?)
             ON CONFLICT (id) DO UPDATE SET data = excluded.data, expiry_date = excluded.expiry_date",
            id,
            data,
            expiry
        )
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(())
    }

    async fn load(&self, session_id: &Id) -> Result<Option<Record>> {
        let id = id_text(session_id);
        let now = OffsetDateTime::now_utc().format(&Rfc3339).map_err(encode)?;
        let row = sqlx::query!(
            "SELECT data, expiry_date FROM session WHERE id = ? AND expiry_date >= ?",
            id,
            now
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?;

        let Some(row) = row else { return Ok(None) };
        Ok(Some(Record {
            id: *session_id,
            data: serde_json::from_slice(&row.data).map_err(decode)?,
            expiry_date: OffsetDateTime::parse(&row.expiry_date, &Rfc3339).map_err(decode)?,
        }))
    }

    async fn delete(&self, session_id: &Id) -> Result<()> {
        let id = id_text(session_id);
        sqlx::query!("DELETE FROM session WHERE id = ?", id)
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(())
    }
}
