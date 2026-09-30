# STYLE.md

The visual design of the site. Read before touching a template.

## The idea

**A quiet, dark interface where the Pokémon are the colour.**

The UI itself is near-monochrome: ink backgrounds, grey surfaces, white type.
All the colour on a page comes from sprites and type badges. That keeps it minimal
and professional, and still makes it unmistakably Pokémon. Strip out the sprites
and you get a clean modern tool. Put them back and it becomes a Pokédex.

Principles, in priority order:

1. **The Pokémon are the brand.** Sprites and type colours are the only saturated
   colour on screen. Chrome never competes with them.
2. **Monochrome chrome.** Hierarchy comes from lightness, weight and space, not hue.
   The primary action is white on ink, not a brand colour.
3. **Numbers are first-class.** Points, records and pick numbers use tabular
   figures, right-aligned, in columns that line up.
4. **Weight follows consequence.** Routine actions are quiet. Final and destructive
   actions look different and always confirm.
5. **Nods, not costume.** Pokémon references show up in structure (dex numbers,
   stat bars, party slots), never in novelty fonts or clip art.
6. **One system.** One layout, one nav, one component set. A page that needs
   something new adds it here first.

## Theme

Dark only, for now. Light mode is deferred, but every colour is a token so it
can be added later without touching templates.

Surfaces step up in lightness to show elevation. There are no drop shadows,
because they don't show on dark backgrounds.

| Token          | Value                    | Use                                    |
|----------------|--------------------------|----------------------------------------|
| `canvas`       | `#0b0b0f`                | page                                   |
| `surface`      | `#131318`                | cards, nav                             |
| `raised`       | `#1c1c23`                | hover rows, inputs, nested panels      |
| `line`         | `rgb(255 255 255 / 0.08)`| borders, dividers                      |
| `line-strong`  | `rgb(255 255 255 / 0.16)`| input borders, focus-adjacent          |
| `ink`          | `#f4f4f5`                | primary text, primary button fill      |
| `muted`        | `#a1a1aa`                | secondary text                         |
| `faint`        | `#71717a`                | tertiary text, disabled, passed picks  |

The cooler, near-black backgrounds keep sprites looking vivid instead of washed out.

### Status colours

These are small, desaturated, and used only for state. They appear as text, a
dot or a thin border, never as large fills, so they don't read as a type colour.

| Token     | Value     | Means                                 |
|-----------|-----------|---------------------------------------|
| `live`    | `#4ade80` | on the clock, available, success      |
| `warn`    | `#fbbf24` | done / out of points, needs attention |
| `danger`  | `#f87171` | error, destructive                    |
| `gold`    | `#f5c542` | a match winner's name                 |

"You" (your row, your match, your turn) is shown with an `ink` left edge
and a `raised` background. Emphasis comes from lightness, not colour.

### Type colours

These are the canonical 18. They live in `tailwind.config.js` as `type-<name>`
and are used **only** for type badges and the glow behind sprites. They never
appear on buttons, links or chrome.

| Type     | Fill      | Text  | Type     | Fill      | Text  |
|----------|-----------|-------|----------|-----------|-------|
| Normal   | `#A8A77A` | dark  | Flying   | `#A98FF3` | dark  |
| Fire     | `#EE8130` | dark  | Psychic  | `#F95587` | dark  |
| Water    | `#6390F0` | dark  | Bug      | `#A6B91A` | dark  |
| Electric | `#F7D02C` | dark  | Rock     | `#B6A136` | dark  |
| Grass    | `#7AC74C` | dark  | Ghost    | `#735797` | white |
| Ice      | `#96D9D6` | dark  | Dragon   | `#6F35FC` | white |
| Fighting | `#C22E28` | white | Dark     | `#705746` | white |
| Poison   | `#A33EA1` | white | Steel    | `#B7B7CE` | dark  |
| Ground   | `#E2BF65` | dark  | Fairy    | `#D685AD` | dark  |

"Dark" text is `#0b0b0f`. Every pairing above clears WCAG AA for small bold text.

## Type

**Inter**, self-hosted as woff2 under `static/fonts/`. **JetBrains Mono** for
IDs, CSV and dex numbers. Both are loaded with `font-display: swap`.

| Role           | Style                                                        |
|----------------|--------------------------------------------------------------|
| Page title     | 28px / semibold / tracking −0.02em                           |
| Section title  | 15px / semibold                                              |
| Body, tables   | 14px / regular                                               |
| Label, header  | 11px / medium / uppercase / tracking +0.06em / `muted`       |
| Numbers        | `tabular-nums`; semibold when it's the figure that matters   |
| Dex / pick no. | mono, `faint`, zero-padded: `#007`                           |

Use sentence case everywhere. Bold marks the one thing in a row that matters, such
as the Pokémon or the team.

## Layout

- `templates/base.html` owns `<head>`, the nav and flash messages. Every page
  extends it.
- **Nav**: sticky, `surface`, bottom `line`. Logo mark and league name on the left,
  then the page links, then the signed-in coach on the right. The current page is
  `ink`; the other links are `muted`. On phones the links become a horizontally
  scrolling row under the logo. There is no hamburger menu, because there are only
  six links.
- **Content**: `max-w-6xl`, 16px gutters on phones and 24px on desktop.
- **Page header**: sits on `canvas`, not in a card. It has a title, one `muted` meta
  line, and at most one primary action on the right.
- **Cards**: `surface`, 1px `line` border, 12px radius, 20px padding. A card
  has a title row and a body. Cards stack with 24px gaps and use a two-column grid
  on desktop when both columns stand alone (e.g. the board's coaches and recent
  picks).
- **Responsive tables**: tables in a card run edge to edge (`card p-0`, with
  the title padded separately). On phones, drop secondary columns
  (`hidden sm:table-cell`) and cap suffixes such as "/ 12" before resorting to
  sideways scrolling. Any grid or flex child that holds a scrolling table needs
  `min-w-0`, or it grows to fit the table and scrolling never happens.
  Likewise every `grid` gets `grid-cols-1` as its phone layout: without an
  explicit column, one long name stretches the track past the screen.
  Truncate long names on an inner block element; `truncate` has no effect on a
  table cell.
- Tap targets are at least 40px tall.

## Pokémon components

These carry the design, so they get the most care.

**Sprite.** Showdown's Gen 5-style pixel sprites, 96×96, self-hosted as
`static/sprites/<slug>.png`. They were chosen over 3D renders because they cover
every Pokémon in the format, including the new Megas. The 11 newest Megas have no
sprite yet and use their base species' sprite. Always render with the `sprite`
class (`image-rendering: pixelated`), and only at sizes that are exact multiples
or fractions of 96px: 96 in cards, 48 or 32 inline.

**Sprite glow.** Behind a card-size sprite sits a soft radial gradient in the
primary type colour at about 25% opacity, fading to transparent. It is the only
place type colour spreads beyond a badge.

**Type badge.** `type-badge type-<name>`. Solid fill, 10px uppercase semibold,
6px radius, and a fixed 64px width so dual types line up in columns. Every
`type-<name>` class sets `--type` (and `--on-type` for dark fills), so any element
can pick up its Pokémon's colour.

**Pokémon row** (lists, tables, queue): 32px sprite, then the name (semibold), then
the type badges, then the cost right-aligned. A drafted or unavailable Pokémon is
greyscale at 40% opacity. It is not struck through.

**Pokémon card** (rosters, the latest pick): a 96px sprite on its glow, then a row
with the name (truncated) on the left and the cost on the right, then the type
badges. The pick number sits in the top corner in mono.

**Party slots.** A roster is shown as its slots, not just a list of picks. Empty slots
up to the minimum of 8 are drawn as a dashed outline, so "how many do I still
need" is visible without reading a number. Slots past the minimum are not drawn.

## Stat bars

Budgets are drawn like base-stat bars from the Pokédex, as a thin 6px track on
`raised` with an `ink` fill. They show points spent against the budget. Where the
reserve rule applies, the reserve is marked on the bar as a hatched segment,
so "what I can actually spend" is visible. Use them wherever a budget appears
more than once on a page.

## Numbers

- Numeric columns are right-aligned and use `tabular-nums`.
- A point value always shows its number, followed by `pts` in `faint`.
- Counts against a cap read `7 / 12`. Points against a budget read `43 of 100`.
- Signed values always show their sign: `+3`, `−2`, `0`.

## Controls

**Buttons.** There are three kinds, all 36px tall (32px `sm` inside rows), with an 8px radius:

- *Primary*: `ink` fill with `canvas` text. There is at most one per view.
- *Secondary*: `raised` fill with a `line-strong` border and `ink` text.
- *Danger*: transparent, `danger` text and border. It always confirms.

Final actions ("I'm done") are danger-styled even though nothing is deleted.

**Inputs and selects**: `raised` fill with a `line-strong` border. Focus shows a
2px `ink` ring. The label goes above the field and help text below it in `muted`.

**Pokémon picker.** A scrolling list of Pokémon rows, each a visually hidden
radio input inside a `<label>`, with a filter box above it. The radios make it a
plain form that works without JavaScript; Alpine only adds the filter. The
selected row gets `raised` plus a ring through `has-[:checked]`. Use this
wherever someone chooses a Pokémon, never a `<select>`.

**Badges (status).** A dot plus a label in `muted` text, such as "● On the clock" or
"● Done". The dot takes the status colour. They are small and are not pills, so
they don't get confused with type badges.

**Callouts.** A `surface` panel with a 2px left edge in the status colour. They
hold flash messages and page-level state. Flash messages render only in `base.html`.

**Motion.** Only 150ms colour and opacity transitions on hover and focus. Nothing
moves on its own. The one exception is the on-the-clock dot, which may pulse gently.

## Nods

These are the only places game flavour appears:

- **Logo mark**: a minimal Pokéball, a circle split by a line, drawn in `ink`.
- **Dex-style numbers** for picks and draft positions: `#007`.
- **Stat bars** for budgets.
- **Party slots** for rosters.
- **Empty states**: a faint Pokéball outline over one sentence.

## Privacy is visible

Queue contents are secret and the queue count is public (see `CLAUDE.md`). The queue card
shows a small lock and "Only you can see this". A public queue shows only a count
("3 queued"), never an empty list that looks like missing data.

## Voice

Write short, plain sentences that state the consequence before the user acts.
Don't use "Click here", exclamation marks or emoji.

## Data

Types are stored in `pokemon.types`, space-separated with the primary type first.
Migration 0009 seeds them from Showdown's `pokedex.json`. A Pokémon added to the
format later needs its types and sprite added the same way.

## Where the system lives

- Tokens: `tailwind.config.js`
- Components and type colours: `static/src/app.css`
- Layout, nav and flash messages: `templates/base.html`. Handlers pass a
  `Layout` (in `src/web.rs`) as the template's `layout` field.
- Shared snippets (status dot, type badges, Pokéball): `templates/macros.html`.
  Call as `{% call ui::types(p.types) %}{% endcall %}`; askama requires the
  `endcall`.
- Buttons in table rows and tight spots add `btn-sm` (`btn btn-sm`,
  `btn-danger btn-sm`).
- Pokémon pickers use `ui::pick_row(listing, 48 or 32)` inside an element whose
  Alpine scope defines `q`. Signed numbers use `ui::signed(n)` (true minus sign).
- A week of matches is one card of slim rows (`templates/schedule.html`):
  Team A right-aligned, `vs` in the middle (`FF` for a forfeit), Team B
  left-aligned. The winner's name is semibold `gold` and the loser's is `muted`;
  the differential lives on the match page, not here. The last column has a
  fixed width so the middle column lines up across rows.
- Playoff matches render as scoreboard cards via `{% include "match_card.html" %}`,
  which expects `m`, `me` and `stage` in scope.
- The playoff cut line is a dashed `line-strong` top border on the first row below
  the cut.
- Leave a space between a Tailwind class and an adjacent askama tag
  (`22rem] {% endif %}`, not `22rem]{% endif %}`), or Tailwind's scanner can miss
  the class.
- Component selectors that style child elements (`.table th`) are wrapped in
  `:where()` so utility classes on the element still win.
- Reference pages: `templates/roster.html` (cards, party slots, budget bar),
  `templates/index.html` (tables, picker, callouts, buttons), `templates/draft.html`
  (the private queue) and `templates/admin.html` (dense editable tables, forms).
