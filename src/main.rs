use minerva_mint::api::{self, build_state};
use minerva_mint::ark_client::MockArkClient;
use minerva_mint::mint_backend::MintBackend;
use minerva_mint::scheduler::spawn_refresh_scheduler;
use minerva_mint::vtxo_inventory::VtxoInventory;
use minerva_mint::AppConfig;
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config_path = std::env::var("MINERVA_CONFIG").unwrap_or_else(|_| "config.toml".to_string());
    let config = AppConfig::load(&config_path)?;

    let ark = Arc::new(MockArkClient::new(
        config.ark.server_pubkey.clone(),
        config.ark.default_vtxo_expiry,
    ));
    let inventory = Arc::new(VtxoInventory::new(
        &config.database.path,
        config.ark.refresh_threshold_blocks,
    )?);
    let backend = Arc::new(MintBackend::new(
        ark.clone(),
        inventory.clone(),
        config.liquidity.clone(),
    ));

    spawn_refresh_scheduler(
        ark,
        inventory,
        config.scheduler.refresh_interval_secs,
    );

    let state = build_state(config.clone(), backend);
    let app = api::router(state);

    let addr = format!("{}:{}", config.server.host, config.server.port);
    info!(%addr, mint = %config.mint.name, url = %config.mint.url, "starting Minerva Mint");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
