mod auth;
mod config;
mod error;
mod models;
mod psp;
mod routes;
mod state;
mod webhooks;

use std::{sync::Arc, time::Duration};

use rand::Rng;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::{config::Config, state::AppState};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "invoice_service=debug,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Config::from_env().map_err(|e| format!("Config error: {e}"))?;

    let db = PgPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&config.database_url)
        .await?;

    // Run migrations
    sqlx::migrate!("../migrations").run(&db).await?;

    // On first run, create a demo business + API key
    seed_demo(&db).await?;

    // Shared HTTP client with PSP timeout baked in
    let http = reqwest::ClientBuilder::new()
        .timeout(Duration::from_secs(config.psp_timeout_secs))
        .build()?;

    let state = AppState {
        db: db.clone(),
        config: Arc::new(config.clone()),
        http,
    };

    // Webhook delivery background worker
    tokio::spawn(webhooks::worker(db.clone(), state.http.clone()));

    let app = routes::build(state);
    let addr = format!("0.0.0.0:{}", config.port);
    tracing::info!("listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn seed_demo(db: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM businesses")
            .fetch_one(db)
            .await?;

    if count > 0 {
        return Ok(());
    }

    // Generate a raw API key: dk_ + 48 random hex chars
    let raw: String = rand::thread_rng()
        .sample_iter(rand::distributions::Alphanumeric)
        .take(48)
        .map(char::from)
        .collect();
    let raw_key = format!("dk_{raw}");
    let key_prefix = &raw_key[..11]; // "dk_" + 8 chars

    let mut hasher = Sha256::new();
    hasher.update(raw_key.as_bytes());
    let key_hash = hex::encode(hasher.finalize());

    let mut tx = db.begin().await?;

    let business_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO businesses (name) VALUES ('Demo Business') RETURNING id",
    )
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO api_keys (business_id, key_prefix, key_hash) VALUES ($1, $2, $3)",
    )
    .bind(business_id)
    .bind(key_prefix)
    .bind(&key_hash)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    tracing::info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    tracing::info!("Demo business created. Your API key:");
    tracing::info!("  {raw_key}");
    tracing::info!("Use as: Authorization: Bearer {raw_key}");
    tracing::info!("This key is shown only once.");
    tracing::info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    Ok(())
}
