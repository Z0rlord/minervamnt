use axum::body::Body;
use axum::http::{Request, StatusCode};
use minerva_mint::api::router;
use minerva_mint::ark_client::MockArkClient;
use minerva_mint::blind_signer::build_blind_signer;
use minerva_mint::mint_backend::MintBackend;
use minerva_mint::pol::PolLedger;
use minerva_mint::spent_store::SpentSecretStore;
use minerva_mint::vtxo_inventory::VtxoInventory;
use minerva_mint::AppConfig;
use std::sync::Arc;
use tower::ServiceExt;

fn test_backend() -> Arc<MintBackend> {
    let config = AppConfig::load("config.toml").expect("config");
    let ark = Arc::new(MockArkClient::new(config.ark.default_vtxo_expiry));
    let signer = build_blind_signer(&config.signatory).expect("signer");
    Arc::new(MintBackend::new(
        config,
        ark,
        signer,
        VtxoInventory::open_in_memory().unwrap(),
        PolLedger::open_in_memory().unwrap(),
        SpentSecretStore::open_in_memory().unwrap(),
        None,
    ))
}

fn test_app() -> axum::Router {
    router(test_backend())
}

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let app = test_app();
    let response = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn info_endpoint_returns_minerva_branding() {
    let app = test_app();
    let response = app
        .oneshot(Request::get("/v1/info").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["name"], "Minerva Mint");
}
