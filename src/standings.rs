//! The regular-season table, derived from match results on every read.

use std::cmp::Ordering;

use crate::coaches::Coach;
use crate::results::Match;

/// One coach's line in the standings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub coach_id: i64,
    /// Team name, falling back to the coach's Discord name.
    pub team: String,
    pub wins: i64,
    pub losses: i64,
    /// Sum of the winner's differential on wins, negated on losses.
    pub differential: i64,
    pub draft_position: i64,
}

/// Ranks coaches by regular-season results, best first.
///
/// Order: wins; head-to-head when exactly two coaches share a win total;
/// differential; draft position, lower first. Playoff matches and matches
/// without a result are ignored.
#[must_use]
pub fn standings(coaches: &[Coach], matches: &[Match]) -> Vec<Row> {
    let played: Vec<&Match> = matches.iter().filter(|m| !m.is_playoff && m.winner.is_some()).collect();

    let mut rows: Vec<Row> = coaches
        .iter()
        .map(|c| {
            let mut row = Row {
                coach_id: c.id,
                team: c.team_name.clone().unwrap_or_else(|| c.discord_username.clone()),
                wins: 0,
                losses: 0,
                differential: 0,
                draft_position: c.draft_position,
            };
            for m in played.iter().filter(|m| m.involves(Some(c.id))) {
                let d = m.differential.unwrap_or_default();
                if m.winner == Some(c.id) {
                    row.wins += 1;
                    row.differential += d;
                } else {
                    row.losses += 1;
                    row.differential -= d;
                }
            }
            row
        })
        .collect();

    rows.sort_by(|x, y| {
        y.wins
            .cmp(&x.wins)
            .then(y.differential.cmp(&x.differential))
            .then(x.draft_position.cmp(&y.draft_position))
    });

    // Head-to-head only settles a two-way tie on wins; it overrides the
    // differential order the sort already applied to that pair.
    let mut i = 0;
    while i < rows.len() {
        let tied = rows[i..].iter().take_while(|r| r.wins == rows[i].wins).count();
        if tied == 2 && head_to_head(&played, rows[i + 1].coach_id, rows[i].coach_id) == Ordering::Greater {
            rows.swap(i, i + 1);
        }
        i += tied;
    }
    rows
}

/// Compares `a`'s wins over `b` against `b`'s wins over `a`.
fn head_to_head(played: &[&Match], a: i64, b: i64) -> Ordering {
    let wins = |x: i64, y: i64| {
        played.iter().filter(|m| m.involves(Some(x)) && m.involves(Some(y)) && m.winner == Some(x)).count()
    };
    wins(a, b).cmp(&wins(b, a))
}
