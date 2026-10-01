//! Webhook URL checks and splitting posts to fit Discord's limit.

use pokemon_draft_site::db::Db;
use pokemon_draft_site::discord::{self, Kind};
use pokemon_draft_site::picks::Drafted;

#[test]
fn only_discord_webhook_urls_are_accepted() {
    for ok in [
        "https://discord.com/api/webhooks/1/abc",
        "https://discordapp.com/api/webhooks/1/abc",
        "https://ptb.discord.com/api/webhooks/1/abc",
        "https://canary.discord.com/api/webhooks/1/abc?thread_id=9",
    ] {
        assert!(discord::valid_url(ok), "{ok} should be accepted");
    }
    for bad in [
        "http://discord.com/api/webhooks/1/abc",
        "https://discord.com.evil.com/api/webhooks/1/abc",
        "https://evil.com/discord.com/api/webhooks/1/abc",
        "https://discord.com/api/webhooks/",
        "https://discord.com/api/channels/1",
        "https://discord.com/api/webhooks/1/abc def",
    ] {
        assert!(!discord::valid_url(bad), "{bad} should be refused");
    }
}

#[test]
fn short_post_is_one_payload_pinging_only_named_users() {
    let p = discord::payloads("<@1> drafted Garchomp\nOn the clock: <@2>", &["1".into(), "2".into(), "3".into()]);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0]["content"], "<@1> drafted Garchomp\nOn the clock: <@2>");
    assert_eq!(p[0]["allowed_mentions"]["users"], serde_json::json!(["1", "2"]));
}

#[test]
fn long_post_splits_at_lines_with_mentions_per_chunk() {
    // 60 lines of ~50 chars is ~3000 chars: two messages.
    let mut lines: Vec<String> = (0..59).map(|i| format!("#{i:03} <@1> drafted {}", "x".repeat(30))).collect();
    lines.push("On the clock: <@2>".into());
    let p = discord::payloads(&lines.join("\n"), &["1".into(), "2".into()]);

    assert_eq!(p.len(), 2);
    for msg in &p {
        assert!(msg["content"].as_str().unwrap().chars().count() <= 2000);
    }
    let rejoined = format!("{}\n{}", p[0]["content"].as_str().unwrap(), p[1]["content"].as_str().unwrap());
    assert_eq!(rejoined, lines.join("\n"), "no line lost or broken");
    assert_eq!(p[0]["allowed_mentions"]["users"], serde_json::json!(["1"]));
    assert!(p[1]["content"].as_str().unwrap().ends_with("On the clock: <@2>"));
}

#[tokio::test]
async fn webhook_urls_set_and_clear_per_kind() {
    let db = Db::connect("sqlite::memory:").await.expect("connect");
    let url = "https://discord.com/api/webhooks/1/abc";

    db.set_webhook(Kind::Draft, url).await.expect("set");
    db.set_webhook(Kind::Draft, url).await.expect("replace");
    assert_eq!(db.webhook_url(Kind::Draft).await.expect("get").as_deref(), Some(url));
    assert_eq!(db.webhook_url(Kind::Trade).await.expect("get"), None);
    assert_eq!(db.webhooks_set().await.expect("list"), vec![Kind::Draft]);

    db.clear_webhook(Kind::Draft).await.expect("clear");
    assert!(db.webhooks_set().await.expect("list").is_empty());
}

fn drafted(number: i64, who: &str, pokemon: &str, points: i64) -> Drafted {
    Drafted { number, discord_id: who.into(), pokemon: pokemon.into(), points }
}

#[test]
fn draft_post_lists_a_chain_under_one_footer() {
    let picks = [drafted(14, "1", "Garchomp", 15), drafted(15, "2", "Rotom-Wash", 9), drafted(16, "1", "Pelipper", 3)];
    let (content, mentions) = discord::draft_post(None, &picks, Some("3"), "https://site/draft");
    assert_eq!(
        content,
        "#014 <@1> drafted Garchomp (15)\n\
         #015 <@2> drafted Rotom-Wash (9)\n\
         #016 <@1> drafted Pelipper (3)\n\
         On the clock: <@3> — https://site/draft"
    );
    assert_eq!(mentions, ["1", "2", "3"], "each coach once, plus the one on the clock");
}

#[test]
fn draft_post_with_a_lead_and_a_finished_draft() {
    let lead = Some(("<@1> is done drafting with 9 Pokémon.".to_owned(), "1".to_owned()));
    let (content, mentions) = discord::draft_post(lead, &[], None, "https://site/draft");
    assert_eq!(content, "<@1> is done drafting with 9 Pokémon.\nThe draft is complete.");
    assert_eq!(mentions, ["1"]);
}
