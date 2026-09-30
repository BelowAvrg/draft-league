//! Turn order, roster states, and pick validation.
//!
//! Everything here is a pure function over a snapshot of the draft, so the
//! rules can be tested without a database. The database side lives in
//! [`crate::picks`].

/// Where a coach stands in the draft.
///
/// Only [`Self::Done`] is stored; the others are derived from the pick count
/// and the live pool, so freeing a cheap Pokémon revives a broke coach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// Still picking.
    Active,
    /// Reached the maximum roster size.
    Full,
    /// Stopped voluntarily at or above the minimum.
    Done,
    /// Cannot afford the cheapest Pokémon still available.
    Broke,
}

impl Standing {
    /// Whether this coach still takes turns.
    #[must_use]
    pub fn is_active(self) -> bool {
        self == Self::Active
    }

    /// A short label for the draft board.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Active => "drafting",
            Self::Full => "full",
            Self::Done => "done",
            Self::Broke => "out of points",
        }
    }
}

impl std::fmt::Display for Standing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// One coach's position in the draft, as the rules need to see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seat {
    pub coach_id: i64,
    /// Draft order within the season, 1-based.
    pub draft_position: i64,
    /// Picks already made.
    pub picks: i64,
    /// Budget minus points spent.
    pub remaining: i64,
    /// Whether this coach has declared themselves finished.
    pub done: bool,
}

/// The roster size range a season drafts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Roster {
    min: i64,
    max: i64,
}

impl Roster {
    /// Builds a roster range.
    ///
    /// # Errors
    /// Fails unless `1 <= min <= max`.
    pub fn new(min: i64, max: i64) -> Result<Self, RosterError> {
        if min < 1 || min > max {
            return Err(RosterError { min, max });
        }
        Ok(Self { min, max })
    }

    /// Fewest picks a coach must end with.
    #[must_use]
    pub fn min(self) -> i64 {
        self.min
    }

    /// Most picks a coach may end with.
    #[must_use]
    pub fn max(self) -> i64 {
        self.max
    }
}

/// A roster range that does not satisfy `1 <= min <= max`.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("roster range {min}-{max} is not valid; need 1 <= min <= max")]
pub struct RosterError {
    pub min: i64,
    pub max: i64,
}

/// Costs of the Pokémon still available, cheapest first.
///
/// Held sorted because every rule that consults it wants the cheapest few.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pool(Vec<i64>);

impl Pool {
    /// Builds a pool from the available costs in any order.
    #[must_use]
    pub fn new(mut costs: Vec<i64>) -> Self {
        costs.sort_unstable();
        Self(costs)
    }

    /// Cost of the cheapest Pokémon still available.
    #[must_use]
    pub fn cheapest(&self) -> Option<i64> {
        self.0.first().copied()
    }

    /// Total cost of the `n` cheapest available Pokémon.
    ///
    /// Returns `None` when fewer than `n` remain, which means the draft cannot
    /// physically be completed and is a setup error rather than a coach's.
    #[must_use]
    pub fn cheapest_n(&self, n: i64) -> Option<i64> {
        let n = usize::try_from(n.max(0)).ok()?;
        if n > self.0.len() {
            return None;
        }
        Some(self.0[..n].iter().sum())
    }

    /// How many Pokémon are still available.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether nothing is left to draft.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Points a coach must keep back to reach the minimum roster size.
///
/// Zero once they have the minimum: picks beyond it are optional, so spending
/// down to nothing there simply ends their draft. This is what makes
/// [`Standing::Broke`] reachable.
///
/// Costs the cheapest actually-available Pokémon rather than assuming 1 point
/// per slot, because the cheap tier can be exhausted.
#[must_use]
pub fn reserve(picks: i64, roster: Roster, pool: &Pool) -> i64 {
    let slots = (roster.min() - picks).max(0);
    // A pool too small to fill the minimum cannot be reserved against; the cap
    // rule still applies and the draft is over for lack of Pokémon anyway.
    pool.cheapest_n(slots).unwrap_or(0)
}

/// Classifies a coach against the roster range and the live pool.
///
/// The pool passed here must exclude nothing on this coach's behalf: it is the
/// league-wide set of undrafted Pokémon.
#[must_use]
pub fn standing(seat: &Seat, roster: Roster, pool: &Pool) -> Standing {
    if seat.picks >= roster.max() {
        return Standing::Full;
    }
    if seat.done {
        return Standing::Done;
    }
    // Below the minimum, the reserve rule guarantees affordability, so a coach
    // can only be broke once they are past it -- or if the pool ran dry.
    match pool.cheapest() {
        Some(cheapest) if seat.remaining >= cheapest => Standing::Active,
        _ => Standing::Broke,
    }
}

/// Whose turn it is, and which round the draft is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    /// The coach on the clock, or `None` when the draft is over.
    pub coach_id: Option<i64>,
    /// 1-based round number, counting only rounds that had an active coach.
    pub round: i64,
}

/// Works out who is on the clock.
///
/// The order is a snake over the coaches still active *now*, so a coach who
/// goes done, full, or broke disappears from it and the remaining coaches
/// snake over the shorter list. See DESIGN.md for why the order compresses
/// rather than holding dead slots open.
#[must_use]
pub fn whose_turn(seats: &[Seat], roster: Roster, pool: &Pool) -> Turn {
    let mut active: Vec<&Seat> =
        seats.iter().filter(|s| standing(s, roster, pool).is_active()).collect();
    active.sort_unstable_by_key(|s| s.draft_position);

    let Some(fewest) = active.iter().map(|s| s.picks).min() else {
        return Turn { coach_id: None, round: 0 };
    };

    // Everyone still active with the fewest picks owes a pick this round; the
    // round number is that count, and the snake alternates direction with it.
    let round = fewest + 1;
    let mut owing: Vec<&&Seat> = active.iter().filter(|s| s.picks == fewest).collect();
    if round % 2 == 0 {
        owing.reverse();
    }

    Turn { coach_id: owing.first().map(|s| s.coach_id), round }
}

/// Why a pick was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PickError {
    #[error("it is not your turn")]
    NotYourTurn,
    #[error("your roster is full at {0} Pokémon")]
    RosterFull(i64),
    #[error("you have finished drafting")]
    AlreadyDone,
    #[error("that Pokémon is already drafted")]
    Taken,
    #[error("that Pokémon has no price this season")]
    Unpriced,
    #[error("costs {cost} but you have {remaining} points left")]
    OverBudget { cost: i64, remaining: i64 },
    #[error("costs {cost}, leaving {left} of the {reserve} needed to reach {min} Pokémon")]
    BreaksReserve { cost: i64, left: i64, reserve: i64, min: i64 },
}

/// Checks a pick against both budget rules and the roster range.
///
/// `cost` is the price of an available Pokémon; `pool` is every undrafted
/// Pokémon including that one.
///
/// # Errors
/// Refuses a full or finished roster, a cost above the remaining budget, and a
/// cost that would leave too little to reach the minimum roster size.
pub fn validate_pick(seat: &Seat, cost: i64, roster: Roster, pool: &Pool) -> Result<(), PickError> {
    if seat.picks >= roster.max() {
        return Err(PickError::RosterFull(roster.max()));
    }
    if seat.done {
        return Err(PickError::AlreadyDone);
    }
    if cost > seat.remaining {
        return Err(PickError::OverBudget { cost, remaining: seat.remaining });
    }

    // The reserve is computed for the roster this pick produces, against the
    // pool it leaves behind -- the picked Pokémon is no longer available to
    // fill one of the slots it is being reserved for.
    let mut left_in_pool = pool.clone();
    if let Some(i) = left_in_pool.0.iter().position(|&c| c == cost) {
        left_in_pool.0.remove(i);
    }
    let reserve = reserve(seat.picks + 1, roster, &left_in_pool);
    let left = seat.remaining - cost;
    if left < reserve {
        return Err(PickError::BreaksReserve { cost, left, reserve, min: roster.min() });
    }
    Ok(())
}

/// Whether a coach may declare themselves finished.
///
/// # Errors
/// Refuses a roster below the minimum, or one that is already finished.
pub fn validate_done(seat: &Seat, roster: Roster) -> Result<(), DoneError> {
    if seat.done {
        return Err(DoneError::AlreadyDone);
    }
    if seat.picks < roster.min() {
        return Err(DoneError::BelowMinimum { picks: seat.picks, min: roster.min() });
    }
    Ok(())
}

/// Why finishing early was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DoneError {
    #[error("you have finished drafting already")]
    AlreadyDone,
    #[error("you have {picks} Pokémon and need at least {min}")]
    BelowMinimum { picks: i64, min: i64 },
}
