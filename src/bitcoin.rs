use crate::config::BitcoinConfig;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, Deserialize)]
pub struct BlockchainInfo {
    pub chain: String,
    pub blocks: u64,
    pub headers: u64,
    pub verificationprogress: f64,
    pub initialblockdownload: bool,
}

#[derive(Debug, Error)]
pub enum BitcoinRpcError {
    #[error("bitcoin RPC credentials not configured")]
    MissingCredentials,
    #[error("bitcoin RPC request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("bitcoin RPC error: {0}")]
    Rpc(String),
}

/// Ping Bitcoin Core `getblockchaininfo` using credentials from config/env.
pub async fn get_blockchain_info(config: &BitcoinConfig) -> Result<BlockchainInfo, BitcoinRpcError> {
    let user = config
        .rpc_user
        .as_deref()
        .ok_or(BitcoinRpcError::MissingCredentials)?;
    let password = config
        .rpc_password
        .as_deref()
        .ok_or(BitcoinRpcError::MissingCredentials)?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let body = serde_json::json!({
        "jsonrpc": "1.0",
        "id": "health",
        "method": "getblockchaininfo",
        "params": []
    });

    let response = client
        .post(&config.rpc_url)
        .basic_auth(user, Some(password))
        .json(&body)
        .send()
        .await?;

    let payload: serde_json::Value = response.json().await?;
    if let Some(err) = payload.get("error").and_then(|e| e.as_object()) {
        return Err(BitcoinRpcError::Rpc(
            err.get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown RPC error")
                .to_string(),
        ));
    }

    serde_json::from_value(
        payload
            .get("result")
            .cloned()
            .ok_or_else(|| BitcoinRpcError::Rpc("missing result".into()))?,
    )
    .map_err(|e| BitcoinRpcError::Rpc(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_credentials_returns_error() {
        let config = BitcoinConfig {
            rpc_url: "http://127.0.0.1:8332".into(),
            rpc_user: None,
            rpc_password: None,
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(get_blockchain_info(&config)).unwrap_err();
        assert!(matches!(err, BitcoinRpcError::MissingCredentials));
    }
}
