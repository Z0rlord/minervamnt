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
    if std::env::args().nth(1).as_deref() == Some("reconcile") {
        return reconcile_command(&config);
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
/// payment hash.
fn reconcile_command(config: &AppConfig) -> anyhow::Result<()> {
    use minerva_mint::mint_backend::verify_melt_preimage;

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
    let spent = SpentSecretStore::open(&config.database.path)?;
    match args.len() {
        1 if args[0] == "list" => {
            let reserved = spent.list_reserved_melts()?;
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
            spent.release_melt(&args[1])?;
            println!("released {}", args[1]);
        }
        3 if args[0] == "settle" => {
            let quote = spent
                .melt_quote(&args[1])?
                .ok_or_else(|| anyhow::anyhow!("quote not found: {}", args[1]))?;
            verify_melt_preimage(&args[2], quote.payment_hash.as_deref())
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            // A preimage verifies against the payment HASH, not this quote's
            // payout: refuse to settle a second quote for an invoice another
            // melt already settled.
            if let Some(hash) = &quote.payment_hash {
                if spent.has_settled_melt_for_hash_excluding(hash, &args[1])? {
                    return Err(anyhow::anyhow!(
                        "invoice for quote {} was already settled by another melt; refusing to settle a second quote for the same payment",
                        args[1]
                    ));
                }
            }
            let inputs = spent.reserved_inputs(&args[1])?;
            spent.settle_melt(&args[1], &args[2])?;
            let pol = PolLedger::open(&config.database.path)?;
            for (secret, keyset_id, amount) in &inputs {
                pol.record_burn(keyset_id, secret, *amount)?;
            }
            println!("settled {} ({} inputs burned)", args[1], inputs.len());
        }
        _ => unreachable!("usage validated above"),
    }
    Ok(())
}
