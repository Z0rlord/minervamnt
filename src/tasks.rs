//! Background tasks: VTXO refresh scheduler, health monitor, PoL epoch closure.

use crate::mint_backend::MintBackend;
use crate::pol::PolLedger;
use std::sync::Arc;
use std::time::Duration;

/// Periodically scan for VTXOs nearing expiry and roll them into fresh
/// rounds with the ASP. Failures are logged at ERROR level — in production
/// this is where the operator alert hook (webhook / ntfy / pagerduty) goes.
pub async fn run_refresh_scheduler(backend: Arc<MintBackend>, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        if let Err(e) = refresh_pass(&backend).await {
            tracing::error!(error = %e, "OPERATOR ALERT: refresh scheduler pass failed");
        }
    }
}

/// One pass of the refresh loop. Extracted for testability.
pub async fn refresh_pass(backend: &MintBackend) -> anyhow::Result<usize> {
    let height = backend.ark().current_block_height().await?;
    let threshold = backend.config().ark.refresh_threshold_blocks;
    let queue = backend.inventory().get_refresh_queue(height, threshold)?;
    let total = queue.len();

    for record in queue {
        let id = record.vtxo.id.clone();
        backend
            .inventory()
            .set_status(&id, crate::types::VtxoStatus::Refreshing)?;
        match backend.ark().refresh_vtxo(&record.vtxo).await {
            Ok(fresh) => {
                backend.inventory().replace_refreshed(&id, &fresh)?;
                tracing::info!(old = %id, new = %fresh.id, "refreshed VTXO");
            }
            Err(e) => {
                // Put it back in the queue for the next pass.
                backend
                    .inventory()
                    .set_status(&id, crate::types::VtxoStatus::Active)?;
                tracing::error!(vtxo = %id, error = %e, "OPERATOR ALERT: VTXO refresh failed");
            }
        }
    }
    Ok(total)
}

/// Periodic health monitor: ASP connectivity and VTXO expiry horizon.
pub async fn run_health_monitor(backend: Arc<MintBackend>, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;

        if let Err(e) = backend.ark().ping().await {
            tracing::error!(error = %e, "OPERATOR ALERT: Ark server unreachable");
            continue;
        }

        match (
            backend.ark().current_block_height().await,
            backend.inventory().next_expiry_height(),
        ) {
            (Ok(height), Ok(Some(next_expiry))) => {
                let horizon = next_expiry.saturating_sub(height);
                if horizon < backend.config().ark.refresh_threshold_blocks {
                    tracing::warn!(
                        blocks_to_expiry = horizon,
                        "VTXO expiry horizon inside refresh threshold"
                    );
                }
            }
            (Ok(_), Ok(None)) => {}
            (h, n) => {
                tracing::error!(?h, ?n, "health monitor query failed");
            }
        }
    }
}

/// Close the previous UTC epoch day on a fixed interval.
///
/// In production this runs shortly after midnight UTC; the scaffold uses a
/// configurable interval and closes the prior day bucket when the day changes.
pub async fn run_pol_epoch_worker(backend: Arc<MintBackend>, interval: Duration) {
    let mut last_day = PolLedger::current_epoch_day();
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let day = PolLedger::current_epoch_day();
        if day != last_day {
            match backend.pol().close_epoch(&last_day) {
                Ok(Some(_)) => {
                    tracing::info!(epoch = %last_day, "PoL epoch closed");
                    if let Err(e) = backend.stamp_epoch_ots(&last_day).await {
                        tracing::error!(epoch = %last_day, error = %e, "PoL OTS stamp failed");
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::error!(epoch = %last_day, error = %e, "PoL epoch close failed"),
            }
            last_day = day;
        }
    }
}

/// Retry OpenTimestamps stamping for any closed epochs missing proofs.
pub async fn run_ots_upgrade_worker(backend: Arc<MintBackend>, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        match backend.stamp_pending_ots_epochs().await {
            Ok(n) if n > 0 => tracing::info!(count = n, "stamped pending PoL epochs with OTS"),
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "OTS upgrade pass failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ark_client::{ArkClient, MockArkClient};
    use crate::blind_signer::build_blind_signer;
    use crate::config::AppConfig;
    use crate::pol::PolLedger;
    use crate::vtxo_inventory::VtxoInventory;

    fn backend_with_ark(ark: Arc<MockArkClient>) -> MintBackend {
        let raw = include_str!("../config.toml");
        let config: AppConfig = toml::from_str(raw).unwrap();
        let signer = build_blind_signer(&config.signatory).unwrap();
        let inventory = VtxoInventory::open_in_memory().unwrap();
        let pol = PolLedger::open_in_memory().unwrap();
        MintBackend::new(config, ark, signer, inventory, pol, None)
    }

    #[tokio::test]
    async fn refresh_pass_rolls_expiring_vtxos() {
        let ark = Arc::new(MockArkClient::new(25920));
        let b = backend_with_ark(ark.clone());

        // Board a VTXO and push the chain to within the refresh threshold.
        let vtxo = ark.board_sats(1_000_000).await.unwrap();
        b.inventory().insert_vtxo(&vtxo).unwrap();
        ark.advance_blocks(25920 - 100); // 100 blocks to expiry < 144 threshold

        let refreshed = refresh_pass(&b).await.unwrap();
        assert_eq!(refreshed, 1);

        // Old VTXO is spent; a fresh active one with a later expiry exists.
        let old = b.inventory().get_vtxo(&vtxo.id).unwrap().unwrap();
        assert_eq!(old.status, crate::types::VtxoStatus::Spent);
        assert!(b.inventory().next_expiry_height().unwrap().unwrap() > vtxo.expiry);

        // Nothing left to refresh on the next pass.
        let refreshed = refresh_pass(&b).await.unwrap();
        assert_eq!(refreshed, 0);
    }

    #[tokio::test]
    async fn refresh_pass_noop_when_far_from_expiry() {
        let ark = Arc::new(MockArkClient::new(25920));
        let b = backend_with_ark(ark.clone());
        let vtxo = ark.board_sats(1_000_000).await.unwrap();
        b.inventory().insert_vtxo(&vtxo).unwrap();

        let refreshed = refresh_pass(&b).await.unwrap();
        assert_eq!(refreshed, 0);
    }
}
