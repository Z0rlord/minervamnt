use crate::bitcoin::{get_blockchain_info, BlockchainInfo};
use crate::config::AppConfig;
use crate::mint_backend::MintBackend;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct HealthStatus {
    pub status: &'static str,
    pub mint: String,
    pub url: String,
    pub active_reserve_msat: u64,
    pub pending_refresh_count: usize,
    pub bitcoin_rpc_url: String,
    pub bitcoin_chain: Option<String>,
    pub bitcoin_blocks: Option<u64>,
    pub bitcoin_synced: Option<bool>,
    pub bitcoin_rpc_error: Option<String>,
    pub ark_server_url: String,
}

pub async fn collect_health(config: &AppConfig, backend: &MintBackend) -> HealthStatus {
    let active_reserve_msat = backend.inventory().free_reserve_msat().unwrap_or(0);
    let pending_refresh_count = backend
        .refresh_status()
        .await
        .map(|r| r.pending_refreshes)
        .unwrap_or(0);
    let bitcoin = probe_bitcoin(&config.bitcoin).await;

    let mut status = if pending_refresh_count == 0 {
        "healthy"
    } else {
        "degraded"
    };
    if bitcoin.rpc_error.is_some() {
        status = "degraded";
    }

    HealthStatus {
        status,
        mint: config.mint.name.clone(),
        url: config.mint.url.clone(),
        active_reserve_msat,
        pending_refresh_count,
        bitcoin_rpc_url: config.bitcoin.rpc_url.clone(),
        bitcoin_chain: bitcoin.chain,
        bitcoin_blocks: bitcoin.blocks,
        bitcoin_synced: bitcoin.synced,
        bitcoin_rpc_error: bitcoin.rpc_error,
        ark_server_url: config.ark.server_url.clone(),
    }
}

struct BitcoinProbe {
    chain: Option<String>,
    blocks: Option<u64>,
    synced: Option<bool>,
    rpc_error: Option<String>,
}

async fn probe_bitcoin(config: &crate::config::BitcoinConfig) -> BitcoinProbe {
    match get_blockchain_info(config).await {
        Ok(info) => {
            let synced = is_synced(&info);
            BitcoinProbe {
                chain: Some(info.chain),
                blocks: Some(info.blocks),
                synced: Some(synced),
                rpc_error: None,
            }
        }
        Err(err) => BitcoinProbe {
            chain: None,
            blocks: None,
            synced: None,
            rpc_error: Some(err.to_string()),
        },
    }
}

fn is_synced(info: &BlockchainInfo) -> bool {
    !info.initialblockdownload && info.verificationprogress >= 0.999
}
