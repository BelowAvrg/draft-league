//! Showdown replays: fetching one by link and reading the game out of its log.

use std::time::Duration;

use serde::Deserialize;

/// Where replays live. Only this host is ever fetched.
const HOST: &str = "https://replay.pokemonshowdown.com";

/// Long enough for a slow Showdown, short enough not to pin a request.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// One finished game, as read from a replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replay {
    /// Showdown's replay id, without any private-replay password.
    pub id: String,
    /// Link to the replay, password included so private replays stay viewable.
    pub url: String,
    /// Showdown format id, like `gen9championsvgc2026regmc`.
    pub format: String,
    /// Player names, p1 then p2.
    pub players: [String; 2],
    /// Index into `players` of the winner.
    pub winner: usize,
    /// Pokémon each side had left: brought minus fainted.
    pub remaining: [i64; 2],
    /// The battle log as Showdown stored it, kept for reading stats later.
    pub log: String,
}

/// Why a replay could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("that isn't a Showdown replay link (replay.pokemonshowdown.com/…)")]
    BadUrl,
    #[error("couldn't fetch that replay from Showdown; check the link and try again")]
    Fetch(#[from] reqwest::Error),
    #[error("couldn't read that replay")]
    Json(#[from] serde_json::Error),
    #[error("couldn't read that replay: {0}")]
    Unreadable(&'static str),
    #[error("that battle has no winner")]
    NoWinner,
}

/// A name as Showdown compares it: lowercase letters and digits only.
#[must_use]
pub fn to_id(name: &str) -> String {
    name.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// The replay path from a pasted link: `gen9…-123`, or `gen9…-123-abcpw` if private.
fn path_of(url: &str) -> Option<&str> {
    let rest = url.trim().trim_start_matches("https://").trim_start_matches("http://");
    let path = rest.strip_prefix("replay.pokemonshowdown.com/")?;
    let path = path.split(['?', '#']).next()?.trim_end_matches('/').trim_end_matches(".json");
    let ok = !path.is_empty() && path.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    ok.then_some(path)
}

/// Fetches a replay by its link and reads the game from it.
///
/// # Errors
/// Refuses anything but a Showdown replay link, and fails when the fetch
/// fails or the battle has no winner.
pub async fn fetch(url: &str) -> Result<Replay, ReplayError> {
    let path = path_of(url).ok_or(ReplayError::BadUrl)?;
    let http = reqwest::Client::builder().timeout(FETCH_TIMEOUT).build()?;
    let body = http.get(format!("{HOST}/{path}.json")).send().await?.error_for_status()?.text().await?;
    parse(&format!("{HOST}/{path}"), &body)
}

#[derive(Deserialize)]
struct Raw {
    id: String,
    formatid: String,
    log: String,
}

/// Reads a game from a replay's JSON. `url` is stored as the link back.
///
/// # Errors
/// Fails on JSON that isn't a replay, a log missing players or team sizes,
/// or a battle with no winner.
pub fn parse(url: &str, json: &str) -> Result<Replay, ReplayError> {
    let raw: Raw = serde_json::from_str(json)?;
    let (players, winner, remaining) = read_log(&raw.log)?;
    Ok(Replay { id: raw.id, url: url.to_owned(), format: raw.formatid, players, winner, remaining, log: raw.log })
}

/// Which side won a battle log: 0 for p1, 1 for p2.
///
/// # Errors
/// Fails on the same logs `parse` refuses.
pub fn winner_side(log: &str) -> Result<usize, ReplayError> {
    read_log(log).map(|(_, winner, _)| winner)
}

/// The players, the index of the winner, and each side's Pokémon remaining.
fn read_log(log: &str) -> Result<([String; 2], usize, [i64; 2]), ReplayError> {
    let mut players: [Option<String>; 2] = [None, None];
    let mut brought: [Option<i64>; 2] = [None, None];
    let mut fainted = [0_i64; 2];
    let mut winner = None;

    for line in log.lines() {
        let mut parts = line.split('|').skip(1);
        let (Some(kind), Some(arg)) = (parts.next(), parts.next()) else { continue };
        let side = match arg.get(..2) {
            Some("p1") => Some(0),
            Some("p2") => Some(1),
            _ => None,
        };
        match (kind, side) {
            ("player", Some(s)) => {
                if let Some(name) = parts.next().filter(|n| !n.is_empty()) {
                    players[s].get_or_insert_with(|| name.to_owned());
                }
            }
            ("teamsize", Some(s)) => brought[s] = parts.next().and_then(|n| n.parse().ok()),
            ("faint", Some(s)) => fainted[s] += 1,
            ("win", _) => winner = Some(to_id(arg)),
            _ => {}
        }
    }

    let [Some(p1), Some(p2)] = players else { return Err(ReplayError::Unreadable("no players")) };
    let [Some(b1), Some(b2)] = brought else { return Err(ReplayError::Unreadable("no team sizes")) };
    let winner = match winner {
        Some(w) if w == to_id(&p1) => 0,
        Some(w) if w == to_id(&p2) => 1,
        _ => return Err(ReplayError::NoWinner),
    };
    Ok(([p1, p2], winner, [b1 - fainted[0], b2 - fainted[1]]))
}
