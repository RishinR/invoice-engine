use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use crate::{error::AppError, state::AppState};

#[derive(Clone, Debug)]
pub struct AuthBusiness {
    pub business_id: Uuid,
}

pub fn hash_api_key(raw_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw_key.as_bytes());
    hex::encode(hasher.finalize())
}

pub async fn authenticate(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let raw_key = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(AppError::Unauthorized)?
        .to_string();

    let key_hash = hash_api_key(&raw_key);

    let row = sqlx::query(
        "SELECT business_id FROM api_keys WHERE key_hash = $1 AND revoked_at IS NULL",
    )
    .bind(&key_hash)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?
    .ok_or(AppError::Unauthorized)?;

    let business_id: Uuid = row.get("business_id");
    req.extensions_mut().insert(AuthBusiness { business_id });

    Ok(next.run(req).await)
}
