//! First-login and repeat-login behaviour.

use pokemon_draft_site::db::Db;

async fn fresh() -> Db {
    // A distinct in-memory database per test, kept alive by the pool.
    Db::connect("sqlite::memory:").await.expect("connect")
}

#[tokio::test]
async fn first_login_is_admin_and_later_ones_are_not() {
    let db = fresh().await;

    let first = db.upsert_discord_person("111", "ann").await.expect("first");
    assert!(first.is_admin, "the first person to log in must be admin");

    let second = db.upsert_discord_person("222", "bo").await.expect("second");
    assert!(!second.is_admin, "later logins must not be admin");
    assert_ne!(first.id, second.id);
}

#[tokio::test]
async fn repeat_login_reuses_the_person_and_refreshes_username() {
    let db = fresh().await;

    let first = db.upsert_discord_person("111", "ann").await.expect("first");
    // Someone changes their Discord display name, then logs in again.
    let again = db.upsert_discord_person("111", "ann2").await.expect("again");

    assert_eq!(first.id, again.id, "same Discord id must not create a second person");
    assert_eq!(again.discord_username, "ann2");
    assert!(again.is_admin, "admin must survive a re-login");
}

#[tokio::test]
async fn repeat_login_does_not_grant_admin_to_an_existing_member() {
    let db = fresh().await;

    db.upsert_discord_person("111", "ann").await.expect("first");
    let bo = db.upsert_discord_person("222", "bo").await.expect("bo");
    assert!(!bo.is_admin);

    // The table is no longer empty, so the is_first branch must stay false --
    // and ON CONFLICT must not overwrite is_admin either way.
    let bo_again = db.upsert_discord_person("222", "bo").await.expect("bo again");
    assert!(!bo_again.is_admin, "a repeat login must never escalate to admin");
}

#[tokio::test]
async fn lookup_by_discord_id_finds_and_misses() {
    let db = fresh().await;

    assert!(db.person_by_discord_id("111").await.expect("miss").is_none());
    let p = db.upsert_discord_person("111", "ann").await.expect("create");
    let found = db.person_by_discord_id("111").await.expect("hit").expect("some");
    assert_eq!(found.id, p.id);
}
