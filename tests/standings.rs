//! Standings order: wins, two-way head-to-head, differential, draft position.

use pokemon_draft_site::coaches::Coach;
use pokemon_draft_site::results::Match;
use pokemon_draft_site::standings::standings;

fn coach(id: i64) -> Coach {
    Coach {
        id,
        person_id: id,
        discord_id: String::new(),
        discord_username: format!("c{id}"),
        showdown_username: None,
        is_admin: false,
        budget: 100,
        draft_position: id,
        spent: 0,
        done_at: None,
        team_name: None,
    }
}

/// `winner` beat `loser` by `diff`.
fn result(winner: i64, loser: i64, diff: i64) -> Match {
    Match {
        id: 0,
        season_id: 1,
        week: 1,
        is_playoff: false,
        coach_a: Some(winner),
        coach_b: Some(loser),
        team_a: None,
        team_b: None,
        winner: Some(winner),
        differential: Some(diff),
        is_forfeit: false,
        best_of: 3,
    }
}

fn order(coaches: &[Coach], matches: &[Match]) -> Vec<i64> {
    standings(coaches, matches).iter().map(|r| r.coach_id).collect()
}

#[test]
fn two_way_tie_goes_to_head_to_head_over_differential() {
    let cs: Vec<Coach> = (1..=4).map(coach).collect();
    // 1 and 2 both have 2 wins. 2's differential is far better, but 1 beat 2.
    // 3 and 4 are both winless and never met, so differential decides: 3 at -8, 4 at -9.
    let ms = [result(1, 2, 1), result(2, 3, 8), result(1, 4, 1), result(2, 4, 8)];
    let rows = standings(&cs, &ms);
    assert_eq!(order(&cs, &ms), vec![1, 2, 3, 4]);
    assert_eq!((rows[1].wins, rows[1].losses, rows[1].differential), (2, 1, 15), "loser's diff is negated");
}

#[test]
fn three_way_tie_skips_head_to_head() {
    let cs: Vec<Coach> = (1..=3).map(coach).collect();
    // A cycle: everyone 1-1. Differentials: 1 at 0, 2 at +7, 3 at -7.
    let ms = [result(1, 2, 1), result(2, 3, 8), result(3, 1, 1)];
    assert_eq!(order(&cs, &ms), vec![2, 1, 3]);
}

#[test]
fn playoffs_and_unplayed_matches_do_not_count() {
    let cs: Vec<Coach> = (1..=3).map(coach).collect();
    let mut playoff = result(3, 1, 8);
    playoff.is_playoff = true;
    let mut unplayed = result(3, 2, 0);
    (unplayed.winner, unplayed.differential) = (None, None);
    // Nothing counts, so draft position orders the table.
    assert_eq!(order(&cs, &[playoff, unplayed]), vec![1, 2, 3]);
}
