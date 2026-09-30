//! Server entry point.

use anyhow::Context as _;
use pokemon_draft_site::auth::DiscordOauth;
use pokemon_draft_site::db::Db;
use pokemon_draft_site::web::{router, AppState};
use tokio::net::TcpListener;
use tower_sessions::cookie::{Key, SameSite};
use tower_sessions::{Expiry, SessionManagerLayer};
use pokemon_draft_site::session_store::SqliteStore;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// Sessions last a fortnight: long enough that a draft spanning days never
/// logs anyone out mid-pick.
const SESSION_DAYS: i64 = 14;

/// Reads `KEY`, falling back to the contents of the file named by `KEY_FILE`.
///
/// Docker Swarm mounts secrets as files, so production sets the `_FILE` form.
fn env(key: &str) -> anyhow::Result<String> {
    if let Ok(path) = std::env::var(format!("{key}_FILE")) {
        let v = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {key}_FILE at {path}"))?;
        return Ok(v.trim().to_owned());
    }
    std::env::var(key).with_context(|| format!("{key} or {key}_FILE must be set"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "pokemon_draft_site=debug,tower_http=debug".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let database_url = env("DATABASE_URL")?;
    let db = Db::connect(&database_url).await.context("opening database")?;

    // Signing key for session cookies. Generated per boot if unset, which logs
    // everyone out on restart -- fine for local work, not for the VPS.
    let key = if let Ok(v) = env("SESSION_KEY") {
        Key::from(&hex_decode(&v).context("SESSION_KEY must be hex")?)
    } else {
        tracing::warn!("SESSION_KEY unset; generating an ephemeral key");
        Key::generate()
    };

    let store = SqliteStore::new(db.pool().clone());
    let sessions = SessionManagerLayer::new(store)
        .with_signed(key)
        .with_secure(std::env::var("INSECURE_COOKIES").is_err())
        // Lax, not the Strict default: the browser drops a Strict cookie on the
        // cross-site return from discord.com, so the callback would find no
        // session and reject every login. Lax still withholds it on cross-site
        // POSTs, which is what the CSRF state guards.
        .with_same_site(SameSite::Lax)
        .with_expiry(Expiry::OnInactivity(time::Duration::days(SESSION_DAYS)));

    let oauth = DiscordOauth::new(
        env("DISCORD_CLIENT_ID")?,
        env("DISCORD_CLIENT_SECRET")?,
        env("DISCORD_REDIRECT_URI")?,
    );

    let app = router(AppState { db, oauth }).layer(sessions);

    let addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".into());
    let listener = TcpListener::bind(&addr).await.with_context(|| format!("binding {addr}"))?;
    tracing::info!(addr = %listener.local_addr()?, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Decodes a hex string into bytes.
fn hex_decode(s: &str) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(s.len().is_multiple_of(2), "odd length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).context("bad hex digit"))
        .collect()
}
