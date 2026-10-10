//! Match-time proposal lifecycle and interaction with direct scheduling.

use pokemon_draft_site::db::{Db, Person};
use pokemon_draft_site::scheduling::ProposalError;
use time::OffsetDateTime;

struct League {
    db: Db,
    game: i64,
    coaches: [i64; 2],
    people: [Person; 3],
}

async fn league() -> League {
    let db = Db::connect("sqlite::memory:").await.expect("connect");
    let a = db.upsert_discord_person("100", "Misty").await.expect("a");
    let b = db.upsert_discord_person("200", "Brock").await.expect("b");
    let outsider = db
        .upsert_discord_person("300", "Tracey")
        .await
        .expect("outsider");
    let season: i64 = sqlx::query_scalar(
        "INSERT INTO season (name, min_roster, max_roster, is_active)
         VALUES ('Test', 1, 2, 1) RETURNING id",
    )
    .fetch_one(db.pool())
    .await
    .expect("season");
    let mut coaches = [0; 2];
    for (index, person) in [&a, &b].into_iter().enumerate() {
        coaches[index] = sqlx::query_scalar(
            "INSERT INTO coach (person_id, season_id, budget, draft_position)
             VALUES (?, ?, 10, ?) RETURNING id",
        )
        .bind(person.id)
        .bind(season)
        .bind(i64::try_from(index + 1).expect("small index"))
        .fetch_one(db.pool())
        .await
        .expect("coach");
    }
    let game: i64 = sqlx::query_scalar(
        "INSERT INTO match (season_id, week, coach_a_id, coach_b_id)
         VALUES (?, 1, ?, ?) RETURNING id",
    )
    .bind(season)
    .bind(coaches[0])
    .bind(coaches[1])
    .fetch_one(db.pool())
    .await
    .expect("match");
    League {
        db,
        game,
        coaches,
        people: [a, b, outsider],
    }
}

fn future_blocks() -> [OffsetDateTime; 3] {
    let unix = OffsetDateTime::now_utc().unix_timestamp();
    let first = (unix.div_euclid(1800) + 2) * 1800;
    [0, 1800, 3600].map(|add| OffsetDateTime::from_unix_timestamp(first + add).expect("time"))
}

#[tokio::test]
async fn recipient_accepts_one_option_and_direct_scheduling_supersedes_the_next_proposal() {
    let league = league().await;
    let times = future_blocks();
    let notice = league
        .db
        .propose_times(league.game, &league.people[0], &times[..2])
        .await
        .expect("propose");
    assert_eq!(
        (notice.from_coach, notice.to_coach, notice.options),
        (league.coaches[0], league.coaches[1], 2)
    );

    let duplicate = league
        .db
        .propose_times(league.game, &league.people[1], &times[1..])
        .await;
    assert!(matches!(duplicate, Err(ProposalError::AlreadyOpen)));
    let outsider = league
        .db
        .propose_times(league.game, &league.people[2], &times[..1])
        .await;
    assert!(matches!(outsider, Err(ProposalError::NotParticipant)));

    let open = league
        .db
        .open_time_proposal(league.game)
        .await
        .expect("read")
        .expect("open");
    let scheduled = league
        .db
        .accept_time_proposal(league.game, open.id, open.options[1].id, &league.people[1])
        .await
        .expect("accept");
    assert_eq!(scheduled.at, Some(times[1]));
    assert!(
        league
            .db
            .open_time_proposal(league.game)
            .await
            .expect("read")
            .is_none()
    );

    league
        .db
        .propose_times(league.game, &league.people[1], &times[2..])
        .await
        .expect("reschedule proposal");
    league
        .db
        .set_schedule(league.game, &league.people[0], Some(times[2]))
        .await
        .expect("direct set");
    assert!(
        league
            .db
            .open_time_proposal(league.game)
            .await
            .expect("read")
            .is_none()
    );
    let statuses: Vec<String> =
        sqlx::query_scalar("SELECT status FROM match_time_proposal WHERE match_id = ? ORDER BY id")
            .bind(league.game)
            .fetch_all(league.db.pool())
            .await
            .expect("statuses");
    assert_eq!(statuses, ["accepted", "superseded"]);
}

#[tokio::test]
async fn recipient_declines_the_whole_set_and_proposer_can_withdraw_the_next() {
    let league = league().await;
    let times = future_blocks();
    league
        .db
        .propose_times(league.game, &league.people[0], &times[..2])
        .await
        .expect("propose");
    let first = league
        .db
        .open_time_proposal(league.game)
        .await
        .expect("read")
        .expect("open");

    let wrong = league
        .db
        .decline_time_proposal(league.game, first.id, &league.people[0])
        .await;
    assert!(matches!(wrong, Err(ProposalError::NotRecipient)));
    league
        .db
        .decline_time_proposal(league.game, first.id, &league.people[1])
        .await
        .expect("decline");

    league
        .db
        .propose_times(league.game, &league.people[1], &times[2..])
        .await
        .expect("counter");
    let second = league
        .db
        .open_time_proposal(league.game)
        .await
        .expect("read")
        .expect("open");
    league
        .db
        .withdraw_time_proposal(league.game, second.id, &league.people[1])
        .await
        .expect("withdraw");
    let statuses: Vec<String> =
        sqlx::query_scalar("SELECT status FROM match_time_proposal WHERE match_id = ? ORDER BY id")
            .bind(league.game)
            .fetch_all(league.db.pool())
            .await
            .expect("statuses");
    assert_eq!(statuses, ["declined", "withdrawn"]);
}
