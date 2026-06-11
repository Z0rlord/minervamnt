use crate::ark_client::ArkClient;
use crate::vtxo_inventory::VtxoInventory;
use chrono::{Duration, Utc};
use std::sync::Arc;
use tokio::time::{interval, Duration as TokioDuration};
use tracing::{error, info, warn};

pub fn spawn_refresh_scheduler<C: ArkClient + 'static>(
    ark: Arc<C>,
    inventory: Arc<VtxoInventory>,
    refresh_interval_secs: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = interval(TokioDuration::from_secs(refresh_interval_secs));
        loop {
            ticker.tick().await;
            if let Err(err) = run_refresh_cycle(ark.clone(), inventory.clone()).await {
                error!(%err, "refresh scheduler cycle failed");
            }
        }
    })
}

async fn run_refresh_cycle<C: ArkClient>(
    ark: Arc<C>,
    inventory: Arc<VtxoInventory>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let queue = inventory.get_refresh_queue()?;
    if queue.is_empty() {
        return Ok(());
    }

    info!(count = queue.len(), "processing VTXO refresh queue");
    for item in queue {
        if let Err(err) = inventory.mark_refreshing(&item.id) {
            warn!(vtxo_id = %item.id, %err, "failed to mark vtxo refreshing");
            continue;
        }

        let vtxo = crate::ark_client::Vtxo {
            id: item.id.clone(),
            amount_msat: item.amount_msat,
            expiry_height: 0,
            branch_tx_hex: item.branch_tx_hex.clone(),
            leaf_tx_hex: item.leaf_tx_hex.clone(),
            asp_pubkey: "scheduler".to_string(),
        };

        match ark.refresh_vtxo(&vtxo).await {
            Ok(refreshed) => {
                let expires_at = Utc::now() + Duration::days(30);
                inventory.update_vtxo_after_refresh(&item.id, &refreshed, expires_at)?;
                info!(old = %item.id, new = %refreshed.id, "refreshed vtxo");
            }
            Err(err) => {
                error!(vtxo_id = %item.id, %err, "operator alert: vtxo refresh failed");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ark_client::MockArkClient;
    use chrono::Utc;

    #[tokio::test]
    async fn scheduler_refreshes_expiring_vtxos() {
        let ark = Arc::new(MockArkClient::new("02abc", 800_000));
        let inventory = Arc::new(VtxoInventory::in_memory(60).unwrap());
        let vtxo = ark.make_vtxo(100_000);
        inventory
            .insert_vtxo(&vtxo, Utc::now() + chrono::Duration::minutes(1))
            .unwrap();

        run_refresh_cycle(ark, inventory.clone()).await.unwrap();
        let queue = inventory.get_refresh_queue().unwrap();
        assert!(queue.is_empty() || queue[0].id != vtxo.id);
    }
}
