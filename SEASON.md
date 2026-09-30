# Pokémon Draft Site — Season Play

Requirements for what follows the draft: schedule, results, standings,
playoffs. Companion to `DESIGN.md`, which covers the draft. Anything marked
**(assumed)** is a default that has not been confirmed. Change it or confirm it.

## Shape of a season

From `draft_league_schedule.csv` (the season as exported from draftleague.net):

- **12 coaches, 11 weeks of round robin.** Each week has 6 matches and everyone
  plays everyone once.
- **Weeks 12–13 are playoffs.** Week 12 has two matches with no teams yet (the
  semifinals) and week 13 has one (the final). That makes a 4-team bracket.

## Decisions

| Area | Decision |
|---|---|
| Result source | Computed from uploaded Showdown replays |
| Replay input | Showdown replay URL (the "Upload and share replay" link) |
| Manual override | Any coach in the season can set or change any result |
| Audit | Every result write is logged: who, when, old value, new value |
| Standings | Derived from results, never stored |
| Schedule source | One-time CSV import per season |

This reverses one line of `CLAUDE.md` ("no replay parsing"). The Showdown
integration now reads replays.

## Matches and games

A **match** is one scheduled pairing: season, week, and two coaches. A playoff
match has empty coach slots until it is seeded.

A **game** is one replay attached to a match. A match holds one or more games,
so Bo1 and Bo3 both work without a schema change. The best-of count is a
per-season setting, **Bo3**.

The match result is a winner plus a **differential**: the winner's KOs dealt
minus KOs taken, summed across games. Both sides bring four, so a game counts
as the winner's Pokémon left minus the loser's. In a Bo3 the loser's games
count against the winner's differential. This matches "Pokémon left standing"
whenever a game is played out; it differs when a player forfeits or times out
with Pokémon still standing, which then count against the winner.

A forfeit scores as the biggest possible win: every game needed, swept with all
four brought Pokémon standing. That is **+8 / -8** in a Bo3 (+4 in a Bo1).

A hand-entered differential must be one the best-of can produce. In a Bo3 that
is **-2 to +8**: the bottom is winning two games by one and losing the third to
a full team. Anything outside is refused as a typo.

## Replay ingestion

A coach pastes a replay URL on the match page. The server:

1. Fetches `<replay-url>.json` from Showdown. This also works for private
   replays, whose URL carries a password suffix.
2. Reads the log and pulls out the two player names, the `|win|` line, the
   `|teamsize|` lines, and the `|faint|` lines. `|poke|` lines are the
   six-Pokémon team preview, not what was brought; `|teamsize|` is the four
   brought.
3. Matches the two player names to the two coaches' `showdown_username`,
   comparing both as Showdown IDs (lowercase, alphanumerics only).
4. Records the game: winner, and Pokémon remaining per side (brought minus
   fainted).
5. Once the games decide the match (two wins in a Bo3), sets the match result
   from them and logs the write. Until then, the match keeps whatever result
   it has.

Only `replay.pokemonshowdown.com` is fetched: the server takes the replay id
from the pasted link and builds the fetch URL itself.

A replay is **rejected** with a specific message, and no data written, when:

- The URL does not fetch or does not parse.
- A player does not match either coach, or a coach has no Showdown username.
  The fix is to set the username and retry, or enter the result by hand.
- The battle has no winner (a tie or an unfinished game).
- The replay is already attached to any match. Replay IDs are unique.
- The replay's format is not the season's format (`season.format`, set to
  `gen9championsvgc2026regmc` for Reg M-C; NULL skips the check). It catches
  accidental uploads of practice games.
- The match's replays already decide it.

Any coach in the season, or an admin, can **remove** a game, e.g. a replay
attached to the wrong match. The replay id is freed. A result the replays set
is re-scored from the games left (usually cleared) and the change is logged.
A hand-entered result is left alone.

Store the replay URL and the parsed numbers. Don't store the raw log.

### Validation against the draft (assumed: not built)

A replay could be checked against the rosters: did each side bring only Pokémon
it drafted? This is cheap once the `|poke|` lines are parsed, but it has not been
asked for. Leave it out until it is.

## Manual override

Any coach in the season, or an admin, can set a match's result by hand: winner
and differential, or a forfeit. Clearing a result is also a logged write. This covers games that weren't recorded, a
disconnect ruling, a replay the parser can't read, and fixes.

- A manual result replaces whatever the replays computed.
- A replay that completes the match's games overwrites the manual result. The
  history keeps both.
- Every write goes to `match_result_edit` with who, when, the old result, the new
  result, and the source (`replay` / `manual`). The match page shows this
  history.

Letting anyone override is deliberately permissive: the league is small and
trusted, and the audit trail is the safeguard. It is still server-checked, as
"logged in *and* a coach in this season," not just "logged in."

## Standings

Derived on every page load from match results:

1. Wins
2. Head-to-head, when exactly two coaches are tied
3. Differential. A loss counts the winner's differential against the loser,
   so a +3 win is -3 for the other side.
4. Draft position, lower first. Seeding is automatic, so the last tiebreaker
   cannot be an admin call; it has to be deterministic.

Only regular-season weeks count. Playoff results don't touch standings.

## Playoffs

Seeding is **fully automatic**. The result write that completes the regular
season fills the week 12 slots with #1 v #4 and #2 v #3 from standings.

- Every result write re-derives the bracket. An override that changes the
  seeding re-seeds the semifinals, unless a semifinal it would move already
  has a result. Then it refuses and says why. An override that leaves the
  seeding alone (a corrected differential, say) is always fine.
- The week 13 final fills automatically from the semifinal winners, with the
  same rule: changing a semifinal winner re-seeds the final unless the final
  already has a result.
- Until seeding, `/standings` shows the projected semifinals ("if the season
  ended today"). After, it shows the real bracket with results.

## Schedule import

Admin uploads the CSV: `Team_One_ID, Team_One_Name, Team_Two_ID,
Team_Two_Name, Week`.

- Teams are matched to coaches by **team name**. Coaches get a `team_name`
  per season, set on the admin page before import. Names are compared after
  trimming, since the export has trailing spaces (`"socksinthedryer "`).
  draftleague.net team IDs are dropped.
- Rows with both teams blank become playoff matches with empty slots.
- As with the tier list: report every bad row at once, and refuse the import
  entirely if the season already has any match results.

## Schema sketch

```
coach.team_name TEXT                            -- new column, per season

season.best_of INTEGER NOT NULL DEFAULT 3       -- odd, >= 1
season.format   TEXT                            -- Showdown format id to check replays against; NULL skips

match(id, season_id, week, is_playoff,
      coach_a_id NULL, coach_b_id NULL,         -- NULL = playoff slot not yet seeded
      winner_coach_id NULL, differential NULL,
      is_forfeit, result_source NULL)           -- 'replay' | 'manual'
    check(coach_a_id <> coach_b_id)
    check(winner is coach_a or coach_b)

game(id, match_id, replay_id UNIQUE, replay_url,
     winner_coach_id, a_remaining, b_remaining, created_by, created_at)

match_result_edit(id, match_id, person_id, created_at, source,
                  old_winner, old_diff, new_winner, new_diff)
```

## Pages

| Route | Purpose |
|---|---|
| `/schedule` | All weeks, matches, and results; your matches highlighted |
| `/match/:id` | Games, replay links, upload box, manual override, edit history |
| `/standings` | Table, plus the bracket once playoffs are seeded |
| `/admin` | Team names, schedule import |

## Deliberately not building

- Deadlines and reminders. The week number is enough; there are no dates yet.
- Roster validation of replays (see above).
- Stats pages (usage, KOs per Pokémon). The parsed data would allow it later if
  the raw numbers are kept.
- Schedule generation. The CSV covers this season.
