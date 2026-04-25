use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;

use crate::climb;
use crate::SharedState;

const INDEX_HTML: &str = include_str!("../static/index.html");

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/", get(page_index))
        .route("/api/upload/gpx", post(upload_gpx))
        .route("/api/upload/strava-csv", post(upload_strava_csv))
        .route("/api/climbs", get(list_climbs))
        .route("/api/climbs/:id", get(get_climb))
        .route("/api/climbs/:id/name", put(rename_climb))
        .route("/api/stats", get(get_stats))
        .route("/api/reset", post(reset_data))
}

async fn page_index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn upload_gpx(
    State(state): State<SharedState>,
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
            let existing = state.db.find_nearby_climb(c.lat, c.lon, 0.5)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            let climb_id = match existing {
                Some(id) => id,
                None => state.db.insert_climb(
                    c.lat, c.lon, c.start_ele, c.end_ele, c.gain,
                    c.end_km - c.start_km, c.gradient, date,
                ).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
            };

            state.db.add_attempt(climb_id, date, file_name.as_deref(), None)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            total_climbs += 1;
        }
    }

    Ok(Json(serde_json::json!({ "climbs_processed": total_climbs })))
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct StravaCsvRow {
    #[serde(rename = "Activity Date")]
    activity_date: Option<String>,
    #[serde(rename = "Activity Name")]
    activity_name: Option<String>,
    #[serde(rename = "Activity Type")]
    activity_type: Option<String>,
    #[serde(rename = "Filename")]
    filename: Option<String>,
    #[serde(rename = "Elevation Gain")]
    elevation_gain: Option<String>,
}

async fn upload_strava_csv(
    State(state): State<SharedState>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let mut gpx_files: Vec<(String, String, Vec<u8>)> = Vec::new(); // (date, name, gpx_bytes)
    let mut csv_rows: Vec<StravaCsvRow> = Vec::new();

    while let Some(field) = multipart.next_field().await.map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))? {
        let name = field.name().unwrap_or("").to_string();
        let data = field.bytes().await.map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

        if name == "csv" {
            let mut rdr = csv::Reader::from_reader(data.as_ref());
            for result in rdr.deserialize() {
                let row: StravaCsvRow = result.map_err(|e| (StatusCode::BAD_REQUEST, format!("CSV error: {e}")))?;
                csv_rows.push(row);
            }
        } else if name == "gpx" {
            gpx_files.push(("unknown".into(), "unknown".into(), data.to_vec()));
        }
    }

    // Match CSV rows to GPX files by index or process GPX files standalone
    let mut total_climbs = 0usize;
    let mut activities_processed = 0usize;

    let files_to_process: Vec<(String, Option<String>, Vec<u8>)> = if !csv_rows.is_empty() && !gpx_files.is_empty() {
        gpx_files.into_iter().enumerate().map(|(i, (_, _, data))| {
            let row = csv_rows.get(i);
            let date = row.and_then(|r| r.activity_date.clone()).unwrap_or_else(|| "unknown".into());
            let name = row.and_then(|r| r.activity_name.clone());
            (date, name, data)
        }).collect()
    } else {
        gpx_files.into_iter().map(|(d, _, data)| (d, None, data)).collect()
    };

    for (date, activity_name, gpx_data) in &files_to_process {
        let gpx_profile = match climb::profile_from_gpx(gpx_data) {
            Ok(p) => p,
            Err(_) => continue,
        };

        let date = if date == "unknown" {
            gpx_profile.date.as_deref().unwrap_or("unknown")
        } else {
            date.as_str()
        };
        let detected = climb::detect_climbs(&gpx_profile.points, 50.0);
        activities_processed += 1;

        for c in &detected {
            let existing = state.db.find_nearby_climb(c.lat, c.lon, 0.5)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            let climb_id = match existing {
                Some(id) => id,
                None => state.db.insert_climb(
                    c.lat, c.lon, c.start_ele, c.end_ele, c.gain,
                    c.end_km - c.start_km, c.gradient, date,
                ).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
            };

            state.db.add_attempt(climb_id, date, activity_name.as_deref(), None)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            total_climbs += 1;
        }
    }

    Ok(Json(serde_json::json!({
        "activities_processed": activities_processed,
        "climbs_processed": total_climbs,
    })))
}

async fn list_climbs(
    State(state): State<SharedState>,
) -> Result<Json<Vec<crate::db::ClimbRecord>>, (StatusCode, String)> {
    state.db.get_climbs()
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn get_climb(
    State(state): State<SharedState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let climb = state.db.get_climb(id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "Climb not found".into()))?;

    let attempts = state.db.get_attempts(id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(serde_json::json!({
        "climb": climb,
        "attempts": attempts,
    })))
}

#[derive(Deserialize)]
struct RenameBody {
    name: String,
}

async fn rename_climb(
    State(state): State<SharedState>,
    Path(id): Path<i64>,
    Json(body): Json<RenameBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let updated = state.db.rename_climb(id, &body.name)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if updated { Ok(StatusCode::NO_CONTENT) } else { Err((StatusCode::NOT_FOUND, "Not found".into())) }
}

async fn get_stats(
    State(state): State<SharedState>,
) -> Result<Json<crate::db::Stats>, (StatusCode, String)> {
    state.db.get_stats()
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn reset_data(
    State(state): State<SharedState>,
) -> Result<StatusCode, (StatusCode, String)> {
    state.db.clear_all()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}
