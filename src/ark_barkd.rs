//! Live Ark integration via [barkd](https://second.tech/docs/barkd) (Second / Bark).

use crate::ark_client::ArkClient;
use crate::ark_wallet_http::WalletHttpClient;
use crate::config::ArkConfig;
use crate::error::{MintError, Result};
use crate::types::{ExitResult, Vtxo};
use async_trait::async_trait;
use std::time::Duration;

pub struct BarkdArkClient {
    wallet: WalletHttpClient,
}

impl BarkdArkClient {
    pub fn new(config: &ArkConfig, auth_token: Option<&str>) -> Result<Self> {
        Ok(BarkdArkClient {
            wallet: WalletHttpClient::new(
                &config.barkd_url,
                auth_token,
                config.poll_timeout_secs,
                config.poll_interval_secs,
                config.exit_claim_address.clone(),
                config.auto_claim_exits,
            )?,
        })
    }
}

#[async_trait]
impl ArkClient for BarkdArkClient {
    async fn board_sats(&self, amount_msat: u64) -> Result<Vtxo> {
        self.wallet.board_sats(amount_msat).await
    }

    async fn refresh_vtxo(&self, vtxo: &Vtxo) -> Result<Vtxo> {
        self.wallet.refresh_vtxo(vtxo).await
    }

    async fn unilateral_exit(&self, vtxo: &Vtxo) -> Result<ExitResult> {
        self.wallet.unilateral_exit(vtxo).await
    }

    async fn get_vtxo_expiry(&self, vtxo: &Vtxo) -> Result<Duration> {
        let height = self.current_block_height().await?;
        let blocks_left = vtxo.expiry.saturating_sub(height);
        Ok(Duration::from_secs(blocks_left * 600))
    }

    async fn current_block_height(&self) -> Result<u64> {
        self.wallet.current_block_height().await
    }

    async fn ping(&self) -> Result<()> {
        if self.wallet.wallet_connected().await? {
            Ok(())
        } else {
            Err(MintError::Ark(
                "barkd wallet not connected to Ark ASP".into(),
            ))
        }
    }

    async fn estimate_lightning_send_fee_sat(&self, amount_sat: u64) -> Result<u64> {
        self.wallet.estimate_lightning_send_fee_sat(amount_sat).await
    }

    async fn pay_lightning_invoice(&self, invoice: &str, amount_sat: u64) -> Result<String> {
        self.wallet.pay_lightning_invoice(invoice, amount_sat).await
    }
}
