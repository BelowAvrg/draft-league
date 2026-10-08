//! Recurring weekly availability in each member's local time.

use crate::db::Db;

/// One half-hour block in a Sunday-first week.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Slot {
    /// Sunday is 0 and Saturday is 6.
    pub weekday: i64,
    /// Half-hour block from midnight, from 0 through 47.
    pub slot: i64,
}

impl Slot {
    /// Builds a valid weekly slot.
    ///
    /// # Errors
    /// Fails unless the weekday is 0-6 and the slot is 0-47.
    pub fn new(weekday: i64, slot: i64) -> Result<Self, AvailabilityError> {
        if !(0..=6).contains(&weekday) || !(0..=47).contains(&slot) {
            return Err(AvailabilityError::InvalidSlot { weekday, slot });
        }
        Ok(Self { weekday, slot })
    }
}

/// Public availability for one member.
#[derive(Debug, Clone)]
pub struct Availability {
    pub person_id: i64,
    pub name: String,
    pub time_zone: Option<String>,
    pub slots: Vec<Slot>,
}

/// Why availability could not be saved or read.
#[derive(Debug, thiserror::Error)]
pub enum AvailabilityError {
    #[error("{0:?} is not a valid time zone name")]
    InvalidTimeZone(String),
    #[error("weekday {weekday}, slot {slot} is not a valid availability block")]
    InvalidSlot { weekday: i64, slot: i64 },
    #[error("no such coach")]
    NoCoach,
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

/// Performs conservative shape validation for an IANA time-zone identifier.
///
/// The browser verifies that the identifier is understood before submitting;
/// this check keeps malformed values out of markup and storage.
#[must_use]
pub fn valid_time_zone(zone: &str) -> bool {
    let zone = zone.trim();
    !zone.is_empty()
        && zone.len() <= 64
        && zone
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+'))
        && !zone.starts_with('/')
        && !zone.ends_with('/')
        && !zone.contains("//")
}

impl Db {
    /// Reads one person's public weekly availability.
    ///
    /// # Errors
    /// Fails on database error or when the person does not exist.
    pub async fn availability(&self, person_id: i64) -> Result<Availability, AvailabilityError> {
        let person = sqlx::query!(
            "SELECT discord_username, time_zone FROM person WHERE id = ?",
            person_id
        )
        .fetch_optional(self.pool())
        .await?
        .ok_or(AvailabilityError::NoCoach)?;
        let slots = sqlx::query_as!(
            Slot,
            "SELECT weekday, slot FROM person_availability
             WHERE person_id = ? ORDER BY weekday, slot",
            person_id
        )
        .fetch_all(self.pool())
        .await?;
        Ok(Availability {
            person_id,
            name: person.discord_username,
            time_zone: person.time_zone,
            slots,
        })
    }

    /// Reads public availability through a coach in the active season.
    ///
    /// # Errors
    /// Fails on database error or when the coach does not exist.
    pub async fn coach_availability(
        &self,
        coach_id: i64,
    ) -> Result<Availability, AvailabilityError> {
        let person_id = sqlx::query_scalar!("SELECT person_id FROM coach WHERE id = ?", coach_id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(AvailabilityError::NoCoach)?;
        self.availability(person_id).await
    }

    /// Replaces one person's timezone and recurring weekly availability.
    ///
    /// # Errors
    /// Refuses malformed timezone or slot values and fails on database error.
    pub async fn set_availability(
        &self,
        person_id: i64,
        time_zone: &str,
        slots: &[Slot],
    ) -> Result<(), AvailabilityError> {
        let time_zone = time_zone.trim();
        if !valid_time_zone(time_zone) {
            return Err(AvailabilityError::InvalidTimeZone(time_zone.to_owned()));
        }
        for slot in slots {
            Slot::new(slot.weekday, slot.slot)?;
        }

        let mut unique = slots.to_vec();
        unique.sort_unstable();
        unique.dedup();

        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query!(
            "UPDATE person SET time_zone = ? WHERE id = ?",
            time_zone,
            person_id
        )
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            return Err(AvailabilityError::NoCoach);
        }
        sqlx::query!(
            "DELETE FROM person_availability WHERE person_id = ?",
            person_id
        )
        .execute(&mut *tx)
        .await?;
        for slot in unique {
            sqlx::query!(
                "INSERT INTO person_availability (person_id, weekday, slot) VALUES (?, ?, ?)",
                person_id,
                slot.weekday,
                slot.slot
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        tracing::info!(
            person_id,
            slots = slots.len(),
            time_zone,
            "availability saved"
        );
        Ok(())
    }
}
