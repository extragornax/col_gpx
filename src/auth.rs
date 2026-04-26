use axum::async_trait;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;

use crate::SharedState;

pub struct CurrentUser(pub i64);

#[async_trait]
impl FromRequestParts<SharedState> for CurrentUser {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, state: &SharedState) -> Result<Self, Self::Rejection> {
        let cookie = parts.headers
            .get("cookie")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        let token = cookie.split(';')
            .filter_map(|s| {
                let s = s.trim();
                s.strip_prefix("session=")
            })
            .next()
            .ok_or(StatusCode::UNAUTHORIZED)?;

        let user_id = state.db.get_session(token)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::UNAUTHORIZED)?;

        Ok(CurrentUser(user_id))
    }
}

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    Ok(bcrypt::hash(password, bcrypt::DEFAULT_COST)?)
}

pub fn verify_password(password: &str, hash: &str) -> anyhow::Result<bool> {
    Ok(bcrypt::verify(password, hash)?)
}

pub fn generate_session_token() -> String {
    use rand::Rng;
    let bytes: [u8; 32] = rand::rng().random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
