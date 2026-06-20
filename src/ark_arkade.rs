//! Arkade ASP integration via `ark-rest` + optional wallet daemon HTTP.
//!
//! Connects to a public Arkade server (e.g. Mutinynet) for health/info and uses
//! a **barkd-compatible wallet daemon** at `ark.wallet_url` for board/refresh/exit.
//! Run `arkd` locally and expose its wallet REST, or use a bridge sidecar.

use crate::ark_client::ArkClient;
use crate::ark_wallet_http::WalletHttpClient;
use crate::config::ArkConfig;
use crate::error::{MintError, Result};
use crate::types::{ExitResult, Vtxo};
use ark_rest::apis::ark_service_api::ark_service_get_info;
use ark_rest::apis::configuration::Configuration;
use async_trait::async_trait;
use std::time::Duration;

pub struct ArkadeArkClient {
    server_url: String,
    wallet: Option<WalletHttpClient>,
}

impl ArkadeArkClient {
    pub fn new(config: &ArkConfig, auth_token: Option<&str>) -> Result<Self> {
        let wallet = match &config.wallet_url {
            Some(url) if !url.is_empty() => Some(WalletHttpClient::new(
                url,
                auth_token,
                config.poll_timeout_secs,
                config.poll_interval_secs,
                config.exit_claim_address.clone(),
                config.auto_claim_exits,
            )?),
            _ => None,
        };
        Ok(ArkadeArkClient {
            server_url: config.server_url.clone(),
            wallet,
        })
    }

    fn ark_config(&self) -> Configuration {
        let mut cfg = Configuration::new();
        cfg.base_path = self.server_url.clone();
        cfg.client = reqwest::Client::new();
        cfg
    }

    fn require_wallet(&self) -> Result<&WalletHttpClient> {
        self.wallet.as_ref().ok_or_else(|| {
            MintError::Ark(
                "ark.wallet_url required for board/refresh/exit on Arkade backend".into(),
            )
        })
    }
}

#[async_trait]
impl ArkClient for ArkadeArkClient {
    async fn board_sats(&self, amount_msat: u64) -> Result<Vtxo> {
        self.require_wallet()?.board_sats(amount_msat).await
    }

    async fn refresh_vtxo(&self, vtxo: &Vtxo) -> Result<Vtxo> {
        self.require_wallet()?.refresh_vtxo(vtxo).await
    }

    async fn unilateral_exit(&self, vtxo: &Vtxo) -> Result<ExitResult> {
        self.require_wallet()?.unilateral_exit(vtxo).await
    }

    async fn get_vtxo_expiry(&self, vtxo: &Vtxo) -> Result<Duration> {
        let height = self.current_block_height().await?;
        let blocks_left = vtxo.expiry.saturating_sub(height);
        Ok(Duration::from_secs(blocks_left * 600))
    }

    async fn current_block_height(&self) -> Result<u64> {
        if let Some(w) = &self.wallet {
            return w.current_block_height().await;
        }
        // Fallback: no wallet daemon — return 0 (health still works via get_info).
        Ok(0)
    }

    async fn ping(&self) -> Result<()> {
        ark_service_get_info(&self.ark_config())
            .await
            .map_err(|e| MintError::Ark(format!("arkade get_info: {e}")))?;
        if let Some(w) = &self.wallet {
            if !w.wallet_connected().await? {
                return Err(MintError::Ark(
                    "arkade wallet daemon not connected to ASP".into(),
                ));
            }
        }
        Ok(())
    }

    async fn estimate_lightning_send_fee_sat(&self, amount_sat: u64) -> Result<u64> {
        self.require_wallet()?
            .estimate_lightning_send_fee_sat(amount_sat)
            .await
    }

    async fn pay_lightning_invoice(&self, invoice: &str, amount_sat: u64) -> Result<String> {
        self.require_wallet()?
            .pay_lightning_invoice(invoice, amount_sat)
            .await
    }
}
