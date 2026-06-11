//! HTTP API: standard Cashu NUT endpoints plus Ark extensions.

use crate::error::Result;
use crate::health::collect_health;
use crate::mint_backend::MintBackend;
use crate::types::*;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use std::sync::Arc;

pub type AppState = Arc<MintBackend>;

pub fn router(backend: AppState) -> Router {
    Router::new()
        // Cashu NUT endpoints
        .route("/v1/info", get(info))
        .route("/v1/mint/quote/bolt11", post(mint_quote))
        .route("/v1/mint/quote/bolt11/{quote_id}", get(get_mint_quote))
        .route("/v1/mint/bolt11", post(mint))
        .route("/v1/melt/quote/bolt11", post(melt_quote))
        .route("/v1/melt/quote/bolt11/{quote_id}", get(get_melt_quote))
        .route("/v1/melt/bolt11", post(melt))
        .route("/v1/swap", post(swap))
        // Ark extensions
        .route("/ark/vtxo/{token_id}", get(token_vtxo))
        .route("/ark/exit", post(ark_exit))
        .route("/ark/refresh/status", get(refresh_status))
        // Transparency + PoL
        .route("/transparency/summary", get(transparency_summary))
        .route("/v1/pol/status", get(pol_status))
        .route("/v1/pol/roots/{keyset_id}", get(pol_roots))
        .route("/v1/pol/ots/{epoch_day}", get(pol_ots))
        // Ops
        .route("/health", get(health))
        .with_state(backend)
}

async fn info(State(b): State<AppState>) -> Json<MintInfo> {
    Json(b.info())
}

async fn mint_quote(
    State(b): State<AppState>,
    Json(req): Json<MintQuoteBolt11Request>,
) -> Result<Json<MintQuoteBolt11Response>> {
    Ok(Json(b.mint_quote(req).await?))
}

async fn get_mint_quote(
    State(b): State<AppState>,
    Path(quote_id): Path<String>,
) -> Result<Json<MintQuoteBolt11Response>> {
    Ok(Json(b.get_mint_quote(&quote_id).await?))
}

async fn mint(
    State(b): State<AppState>,
    Json(req): Json<MintBolt11Request>,
) -> Result<Json<MintBolt11Response>> {
    Ok(Json(b.mint(req).await?))
}

async fn melt_quote(
    State(b): State<AppState>,
    Json(req): Json<MeltQuoteBolt11Request>,
) -> Result<Json<MeltQuoteBolt11Response>> {
    Ok(Json(b.melt_quote(req).await?))
}

async fn get_melt_quote(
    State(b): State<AppState>,
    Path(quote_id): Path<String>,
) -> Result<Json<MeltQuoteBolt11Response>> {
    Ok(Json(b.get_melt_quote(&quote_id).await?))
}

async fn melt(
    State(b): State<AppState>,
    Json(req): Json<MeltBolt11Request>,
) -> Result<Json<MeltBolt11Response>> {
    Ok(Json(b.melt(req).await?))
}

async fn swap(
    State(b): State<AppState>,
    Json(req): Json<SwapRequest>,
) -> Result<Json<SwapResponse>> {
    Ok(Json(b.swap(req).await?))
}

async fn token_vtxo(
    State(b): State<AppState>,
    Path(token_id): Path<String>,
) -> Result<Json<TokenVtxoResponse>> {
    Ok(Json(b.token_vtxo(&token_id).await?))
}

async fn ark_exit(
    State(b): State<AppState>,
    Json(req): Json<ExitRequest>,
) -> Result<Json<ExitResponse>> {
    Ok(Json(b.unilateral_exit(&req.token_id).await?))
}

async fn refresh_status(State(b): State<AppState>) -> Result<Json<RefreshStatusResponse>> {
    Ok(Json(b.refresh_status().await?))
}

async fn transparency_summary(
    State(b): State<AppState>,
) -> Result<Json<TransparencySummary>> {
    Ok(Json(b.transparency_summary().await?))
}

async fn pol_status(State(b): State<AppState>) -> Result<Json<PolStatusResponse>> {
    Ok(Json(b.pol_status()?))
}

async fn pol_roots(
    State(b): State<AppState>,
    Path(keyset_id): Path<String>,
) -> Result<Json<PolRootsResponse>> {
    Ok(Json(b.pol_roots(&keyset_id)?))
}

async fn pol_ots(
    State(b): State<AppState>,
    Path(epoch_day): Path<String>,
) -> Result<Json<PolOtsResponse>> {
    Ok(Json(b.pol_ots_proof(&epoch_day)?))
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    ark_connected: bool,
    block_height: Option<u64>,
    mint: String,
    url: String,
    active_reserve_msat: u64,
    pending_refresh_count: usize,
    bitcoin_rpc_url: String,
    bitcoin_chain: Option<String>,
    bitcoin_blocks: Option<u64>,
    bitcoin_synced: Option<bool>,
    bitcoin_rpc_error: Option<String>,
    ark_server_url: String,
}

async fn health(State(b): State<AppState>) -> Json<Health> {
    let ark_connected = b.ark().ping().await.is_ok();
    let block_height = b.ark().current_block_height().await.ok();
    let detailed = collect_health(b.config(), &b).await;

    // Ark connectivity drives the top-level status; Bitcoin RPC is reported
    // separately so /health stays useful during IBD or missing .env in dev.
    let status = if ark_connected { "ok" } else { "degraded" };

    Json(Health {
        status,
        ark_connected,
        block_height,
        mint: detailed.mint,
        url: detailed.url,
        active_reserve_msat: detailed.active_reserve_msat,
        pending_refresh_count: detailed.pending_refresh_count,
        bitcoin_rpc_url: detailed.bitcoin_rpc_url,
        bitcoin_chain: detailed.bitcoin_chain,
        bitcoin_blocks: detailed.bitcoin_blocks,
        bitcoin_synced: detailed.bitcoin_synced,
        bitcoin_rpc_error: detailed.bitcoin_rpc_error,
        ark_server_url: detailed.ark_server_url,
    })
}
