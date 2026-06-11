use crate::api::AppState;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct VtxoProofResponse {
    pub token_id: String,
    pub vtxo_id: String,
    pub amount_msat: u64,
    pub branch_tx_hex: String,
    pub leaf_tx_hex: String,
}

#[derive(Debug, Serialize)]
pub struct ExitResponse {
    pub txid: String,
}

#[derive(Debug, Serialize)]
pub struct RefreshStatusResponse {
    pub pending: Vec<String>,
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/ark/vtxo/{token_id}", get(vtxo_proof))
        .route("/ark/exit", post(exit))
        .route("/ark/refresh/status", get(refresh_status))
}

async fn vtxo_proof(
    State(state): State<Arc<AppState>>,
    Path(token_id): Path<String>,
) -> Result<Json<VtxoProofResponse>, (axum::http::StatusCode, String)> {
    let vtxo = state
        .backend
        .get_vtxo_proof(&token_id)
        .await
        .map_err(|e| (axum::http::StatusCode::NOT_FOUND, e.to_string()))?;
    Ok(Json(VtxoProofResponse {
        token_id: token_id.clone(),
        vtxo_id: vtxo.id,
        amount_msat: vtxo.amount_msat,
        branch_tx_hex: vtxo.branch_tx_hex,
        leaf_tx_hex: vtxo.leaf_tx_hex,
    }))
}

#[derive(serde::Deserialize)]
pub struct ExitRequest {
    pub token_id: String,
}

async fn exit(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ExitRequest>,
) -> Result<Json<ExitResponse>, (axum::http::StatusCode, String)> {
    let txid = state
        .backend
        .initiate_exit(&body.token_id)
        .await
        .map_err(|e| (axum::http::StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(ExitResponse { txid }))
}

async fn refresh_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<RefreshStatusResponse>, (axum::http::StatusCode, String)> {
    let pending = state
        .backend
        .refresh_status()
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(RefreshStatusResponse { pending }))
}
