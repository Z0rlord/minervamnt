use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tracing_subscriber::EnvFilter;

use minerva_mint::api::router;
use minerva_mint::ark_client::MockArkClient;
use minerva_mint::mint_backend::MintBackend;
use minerva_mint::tasks::{run_health_monitor, run_refresh_scheduler};
use minerva_mint::vtxo_inventory::VtxoInventory;
use minerva_mint::AppConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config_path = std::env::var("MINERVA_CONFIG").unwrap_or_else(|_| "config.toml".to_string());
    let config = AppConfig::load(&config_path)?;
    tracing::info!(config = %config_path, mint = %config.mint.name, "configuration loaded");

    if let Some(parent) = PathBuf::from(&config.database.path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let inventory = VtxoInventory::open(&config.database.path)?;

    tracing::warn!("using MOCK Ark client — no real ASP integration yet");
    let ark = Arc::new(MockArkClient::new(config.ark.default_vtxo_expiry));

    let backend = Arc::new(MintBackend::new(config.clone(), ark, inventory));

    tokio::spawn(run_refresh_scheduler(
        backend.clone(),
        Duration::from_secs(config.scheduler.refresh_interval_secs),
    ));
    tokio::spawn(run_health_monitor(
        backend.clone(),
        Duration::from_secs(60),
    ));

    let app = router(backend);
    let addr = config.bind_addr();
    tracing::info!(%addr, url = %config.mint.url, "minerva-mint listening");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
