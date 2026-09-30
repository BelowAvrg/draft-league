//! Discord OAuth login and session-backed identity.

use axum::extract::{FromRef, FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Redirect};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use std::fmt::Write as _;
use tower_sessions::Session;

use crate::db::{Db, Person};
use crate::error::AppError;

/// Session key holding the signed-in person's id.
const SESSION_USER_ID: &str = "user_id";
/// Session key holding the CSRF state issued at login.
const SESSION_OAUTH_STATE: &str = "oauth_state";

const DISCORD_AUTHORIZE: &str = "https://discord.com/oauth2/authorize";
const DISCORD_TOKEN: &str = "https://discord.com/api/oauth2/token";
const DISCORD_ME: &str = "https://discord.com/api/users/@me";

/// Discord OAuth application credentials.
#[derive(Clone)]
pub struct DiscordOauth {
    client_id: String,
    client_secret: String,
    redirect_uri: String,
    http: reqwest::Client,
}

// The secret must never reach a log line.
#[expect(clippy::missing_fields_in_debug, reason = "client_secret is redacted deliberately")]
impl std::fmt::Debug for DiscordOauth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordOauth")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("redirect_uri", &self.redirect_uri)
            .finish()
    }
}

impl DiscordOauth {
    /// Builds the client from explicit credentials.
    #[must_use]
    pub fn new(client_id: String, client_secret: String, redirect_uri: String) -> Self {
        Self { client_id, client_secret, redirect_uri, http: reqwest::Client::new() }
    }

    fn authorize_url(&self, state: &str) -> String {
        format!(
            "{DISCORD_AUTHORIZE}?client_id={}&redirect_uri={}&response_type=code&scope=identify&state={state}",
            urlencoding(&self.client_id),
            urlencoding(&self.redirect_uri),
        )
    }
}

/// Percent-encodes the characters that appear in ids, URLs and tokens.
fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => { let _ = write!(out, "%{b:02X}"); }
        }
    }
    out
}

/// The signed-in person, rejected with 401 when absent.
///
/// Extracting this is the access check: a handler that takes it cannot be
/// reached anonymously.
#[derive(Debug)]
pub struct CurrentUser(pub Person);

impl<S> FromRequestParts<S> for CurrentUser
where
    Db: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let session = Session::from_request_parts(parts, state)
            .await
            .map_err(|_missing_layer| AppError::Unauthorized)?;
        let id: i64 = session.get(SESSION_USER_ID).await?.ok_or(AppError::Unauthorized)?;
        let db = Db::from_ref(state);
        // The row can be gone if the person was deleted mid-session.
        let person = db.person(id).await?.ok_or(AppError::Unauthorized)?;
        Ok(Self(person))
    }
}

/// Absent rather than rejected when nobody is signed in.
///
/// Used by pages that render for guests and members alike.
impl<S> axum::extract::OptionalFromRequestParts<S> for CurrentUser
where
    Db: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Option<Self>, Self::Rejection> {
        match <Self as FromRequestParts<S>>::from_request_parts(parts, state).await {
            Ok(user) => Ok(Some(user)),
            Err(AppError::Unauthorized) => Ok(None),
            Err(other) => Err(other),
        }
    }
}

/// An admin, rejected with 403 for signed-in non-admins.
///
/// Every admin-only mutation takes this; template-level hiding is not access
/// control.
#[derive(Debug)]
pub struct AdminUser(pub Person);

impl<S> FromRequestParts<S> for AdminUser
where
    Db: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let CurrentUser(person) = CurrentUser::from_request_parts(parts, state).await?;
        if person.is_admin { Ok(Self(person)) } else { Err(AppError::Forbidden) }
    }
}

/// Auth routes: start login, handle the callback, log out.
pub fn routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    Db: FromRef<S>,
    DiscordOauth: FromRef<S>,
{
    Router::new()
        .route("/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/logout", get(logout))
}

async fn login(
    State(oauth): State<DiscordOauth>,
    session: Session,
) -> Result<impl IntoResponse, AppError> {
    // Random state, stored in the session and required to match on return.
    // Without it, an attacker can feed the user someone else's auth code.
    let state = format!("{:032x}", rand_u128());
    session.insert(SESSION_OAUTH_STATE, &state).await?;
    Ok(Redirect::to(&oauth.authorize_url(&state)))
}

#[derive(Debug, Deserialize)]
struct Callback {
    code: String,
    state: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Debug, Deserialize)]
struct DiscordUser {
    id: String,
    username: String,
}

async fn callback(
    State(oauth): State<DiscordOauth>,
    State(db): State<Db>,
    session: Session,
    Query(cb): Query<Callback>,
) -> Result<impl IntoResponse, AppError> {
    let expected: Option<String> = session.get(SESSION_OAUTH_STATE).await?;
    let expected = expected.ok_or_else(|| AppError::Auth("no login in progress".into()))?;
    session.remove::<String>(SESSION_OAUTH_STATE).await?;
    if expected != cb.state {
        return Err(AppError::Auth("state mismatch".into()));
    }

    let token: TokenResponse = oauth
        .http
        .post(DISCORD_TOKEN)
        .form(&[
            ("client_id", oauth.client_id.as_str()),
            ("client_secret", oauth.client_secret.as_str()),
            ("grant_type", "authorization_code"),
            ("code", cb.code.as_str()),
            ("redirect_uri", oauth.redirect_uri.as_str()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let user: DiscordUser = oauth
        .http
        .get(DISCORD_ME)
        .bearer_auth(&token.access_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let person = db.upsert_discord_person(&user.id, &user.username).await?;
    // New session id on privilege change, so a pre-login cookie cannot be
    // replayed as a logged-in one.
    session.cycle_id().await?;
    session.insert(SESSION_USER_ID, person.id).await?;
    tracing::info!(person_id = person.id, discord_id = %user.id, "login");

    Ok(Redirect::to("/"))
}

async fn logout(session: Session) -> Result<impl IntoResponse, AppError> {
    session.flush().await?;
    Ok(Redirect::to("/"))
}

/// Cryptographically random 128-bit value for CSRF state.
fn rand_u128() -> u128 {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).expect("system RNG unavailable");
    u128::from_le_bytes(buf)
}
