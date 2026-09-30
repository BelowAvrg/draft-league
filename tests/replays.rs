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
    l.db.add_game(l.game, &l.coach, &replay(3, false, 2)).await.expect("game 3");
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
