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
async fn keys_endpoint_returns_active_keyset_keys() {
    let app = test_app();
    let response = app
        .oneshot(Request::get("/v1/keys").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let keysets = json["keysets"].as_array().unwrap();
    assert!(!keysets.is_empty());
    let keyset = &keysets[0];
    assert_eq!(keyset["id"], minerva_mint::mint_backend::KEYSET_ID);
    assert_eq!(keyset["unit"], "sat");
    // NUT-01 wire format: amounts are JSON string keys mapping to pubkey hex.
    let keys = keyset["keys"].as_object().unwrap();
    assert!(keys.contains_key("1"));
    assert!(keys["1"].as_str().unwrap().len() == 66);
}

#[tokio::test]
async fn keys_by_id_endpoint_returns_keyset_and_404_for_unknown() {
    let app = test_app();
    let path = format!("/v1/keys/{}", minerva_mint::mint_backend::KEYSET_ID);
    let response = app
        .clone()
        .oneshot(Request::get(&path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["keysets"].as_array().unwrap().len(), 1);
    assert_eq!(
        json["keysets"][0]["id"],
        minerva_mint::mint_backend::KEYSET_ID
    );

    let response = app
        .oneshot(
            Request::get("/v1/keys/00deadbeef000000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
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
