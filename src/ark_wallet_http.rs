//! Shared HTTP helpers for wallet daemons (barkd, Arkade wallet sidecar).
//!
//! Both Second barkd and an Arkade operator wallet can expose a compatible REST
//! surface for board / refresh / exit when configured via `wallet_url`.

use crate::error::{MintError, Result};
use crate::types::{ExitResult, Vtxo};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const API_PREFIX: &str = "/api/v1";

#[derive(Clone)]
pub struct WalletHttpClient {
    http: reqwest::Client,
    base_url: String,
    poll_timeout: Duration,
    poll_interval: Duration,
    exit_claim_address: Option<String>,
    auto_claim: bool,
}

impl WalletHttpClient {
    pub fn new(
        base_url: impl Into<String>,
        auth_token: Option<&str>,
        poll_timeout_secs: u64,
        poll_interval_secs: u64,
        exit_claim_address: Option<String>,
        auto_claim: bool,
    ) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("minerva-mint/0.1");
        if let Some(token) = auth_token.filter(|t| !t.is_empty()) {
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {token}")
                    .parse()
                    .map_err(|e| MintError::Ark(format!("invalid wallet auth token: {e}")))?,
            );
            builder = builder.default_headers(headers);
        }
        let http = builder
            .build()
            .map_err(|e| MintError::Ark(format!("wallet HTTP client: {e}")))?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            poll_timeout: Duration::from_secs(poll_timeout_secs),
            poll_interval: Duration::from_secs(poll_interval_secs),
            exit_claim_address,
            auto_claim,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self
            .http
            .get(self.url(path))
            .send()
            .await
            .map_err(|e| MintError::Ark(format!("wallet GET {path}: {e}")))?;
        Self::decode(resp, path).await
    }

    async fn post_json<T: serde::de::DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let resp = self
            .http
            .post(self.url(path))
            .json(body)
            .send()
            .await
            .map_err(|e| MintError::Ark(format!("wallet POST {path}: {e}")))?;
        Self::decode(resp, path).await
    }

    async fn decode<T: serde::de::DeserializeOwned>(
        resp: reqwest::Response,
        path: &str,
    ) -> Result<T> {
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| MintError::Ark(format!("wallet read {path}: {e}")))?;
        if !status.is_success() {
            return Err(MintError::Ark(format!(
                "wallet {path} returned {status}: {text}"
            )));
        }
        serde_json::from_str(&text)
            .map_err(|e| MintError::Ark(format!("wallet decode {path}: {e}: {text}")))
    }

    pub async fn board_sats(&self, amount_msat: u64) -> Result<Vtxo> {
        if amount_msat == 0 {
            return Err(MintError::Ark("cannot board zero sats".into()));
        }
        let min_sat = msat_to_sat(amount_msat);
        if let Some(vtxo) = self.find_spendable_vtxo(min_sat).await? {
            return Ok(vtxo);
        }
        let board: PendingBoardInfo = self
            .post_json(
                &format!("{API_PREFIX}/boards/board-amount"),
                &BoardRequest { amount_sat: min_sat },
            )
            .await?;
        let vtxo_id = board
            .vtxos
            .first()
            .ok_or_else(|| MintError::Ark("board returned no VTXO ids".into()))?;
        self.wait_until_spendable(vtxo_id).await
    }

    pub async fn refresh_vtxo(&self, vtxo: &Vtxo) -> Result<Vtxo> {
        let amount_sat = msat_to_sat(vtxo.amount_msat);
        let _: PendingRoundInfo = self
            .post_json(
                &format!("{API_PREFIX}/wallet/refresh/vtxos"),
                &RefreshRequest {
                    vtxos: vec![vtxo.id.clone()],
                },
            )
            .await?;
        self.wait_for_refresh_output(&vtxo.id, amount_sat).await
    }

    pub async fn unilateral_exit(&self, vtxo: &Vtxo) -> Result<ExitResult> {
        let _: ExitStartResponse = self
            .post_json(
                &format!("{API_PREFIX}/exits/start/vtxos"),
                &ExitStartRequest {
                    vtxos: vec![vtxo.id.clone()],
                },
            )
            .await?;

        let deadline = tokio::time::Instant::now() + self.poll_timeout;
        let mut phase = String::new();
        let mut claim_txid = None;

        loop {
            let _: serde_json::Value = self
                .post_json(
                    &format!("{API_PREFIX}/exits/progress"),
                    &serde_json::json!({}),
                )
                .await?;

            let status: ExitTransactionStatus = self
                .get_json(&format!("{API_PREFIX}/exits/status/{}", vtxo.id))
                .await?;

            phase = exit_phase(&status.state);
            if phase == "claimable" && self.auto_claim {
                if let Some(dest) = &self.exit_claim_address {
                    let claim: ExitClaimResponse = self
                        .post_json(
                            &format!("{API_PREFIX}/exits/claim/vtxos"),
                            &ExitClaimVtxosRequest {
                                destination: dest.clone(),
                                vtxos: vec![vtxo.id.clone()],
                                fee_rate: None,
                            },
                        )
                        .await?;
                    claim_txid = claim.txid;
                    phase = "claimed".into();
                    break;
                }
            }
            if phase == "claimed" {
                claim_txid = extract_claim_txid(&status);
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(ExitResult {
                    exit_txid: vtxo.id.clone(),
                    phase,
                    claim_txid,
                });
            }
            if phase == "claimable" && !self.auto_claim {
                break;
            }
            tokio::time::sleep(self.poll_interval).await;
        }

        Ok(ExitResult {
            exit_txid: claim_txid.clone().unwrap_or_else(|| vtxo.id.clone()),
            phase,
            claim_txid,
        })
    }

    pub async fn current_block_height(&self) -> Result<u64> {
        let tip: BlockTip = self.get_json(&format!("{API_PREFIX}/bitcoin/tip")).await?;
        Ok(tip.height)
    }

    pub async fn wallet_connected(&self) -> Result<bool> {
        let connected: ConnectedResponse =
            self.get_json(&format!("{API_PREFIX}/wallet/connected")).await?;
        Ok(connected.connected)
    }

    pub async fn estimate_lightning_send_fee_sat(&self, amount_sat: u64) -> Result<u64> {
        let estimate: FeeEstimateResponse = self
            .get_json(&format!(
                "{API_PREFIX}/fees/lightning/pay?amount_sat={amount_sat}"
            ))
            .await?;
        Ok(estimate.fee_sat)
    }

    /// Pay a BOLT11 invoice via barkd and return the payment preimage (hex).
    pub async fn pay_lightning_invoice(&self, destination: &str, amount_sat: u64) -> Result<String> {
        if destination.trim().is_empty() {
            return Err(MintError::Ark("empty lightning destination".into()));
        }
        let body = LightningPayRequest {
            destination: destination.to_string(),
            amount_sat: None,
        };
        let resp: serde_json::Value = self
            .post_json(&format!("{API_PREFIX}/lightning/pay"), &body)
            .await?;
        if let Some(preimage) = extract_preimage(&resp) {
            return Ok(preimage);
        }
        self.poll_lightning_preimage(destination, amount_sat).await
    }

    async fn poll_lightning_preimage(&self, destination: &str, amount_sat: u64) -> Result<String> {
        let deadline = tokio::time::Instant::now() + self.poll_timeout;
        loop {
            let _: Result<serde_json::Value> = self
                .post_json(
                    &format!("{API_PREFIX}/wallet/sync"),
                    &serde_json::json!({}),
                )
                .await;

            let movements: Vec<serde_json::Value> =
                self.get_json(&format!("{API_PREFIX}/history")).await?;
            for movement in &movements {
                if movement.get("status").and_then(|s| s.as_str()) != Some("successful") {
                    continue;
                }
                if !movement_matches_destination(movement, destination) {
                    continue;
                }
                if let Some(preimage) = extract_preimage_from_movement(movement) {
                    return Ok(preimage);
                }
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(MintError::Ark(format!(
                    "timed out waiting for lightning payment preimage ({amount_sat} sat to {destination})"
                )));
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    async fn fetch_vtxo(&self, id: &str) -> Result<WalletVtxoInfo> {
        self.get_json(&format!("{API_PREFIX}/wallet/vtxos/{id}"))
            .await
    }

    async fn fetch_encoded(&self, id: &str) -> Result<Option<String>> {
        let resp: EncodedVtxoResponse = self
            .get_json(&format!("{API_PREFIX}/wallet/vtxos/{id}/encoded"))
            .await?;
        Ok(Some(resp.encoded))
    }

    async fn map_vtxo(&self, info: &WalletVtxoInfo) -> Result<Vtxo> {
        let encoded = self.fetch_encoded(&info.id).await.ok().flatten();
        Ok(wallet_vtxo_to_domain(info, encoded))
    }

    async fn wait_until_spendable(&self, id: &str) -> Result<Vtxo> {
        let deadline = tokio::time::Instant::now() + self.poll_timeout;
        loop {
            let info = self.fetch_vtxo(id).await?;
            if vtxo_is_spendable(&info.state) {
                return self.map_vtxo(&info).await;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(MintError::Ark(format!(
                    "timed out waiting for VTXO {id} to become spendable"
                )));
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    async fn find_spendable_vtxo(&self, min_sat: u64) -> Result<Option<Vtxo>> {
        let vtxos: Vec<WalletVtxoInfo> =
            self.get_json(&format!("{API_PREFIX}/wallet/vtxos")).await?;
        let mut best: Option<&WalletVtxoInfo> = None;
        for v in &vtxos {
            if !vtxo_is_spendable(&v.state) || v.amount_sat < min_sat {
                continue;
            }
            best = match best {
                None => Some(v),
                Some(cur) if v.amount_sat < cur.amount_sat => Some(v),
                other => other,
            };
        }
        match best {
            Some(info) => Ok(Some(self.map_vtxo(info).await?)),
            None => Ok(None),
        }
    }

    async fn wait_for_refresh_output(&self, old_id: &str, amount_sat: u64) -> Result<Vtxo> {
        let deadline = tokio::time::Instant::now() + self.poll_timeout;
        loop {
            let rounds: Vec<PendingRoundInfo> =
                self.get_json(&format!("{API_PREFIX}/wallet/rounds")).await?;
            for round in &rounds {
                if round_status_confirmed(&round.status) {
                    if let Some(out) = round_output_for_input(&round.participation, old_id) {
                        return self.wait_until_spendable(&out).await;
                    }
                }
            }
            let vtxos: Vec<WalletVtxoInfo> =
                self.get_json(&format!("{API_PREFIX}/wallet/vtxos")).await?;
            for v in &vtxos {
                if v.id != old_id && vtxo_is_spendable(&v.state) && v.amount_sat == amount_sat {
                    return self.map_vtxo(v).await;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(MintError::Ark(format!(
                    "timed out waiting for refresh of VTXO {old_id}"
                )));
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }
}

pub fn msat_to_sat(amount_msat: u64) -> u64 {
    amount_msat.div_ceil(1000)
}

pub fn wallet_vtxo_to_domain(info: &WalletVtxoInfo, encoded: Option<String>) -> Vtxo {
    Vtxo {
        id: info.id.clone(),
        amount_msat: info.amount_sat.saturating_mul(1000),
        expiry: info.expiry_height,
        branch_tx: info.chain_anchor.clone(),
        leaf_tx: encoded
            .clone()
            .unwrap_or_else(|| info.chain_anchor.clone()),
        asp_pubkey: info.server_pubkey.clone(),
        vpack_hex: encoded,
    }
}

fn vtxo_is_spendable(state: &serde_json::Value) -> bool {
    state.get("type").and_then(|t| t.as_str()) == Some("spendable")
}

fn round_status_confirmed(status: &serde_json::Value) -> bool {
    status.get("status").and_then(|s| s.as_str()) == Some("confirmed")
}

fn round_output_for_input(participation: &RoundParticipationInfo, input_id: &str) -> Option<String> {
    if participation.inputs.iter().any(|i| i == input_id) {
        return participation.outputs.first().cloned();
    }
    None
}

fn exit_phase(state: &serde_json::Value) -> String {
    state
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("unknown")
        .to_string()
}

fn extract_claim_txid(status: &ExitTransactionStatus) -> Option<String> {
    status
        .claim_txid
        .clone()
        .or_else(|| status.txid.clone())
}

fn extract_preimage(value: &serde_json::Value) -> Option<String> {
    for key in ["preimage", "payment_preimage", "paymentPreimage"] {
        if let Some(s) = value.get(key).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn extract_preimage_from_movement(movement: &serde_json::Value) -> Option<String> {
    if let Some(preimage) = extract_preimage(movement) {
        return Some(preimage);
    }
    movement
        .get("metadata")
        .and_then(|m| extract_preimage(m))
}

fn movement_matches_destination(movement: &serde_json::Value, destination: &str) -> bool {
    let dest_norm = destination.trim();
    if let Some(sent_to) = movement.get("sent_to").and_then(|v| v.as_array()) {
        for entry in sent_to {
            if entry
                .get("destination")
                .and_then(|d| d.as_str())
                .is_some_and(|d| d.trim() == dest_norm)
            {
                return true;
            }
            if entry
                .as_str()
                .is_some_and(|d| d.trim() == dest_norm)
            {
                return true;
            }
        }
    }
    false
}

#[derive(Debug, Serialize)]
struct LightningPayRequest {
    destination: String,
    amount_sat: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct FeeEstimateResponse {
    fee_sat: u64,
}

#[derive(Debug, Deserialize)]
struct ConnectedResponse {
    connected: bool,
}

#[derive(Debug, Deserialize)]
struct BlockTip {
    height: u64,
}

#[derive(Debug, Serialize)]
struct BoardRequest {
    amount_sat: u64,
}

#[derive(Debug, Deserialize)]
struct PendingBoardInfo {
    #[allow(dead_code)]
    amount_sat: u64,
    movement_id: i32,
    vtxos: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RefreshRequest {
    vtxos: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PendingRoundInfo {
    status: serde_json::Value,
    participation: RoundParticipationInfo,
}

#[derive(Debug, Deserialize)]
struct RoundParticipationInfo {
    inputs: Vec<String>,
    outputs: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ExitStartRequest {
    vtxos: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ExitStartResponse {
    #[allow(dead_code)]
    message: String,
}

#[derive(Debug, Serialize)]
struct ExitClaimVtxosRequest {
    destination: String,
    vtxos: Vec<String>,
    fee_rate: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ExitClaimResponse {
    txid: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExitTransactionStatus {
    state: serde_json::Value,
    txid: Option<String>,
    claim_txid: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EncodedVtxoResponse {
    encoded: String,
}

#[derive(Debug, Deserialize)]
pub struct WalletVtxoInfo {
    pub id: String,
    pub amount_sat: u64,
    pub expiry_height: u64,
    pub chain_anchor: String,
    pub server_pubkey: String,
    pub state: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msat_rounds_up_to_sat() {
        assert_eq!(msat_to_sat(1), 1);
        assert_eq!(msat_to_sat(1000), 1);
        assert_eq!(msat_to_sat(1001), 2);
    }
}
