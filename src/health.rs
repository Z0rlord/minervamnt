use crate::config::AppConfig;
use crate::mint_backend::MintBackend;
use crate::ark_client::ArkClient;
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct HealthStatus {
    pub status: &'static str,
    pub mint: String,
    pub url: String,
    pub active_reserve_msat: u64,
    pub pending_refresh_count: usize,
    pub bitcoin_rpc_url: String,
    pub ark_server_url: String,
}

pub async fn collect_health<C: ArkClient>(
    config: &AppConfig,
    backend: &MintBackend<C>,
) -> HealthStatus {
    let active_reserve_msat = backend.active_reserve_msat().unwrap_or(0);
    let pending_refresh_count = backend.refresh_status().map(|q| q.len()).unwrap_or(0);

    HealthStatus {
        status: if pending_refresh_count == 0 {
            "healthy"
        } else {
            "degraded"
        },
        mint: config.mint.name.clone(),
        url: config.mint.url.clone(),
        active_reserve_msat,
        pending_refresh_count,
        bitcoin_rpc_url: config.bitcoin.rpc_url.clone(),
        ark_server_url: config.ark.server_url.clone(),
    }
}

pub struct HealthState<C: ArkClient> {
    pub config: Arc<AppConfig>,
    pub backend: Arc<MintBackend<C>>,
}
