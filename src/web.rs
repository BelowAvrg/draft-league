//! Router, shared state, and the pages built so far.

use askama::Template;
use axum::extract::{FromRef, Path, Query, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use tower_http::services::ServeDir;

use crate::auth::{AdminUser, CurrentUser, DiscordOauth};
use crate::coaches::{Coach, CoachError};
use crate::db::{Db, Person, Season};
use crate::draft::{self, Standing};
use crate::error::AppError;
use crate::picks::{Board, DraftError, Listing, Pick};
use crate::results::{Entry, Game, Match, ResultError, Score};
use crate::standings::Row;
use crate::tiers::ImportError;

/// Everything a handler can extract from application state.
#[derive(Debug, Clone)]
pub struct AppState {
    pub db: Db,
    pub oauth: DiscordOauth,
}

impl FromRef<AppState> for Db {
    fn from_ref(input: &AppState) -> Self {
        input.db.clone()
    }
}

impl FromRef<AppState> for DiscordOauth {
    fn from_ref(input: &AppState) -> Self {
        input.oauth.clone()
    }
}

/// Builds the application router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/board", get(index))
        .route("/roster/mine", get(my_roster))
        .route("/roster/{id}", get(roster))
        .route("/pokemon", get(pokemon))
        .route("/draft", get(draft_page))
        .route("/draft/pick", post(make_pick))
        .route("/draft/done", post(finish_drafting))
        .route("/draft/queue", post(set_queue))
        .route("/draft/queue/{slot}/clear", post(clear_queue))
        .route("/admin", get(admin))
        .route("/admin/coaches/{id}/reopen", post(reopen_coach))
        .route("/admin/picks/undo", post(undo_pick))
        .route("/admin/tiers", post(import_tiers))
        .route("/admin/schedule", post(import_schedule))
        .route("/admin/coaches", post(add_coach))
        .route("/admin/coaches/{id}", post(update_coach))
        .route("/admin/coaches/{id}/remove", post(remove_coach))
        .route("/admin/members/{id}", post(update_member))
        .route("/schedule", get(schedule_page))
        .route("/standings", get(standings_page))
        .route("/match/{id}", get(match_page))
        .route("/match/{id}/result", post(save_result))
        .route("/match/{id}/replay", post(upload_replay))
        .route("/match/{id}/game/{game_id}/remove", post(remove_game))
        .route("/profile", get(profile).post(save_profile))
        .route("/health", get(health))
        .merge(crate::auth::routes())
        .with_state(state)
        .nest_service("/static", ServeDir::new("static"))
}

/// Liveness probe for the reverse proxy. No database work on purpose.
async fn health() -> &'static str {
    "ok"
}

/// One coach's line on the draft board. Queue *count* is public; contents are not.
struct BoardRow {
    coach_id: i64,
    draft_position: i64,
    name: String,
    picks: i64,
    budget: i64,
    remaining: i64,
    queued: i64,
    status: &'static str,
    on_clock: bool,
    /// Drafted Pokémon in pick order, shown as a sprite strip under the row.
    team: Vec<Pick>,
}

/// What the signed-in coach needs to act on their own turn.
struct MySeat {
    picks: i64,
    remaining: i64,
    reserve: i64,
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    layout: Layout,
    board: Option<Board>,
    rows: Vec<BoardRow>,
    /// Name of the coach on the clock, absent once the draft is over.
    on_the_clock: Option<String>,
    /// Present only when the viewer is the coach on the clock.
    my_turn: Option<MySeat>,
    /// Present only when the viewer may finish drafting right now.
    can_finish: Option<MySeat>,
    /// Pokémon the viewer can legally draft, empty unless it is their turn.
    affordable: Vec<Listing>,
}

/// Draft board: whose turn it is and the coach table.
/// The draft board while the draft runs, the schedule once it is over.
async fn home(
    user: Option<CurrentUser>,
    State(db): State<Db>,
    flash: Query<Flash>,
) -> Result<Response, AppError> {
    let over = match db.board().await {
        Ok(board) => !board.seats.is_empty() && board.turn.coach_id.is_none(),
        Err(DraftError::Sqlx(e)) => return Err(e.into()),
        Err(_) => false,
    };
    if over {
        return Ok(Redirect::to("/schedule").into_response());
    }
    Ok(index(user, State(db), flash).await?.into_response())
}

async fn index(
    user: Option<CurrentUser>,
    State(db): State<Db>,
    Query(flash): Query<Flash>,
) -> Result<impl IntoResponse, AppError> {
    let me = user.as_ref().map(|CurrentUser(p)| p.id);
    let mut page = IndexTemplate {
        layout: Layout::new(user.as_ref(), "board", flash),
        board: None,
        rows: Vec::new(),
        on_the_clock: None,
        my_turn: None,
        can_finish: None,
        affordable: Vec::new(),
    };

    let board = match db.board().await {
        Ok(board) => board,
        Err(DraftError::NoSeason) => return Ok(Html(page.render()?)),
        Err(DraftError::Sqlx(e)) => return Err(e.into()),
        Err(e) => {
            page.layout.flash.err = Some(e.to_string());
            return Ok(Html(page.render()?));
        }
    };

    let coaches = db.coaches(board.season.id).await?;
    let queued = db.queue_counts(board.season.id).await?;
    // Every pick this season, newest first; reversed per coach into pick order.
    let picks = db.recent_picks(board.season.id, i64::MAX).await?;
    for coach in &coaches {
        let Some(seat) = board.seat_of(coach.id) else { continue };
        let on_clock = board.turn.coach_id == Some(coach.id);
        page.rows.push(BoardRow {
            coach_id: coach.id,
            draft_position: coach.draft_position,
            name: coach.discord_username.clone(),
            picks: seat.picks,
            budget: coach.budget,
            remaining: seat.remaining,
            queued: queued.iter().find(|(id, _)| *id == coach.id).map_or(0, |(_, n)| *n),
            status: board.standing(seat).label(),
            on_clock,
            team: picks.iter().rev().filter(|p| p.coach_id == coach.id).cloned().collect(),
        });
        if on_clock {
            page.on_the_clock = Some(coach.discord_username.clone());
        }
    }

    // The viewer's own controls. Rendering them is a convenience; both POST
    // handlers re-check every rule server-side.
    if let Some(person_id) = me
        && let Some(coach_id) = db.coach_of(person_id, board.season.id).await?
        && let Some(seat) = board.seat_of(coach_id)
    {
        let reserve = draft::reserve(seat.picks + 1, board.roster, &board.pool);
        let mine = || MySeat { picks: seat.picks, remaining: seat.remaining, reserve };
        if board.turn.coach_id == Some(coach_id) {
            page.affordable = db
                .listings(board.season.id)
                .await?
                .into_iter()
                .filter(|l| {
                    l.taken_by.is_none()
                        && draft::validate_pick(seat, l.points, board.roster, &board.pool).is_ok()
                })
                .collect();
            page.my_turn = Some(mine());
        }
        // Offered alongside the pick form as well: being on the clock at the
        // minimum is exactly when a coach decides to stop.
        if draft::validate_done(seat, board.roster).is_ok() {
            page.can_finish = Some(mine());
        }
    }

    page.board = Some(board);
    Ok(Html(page.render()?))
}

/// What `base.html` needs: the nav and any flash message.
struct Layout {
    /// The signed-in person's name, if anyone is signed in.
    name: Option<String>,
    is_admin: bool,
    /// Which nav link is the current page; empty for pages not in the nav.
    page: &'static str,
    flash: Flash,
}

impl Layout {
    fn new(user: Option<&CurrentUser>, page: &'static str, flash: Flash) -> Self {
        Self {
            name: user.map(|CurrentUser(p)| p.discord_username.clone()),
            is_admin: user.is_some_and(|CurrentUser(p)| p.is_admin),
            page,
            flash,
        }
    }
}

#[derive(Template)]
#[template(path = "roster.html")]
struct RosterTemplate {
    layout: Layout,
    coach: Coach,
    picks: Vec<Pick>,
    min_roster: i64,
    max_roster: i64,
    status: &'static str,
    on_clock: bool,
    /// Points held back to reach the minimum roster.
    reserve: i64,
    /// Empty slots still needed to reach the minimum.
    open_required: i64,
}

/// One coach's roster. Public: completed picks are visible to everyone.
/// The nav's "Your roster" link: the signed-in coach's roster this season.
async fn my_roster(CurrentUser(me): CurrentUser, State(db): State<Db>) -> Result<Redirect, AppError> {
    let season = db.active_season().await?.ok_or(AppError::NotFound)?;
    Ok(match db.coach_of(me.id, season.id).await? {
        Some(coach_id) => Redirect::to(&format!("/roster/{coach_id}")),
        None => Redirect::to(&format!("/board?err={}", urlencode("You are not a coach this season."))),
    })
}

async fn roster(
    user: Option<CurrentUser>,
    State(db): State<Db>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let board = db.board().await.map_err(draft_error)?;
    let coaches = db.coaches(board.season.id).await?;
    let coach = coaches.into_iter().find(|c| c.id == id).ok_or(AppError::NotFound)?;
    let seat = board.seat_of(id);
    let status = seat.map_or(Standing::Active, |s| board.standing(s)).label();
    let picks = db.roster_of(id).await?;
    let drafted = i64::try_from(picks.len()).expect("a roster holds at most max_roster picks");
    let (min, max) = (board.roster.min(), board.roster.max());
    Ok(Html(
        RosterTemplate {
            // Your own roster lights up the nav's "Your roster" link.
            layout: Layout::new(
                user.as_ref(),
                if user.as_ref().is_some_and(|CurrentUser(p)| p.id == coach.person_id) { "roster" } else { "" },
                Flash::default(),
            ),
            reserve: seat.map_or(0, |s| draft::reserve(s.picks, board.roster, &board.pool)),
            on_clock: board.turn.coach_id == Some(id),
            open_required: (min - drafted).max(0),
            min_roster: min,
            max_roster: max,
            coach,
            picks,
            status,
        }
        .render()?,
    ))
}

#[derive(Template)]
#[template(path = "pokemon.html")]
struct PokemonTemplate {
    layout: Layout,
    listings: Vec<Listing>,
    taken: usize,
}

/// The tier list, showing what is already drafted. Public.
async fn pokemon(
    user: Option<CurrentUser>,
    State(db): State<Db>,
) -> Result<impl IntoResponse, AppError> {
    let listings = match db.active_season().await? {
        Some(season) => db.listings(season.id).await?,
        None => Vec::new(),
    };
    let taken = listings.iter().filter(|l| l.taken_by.is_some()).count();
    Ok(Html(PokemonTemplate {
        layout: Layout::new(user.as_ref(), "pokemon", Flash::default()),
        listings,
        taken,
    }.render()?))
}

/// Maps the errors that are really "no season set up" onto a page-level error.
fn draft_error(e: DraftError) -> AppError {
    match e {
        DraftError::Sqlx(e) => e.into(),
        DraftError::NoSeason => AppError::NotFound,
        other => AppError::Conflict(other.to_string()),
    }
}

/// Back to the draft board carrying a one-line outcome.
fn back_to_board(result: Result<&str, DraftError>) -> Result<Redirect, AppError> {
    let (key, msg) = match result {
        Ok(msg) => ("ok", msg.to_owned()),
        Err(DraftError::Sqlx(e)) => return Err(e.into()),
        Err(e) => ("err", e.to_string()),
    };
    Ok(Redirect::to(&format!("/?{key}={}", urlencode(&msg))))
}

#[derive(Debug, Deserialize)]
struct PickForm {
    pokemon_id: i64,
}

/// Records the signed-in coach's pick.
///
/// Every rule is checked here: the turn, the cap, the reserve, and the roster
/// maximum. The template hiding the form is not the access check.
async fn make_pick(
    CurrentUser(person): CurrentUser,
    State(db): State<Db>,
    Form(form): Form<PickForm>,
) -> Result<impl IntoResponse, AppError> {
    let outcome = async {
        let season = db.active_season().await?.ok_or(DraftError::NoSeason)?;
        let coach_id =
            db.coach_of(person.id, season.id).await?.ok_or(DraftError::NotACoach)?;
        db.make_pick(coach_id, form.pokemon_id).await
    }
    .await;
    back_to_board(outcome.map(|()| "Pick recorded."))
}

/// Marks the signed-in coach as finished drafting.
async fn finish_drafting(
    CurrentUser(person): CurrentUser,
    State(db): State<Db>,
) -> Result<impl IntoResponse, AppError> {
    let outcome = async {
        let season = db.active_season().await?.ok_or(DraftError::NoSeason)?;
        let coach_id =
            db.coach_of(person.id, season.id).await?.ok_or(DraftError::NotACoach)?;
        db.finish_drafting(coach_id).await
    }
    .await;
    back_to_board(outcome.map(|()| "You have finished drafting."))
}

/// One row of the queue editor: a pick number and whatever is queued for it.
struct QueueRow {
    slot_number: i64,
    /// Empty when nothing is queued for this pick.
    queued: Option<crate::picks::Queued>,
    /// Types of the queued Pokémon, empty when nothing is queued.
    types: String,
    /// Whether the draft has already passed this pick.
    passed: bool,
}

#[derive(Template)]
#[template(path = "draft.html")]
struct DraftTemplate {
    layout: Layout,
    season: Season,
    /// Absent when the viewer does not coach this season.
    seat: Option<MySeat>,
    my_turn: bool,
    /// The coach the draft is waiting on, when it is not the viewer.
    waiting_on: Option<String>,
    /// One row per pick number, passed ones included but locked.
    slots: Vec<QueueRow>,
    /// Undrafted Pokémon, for both the pick form and the queue editor.
    available: Vec<Listing>,
    /// Of `available`, those the viewer could legally take right now.
    affordable: Vec<i64>,
    /// The slot the queue picker starts on: the first open empty one.
    next_slot: Option<i64>,
}

/// The signed-in coach's own draft page: their turn, and their queue editor.
///
/// Queue contents are read only for the signed-in coach's own `coach_id`, so
/// there is no path here that serves anyone else's slots -- admins included.
async fn draft_page(
    user: CurrentUser,
    State(db): State<Db>,
    Query(flash): Query<Flash>,
) -> Result<impl IntoResponse, AppError> {
    let CurrentUser(person) = &user;
    let board = db.board().await.map_err(draft_error)?;
    let coach_id = db.coach_of(person.id, board.season.id).await?;

    let coaches = db.coaches(board.season.id).await?;
    let waiting_on = board
        .turn
        .coach_id
        .filter(|id| Some(*id) != coach_id)
        .and_then(|id| coaches.iter().find(|c| c.id == id))
        .map(|c| c.discord_username.clone());

    let available: Vec<Listing> =
        db.listings(board.season.id).await?.into_iter().filter(|l| l.taken_by.is_none()).collect();

    let mut page = DraftTemplate {
        layout: Layout::new(Some(&user), "draft", flash),
        season: board.season.clone(),
        seat: None,
        my_turn: false,
        waiting_on,
        slots: Vec::new(),
        affordable: Vec::new(),
        available,
        next_slot: None,
    };

    let Some(coach_id) = coach_id else { return Ok(Html(page.render()?)) };
    let Some(seat) = board.seat_of(coach_id) else { return Ok(Html(page.render()?)) };

    page.my_turn = board.turn.coach_id == Some(coach_id);
    page.seat = Some(MySeat {
        picks: seat.picks,
        remaining: seat.remaining,
        reserve: draft::reserve(seat.picks + 1, board.roster, &board.pool),
    });
    page.affordable = page
        .available
        .iter()
        .filter(|l| draft::validate_pick(seat, l.points, board.roster, &board.pool).is_ok())
        .map(|l| l.pokemon_id)
        .collect();

    // One row per pick of the roster maximum. Slots at or below the completed
    // pick count are dead -- the draft will not read them again -- so they
    // render locked rather than vanishing, which would renumber the rest.
    let mut queued = db.queue_of(coach_id).await?;
    page.slots = (1..=board.season.max_roster)
        .map(|slot_number| QueueRow {
            slot_number,
            queued: queued
                .iter()
                .position(|q| q.slot_number == slot_number)
                .map(|i| queued.remove(i)),
            types: String::new(),
            passed: slot_number <= seat.picks,
        })
        .collect();
    for row in &mut page.slots {
        if let Some(q) = &row.queued
            && let Some(l) = page.available.iter().find(|l| l.pokemon_id == q.pokemon_id)
        {
            row.types.clone_from(&l.types);
        }
    }
    let open = || page.slots.iter().filter(|r| !r.passed);
    page.next_slot =
        open().find(|r| r.queued.is_none()).or_else(|| open().next()).map(|r| r.slot_number);

    Ok(Html(page.render()?))
}

#[derive(Debug, Deserialize)]
struct QueueForm {
    slot_number: i64,
    pokemon_id: i64,
}

/// Queues a Pokémon for one of the signed-in coach's future picks.
///
/// The slot is keyed to the requester's own coach row, so a coach cannot write
/// into anyone else's queue regardless of what they post.
async fn set_queue(
    CurrentUser(person): CurrentUser,
    State(db): State<Db>,
    Form(form): Form<QueueForm>,
) -> Result<impl IntoResponse, AppError> {
    let outcome = async {
        let season = db.active_season().await?.ok_or(DraftError::NoSeason)?;
        let coach_id = db.coach_of(person.id, season.id).await?.ok_or(DraftError::NotACoach)?;
        db.set_queue_slot(coach_id, form.slot_number, form.pokemon_id).await
    }
    .await;
    back_to_draft(outcome.map(|()| "Queued."))
}

/// Empties one of the signed-in coach's queue slots.
async fn clear_queue(
    CurrentUser(person): CurrentUser,
    State(db): State<Db>,
    Path(slot): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let outcome = async {
        let season = db.active_season().await?.ok_or(DraftError::NoSeason)?;
        let coach_id = db.coach_of(person.id, season.id).await?.ok_or(DraftError::NotACoach)?;
        db.clear_queue_slot(coach_id, slot).await?;
        Ok(())
    }
    .await;
    back_to_draft(outcome.map(|()| "Slot cleared."))
}

/// Back to the draft page carrying a one-line outcome.
fn back_to_draft(result: Result<&str, DraftError>) -> Result<Redirect, AppError> {
    let (key, msg) = match result {
        Ok(msg) => ("ok", msg.to_owned()),
        Err(DraftError::Sqlx(e)) => return Err(e.into()),
        Err(e) => ("err", e.to_string()),
    };
    Ok(Redirect::to(&format!("/draft?{key}={}", urlencode(&msg))))
}

/// Reopens a coach's draft after they finished early. Admin correction.
async fn reopen_coach(
    _admin: AdminUser,
    State(db): State<Db>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    db.reopen_drafting(id).await?;
    back_to_admin(Ok("Coach reopened for drafting."))
}

#[derive(Debug, Deserialize)]
struct UndoForm {
    coach_id: i64,
    pick_number: i64,
}

/// Undoes the season's latest pick. Admin correction.
async fn undo_pick(
    AdminUser(admin): AdminUser,
    State(db): State<Db>,
    Form(form): Form<UndoForm>,
) -> Result<impl IntoResponse, AppError> {
    let (key, msg) = match db.undo_pick(form.coach_id, form.pick_number, admin.id).await {
        Ok(()) => ("ok", "Pick undone.".to_owned()),
        Err(DraftError::Sqlx(e)) => return Err(e.into()),
        Err(e) => ("err", e.to_string()),
    };
    Ok(Redirect::to(&format!("/admin?{key}={}", urlencode(&msg))))
}

#[derive(Template)]
#[template(path = "admin.html")]
struct AdminTemplate {
    layout: Layout,
    season: Option<Season>,
    coaches: Vec<Coach>,
    /// The only pick an admin can undo.
    last_pick: Option<Pick>,
    candidates: Vec<Person>,
    row_errors: Vec<String>,
}

impl AdminTemplate {
    /// A fresh page with no import rows attached.
    fn new(admin: Person, season: Option<Season>, flash: Flash) -> Self {
        Self {
            layout: Layout::new(Some(&CurrentUser(admin)), "admin", flash),
            season,
            coaches: Vec::new(),
            last_pick: None,
            candidates: Vec::new(),
            row_errors: Vec::new(),
        }
    }

    /// Loads the coach roster for whichever season the page is showing.
    async fn with_roster(mut self, db: &Db) -> Result<Self, AppError> {
        if let Some(season) = &self.season {
            self.coaches = db.coaches(season.id).await?;
            self.candidates = db.people_without_coach(season.id).await?;
            self.last_pick = db.recent_picks(season.id, 1).await?.pop();
        }
        Ok(self)
    }
}

/// Flash messages carried across a redirect. Short enough for a query string;
/// the CSV importer renders in place instead because its list is not.
#[derive(Debug, Default, Deserialize)]
struct Flash {
    ok: Option<String>,
    err: Option<String>,
}

/// Admin console. `AdminUser` is the access check; the template only decorates.
async fn admin(
    AdminUser(me): AdminUser,
    State(db): State<Db>,
    Query(flash): Query<Flash>,
) -> Result<impl IntoResponse, AppError> {
    let page = AdminTemplate::new(me, db.active_season().await?, flash).with_roster(&db).await?;
    Ok(Html(page.render()?))
}

/// Back to the admin page carrying a one-line outcome.
fn back_to_admin(result: Result<&str, CoachError>) -> Result<Redirect, AppError> {
    let (key, msg) = match result {
        Ok(msg) => ("ok", msg.to_owned()),
        Err(CoachError::Sqlx(e)) => return Err(e.into()),
        Err(e) => ("err", e.to_string()),
    };
    Ok(Redirect::to(&format!("/admin?{key}={}", urlencode(&msg))))
}

/// Percent-encodes a flash message for the redirect query string.
fn urlencode(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

#[derive(Debug, Deserialize)]
struct AddCoach {
    /// Existing person, when one was chosen from the list.
    person_id: Option<i64>,
    /// Hand-entered Discord ID, used when no person was chosen.
    discord_id: Option<String>,
    discord_username: Option<String>,
    budget: i64,
    draft_position: i64,
}

/// Adds a coach to the active season.
async fn add_coach(
    _admin: AdminUser,
    State(db): State<Db>,
    Form(form): Form<AddCoach>,
) -> Result<impl IntoResponse, AppError> {
    let Some(season) = db.active_season().await? else {
        return back_to_admin(Err(CoachError::Duplicate)).map(IntoResponse::into_response);
    };

    let outcome = async {
        let person_id = if let Some(id) = form.person_id {
            id
        } else {
            let discord_id = form.discord_id.unwrap_or_default();
            db.person_by_hand(&discord_id, form.discord_username.as_deref().unwrap_or("")).await?
        };
        db.add_coach(season.id, person_id, form.budget, form.draft_position).await
    }
    .await;

    back_to_admin(outcome.map(|()| "Coach added.")).map(IntoResponse::into_response)
}

#[derive(Debug, Deserialize)]
struct EditCoach {
    budget: i64,
    draft_position: i64,
    team_name: String,
}

/// Updates one coach's budget and draft position.
async fn update_coach(
    _admin: AdminUser,
    State(db): State<Db>,
    Path(id): Path<i64>,
    Form(form): Form<EditCoach>,
) -> Result<impl IntoResponse, AppError> {
    let outcome = db.update_coach(id, form.budget, form.draft_position, &form.team_name).await;
    back_to_admin(outcome.map(|()| "Coach updated."))
}

/// Drops a coach from the season.
async fn remove_coach(
    _admin: AdminUser,
    State(db): State<Db>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let outcome = db.remove_coach(id).await;
    back_to_admin(outcome.map(|()| "Coach removed."))
}

#[derive(Debug, Deserialize)]
struct EditMember {
    showdown_username: String,
    /// Absent when the checkbox is unticked, which is how HTML posts it.
    is_admin: Option<String>,
}

/// Sets another member's fields. The admin flag is re-checked here, not in the
/// template that drew the form.
async fn update_member(
    AdminUser(me): AdminUser,
    State(db): State<Db>,
    Path(id): Path<i64>,
    Form(form): Form<EditMember>,
) -> Result<impl IntoResponse, AppError> {
    // Dropping your own admin flag locks you out of this page, and if you are
    // the only admin it locks everyone out.
    let is_admin = if id == me.id { true } else { form.is_admin.is_some() };
    db.set_member_fields(id, Some(&form.showdown_username), is_admin).await?;
    let msg = if id == me.id && form.is_admin.is_none() {
        "Saved. You cannot remove your own admin rights here."
    } else {
        "Member updated."
    };
    back_to_admin(Ok(msg))
}

#[derive(Template)]
#[template(path = "profile.html")]
struct ProfileTemplate {
    layout: Layout,
    person: Person,
}

/// Everyone's own member fields.
async fn profile(
    user: CurrentUser,
    Query(flash): Query<Flash>,
) -> Result<impl IntoResponse, AppError> {
    // save_profile redirects with `ok=1`; the wording lives here.
    let flash = Flash { ok: flash.ok.map(|_| "Saved.".to_owned()), err: None };
    let layout = Layout::new(Some(&user), "", flash);
    Ok(Html(ProfileTemplate { layout, person: user.0 }.render()?))
}

#[derive(Debug, Deserialize)]
struct EditProfile {
    showdown_username: String,
}

/// Saves your own Showdown username. Admin status is not settable here.
async fn save_profile(
    CurrentUser(person): CurrentUser,
    State(db): State<Db>,
    Form(form): Form<EditProfile>,
) -> Result<impl IntoResponse, AppError> {
    db.set_showdown_username(person.id, &form.showdown_username).await?;
    Ok(Redirect::to("/profile?ok=1"))
}

#[derive(Debug, Deserialize)]
struct TierUpload {
    csv: String,
}

/// Imports a pasted tier sheet into the active season.
///
/// Renders the outcome in place rather than redirecting: a rejected upload
/// reports every bad row, which is too much to carry in a query string.
async fn import_tiers(
    AdminUser(me): AdminUser,
    State(db): State<Db>,
    Form(upload): Form<TierUpload>,
) -> Result<impl IntoResponse, AppError> {
    let Some(season) = db.active_season().await? else {
        return Ok(Html(AdminTemplate::new(me, None, Flash::default()).render()?));
    };

    let mut flash = Flash::default();
    let mut row_errors = Vec::new();
    match db.import_tiers(season.id, &upload.csv).await {
        Ok(n) => flash.ok = Some(format!("Imported {n} priced entries.")),
        Err(ImportError::Rows(errors)) => row_errors = errors.iter().map(ToString::to_string).collect(),
        Err(ImportError::Sqlx(e)) => return Err(e.into()),
        Err(e) => flash.err = Some(e.to_string()),
    }
    // Re-read so the entry count reflects what the import just did.
    let mut page = AdminTemplate::new(me, db.active_season().await?, flash);
    page.row_errors = row_errors;
    Ok(Html(page.with_roster(&db).await?.render()?))
}

#[derive(Debug, Deserialize)]
struct ScheduleUpload {
    csv: String,
}

/// Imports a pasted schedule export into the active season.
///
/// Renders in place for the same reason as the tier import.
async fn import_schedule(
    AdminUser(me): AdminUser,
    State(db): State<Db>,
    Form(upload): Form<ScheduleUpload>,
) -> Result<impl IntoResponse, AppError> {
    use crate::schedule::ImportError;

    let Some(season) = db.active_season().await? else {
        return Ok(Html(AdminTemplate::new(me, None, Flash::default()).render()?));
    };

    let mut flash = Flash::default();
    let mut row_errors = Vec::new();
    match db.import_schedule(season.id, &upload.csv).await {
        Ok(n) => flash.ok = Some(format!("Imported {n} matches.")),
        Err(ImportError::Rows(errors)) => row_errors = errors.iter().map(ToString::to_string).collect(),
        Err(ImportError::Sqlx(e)) => return Err(e.into()),
        Err(e) => flash.err = Some(e.to_string()),
    }
    let mut page = AdminTemplate::new(me, Some(season), flash);
    page.row_errors = row_errors;
    Ok(Html(page.with_roster(&db).await?.render()?))
}

#[derive(Template)]
#[template(path = "schedule.html")]
struct ScheduleTemplate {
    layout: Layout,
    season: Option<Season>,
    /// Matches grouped by week, in week order.
    weeks: Vec<(i64, Vec<Match>)>,
    /// The week on show.
    week: i64,
    /// The viewer's coach id this season, to highlight their matches.
    me: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct WeekQuery {
    week: Option<i64>,
}

/// One week's matches and results. Public, like the draft board.
async fn schedule_page(
    user: Option<CurrentUser>,
    State(db): State<Db>,
    Query(query): Query<WeekQuery>,
) -> Result<impl IntoResponse, AppError> {
    let layout = Layout::new(user.as_ref(), "schedule", Flash::default());
    let mut page = ScheduleTemplate { layout, season: None, weeks: Vec::new(), week: 0, me: None };
    let Some(season) = db.active_season().await? else {
        return Ok(Html(page.render()?));
    };
    if let Some(CurrentUser(person)) = user {
        page.me = db.coach_of(person.id, season.id).await?;
    }
    for m in db.matches(season.id).await? {
        match page.weeks.last_mut() {
            Some((week, matches)) if *week == m.week => matches.push(m),
            _ => page.weeks.push((m.week, vec![m])),
        }
    }
    // The first week with an unfinished match, or the last week once all are done.
    let current = page.weeks.iter().find(|(_, ms)| ms.iter().any(|m| m.winner.is_none())).or(page.weeks.last());
    page.week = query
        .week
        .filter(|w| page.weeks.iter().any(|(week, _)| week == w))
        .or(current.map(|(w, _)| *w))
        .unwrap_or_default();
    page.season = Some(season);
    Ok(Html(page.render()?))
}

#[derive(Template)]
#[template(path = "standings.html")]
struct StandingsTemplate {
    layout: Layout,
    season: Option<Season>,
    rows: Vec<Row>,
    /// Playoff matches in order: semifinals, then the final.
    playoffs: Vec<Match>,
    /// The viewer's coach id this season, to highlight their row.
    me: Option<i64>,
}

impl StandingsTemplate {
    /// Whether the semifinals have teams, so the real bracket replaces the projection.
    fn seeded(&self) -> bool {
        self.playoffs.first().is_some_and(|m| m.coach_a.is_some())
    }
}

/// The regular-season table. Public, like the schedule.
async fn standings_page(
    user: Option<CurrentUser>,
    State(db): State<Db>,
) -> Result<impl IntoResponse, AppError> {
    let layout = Layout::new(user.as_ref(), "standings", Flash::default());
    let mut page = StandingsTemplate { layout, season: None, rows: Vec::new(), playoffs: Vec::new(), me: None };
    let Some(season) = db.active_season().await? else {
        return Ok(Html(page.render()?));
    };
    if let Some(CurrentUser(person)) = user {
        page.me = db.coach_of(person.id, season.id).await?;
    }
    let matches = db.matches(season.id).await?;
    page.rows = crate::standings::standings(&db.coaches(season.id).await?, &matches);
    page.playoffs = matches.into_iter().filter(|m| m.is_playoff).collect();
    page.season = Some(season);
    Ok(Html(page.render()?))
}

#[derive(Template)]
#[template(path = "match.html")]
struct MatchTemplate {
    layout: Layout,
    m: Match,
    games: Vec<Game>,
    /// Whether the viewer may enter a result. Decoration only; the write re-checks.
    can_edit: bool,
    min_differential: i64,
    max_differential: i64,
}

/// One match: its games, its result, and the forms to change them.
async fn match_page(
    user: Option<CurrentUser>,
    State(db): State<Db>,
    Path(id): Path<i64>,
    Query(flash): Query<Flash>,
) -> Result<impl IntoResponse, AppError> {
    let m = db.match_by_id(id).await?.ok_or(AppError::NotFound)?;
    let layout = Layout::new(user.as_ref(), "schedule", flash);
    let can_edit = match user {
        Some(CurrentUser(p)) => p.is_admin || db.coach_of(p.id, m.season_id).await?.is_some(),
        None => false,
    };
    let range = crate::results::differential_range(m.best_of);
    let page = MatchTemplate {
        games: db.games(id).await?,
        can_edit,
        min_differential: *range.start(),
        max_differential: *range.end(),
        m,
        layout,
    };
    Ok(Html(page.render()?))
}

#[derive(Debug, Deserialize)]
struct ResultForm {
    /// Coach id of the winner; blank clears the result.
    winner: String,
    differential: Option<String>,
    /// Present when the forfeit box is ticked.
    forfeit: Option<String>,
}

/// Sets or clears a match result by hand.
async fn save_result(
    CurrentUser(person): CurrentUser,
    State(db): State<Db>,
    Path(id): Path<i64>,
    Form(form): Form<ResultForm>,
) -> Result<impl IntoResponse, AppError> {
    let entry = if form.winner.is_empty() {
        Ok(None)
    } else {
        let differential = form.differential.as_deref().unwrap_or_default().trim().parse::<i64>();
        match (form.winner.parse::<i64>(), form.forfeit.is_some(), differential) {
            (Err(_), ..) => Err("Choose a winner."),
            (Ok(winner), true, _) => Ok(Some(Entry { winner, score: Score::Forfeit })),
            (Ok(winner), false, Ok(d)) => Ok(Some(Entry { winner, score: Score::Differential(d) })),
            (Ok(_), false, Err(_)) => Err("Enter the winner's differential as a number."),
        }
    };
    let (key, msg) = match entry {
        Err(msg) => ("err", msg.to_owned()),
        Ok(entry) => match db.set_result(id, &person, entry).await {
            Ok(()) => ("ok", "Result saved.".to_owned()),
            Err(ResultError::NoMatch) => return Err(AppError::NotFound),
            Err(ResultError::NotACoach) => return Err(AppError::Forbidden),
            Err(ResultError::Sqlx(e)) => return Err(e.into()),
            Err(e) => ("err", e.to_string()),
        },
    };
    Ok(Redirect::to(&format!("/match/{id}?{key}={}", urlencode(&msg))))
}

#[derive(Debug, Deserialize)]
struct ReplayForm {
    url: String,
}

/// Attaches a Showdown replay to a match as one game.
async fn upload_replay(
    CurrentUser(person): CurrentUser,
    State(db): State<Db>,
    Path(id): Path<i64>,
    Form(form): Form<ReplayForm>,
) -> Result<impl IntoResponse, AppError> {
    let (key, msg) = match crate::replays::fetch(&form.url).await {
        Err(e) => {
            tracing::warn!(match_id = id, url = %form.url, error = ?e, "replay refused");
            ("err", e.to_string())
        }
        Ok(replay) => match db.add_game(id, &person, &replay).await {
            Ok(()) => ("ok", "Replay added.".to_owned()),
            Err(ResultError::NoMatch) => return Err(AppError::NotFound),
            Err(ResultError::NotACoach) => return Err(AppError::Forbidden),
            Err(ResultError::Sqlx(e)) => return Err(e.into()),
            Err(e) => ("err", e.to_string()),
        },
    };
    Ok(Redirect::to(&format!("/match/{id}?{key}={}", urlencode(&msg))))
}

/// Detaches a replay from a match.
async fn remove_game(
    CurrentUser(person): CurrentUser,
    State(db): State<Db>,
    Path((id, game_id)): Path<(i64, i64)>,
) -> Result<impl IntoResponse, AppError> {
    let (key, msg) = match db.remove_game(id, game_id, &person).await {
        Ok(()) => ("ok", "Replay removed.".to_owned()),
        Err(ResultError::NoMatch) => return Err(AppError::NotFound),
        Err(ResultError::NotACoach) => return Err(AppError::Forbidden),
        Err(ResultError::Sqlx(e)) => return Err(e.into()),
        Err(e) => ("err", e.to_string()),
    };
    Ok(Redirect::to(&format!("/match/{id}?{key}={}", urlencode(&msg))))
}
