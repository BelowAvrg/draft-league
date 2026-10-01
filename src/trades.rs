//! Trade offers between coaches, and the season's public move log.
//!
//! Offers are private to their two coaches; accepted moves are public.
//! Every rule is checked again on accept, against rosters with all pending
//! moves applied. See TRADES.md.

use sqlx::SqliteConnection;

use crate::db::Db;
use crate::draft::Roster;
use crate::moves::{self, MAX_MOVES, MoveError, check_side};
use crate::picks::DraftError;

/// One Pokémon in an offer.
#[derive(Debug, Clone)]
pub struct OfferItem {
    pub offer_id: i64,
    pub pokemon_id: i64,
    pub slug: String,
    pub display_name: String,
    pub points: i64,
    /// The coach giving it up.
    pub from_coach_id: i64,
}

/// A trade offer, as its two coaches see it.
#[derive(Debug, Clone)]
pub struct Offer {
    pub id: i64,
    pub from_coach_id: i64,
    pub from_name: String,
    pub to_coach_id: i64,
    pub to_name: String,
    /// `open`, `accepted`, `declined`, or `withdrawn`.
    pub status: String,
    /// Why the site declined it, when it did.
    pub reason: Option<String>,
    pub created_at: String,
    pub items: Vec<OfferItem>,
}

impl Offer {
    /// What `coach_id` gives up in this offer.
    #[must_use]
    pub fn given_by(&self, coach_id: i64) -> Vec<&OfferItem> {
        self.items.iter().filter(|i| i.from_coach_id == coach_id).collect()
    }
}

/// One Pokémon changing hands in the move log.
#[derive(Debug, Clone)]
pub struct Moved {
    pub move_id: i64,
    pub slug: String,
    pub display_name: String,
    /// The coach it went to, or the coach who dropped it.
    pub coach: String,
    /// Dropped to the pool rather than arriving.
    pub dropped: bool,
}

/// One entry in the season's move log.
#[derive(Debug, Clone)]
pub struct MoveLine {
    pub id: i64,
    /// `trade` or `free_agency`.
    pub kind: String,
    pub effective_week: i64,
    pub created_at: String,
    pub items: Vec<Moved>,
}

/// The coaches on each side of an open offer.
struct Parties {
    season_id: i64,
    from: i64,
    to: i64,
}

impl Db {
    /// Offers a trade: `give` from `from`'s roster for `get` from `to`'s.
    ///
    /// Only ownership and the proposer's move count are checked here; the
    /// rest waits for the accept, when the rosters may have changed.
    ///
    /// # Errors
    /// Refuses during the draft, a trade with yourself or a non-coach, an
    /// empty side, a Pokémon the side does not own, or no moves left.
    pub async fn propose_trade(
        &self,
        from: i64,
        to: i64,
        give: &[i64],
        get: &[i64],
    ) -> Result<(), MoveError> {
        let board = self.board().await?;
        if board.seats.is_empty() || board.turn.coach_id.is_some() {
            return Err(MoveError::DraftRunning);
        }
        if from == to {
            return Err(MoveError::SelfTrade);
        }
        board.seat_of(from).ok_or(DraftError::NotACoach)?;
        board.seat_of(to).ok_or(DraftError::NotACoach)?;
        if give.is_empty() || get.is_empty() {
            return Err(MoveError::EmptySide);
        }

        let mut tx = self.pool().begin().await?;
        if moves::moves_used(&mut tx, from).await? >= MAX_MOVES {
            return Err(MoveError::NoMovesLeft { who: "you".to_owned() });
        }
        for (coach, ids) in [(from, give), (to, get)] {
            for &id in ids {
                let owns: bool = sqlx::query_scalar!(
                    r#"SELECT EXISTS(SELECT 1 FROM roster_entry
                           WHERE coach_id = ? AND pokemon_id = ? AND until_week IS NULL) AS "e!: bool""#,
                    coach,
                    id
                )
                .fetch_one(&mut *tx)
                .await?;
                if !owns {
                    return Err(MoveError::NotOwned);
                }
            }
        }

        let offer_id = sqlx::query_scalar!(
            "INSERT INTO trade_offer (season_id, from_coach_id, to_coach_id) VALUES (?, ?, ?)
             RETURNING id",
            board.season.id,
            from,
            to
        )
        .fetch_one(&mut *tx)
        .await?;
        for (coach, ids) in [(from, give), (to, get)] {
            for &id in ids {
                // A repeated id would only repeat the same row.
                sqlx::query!(
                    "INSERT OR IGNORE INTO trade_offer_item (offer_id, pokemon_id, from_coach_id)
                     VALUES (?, ?, ?)",
                    offer_id,
                    id,
                    coach
                )
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
        tracing::info!(offer_id, from, to, "trade offered");
        Ok(())
    }

    /// Accepts an open offer made to `coach_id`, making the trade.
    ///
    /// Returns the week the trade takes effect. If the trade no longer passes, the offer is declined with the reason
    /// and the same error is returned.
    ///
    /// # Errors
    /// Refuses an offer that is not open and addressed to `coach_id`, and
    /// any trade that breaks a rule for either side.
    pub async fn accept_trade(&self, offer_id: i64, coach_id: i64) -> Result<i64, MoveError> {
        let roster = self.board().await?.roster;
        let mut tx = self.pool().begin().await?;
        let parties = sqlx::query_as!(
            Parties,
            r#"SELECT season_id, from_coach_id AS "from", to_coach_id AS "to" FROM trade_offer
               WHERE id = ? AND to_coach_id = ? AND status = 'open'"#,
            offer_id,
            coach_id
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(MoveError::NoOffer)?;

        match make_trade(&mut tx, offer_id, &parties, roster).await {
            Ok(week) => {
                tx.commit().await?;
                tracing::info!(offer_id, from = parties.from, to = parties.to, week, "trade made");
                Ok(week)
            }
            Err(MoveError::Sqlx(e)) => Err(e.into()),
            Err(e) => {
                tx.rollback().await?;
                let reason = e.to_string();
                sqlx::query!(
                    "UPDATE trade_offer SET status = 'declined', decided_at = datetime('now'), reason = ?
                     WHERE id = ? AND status = 'open'",
                    reason,
                    offer_id
                )
                .execute(self.pool())
                .await?;
                tracing::info!(offer_id, %reason, "trade failed on accept");
                Err(e)
            }
        }
    }

    /// Declines an offer made to `coach_id`, or withdraws one made by them.
    ///
    /// # Errors
    /// Refuses an offer that is not open or does not involve `coach_id`.
    pub async fn close_offer(&self, offer_id: i64, coach_id: i64) -> Result<(), MoveError> {
        let done = sqlx::query!(
            "UPDATE trade_offer
             SET status = CASE WHEN to_coach_id = ? THEN 'declined' ELSE 'withdrawn' END,
                 decided_at = datetime('now')
             WHERE id = ? AND status = 'open' AND ? IN (from_coach_id, to_coach_id)",
            coach_id,
            offer_id,
            coach_id
        )
        .execute(self.pool())
        .await?;
        if done.rows_affected() == 0 {
            return Err(MoveError::NoOffer);
        }
        tracing::info!(offer_id, coach_id, "trade offer closed");
        Ok(())
    }

    /// Every offer `coach_id` made or received, newest first. Owner-only data.
    ///
    /// Callers must have established that the requester *is* this coach.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn offers_of(&self, coach_id: i64) -> Result<Vec<Offer>, sqlx::Error> {
        let rows = sqlx::query!(
            r#"SELECT o.id, o.from_coach_id, pf.discord_username AS from_name,
                      o.to_coach_id, pt.discord_username AS to_name,
                      o.status, o.reason, o.created_at
               FROM trade_offer o
               JOIN coach cf ON cf.id = o.from_coach_id JOIN person pf ON pf.id = cf.person_id
               JOIN coach ct ON ct.id = o.to_coach_id JOIN person pt ON pt.id = ct.person_id
               WHERE ? IN (o.from_coach_id, o.to_coach_id)
               ORDER BY o.id DESC"#,
            coach_id
        )
        .fetch_all(self.pool())
        .await?;
        let mut items = sqlx::query_as!(
            OfferItem,
            r#"SELECT ti.offer_id, ti.pokemon_id, m.slug, m.display_name,
                      ct.points AS "points!: i64", ti.from_coach_id
               FROM trade_offer_item ti
               JOIN trade_offer o ON o.id = ti.offer_id
               JOIN pokemon m ON m.id = ti.pokemon_id
               JOIN cost ct ON ct.season_id = o.season_id AND ct.pokemon_id = ti.pokemon_id
               WHERE ? IN (o.from_coach_id, o.to_coach_id)
               ORDER BY ct.points DESC"#,
            coach_id
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| Offer {
                items: items.extract_if(.., |i| i.offer_id == r.id).collect(),
                id: r.id,
                from_coach_id: r.from_coach_id,
                from_name: r.from_name,
                to_coach_id: r.to_coach_id,
                to_name: r.to_name,
                status: r.status,
                reason: r.reason,
                created_at: r.created_at,
            })
            .collect())
    }

    /// Every move this season, newest first. Public.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn move_log(&self, season_id: i64) -> Result<Vec<MoveLine>, sqlx::Error> {
        let rows = sqlx::query!(
            "SELECT id, kind, effective_week, created_at FROM move
             WHERE season_id = ? ORDER BY id DESC",
            season_id
        )
        .fetch_all(self.pool())
        .await?;
        let mut items = sqlx::query_as!(
            Moved,
            r#"SELECT re.move_id AS "move_id!: i64", m.slug, m.display_name,
                      p.discord_username AS coach, FALSE AS "dropped!: bool"
               FROM roster_entry re
               JOIN pokemon m ON m.id = re.pokemon_id
               JOIN coach c ON c.id = re.coach_id JOIN person p ON p.id = c.person_id
               WHERE re.season_id = ? AND re.move_id IS NOT NULL
               UNION ALL
               -- A trade's departures are the other side's arrivals; only a
               -- free agency drop has no arrival to show it.
               SELECT re.left_move_id, m.slug, m.display_name, p.discord_username, TRUE
               FROM roster_entry re
               JOIN move mv ON mv.id = re.left_move_id AND mv.kind = 'free_agency'
               JOIN pokemon m ON m.id = re.pokemon_id
               JOIN coach c ON c.id = re.coach_id JOIN person p ON p.id = c.person_id
               WHERE re.season_id = ?"#,
            season_id,
            season_id
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| MoveLine {
                items: items.extract_if(.., |i| i.move_id == r.id).collect(),
                id: r.id,
                kind: r.kind,
                effective_week: r.effective_week,
                created_at: r.created_at,
            })
            .collect())
    }
}

/// Validates and writes an accepted trade; returns the week it plays from.
async fn make_trade(
    conn: &mut SqliteConnection,
    offer_id: i64,
    parties: &Parties,
    roster: Roster,
) -> Result<i64, MoveError> {
    let week = moves::effective_week(conn, parties.season_id, &[parties.from, parties.to]).await?;

    let mut sides = [moves::side(conn, parties.from).await?, moves::side(conn, parties.to).await?];
    for (coach, side) in [parties.from, parties.to].into_iter().zip(&sides) {
        if moves::moves_used(conn, coach).await? >= MAX_MOVES {
            return Err(MoveError::NoMovesLeft { who: side.name.clone() });
        }
    }

    let items = sqlx::query!(
        r#"SELECT ti.pokemon_id, ti.from_coach_id, ct.points AS "points!: i64",
                  re.id AS "entry_id?: i64", re.from_week AS "from_week?: i64"
           FROM trade_offer_item ti
           JOIN cost ct ON ct.season_id = ? AND ct.pokemon_id = ti.pokemon_id
           LEFT JOIN roster_entry re ON re.pokemon_id = ti.pokemon_id
                AND re.coach_id = ti.from_coach_id AND re.until_week IS NULL
           WHERE ti.offer_id = ?"#,
        parties.season_id,
        offer_id
    )
    .fetch_all(&mut *conn)
    .await?;

    for item in &items {
        let from_week = item.from_week.ok_or(MoveError::NotOwned)?;
        if from_week >= week {
            return Err(MoveError::StillPending);
        }
        let (giver, taker) = if item.from_coach_id == parties.from { (0, 1) } else { (1, 0) };
        sides[giver].size -= 1;
        sides[giver].cost -= item.points;
        sides[taker].size += 1;
        sides[taker].cost += item.points;
    }
    for side in &sides {
        check_side(&side.name, side.size, side.cost, side.budget, roster)?;
    }

    let move_id = sqlx::query_scalar!(
        "INSERT INTO move (season_id, kind, coach_a_id, coach_b_id, effective_week)
         VALUES (?, 'trade', ?, ?, ?) RETURNING id",
        parties.season_id,
        parties.from,
        parties.to,
        week
    )
    .fetch_one(&mut *conn)
    .await?;
    for item in &items {
        let to = if item.from_coach_id == parties.from { parties.to } else { parties.from };
        sqlx::query!(
            "UPDATE roster_entry SET until_week = ?, left_move_id = ? WHERE id = ?",
            week,
            move_id,
            item.entry_id
        )
        .execute(&mut *conn)
        .await?;
        sqlx::query!(
            "INSERT INTO roster_entry (season_id, coach_id, pokemon_id, from_week, move_id)
             VALUES (?, ?, ?, ?, ?)",
            parties.season_id,
            to,
            item.pokemon_id,
            week,
            move_id
        )
        .execute(&mut *conn)
        .await?;
    }
    sqlx::query!(
        "UPDATE trade_offer SET status = 'accepted', decided_at = datetime('now'), move_id = ?
         WHERE id = ?",
        move_id,
        offer_id
    )
    .execute(&mut *conn)
    .await?;
    let moved: Vec<i64> = items.iter().map(|i| i.pokemon_id).collect();
    moves::close_offers_with(conn, &moved, Some(offer_id)).await?;
    Ok(week)
}
