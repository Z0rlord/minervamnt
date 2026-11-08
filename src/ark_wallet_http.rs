//! Shared HTTP helpers for wallet daemons (barkd, Arkade wallet sidecar).
//!
//! Both Second barkd and an Arkade operator wallet can expose a compatible REST
//! surface for board / refresh / exit when configured via `wallet_url`.

use crate::ark_client::PayoutResult;
use crate::bolt11_util::decode_invoice;
use crate::error::{MintError, Result};
use crate::types::{ExitResult, Vtxo};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
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

    /// Pay a BOLT11 invoice via barkd and return the payout result.
    ///
    /// Outcome classification for the mint's reserve/settle/release flow:
    ///
    /// - `Ok(PayoutResult)`: the payment succeeded. The preimage is verified
    ///   against the invoice's payment hash before being returned. The barkd
    ///   wallet API does not report the fee actually paid on a completed
    ///   payment (only a pre-payment estimate), so `fee_paid_sat` is `None`
    ///   and the mint charges no fee, returning the whole reserve as NUT-08
    ///   change: an unmeasured fee is never charged to the client.
    /// - [`MintError::PaymentFailed`]: the wallet's status endpoint reported
    ///   the current attempt's terminal "failed" state (with no earlier
    ///   success signal), and the mint has no record of a prior attempt on
    ///   this payment hash. This is the only definitive-failure signal; the
    ///   mint auto-releases the reservation on it.
    /// - Any other error: outcome UNKNOWN. Wallet history is not
    ///   attempt-scoped - a re-quoted invoice has the same payment hash, and
    ///   a previous attempt's rows can flip state after dispatch - so
    ///   history alone never settles and never fails a payment. The mint
    ///   holds the reservation for manual reconciliation on this path.
    pub async fn pay_lightning_invoice(
        &self,
        destination: &str,
        amount_sat: u64,
        attempted_before: bool,
    ) -> Result<PayoutResult> {
        let preimage = self
            .pay_lightning_invoice_preimage(destination, amount_sat, attempted_before)
            .await?;
        Ok(PayoutResult {
            preimage,
            fee_paid_sat: None,
        })
    }

    async fn pay_lightning_invoice_preimage(
        &self,
        destination: &str,
        amount_sat: u64,
        attempted_before: bool,
    ) -> Result<String> {
        if destination.trim().is_empty() {
            return Err(MintError::Ark("empty lightning destination".into()));
        }
        let (_, payment_hash) = decode_invoice(destination)?;
        // Snapshot history rows for this payment hash BEFORE dispatch so a
        // previous attempt's movements are never read as this attempt's
        // outcome. If the snapshot fails, history cannot be scoped to this
        // attempt and history-based success detection is disabled: the
        // outcome then rests on the status endpoint alone.
        let prior_movements = self.history_fingerprints(&payment_hash).await.ok();
        let body = LightningPayRequest {
            destination: destination.to_string(),
            amount_sat: None,
        };
        let resp: serde_json::Value = self
            .post_json(&format!("{API_PREFIX}/lightning/pay"), &body)
            .await?;
        if let Some(preimage) = extract_preimage(&resp) {
            return self.verified_preimage(preimage, &payment_hash);
        }
        match self
            .poll_lightning_pay_status(&payment_hash, prior_movements.as_ref(), attempted_before)
            .await
        {
            Ok(preimage) => Ok(preimage),
            // A definitive failure from the status endpoint is final; the
            // unscoped history fallback must never second-guess it.
            // Definitive failure is final, and an incoherent wallet answer
            // is an answer: neither is second-guessed by the history
            // fallback below.
            Err(e @ (MintError::PaymentFailed(_) | MintError::PayoutUnknown(_))) => Err(e),
            Err(e) => {
                tracing::debug!(
                    payment_hash = %payment_hash,
                    "pay status poll unavailable, falling back to history: {e}"
                );
                self.poll_lightning_preimage(
                    destination,
                    amount_sat,
                    &payment_hash,
                    prior_movements.as_ref(),
                )
                .await
            }
        }
    }

    /// Accept a preimage only if it hashes to the invoice's payment hash. A
    /// mismatch means the wallet answered about a different payment; the
    /// outcome is then unknown, not success.
    fn verified_preimage(&self, preimage: String, payment_hash: &str) -> Result<String> {
        let bytes = hex::decode(&preimage).map_err(|_| {
            MintError::PayoutUnknown(format!(
                "wallet returned malformed preimage for {payment_hash}"
            ))
        })?;
        if !hex::encode(Sha256::digest(&bytes)).eq_ignore_ascii_case(payment_hash) {
            return Err(MintError::PayoutUnknown(format!(
                "wallet returned preimage that does not match payment hash {payment_hash}"
            )));
        }
        Ok(preimage)
    }

    /// Serialized history rows mentioning this payment hash, captured before
    /// dispatch. Rows are matched by exact serialization; any row present
    /// here belongs to a previous attempt and is ignored when classifying
    /// this one.
    async fn history_fingerprints(&self, payment_hash: &str) -> Result<HashSet<String>> {
        let movements: Vec<serde_json::Value> =
            self.get_json(&format!("{API_PREFIX}/history")).await?;
        let mut out = HashSet::new();
        for movement in &movements {
            let md_hash = movement
                .get("metadata")
                .and_then(|m| m.get("payment_hash"))
                .and_then(|h| h.as_str());
            if md_hash.is_some_and(|h| payment_hashes_match(h, payment_hash)) {
                out.insert(movement.to_string());
            }
        }
        Ok(out)
    }

    async fn poll_lightning_pay_status(
        &self,
        payment_hash: &str,
        prior_movements: Option<&HashSet<String>>,
        attempted_before: bool,
    ) -> Result<String> {
        let deadline = tokio::time::Instant::now() + self.poll_timeout;
        let mut status_api_available = true;
        let mut seen_success = false;
        loop {
            let _: Result<serde_json::Value> = self
                .post_json(
                    &format!("{API_PREFIX}/wallet/sync"),
                    &serde_json::json!({}),
                )
                .await;

            if status_api_available {
                match self
                    .get_json::<LightningPayStatus>(&format!(
                        "{API_PREFIX}/lightning/pay/status/{payment_hash}"
                    ))
                    .await
                {
                    Ok(status) => {
                        if let Some(preimage) = status.preimage.filter(|p| !p.is_empty()) {
                            return self.verified_preimage(preimage, payment_hash);
                        }
                        if lightning_payment_succeeded(&status.state) {
                            seen_success = true;
                            if let Ok(preimage) = self
                                .poll_history_for_preimage(payment_hash, None, prior_movements)
                                .await
                            {
                                return Ok(preimage);
                            }
                        }
                        if status.state == "failed" {
                            // A failure report after a success signal is a
                            // contradiction, not a definitive failure: the
                            // outcome is unknown and must be reconciled by an
                            // operator.
                            if seen_success {
                                return Err(MintError::PayoutUnknown(format!(
                                    "lightning payment {payment_hash} reported failed after success"
                                )));
                            }
                            // The status endpoint is keyed by payment hash and
                            // we cannot verify when it resets on re-pay of a
                            // previously-failed hash: if this hash was
                            // attempted before (or the pre-dispatch snapshot
                            // is unavailable or incomplete), a "failed" may
                            // belong to the PREVIOUS attempt while this one
                            // is in flight. Only a first-ever attempt's
                            // "failed" is a definitive failure; anything else
                            // holds for operator reconciliation. Prior
                            // attempts are detected BOTH from the wallet
                            // snapshot (non-empty pre-dispatch history) and
                            // from the mint's own quote records
                            // (attempted_before), so a paginated, pruned or
                            // lagging wallet history alone cannot ungate a
                            // stale "failed".
                            if attempted_before
                                || prior_movements.map(|p| !p.is_empty()).unwrap_or(true)
                            {
                                return Err(MintError::PayoutUnknown(format!(
                                    "lightning payment {payment_hash} reported failed, but this payment hash was attempted before (or history is unreadable); outcome unknown"
                                )));
                            }
                            return Err(MintError::PaymentFailed(format!(
                                "lightning payment {payment_hash} failed"
                            )));
                        }
                    }
                    Err(MintError::Ark(msg)) if msg.contains("path not round") => {
                        status_api_available = false;
                    }
                    Err(e) => return Err(e),
                }
            }

            if let Ok(preimage) = self
                .poll_history_for_preimage(payment_hash, None, prior_movements)
                .await
            {
                return Ok(preimage);
            }
            // History rows are not attempt-scoped, so a history "failed" is
            // never definitive: log it and keep waiting for a scoped signal.
            if let Ok(PaymentOutcome::Failed) = self
                .poll_payment_outcome(payment_hash, None, prior_movements)
                .await
            {
                tracing::warn!(
                    payment_hash = %payment_hash,
                    "wallet history reports a failed movement for this payment hash; history is not attempt-scoped, treating outcome as unknown"
                );
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(MintError::Ark(format!(
                    "timed out waiting for lightning payment status ({payment_hash})"
                )));
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    async fn poll_history_for_preimage(
        &self,
        payment_hash: &str,
        destination: Option<&str>,
        prior_movements: Option<&HashSet<String>>,
    ) -> Result<String> {
        match self
            .poll_payment_outcome(payment_hash, destination, prior_movements)
            .await?
        {
            PaymentOutcome::Succeeded(preimage) => self.verified_preimage(preimage, payment_hash),
            PaymentOutcome::Failed | PaymentOutcome::Pending => {
                Err(MintError::Ark("no confirmed payment in history".into()))
            }
        }
    }

    async fn poll_payment_outcome(
        &self,
        payment_hash: &str,
        destination: Option<&str>,
        prior_movements: Option<&HashSet<String>>,
    ) -> Result<PaymentOutcome> {
        // Without a pre-dispatch snapshot, history cannot be scoped to this
        // attempt; report no outcome rather than guess.
        let prior_movements = match prior_movements {
            Some(p) => p,
            None => return Ok(PaymentOutcome::Pending),
        };
        let movements: Vec<serde_json::Value> =
            self.get_json(&format!("{API_PREFIX}/history")).await?;
        for movement in movements.iter().rev() {
            let md_hash = movement
                .get("metadata")
                .and_then(|m| m.get("payment_hash"))
                .and_then(|h| h.as_str());
            let hash_match = md_hash.is_some_and(|h| payment_hashes_match(h, payment_hash));
            let dest_match = destination
                .map(|dest| movement_matches_destination(movement, dest))
                .unwrap_or(false);
            if !hash_match && !dest_match {
                continue;
            }
            // A row that existed before this attempt's dispatch belongs to a
            // previous attempt for the same invoice.
            if hash_match && prior_movements.contains(&movement.to_string()) {
                continue;
            }
            match movement.get("status").and_then(|s| s.as_str()) {
                Some("failed") => return Ok(PaymentOutcome::Failed),
                Some("successful") => {
                    if let Some(preimage) = extract_preimage_from_movement(movement) {
                        return Ok(PaymentOutcome::Succeeded(preimage));
                    }
                }
                _ => {}
            }
        }
        Ok(PaymentOutcome::Pending)
    }

    async fn poll_lightning_preimage(
        &self,
        destination: &str,
        amount_sat: u64,
        payment_hash: &str,
        prior_movements: Option<&HashSet<String>>,
    ) -> Result<String> {
        let deadline = tokio::time::Instant::now() + self.poll_timeout;
        loop {
            let _: Result<serde_json::Value> = self
                .post_json(
                    &format!("{API_PREFIX}/wallet/sync"),
                    &serde_json::json!({}),
                )
                .await;

            match self
                .poll_payment_outcome(payment_hash, Some(destination), prior_movements)
                .await?
            {
                PaymentOutcome::Succeeded(preimage) => {
                    return self.verified_preimage(preimage, payment_hash)
                }
                // History "failed" is not attempt-scoped: unknown, never a
                // definitive failure. Keep waiting for a scoped signal.
                PaymentOutcome::Failed => {
                    tracing::warn!(
                        payment_hash = %payment_hash,
                        "wallet history reports a failed movement for this payment hash; history is not attempt-scoped, treating outcome as unknown"
                    );
                }
                PaymentOutcome::Pending => {}
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

enum PaymentOutcome {
    Succeeded(String),
    Failed,
    Pending,
}

fn payment_hashes_match(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim().trim_start_matches("0x").to_ascii_lowercase();
    norm(a) == norm(b)
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

fn lightning_payment_succeeded(state: &str) -> bool {
    // Exact matches only: a substring match would read "unsuccessful" as
    // success.
    matches!(
        state,
        "successful" | "payment-successful" | "succeeded" | "paid" | "completed"
    )
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
                .get("destination")
                .and_then(|d| d.get("value"))
                .and_then(|v| v.as_str())
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
struct LightningPayStatus {
    state: String,
    #[serde(default)]
    preimage: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConnectedResponse {
    connected: bool,
}

#[derive(Debug, Deserialize)]
struct BlockTip {
    #[serde(alias = "tip_height")]
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

    #[test]
    fn unsuccessful_is_not_a_success_state() {
        assert!(!lightning_payment_succeeded("unsuccessful"));
        assert!(!lightning_payment_succeeded("failed"));
        assert!(!lightning_payment_succeeded("pending"));
        assert!(lightning_payment_succeeded("successful"));
        assert!(lightning_payment_succeeded("succeeded"));
        assert!(lightning_payment_succeeded("paid"));
    }
}

/// Classification tests for the reserve/settle/release flow, against a mock
/// barkd HTTP wallet. These exist because the melt-level tests stub out the
/// Ark client and cannot see this layer (finding 1).
#[cfg(test)]
mod wallet_server_tests {
    use super::*;
    use axum::extract::{Path, State};
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use cdk_common::bitcoin::hashes::{sha256, Hash};
    use std::sync::{Arc, Mutex};

    const TEST_PREIMAGE: [u8; 32] = *b"minerva wallet-http test preimag";

    fn test_payment_hash() -> String {
        hex::encode(sha256::Hash::hash(&TEST_PREIMAGE).to_byte_array())
    }

    fn test_invoice(amount_sat: u64) -> String {
        crate::bolt11_util::signed_test_invoice(
            amount_sat,
            sha256::Hash::hash(&TEST_PREIMAGE).to_byte_array(),
        )
    }

    fn movement(payment_hash: &str, status: &str, preimage: Option<&str>) -> serde_json::Value {
        let mut m = serde_json::json!({
            "kind": "lightning-pay",
            "status": status,
            "metadata": { "payment_hash": payment_hash },
        });
        if let Some(p) = preimage {
            m["metadata"]["preimage"] = serde_json::Value::String(p.to_string());
        }
        m
    }

    #[derive(Default)]
    struct MockWalletState {
        /// Status-endpoint responses per payment hash, one per call; the
        /// last is repeated. A hash with no entry -> 404 "path not round"
        /// (endpoint absent / unknown payment).
        status_sequences: std::collections::HashMap<String, Vec<serde_json::Value>>,
        status_calls: std::collections::HashMap<String, usize>,
        history: Vec<serde_json::Value>,
        /// Rows appended to history when POST /lightning/pay is dispatched.
        on_dispatch: Vec<serde_json::Value>,
    }

    fn statuses(seq: Vec<serde_json::Value>) -> std::collections::HashMap<String, Vec<serde_json::Value>> {
        [(test_payment_hash(), seq)].into_iter().collect()
    }

    #[derive(Clone, Default)]
    struct MockWallet(Arc<Mutex<MockWalletState>>);

    async fn wallet_sync() -> Json<serde_json::Value> {
        Json(serde_json::json!({}))
    }

    async fn lightning_pay(State(w): State<MockWallet>) -> Json<serde_json::Value> {
        let mut s = w.0.lock().unwrap();
        let new_rows = std::mem::take(&mut s.on_dispatch);
        s.history.extend(new_rows);
        Json(serde_json::json!({}))
    }

    async fn pay_status(
        State(w): State<MockWallet>,
        Path(hash): Path<String>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        let mut s = w.0.lock().unwrap();
        let Some(seq) = s.status_sequences.get(&hash) else {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!("path not round")),
            );
        };
        let seq_len = seq.len();
        let calls = s.status_calls.entry(hash.clone()).or_insert(0);
        let i = (*calls).min(seq_len - 1);
        *calls += 1;
        (StatusCode::OK, Json(s.status_sequences.get(&hash).unwrap()[i].clone()))
    }

    async fn history(State(w): State<MockWallet>) -> Json<serde_json::Value> {
        let rows = w.0.lock().unwrap().history.clone();
        Json(serde_json::Value::Array(rows))
    }

    async fn spawn_wallet(state: MockWalletState) -> String {
        let app = Router::new()
            .route("/api/v1/wallet/sync", post(wallet_sync))
            .route("/api/v1/lightning/pay", post(lightning_pay))
            .route("/api/v1/lightning/pay/status/{hash}", get(pay_status))
            .route("/api/v1/history", get(history))
            .with_state(MockWallet(Arc::new(Mutex::new(state))));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn client(base: &str) -> WalletHttpClient {
        // 3s poll timeout keeps hold-outcome tests fast.
        WalletHttpClient::new(base, None, 3, 1, None, false).unwrap()
    }

    /// Finding 1 regression: a terminal "failed" from the status endpoint
    /// must surface as PaymentFailed, not be swallowed by the history
    /// fallback into an ambiguous timeout.
    #[tokio::test]
    async fn definitive_status_failure_is_payment_failed() {
        let base = spawn_wallet(MockWalletState {
            status_sequences: statuses(vec![serde_json::json!({"state": "failed"})]),
            ..Default::default()
        })
        .await;
        let err = client(&base)
            .pay_lightning_invoice(&test_invoice(500), 500, false)
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::PaymentFailed(_)), "got {err:?}");
    }

    /// Finding 2 regression: a failed movement from a PREVIOUS attempt (in
    /// history before dispatch) must not fail the current attempt.
    #[tokio::test]
    async fn stale_failed_history_does_not_release() {
        let hash = test_payment_hash();
        let base = spawn_wallet(MockWalletState {
            // Status endpoint absent -> history fallback path.
            history: vec![movement(&hash, "failed", None)],
            on_dispatch: vec![movement(&hash, "pending", None)],
            ..Default::default()
        })
        .await;
        let err = client(&base)
            .pay_lightning_invoice(&test_invoice(500), 500, false)
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::Ark(_)), "got {err:?}");
        assert!(err.to_string().contains("timed out"), "got {err}");
    }

    /// Finding 2 regression: a stale successful movement (valid preimage but
    /// from a previous attempt) must not settle the current attempt.
    #[tokio::test]
    async fn stale_success_history_does_not_settle() {
        let hash = test_payment_hash();
        let base = spawn_wallet(MockWalletState {
            history: vec![movement(&hash, "successful", Some(&hex::encode(TEST_PREIMAGE)))],
            on_dispatch: vec![movement(&hash, "pending", None)],
            ..Default::default()
        })
        .await;
        let err = client(&base)
            .pay_lightning_invoice(&test_invoice(500), 500, false)
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::Ark(_)), "got {err:?}");
    }

    /// A NEW successful movement (created at dispatch) with a hash-verified
    /// preimage settles via the history fallback.
    #[tokio::test]
    async fn new_successful_movement_settles() {
        let hash = test_payment_hash();
        let base = spawn_wallet(MockWalletState {
            on_dispatch: vec![movement(&hash, "successful", Some(&hex::encode(TEST_PREIMAGE)))],
            ..Default::default()
        })
        .await;
        let payout = client(&base)
            .pay_lightning_invoice(&test_invoice(500), 500, false)
            .await
            .unwrap();
        assert_eq!(payout.preimage, hex::encode(TEST_PREIMAGE));
        // The barkd wallet API reports no actual paid fee; unmeasured stays
        // None so the mint charges nothing.
        assert_eq!(payout.fee_paid_sat, None);
    }

    /// A preimage that does not hash to the payment hash is not a success.
    #[tokio::test]
    async fn mismatched_preimage_is_unknown_not_success() {
        let base = spawn_wallet(MockWalletState {
            status_sequences: statuses(vec![serde_json::json!({
                "state": "successful",
                "preimage": hex::encode([0u8; 32]),
            })]),
            ..Default::default()
        })
        .await;
        let err = client(&base)
            .pay_lightning_invoice(&test_invoice(500), 500, false)
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::PayoutUnknown(_)), "got {err:?}");
    }

    /// Failed-after-success is a contradiction: unknown, never PaymentFailed.
    #[tokio::test]
    async fn failed_after_success_signal_is_unknown() {
        let base = spawn_wallet(MockWalletState {
            status_sequences: statuses(vec![
                serde_json::json!({"state": "successful"}),
                serde_json::json!({"state": "failed"}),
            ]),
            ..Default::default()
        })
        .await;
        let err = client(&base)
            .pay_lightning_invoice(&test_invoice(500), 500, false)
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::PayoutUnknown(_)), "got {err:?}");
    }

    /// Re-quoted hash with a stale terminal "failed" on the status endpoint
    /// (left over from the previous attempt while the new payout is in
    /// flight): must hold as unknown, never auto-release.
    #[tokio::test]
    async fn stale_status_failure_on_requoted_hash_is_unknown() {
        let hash = test_payment_hash();
        let base = spawn_wallet(MockWalletState {
            // Previous attempt's row in history => snapshot is non-empty =>
            // the status "failed" cannot be scoped to the new attempt.
            status_sequences: statuses(vec![serde_json::json!({"state": "failed"})]),
            history: vec![movement(&hash, "failed", None)],
            on_dispatch: vec![movement(&hash, "pending", None)],
            ..Default::default()
        })
        .await;
        let err = client(&base)
            .pay_lightning_invoice(&test_invoice(500), 500, false)
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::PayoutUnknown(_)), "got {err:?}");
    }

    /// A "failed" status with an EMPTY pre-dispatch snapshot is still not a
    /// definitive failure when the mint's own records show a prior attempt
    /// on this payment hash: wallet history can paginate, prune or lag, so
    /// an empty snapshot alone must not ungate auto-release.
    #[tokio::test]
    async fn failed_status_with_mint_side_prior_attempt_is_unknown() {
        let base = spawn_wallet(MockWalletState {
            status_sequences: statuses(vec![serde_json::json!({"state": "failed"})]),
            on_dispatch: vec![movement(&test_payment_hash(), "pending", None)],
            ..Default::default()
        })
        .await;
        let err = client(&base)
            .pay_lightning_invoice(&test_invoice(500), 500, true)
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::PayoutUnknown(_)), "got {err:?}");
    }

    /// "unsuccessful" from the status endpoint is not a success signal.
    #[tokio::test]
    async fn unsuccessful_status_is_not_success() {
        let base = spawn_wallet(MockWalletState {
            status_sequences: statuses(vec![serde_json::json!({"state": "unsuccessful"})]),
            ..Default::default()
        })
        .await;
        let err = client(&base)
            .pay_lightning_invoice(&test_invoice(500), 500, false)
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::Ark(_)), "got {err:?}");
        assert!(err.to_string().contains("timed out"), "got {err}");
    }
}
