use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tracing_subscriber::EnvFilter;

use minerva_mint::api::router;
use minerva_mint::ark_client::build_ark_client;
use minerva_mint::blind_signer::build_blind_signer;
use minerva_mint::mint_backend::MintBackend;
use minerva_mint::ots::HttpOtsStamper;
use minerva_mint::pol::PolLedger;
use minerva_mint::spent_store::SpentSecretStore;
use minerva_mint::tasks::{
    run_health_monitor, run_ots_upgrade_worker, run_pol_epoch_worker, run_refresh_scheduler,
};
use minerva_mint::vtxo_inventory::VtxoInventory;
use minerva_mint::vtxo_verify::VtxoVerifyMode;
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
    let verify_mode = VtxoVerifyMode::parse(&config.trust.vtxo_verify_mode)
        .unwrap_or(VtxoVerifyMode::Scaffold);
    let inventory = VtxoInventory::open_with_mode(&config.database.path, verify_mode)?;
    let pol = PolLedger::open(&config.database.path)?;
    let spent = SpentSecretStore::open(&config.database.path)?;

    let ots: Option<Arc<dyn minerva_mint::ots::OtsStamper>> = if config.trust.ots.enabled {
        match HttpOtsStamper::new(config.trust.ots.calendar_urls.clone()) {
            Ok(s) => Some(Arc::new(s)),
            Err(e) => {
                tracing::error!(error = %e, "OTS stamper init failed; continuing without OTS");
                None
            }
        }
    } else {
        None
    };

    let ark = build_ark_client(&config.ark).map_err(|e| anyhow::anyhow!("{e}"))?;
    let signer = build_blind_signer(&config.signatory).map_err(|e| anyhow::anyhow!("{e}"))?;

    let backend = Arc::new(MintBackend::new(
        config.clone(),
        ark,
        signer,
        inventory,
        pol,
        spent,
        ots,
    ));
    backend.init_keysets().await?;

    tokio::spawn(run_refresh_scheduler(
        backend.clone(),
        Duration::from_secs(config.scheduler.refresh_interval_secs),
    ));
    tokio::spawn(run_health_monitor(
        backend.clone(),
        Duration::from_secs(60),
    ));
    if config.trust.pol_enabled {
        tokio::spawn(run_pol_epoch_worker(
            backend.clone(),
            Duration::from_secs(300),
        ));
    }
    if config.trust.ots.enabled {
        tokio::spawn(run_ots_upgrade_worker(
            backend.clone(),
            Duration::from_secs(config.trust.ots.upgrade_interval_secs),
        ));
    }

    let app = router(backend);
    let addr = config.bind_addr();
    tracing::info!(%addr, url = %config.mint.url, "minerva-mint listening");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
