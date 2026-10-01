//! Recording picks and finishing early.

use crate::db::{Db, Season};
use crate::draft::{self, PickError, Pool, Roster, Seat, Standing, Turn};

/// A completed pick, as the draft board shows it.
#[derive(Debug, Clone)]
pub struct Pick {
    pub pick_number: i64,
    pub coach_id: i64,
    pub discord_username: String,
    pub slug: String,
    pub display_name: String,
    pub points_paid: i64,
    /// Space-separated, primary type first.
    pub types: String,
}

/// A pick a draft action just recorded, as the Discord post lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drafted {
    /// Position in the whole season's draft, 1-based. Not the coach's own count.
    pub number: i64,
    pub discord_id: String,
    pub pokemon: String,
    pub points: i64,
}

/// One draftable Pokémon with this season's price and whether it is taken.
#[derive(Debug, Clone)]
pub struct Listing {
    pub pokemon_id: i64,
    pub slug: String,
    pub display_name: String,
    pub points: i64,
    /// Space-separated, primary type first.
    pub types: String,
    /// The coach who owns it, if anyone does. Pending moves count.
    pub taken_by: Option<String>,
    /// That owner's coach id.
    pub owner_id: Option<i64>,
}

/// Ceiling on one cascade of queue auto-picks.
///
/// A cascade cannot exceed the picks the draft has left, so this only ever
/// fires on a bug. It is the largest draft this league could run: the maximum
/// roster times a generous coach count.
const MAX_AUTO_PICKS: i32 = 12 * 32;

/// One of a coach's queued slots. Visible only to that coach.
#[derive(Debug, Clone)]
pub struct Queued {
    /// The pick number this slot binds to.
    pub slot_number: i64,
    pub pokemon_id: i64,
    pub slug: String,
    pub display_name: String,
    /// This season's price, as a hint; the pick pays the price at pick time.
    pub points: i64,
}

/// Why a queue slot could not be set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum QueueError {
    #[error("pick {slot} has already happened; you can queue from pick {next} on")]
    SlotPassed { slot: i64, next: i64 },
    #[error("pick {slot} is past the {max} Pokémon maximum")]
    PastMaximum { slot: i64, max: i64 },
    #[error("you already have that queued for pick {slot}")]
    AlreadyQueued { slot: i64 },
}

/// Why a pick or a finish could not be recorded.
#[derive(Debug, thiserror::Error)]
pub enum DraftError {
    #[error("no season is active")]
    NoSeason,
    #[error("the season has no tier list yet")]
    NoTierList,
    #[error("you are not a coach this season")]
    NotACoach,
    #[error("that is no longer the latest pick; reload and try again")]
    NotLatestPick,
    #[error(transparent)]
    Pick(#[from] PickError),
    #[error(transparent)]
    Queue(#[from] QueueError),
    #[error(transparent)]
    Done(#[from] draft::DoneError),
    #[error(transparent)]
    Roster(#[from] draft::RosterError),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

/// A snapshot of the active season's draft, enough to answer every rule.
#[derive(Debug, Clone)]
pub struct Board {
    pub season: Season,
    pub roster: Roster,
    pub pool: Pool,
    pub seats: Vec<Seat>,
    pub turn: Turn,
}

impl Board {
    /// The standing of one seat.
    #[must_use]
    pub fn standing(&self, seat: &Seat) -> Standing {
        draft::standing(seat, self.roster, &self.pool)
    }

    /// The seat belonging to a person's coach row, if they coach this season.
    #[must_use]
    pub fn seat_of(&self, coach_id: i64) -> Option<&Seat> {
        self.seats.iter().find(|s| s.coach_id == coach_id)
    }
}

impl Db {
    /// Loads the active season's draft state.
    ///
    /// # Errors
    /// Fails when no season is active, or on database error.
    pub async fn board(&self) -> Result<Board, DraftError> {
        let season = self.active_season().await?.ok_or(DraftError::NoSeason)?;
        let roster = Roster::new(season.min_roster, season.max_roster)?;

        let seats = sqlx::query!(
            r#"SELECT c.id, c.draft_position, c.done_at,
                      (SELECT COUNT(*) FROM pick WHERE coach_id = c.id) AS "picks!: i64",
                      c.budget - COALESCE(
                          (SELECT SUM(points_paid) FROM pick WHERE coach_id = c.id), 0)
                          AS "remaining!: i64"
               FROM coach c WHERE c.season_id = ?
               ORDER BY c.draft_position"#,
            season.id
        )
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(|r| Seat {
            coach_id: r.id,
            draft_position: r.draft_position,
            picks: r.picks,
            remaining: r.remaining,
            done: r.done_at.is_some(),
        })
        .collect::<Vec<_>>();

        let pool = Pool::new(self.available_costs(season.id).await?);
        let turn = draft::whose_turn(&seats, roster, &pool);
        Ok(Board { season, roster, pool, seats, turn })
    }

    /// Costs of every priced Pokémon not yet drafted this season.
    async fn available_costs(&self, season_id: i64) -> Result<Vec<i64>, sqlx::Error> {
        sqlx::query_scalar!(
            r#"SELECT points AS "points!: i64" FROM cost
               WHERE season_id = ?
                 AND pokemon_id NOT IN (SELECT pokemon_id FROM pick WHERE season_id = ?)"#,
            season_id,
            season_id
        )
        .fetch_all(self.pool())
        .await
    }

    /// The coach row for a person in the active season.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn coach_of(&self, person_id: i64, season_id: i64) -> Result<Option<i64>, sqlx::Error> {
        sqlx::query_scalar!(
            "SELECT id FROM coach WHERE person_id = ? AND season_id = ?",
            person_id,
            season_id
        )
        .fetch_optional(self.pool())
        .await
    }

    /// Records a pick for a coach, then any queue auto-picks it sets off.
    ///
    /// Returns every pick recorded, in order.
    ///
    /// Re-reads the turn and the budget inside the transaction, so two tabs
    /// cannot both spend the same points. The unique constraints on `pick`
    /// settle a race for the same Pokémon.
    ///
    /// # Errors
    /// Refuses a pick out of turn, over budget, breaking the reserve, on a
    /// full or finished roster, or for an unpriced or already-drafted Pokémon.
    pub async fn make_pick(&self, coach_id: i64, pokemon_id: i64) -> Result<Vec<Drafted>, DraftError> {
        let board = self.board().await?;
        let seat = *board.seat_of(coach_id).ok_or(DraftError::NotACoach)?;

        if board.turn.coach_id != Some(coach_id) {
            return Err(PickError::NotYourTurn.into());
        }
        if board.pool.is_empty() {
            return Err(DraftError::NoTierList);
        }

        let first = self.record_pick(&board, &seat, pokemon_id).await?;

        // This pick may have put a coach with a ready queue slot on the clock.
        let mut drafted = vec![first];
        drafted.extend(self.run_queue().await?);
        Ok(drafted)
    }

    /// Writes one validated pick and breaks any queue bindings it kills.
    ///
    /// Takes the board the caller already validated against; the cost, the
    /// taken check, and both budget rules are re-read inside the transaction
    /// so two tabs cannot both spend the same points.
    async fn record_pick(
        &self,
        board: &Board,
        seat: &Seat,
        pokemon_id: i64,
    ) -> Result<Drafted, DraftError> {
        let season_id = board.season.id;
        let coach_id = seat.coach_id;
        let mut tx = self.pool().begin().await?;

        let cost: Option<i64> = sqlx::query_scalar!(
            r#"SELECT points AS "points!: i64" FROM cost
               WHERE season_id = ? AND pokemon_id = ?"#,
            season_id,
            pokemon_id
        )
        .fetch_optional(&mut *tx)
        .await?;
        let cost = cost.ok_or(PickError::Unpriced)?;

        let taken: bool = sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT 1 FROM pick WHERE season_id = ? AND pokemon_id = ?)
                   AS "e!: bool""#,
            season_id,
            pokemon_id
        )
        .fetch_one(&mut *tx)
        .await?;
        if taken {
            return Err(PickError::Taken.into());
        }

        draft::validate_pick(seat, cost, board.roster, &board.pool)?;

        let pick_number = seat.picks + 1;
        sqlx::query!(
            "INSERT INTO pick (coach_id, season_id, pokemon_id, pick_number, points_paid)
             VALUES (?, ?, ?, ?, ?)",
            coach_id,
            season_id,
            pokemon_id,
            pick_number,
            cost
        )
        .execute(&mut *tx)
        .await?;

        // The slot that just spent itself, plus any slot in another coach's
        // queue holding the same Pokémon: that binding is dead, and it breaks
        // rather than promoting the next slot.
        sqlx::query!("DELETE FROM queue_slot WHERE pokemon_id = ?", pokemon_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query!(
            "DELETE FROM queue_slot WHERE coach_id = ? AND slot_number = ?",
            coach_id,
            pick_number
        )
        .execute(&mut *tx)
        .await?;

        // Only the latest pick can be undone, so the count is this pick's place.
        let drafted = sqlx::query!(
            r#"SELECT (SELECT COUNT(*) FROM pick WHERE season_id = ?) AS "number!: i64",
                      p.discord_id, m.display_name
               FROM coach c JOIN person p ON p.id = c.person_id, pokemon m
               WHERE c.id = ? AND m.id = ?"#,
            season_id,
            coach_id,
            pokemon_id
        )
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;
        tracing::info!(coach_id, pokemon_id, pick_number, cost, "pick recorded");
        Ok(Drafted {
            number: drafted.number,
            discord_id: drafted.discord_id,
            pokemon: drafted.display_name,
            points: cost,
        })
    }

    /// Auto-picks for whoever is on the clock, as long as their slot resolves.
    ///
    /// Positional: the coach on the clock for pick N is auto-picked only from
    /// *their* slot N. An empty or sniped slot stalls the draft rather than
    /// promoting slot N+1. Each auto-pick can put the next coach on the clock
    /// with their own ready slot, so this loops until one stalls.
    ///
    /// A slot that no longer validates -- unaffordable past the minimum, or
    /// breaking the reserve -- also stalls, leaving the coach to pick by hand.
    ///
    /// Returns the picks it made, in order.
    ///
    /// # Errors
    /// Fails on database error. A slot that cannot be picked is not an error.
    async fn run_queue(&self) -> Result<Vec<Drafted>, DraftError> {
        // Bounded by the picks the draft has left, so a slot that somehow
        // fails to clear cannot spin forever.
        let mut guard = 0;
        let mut drafted = Vec::new();
        loop {
            let board = self.board().await?;
            let Some(coach_id) = board.turn.coach_id else { return Ok(drafted) };
            let Some(seat) = board.seat_of(coach_id).copied() else { return Ok(drafted) };

            guard += 1;
            if guard > MAX_AUTO_PICKS {
                tracing::error!(coach_id, "queue auto-pick did not settle; stopping");
                return Ok(drafted);
            }

            // Slot N binds to pick N, and nothing else.
            let slot = seat.picks + 1;
            let Some(pokemon_id) = self.queued_at(coach_id, slot).await? else { return Ok(drafted) };

            match self.record_pick(&board, &seat, pokemon_id).await {
                Ok(d) => {
                    tracing::info!(coach_id, slot, pokemon_id, "queue slot auto-picked");
                    drafted.push(d);
                }
                Err(DraftError::Sqlx(e)) => return Err(e.into()),
                Err(e) => {
                    // The slot is unpickable now but may become pickable again
                    // (an admin correction frees points or Pokémon), so it
                    // stays put and the coach is simply left on the clock.
                    tracing::info!(coach_id, slot, %e, "queue slot stalled the draft");
                    return Ok(drafted);
                }
            }
        }
    }

    /// The Pokémon a coach has queued for one slot, if any.
    ///
    /// # Errors
    /// Fails on database error.
    async fn queued_at(&self, coach_id: i64, slot_number: i64) -> Result<Option<i64>, sqlx::Error> {
        sqlx::query_scalar!(
            "SELECT pokemon_id FROM queue_slot WHERE coach_id = ? AND slot_number = ?",
            coach_id,
            slot_number
        )
        .fetch_optional(self.pool())
        .await
    }

    /// Marks a coach as finished drafting, then runs the queue.
    ///
    /// Returns the coach's pick count and any auto-picks the queue made.
    ///
    /// # Errors
    /// Refuses a roster below the season minimum, or one already finished.
    pub async fn finish_drafting(&self, coach_id: i64) -> Result<(i64, Vec<Drafted>), DraftError> {
        let board = self.board().await?;
        let seat = board.seat_of(coach_id).ok_or(DraftError::NotACoach)?;
        draft::validate_done(seat, board.roster)?;

        sqlx::query!(
            "UPDATE coach SET done_at = datetime('now') WHERE id = ? AND done_at IS NULL",
            coach_id
        )
        .execute(self.pool())
        .await?;
        tracing::info!(coach_id, picks = seat.picks, "coach finished drafting");
        // The next coach on the clock may have a ready slot.
        Ok((seat.picks, self.run_queue().await?))
    }

    /// Clears a coach's finished flag, then runs the queue. Admin correction only.
    ///
    /// Returns any auto-picks the queue made.
    ///
    /// # Errors
    /// Fails on database error, or when no season is active.
    pub async fn reopen_drafting(&self, coach_id: i64) -> Result<Vec<Drafted>, DraftError> {
        sqlx::query!("UPDATE coach SET done_at = NULL WHERE id = ?", coach_id)
            .execute(self.pool())
            .await?;
        tracing::info!(coach_id, "coach reopened for drafting");
        // The reopened coach may be back on the clock with a ready slot.
        self.run_queue().await
    }

    /// Undoes the season's latest pick. Admin correction only.
    ///
    /// Only the latest pick can go, so nothing after it needs unwinding: the
    /// Pokémon returns to the pool, the points return to the coach, and the
    /// turn returns to them. Older mistakes are reached by undoing repeatedly.
    /// The pick is copied into `pick_correction` before it is deleted.
    ///
    /// A coach who finished early and drops below the minimum is reopened, since
    /// finishing is only allowed at the minimum. Queue slots broken by the pick
    /// stay broken. The queue runs afterwards like any other draft action.
    ///
    /// Returns the undone pick and any auto-picks the queue made.
    ///
    /// # Errors
    /// Refuses when `(coach_id, pick_number)` is not the season's latest pick,
    /// so a stale page cannot undo a second pick.
    pub async fn undo_pick(
        &self,
        coach_id: i64,
        pick_number: i64,
        admin_id: i64,
    ) -> Result<(Drafted, Vec<Drafted>), DraftError> {
        let season = self.active_season().await?.ok_or(DraftError::NoSeason)?;
        let mut tx = self.pool().begin().await?;

        let last = sqlx::query!(
            "SELECT id, coach_id, pokemon_id, pick_number FROM pick
             WHERE season_id = ? ORDER BY id DESC LIMIT 1",
            season.id
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(last) = last.filter(|l| l.coach_id == coach_id && l.pick_number == pick_number)
        else {
            return Err(DraftError::NotLatestPick);
        };

        sqlx::query!(
            "INSERT INTO pick_correction
                 (season_id, person_id, pokemon_id, pick_number, points_paid, picked_at, undone_by)
             SELECT pk.season_id, c.person_id, pk.pokemon_id, pk.pick_number, pk.points_paid,
                    pk.created_at, ?
             FROM pick pk JOIN coach c ON c.id = pk.coach_id
             WHERE pk.id = ?",
            admin_id,
            last.id
        )
        .execute(&mut *tx)
        .await?;
        let undone = sqlx::query!(
            r#"SELECT (SELECT COUNT(*) FROM pick WHERE season_id = ?) AS "number!: i64",
                      p.discord_id, m.display_name, pk.points_paid
               FROM pick pk
               JOIN coach c ON c.id = pk.coach_id
               JOIN person p ON p.id = c.person_id
               JOIN pokemon m ON m.id = pk.pokemon_id
               WHERE pk.id = ?"#,
            season.id,
            last.id
        )
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query!("DELETE FROM pick WHERE id = ?", last.id).execute(&mut *tx).await?;
        sqlx::query!(
            "UPDATE coach SET done_at = NULL
             WHERE id = ? AND (SELECT COUNT(*) FROM pick WHERE coach_id = ?) < ?",
            coach_id,
            coach_id,
            season.min_roster
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        tracing::info!(coach_id, pick_number, pokemon_id = last.pokemon_id, admin_id, "pick undone");
        let undone = Drafted {
            number: undone.number,
            discord_id: undone.discord_id,
            pokemon: undone.display_name,
            points: undone.points_paid,
        };
        Ok((undone, self.run_queue().await?))
    }

    /// How many picks each coach has queued, by coach id.
    ///
    /// The count is public; the contents are not. This query deliberately
    /// returns no `pokemon_id` so a careless caller cannot leak them.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn queue_counts(&self, season_id: i64) -> Result<Vec<(i64, i64)>, sqlx::Error> {
        let rows = sqlx::query!(
            r#"SELECT c.id AS "coach_id!: i64",
                      (SELECT COUNT(*) FROM queue_slot q WHERE q.coach_id = c.id) AS "n!: i64"
               FROM coach c WHERE c.season_id = ?"#,
            season_id
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows.into_iter().map(|r| (r.coach_id, r.n)).collect())
    }

    /// One coach's queued slots, in slot order. Owner-only data.
    ///
    /// Callers must have established that the requester *is* this coach. No
    /// admin override: an admin who can read queues can snipe them.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn queue_of(&self, coach_id: i64) -> Result<Vec<Queued>, sqlx::Error> {
        sqlx::query_as!(
            Queued,
            r#"SELECT q.slot_number, q.pokemon_id, m.slug, m.display_name,
                      ct.points AS "points!: i64"
               FROM queue_slot q
               JOIN pokemon m ON m.id = q.pokemon_id
               JOIN coach c ON c.id = q.coach_id
               JOIN cost ct ON ct.pokemon_id = q.pokemon_id AND ct.season_id = c.season_id
               WHERE q.coach_id = ?
               ORDER BY q.slot_number"#,
            coach_id
        )
        .fetch_all(self.pool())
        .await
    }

    /// Queues a Pokémon for one of a coach's future picks.
    ///
    /// Replaces whatever that slot held. The Pokémon is not reserved and the
    /// budget is not checked here: both are settled when the slot is reached,
    /// because the pool and the points will have moved by then.
    ///
    /// Then runs the queue, and returns any auto-picks it made.
    ///
    /// # Errors
    /// Refuses a slot at or below the coach's completed picks, a slot past the
    /// roster maximum, an unpriced or already-drafted Pokémon, and a Pokémon
    /// already sitting in another of this coach's slots.
    pub async fn set_queue_slot(
        &self,
        coach_id: i64,
        slot_number: i64,
        pokemon_id: i64,
    ) -> Result<Vec<Drafted>, DraftError> {
        let board = self.board().await?;
        let seat = board.seat_of(coach_id).ok_or(DraftError::NotACoach)?;

        // Slots at or below the completed pick count are dead: the draft has
        // already passed them and will never read them again.
        if slot_number <= seat.picks {
            return Err(QueueError::SlotPassed { slot: slot_number, next: seat.picks + 1 }.into());
        }
        if slot_number > board.roster.max() {
            return Err(QueueError::PastMaximum { slot: slot_number, max: board.roster.max() }.into());
        }

        let season_id = board.season.id;
        let priced: bool = sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT 1 FROM cost WHERE season_id = ? AND pokemon_id = ?)
                   AS "e!: bool""#,
            season_id,
            pokemon_id
        )
        .fetch_one(self.pool())
        .await?;
        if !priced {
            return Err(PickError::Unpriced.into());
        }

        let taken: bool = sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT 1 FROM pick WHERE season_id = ? AND pokemon_id = ?)
                   AS "e!: bool""#,
            season_id,
            pokemon_id
        )
        .fetch_one(self.pool())
        .await?;
        if taken {
            return Err(PickError::Taken.into());
        }

        // Queuing one Pokémon in two slots means one of them is guaranteed to
        // be dead by the time it is read.
        let elsewhere: Option<i64> = sqlx::query_scalar!(
            "SELECT slot_number FROM queue_slot
             WHERE coach_id = ? AND pokemon_id = ? AND slot_number != ?",
            coach_id,
            pokemon_id,
            slot_number
        )
        .fetch_optional(self.pool())
        .await?;
        if let Some(slot) = elsewhere {
            return Err(QueueError::AlreadyQueued { slot }.into());
        }

        sqlx::query!(
            "INSERT INTO queue_slot (coach_id, slot_number, pokemon_id) VALUES (?, ?, ?)
             ON CONFLICT (coach_id, slot_number) DO UPDATE SET pokemon_id = excluded.pokemon_id",
            coach_id,
            slot_number,
            pokemon_id
        )
        .execute(self.pool())
        .await?;
        tracing::info!(coach_id, slot_number, "queue slot set");

        // Queuing for the slot you are on the clock for picks immediately.
        self.run_queue().await
    }

    /// The Discord ID of a coach's person, for mentions.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn coach_discord_id(&self, coach_id: i64) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar!(
            "SELECT p.discord_id FROM coach c JOIN person p ON p.id = c.person_id WHERE c.id = ?",
            coach_id
        )
        .fetch_optional(self.pool())
        .await
    }

    /// Empties one of a coach's queue slots.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn clear_queue_slot(
        &self,
        coach_id: i64,
        slot_number: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "DELETE FROM queue_slot WHERE coach_id = ? AND slot_number = ?",
            coach_id,
            slot_number
        )
        .execute(self.pool())
        .await?;
        tracing::info!(coach_id, slot_number, "queue slot cleared");
        Ok(())
    }

    /// Picks made this season, most recent first.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn recent_picks(&self, season_id: i64, limit: i64) -> Result<Vec<Pick>, sqlx::Error> {
        sqlx::query_as!(
            Pick,
            r#"SELECT pk.pick_number, pk.coach_id, pk.points_paid,
                      p.discord_username, m.slug, m.display_name, m.types
               FROM pick pk
               JOIN coach c ON c.id = pk.coach_id
               JOIN person p ON p.id = c.person_id
               JOIN pokemon m ON m.id = pk.pokemon_id
               WHERE pk.season_id = ?
               ORDER BY pk.id DESC LIMIT ?"#,
            season_id,
            limit
        )
        .fetch_all(self.pool())
        .await
    }

    /// One coach's roster, in pick order.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn roster_of(&self, coach_id: i64) -> Result<Vec<Pick>, sqlx::Error> {
        sqlx::query_as!(
            Pick,
            r#"SELECT pk.pick_number, pk.coach_id, pk.points_paid,
                      p.discord_username, m.slug, m.display_name, m.types
               FROM pick pk
               JOIN coach c ON c.id = pk.coach_id
               JOIN person p ON p.id = c.person_id
               JOIN pokemon m ON m.id = pk.pokemon_id
               WHERE pk.coach_id = ?
               ORDER BY pk.pick_number"#,
            coach_id
        )
        .fetch_all(self.pool())
        .await
    }

    /// The season's tier list, with who owns what.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn listings(&self, season_id: i64) -> Result<Vec<Listing>, sqlx::Error> {
        sqlx::query_as!(
            Listing,
            r#"SELECT m.id AS "pokemon_id!: i64", m.slug, m.display_name, m.types,
                      ct.points AS "points!: i64",
                      p.discord_username AS "taken_by?: String", c.id AS "owner_id?: i64"
               FROM cost ct
               JOIN pokemon m ON m.id = ct.pokemon_id
               LEFT JOIN roster_entry re ON re.pokemon_id = ct.pokemon_id
                                        AND re.season_id = ct.season_id AND re.until_week IS NULL
               LEFT JOIN coach c ON c.id = re.coach_id
               LEFT JOIN person p ON p.id = c.person_id
               WHERE ct.season_id = ?
               ORDER BY ct.points DESC, m.display_name"#,
            season_id
        )
        .fetch_all(self.pool())
        .await
    }
}
