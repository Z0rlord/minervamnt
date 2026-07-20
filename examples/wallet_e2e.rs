//! Real Cashu wallet mint → melt e2e against a running Minerva Mint + remote
//! cdk-signatory (valid BDHKE `B_` points — not the mock-signatory bash smoke).
//!
//! Env:
//!   MINT_URL           (default http://127.0.0.1:3338)
//!   SIGNATORY_URL      (default https://localhost:3340)
//!   SIGNATORY_TLS_DIR  (required for mTLS; default data/cdk-signatory-signet)
//!   MELT_INVOICE       bolt11 from a *separate* barkd recv wallet
//!   MINT_AMOUNT_SAT    optional override (default: next power-of-two covering melt)
//!
//! Exit 0 on mint+melt PAID; non-zero on failure. Prints only non-secret status.

use anyhow::{anyhow, bail, Context, Result};
use cdk_common::dhke::{blind_message, construct_proofs};
use cdk_common::nuts::{BlindSignature, Id, PublicKey};
use cdk_common::secret::Secret;
use cdk_common::Amount;
use cdk_signatory::signatory::Signatory;
use cdk_signatory::SignatoryRpcClient;
use serde_json::{json, Value};
use std::str::FromStr;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    let mint_url = std::env::var("MINT_URL").unwrap_or_else(|_| "http://127.0.0.1:3338".into());
    let signatory_url =
        std::env::var("SIGNATORY_URL").unwrap_or_else(|_| "https://localhost:3340".into());
    let tls_dir = std::env::var("SIGNATORY_TLS_DIR")
        .unwrap_or_else(|_| "data/cdk-signatory-signet".into());
    let melt_invoice = std::env::var("MELT_INVOICE").context("MELT_INVOICE required")?;

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(1500))
        .build()?;

    println!("wallet-e2e: mint={mint_url} signatory={signatory_url}");

    // --- health ---
    let health: Value = http
        .get(format!("{mint_url}/health"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let ark_ok = health.get("ark_connected").and_then(|v| v.as_bool()) == Some(true);
    println!(
        "health: status={} ark_connected={ark_ok}",
        health.get("status").and_then(|v| v.as_str()).unwrap_or("?")
    );
    if !ark_ok {
        bail!("mint ark_connected=false");
    }
    println!("PASS health");

    // --- keyset + amount keys from signatory (mint has no /v1/keys yet) ---
    let client = SignatoryRpcClient::new(signatory_url, Some(tls_dir.as_str()))
        .await
        .context("signatory connect")?;
    let ks = Signatory::keysets(&client)
        .await
        .context("signatory keysets")?;
    let active = ks
        .keysets
        .iter()
        .find(|k| k.active)
        .or_else(|| ks.keysets.first())
        .ok_or_else(|| anyhow!("no keysets from signatory"))?;
    let keyset_id = active.id.to_string();
    let keys = active.keys.clone();
    println!(
        "keyset id={keyset_id} amounts={} pubkey={}",
        active.amounts.len(),
        &ks.pubkey.to_string()[..16.min(ks.pubkey.to_string().len())]
    );
    println!("PASS signatory keys");

    // --- melt quote (need cover amount before minting) ---
    let melt_q: Value = http
        .post(format!("{mint_url}/v1/melt/quote/bolt11"))
        .json(&json!({"request": melt_invoice, "unit": "sat"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let melt_quote_id = melt_q["quote"]
        .as_str()
        .ok_or_else(|| anyhow!("melt quote missing id: {melt_q}"))?
        .to_string();
    let melt_amount = melt_q["amount"].as_u64().unwrap_or(0);
    let fee_reserve = melt_q["fee_reserve"].as_u64().unwrap_or(0);
    let cover = melt_amount + fee_reserve;
    println!("melt quote id={melt_quote_id} amount={melt_amount} fee_reserve={fee_reserve} cover={cover}");
    println!("PASS melt quote");

    let mint_amount = match std::env::var("MINT_AMOUNT_SAT") {
        Ok(s) => s.parse::<u64>()?,
        Err(_) => next_pow2_in_keyset(cover, &active.amounts)?,
    };
    if mint_amount < cover {
        bail!("MINT_AMOUNT_SAT={mint_amount} < cover={cover}");
    }
    if keys.amount_key(Amount::from(mint_amount)).is_none() {
        bail!("keyset has no pubkey for amount {mint_amount}");
    }

    // --- blind + mint ---
    let secret = Secret::generate();
    let (b_, r) = blind_message(secret.as_bytes(), None).context("blind_message")?;
    let b_hex = b_.to_string();
    if !b_hex.starts_with("02") && !b_hex.starts_with("03") {
        bail!("B_ is not a compressed pubkey: {}", &b_hex[..8.min(b_hex.len())]);
    }

    let mint_q: Value = http
        .post(format!("{mint_url}/v1/mint/quote/bolt11"))
        .json(&json!({"amount": mint_amount, "unit": "sat"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let mint_quote_id = mint_q["quote"]
        .as_str()
        .ok_or_else(|| anyhow!("mint quote missing id: {mint_q}"))?
        .to_string();
    println!("mint quote id={mint_quote_id} amount={mint_amount}");

    let mint_http = http
        .post(format!("{mint_url}/v1/mint/bolt11"))
        .json(&json!({
            "quote": mint_quote_id,
            "outputs": [{
                "amount": mint_amount,
                "id": keyset_id,
                "B_": b_hex,
            }]
        }))
        .send()
        .await
        .context("POST /v1/mint/bolt11 transport")?;
    let mint_status = mint_http.status();
    let mint_text = mint_http.text().await.unwrap_or_default();
    if !mint_status.is_success() {
        eprintln!("mint HTTP {mint_status}: {mint_text}");
        bail!("POST /v1/mint/bolt11 status {mint_status}");
    }
    let mint_resp: Value = serde_json::from_str(&mint_text).context("parse mint response")?;

    let sigs = mint_resp["signatures"]
        .as_array()
        .ok_or_else(|| anyhow!("mint response missing signatures: {mint_resp}"))?;
    if sigs.len() != 1 {
        bail!("expected 1 signature, got {}: {mint_resp}", sigs.len());
    }
    let c_hex = sigs[0]["C_"]
        .as_str()
        .ok_or_else(|| anyhow!("signature missing C_: {mint_resp}"))?;
    let c_ = PublicKey::from_str(c_hex).context("parse C_")?;
    let promise = BlindSignature {
        amount: Amount::from(mint_amount),
        keyset_id: Id::from_str(&keyset_id)?,
        c: c_,
        dleq: None,
    };
    let proofs = construct_proofs(vec![promise], vec![r], vec![secret.clone()], &keys)
        .context("construct_proofs / unblind")?;
    let proof = &proofs[0];
    println!(
        "minted amount={} C_len={} unblinded_C_prefix={}",
        mint_amount,
        c_hex.len(),
        &proof.c.to_string()[..16.min(proof.c.to_string().len())]
    );
    println!("PASS mint (remote signatory BDHKE)");

    // --- melt pay ---
    let melt_body = json!({
        "quote": melt_quote_id,
        "inputs": [{
            "amount": u64::from(proof.amount),
            "id": proof.keyset_id.to_string(),
            "secret": proof.secret.to_string(),
            "C": proof.c.to_string(),
        }],
        "token_ids": [mint_quote_id],
    });
    println!("melt pay (may take several minutes for Ark LN)…");
    let melt_http = http
        .post(format!("{mint_url}/v1/melt/bolt11"))
        .json(&melt_body)
        .send()
        .await
        .context("POST /v1/melt/bolt11 transport")?;
    let melt_status = melt_http.status();
    let melt_text = melt_http.text().await.unwrap_or_default();
    let melt_resp: Value = serde_json::from_str(&melt_text).unwrap_or_else(|_| json!({"raw": melt_text}));
    if !melt_status.is_success() {
        eprintln!("melt HTTP {melt_status}: {melt_resp}");
        bail!("POST /v1/melt/bolt11 status {melt_status}");
    }

    let state = melt_resp["state"].as_str().unwrap_or("");
    let preimage = melt_resp["payment_preimage"].as_str().unwrap_or("");
    if state == "PAID" && preimage.len() >= 32 {
        println!(
            "PASS melt state=PAID preimage_len={}",
            preimage.len()
        );
        println!("PASS wallet-e2e mint→melt");
        Ok(())
    } else {
        eprintln!("melt response: {melt_resp}");
        bail!("melt did not complete: state={state}");
    }
}

fn next_pow2_in_keyset(cover: u64, amounts: &[u64]) -> Result<u64> {
    let mut p = 1u64;
    while p < cover {
        p = p
            .checked_mul(2)
            .ok_or_else(|| anyhow!("cover {cover} too large"))?;
    }
    if amounts.is_empty() || amounts.contains(&p) {
        return Ok(p);
    }
    amounts
        .iter()
        .copied()
        .filter(|&a| a >= cover)
        .min()
        .ok_or_else(|| anyhow!("no keyset amount >= cover {cover}"))
}
