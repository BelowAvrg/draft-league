# Pokémon Draft Site — Design

Replacement for draftleague.net for our league. Scope of this document: the
draft. Season play (schedule, results, standings) is explicitly out of scope for
now; the schema should not make it painful to add later, but nothing is built
for it.

## Why build it

Three things draftleague.net does not do:

1. **Per-coach point budgets.** Our league mixes serious VGC players with people
   who barely know the game. Budgets vary per person to balance that — strong
   players get fewer points. draftleague.net assumes one budget for everyone.
2. **Admin editing of member fields.** An admin needs to set someone else's
   Discord ID and Showdown username, not just their own.
3. **Queued-pick visibility.** Coaches should see *that* someone has picks
   queued and *how many*, never *which*.

## Decisions

| Area | Decision |
|---|---|
| Draft format | Snake, points as a **cap** |
| Roster size | A **range**: 8 minimum, 12 maximum |
| Leaving early | Coach clicks "I'm done" at ≥8 picks, or runs out of points |
| Turn order | Snake over the **still-active** coaches; the order compresses |
| Budgets | Admin types a number per coach |
| Queue | **Positional** — slot N binds to that coach's pick N |
| Pick timer | None; draft waits for the coach |
| Overspend | Blocked — must reserve enough to reach 8 picks, no further |
| Tier list | Admin uploads a CSV per season |
| Draftable unit | Individual forms, not species |
| Auth | Discord OAuth |
| Discord | Stores the ID only; no messaging yet |
| Showdown | Stores the username only |
| Multi-tenancy | One league, multiple seasons |

## Domain model

### Season
A draft cycle. Holds the tier list (via `costs`), the roster size range, the
snake order, and the picks. Exactly one season is active at a time. Past seasons
stay readable.

### Coach
A league member's participation in a season. Budget lives here, not on the
person — it changes season to season and differs per coach. So does `done_at`:
finishing early is a fact about this season's participation.

The person (Discord identity, Showdown username) is separate from their
participation, because the same person plays across seasons with different
budgets.

### Pokémon
A draftable entry, keyed by a stable slug. **Forms are separate entries**:
`landorus-therian` is not `landorus-incarnate`. The league plays Reg M-C;
under Reg M-B the list was ~208 species but ~235 legal entries, and forms are
priced differently.

Costs are per-season, so the entry list and its prices are separate concerns:
the list is stable, the prices change every season.

### Pick
An append-only record: coach, Pokémon, cost paid, pick number. Cost is copied
onto the pick rather than read live from the tier list — a mid-draft price
correction must not silently rewrite what people already paid.

### Queue slot
`(coach, slot_number, pokemon)`. Contents are secret; the count is public.

## The queue, precisely

This is the subtlest part of the system and the easiest to get wrong.

A queue slot is bound to a **specific pick number**, not to a priority list.
Slot 3 means "my third pick of the draft." It is **not** a backup for slot 2.

When the draft reaches coach C's pick number N:

- Slot N holds an available Pokémon → **auto-pick it**, draft advances.
- Slot N is empty, or its Pokémon was drafted by someone else → **the draft
  stops and waits for C to pick manually.** It does *not* promote slot N+1.

When someone drafts a Pokémon sitting in another coach's queue, that slot is
**emptied** (the binding is broken). The coach sees the empty slot next time
they load their draft page — no notification, no Discord ping.

Consequence worth stating: a sniped slot stalls the draft until that coach acts.
That is intended. Slot N+1 stays reserved for pick N+1 regardless.

### Why not a fallback chain

The obvious alternative — "try each queued pick in order until one works" —
would keep the draft moving without human input. It is rejected because it
silently spends a coach's points on a Pokémon they meant for a *later, cheaper*
round. Stalling is the safer failure.

## Roster size and leaving the draft

A roster is **8 to 12** Pokémon. Everyone must reach 8; nobody exceeds 12.
Between those, a coach chooses how far to go.

A coach stops drafting in one of three ways:

| State | Cause | Stored? |
|---|---|---|
| **Full** | 12 picks | Derived from pick count |
| **Done** | Clicked "I'm done" at ≥8 picks | `coach.done_at` |
| **Broke** | Cannot afford the cheapest available Pokémon | Derived |

"I'm done" is **final** — a coach cannot un-done themselves. An admin can clear
it as a correction, the same way pick corrections work. The reason is turn
order: coaches plan around who is left, and letting someone rejoin after the
order has compressed around their absence rewrites picks that already happened.

**Broke** is not stored because it is not a decision. It is recomputed from
points remaining against the live pool, so a coach who looks broke while the
cheap tier is thin becomes active again if a cheaper Pokémon is freed by an
admin pick correction.

### Turn order compresses

Each round is a snake over the coaches still active *at that moment*. A coach
who goes done, full, or broke disappears from the order; the remaining coaches
snake over the shorter list.

The alternative — a fixed 12-round snake over everyone, skipping dead slots in
place — was rejected because the late rounds are mostly empty and the board
becomes hard to read at exactly the point the draft gets interesting.

Consequence worth stating: because the active set shrinks mid-round, a coach can
occasionally pick twice in close succession as the snake turns around a newly
shorter list. That is normal for a compressing snake and is accepted.

## Budget rules

Two constraints, both enforced server-side on every pick:

1. **Cap.** Total roster cost ≤ the coach's budget.
2. **Reserve to the minimum.** After a pick, remaining points must cover the
   cheapest available Pokémon for each slot up to **8**. Past 8 picks the
   reserve is zero.

Rule 2 is what makes an *unfinishable* roster impossible: no coach can spend big
early and then be unable to reach the 8-pick minimum. It must be computed
against actually-available Pokémon rather than assuming 1 point per slot — the
cheap tier can be exhausted.

Deliberately, the reserve does **not** protect picks 9 through 12. Those are
optional, so running out of points there is a legitimate way to end your draft,
and it is what makes the **broke** state reachable at all. A coach who wants all
12 manages their own points.

Because rule 2 holds, a queued Pokémon for a slot at or below 8 can never become
unaffordable. A slot above 8 can — if the coach spends down past it, that slot
is simply never reached. Only sniping empties a slot.

## Visibility

| Data | Who sees it |
|---|---|
| Completed picks and rosters | Everyone |
| Queue **count** per coach | Everyone |
| Queue **contents** | Owner only — not other coaches, not admins |
| Budgets and points remaining | Everyone |
| Whether a coach is done or broke | Everyone — it changes whose turn it is |
| Discord ID, Showdown username | Owner and admins |

Queue contents are secret from admins too. An admin who can see queues can
snipe them, and an admin is also a coach in this league. If a dispute ever needs
adjudicating, the pick history is the record.

Every admin-only mutation re-checks the admin flag server-side. Hiding a button
in a template is not access control.

## Schema sketch

SQLite. Not final — column types and constraints get settled at implementation.

```
person(id, discord_id unique, discord_username, showdown_username, is_admin)
season(id, name, min_roster, max_roster, is_active, created_at)
    check(0 < min_roster <= max_roster)
coach(id, person_id, season_id, budget, draft_position, done_at)
    unique(person_id, season_id)
    -- done_at NULL means still drafting; a timestamp is the trail

pokemon(id, slug unique, display_name)
cost(season_id, pokemon_id, points)        -- the tier list, per season
    unique(season_id, pokemon_id)
pick(id, coach_id, pokemon_id, pick_number, points_paid, created_at)
    unique(coach_id, pick_number)
    unique(season_id, pokemon_id) via coach  -- a Pokémon goes once per season
queue_slot(coach_id, slot_number, pokemon_id)
    unique(coach_id, slot_number)
```

The "drafted once per season" rule needs care: it spans `pick` → `coach` →
`season`. Either denormalize `season_id` onto `pick` to get a plain unique
constraint, or enforce it in a transaction. **Prefer the denormalized column** —
a database constraint that cannot be bypassed beats application logic that can.

`roster_size` is replaced by the `min_roster`/`max_roster` pair; it had no data
worth migrating.

Whose turn it is derives from the picks so far, the snake order, and which
coaches are still active — rather than being stored. One less thing to get out
of sync. The draft is over when every coach is full, done, or broke.

## Tier list import

Admin uploads a CSV per season: slug, display name, cost. Import is
season-scoped and replaces that season's costs.

Validation matters, since this is the one place bad data enters the system:
reject unknown slugs, reject duplicates, reject costs < 1. Report every problem
at once rather than failing on the first bad row. Refuse entirely if the season
has picks — silently repricing a draft in progress would corrupt it.

Source data for our list: `../draft-league-promo/src/dex.ts` has the Reg M-B
national dex numbers (the league now plays Reg M-C; the tier-list CSV import is
what sets the legal list); `../draft-spreadsheet` has CSV conversion work already.

## Auth

Discord OAuth, `identify` scope. No approval process and no verification
threshold at our size — verification only applies at 100+ servers, which is
about bots, not OAuth.

First login creates a `person` from the Discord ID. Sessions via
`tower-sessions` with a signed cookie, SQLite-backed. The first account, or one
promoted by hand in the DB, is admin.

Storing the Discord ID via OAuth rather than a typed-in field is also a
correctness win: a hand-entered 18-digit snowflake cannot be verified.

## Pages

| Route | Purpose |
|---|---|
| `/` | Draft board — whose turn, recent picks, queue counts |
| `/roster/:coach` | One coach's roster, points spent and remaining |
| `/draft` | Your pick screen (when it's your turn) and your queue editor |
| `/pokemon` | Tier list, filterable, showing what's taken |
| `/admin` | Coaches, budgets, member fields, CSV import, pick corrections |

Server-rendered askama, plain page refresh, Tailwind for styling, Alpine only
where a page genuinely needs interactivity (queue reordering, tier filtering).

## Deliberately not building

- **Websockets / live updates.** Picks happen over days. Refresh is enough.
- **Pick timers.** Social pressure and admin nudges handle it today.
- **Auction drafting.** Not our format.
- **Discord messaging.** Webhook announcements and turn pings are the most
  likely first addition — a webhook is ~20 lines and needs no bot. Revisit once
  the draft works and we see whether stalls are actually a problem.
- **Season play.** Schedule, results, standings, playoffs. The largest deferred
  piece and the main remaining gap versus draftleague.net.
- **Showdown team export / validation.** Needs season play to be worth much.
- **Multi-league tenancy.** One league. Adding `league_id` everywhere later is
  mechanical if it's ever wanted.

## Build order

1. Schema and migrations; seed the Pokémon list.
2. Discord OAuth and sessions.
3. Tier list CSV import (needed before any draft can happen).
4. Admin: coaches, budgets, member fields.
5. Draft board and manual picking: both budget rules, the 8-12 range, "I'm
   done", auto-skipping broke coaches, and the compressing snake order.
6. Queues, including the positional snipe behavior.

Steps 5 and 6 carry the real logic and each need tests: snake order over a
shrinking active set, the two budget rules, the done/broke transitions, and
queue auto-pick including a sniped slot.

## Pick corrections

An admin can undo only the season's **latest** pick. The Pokémon returns to the
pool, the points return to the coach, and the turn returns to them. Nothing
cascades, because nothing comes after it. An older mistake is reached by undoing
back to it. Each undone pick is copied to `pick_correction` with who undid it
and when. A done coach who drops below the minimum is reopened. Queue slots the
pick broke stay broken.

## Open questions

- Draft order: manually set by the admin, or randomized by the site?
- Can a coach edit a queue slot for a pick already passed? (Presumably no —
  slots below the current pick number are dead.)
- Queue slots above 8: a coach can queue slot 11 and then spend past it, or go
  done at 9, leaving the slot unreachable. Warn them, or leave it silent?
- Does a coach who has gone done or broke keep their queued slots, or are they
  cleared? (Leaning: leave them, they are simply never read.)
