use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Vtxo {
    pub id: String,
    pub amount_msat: u64,
    pub expiry_height: u64,
    pub branch_tx_hex: String,
    pub leaf_tx_hex: String,
    pub asp_pubkey: String,
}

#[derive(Debug, Error)]
pub enum ArkError {
    #[error("insufficient liquidity")]
    InsufficientLiquidity,
    #[error("vtxo not found: {0}")]
    NotFound(String),
    #[error("refresh failed: {0}")]
    RefreshFailed(String),
    #[error("exit failed: {0}")]
    ExitFailed(String),
}

#[async_trait]
pub trait ArkClient: Send + Sync {
    async fn board_sats(&self, amount_msat: u64) -> Result<Vtxo, ArkError>;
    async fn refresh_vtxo(&self, vtxo: &Vtxo) -> Result<Vtxo, ArkError>;
    async fn unilateral_exit(&self, vtxo: &Vtxo) -> Result<String, ArkError>;
    async fn get_vtxo_expiry(&self, vtxo: &Vtxo) -> Result<Duration, ArkError>;
}

/// Deterministic in-memory Ark client for local development and tests.
#[derive(Debug, Default)]
pub struct MockArkClient {
    asp_pubkey: String,
    default_expiry_height: u64,
}

impl MockArkClient {
    pub fn new(asp_pubkey: impl Into<String>, default_expiry_height: u64) -> Self {
        Self {
            asp_pubkey: asp_pubkey.into(),
            default_expiry_height,
        }
    }

    pub fn make_vtxo(&self, amount_msat: u64) -> Vtxo {
        let id = Uuid::new_v4().to_string();
        Vtxo {
            id: id.clone(),
            amount_msat,
            expiry_height: self.default_expiry_height,
            branch_tx_hex: format!("branch-{id}"),
            leaf_tx_hex: format!("leaf-{id}"),
            asp_pubkey: self.asp_pubkey.clone(),
        }
    }
}

#[async_trait]
impl ArkClient for MockArkClient {
    async fn board_sats(&self, amount_msat: u64) -> Result<Vtxo, ArkError> {
        if amount_msat == 0 {
            return Err(ArkError::InsufficientLiquidity);
        }
        Ok(self.make_vtxo(amount_msat))
    }

    async fn refresh_vtxo(&self, vtxo: &Vtxo) -> Result<Vtxo, ArkError> {
        let mut refreshed = self.make_vtxo(vtxo.amount_msat);
        refreshed.expiry_height = vtxo.expiry_height.saturating_add(144);
        refreshed.branch_tx_hex = format!("{}-refreshed", vtxo.branch_tx_hex);
        refreshed.leaf_tx_hex = format!("{}-refreshed", vtxo.leaf_tx_hex);
        Ok(refreshed)
    }

    async fn unilateral_exit(&self, vtxo: &Vtxo) -> Result<String, ArkError> {
        Ok(format!("txid-exit-{}", vtxo.id))
    }

    async fn get_vtxo_expiry(&self, _vtxo: &Vtxo) -> Result<Duration, ArkError> {
        let now = Utc::now();
        let expiry: DateTime<Utc> = now + chrono::Duration::days(30);
        Ok(expiry.signed_duration_since(now).to_std().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_board_and_refresh() {
        let client = MockArkClient::new("02abc", 800_000);
        let vtxo = client.board_sats(50_000).await.unwrap();
        assert_eq!(vtxo.amount_msat, 50_000);

        let refreshed = client.refresh_vtxo(&vtxo).await.unwrap();
        assert!(refreshed.expiry_height > vtxo.expiry_height);
        assert!(refreshed.branch_tx_hex.contains("refreshed"));
    }
}
