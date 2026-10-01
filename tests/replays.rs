//! Replay parsing, game tallies, and attaching replays to matches.

use pokemon_draft_site::db::{Db, Person};
use pokemon_draft_site::replays::{Replay, ReplayError, fetch, parse};
use pokemon_draft_site::results::{Entry, GameScore, ResultError, Score, tally};

#[test]
fn reads_the_real_replay() {
    let json = include_str!("replays/regmc-sweep.json");
    let r = parse("https://replay.pokemonshowdown.com/x", json).expect("parses");
    assert_eq!(r.id, "gen9championsvgc2026regmc-2688471452");
    assert_eq!(r.format, "gen9championsvgc2026regmc");
    assert_eq!(r.players, ["BelowAvrg".to_owned(), "aspen the dragon".to_owned()]);
    assert_eq!(r.winner, 0);
    // Six in team preview, four brought: p1 lost none, p2 lost all four.
    assert_eq!(r.remaining, [4, 0]);
    assert!(r.log.contains("|win|BelowAvrg"), "log kept whole");
}

#[test]
fn a_battle_without_a_winner_is_refused() {
    let json = r#"{"id":"x-1","formatid":"f","log":"|player|p1|A|\n|player|p2|B|\n|teamsize|p1|4\n|teamsize|p2|4\n|tie\n"}"#;
    assert!(matches!(parse("u", json), Err(ReplayError::NoWinner)));
}

#[tokio::test]
async fn only_showdown_replay_links_are_fetched() {
    for url in ["https://example.com/gen9-1", "http://127.0.0.1/x", "replay.pokemonshowdown.com/../etc"] {
        assert!(matches!(fetch(url).await, Err(ReplayError::BadUrl)), "{url}");
    }
}

#[test]
fn tally_decides_only_once_enough_games_are_won() {
    let g = |a_won, a_remaining, b_remaining| GameScore { a_won, a_remaining, b_remaining };
    assert_eq!(tally(3, &[g(true, 2, 0)]), None, "one game of three decides nothing");
    assert_eq!(tally(3, &[g(true, 2, 0), g(true, 3, 0)]), Some((true, 5)));
    // B wins 2-1: +1, +1, and A's game at 4 left counts against B.
    assert_eq!(tally(3, &[g(false, 0, 1), g(true, 4, 0), g(false, 0, 1)]), Some((false, -2)));
    assert_eq!(tally(1, &[g(false, 0, 3)]), Some((false, 3)));
    // KOs dealt minus taken: B forfeited mid-game with 3 standing to A's 1.
    assert_eq!(tally(1, &[g(true, 1, 3)]), Some((true, -2)));
}

struct League {
    db: Db,
    game: i64,
    a: i64,
    b: i64,
    coach: Person,
}

/// Two coaches, Ash (slot A) and Misty (slot B), in one Bo3 regular-season match.
async fn league() -> League {
    let db = Db::connect("sqlite::memory:").await.expect("connect");
    let season = sqlx::query_scalar!(
        "INSERT INTO season (name, min_roster, max_roster, is_active, format)
         VALUES ('S1', 8, 12, 1, 'gen9championsvgc2026regmc') RETURNING id"
    )
    .fetch_one(db.pool())
    .await
    .expect("season");
    let mut people = Vec::new();
    for (n, showdown) in [(1u8, "Ash K."), (2, "misty")] {
        let id = db.person_by_hand(&format!("1000000000000000{n:02}"), &format!("p{n}")).await.expect("person");
        db.set_member_fields(id, Some(showdown), false).await.expect("showdown name");
        db.add_coach(season, id, 100, i64::from(n)).await.expect("coach");
        people.push(db.person(id).await.expect("read").expect("exists"));
    }
    let ids: Vec<i64> = db.coaches(season).await.expect("coaches").iter().map(|c| c.id).collect();
    let (a, b) = (ids[0], ids[1]);
    let game = sqlx::query_scalar!(
        "INSERT INTO match (season_id, week, coach_a_id, coach_b_id) VALUES (?, 1, ?, ?) RETURNING id AS \"id!\"",
        season,
        a,
        b
    )
    .fetch_one(db.pool())
    .await
    .expect("match");
    League { db, game, a, b, coach: people.remove(0) }
}

/// A replay with Misty as p1, so sides have to be swapped onto the match's slots.
fn replay(n: u32, misty_won: bool, left: i64) -> Replay {
    Replay {
        id: format!("gen9championsvgc2026regmc-{n}"),
        url: format!("https://replay.pokemonshowdown.com/gen9championsvgc2026regmc-{n}"),
        format: "gen9championsvgc2026regmc".to_owned(),
        players: ["Misty".to_owned(), "ashk".to_owned()],
        winner: usize::from(!misty_won),
        remaining: if misty_won { [left, 0] } else { [0, left] },
        log: String::new(),
    }
}

#[tokio::test]
async fn replays_score_the_match_once_they_decide_it() {
    let l = league().await;
    l.db.set_result(l.game, &l.coach, Some(Entry { winner: l.b, score: Score::Forfeit })).await.expect("manual");

    l.db.add_game(l.game, &l.coach, &replay(1, false, 3)).await.expect("game 1");
    let m = l.db.match_by_id(l.game).await.expect("read").expect("exists");
    assert_eq!(m.winner, Some(l.b), "one game leaves the manual result alone");

    l.db.add_game(l.game, &l.coach, &replay(2, true, 1)).await.expect("game 2");
    let added = l.db.add_game(l.game, &l.coach, &replay(3, false, 2)).await.expect("game 3");
    let won = added.decided.expect("game 3 decides it");
    assert_eq!((added.game, won.winner, won.won, won.lost, won.differential), (3, l.a, 2, 1, 4));
    let m = l.db.match_by_id(l.game).await.expect("read").expect("exists");
    // Ash (slot A) won 2-1: +3 and +2, minus Misty's 1.
    assert_eq!((m.winner, m.differential, m.is_forfeit), (Some(l.a), Some(4), false));

    let games = l.db.games(l.game).await.expect("games");
    assert_eq!(games[0].winner_remaining, 3, "sides swapped onto slots");
    let history = l.db.result_history(l.game).await.expect("history");
    assert_eq!(history.last().map(|e| e.source.as_str()), Some("replay"));

    let err = l.db.add_game(l.game, &l.coach, &replay(4, true, 4)).await;
    assert!(matches!(err, Err(ResultError::Decided)), "got: {err:?}");
}

#[tokio::test]
async fn refuses_replays_that_do_not_belong() {
    let l = league().await;

    let mut wrong_format = replay(1, true, 2);
    wrong_format.format = "gen9vgc2025regh".to_owned();
    let err = l.db.add_game(l.game, &l.coach, &wrong_format).await;
    assert!(matches!(err, Err(ResultError::WrongFormat { .. })), "got: {err:?}");

    let mut stranger = replay(1, true, 2);
    stranger.players[1] = "brock".to_owned();
    let err = l.db.add_game(l.game, &l.coach, &stranger).await;
    assert!(matches!(&err, Err(ResultError::UnknownPlayer(n)) if n == "brock"), "got: {err:?}");

    l.db.add_game(l.game, &l.coach, &replay(1, true, 2)).await.expect("first upload");
    let err = l.db.add_game(l.game, &l.coach, &replay(1, true, 2)).await;
    assert!(matches!(err, Err(ResultError::AlreadyAttached)), "got: {err:?}");
    assert_eq!(l.db.games(l.game).await.expect("games").len(), 1);
}

#[tokio::test]
async fn removing_a_game_rescores_a_replay_result_only() {
    let l = league().await;
    l.db.add_game(l.game, &l.coach, &replay(1, false, 3)).await.expect("game 1");
    l.db.add_game(l.game, &l.coach, &replay(2, false, 2)).await.expect("game 2");
    let games = l.db.games(l.game).await.expect("games");

    l.db.remove_game(l.game, games[1].id, &l.coach).await.expect("remove");
    let m = l.db.match_by_id(l.game).await.expect("read").expect("exists");
    assert_eq!(m.winner, None, "one game of three no longer decides it");
    assert_eq!(l.db.result_history(l.game).await.expect("history").len(), 2, "set, then cleared");

    // The removed replay can go back on; a hand-entered result survives a removal.
    l.db.add_game(l.game, &l.coach, &replay(2, false, 2)).await.expect("re-add");
    l.db.set_result(l.game, &l.coach, Some(Entry { winner: l.b, score: Score::Forfeit })).await.expect("manual");
    l.db.remove_game(l.game, games[0].id, &l.coach).await.expect("remove");
    let m = l.db.match_by_id(l.game).await.expect("read").expect("exists");
    assert_eq!((m.winner, m.is_forfeit), (Some(l.b), true));

    let err = l.db.remove_game(l.game, games[0].id, &l.coach).await;
    assert!(matches!(err, Err(ResultError::NoMatch)), "already gone, got: {err:?}");
}

/// Names Ash and Misty as the real replay's players, gives Ash a roster, and attaches it.
async fn attach_sweep(l: &League) {
    let misty = sqlx::query_scalar!("SELECT person_id FROM coach WHERE id = ?", l.b)
        .fetch_one(l.db.pool())
        .await
        .expect("misty");
    l.db.set_member_fields(l.coach.id, Some("BelowAvrg"), false).await.expect("ash");
    l.db.set_member_fields(misty, Some("aspen the dragon"), false).await.expect("misty");
    // Bellibolt only joins Ash in week 2, after this week-1 match.
    for (slug, from_week) in [("greninja-mega", 1), ("mawile-mega", 1), ("sableye", 1), ("bellibolt", 2)] {
        sqlx::query(
            "INSERT INTO roster_entry (season_id, coach_id, pokemon_id, from_week)
             SELECT season_id, id, (SELECT id FROM pokemon WHERE slug = ?), ? FROM coach WHERE id = ?",
        )
        .bind(slug)
        .bind(from_week)
        .bind(l.a)
        .execute(l.db.pool())
        .await
        .expect("roster");
    }

    let json = include_str!("replays/regmc-sweep.json");
    let replay = parse("https://replay.pokemonshowdown.com/gen9championsvgc2026regmc-2688471452", json).expect("parses");
    l.db.add_game(l.game, &l.coach, &replay).await.expect("attach");
}

#[tokio::test]
async fn attaching_a_replay_stores_its_stats_against_the_roster() {
    let l = league().await;
    attach_sweep(&l).await;

    let rows: Vec<(String, i64, Option<String>, i64)> = sqlx::query_as(
        "SELECT gm.species, gm.coach_id, p.slug, gm.direct_kos FROM game_mon gm
         LEFT JOIN pokemon p ON p.id = gm.pokemon_id ORDER BY gm.id",
    )
    .fetch_all(l.db.pool())
    .await
    .expect("game_mon");
    assert_eq!(rows.len(), 12, "every previewed Pokémon: {rows:?}");
    let row = |species: &str| rows.iter().find(|r| r.0 == species).unwrap_or_else(|| panic!("no {species}"));
    assert_eq!(row("Greninja-Mega").2.as_deref(), Some("greninja-mega"), "exact form");
    assert_eq!(row("Mawile").2.as_deref(), Some("mawile-mega"), "a Mega that never evolved finds its entry");
    assert_eq!(row("Sableye").2.as_deref(), Some("sableye"));
    assert_eq!(row("Bellibolt").2, None, "not on the roster that week");
    assert_eq!(row("Vanilluxe").2, None, "not on the roster at all");
    assert_eq!((row("Bellibolt").1, row("Bellibolt").3), (l.a, 2), "p1 is slot A");
    assert_eq!(row("Metagross-Mega").1, l.b);

    let reveals: Vec<(String, String)> = sqlx::query_as(
        "SELECT r.kind, r.name FROM game_mon_reveal r JOIN game_mon gm ON gm.id = r.game_mon_id
         WHERE gm.species = 'Metagross-Mega' ORDER BY r.kind, r.name",
    )
    .fetch_all(l.db.pool())
    .await
    .expect("reveals");
    let want = [("item", "Metagrossite"), ("move", "Meteor Mash"), ("move", "Protect")];
    assert_eq!(reveals, want.map(|(k, n)| (k.to_owned(), n.to_owned())));

    let lines = l.db.match_mons(l.game).await.expect("match_mons");
    let names: Vec<&str> = lines.iter().filter(|x| x.coach_id == l.a).map(|x| x.name.as_str()).collect();
    // Played first, leads first, the rest in preview order. Unmatched keep Showdown's name.
    assert_eq!(names, ["Greninja (Mega)", "Sableye", "Bellibolt", "Diggersby", "Vanilluxe", "Mawile (Mega)"]);
    let meta = lines.iter().find(|x| x.name == "Metagross-Mega").expect("metagross");
    assert_eq!((meta.moves.as_str(), meta.items.as_str(), meta.mega), ("Meteor Mash, Protect", "Metagrossite", true));

    let game = l.db.games(l.game).await.expect("games")[0].id;
    l.db.remove_game(l.game, game, &l.coach).await.expect("remove");
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM game_mon").fetch_one(l.db.pool()).await.expect("count");
    assert_eq!(left, 0, "stats go with the game");
}

#[tokio::test]
async fn rereading_rewrites_stats_and_counts_missing_logs() {
    let l = league().await;
    attach_sweep(&l).await;
    let count = || async {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM game_mon").fetch_one(l.db.pool()).await.expect("count");
        n
    };
    // Stale stats from an older parser, and a game from before logs were kept
    // whose replay can't be fetched.
    sqlx::query("UPDATE game_mon SET direct_kos = 9").execute(l.db.pool()).await.expect("stale");
    sqlx::query(
        "INSERT INTO game (match_id, replay_id, replay_url, winner_coach_id, a_remaining, b_remaining, created_by)
         VALUES (?, 'old-1', 'https://example.com/gone', ?, 1, 0, ?)",
    )
    .bind(l.game)
    .bind(l.a)
    .bind(l.coach.id)
    .execute(l.db.pool())
    .await
    .expect("old game");
    let season: i64 = sqlx::query_scalar("SELECT season_id FROM match WHERE id = ?")
        .bind(l.game)
        .fetch_one(l.db.pool())
        .await
        .expect("season");

    let r = l.db.reread_stats(season).await.expect("reread");
    assert_eq!((r.read, r.missing), (1, 1));
    assert_eq!(count().await, 12);
    let bellibolt: (i64, i64) =
        sqlx::query_as("SELECT coach_id, direct_kos FROM game_mon WHERE species = 'Bellibolt'")
            .fetch_one(l.db.pool())
            .await
            .expect("bellibolt");
    assert_eq!(bellibolt, (l.a, 2), "rewritten, with sides taken from the stored winner");
    let m = l.db.match_by_id(l.game).await.expect("read").expect("exists");
    assert_eq!(m.winner, None, "results untouched");
}

#[tokio::test]
async fn season_stats_sum_each_pokemon_and_coach() {
    let l = league().await;
    attach_sweep(&l).await;
    let season: i64 = sqlx::query_scalar("SELECT season_id FROM match WHERE id = ?")
        .bind(l.game)
        .fetch_one(l.db.pool())
        .await
        .expect("season");

    let mons = l.db.mon_stats(season).await.expect("mon_stats");
    let names: Vec<&str> = mons.iter().map(|x| x.name.as_str()).collect();
    // Only what matched Ash's roster; Misty has none. Most KOs first.
    assert_eq!(names, ["Greninja (Mega)", "Sableye", "Mawile (Mega)"]);
    let greninja = &mons[0];
    assert_eq!((greninja.previewed, greninja.eligible, greninja.played, greninja.won), (1, 1, 1, 1));
    assert_eq!((greninja.kos(), greninja.net(), greninja.mega_rate(), greninja.lead_rate()), (1, 1, Some(100), Some(100)));
    assert!(greninja.moves.iter().any(|m| m.name == "Dark Pulse" && m.games == 1), "{:?}", greninja.moves);
    let mawile = &mons[2];
    assert_eq!((mawile.preview_rate(), mawile.played, mawile.win_rate()), (Some(100), 0, None), "benched, no rate");

    let coaches = l.db.coach_stats(season).await.expect("coach_stats");
    let line = |id: i64| {
        let c = coaches.iter().find(|c| c.coach_id == id).expect("coach");
        (c.games, c.dealt, c.taken, c.uncredited())
    };
    assert_eq!(line(l.a), (1, 4, 0, 0));
    assert_eq!(line(l.b), (1, 0, 4, 0));
}
