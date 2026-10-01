//! Per-Pokémon stats read from a replay log. See `docs/STATS.md`.
//!
//! This is a second pass over the log `replays::parse` already accepted, so it
//! never fails: whatever it can't attribute is left uncredited.

use std::collections::{BTreeSet, HashMap};

use sqlx::SqliteConnection;

use crate::db::Db;
use crate::replays::{self, to_id};

/// One previewed Pokémon's game, as read from a replay log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[expect(clippy::struct_excessive_bools, reason = "mirrors the 0/1 columns of game_mon")]
pub struct GameMon {
    /// Index of the side it played for: 0 for p1, 1 for p2.
    pub side: usize,
    /// Species as shown in team preview, like `Metagross`.
    pub previewed: String,
    /// Final form in the log, like `Metagross-Mega`.
    pub species: String,
    /// Switched in at least once.
    pub played: bool,
    /// Was on the field before the first turn.
    pub led: bool,
    /// Mega Evolved.
    pub mega: bool,
    /// Fainted.
    pub fainted: bool,
    /// Faints it caused by its own move.
    pub direct_kos: i64,
    /// Faints it caused by status, weather, an ability, an item, or Destiny Bond.
    pub passive_kos: i64,
    /// Moves it used.
    pub moves: BTreeSet<String>,
    /// Items the game showed it holding.
    pub items: BTreeSet<String>,
    /// Abilities the game showed it having.
    pub abilities: BTreeSet<String>,
}

/// Who a faint is credited to, if anyone.
#[derive(Clone, Copy)]
enum Credit {
    Direct(usize),
    Passive(usize),
}

/// Reads one row per previewed Pokémon, p1's six then p2's, from a battle log.
#[must_use]
pub fn read(log: &str) -> Vec<GameMon> {
    let mut r = Reader::default();
    for line in log.lines() {
        r.line(line);
    }
    r.mons
}

/// What a re-read of a season's replays did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reread {
    /// Games whose stats were rewritten from their log.
    pub read: usize,
    /// Games with no log, because their replay no longer fetches.
    pub missing: usize,
}

/// One Pokémon's stored game, as shown under a match's games.
#[derive(Debug, Clone)]
#[expect(clippy::struct_excessive_bools, reason = "mirrors the 0/1 columns of game_mon")]
pub struct MonLine {
    pub game_id: i64,
    pub coach_id: i64,
    /// The roster entry's slug; `None` when it didn't match the roster.
    pub slug: Option<String>,
    /// The roster entry's name, else Showdown's species.
    pub name: String,
    pub played: bool,
    pub led: bool,
    pub mega: bool,
    pub fainted: bool,
    pub direct_kos: i64,
    pub passive_kos: i64,
    /// Revealed moves, comma-separated; empty when none were seen.
    pub moves: String,
    pub items: String,
    pub abilities: String,
}

impl MonLine {
    /// Direct and passive KOs together.
    #[must_use]
    pub fn kos(&self) -> i64 {
        self.direct_kos + self.passive_kos
    }
}

/// One Pokémon's season with one coach, summed over their replay games.
#[derive(Debug, Clone)]
pub struct MonStats {
    pub coach_id: i64,
    pub pokemon_id: i64,
    /// Team name, falling back to the coach's Discord name.
    pub team: String,
    pub slug: String,
    pub name: String,
    /// Tier-list points; `None` if the season's tier list doesn't price it.
    pub cost: Option<i64>,
    /// Games it was one of the six.
    pub previewed: i64,
    /// The coach's replay games while it was on their roster.
    pub eligible: i64,
    pub played: i64,
    pub led: i64,
    /// Games it played that its side won.
    pub won: i64,
    pub direct_kos: i64,
    pub passive_kos: i64,
    pub fainted: i64,
    pub mega: i64,
    /// Moves, items and abilities seen, with how many games each was seen in.
    pub moves: Vec<Seen>,
    pub items: Vec<Seen>,
    pub abilities: Vec<Seen>,
}

/// A revealed move, item or ability and the number of games it was seen in.
#[derive(Debug, Clone)]
pub struct Seen {
    pub name: String,
    pub games: i64,
}

impl MonStats {
    /// Direct and passive KOs together.
    #[must_use]
    pub fn kos(&self) -> i64 {
        self.direct_kos + self.passive_kos
    }

    /// KOs minus faints.
    #[must_use]
    pub fn net(&self) -> i64 {
        self.kos() - self.fainted
    }

    /// Whether it's a Mega entry, the only kind with a Mega rate.
    #[must_use]
    pub fn is_mega(&self) -> bool {
        self.slug.ends_with("-mega")
    }

    /// Previewed ÷ eligible games, as a whole percent.
    #[must_use]
    pub fn preview_rate(&self) -> Option<i64> {
        percent(self.previewed, self.eligible)
    }

    /// Led ÷ played, as a whole percent.
    #[must_use]
    pub fn lead_rate(&self) -> Option<i64> {
        percent(self.led, self.played)
    }

    /// Won ÷ played, as a whole percent.
    #[must_use]
    pub fn win_rate(&self) -> Option<i64> {
        percent(self.won, self.played)
    }

    /// Mega Evolved ÷ played, as a whole percent.
    #[must_use]
    pub fn mega_rate(&self) -> Option<i64> {
        percent(self.mega, self.played)
    }
}

/// `part` of `whole` as a rounded whole percent; `None` when `whole` is 0.
fn percent(part: i64, whole: i64) -> Option<i64> {
    (whole > 0).then(|| (part * 200 + whole) / (whole * 2))
}

/// One coach's season, summed over their replay games.
#[derive(Debug, Clone)]
pub struct CoachStats {
    pub coach_id: i64,
    pub team: String,
    pub games: i64,
    /// KOs credited to their Pokémon.
    pub dealt: i64,
    /// KOs credited to opponents against them.
    pub taken: i64,
    /// Their Pokémon's faints.
    pub fainted: i64,
}

impl CoachStats {
    /// Faints nobody was credited for: hazards, recoil, their own partner.
    #[must_use]
    pub fn uncredited(&self) -> i64 {
        self.fainted - self.taken
    }
}

impl Db {
    /// Every Pokémon's season with each coach it played for, most KOs first.
    ///
    /// Only Pokémon that matched their coach's roster are included; anything
    /// else still counts in the coach table.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn mon_stats(&self, season_id: i64) -> Result<Vec<MonStats>, sqlx::Error> {
        let rows = sqlx::query!(
            r#"WITH gm AS (
                   SELECT gm.*, m.week, g.winner_coach_id FROM game_mon gm
                   JOIN coach co ON co.id = gm.coach_id
                   JOIN game g ON g.id = gm.game_id JOIN match m ON m.id = g.match_id
                   WHERE co.season_id = ?1
               ),
               coach_game AS (SELECT DISTINCT game_id, coach_id, week FROM gm)
               SELECT gm.coach_id AS "coach_id!", gm.pokemon_id AS "pokemon_id!",
                      COALESCE(co.team_name, pe.discord_username) AS "team!: String",
                      p.slug, p.display_name, c.points AS "cost?",
                      COUNT(*) AS "previewed!: i64",
                      (SELECT COUNT(*) FROM coach_game cg
                       WHERE cg.coach_id = gm.coach_id AND EXISTS (
                           SELECT 1 FROM roster_entry r
                           WHERE r.coach_id = cg.coach_id AND r.pokemon_id = gm.pokemon_id
                             AND r.from_week <= cg.week AND (r.until_week IS NULL OR r.until_week > cg.week))
                      ) AS "eligible!: i64",
                      SUM(gm.played) AS "played!: i64", SUM(gm.led) AS "led!: i64",
                      SUM(gm.played AND gm.winner_coach_id = gm.coach_id) AS "won!: i64",
                      SUM(gm.direct_kos) AS "direct_kos!: i64", SUM(gm.passive_kos) AS "passive_kos!: i64",
                      SUM(gm.fainted) AS "fainted!: i64", SUM(gm.mega) AS "mega!: i64"
               FROM gm JOIN pokemon p ON p.id = gm.pokemon_id
               JOIN coach co ON co.id = gm.coach_id JOIN person pe ON pe.id = co.person_id
               LEFT JOIN cost c ON c.season_id = ?1 AND c.pokemon_id = gm.pokemon_id
               GROUP BY gm.coach_id, gm.pokemon_id
               ORDER BY SUM(gm.direct_kos + gm.passive_kos) DESC, SUM(gm.played) DESC, p.display_name"#,
            season_id
        )
        .fetch_all(self.pool())
        .await?;

        let seen = sqlx::query!(
            r#"SELECT gm.coach_id, gm.pokemon_id AS "pokemon_id!", r.kind, r.name, COUNT(*) AS "games!: i64"
               FROM game_mon_reveal r JOIN game_mon gm ON gm.id = r.game_mon_id
               JOIN coach co ON co.id = gm.coach_id
               WHERE co.season_id = ? AND gm.pokemon_id IS NOT NULL
               GROUP BY gm.coach_id, gm.pokemon_id, r.kind, r.name
               ORDER BY COUNT(*) DESC, r.name"#,
            season_id
        )
        .fetch_all(self.pool())
        .await?;
        let mut revealed: HashMap<(i64, i64, String), Vec<Seen>> = HashMap::new();
        for r in seen {
            revealed.entry((r.coach_id, r.pokemon_id, r.kind)).or_default().push(Seen { name: r.name, games: r.games });
        }
        let mut take = |coach: i64, pokemon: i64, kind: &str| revealed.remove(&(coach, pokemon, kind.to_owned())).unwrap_or_default();

        Ok(rows
            .into_iter()
            .map(|r| MonStats {
                moves: take(r.coach_id, r.pokemon_id, "move"),
                items: take(r.coach_id, r.pokemon_id, "item"),
                abilities: take(r.coach_id, r.pokemon_id, "ability"),
                coach_id: r.coach_id,
                pokemon_id: r.pokemon_id,
                team: r.team,
                slug: r.slug,
                name: r.display_name,
                cost: r.cost,
                previewed: r.previewed,
                eligible: r.eligible,
                played: r.played,
                led: r.led,
                won: r.won,
                direct_kos: r.direct_kos,
                passive_kos: r.passive_kos,
                fainted: r.fainted,
                mega: r.mega,
            })
            .collect())
    }

    /// Every coach's season from their replay games, most KOs dealt first.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn coach_stats(&self, season_id: i64) -> Result<Vec<CoachStats>, sqlx::Error> {
        sqlx::query_as!(
            CoachStats,
            r#"SELECT co.id AS "coach_id!", COALESCE(co.team_name, pe.discord_username) AS "team!: String",
                      COUNT(DISTINCT gm.game_id) AS "games!: i64",
                      COALESCE(SUM(gm.direct_kos + gm.passive_kos), 0) AS "dealt!: i64",
                      (SELECT COALESCE(SUM(o.direct_kos + o.passive_kos), 0) FROM game_mon o
                       WHERE o.coach_id <> co.id
                         AND o.game_id IN (SELECT game_id FROM game_mon WHERE coach_id = co.id)) AS "taken!: i64",
                      COALESCE(SUM(gm.fainted), 0) AS "fainted!: i64"
               FROM coach co JOIN person pe ON pe.id = co.person_id
               LEFT JOIN game_mon gm ON gm.coach_id = co.id
               WHERE co.season_id = ?
               GROUP BY co.id
               ORDER BY 4 DESC, 1"#,
            season_id
        )
        .fetch_all(self.pool())
        .await
    }

    /// Every stored Pokémon in a match's games: played first, leads first.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn match_mons(&self, match_id: i64) -> Result<Vec<MonLine>, sqlx::Error> {
        sqlx::query_as!(
            MonLine,
            r#"SELECT gm.game_id, gm.coach_id, p.slug AS "slug?", COALESCE(p.display_name, gm.species) AS "name!: String",
                      gm.played AS "played: bool", gm.led AS "led: bool", gm.mega AS "mega: bool",
                      gm.fainted AS "fainted: bool", gm.direct_kos, gm.passive_kos,
                      (SELECT COALESCE(group_concat(name, ', '), '') FROM (SELECT name FROM game_mon_reveal
                        WHERE game_mon_id = gm.id AND kind = 'move' ORDER BY name)) AS "moves!: String",
                      (SELECT COALESCE(group_concat(name, ', '), '') FROM (SELECT name FROM game_mon_reveal
                        WHERE game_mon_id = gm.id AND kind = 'item' ORDER BY name)) AS "items!: String",
                      (SELECT COALESCE(group_concat(name, ', '), '') FROM (SELECT name FROM game_mon_reveal
                        WHERE game_mon_id = gm.id AND kind = 'ability' ORDER BY name)) AS "abilities!: String"
               FROM game_mon gm JOIN game g ON g.id = gm.game_id LEFT JOIN pokemon p ON p.id = gm.pokemon_id
               WHERE g.match_id = ?
               ORDER BY gm.game_id, gm.played DESC, gm.led DESC, gm.id"#,
            match_id
        )
        .fetch_all(self.pool())
        .await
    }

    /// Rewrites a season's stats from each game's stored log.
    ///
    /// Results are never touched. A game uploaded before logs were kept has
    /// its replay fetched once and the log stored; one that won't fetch is
    /// counted as missing and keeps no stats.
    ///
    /// # Errors
    /// Fails only on a database error.
    pub async fn reread_stats(&self, season_id: i64) -> Result<Reread, sqlx::Error> {
        let unfetched = sqlx::query!(
            "SELECT g.id, g.replay_url FROM game g JOIN match m ON m.id = g.match_id
             WHERE m.season_id = ? AND g.log IS NULL",
            season_id
        )
        .fetch_all(self.pool())
        .await?;
        // ponytail: fetched one at a time, once per old game; fine for a season's worth.
        for g in unfetched {
            match replays::fetch(&g.replay_url).await {
                Ok(r) => {
                    sqlx::query!("UPDATE game SET log = ? WHERE id = ?", r.log, g.id).execute(self.pool()).await?;
                }
                Err(e) => tracing::warn!(game = g.id, error = %e, "replay no longer fetches"),
            }
        }

        let mut tx = self.pool().begin().await?;
        let games = sqlx::query!(
            r#"SELECT g.id AS "id!", g.log, g.winner_coach_id, m.week,
                      m.coach_a_id AS "a!", m.coach_b_id AS "b!"
               FROM game g JOIN match m ON m.id = g.match_id WHERE m.season_id = ?"#,
            season_id
        )
        .fetch_all(&mut *tx)
        .await?;
        let mut done = Reread { read: 0, missing: 0 };
        for g in games {
            sqlx::query!("DELETE FROM game_mon WHERE game_id = ?", g.id).execute(&mut *tx).await?;
            let Some(log) = g.log else {
                done.missing += 1;
                continue;
            };
            // Sides from the stored winner, not from Showdown names that may have changed since.
            let loser = if g.winner_coach_id == g.a { g.b } else { g.a };
            let coaches = match replays::winner_side(&log) {
                Ok(0) => [g.winner_coach_id, loser],
                Ok(_) => [loser, g.winner_coach_id],
                Err(e) => {
                    // Its log was accepted on upload, so this is a parser regression.
                    tracing::warn!(game = g.id, error = %e, "stored log no longer reads");
                    done.missing += 1;
                    continue;
                }
            };
            store(&mut tx, g.id, g.week, coaches, &log).await?;
            done.read += 1;
        }
        tx.commit().await?;
        tracing::info!(season_id, read = done.read, missing = done.missing, "stats re-read");
        Ok(done)
    }
}

/// Stores a game's stats: a `game_mon` row per previewed Pokémon, plus its reveals.
///
/// `coaches` are the coaches on p1's and p2's sides. Each Pokémon is matched
/// against its coach's roster for `week`; one that doesn't match is stored
/// without a `pokemon_id`.
///
/// # Errors
/// Fails only on a database error. Nothing in the log is rejected.
pub(crate) async fn store(
    conn: &mut SqliteConnection,
    game_id: i64,
    week: i64,
    coaches: [i64; 2],
    log: &str,
) -> Result<(), sqlx::Error> {
    let mut rosters = Vec::new();
    for coach in coaches {
        let roster = sqlx::query!(
            "SELECT r.pokemon_id, p.slug FROM roster_entry r JOIN pokemon p ON p.id = r.pokemon_id
             WHERE r.coach_id = ? AND r.from_week <= ? AND (r.until_week IS NULL OR r.until_week > ?)",
            coach,
            week,
            week
        )
        .fetch_all(&mut *conn)
        .await?;
        rosters.push(roster.into_iter().map(|r| (r.pokemon_id, r.slug)).collect::<Vec<_>>());
    }

    for m in read(log) {
        let coach = coaches[m.side];
        let pokemon_id = resolve(&m.species, &rosters[m.side]);
        // The same species twice on one side can't happen in VGC; if a log
        // claims it, the first row stands rather than failing the upload.
        let Some(id) = sqlx::query_scalar!(
            "INSERT INTO game_mon (game_id, coach_id, pokemon_id, species, played, led, mega, fainted, direct_kos, passive_kos)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING RETURNING id AS \"id!\"",
            game_id,
            coach,
            pokemon_id,
            m.species,
            m.played,
            m.led,
            m.mega,
            m.fainted,
            m.direct_kos,
            m.passive_kos
        )
        .fetch_optional(&mut *conn)
        .await?
        else {
            continue;
        };
        let reveals = m.moves.iter().map(|n| ("move", n))
            .chain(m.items.iter().map(|n| ("item", n)))
            .chain(m.abilities.iter().map(|n| ("ability", n)));
        for (kind, name) in reveals {
            sqlx::query!("INSERT INTO game_mon_reveal (game_mon_id, kind, name) VALUES (?, ?, ?)", id, kind, name)
                .execute(&mut *conn)
                .await?;
        }
    }
    Ok(())
}

/// The roster entry a replay species is, compared by Showdown ID.
///
/// The exact form wins; otherwise the one entry sharing its base species, so
/// a Mega that never Mega Evolved (`Metagross`) finds `metagross-mega`.
fn resolve(species: &str, roster: &[(i64, String)]) -> Option<i64> {
    let base = |s: &str| to_id(s.split('-').next().unwrap_or(s));
    let id = to_id(species);
    if let Some((p, _)) = roster.iter().find(|(_, slug)| to_id(slug) == id) {
        return Some(*p);
    }
    let mut same = roster.iter().filter(|(_, slug)| base(slug) == base(species));
    match (same.next(), same.next()) {
        (Some((p, _)), None) => Some(*p),
        _ => None,
    }
}

#[derive(Default)]
struct Reader {
    mons: Vec<GameMon>,
    /// Field position (`p1a`) to the index of the Pokémon in it.
    slots: HashMap<String, usize>,
    /// The Pokémon whose move is resolving; damage without `[from]` is its.
    mover: Option<usize>,
    /// Who inflicted each Pokémon's current status.
    status_by: HashMap<usize, usize>,
    /// Who set the current weather.
    weather_by: Option<usize>,
    /// A Pokémon whose Destiny Bond just activated.
    destiny: Option<usize>,
    /// Credit for a Pokémon that dropped to 0 HP, paid out on its `faint`.
    pending: HashMap<usize, Credit>,
    /// Turn 1 has started, so switches are no longer leads.
    started: bool,
}

/// The species in a details string: `Metagross-Mega` from `Metagross-Mega, L50, shiny`.
fn species_of(details: &str) -> &str {
    details.split(',').next().unwrap_or(details).trim()
}

/// The side a position like `p1a` or `p2` belongs to.
fn side_of(pos: &str) -> Option<usize> {
    match pos.get(..2) {
        Some("p1") => Some(0),
        Some("p2") => Some(1),
        _ => None,
    }
}

impl Reader {
    #[expect(clippy::too_many_lines, reason = "one arm per log line kind reads best as one match")]
    fn line(&mut self, line: &str) {
        let mut parts = line.split('|').skip(1);
        let Some(kind) = parts.next() else { return };
        let (args, tags): (Vec<&str>, Vec<&str>) = parts.partition(|p| !p.starts_with('['));
        let tag = |name: &str| tags.iter().find_map(|t| t.strip_prefix(name)).map(str::trim);
        let from = tag("[from]");
        let of = tag("[of]").and_then(|p| self.at(p));
        let first = args.first().and_then(|p| self.at(p));

        match kind {
            "poke" => {
                if let (Some(side), Some(details)) = (args.first().and_then(|p| side_of(p)), args.get(1)) {
                    self.add(side, species_of(details));
                }
            }
            "switch" | "drag" | "replace" => self.switch(kind, &args),
            "detailschange" => {
                if let (Some(i), Some(details)) = (first, args.get(1)) {
                    species_of(details).clone_into(&mut self.mons[i].species);
                }
            }
            "-mega" => {
                if let Some(i) = first {
                    self.mons[i].mega = true;
                    self.reveal_item(i, args.get(2));
                }
            }
            "move" => {
                self.mover = first;
                self.destiny = None;
                // Moves called by another (Dancer, Metronome) aren't in the set.
                let own = from.is_none_or(|f| f == "lockedmove");
                if let (Some(i), Some(m), true) = (first, args.get(1), own) {
                    self.mons[i].moves.insert((*m).to_owned());
                }
            }
            "-damage" => {
                let ko = args.get(1).is_some_and(|hp| hp.starts_with("0 ") || *hp == "0");
                if let (Some(i), true) = (first, ko)
                    && let Some(c) = self.credit(i, from, of)
                {
                    self.pending.insert(i, c);
                }
            }
            "faint" => {
                if let Some(i) = first {
                    self.faint(i);
                }
            }
            "-status" => {
                if let Some(i) = first {
                    let by = if from.is_some() { of } else { self.mover };
                    match by {
                        Some(by) => self.status_by.insert(i, by),
                        None => self.status_by.remove(&i),
                    };
                }
            }
            "-weather" => match args.first() {
                Some(&"none") => self.weather_by = None,
                _ if tags.contains(&"[upkeep]") => {}
                _ => self.weather_by = of.or(self.mover),
            },
            "-activate" => {
                if let (Some(i), Some(effect)) = (first, args.get(1)) {
                    if *effect == "move: Destiny Bond" {
                        self.destiny = Some(i);
                    }
                    self.reveal_effect(i, Some(effect));
                }
            }
            "-item" if !from.is_some_and(|f| f.starts_with("move:")) => {
                if let Some(i) = first {
                    self.reveal_item(i, args.get(1));
                }
            }
            "-enditem" => {
                if let Some(i) = first {
                    self.reveal_item(i, args.get(1));
                }
            }
            "-ability" => {
                // `[from] ability: Trace` names the holder's ability; the one
                // shown is what it copied.
                if let Some(i) = first {
                    match from {
                        Some(f) => self.reveal_effect(i, Some(&f)),
                        None => {
                            if let Some(a) = args.get(1) {
                                self.mons[i].abilities.insert((*a).to_owned());
                            }
                        }
                    }
                }
                return;
            }
            "turn" => {
                self.started = true;
                self.mover = None;
            }
            "" | "upkeep" => self.mover = None,
            _ => {}
        }

        // `[from] item: X` / `[from] ability: X` belong to the `[of]` Pokémon
        // when there is one (Rocky Helmet, Flame Body), else the line's own.
        if let Some(i) = of.or(first) {
            self.reveal_effect(i, from.as_ref());
        }
    }

    /// The Pokémon at a position like `p1a: Nickname`.
    fn at(&self, pokemon: &str) -> Option<usize> {
        let (pos, _) = pokemon.split_once(':')?;
        (pos.len() == 3).then_some(())?;
        self.slots.get(pos).copied()
    }

    fn add(&mut self, side: usize, species: &str) -> usize {
        self.mons.push(GameMon { side, previewed: species.to_owned(), species: species.to_owned(), ..GameMon::default() });
        self.mons.len() - 1
    }

    /// The previewed Pokémon a switched-in species is. Preview shows the base
    /// species (`Metagross`, or `Urshifu-*`) while a switch can show a form.
    fn find(&mut self, side: usize, species: &str) -> usize {
        let id = to_id(species);
        let on_side = || self.mons.iter().enumerate().filter(|(_, m)| m.side == side);
        let exact = on_side().find(|(_, m)| to_id(&m.previewed) == id || to_id(&m.species) == id);
        let base = || {
            on_side()
                .map(|(i, m)| (i, to_id(m.previewed.trim_end_matches("-*"))))
                .filter(|(_, b)| id.starts_with(b.as_str()))
                .max_by_key(|(_, b)| b.len())
        };
        match exact.map(|(i, _)| i).or_else(|| base().map(|(i, _)| i)) {
            Some(i) => i,
            // Not in preview: still counted, so nothing played goes missing.
            None => self.add(side, species),
        }
    }

    fn switch(&mut self, kind: &str, args: &[&str]) {
        let (Some(pokemon), Some(details)) = (args.first(), args.get(1)) else { return };
        let Some((pos, _)) = pokemon.split_once(':') else { return };
        let Some(side) = side_of(pos) else { return };
        let species = species_of(details);
        let i = self.find(side, species);
        self.slots.insert(pos.to_owned(), i);
        let m = &mut self.mons[i];
        m.played = true;
        m.led |= !self.started;
        species.clone_into(&mut m.species);
        // Illusion breaking mid-move keeps the move resolving; a real switch ends it.
        if kind != "replace" {
            self.mover = None;
        }
    }

    /// Who gets the KO if the damage on this line faints `target`.
    fn credit(&self, target: usize, from: Option<&str>, of: Option<usize>) -> Option<Credit> {
        let Some(from) = from else { return self.mover.map(Credit::Direct) };
        if let Some(of) = of {
            return Some(Credit::Passive(of));
        }
        match from.to_ascii_lowercase().as_str() {
            "brn" | "psn" | "tox" => self.status_by.get(&target).copied().map(Credit::Passive),
            "sandstorm" | "hail" => self.weather_by.map(Credit::Passive),
            _ => None,
        }
    }

    fn faint(&mut self, i: usize) {
        self.mons[i].fainted = true;
        let credit = self.pending.remove(&i).or(self.destiny.map(Credit::Passive));
        let (by, direct) = match credit {
            Some(Credit::Direct(by)) => (by, true),
            Some(Credit::Passive(by)) => (by, false),
            None => return,
        };
        // Self-KOs, partners, and your own weather credit no one.
        if self.mons[by].side == self.mons[i].side {
            return;
        }
        let m = &mut self.mons[by];
        if direct { m.direct_kos += 1 } else { m.passive_kos += 1 }
    }

    fn reveal_item(&mut self, i: usize, item: Option<&&str>) {
        if let Some(item) = item.filter(|s| !s.is_empty()) {
            self.mons[i].items.insert((*item).to_owned());
        }
    }

    /// Records an effect like `item: Leftovers` or `ability: Rough Skin`.
    fn reveal_effect(&mut self, i: usize, effect: Option<&&str>) {
        let Some(effect) = effect else { return };
        let m = &mut self.mons[i];
        if let Some(item) = effect.strip_prefix("item:") {
            m.items.insert(item.trim().to_owned());
        } else if let Some(ability) = effect.strip_prefix("ability:") {
            m.abilities.insert(ability.trim().to_owned());
        }
    }
}
