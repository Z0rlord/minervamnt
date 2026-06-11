//! End-to-end tests: full mint -> swap -> melt -> exit flows over HTTP via
//! the axum router, backed by the mock ASP and an in-memory SQLite inventory.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use minerva_mint::api::router;
use minerva_mint::ark_client::MockArkClient;
use minerva_mint::mint_backend::MintBackend;
use minerva_mint::pol::PolLedger;
use minerva_mint::vtxo_inventory::VtxoInventory;
use minerva_mint::AppConfig;
use minerva_mint::KEYSET_ID;

fn test_backend() -> Arc<MintBackend> {
    let config: AppConfig = toml::from_str(include_str!("../config.toml")).expect("config parses");
    let ark = Arc::new(MockArkClient::new(config.ark.default_vtxo_expiry));
    Arc::new(MintBackend::new(
        config,
        ark,
        VtxoInventory::open_in_memory().unwrap(),
        PolLedger::open_in_memory().unwrap(),
        None,
    ))
}

async fn request(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder().method(method).uri(uri);
    let req = match body {
        Some(v) => builder
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

#[tokio::test]
async fn info_and_health() {
    let app = router(test_backend());

    let (status, body) = request(&app, "GET", "/v1/info", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "Minerva Mint");
    assert_eq!(body["nuts"]["4"]["methods"][0]["method"], "bolt11");

    let (status, body) = request(&app, "GET", "/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["ark_connected"], true);
    assert!(body["block_height"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn full_mint_swap_melt_flow_over_http() {
    let app = router(test_backend());

    // 1. Mint quote for 64 sat (mock ASP settles instantly -> PAID).
    let (status, quote) = request(
        &app,
        "POST",
        "/v1/mint/quote/bolt11",
        Some(json!({ "amount": 64, "unit": "sat" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(quote["state"], "PAID");
    let quote_id = quote["quote"].as_str().unwrap().to_string();

    // 2. Mint: one 64-sat blinded output (amounts must be powers of two).
    let outputs = json!([{ "amount": 64, "id": KEYSET_ID, "B_": "02blinded-a" }]);
    let (status, minted) = request(
        &app,
        "POST",
        "/v1/mint/bolt11",
        Some(json!({ "quote": quote_id, "outputs": outputs })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sigs = minted["signatures"].as_array().unwrap();
    assert_eq!(sigs.len(), 1);
    assert_eq!(sigs[0]["amount"], 64);

    // Re-minting the same quote must fail.
    let (status, _) = request(
        &app,
        "POST",
        "/v1/mint/bolt11",
        Some(json!({ "quote": quote_id, "outputs": outputs })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 3. The issuance batch is mapped to a backing VTXO.
    let (status, mapped) = request(&app, "GET", &format!("/ark/vtxo/{quote_id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(mapped["status"], "active");
    assert!(mapped["vtxo"]["amount_msat"].as_u64().unwrap() >= 64_000);

    // 4. Swap the 64-sat proof into two 32-sat tokens.
    let proof = json!({
        "amount": 64, "id": KEYSET_ID,
        "secret": "secret-a", "C": sigs[0]["C_"],
    });
    let swap_outputs = json!([
        { "amount": 32, "id": KEYSET_ID, "B_": "02blinded-b" },
        { "amount": 32, "id": KEYSET_ID, "B_": "02blinded-c" },
    ]);
    let (status, swapped) = request(
        &app,
        "POST",
        "/v1/swap",
        Some(json!({ "inputs": [proof], "outputs": swap_outputs })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(swapped["signatures"].as_array().unwrap().len(), 2);

    // Replaying the spent proof must fail.
    let (status, body) = request(
        &app,
        "POST",
        "/v1/swap",
        Some(json!({ "inputs": [proof], "outputs": swap_outputs })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["detail"].as_str().unwrap().contains("spent"));

    // 5. Melt. The scaffold derives the amount from the request string:
    //    "lnbc1" -> 5 chars * 100 = 500 sat, fee reserve 5 sat.
    let (status, melt_quote) = request(
        &app,
        "POST",
        "/v1/melt/quote/bolt11",
        Some(json!({ "request": "lnbc1", "unit": "sat" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(melt_quote["state"], "UNPAID");
    assert_eq!(melt_quote["amount"], 500);
    assert_eq!(melt_quote["fee_reserve"], 5);
    let melt_quote_id = melt_quote["quote"].as_str().unwrap().to_string();

    // Mint a 512-sat token to cover 500 + 5.
    let (_, q2) = request(
        &app,
        "POST",
        "/v1/mint/quote/bolt11",
        Some(json!({ "amount": 512, "unit": "sat" })),
    )
    .await;
    let q2_id = q2["quote"].as_str().unwrap().to_string();
    let (status, minted2) = request(
        &app,
        "POST",
        "/v1/mint/bolt11",
        Some(json!({
            "quote": q2_id,
            "outputs": [{ "amount": 512, "id": KEYSET_ID, "B_": "02blinded-d" }],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sig512 = &minted2["signatures"][0];

    let (status, melted) = request(
        &app,
        "POST",
        "/v1/melt/bolt11",
        Some(json!({
            "quote": melt_quote_id,
            "inputs": [{
                "amount": 512, "id": KEYSET_ID,
                "secret": "secret-512", "C": sig512["C_"],
            }],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(melted["state"], "PAID");
    assert_eq!(melted["payment_preimage"].as_str().unwrap().len(), 64);

    // 6. Refresh status endpoint responds with scheduler info.
    let (status, refresh) = request(&app, "GET", "/ark/refresh/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refresh["refresh_threshold_blocks"], 144);
    assert!(refresh["current_block_height"].as_u64().unwrap() > 0);
    assert!(refresh.get("pending_refreshes").is_some());

    // 7. Transparency summary reflects issuance + backing (64 sat left after melt).
    let (status, summary) = request(&app, "GET", "/transparency/summary", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(summary["outstanding_ecash_sat"], 64);
    assert!(summary["active_vtxo_msat"].as_u64().unwrap() > 0);
    assert_eq!(summary["solvency_ok"], true);
    assert_eq!(summary["signatory_policy_enforced"], true);

    let (status, pol) = request(&app, "GET", "/v1/pol/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(pol["outstanding_sat"].as_u64().unwrap(), 64);
}

#[tokio::test]
async fn unilateral_exit_over_http() {
    let app = router(test_backend());

    let (_, quote) = request(
        &app,
        "POST",
        "/v1/mint/quote/bolt11",
        Some(json!({ "amount": 32, "unit": "sat" })),
    )
    .await;
    let quote_id = quote["quote"].as_str().unwrap().to_string();
    let (status, _) = request(
        &app,
        "POST",
        "/v1/mint/bolt11",
        Some(json!({
            "quote": quote_id,
            "outputs": [{ "amount": 32, "id": KEYSET_ID, "B_": "02blinded-x" }],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, exit) = request(
        &app,
        "POST",
        "/ark/exit",
        Some(json!({ "token_id": quote_id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(exit["token_id"], quote_id.as_str());
    assert_eq!(exit["txid"].as_str().unwrap().len(), 64);

    // Mapping is released after the exit.
    let (status, _) = request(&app, "GET", &format!("/ark/vtxo/{quote_id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A second exit for the same token fails.
    let (status, _) = request(
        &app,
        "POST",
        "/ark/exit",
        Some(json!({ "token_id": quote_id })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn invalid_requests_are_rejected() {
    let app = router(test_backend());

    // Unsupported unit.
    let (status, _) = request(
        &app,
        "POST",
        "/v1/mint/quote/bolt11",
        Some(json!({ "amount": 4, "unit": "usd" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Non-power-of-two output amount.
    let (_, quote) = request(
        &app,
        "POST",
        "/v1/mint/quote/bolt11",
        Some(json!({ "amount": 3, "unit": "sat" })),
    )
    .await;
    let (status, body) = request(
        &app,
        "POST",
        "/v1/mint/bolt11",
        Some(json!({
            "quote": quote["quote"],
            "outputs": [{ "amount": 3, "id": KEYSET_ID, "B_": "02blinded-z" }],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["detail"].as_str().unwrap().contains("power of two"));

    // Unknown mint quote.
    let (status, _) = request(
        &app,
        "POST",
        "/v1/mint/bolt11",
        Some(json!({
            "quote": "nonexistent",
            "outputs": [{ "amount": 2, "id": KEYSET_ID, "B_": "02blinded-w" }],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
