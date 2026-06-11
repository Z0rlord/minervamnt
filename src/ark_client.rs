//! Ark client abstraction.
//!
//! `ArkClient` is the trait boundary between the mint and the Ark Service
//! Provider (ASP). The real implementation will wrap the `arkade`/`second`
//! client library and talk to a live ASP; the scaffold ships a deterministic
//! in-memory `MockArkClient` so the full mint flow is testable end-to-end.

use crate::error::{MintError, Result};
use crate::types::Vtxo;
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Bitcoin mainnet averages one block per ~600 seconds.
const SECONDS_PER_BLOCK: u64 = 600;

#[async_trait]
pub trait ArkClient: Send + Sync {
    /// Fund the mint: on-board `amount_msat` with the ASP, producing a VTXO.
    async fn board_sats(&self, amount_msat: u64) -> Result<Vtxo>;

    /// Roll a VTXO into a fresh round before it expires.
    async fn refresh_vtxo(&self, vtxo: &Vtxo) -> Result<Vtxo>;

    /// Broadcast the pre-signed branch/leaf transactions to exit to L1.
    /// Returns the txid of the broadcast leaf transaction.
    async fn unilateral_exit(&self, vtxo: &Vtxo) -> Result<String>;

    /// Time remaining until the VTXO expires (based on current block height).
    async fn get_vtxo_expiry(&self, vtxo: &Vtxo) -> Result<Duration>;

    /// Current chain tip height as seen by the ASP / bitcoind.
    async fn current_block_height(&self) -> Result<u64>;

    /// Connectivity check for the health monitor.
    async fn ping(&self) -> Result<()>;
}

/// Deterministic in-memory mock of an ASP.
///
/// IDs and transaction hexes are derived from a SHA-256 counter so tests are
/// reproducible. The mock tracks which VTXOs it issued and rejects operations
/// on unknown or already-exited VTXOs, mimicking real ASP behavior.
pub struct MockArkClient {
    asp_pubkey: String,
    block_height: AtomicU64,
    default_expiry_blocks: u64,
    counter: AtomicU64,
    issued: Mutex<HashMap<String, Vtxo>>,
}

impl MockArkClient {
    pub fn new(default_expiry_blocks: u64) -> Self {
        MockArkClient {
            asp_pubkey: "02deadbeef".to_string() + &"00".repeat(28),
            block_height: AtomicU64::new(850_000),
            default_expiry_blocks,
            counter: AtomicU64::new(0),
            issued: Mutex::new(HashMap::new()),
        }
    }

    /// Advance the mock chain, useful for expiry tests.
    pub fn advance_blocks(&self, n: u64) {
        self.block_height.fetch_add(n, Ordering::SeqCst);
    }

    fn deterministic_hex(&self, label: &str, n: u64) -> String {
        let mut hasher = Sha256::new();
        hasher.update(label.as_bytes());
        hasher.update(n.to_be_bytes());
        hex::encode(hasher.finalize())
    }

    fn make_vtxo(&self, amount_msat: u64) -> Vtxo {
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        let height = self.block_height.load(Ordering::SeqCst);
        Vtxo {
            id: format!("vtxo-{}", self.deterministic_hex("id", n)),
            amount_msat,
            expiry: height + self.default_expiry_blocks,
            branch_tx: self.deterministic_hex("branch", n),
            leaf_tx: self.deterministic_hex("leaf", n),
            asp_pubkey: self.asp_pubkey.clone(),
        }
    }
}

#[async_trait]
impl ArkClient for MockArkClient {
    async fn board_sats(&self, amount_msat: u64) -> Result<Vtxo> {
        if amount_msat == 0 {
            return Err(MintError::Ark("cannot board zero sats".into()));
        }
        let vtxo = self.make_vtxo(amount_msat);
        self.issued
            .lock()
            .unwrap()
            .insert(vtxo.id.clone(), vtxo.clone());
        Ok(vtxo)
    }

    async fn refresh_vtxo(&self, vtxo: &Vtxo) -> Result<Vtxo> {
        let mut issued = self.issued.lock().unwrap();
        if issued.remove(&vtxo.id).is_none() {
            return Err(MintError::Ark(format!("unknown vtxo: {}", vtxo.id)));
        }
        let fresh = self.make_vtxo(vtxo.amount_msat);
        issued.insert(fresh.id.clone(), fresh.clone());
        Ok(fresh)
    }

    async fn unilateral_exit(&self, vtxo: &Vtxo) -> Result<String> {
        let mut issued = self.issued.lock().unwrap();
        if issued.remove(&vtxo.id).is_none() {
            return Err(MintError::Ark(format!("unknown vtxo: {}", vtxo.id)));
        }
        // The "txid" of the broadcast leaf tx is just its hash in the mock.
        let mut hasher = Sha256::new();
        hasher.update(vtxo.leaf_tx.as_bytes());
        Ok(hex::encode(hasher.finalize()))
    }

    async fn get_vtxo_expiry(&self, vtxo: &Vtxo) -> Result<Duration> {
        let height = self.block_height.load(Ordering::SeqCst);
        let blocks_left = vtxo.expiry.saturating_sub(height);
        Ok(Duration::from_secs(blocks_left * SECONDS_PER_BLOCK))
    }

    async fn current_block_height(&self) -> Result<u64> {
        Ok(self.block_height.load(Ordering::SeqCst))
    }

    async fn ping(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn board_refresh_exit_roundtrip() {
        let ark = MockArkClient::new(25920);
        let vtxo = ark.board_sats(1_000_000).await.unwrap();
        assert_eq!(vtxo.amount_msat, 1_000_000);
        assert_eq!(vtxo.expiry, 850_000 + 25920);

        let fresh = ark.refresh_vtxo(&vtxo).await.unwrap();
        assert_ne!(fresh.id, vtxo.id);
        assert_eq!(fresh.amount_msat, vtxo.amount_msat);

        // Old VTXO is consumed by the refresh.
        assert!(ark.refresh_vtxo(&vtxo).await.is_err());

        let txid = ark.unilateral_exit(&fresh).await.unwrap();
        assert_eq!(txid.len(), 64);
        // Exited VTXO cannot be exited again.
        assert!(ark.unilateral_exit(&fresh).await.is_err());
    }

    #[tokio::test]
    async fn expiry_shrinks_as_chain_advances() {
        let ark = MockArkClient::new(144);
        let vtxo = ark.board_sats(500_000).await.unwrap();
        let before = ark.get_vtxo_expiry(&vtxo).await.unwrap();
        ark.advance_blocks(100);
        let after = ark.get_vtxo_expiry(&vtxo).await.unwrap();
        assert!(after < before);
        ark.advance_blocks(1000);
        let expired = ark.get_vtxo_expiry(&vtxo).await.unwrap();
        assert_eq!(expired, Duration::ZERO);
    }

    #[tokio::test]
    async fn boarding_zero_fails() {
        let ark = MockArkClient::new(144);
        assert!(ark.board_sats(0).await.is_err());
    }
}
