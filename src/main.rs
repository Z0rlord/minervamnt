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
use minerva_mint::receipts::{ReceiptSigner, ReceiptStore};
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
    if std::env::args().nth(1).as_deref() == Some("reconcile") {
        return reconcile_command(&config).await;
    }

    let verify_mode =
        VtxoVerifyMode::parse(&config.trust.vtxo_verify_mode).unwrap_or(VtxoVerifyMode::Scaffold);
    let inventory = VtxoInventory::open_with_mode(&config.database.path, verify_mode)?;
    let pol = PolLedger::open(&config.database.path)?;
    let spent = SpentSecretStore::open(&config.database.path)?;
    let receipts = ReceiptStore::open(&config.database.path)?;
    // The public dev receipt key is only tolerated against the mock ASP;
    // any real backend fails closed on a missing key.
    let receipt_signer =
        ReceiptSigner::from_env_or_dev(&config.receipts.key_env, config.ark.backend == "mock")
            .map_err(|e| anyhow::anyhow!("{e}"))?;

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
        receipts,
        receipt_signer,
        ots,
    ));
    backend.init_keysets().await?;
    // Re-anchor any receipts whose PoL epoch event was lost to a crash
    // between the two writes.
    backend.recover_receipt_anchors()?;
    // Re-finalize settled melt quotes whose backing release or change
    // signing was lost to a crash or operator bookkeeping failure.
    backend.recover_pending_finalize().await?;

    tokio::spawn(run_refresh_scheduler(
        backend.clone(),
        Duration::from_secs(config.scheduler.refresh_interval_secs),
    ));
    tokio::spawn(run_health_monitor(backend.clone(), Duration::from_secs(60)));
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

/// Operator reconciliation for RESERVED melt quotes (payout outcome
/// unknown). Local CLI only - deliberately not exposed over HTTP.
///
///   minerva-mint reconcile list
///   minerva-mint reconcile release <quote_id>
///   minerva-mint reconcile settle <quote_id> <preimage_hex>
///
/// RELEASE returns the reserved proofs to spendable: only after verifying
/// the payout definitively failed or never started, or the mint's backing
/// is spent twice. SETTLE burns the reserved proofs: only for a payout that
/// actually completed, and the preimage is checked against the quote's
/// payment hash. Both go through the same MintBackend paths as the service
/// so receipts, PoL records and NUT-08 change stay consistent; settle signs
/// the persisted blank outputs (no fee measurement exists on this path, so
/// the full reserve returns as change) and the wallet recovers the change
/// from the quote check endpoint.
async fn reconcile_command(config: &AppConfig) -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(2).collect();
    let valid = matches!(args.len(), 1 if args[0] == "list")
        || matches!(args.len(), 2 if args[0] == "release")
        || matches!(args.len(), 3 if args[0] == "settle");
    if !valid {
        eprintln!(
            "usage: minerva-mint reconcile list | release <quote_id> | settle <quote_id> <preimage_hex>"
        );
        std::process::exit(2);
    }
    if let Some(parent) = std::path::PathBuf::from(&config.database.path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let verify_mode =
        VtxoVerifyMode::parse(&config.trust.vtxo_verify_mode).unwrap_or(VtxoVerifyMode::Scaffold);
    let inventory = VtxoInventory::open_with_mode(&config.database.path, verify_mode)?;
    let pol = PolLedger::open(&config.database.path)?;
    let spent = SpentSecretStore::open(&config.database.path)?;
    let receipts = ReceiptStore::open(&config.database.path)?;
    let receipt_signer =
        ReceiptSigner::from_env_or_dev(&config.receipts.key_env, config.ark.backend == "mock")
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    let ark = build_ark_client(&config.ark).map_err(|e| anyhow::anyhow!("{e}"))?;
    let signer = build_blind_signer(&config.signatory).map_err(|e| anyhow::anyhow!("{e}"))?;
    let backend = MintBackend::new(
        config.clone(),
        ark,
        signer,
        inventory,
        pol,
        spent,
        receipts,
        receipt_signer,
        None,
    );
    // Re-anchor orphaned receipts and re-finalize settled melts whose
    // backing release or change signing was lost to a crash, so the
    // operator does not have to repair either by hand. Keysets (needed
    // for change signing) are only loaded when an action requires them.
    backend.recover_receipt_anchors()?;
    if args[0] != "list" {
        backend.init_keysets().await?;
        let recovered = backend.recover_pending_finalize().await?;
        if recovered > 0 {
            eprintln!("re-finalized {recovered} settled melt quote(s)");
        }
    }
    match args.len() {
        1 if args[0] == "list" => {
            let reserved = backend.list_reserved_melts()?;
            if reserved.is_empty() {
                println!("no reserved melt quotes");
            }
            for q in reserved {
                println!(
                    "{} amount={}sat fee_reserve={}sat expiry={} payment_hash={}",
                    q.quote_id,
                    q.amount_sat,
                    q.fee_reserve_sat,
                    q.expiry,
                    q.payment_hash.as_deref().unwrap_or("-")
                );
            }
        }
        2 if args[0] == "release" => {
            eprintln!(
                "WARNING: releasing returns the reserved proofs to spendable. \
                 Only proceed after verifying out-of-band that the payout \
                 definitively failed or never started."
            );
            backend.reconcile_release_reserved_melt(&args[1])?;
            println!("released {}", args[1]);
        }
        3 if args[0] == "settle" => {
            backend
                .reconcile_settle_reserved_melt(&args[1], &args[2])
                .await?;
            println!("settled {}", args[1]);
        }
        _ => unreachable!("usage validated above"),
    }
    Ok(())
}
