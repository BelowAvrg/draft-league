//! Application error type and its HTTP mapping.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;

/// Anything a request handler can fail with.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("template error: {0}")]
    Askama(#[from] askama::Error),
    #[error("session error: {0}")]
    Session(#[from] tower_sessions::session::Error),
    #[error("upstream request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// The caller is not signed in.
    #[error("not signed in")]
    Unauthorized,
    /// Signed in, but not permitted to do this.
    #[error("forbidden")]
    Forbidden,
    /// OAuth callback did not line up with the session we issued.
    #[error("login failed: {0}")]
    Auth(String),
    /// No such page, coach, or season.
    #[error("not found")]
    NotFound,
    /// The request was well-formed but the draft state refuses it.
    #[error("{0}")]
    Conflict(String),
}

impl AppError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Auth(_) => StatusCode::BAD_REQUEST,
            Self::Sqlx(_) | Self::Askama(_) | Self::Session(_) | Self::Http(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        // Log the detail, show the client only the status: error text can carry
        // connection strings and upstream URLs.
        tracing::error!(error = %self, %status, "request failed");
        // Conflicts carry a rule the caller broke and are safe to show; every
        // other message can carry connection strings and upstream URLs.
        let body = match (status, &self) {
            (_, Self::Conflict(msg)) => msg.clone(),
            (StatusCode::INTERNAL_SERVER_ERROR, _) => "internal server error".to_owned(),
            _ => "request failed".to_owned(),
        };
        (status, body).into_response()
    }
}
