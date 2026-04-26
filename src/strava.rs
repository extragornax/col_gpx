use crate::climb::ProfilePoint;
use serde::Deserialize;

pub struct StravaConfig {
    pub client_id: String,
    pub client_secret: String,
    pub base_url: String,
    pub webhook_verify_token: String,
}

impl StravaConfig {
    pub fn from_env() -> Option<Self> {
        let client_id = std::env::var("STRAVA_CLIENT_ID").ok()?;
        let client_secret = std::env::var("STRAVA_CLIENT_SECRET").ok()?;
        let base_url = std::env::var("BASE_URL").unwrap_or_else(|_| "http://localhost:3000".into());
        let webhook_verify_token = std::env::var("STRAVA_WEBHOOK_VERIFY_TOKEN")
            .unwrap_or_else(|_| format!("col-verify-{}", &client_secret[..8.min(client_secret.len())]));
        Some(Self { client_id, client_secret, base_url, webhook_verify_token })
    }

    pub fn authorize_url(&self) -> String {
        format!(
            "https://www.strava.com/oauth/authorize?client_id={}&redirect_uri={}/auth/strava/callback&response_type=code&scope=activity:read_all&approval_prompt=auto",
            self.client_id, self.base_url,
        )
    }
}

#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub athlete: Athlete,
}

#[derive(Deserialize)]
pub struct RefreshResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
}

#[derive(Deserialize)]
pub struct Athlete {
    pub id: i64,
    pub firstname: Option<String>,
    pub lastname: Option<String>,
}

pub async fn exchange_code(config: &StravaConfig, code: &str) -> anyhow::Result<TokenResponse> {
    let client = reqwest::Client::new();
    let resp = client
        .post("https://www.strava.com/oauth/token")
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("code", code),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<TokenResponse>()
        .await?;
    Ok(resp)
}

pub async fn refresh_token(config: &StravaConfig, refresh: &str) -> anyhow::Result<RefreshResponse> {
    let client = reqwest::Client::new();
    let resp = client
        .post("https://www.strava.com/oauth/token")
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("refresh_token", refresh),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<RefreshResponse>()
        .await?;
    Ok(resp)
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
pub struct StravaActivity {
    pub id: i64,
    pub name: String,
    pub start_date_local: String,
    #[serde(rename = "type")]
    pub activity_type: String,
    pub total_elevation_gain: Option<f64>,
}

pub async fn fetch_activities(access_token: &str, page: u32) -> anyhow::Result<Vec<StravaActivity>> {
    let client = reqwest::Client::new();
    let resp = client
        .get("https://www.strava.com/api/v3/athlete/activities")
        .bearer_auth(access_token)
        .query(&[("per_page", "200"), ("page", &page.to_string())])
        .send()
        .await?
        .error_for_status()?
        .json::<Vec<StravaActivity>>()
        .await?;
    Ok(resp)
}

#[derive(Deserialize)]
struct StreamEntry {
    #[serde(rename = "type")]
    stream_type: String,
    data: serde_json::Value,
}

pub async fn fetch_streams(access_token: &str, activity_id: i64) -> anyhow::Result<Option<Vec<ProfilePoint>>> {
    let client = reqwest::Client::new();
    let url = format!("https://www.strava.com/api/v3/activities/{activity_id}/streams");
    let resp = client
        .get(&url)
        .bearer_auth(access_token)
        .query(&[("keys", "latlng,altitude,distance"), ("key_type", "distance")])
        .send()
        .await?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let resp = resp.error_for_status()?;
    let streams: Vec<StreamEntry> = resp.json().await?;

    let mut latlng: Option<Vec<[f64; 2]>> = None;
    let mut altitude: Option<Vec<f64>> = None;
    let mut distance: Option<Vec<f64>> = None;

    for s in streams {
        match s.stream_type.as_str() {
            "latlng" => latlng = serde_json::from_value(s.data).ok(),
            "altitude" => altitude = serde_json::from_value(s.data).ok(),
            "distance" => distance = serde_json::from_value(s.data).ok(),
            _ => {}
        }
    }

    let (Some(ll), Some(alt), Some(dist)) = (latlng, altitude, distance) else {
        return Ok(None);
    };

    if ll.len() != alt.len() || ll.len() != dist.len() {
        return Ok(None);
    }

    let profile: Vec<ProfilePoint> = ll
        .iter()
        .zip(alt.iter())
        .zip(dist.iter())
        .map(|((coord, &ele), &d)| (d / 1000.0, ele, coord[0], coord[1]))
        .collect();

    Ok(Some(profile))
}

pub async fn fetch_activity(access_token: &str, activity_id: i64) -> anyhow::Result<Option<StravaActivity>> {
    let client = reqwest::Client::new();
    let url = format!("https://www.strava.com/api/v3/activities/{activity_id}");
    let resp = client.get(&url).bearer_auth(access_token).send().await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    Ok(Some(resp.error_for_status()?.json().await?))
}
