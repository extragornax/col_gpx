mod auth;
mod climb;
mod db;
mod routes;
mod strava;

use std::sync::Arc;

pub struct AppState {
    pub db: db::Db,
    pub strava: Option<strava::StravaConfig>,
}

pub type SharedState = Arc<AppState>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,col_gpx=debug".into()),
        )
        .init();

    let db_path = std::env::var("DB_PATH").unwrap_or_else(|_| "data/col.db".into());
    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let db = db::Db::open(&db_path)?;
    db.migrate()?;

    let strava = strava::StravaConfig::from_env();
    if strava.is_some() {
        tracing::info!("Strava integration enabled");
    } else {
        tracing::info!("Strava integration disabled (set STRAVA_CLIENT_ID + STRAVA_CLIENT_SECRET to enable)");
    }

    let state: SharedState = Arc::new(AppState { db, strava });

    let app = routes::router().with_state(state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    tracing::info!("listening on :{port}");
    axum::serve(listener, app).await?;
    Ok(())
}
