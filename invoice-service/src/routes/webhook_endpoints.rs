use axum::{
    extract::{Extension, State},
    http::StatusCode,
    Json,
};
use rand::Rng;

use crate::{
    auth::AuthBusiness,
    error::AppError,
    models::webhook::{CreateWebhookEndpointRequest, WebhookEndpoint},
    state::AppState,
};

pub async fn create(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthBusiness>,
    Json(req): Json<CreateWebhookEndpointRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    if req.url.trim().is_empty() {
        return Err(AppError::Unprocessable("url is required".into()));
    }

    // 32-byte random secret for HMAC signing
    let secret: String = rand::thread_rng()
        .sample_iter(rand::distributions::Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();

    let endpoint = sqlx::query_as::<_, WebhookEndpoint>(
        r#"INSERT INTO webhook_endpoints (business_id, url, secret)
           VALUES ($1, $2, $3)
           RETURNING *"#,
    )
    .bind(auth.business_id)
    .bind(req.url.trim())
    .bind(&secret)
    .fetch_one(&state.db)
    .await?;

    // Return the secret only on creation — never shown again
    let resp = serde_json::json!({
        "id": endpoint.id,
        "url": endpoint.url,
        "active": endpoint.active,
        "created_at": endpoint.created_at,
        "signing_secret": secret,
    });

    Ok((StatusCode::CREATED, Json(resp)))
}

pub async fn list(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthBusiness>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let endpoints = sqlx::query_as::<_, WebhookEndpoint>(
        "SELECT * FROM webhook_endpoints WHERE business_id = $1 ORDER BY created_at DESC",
    )
    .bind(auth.business_id)
    .fetch_all(&state.db)
    .await?;

    let resp: Vec<serde_json::Value> = endpoints
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "id": e.id,
                "url": e.url,
                "active": e.active,
                "created_at": e.created_at,
            })
        })
        .collect();

    Ok(Json(resp))
}
