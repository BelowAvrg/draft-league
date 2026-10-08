//! Match-time proposals and their lifecycle.

use sqlx::SqliteConnection;
use time::OffsetDateTime;

use crate::db::{Db, Person};
use crate::results::{ResultError, Scheduled, write_schedule};

/// One concrete time in a proposal.
#[derive(Debug, Clone)]
pub struct TimeOption {
    pub id: i64,
    /// UTC as an RFC 3339 value for templates and browser conversion.
    pub starts_at: String,
    pub accepted: bool,
}

/// The currently pending time proposal for a match.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub id: i64,
    pub match_id: i64,
    pub proposed_by_person_id: i64,
    pub proposed_by: String,
    pub status: String,
    pub created_at: String,
    pub options: Vec<TimeOption>,
}

/// A proposal notification addressed to the other coach.
#[derive(Debug, Clone, Copy)]
pub struct Proposed {
    pub match_id: i64,
    pub from_coach: i64,
    pub to_coach: i64,
    pub options: usize,
}

/// One match with an open proposal involving a person.
#[derive(Debug, Clone, Copy)]
pub struct Pending {
    pub match_id: i64,
    pub proposed_by_person_id: i64,
}

/// Why a time proposal action was refused.
#[derive(Debug, thiserror::Error)]
pub enum ProposalError {
    #[error("no such match or proposal")]
    NotFound,
    #[error("this playoff match has no teams yet")]
    Unseeded,
    #[error("only the two coaches in this match may do that")]
    NotParticipant,
    #[error("only the other coach may respond to this proposal")]
    NotRecipient,
    #[error("only the coach who proposed these times may withdraw them")]
    NotProposer,
    #[error("this match already has a pending time proposal")]
    AlreadyOpen,
    #[error("suggest at least one time")]
    Empty,
    #[error("suggested times must be future 30-minute blocks")]
    InvalidTime,
    #[error("that proposed time is no longer available")]
    Stale,
    #[error(transparent)]
    Result(#[from] ResultError),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

struct Parties {
    coach_a: i64,
    coach_b: i64,
    person_a: i64,
    person_b: i64,
}

impl Parties {
    fn coaches_for(&self, person_id: i64) -> Option<(i64, i64)> {
        if person_id == self.person_a {
            Some((self.coach_a, self.coach_b))
        } else if person_id == self.person_b {
            Some((self.coach_b, self.coach_a))
        } else {
            None
        }
    }

    fn recipient(&self, proposer: i64) -> Option<i64> {
        if proposer == self.person_a {
            Some(self.person_b)
        } else if proposer == self.person_b {
            Some(self.person_a)
        } else {
            None
        }
    }
}

impl Db {
    /// Open proposals in a season involving this person.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn pending_time_proposals(
        &self,
        season_id: i64,
        person_id: i64,
    ) -> Result<Vec<Pending>, sqlx::Error> {
        sqlx::query_as!(
            Pending,
            "SELECT p.match_id, p.proposed_by_person_id
             FROM match_time_proposal p
             JOIN match m ON m.id = p.match_id
             JOIN coach ca ON ca.id = m.coach_a_id
             JOIN coach cb ON cb.id = m.coach_b_id
             WHERE m.season_id = ? AND p.status = 'open'
               AND ? IN (ca.person_id, cb.person_id)",
            season_id,
            person_id
        )
        .fetch_all(self.pool())
        .await
    }

    /// Reads the open proposal for a match, if one exists.
    ///
    /// Callers decide whether its private contents may be shown.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn open_time_proposal(&self, match_id: i64) -> Result<Option<Proposal>, sqlx::Error> {
        let Some(row) = sqlx::query!(
            "SELECT p.id, p.match_id, p.proposed_by_person_id,
                    pe.discord_username AS proposed_by, p.status, p.created_at
             FROM match_time_proposal p
             JOIN person pe ON pe.id = p.proposed_by_person_id
             WHERE p.match_id = ? AND p.status = 'open'",
            match_id
        )
        .fetch_optional(self.pool())
        .await?
        else {
            return Ok(None);
        };
        let options = sqlx::query_as!(
            TimeOption,
            r#"SELECT id, strftime('%Y-%m-%dT%H:%M:%SZ', starts_at) AS "starts_at!: String",
                      accepted AS "accepted!: bool"
               FROM match_time_option WHERE proposal_id = ? ORDER BY starts_at"#,
            row.id
        )
        .fetch_all(self.pool())
        .await?;
        Ok(Some(Proposal {
            id: row.id,
            match_id: row.match_id,
            proposed_by_person_id: row.proposed_by_person_id,
            proposed_by: row.proposed_by,
            status: row.status,
            created_at: row.created_at,
            options,
        }))
    }

    /// Offers one or more future half-hour times to the other coach.
    ///
    /// # Errors
    /// Refuses outsiders, unseeded matches, empty or malformed times, and a
    /// match that already has a pending proposal.
    pub async fn propose_times(
        &self,
        match_id: i64,
        by: &Person,
        times: &[OffsetDateTime],
    ) -> Result<Proposed, ProposalError> {
        if times.is_empty() {
            return Err(ProposalError::Empty);
        }
        let now = OffsetDateTime::now_utc();
        if times.iter().any(|at| {
            *at <= now || at.minute() % 30 != 0 || at.second() != 0 || at.nanosecond() != 0
        }) {
            return Err(ProposalError::InvalidTime);
        }
        let mut unix: Vec<i64> = times.iter().map(|at| at.unix_timestamp()).collect();
        let before = unix.len();
        unix.sort_unstable();
        unix.dedup();
        if unix.len() != before {
            return Err(ProposalError::InvalidTime);
        }

        let mut tx = self.pool().begin().await?;
        let parties = parties(&mut tx, match_id).await?;
        let (from_coach, to_coach) = parties
            .coaches_for(by.id)
            .ok_or(ProposalError::NotParticipant)?;
        let proposal_id = match sqlx::query_scalar!(
            "INSERT INTO match_time_proposal (match_id, proposed_by_person_id)
             VALUES (?, ?) RETURNING id",
            match_id,
            by.id
        )
        .fetch_one(&mut *tx)
        .await
        {
            Ok(id) => id,
            Err(e) if unique_violation(&e) => return Err(ProposalError::AlreadyOpen),
            Err(e) => return Err(e.into()),
        };
        for at in unix {
            sqlx::query!(
                "INSERT INTO match_time_option (proposal_id, starts_at)
                 VALUES (?, datetime(?, 'unixepoch'))",
                proposal_id,
                at
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        tracing::info!(
            proposal_id,
            match_id,
            by = by.id,
            options = times.len(),
            "match times proposed"
        );
        Ok(Proposed {
            match_id,
            from_coach,
            to_coach,
            options: times.len(),
        })
    }

    /// Accepts one option as the match's scheduled time.
    ///
    /// # Errors
    /// Refuses anyone other than the recipient and stale proposal options.
    pub async fn accept_time_proposal(
        &self,
        match_id: i64,
        proposal_id: i64,
        option_id: i64,
        by: &Person,
    ) -> Result<Scheduled, ProposalError> {
        let mut tx = self.pool().begin().await?;
        let parties = parties(&mut tx, match_id).await?;
        let proposal = sqlx::query!(
            "SELECT proposed_by_person_id FROM match_time_proposal
             WHERE id = ? AND match_id = ? AND status = 'open'",
            proposal_id,
            match_id
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ProposalError::Stale)?;
        if parties.recipient(proposal.proposed_by_person_id) != Some(by.id) {
            return Err(ProposalError::NotRecipient);
        }
        let unix = sqlx::query_scalar!(
            r#"SELECT unixepoch(starts_at) AS "at!: i64" FROM match_time_option
               WHERE id = ? AND proposal_id = ?"#,
            option_id,
            proposal_id
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ProposalError::Stale)?;
        let at = OffsetDateTime::from_unix_timestamp(unix)
            .map_err(|_range| ProposalError::InvalidTime)?;
        if at <= OffsetDateTime::now_utc() {
            return Err(ProposalError::Stale);
        }
        sqlx::query!(
            "UPDATE match_time_option SET accepted = 1 WHERE id = ?",
            option_id
        )
        .execute(&mut *tx)
        .await?;
        let scheduled = write_schedule(&mut tx, match_id, by, Some(at), Some(proposal_id)).await?;
        tx.commit().await?;
        tracing::info!(
            proposal_id,
            option_id,
            match_id,
            by = by.id,
            "match time accepted"
        );
        Ok(scheduled)
    }

    /// Declines every option in an open proposal.
    ///
    /// # Errors
    /// Refuses anyone other than the recipient.
    pub async fn decline_time_proposal(
        &self,
        match_id: i64,
        proposal_id: i64,
        by: &Person,
    ) -> Result<(), ProposalError> {
        self.close_time_proposal(match_id, proposal_id, by, "declined", false)
            .await
    }

    /// Withdraws an open proposal.
    ///
    /// # Errors
    /// Refuses anyone other than the proposer.
    pub async fn withdraw_time_proposal(
        &self,
        match_id: i64,
        proposal_id: i64,
        by: &Person,
    ) -> Result<(), ProposalError> {
        self.close_time_proposal(match_id, proposal_id, by, "withdrawn", true)
            .await
    }

    async fn close_time_proposal(
        &self,
        match_id: i64,
        proposal_id: i64,
        by: &Person,
        status: &str,
        proposer_only: bool,
    ) -> Result<(), ProposalError> {
        let mut tx = self.pool().begin().await?;
        let parties = parties(&mut tx, match_id).await?;
        let proposal = sqlx::query!(
            "SELECT proposed_by_person_id FROM match_time_proposal
             WHERE id = ? AND match_id = ? AND status = 'open'",
            proposal_id,
            match_id
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ProposalError::Stale)?;
        let allowed = if proposer_only {
            proposal.proposed_by_person_id == by.id
        } else {
            parties.recipient(proposal.proposed_by_person_id) == Some(by.id)
        };
        if !allowed {
            return Err(if proposer_only {
                ProposalError::NotProposer
            } else {
                ProposalError::NotRecipient
            });
        }
        sqlx::query!(
            "UPDATE match_time_proposal SET status = ?, decided_by_person_id = ?,
                    decided_at = datetime('now') WHERE id = ?",
            status,
            by.id,
            proposal_id
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        tracing::info!(
            proposal_id,
            match_id,
            by = by.id,
            status,
            "match time proposal closed"
        );
        Ok(())
    }
}

async fn parties(conn: &mut SqliteConnection, match_id: i64) -> Result<Parties, ProposalError> {
    let row = sqlx::query!(
        "SELECT m.coach_a_id, m.coach_b_id,
                ca.person_id AS person_a, cb.person_id AS person_b
         FROM match m
         LEFT JOIN coach ca ON ca.id = m.coach_a_id
         LEFT JOIN coach cb ON cb.id = m.coach_b_id
         WHERE m.id = ?",
        match_id
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(ProposalError::NotFound)?;
    let (Some(coach_a), Some(coach_b), Some(person_a), Some(person_b)) =
        (row.coach_a_id, row.coach_b_id, row.person_a, row.person_b)
    else {
        return Err(ProposalError::Unseeded);
    };
    Ok(Parties {
        coach_a,
        coach_b,
        person_a,
        person_b,
    })
}

fn unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(e) if e.is_unique_violation())
}
