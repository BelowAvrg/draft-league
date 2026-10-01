//! Posting league events to Discord channel webhooks. See DISCORD.md.

use std::fmt;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::db::Db;
use crate::picks::Drafted;
use crate::results::{GameAdded, Scheduled};

/// Discord rejects message `content` longer than this many characters.
const MAX_CONTENT: usize = 2000;

/// Long enough for a slow Discord, short enough that one bad post can't stall the queue.
const POST_TIMEOUT: Duration = Duration::from_secs(10);

/// Rate-limit retries before a post is dropped. Discord's waits are seconds,
/// so this only gives up on something persistently wrong.
const MAX_RETRIES: u32 = 5;

/// Hosts a stored webhook URL may point at. The server requests whatever is
/// stored, so nothing outside Discord gets in.
const HOSTS: [&str; 4] = ["discord.com", "discordapp.com", "ptb.discord.com", "canary.discord.com"];

/// Which channel an event goes to. Each kind has its own webhook URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Draft,
    Schedule,
    Replay,
    Trade,
}

impl Kind {
    /// Every kind, in the order the admin page lists them.
    pub const ALL: [Self; 4] = [Self::Draft, Self::Schedule, Self::Replay, Self::Trade];

    /// The kind's name as stored in the database and used in URLs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Schedule => "schedule",
            Self::Replay => "replay",
            Self::Trade => "trade",
        }
    }

    /// Parses a kind from its stored name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == name)
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether `url` is a Discord webhook URL the server may post to.
#[must_use]
pub fn valid_url(url: &str) -> bool {
    !url.chars().any(char::is_whitespace)
        && HOSTS.iter().any(|h| {
            url.strip_prefix("https://")
                .and_then(|r| r.strip_prefix(h))
                .and_then(|r| r.strip_prefix("/api/webhooks/"))
                .is_some_and(|r| !r.is_empty())
        })
}

/// Splits a post into Discord payloads, each under the length limit.
///
/// Splits at line boundaries. Each payload pings only those `mentions`
/// (Discord user IDs) whose `<@id>` appears in its own text, so text that
/// merely looks like a mention, such as a team name, pings nobody.
#[must_use]
pub fn payloads(content: &str, mentions: &[String]) -> Vec<Value> {
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in content.lines() {
        // A single line over the limit can't happen with real names; cut it rather than fail.
        let line: String = line.chars().take(MAX_CONTENT).collect();
        let joined = current.chars().count() + 1 + line.chars().count();
        if !current.is_empty() && joined > MAX_CONTENT {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(&line);
    }
    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
        .into_iter()
        .map(|c| {
            let users: Vec<&String> =
                mentions.iter().filter(|id| c.contains(&format!("<@{id}>"))).collect();
            json!({ "content": c, "allowed_mentions": { "users": users } })
        })
        .collect()
}

/// Builds a draft post: an optional lead line, one line per pick, and a footer.
///
/// `lead` is the line's text and the Discord ID it names. `on_clock` is the
/// Discord ID of the coach now on the clock, or `None` once the draft is over.
/// Returns the content and every Discord ID it pings: each coach named, plus
/// the coach on the clock.
#[must_use]
pub fn draft_post(
    lead: Option<(String, String)>,
    picks: &[Drafted],
    on_clock: Option<&str>,
    link: &str,
) -> (String, Vec<String>) {
    let mut lines = Vec::new();
    let mut mentions: Vec<String> = Vec::new();
    let mut mention = |id: &str| {
        if !mentions.iter().any(|m| m == id) {
            mentions.push(id.to_owned());
        }
    };
    if let Some((text, id)) = lead {
        lines.push(text);
        mention(&id);
    }
    for p in picks {
        lines.push(format!("#{:03} <@{}> drafted {} ({})", p.number, p.discord_id, p.pokemon, p.points));
        mention(&p.discord_id);
    }
    lines.push(match on_clock {
        Some(id) => {
            mention(id);
            format!("On the clock: <@{id}> — {link}")
        }
        None => "The draft is complete.".to_owned(),
    });
    (lines.join("\n"), mentions)
}

/// One message waiting to be sent.
#[derive(Debug)]
struct Post {
    kind: Kind,
    url: String,
    payload: Value,
}

/// Queues posts to Discord webhooks. Cheap to clone.
///
/// A single background task sends every post in the order queued. Posts never
/// fail the caller: an unset URL skips the post, and delivery errors are logged.
#[derive(Debug, Clone)]
pub struct Webhooks {
    db: Db,
    /// The site's public origin, like `https://draft.belowavrg.com`, for links.
    site: String,
    tx: mpsc::UnboundedSender<Post>,
}

impl Webhooks {
    /// Starts the sender task. Must be called inside a Tokio runtime.
    ///
    /// Links in posts point at the origin of `public_url`; any path is ignored.
    #[must_use]
    pub fn new(db: Db, public_url: &str) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(send_all(rx));
        let site = public_url.split('/').take(3).collect::<Vec<_>>().join("/");
        Self { db, site, tx }
    }

    /// Posts what a draft action did, footed by who is now on the clock.
    ///
    /// `lead` is a line to open with and the Discord ID it names, for actions
    /// other than a pick. Failures are logged, never returned.
    pub async fn draft(&self, lead: Option<(String, String)>, picks: &[Drafted]) {
        let on_clock = match self.on_clock().await {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(error = %e, "reading the draft for a post; post dropped");
                return;
            }
        };
        let link = format!("{}/draft", self.site);
        let (content, mentions) = draft_post(lead, picks, on_clock.as_deref(), &link);
        self.post(Kind::Draft, &content, &mentions).await;
    }

    /// Posts that `from` offered `to` a trade. The contents stay private.
    pub async fn trade_offered(&self, from: i64, to: i64) {
        let post = async {
            let (a, b) = (self.discord_id(from).await?, self.discord_id(to).await?);
            Ok((format!("<@{a}> sent <@{b}> a trade offer."), vec![a, b]))
        };
        self.post_or_warn(Kind::Trade, post.await).await;
    }

    /// Posts an accepted trade in full. Accepted moves are public.
    ///
    /// `to` is the coach who accepted, whose offers include this one.
    pub async fn trade_made(&self, offer_id: i64, to: i64, week: i64) {
        let post = async {
            let offers = self.db.offers_of(to).await?;
            let offer = offers.iter().find(|o| o.id == offer_id).ok_or(sqlx::Error::RowNotFound)?;
            let names = |coach| {
                offer.given_by(coach).iter().map(|i| i.display_name.as_str()).collect::<Vec<_>>().join(", ")
            };
            let a = self.discord_id(offer.from_coach_id).await?;
            let b = self.discord_id(offer.to_coach_id).await?;
            let content = format!(
                "Trade: <@{a}> sends {} to <@{b}> for {}. Effective week {week}.",
                names(offer.from_coach_id),
                names(offer.to_coach_id),
            );
            Ok((content, vec![a, b]))
        };
        self.post_or_warn(Kind::Trade, post.await).await;
    }

    /// Posts a free agency move.
    pub async fn free_agency(&self, coach: i64, drop: i64, pickup: i64, week: i64) {
        let post = async {
            let id = self.discord_id(coach).await?;
            let drop = self.db.pokemon_name(drop).await?.ok_or(sqlx::Error::RowNotFound)?;
            let pickup = self.db.pokemon_name(pickup).await?.ok_or(sqlx::Error::RowNotFound)?;
            let content =
                format!("Free agency: <@{id}> drops {drop} and picks up {pickup}. Effective week {week}.");
            Ok((content, vec![id]))
        };
        self.post_or_warn(Kind::Trade, post.await).await;
    }

    /// Posts an accepted replay, and the match result when it decided one.
    pub async fn replay(&self, added: &GameAdded) {
        let post = async {
            let (w, l) = (self.discord_id(added.winner).await?, self.discord_id(added.loser).await?);
            let decided = added.decided.map_or_else(String::new, |m| {
                let id = if m.winner == added.winner { &w } else { &l };
                format!("\n<@{id}> wins the match {}–{} ({:+}).", m.won, m.lost, m.differential)
            });
            let content = format!(
                "{}, game {}: <@{w}> beat <@{l}>, {}–{} remaining.\n{}{decided}",
                added.label, added.game, added.winner_remaining, added.loser_remaining, added.replay_url
            );
            Ok((content, vec![w, l]))
        };
        self.post_or_warn(Kind::Replay, post.await).await;
    }

    /// Posts that a match was scheduled, rescheduled, or unscheduled.
    pub async fn schedule(&self, s: &Scheduled) {
        let when = match (s.previous, s.at) {
            (prev, at) if prev == at => return,
            (_, Some(at)) => {
                let verb = if s.previous.is_some() { "rescheduled" } else { "scheduled" };
                let t = at.unix_timestamp();
                format!("is {verb} for <t:{t}:F> (<t:{t}:R>)")
            }
            (_, None) => "is no longer scheduled".to_owned(),
        };
        let post = async {
            let (a, b) = (self.discord_id(s.coaches[0]).await?, self.discord_id(s.coaches[1]).await?);
            // A backtick would break the code span; Showdown names can't hold one anyway.
            let challenge = match (s.at, s.opponent_showdown.as_deref().filter(|n| !n.contains('`'))) {
                (Some(_), Some(name)) => {
                    // Showdown's `bestof` rule takes only odd N from 3 to 9, and needs a format.
                    let format = match (s.format.as_deref(), s.best_of) {
                        (Some(id), games @ 3..=9) => format!(", {id} @@@ Best Of = {games}"),
                        (Some(id), _) => format!(", {id}"),
                        (None, _) => String::new(),
                    };
                    format!("\nChallenge on Showdown: `/challenge {name}{format}`")
                }
                _ => String::new(),
            };
            let content =
                format!("{}: <@{a}> vs <@{b}> {when}.\n{}/match/{}{challenge}", s.label, self.site, s.match_id);
            Ok((content, vec![a, b]))
        };
        self.post_or_warn(Kind::Schedule, post.await).await;
    }

    /// Posts a message whose lookups succeeded, or logs why they failed.
    async fn post_or_warn(&self, kind: Kind, post: Result<(String, Vec<String>), sqlx::Error>) {
        match post {
            Ok((content, mentions)) => self.post(kind, &content, &mentions).await,
            Err(e) => tracing::warn!(%kind, error = %e, "building post; post dropped"),
        }
    }

    /// A coach's Discord ID, for a mention.
    async fn discord_id(&self, coach_id: i64) -> Result<String, sqlx::Error> {
        self.db.coach_discord_id(coach_id).await?.ok_or(sqlx::Error::RowNotFound)
    }

    /// The Discord ID of the coach on the clock, if the draft is still going.
    async fn on_clock(&self) -> Result<Option<String>, crate::picks::DraftError> {
        let Some(coach_id) = self.db.board().await?.turn.coach_id else { return Ok(None) };
        Ok(self.db.coach_discord_id(coach_id).await?)
    }

    /// Queues `content` for the `kind` channel, pinging exactly `mentions`.
    ///
    /// Does nothing when no URL is set for `kind`.
    pub async fn post(&self, kind: Kind, content: &str, mentions: &[String]) {
        let url = match self.db.webhook_url(kind).await {
            Ok(Some(url)) => url,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(%kind, error = %e, "reading webhook url; post dropped");
                return;
            }
        };
        for payload in payloads(content, mentions) {
            // The receiver lives as long as the runtime; a send error means shutdown.
            let _ = self.tx.send(Post { kind, url: url.clone(), payload });
        }
    }
}

/// Sends queued posts one at a time until every sender is dropped.
async fn send_all(mut rx: mpsc::UnboundedReceiver<Post>) {
    let http = match reqwest::Client::builder().timeout(POST_TIMEOUT).build() {
        Ok(http) => http,
        Err(e) => {
            tracing::error!(error = %e, "building webhook client; Discord posts disabled");
            return;
        }
    };
    while let Some(post) = rx.recv().await {
        send(&http, &post).await;
    }
}

/// Sends one post, waiting out rate limits. Other failures are logged and dropped.
async fn send(http: &reqwest::Client, post: &Post) {
    let kind = post.kind;
    for _ in 0..=MAX_RETRIES {
        let res = match http.post(&post.url).json(&post.payload).send().await {
            Ok(res) => res,
            Err(e) => {
                tracing::warn!(%kind, error = %e, "webhook post failed");
                return;
            }
        };
        let status = res.status();
        if status.is_success() {
            return;
        }
        if status != reqwest::StatusCode::TOO_MANY_REQUESTS {
            tracing::warn!(%kind, %status, "webhook post rejected");
            return;
        }
        // Discord gives the wait in seconds, possibly fractional.
        let wait = res
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| v["retry_after"].as_f64())
            .unwrap_or(1.0);
        tracing::info!(%kind, wait, "webhook rate limited; retrying");
        tokio::time::sleep(Duration::from_secs_f64(wait.clamp(0.0, 60.0))).await;
    }
    tracing::warn!(%kind, "webhook still rate limited; post dropped");
}

impl Db {
    /// The webhook URL for `kind`, if one is set.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn webhook_url(&self, kind: Kind) -> Result<Option<String>, sqlx::Error> {
        let kind = kind.as_str();
        sqlx::query_scalar!("SELECT url FROM webhook WHERE kind = ?", kind)
            .fetch_optional(self.pool())
            .await
    }

    /// The kinds that have a webhook URL set. The URLs themselves stay hidden.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn webhooks_set(&self) -> Result<Vec<Kind>, sqlx::Error> {
        let names = sqlx::query_scalar!("SELECT kind FROM webhook").fetch_all(self.pool()).await?;
        Ok(names.iter().filter_map(|n| Kind::from_name(n)).collect())
    }

    /// Stores the webhook URL for `kind`, replacing any previous one.
    ///
    /// Callers check the URL with [`valid_url`] first.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn set_webhook(&self, kind: Kind, url: &str) -> Result<(), sqlx::Error> {
        let kind = kind.as_str();
        sqlx::query!(
            "INSERT INTO webhook (kind, url) VALUES (?, ?)
             ON CONFLICT (kind) DO UPDATE SET url = excluded.url",
            kind,
            url
        )
        .execute(self.pool())
        .await?;
        tracing::info!(kind, "webhook url set");
        Ok(())
    }

    /// Removes the webhook URL for `kind`, so that kind is no longer posted.
    ///
    /// # Errors
    /// Fails on database error.
    pub async fn clear_webhook(&self, kind: Kind) -> Result<(), sqlx::Error> {
        let kind = kind.as_str();
        sqlx::query!("DELETE FROM webhook WHERE kind = ?", kind).execute(self.pool()).await?;
        tracing::info!(kind, "webhook url cleared");
        Ok(())
    }
}
