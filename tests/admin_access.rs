//! The admin gate is server-side, not a hidden button.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pokemon_draft_site::auth::DiscordOauth;
use pokemon_draft_site::db::Db;
use pokemon_draft_site::discord::Webhooks;
use pokemon_draft_site::web::{router, AppState};
use tower::ServiceExt as _;

async fn app() -> axum::Router {
    let db = Db::connect("sqlite::memory:").await.expect("connect");
    let oauth = DiscordOauth::new("id".into(), "secret".into(), "http://x/cb".into());
    router(AppState { webhooks: Webhooks::new(db.clone(), "http://x"), db, oauth })
}

#[tokio::test]
async fn admin_pages_reject_anonymous_callers() {
    for (method, uri) in [
        ("GET", "/admin"),
        ("POST", "/admin/tiers"),
        ("POST", "/admin/schedule"),
        ("POST", "/admin/coaches"),
        ("POST", "/admin/coaches/1"),
        ("POST", "/admin/coaches/1/remove"),
        ("POST", "/admin/members/1"),
        ("POST", "/admin/webhooks/draft"),
        ("POST", "/admin/webhooks/draft/clear"),
        ("GET", "/profile"),
        ("POST", "/profile"),
        ("POST", "/match/1/result"),
        ("POST", "/match/1/schedule"),
        // Queue routes are owner-keyed, so they must have an owner to key to.
        ("GET", "/draft"),
        ("POST", "/draft/queue"),
        ("POST", "/draft/queue/1/clear"),
        // Moves are keyed to the requester's own coach row.
        ("POST", "/roster/free-agency"),
        ("POST", "/trades/propose"),
        ("POST", "/trades/1/accept"),
        ("POST", "/trades/1/close"),
    ] {
        let res = app()
            .await
            .oneshot(Request::builder().method(method).uri(uri).body(Body::empty()).unwrap())
            .await
            .expect("response");
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{method} {uri} must not be open");
    }
}
