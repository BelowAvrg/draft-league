# CLAUDE.md

Guidance for Claude Code working in this repository.

## Project

Web app for running a Pokémon draft league. Replaces draftleague.net for our
league, which runs Pokémon Champions (Reg M-C).

Features that motivated building it, and that draftleague.net lacks:

- **Per-coach point budgets.** Budgets vary by player, not a league-wide constant.
  Strong VGC players get fewer points than newcomers to balance the field. The
  budget is a property of the coach's league membership, not the league.
- **Admin editing of member fields.** An admin can set another member's Discord
  ID and Showdown username, not just their own.
- **Queued-pick visibility.** Anyone can see *how many* picks a coach has queued.
  Nobody but that coach (and nobody at all, including admins, before the pick
  lands) can see *which* Pokémon are queued. Treat the count as public and the
  contents as secret — this split is deliberate, don't collapse it.

## Stack

- **axum** + **askama** for server-rendered HTML. Same shape as
  `../axum-askama-sample` (a working reference for router, error, and template
  wiring).
- **SQLite** via `sqlx` (`runtime-tokio-rustls`, `sqlite`, `macros`, `migrate`).
  Migrations in `migrations/`, offline query data in `.sqlx/` committed.
- **Tailwind** for styling, **Alpine.js** for the small amount of interactivity.
- Plain page refresh. No websockets, no polling — picks happen over days.
- Deployed with Docker Swarm (single replica); details in `deploy/`.
  The SQLite file lives on a mounted volume.

Rust 2024 edition. Copy the `[lints.rust]` and `[lints.clippy]` blocks from
`../carv_assesment/Cargo.toml` when setting up `Cargo.toml`.

## Rust guidelines

Condensed from the Microsoft Pragmatic Rust Guidelines. Full text: `docs/guidelines.md`.

### Errors
- Application code: `anyhow` for propagation. Don't mix app-level error crates.
- Library code: `thiserror`, canonical struct errors (not enums as the top-level
  type). Implement `From<UpstreamError>` instead of scattering `.map_err()`.
- Detected programming bugs → `panic!`. Runtime failures → `Result`.
- Panic messages include the relevant values and say what went wrong.
- Web handlers return `Result<impl IntoResponse, AppError>`, with `AppError`
  implementing `IntoResponse` — see `../axum-askama-sample/src/error.rs`.

### Types & API design
- All public types derive `Debug`. Sensitive types get a custom `Debug` that redacts.
- Types users read implement `Display`.
- Prefer `async fn foo()` over `fn foo() -> impl Future<Output = ...>`.
- Accept `impl AsRef<str>` / `AsRef<Path>` / `AsRef<[u8]>` where you don't need ownership.
- No `Arc<Mutex<T>>` or `Rc<RefCell<T>>` in public APIs — hide them behind `&T` / `&mut T` / `T`.
- Newtypes encoding invariants enforce them at construction (fallible `TryFrom` /
  `from_*`), with no public inner fields. Points and budgets are good candidates.
- Names: ≤2 short words, no weasel words (`Manager`, `Service`, `Factory`) — use
  domain names (`Draft`, `Roster`, `Pick`) or `Builder`.
- Always provide `Foo::new()`. 4+ init permutations → `Foo::builder()` / `.build()`.
- Builder setters are infallible; validation happens in `.build() -> Result<_>`.
- Essential functionality is inherent (`impl Foo`), not only behind traits.

### Modules & visibility
- Each public item reachable by exactly one path. No `pub use` aliases creating
  duplicate paths, no glob re-exports, no `prelude` modules.
- Integration tests (public API only) go in `tests/`, not `mod tests {}`.

### Async & concurrency
- Public futures must be `Send`.
- Shared service types use `Arc<Inner>` internally and `Clone` cheaply.

### `unsafe`
- Effectively never needed here. If it appears, every block gets a plain-text
  soundness comment. No unsound code, ever.

### Lints & docs
- `#[expect(lint, reason = "...")]` over `#[allow(lint)]`.
- Public items get doc comments; first sentence ≤15 words on one line.
  Sections where applicable: `# Examples`, `# Errors`, `# Panics`.
- No design narratives ("we chose X over Y") in user-facing docs.
- `tracing` events with named structured fields — never `println!` / `dbg!`.
- Magic constants are named `const`s with a comment explaining the value.

## Domain rules

These are correctness-critical. Enforce them in the database where a constraint
can express it, and in types where it can't.

- A pick must not exceed the coach's **remaining** budget (their personal budget
  minus spent points). Budget is per-membership.
- Rosters are **8 to 12** Pokémon. A pick must leave enough points to reach 8;
  it need not leave enough to reach 12. Running out of points past 8 ends that
  coach's draft, which is a legitimate outcome, not an error.
- A coach who is full (12), done (clicked it at ≥8), or broke (cannot afford the
  cheapest available) is skipped. Turn order is a snake over the coaches still
  active, so the order **compresses** as coaches drop out.
- "I'm done" is final for the coach; only an admin can clear it.
- A species can be drafted once per league. Enforce with a unique constraint.
- Picks are append-only; correcting one is an admin action that leaves a trail.
- Queued pick *contents* are private to their owner. Queued pick *counts* are public.
- Admin-only mutations check the admin flag server-side on every request. Never
  rely on the template hiding a button.

## Testing

Non-trivial logic leaves one runnable check behind — the smallest thing that
fails if the logic breaks. Budget math, draft-order/snake calculation, and pick
validation each need a test. No frameworks beyond `#[test]` / `#[tokio::test]`.
Trivial one-liners need no test.

## Design

`docs/DESIGN.md` holds the feature set and the decisions behind it. Read it before
building anything; it settles draft format, budget rules, queue semantics, and
schema. Its "Open questions" section lists what is still genuinely undecided —
ask rather than guess.

Decisions most likely to be assumed wrongly:

- The queue is **positional**, not a priority list. Slot N is for pick N; a
  sniped slot stalls the draft rather than promoting slot N+1.
- Roster size is a **range**, not a constant. There is no `roster_size`; it is
  `min_roster` (8) and `max_roster` (12), and the reserve rule protects only the
  minimum.
- Auth is Discord OAuth (`identify` scope). Discord messaging is channel
  webhooks only (see `docs/DISCORD.md`). Showdown replays are fetched and parsed for match
  results (see `docs/SEASON.md`).
- Season play (schedule, results, standings, playoffs) is specified in
  `docs/SEASON.md`. Read it before touching those.
- Trades and free agency are specified in `docs/TRADES.md`. After the draft, the
  roster is `roster_entry`, not `pick`.
- Replay stats (KO credit, previewed vs. played) are specified in `docs/STATS.md`.
- Visual design (layout, palette, components) is in `docs/STYLE.md`. Follow it in
  every template.

## Deploying

Deploy instructions live in the gitignored `deploy/` directory:
@deploy/CLAUDE.md
