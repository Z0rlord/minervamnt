use crate::api::AppState;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct InfoResponse {
    pub name: String,
    pub description: String,
    pub pubkey: String,
    pub version: String,
    pub nuts: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct QuoteBolt11Request {
    pub amount: u64,
    pub unit: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct QuoteBolt11Response {
    pub quote: String,
    pub amount: u64,
    pub unit: String,
    pub request: String,
    pub state: String,
}

#[derive(Debug, Deserialize)]
pub struct MintBolt11Request {
    pub quote: String,
}

#[derive(Debug, Serialize)]
pub struct MintBolt11Response {
    pub proofs: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct MeltQuoteRequest {
    pub request: String,
    pub unit: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MeltQuoteResponse {
    pub quote: String,
    pub amount: u64,
    pub fee_reserve: u64,
    pub state: String,
}

#[derive(Debug, Deserialize)]
pub struct MeltBolt11Request {
    pub quote: String,
    pub proofs: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct MeltBolt11Response {
    pub paid: bool,
    pub payment_preimage: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SwapRequest {
    pub inputs: Vec<serde_json::Value>,
    pub outputs: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct SwapResponse {
    pub signatures: Vec<String>,
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/info", get(info))
        .route("/v1/mint/quote/bolt11", post(mint_quote_bolt11))
        .route("/v1/mint/quote/bolt11/{quote_id}", get(mint_quote_state))
        .route("/v1/mint/bolt11", post(mint_bolt11))
        .route("/v1/melt/quote/bolt11", post(melt_quote_bolt11))
        .route("/v1/melt/bolt11", post(melt_bolt11))
        .route("/v1/swap", post(swap))
}

async fn info(State(state): State<Arc<AppState>>) -> Json<InfoResponse> {
    Json(InfoResponse {
        name: state.config.mint.name.clone(),
        description: state.config.mint.description.clone(),
        pubkey: state.config.ark.server_pubkey.clone(),
        version: "0.1.0".to_string(),
        nuts: serde_json::json!({
            "4": { "methods": ["bolt11"], "disabled": false },
            "5": { "methods": ["bolt11"], "disabled": false },
            "7": { "supported": true },
            "8": { "supported": true },
            "9": { "supported": true },
            "10": { "supported": true }
        }),
    })
}

async fn mint_quote_bolt11(
    State(state): State<Arc<AppState>>,
    Json(body): Json<QuoteBolt11Request>,
) -> Json<QuoteBolt11Response> {
    let request = format!("lnbc{}n1pstub", body.amount);
    let quote = state
        .backend
        .create_mint_quote(body.amount, request.clone())
        .await;
    Json(QuoteBolt11Response {
        quote: quote.quote_id,
        amount: quote.amount_msat,
        unit: body.unit.unwrap_or_else(|| "sat".to_string()),
        request,
        state: "UNPAID".to_string(),
    })
}

async fn mint_quote_state(
    State(state): State<Arc<AppState>>,
    Path(quote_id): Path<String>,
) -> Result<Json<QuoteBolt11Response>, (axum::http::StatusCode, String)> {
    let Some(quote) = state.backend.get_mint_quote(&quote_id) else {
        return Err((axum::http::StatusCode::NOT_FOUND, "quote not found".into()));
    };
    Ok(Json(QuoteBolt11Response {
        quote: quote.quote_id,
        amount: quote.amount_msat,
        unit: "sat".to_string(),
        request: quote.request,
        state: "UNPAID".to_string(),
    }))
}

async fn mint_bolt11(
    State(state): State<Arc<AppState>>,
    Json(body): Json<MintBolt11Request>,
) -> Result<Json<MintBolt11Response>, (axum::http::StatusCode, String)> {
    let issued = state
        .backend
        .mint_tokens(&body.quote)
        .await
        .map_err(|e| (axum::http::StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(MintBolt11Response {
        proofs: vec![serde_json::json!({
            "id": issued.token_id,
            "amount": issued.amount_msat,
            "secret": issued.proofs[0]
        })],
    }))
}

async fn melt_quote_bolt11(
    State(state): State<Arc<AppState>>,
    Json(body): Json<MeltQuoteRequest>,
) -> Json<MeltQuoteResponse> {
    let quote = state
        .backend
        .create_melt_quote(1000, body.request)
        .await;
    Json(MeltQuoteResponse {
        quote: quote.quote_id,
        amount: quote.amount_msat,
        fee_reserve: 0,
        state: "UNPAID".to_string(),
    })
}

async fn melt_bolt11(
    State(state): State<Arc<AppState>>,
    Json(body): Json<MeltBolt11Request>,
) -> Result<Json<MeltBolt11Response>, (axum::http::StatusCode, String)> {
    let token_id = body
        .proofs
        .first()
        .and_then(|p| p.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown-token")
        .to_string();
    let preimage = state
        .backend
        .melt_tokens(&body.quote, &token_id)
        .await
        .map_err(|e| (axum::http::StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(MeltBolt11Response {
        paid: true,
        payment_preimage: Some(preimage),
    }))
}

async fn swap(
    State(state): State<Arc<AppState>>,
    Json(_body): Json<SwapRequest>,
) -> Result<Json<SwapResponse>, (axum::http::StatusCode, String)> {
    let issued = state
        .backend
        .swap_tokens(1000)
        .await
        .map_err(|e| (axum::http::StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(SwapResponse {
        signatures: issued.proofs,
    }))
}
