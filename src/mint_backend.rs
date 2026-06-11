use crate::ark_client::{ArkClient, ArkError, Vtxo};
use crate::config::LiquidityConfig;
use crate::vtxo_inventory::{InventoryError, VtxoInventory};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum MintError {
    #[error("inventory error: {0}")]
    Inventory(#[from] InventoryError),
    #[error("ark error: {0}")]
    Ark(#[from] ArkError),
    #[error("insufficient reserve")]
    InsufficientReserve,
    #[error("quote not found: {0}")]
    QuoteNotFound(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MintQuote {
    pub quote_id: String,
    pub amount_msat: u64,
    pub request: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeltQuote {
    pub quote_id: String,
    pub amount_msat: u64,
    pub request: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssuedToken {
    pub token_id: String,
    pub amount_msat: u64,
    pub proofs: Vec<String>,
}

pub struct MintBackend<C: ArkClient> {
    ark: Arc<C>,
    inventory: Arc<VtxoInventory>,
    liquidity: LiquidityConfig,
    mint_quotes: std::sync::RwLock<Vec<MintQuote>>,
    melt_quotes: std::sync::RwLock<Vec<MeltQuote>>,
}

impl<C: ArkClient> MintBackend<C> {
    pub fn new(ark: Arc<C>, inventory: Arc<VtxoInventory>, liquidity: LiquidityConfig) -> Self {
        Self {
            ark,
            inventory,
            liquidity,
            mint_quotes: std::sync::RwLock::new(Vec::new()),
            melt_quotes: std::sync::RwLock::new(Vec::new()),
        }
    }

    pub async fn create_mint_quote(&self, amount_msat: u64, request: String) -> MintQuote {
        let quote = MintQuote {
            quote_id: Uuid::new_v4().to_string(),
            amount_msat,
            request,
        };
        self.mint_quotes.write().expect("mint quotes lock").push(quote.clone());
        quote
    }

    pub fn get_mint_quote(&self, quote_id: &str) -> Option<MintQuote> {
        let guard = self.mint_quotes.read().ok()?;
        guard.iter().find(|q| q.quote_id == quote_id).cloned()
    }

    pub async fn mint_tokens(&self, quote_id: &str) -> Result<IssuedToken, MintError> {
        let quote = self
            .get_mint_quote(quote_id)
            .ok_or_else(|| MintError::QuoteNotFound(quote_id.to_string()))?;

        self.ensure_liquidity(quote.amount_msat).await?;

        let vtxo = self
            .inventory
            .allocate_vtxo_for_tokens(quote.amount_msat)?;

        let token_id = Uuid::new_v4().to_string();
        let expires_at = Utc::now() + Duration::days(30);
        self.inventory
            .map_token_to_vtxo(&token_id, &vtxo.id, quote.amount_msat, expires_at)?;

        Ok(IssuedToken {
            token_id,
            amount_msat: quote.amount_msat,
            proofs: vec![format!("proof-for-{}", quote.quote_id)],
        })
    }

    pub async fn create_melt_quote(&self, amount_msat: u64, request: String) -> MeltQuote {
        let quote = MeltQuote {
            quote_id: Uuid::new_v4().to_string(),
            amount_msat,
            request,
        };
        self.melt_quotes.write().expect("melt quotes lock").push(quote.clone());
        quote
    }

    pub fn get_melt_quote(&self, quote_id: &str) -> Option<MeltQuote> {
        let guard = self.melt_quotes.read().ok()?;
        guard.iter().find(|q| q.quote_id == quote_id).cloned()
    }

    pub async fn melt_tokens(&self, quote_id: &str, token_id: &str) -> Result<String, MintError> {
        let quote = self
            .get_melt_quote(quote_id)
            .ok_or_else(|| MintError::QuoteNotFound(quote_id.to_string()))?;

        self.inventory.release_vtxo_mapping(token_id)?;
        Ok(format!("melt-paid-{}", quote.amount_msat))
    }

    pub async fn swap_tokens(&self, amount_msat: u64) -> Result<IssuedToken, MintError> {
        Ok(IssuedToken {
            token_id: Uuid::new_v4().to_string(),
            amount_msat,
            proofs: vec![format!("swapped-{amount_msat}")],
        })
    }

    pub async fn get_vtxo_proof(&self, token_id: &str) -> Result<Vtxo, MintError> {
        let inv = self.inventory.get_vtxo_for_token(token_id)?;
        Ok(Vtxo {
            id: inv.id,
            amount_msat: inv.amount_msat,
            expiry_height: 0,
            branch_tx_hex: inv.branch_tx_hex,
            leaf_tx_hex: inv.leaf_tx_hex,
            asp_pubkey: "from-inventory".to_string(),
        })
    }

    pub async fn initiate_exit(&self, token_id: &str) -> Result<String, MintError> {
        let vtxo = self.get_vtxo_proof(token_id).await?;
        let txid = self.ark.unilateral_exit(&vtxo).await?;
        self.inventory.release_vtxo_mapping(token_id)?;
        Ok(txid)
    }

    pub fn refresh_status(&self) -> Result<Vec<String>, MintError> {
        Ok(self
            .inventory
            .get_refresh_queue()?
            .into_iter()
            .map(|v| v.id)
            .collect())
    }

    pub fn active_reserve_msat(&self) -> Result<u64, MintError> {
        Ok(self.inventory.active_reserve_msat()?)
    }

    async fn ensure_liquidity(&self, amount_msat: u64) -> Result<(), MintError> {
        if self.inventory.allocate_vtxo_for_tokens(amount_msat).is_ok() {
            return Ok(());
        }

        let reserve = self.inventory.active_reserve_msat()?;
        let target = self.liquidity.min_vtxo_reserve_msat.max(1);
        let reserve_ratio = reserve as f64 / target as f64;
        if reserve_ratio >= self.liquidity.auto_board_threshold
            && self.inventory.allocate_vtxo_for_tokens(amount_msat).is_err()
        {
            return Err(MintError::InsufficientReserve);
        }

        let board_amount = amount_msat
            .max(target / 10)
            .min(self.liquidity.max_single_vtxo_msat);
        let vtxo = self.ark.board_sats(board_amount).await?;
        let expires_at = Utc::now() + Duration::days(30);
        self.inventory.insert_vtxo(&vtxo, expires_at)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ark_client::MockArkClient;
    use crate::config::LiquidityConfig;

    fn test_liquidity() -> LiquidityConfig {
        LiquidityConfig {
            min_vtxo_reserve_msat: 1_000_000,
            max_single_vtxo_msat: 500_000,
            auto_board_threshold: 0.5,
        }
    }

    #[tokio::test]
    async fn mint_and_melt_flow() {
        let ark = Arc::new(MockArkClient::new("02abc", 800_000));
        let inventory = Arc::new(VtxoInventory::in_memory(10).unwrap());
        let backend = MintBackend::new(ark.clone(), inventory.clone(), test_liquidity());

        let quote = backend.create_mint_quote(25_000, "lnbc...".into()).await;
        let issued = backend.mint_tokens(&quote.quote_id).await.unwrap();
        assert_eq!(issued.amount_msat, 25_000);

        let melt_quote = backend
            .create_melt_quote(25_000, "lnbc1...".into())
            .await;
        let payment = backend
            .melt_tokens(&melt_quote.quote_id, &issued.token_id)
            .await
            .unwrap();
        assert!(payment.contains("25000"));
    }
}
