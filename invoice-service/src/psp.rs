use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct PspResponse {
    pub status: String,
    pub psp_ref: Option<String>,
    pub code: Option<String>,
}

#[derive(Debug)]
pub enum PspResult {
    Succeeded { psp_ref: String },
    Failed { code: String },
    Timeout,
    NetworkError(String),
}

pub async fn charge(
    http: &reqwest::Client,
    psp_url: &str,
    card_token: &str,
) -> PspResult {
    let url = format!("{}/charge", psp_url);
    let body = serde_json::json!({ "card_token": card_token });

    // Timeout is already baked into the reqwest client in AppState.
    // tok_timeout sleeps 30s; our client has a 10s timeout — it fires first.
    let result = http.post(&url).json(&body).send().await;

    let response = match result {
        Err(e) if e.is_timeout() => return PspResult::Timeout,
        Err(e) => return PspResult::NetworkError(e.to_string()),
        Ok(r) => r,
    };

    if !response.status().is_success() {
        return PspResult::NetworkError(format!("PSP HTTP {}", response.status()));
    }

    let psp: PspResponse = match response.json().await {
        Err(e) => return PspResult::NetworkError(e.to_string()),
        Ok(p) => p,
    };

    match psp.status.as_str() {
        "succeeded" => PspResult::Succeeded {
            psp_ref: psp.psp_ref.unwrap_or_default(),
        },
        _ => PspResult::Failed {
            code: psp.code.unwrap_or_else(|| "unknown".into()),
        },
    }
}
