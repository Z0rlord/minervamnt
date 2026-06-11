use axum::body::Body;
use axum::http::{Request, StatusCode};
use minerva_mint::api::{self, build_state};
use minerva_mint::ark_client::MockArkClient;
use minerva_mint::mint_backend::MintBackend;
use minerva_mint::vtxo_inventory::VtxoInventory;
use minerva_mint::AppConfig;
use std::sync::Arc;
use tower::ServiceExt;

fn test_config() -> AppConfig {
    AppConfig::load("config.toml").expect("config")
}

fn test_app() -> axum::Router {
    let config = test_config();
    let ark = Arc::new(MockArkClient::new(
        config.ark.server_pubkey.clone(),
        config.ark.default_vtxo_expiry,
    ));
    let inventory = Arc::new(VtxoInventory::in_memory(10).unwrap());
    let backend = Arc::new(MintBackend::new(
        ark,
        inventory,
        config.liquidity.clone(),
    ));
    api::router(build_state(config, backend))
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
