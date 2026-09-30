//! Turn order over a shrinking active set, the reserve rule, and finishing early.

use pokemon_draft_site::draft::{
    DoneError, PickError, Pool, Roster, Seat, Standing, standing, validate_done, validate_pick,
    whose_turn,
};

/// This league's range: 8 minimum, 12 maximum.
fn roster() -> Roster {
    Roster::new(8, 12).expect("8-12 is valid")
}

/// A pool deep enough that the reserve is always the cheap tier.
fn deep_pool() -> Pool {
    Pool::new(vec![1; 40])
}

fn seat(coach_id: i64, draft_position: i64, picks: i64, remaining: i64) -> Seat {
    Seat { coach_id, draft_position, picks, remaining, done: false }
}

#[test]
fn snake_order_compresses_when_a_coach_finishes() {
    let roster = roster();
    let pool = deep_pool();

    // Three coaches, one pick each: round 2 runs in reverse draft order.
    let seats = vec![seat(1, 1, 1, 50), seat(2, 2, 1, 50), seat(3, 3, 1, 50)];
    let turn = whose_turn(&seats, roster, &pool);
    assert_eq!(turn.round, 2);
    assert_eq!(turn.coach_id, Some(3), "round 2 starts at the back");

    // Coach 3 is done. The order compresses to coaches 1 and 2, and round 2
    // now begins with coach 2 -- the new back of a shorter list.
    let mut seats = seats;
    seats[2].done = true;
    seats[2].picks = 8;
    let turn = whose_turn(&seats, roster, &pool);
    assert_eq!(turn.round, 2);
    assert_eq!(turn.coach_id, Some(2), "the done coach vanishes from the order");

    // Odd round still runs forwards over whoever is left.
    let seats = vec![seat(1, 1, 2, 50), seat(2, 2, 2, 50), Seat { done: true, ..seat(3, 3, 8, 50) }];
    let turn = whose_turn(&seats, roster, &pool);
    assert_eq!((turn.round, turn.coach_id), (3, Some(1)));
}

#[test]
fn draft_ends_when_everyone_is_out() {
    let roster = roster();
    let pool = deep_pool();
    let seats = vec![
        Seat { done: true, ..seat(1, 1, 9, 5) },  // done
        seat(2, 2, 12, 5),                        // full
        seat(3, 3, 10, 0),                        // broke
    ];
    let turn = whose_turn(&seats, roster, &pool);
    assert_eq!(turn.coach_id, None, "nobody is on the clock");
}

#[test]
fn reserve_protects_the_minimum_but_not_the_maximum() {
    let roster = roster();
    // Cheapest available are all 1 point, so reaching 8 costs 1 per slot.
    let pool = Pool::new(vec![1, 1, 1, 1, 1, 1, 1, 1, 30, 30]);

    // 6 picks: this pick is the 7th, so one slot still follows it before the
    // minimum. Spending everything on a 30 leaves nothing for that slot.
    let short = seat(1, 1, 6, 30);
    let err = validate_pick(&short, 30, roster, &pool).expect_err("must reserve for slot 8");
    assert!(
        matches!(err, PickError::BreaksReserve { left: 0, reserve: 1, min: 8, .. }),
        "got {err:?}"
    );
    // One point more and the reserve is satisfied.
    validate_pick(&seat(1, 1, 6, 31), 30, roster, &pool).expect("31 covers the reserve");

    // The pick that lands on the minimum reserves nothing: there is no slot
    // after it that the coach is obliged to fill.
    assert!(
        validate_pick(&seat(1, 1, 7, 30), 30, roster, &pool).is_ok(),
        "the 8th pick itself needs no reserve"
    );

    // Past the minimum, picks 9-12 are optional, so the same spend is allowed
    // even though it leaves the coach with nothing.
    assert!(
        validate_pick(&seat(1, 1, 8, 30), 30, roster, &pool).is_ok(),
        "past the minimum there is nothing to reserve for"
    );
}

#[test]
fn reserve_counts_the_pokemon_being_picked() {
    let roster = roster();
    // Exactly two Pokémon left: a 1 and a 5. At 6 picks, taking the 5 as pick
    // 7 leaves one slot to fill from a pool holding only the 1, so the reserve
    // is 1 -- the 5 it just took is no longer available to reserve against.
    let pool = Pool::new(vec![1, 5]);
    let err = validate_pick(&seat(1, 1, 6, 5), 5, roster, &pool).expect_err("needs 1 for slot 8");
    assert!(matches!(err, PickError::BreaksReserve { reserve: 1, .. }), "got {err:?}");
    validate_pick(&seat(1, 1, 6, 6), 5, roster, &pool).expect("6 covers the 5 plus the reserve");
}

#[test]
fn a_broke_coach_is_skipped_automatically() {
    let roster = roster();
    let pool = Pool::new(vec![3, 4, 10]);

    // Past the minimum with less than the cheapest Pokémon costs.
    let broke = seat(1, 1, 9, 2);
    assert_eq!(standing(&broke, roster, &pool), Standing::Broke);

    let seats = vec![broke, seat(2, 2, 9, 20)];
    let turn = whose_turn(&seats, roster, &pool);
    assert_eq!(turn.coach_id, Some(2), "the broke coach does not hold up the draft");

    // Freeing a cheaper Pokémon revives them: broke is derived, not stored.
    let cheaper = Pool::new(vec![1, 3, 4, 10]);
    assert_eq!(standing(&broke, roster, &cheaper), Standing::Active);
}

#[test]
fn standing_ranks_full_over_done_and_done_over_broke() {
    let roster = roster();
    let pool = Pool::new(vec![5]);
    assert_eq!(standing(&seat(1, 1, 12, 0), roster, &pool), Standing::Full);
    assert_eq!(standing(&Seat { done: true, ..seat(1, 1, 9, 0) }, roster, &pool), Standing::Done);
    assert_eq!(standing(&seat(1, 1, 9, 0), roster, &pool), Standing::Broke);
    assert_eq!(standing(&seat(1, 1, 9, 5), roster, &pool), Standing::Active);
}

#[test]
fn finishing_early_needs_the_minimum() {
    let roster = roster();
    let err = validate_done(&seat(1, 1, 7, 50), roster).expect_err("7 is below the minimum");
    assert!(matches!(err, DoneError::BelowMinimum { picks: 7, min: 8 }), "got {err:?}");

    validate_done(&seat(1, 1, 8, 50), roster).expect("8 is enough to stop");

    let err = validate_done(&Seat { done: true, ..seat(1, 1, 8, 50) }, roster)
        .expect_err("already done");
    assert!(matches!(err, DoneError::AlreadyDone), "got {err:?}");
}

#[test]
fn a_full_roster_cannot_pick_again() {
    let roster = roster();
    let pool = deep_pool();
    let err = validate_pick(&seat(1, 1, 12, 50), 1, roster, &pool).expect_err("12 is the maximum");
    assert!(matches!(err, PickError::RosterFull(12)), "got {err:?}");
}

#[test]
fn cap_rule_refuses_a_pick_over_the_remaining_budget() {
    let roster = roster();
    let pool = deep_pool();
    let err = validate_pick(&seat(1, 1, 9, 5), 6, roster, &pool).expect_err("over budget");
    assert!(matches!(err, PickError::OverBudget { cost: 6, remaining: 5 }), "got {err:?}");
}
