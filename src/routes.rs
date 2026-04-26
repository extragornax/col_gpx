use axum::extract::{Multipart, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;

use crate::auth::{self, CurrentUser};
use crate::climb;
use crate::strava;
use crate::SharedState;

const INDEX_HTML: &str = include_str!("../static/index.html");

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/", get(page_index))
        // Auth
        .route("/api/register", post(register))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/share-id", post(regenerate_share_link))
        // Climbs (protected)
        .route("/api/upload/gpx", post(upload_gpx))
        .route("/api/climbs", get(list_climbs))
        .route("/api/climbs/:id", get(get_climb))
        .route("/api/climbs/:id/name", put(rename_climb))
        .route("/api/stats", get(get_stats))
        .route("/api/reset", post(reset_data))
        // Strava (protected)
        .route("/auth/strava", get(strava_auth))
        .route("/auth/strava/callback", get(strava_callback))
        .route("/api/strava/status", get(strava_status))
        .route("/api/strava/sync", post(strava_sync))
        .route("/api/strava", delete(strava_disconnect))
        // Strava webhooks (public)
        .route("/webhook/strava", get(strava_webhook_verify))
        .route("/webhook/strava", post(strava_webhook_event))
        // Public profile
        .route("/p/:share_id", get(public_profile))
        .route("/api/public/:share_id/climbs", get(public_climbs))
        .route("/api/public/:share_id/stats", get(public_stats))
}

async fn page_index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

// ── Auth ──

#[derive(Deserialize)]
struct AuthBody {
    username: String,
    password: String,
}

async fn register(
    State(state): State<SharedState>,
    Json(body): Json<AuthBody>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    if body.username.len() < 2 || body.password.len() < 6 {
        return Err((StatusCode::BAD_REQUEST, "Username min 2 chars, password min 6 chars".into()));
    }
    if state.db.get_user_by_username(&body.username).map_err(err500)?.is_some() {
        return Err((StatusCode::CONFLICT, "Username taken".into()));
    }

    let hash = auth::hash_password(&body.password).map_err(err500)?;
    let share_id = auth::generate_share_id();
    let user_id = state.db.create_user(&body.username, &hash, &share_id).map_err(err500)?;

    let token = auth::generate_session_token();
    state.db.create_session(&token, user_id).map_err(err500)?;

    Ok((StatusCode::CREATED, session_headers(&token), Json(serde_json::json!({ "username": body.username, "share_id": share_id }))))
}

async fn login(
    State(state): State<SharedState>,
    Json(body): Json<AuthBody>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let (user_id, hash) = state.db.get_user_by_username(&body.username)
        .map_err(err500)?
        .ok_or((StatusCode::UNAUTHORIZED, "Invalid credentials".into()))?;

    if !auth::verify_password(&body.password, &hash).map_err(err500)? {
        return Err((StatusCode::UNAUTHORIZED, "Invalid credentials".into()));
    }

    let token = auth::generate_session_token();
    state.db.create_session(&token, user_id).map_err(err500)?;

    Ok((session_headers(&token), Json(serde_json::json!({ "username": body.username }))))
}

async fn logout(
    State(state): State<SharedState>,
    user: CurrentUser,
    headers: HeaderMap,
) -> Result<StatusCode, (StatusCode, String)> {
    let _ = user;
    if let Some(token) = extract_session_cookie(&headers) {
        state.db.delete_session(token).map_err(err500)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn me(
    State(state): State<SharedState>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (_, username) = state.db.get_user_by_id(user.0).map_err(err500)?
        .ok_or((StatusCode::UNAUTHORIZED, "User not found".into()))?;
    let share_id = state.db.get_share_id(user.0).map_err(err500)?
        .ok_or((StatusCode::UNAUTHORIZED, "User not found".into()))?;
    Ok(Json(serde_json::json!({ "username": username, "share_id": share_id })))
}

async fn regenerate_share_link(
    State(state): State<SharedState>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let new_id = auth::generate_share_id();
    state.db.regenerate_share_id(user.0, &new_id).map_err(err500)?;
    Ok(Json(serde_json::json!({ "share_id": new_id })))
}

fn session_headers(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "set-cookie",
        format!("session={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=2592000")
            .parse().unwrap(),
    );
    headers
}

fn extract_session_cookie(headers: &HeaderMap) -> Option<&str> {
    headers.get("cookie")?
        .to_str().ok()?
        .split(';')
        .filter_map(|s| s.trim().strip_prefix("session="))
        .next()
}

// ── Climbs ──

async fn upload_gpx(
    State(state): State<SharedState>,
    user: CurrentUser,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let mut total_climbs = 0usize;

    while let Some(field) = multipart.next_field().await.map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))? {
        let file_name = field.file_name().map(|s| s.to_string());
        let data = field.bytes().await.map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

        let gpx_profile = climb::profile_from_gpx(&data)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("GPX parse error: {e}")))?;

        let date = gpx_profile.date.as_deref().unwrap_or("unknown");
        let detected = climb::detect_climbs(&gpx_profile.points, 50.0);

        for c in &detected {
            let existing = state.db.find_nearby_climb(user.0, c.lat, c.lon, 0.5).map_err(err500)?;

            let climb_id = match existing {
                Some(id) => id,
                None => state.db.insert_climb(
                    user.0, c.lat, c.lon, c.start_ele, c.end_ele, c.gain,
                    c.end_km - c.start_km, c.gradient, date,
                ).map_err(err500)?,
            };

            state.db.add_attempt(climb_id, date, file_name.as_deref(), None).map_err(err500)?;
            total_climbs += 1;
        }
    }

    Ok(Json(serde_json::json!({ "climbs_processed": total_climbs })))
}

async fn list_climbs(
    State(state): State<SharedState>,
    user: CurrentUser,
) -> Result<Json<Vec<crate::db::ClimbRecord>>, (StatusCode, String)> {
    state.db.get_climbs(user.0).map(Json).map_err(err500)
}

async fn get_climb(
    State(state): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let climb = state.db.get_climb(user.0, id).map_err(err500)?
        .ok_or((StatusCode::NOT_FOUND, "Climb not found".into()))?;
    let attempts = state.db.get_attempts(id).map_err(err500)?;
    Ok(Json(serde_json::json!({ "climb": climb, "attempts": attempts })))
}

#[derive(Deserialize)]
struct RenameBody {
    name: String,
}

async fn rename_climb(
    State(state): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(body): Json<RenameBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let updated = state.db.rename_climb(user.0, id, &body.name).map_err(err500)?;
    if updated { Ok(StatusCode::NO_CONTENT) } else { Err((StatusCode::NOT_FOUND, "Not found".into())) }
}

async fn get_stats(
    State(state): State<SharedState>,
    user: CurrentUser,
) -> Result<Json<crate::db::Stats>, (StatusCode, String)> {
    state.db.get_stats(user.0).map(Json).map_err(err500)
}

async fn reset_data(
    State(state): State<SharedState>,
    user: CurrentUser,
) -> Result<StatusCode, (StatusCode, String)> {
    state.db.clear_user_data(user.0).map_err(err500)?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Strava OAuth ──

async fn strava_auth(
    State(state): State<SharedState>,
    _user: CurrentUser,
) -> Result<Redirect, (StatusCode, String)> {
    let config = state.strava.as_ref()
        .ok_or((StatusCode::NOT_FOUND, "Strava integration not configured".into()))?;
    Ok(Redirect::temporary(&config.authorize_url()))
}

#[derive(Deserialize)]
struct StravaCallbackParams {
    code: String,
}

async fn strava_callback(
    State(state): State<SharedState>,
    user: CurrentUser,
    Query(params): Query<StravaCallbackParams>,
) -> Result<Redirect, (StatusCode, String)> {
    let config = state.strava.as_ref()
        .ok_or((StatusCode::NOT_FOUND, "Strava integration not configured".into()))?;

    let token = strava::exchange_code(config, &params.code).await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Strava token exchange failed: {e}")))?;

    let name = match (&token.athlete.firstname, &token.athlete.lastname) {
        (Some(f), Some(l)) => Some(format!("{f} {l}")),
        (Some(f), None) => Some(f.clone()),
        _ => None,
    };

    state.db.save_strava_tokens(
        user.0, &token.access_token, &token.refresh_token,
        token.expires_at, token.athlete.id, name.as_deref(),
    ).map_err(err500)?;

    Ok(Redirect::temporary("/"))
}

async fn strava_status(
    State(state): State<SharedState>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let configured = state.strava.is_some();
    let tokens = state.db.get_strava_tokens(user.0).map_err(err500)?;

    Ok(Json(serde_json::json!({
        "configured": configured,
        "connected": tokens.is_some(),
        "athlete_name": tokens.as_ref().and_then(|t| t.athlete_name.clone()),
    })))
}

async fn strava_disconnect(
    State(state): State<SharedState>,
    user: CurrentUser,
) -> Result<StatusCode, (StatusCode, String)> {
    state.db.delete_strava_tokens(user.0).map_err(err500)?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Strava sync (background) ──

async fn strava_sync(
    State(state): State<SharedState>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let token = get_valid_token(&state, user.0).await?;

    tokio::spawn({
        let state = state.clone();
        let user_id = user.0;
        async move {
            if let Err(e) = run_sync(&state, user_id, &token).await {
                tracing::error!(user_id, "background sync failed: {e}");
            }
        }
    });

    Ok(Json(serde_json::json!({ "status": "sync_started" })))
}

async fn run_sync(state: &SharedState, user_id: i64, token: &str) -> anyhow::Result<()> {
    let mut page = 1u32;
    loop {
        let activities = strava::fetch_activities(token, page).await?;
        if activities.is_empty() {
            break;
        }

        for activity in &activities {
            if state.db.is_activity_synced(user_id, activity.id)? {
                continue;
            }

            if !matches!(activity.activity_type.as_str(), "Ride" | "VirtualRide" | "GravelRide" | "EBikeRide" | "Run" | "TrailRun" | "Hike" | "Walk") {
                state.db.mark_activity_synced(user_id, activity.id)?;
                continue;
            }

            process_activity(state, user_id, activity.id, &activity.name, &activity.start_date_local).await?;
            state.db.mark_activity_synced(user_id, activity.id)?;
        }

        if activities.len() < 200 {
            break;
        }
        page += 1;
    }
    tracing::info!(user_id, "sync complete");
    Ok(())
}

async fn process_activity(state: &SharedState, user_id: i64, activity_id: i64, name: &str, start_date: &str) -> anyhow::Result<()> {
    let config = state.strava.as_ref().ok_or_else(|| anyhow::anyhow!("Strava not configured"))?;
    let tokens = state.db.get_strava_tokens(user_id)?
        .ok_or_else(|| anyhow::anyhow!("No Strava tokens for user {user_id}"))?;

    let access_token = ensure_fresh_token(state, config, user_id, &tokens).await?;

    let streams = strava::fetch_streams(&access_token, activity_id).await?;
    let Some(profile) = streams else { return Ok(()); };

    let date = &start_date[..10.min(start_date.len())];
    let detected = climb::detect_climbs(&profile, 50.0);

    for c in &detected {
        let existing = state.db.find_nearby_climb(user_id, c.lat, c.lon, 0.5)?;
        let climb_id = match existing {
            Some(id) => id,
            None => state.db.insert_climb(
                user_id, c.lat, c.lon, c.start_ele, c.end_ele, c.gain,
                c.end_km - c.start_km, c.gradient, date,
            )?,
        };
        state.db.add_attempt(climb_id, date, Some(name), None)?;
    }
    Ok(())
}

async fn ensure_fresh_token(state: &SharedState, config: &strava::StravaConfig, user_id: i64, tokens: &crate::db::StravaTokens) -> anyhow::Result<String> {
    let now = chrono::Utc::now().timestamp();
    if now < tokens.expires_at - 60 {
        return Ok(tokens.access_token.clone());
    }
    let refreshed = strava::refresh_token(config, &tokens.refresh_token).await?;
    state.db.save_strava_tokens(
        user_id, &refreshed.access_token, &refreshed.refresh_token,
        refreshed.expires_at, tokens.athlete_id, tokens.athlete_name.as_deref(),
    )?;
    Ok(refreshed.access_token)
}

async fn get_valid_token(state: &SharedState, user_id: i64) -> Result<String, (StatusCode, String)> {
    let config = state.strava.as_ref()
        .ok_or((StatusCode::NOT_FOUND, "Strava not configured".into()))?;
    let tokens = state.db.get_strava_tokens(user_id).map_err(err500)?
        .ok_or((StatusCode::UNAUTHORIZED, "Strava not connected".into()))?;
    ensure_fresh_token(state, config, user_id, &tokens)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Token refresh failed: {e}")))
}

// ── Strava webhooks ──

#[derive(Deserialize)]
struct WebhookVerifyParams {
    #[serde(rename = "hub.mode")]
    mode: String,
    #[serde(rename = "hub.challenge")]
    challenge: String,
    #[serde(rename = "hub.verify_token")]
    verify_token: String,
}

async fn strava_webhook_verify(
    State(state): State<SharedState>,
    Query(params): Query<WebhookVerifyParams>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let config = state.strava.as_ref().ok_or(StatusCode::NOT_FOUND)?;

    if params.mode != "subscribe" || params.verify_token != config.webhook_verify_token {
        return Err(StatusCode::FORBIDDEN);
    }

    Ok(Json(serde_json::json!({ "hub.challenge": params.challenge })))
}

#[derive(Deserialize)]
struct WebhookEvent {
    object_type: String,
    object_id: i64,
    aspect_type: String,
    owner_id: i64,
}

async fn strava_webhook_event(
    State(state): State<SharedState>,
    Json(event): Json<WebhookEvent>,
) -> StatusCode {
    if event.object_type != "activity" || event.aspect_type != "create" {
        return StatusCode::OK;
    }

    tokio::spawn(async move {
        if let Err(e) = handle_webhook_activity(&state, event.owner_id, event.object_id).await {
            tracing::error!(athlete_id = event.owner_id, activity_id = event.object_id, "webhook processing failed: {e}");
        }
    });

    StatusCode::OK
}

async fn handle_webhook_activity(state: &SharedState, athlete_id: i64, activity_id: i64) -> anyhow::Result<()> {
    let (user_id, tokens) = state.db.get_strava_tokens_by_athlete(athlete_id)?
        .ok_or_else(|| anyhow::anyhow!("No user linked to athlete {athlete_id}"))?;

    if state.db.is_activity_synced(user_id, activity_id)? {
        return Ok(());
    }

    let config = state.strava.as_ref().ok_or_else(|| anyhow::anyhow!("Strava not configured"))?;
    let access_token = ensure_fresh_token(state, config, user_id, &tokens).await?;

    let activity = strava::fetch_activity(&access_token, activity_id).await?;
    let Some(activity) = activity else { return Ok(()); };

    if !matches!(activity.activity_type.as_str(), "Ride" | "VirtualRide" | "GravelRide" | "EBikeRide" | "Run" | "TrailRun" | "Hike" | "Walk") {
        state.db.mark_activity_synced(user_id, activity_id)?;
        return Ok(());
    }

    process_activity(state, user_id, activity_id, &activity.name, &activity.start_date_local).await?;
    state.db.mark_activity_synced(user_id, activity_id)?;
    tracing::info!(user_id, activity_id, "webhook: processed activity");
    Ok(())
}

// ── Public profile ──

async fn public_profile() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn public_climbs(
    State(state): State<SharedState>,
    Path(share_id): Path<String>,
) -> Result<Json<Vec<crate::db::ClimbRecord>>, (StatusCode, String)> {
    let user_id = state.db.get_user_by_share_id(&share_id).map_err(err500)?
        .ok_or((StatusCode::NOT_FOUND, "Not found".into()))?;
    state.db.get_climbs(user_id).map(Json).map_err(err500)
}

async fn public_stats(
    State(state): State<SharedState>,
    Path(share_id): Path<String>,
) -> Result<Json<crate::db::Stats>, (StatusCode, String)> {
    let user_id = state.db.get_user_by_share_id(&share_id).map_err(err500)?
        .ok_or((StatusCode::NOT_FOUND, "Not found".into()))?;
    state.db.get_stats(user_id).map(Json).map_err(err500)
}

fn err500(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}
