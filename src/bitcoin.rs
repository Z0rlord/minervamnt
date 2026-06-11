use crate::config::BitcoinConfig;
use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct BlockchainInfo {
    pub chain: String,
    pub blocks: u64,
    pub headers: u64,
    pub verificationprogress: f64,
    pub initialblockdownload: bool,
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    result: Option<BlockchainInfo>,
    error: Option<serde_json::Value>,
}

/// Ping Bitcoin Core `getblockchaininfo`. Credentials come from config/env only — never hardcoded.
pub async fn get_blockchain_info(config: &BitcoinConfig) -> Result<BlockchainInfo> {
    let user = config
        .rpc_user
        .as_deref()
        .context("BITCOIN_RPC_USER not set")?;
    let password = config
        .rpc_password
        .as_deref()
        .context("BITCOIN_RPC_PASSWORD not set")?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let body = serde_json::json!({
        "jsonrpc": "1.0",
        "id": "minerva-health",
        "method": "getblockchaininfo",
        "params": []
    });

    let response = client
        .post(&config.rpc_url)
        .basic_auth(user, Some(password))
        .json(&body)
        .send()
        .await
        .with_context(|| format!("bitcoin RPC request to {}", config.rpc_url))?;

    let rpc: RpcResponse = response
        .error_for_status()
        .context("bitcoin RPC HTTP error")?
        .json()
        .await
        .context("parsing bitcoin RPC response")?;

    if let Some(err) = rpc.error {
        anyhow::bail!("bitcoin RPC error: {err}");
    }

    rpc.result.context("bitcoin RPC returned no result")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blockchain_info_deserializes() {
        let json = r#"{
            "chain": "main",
            "blocks": 800000,
            "headers": 800000,
            "verificationprogress": 0.99,
            "initialblockdownload": false
        }"#;
        let info: BlockchainInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.chain, "main");
        assert!(!info.initialblockdownload);
    }
}
