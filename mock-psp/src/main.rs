use axum::{routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use uuid::Uuid;

#[derive(Deserialize)]
struct ChargeRequest {
    card_token: String,
}

#[derive(Serialize)]
struct ChargeResponse {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    psp_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'static str>,
}

async fn charge(Json(req): Json<ChargeRequest>) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    match req.card_token.as_str() {
        "tok_success" => {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Json(ChargeResponse {
                status: "succeeded",
                psp_ref: Some(Uuid::new_v4().to_string()),
                code: None,
            })
            .into_response()
        }

        "tok_insufficient_funds" => {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Json(ChargeResponse {
                status: "failed",
                psp_ref: None,
                code: Some("insufficient_funds"),
            })
            .into_response()
        }

        "tok_card_declined" => {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Json(ChargeResponse {
                status: "failed",
                psp_ref: None,
                code: Some("card_declined"),
            })
            .into_response()
        }

        "tok_timeout" => {
            // Sleep longer than the invoice-service timeout (10 s) to trigger it
            tokio::time::sleep(Duration::from_secs(30)).await;
            Json(ChargeResponse {
                status: "succeeded",
                psp_ref: Some(Uuid::new_v4().to_string()),
                code: None,
            })
            .into_response()
        }

        "tok_network_error" => {
            // Return HTTP 500 — invoice-service treats non-2xx as network error
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "internal server error" })),
            )
                .into_response()
        }

        _ => {
            // Unknown token → decline
            tokio::time::sleep(Duration::from_millis(100)).await;
            Json(ChargeResponse {
                status: "failed",
                psp_ref: None,
                code: Some("card_declined"),
            })
            .into_response()
        }
    }
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    let port: u16 = std::env::var("PSP_PORT")
        .unwrap_or_else(|_| "4000".into())
        .parse()
        .expect("PSP_PORT must be a valid port number");

    let app = Router::new().route("/charge", post(charge));

    let addr = format!("0.0.0.0:{port}");
    tracing::info!("mock-psp listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind");
    axum::serve(listener, app)
        .await
        .expect("server failed");
}
