//! Mint backend: Cashu mint logic with Ark VTXO backing instead of a
//! Lightning node.
//!
//! Flow per the spec:
//! - Deposit (`/v1/mint/*`): quote -> check VTXO inventory -> board sats via
//!   the ASP if free reserve is insufficient -> issue blinded tokens -> map
//!   token batch to backing VTXO.
//! - Withdrawal (`/v1/melt/*`): verify tokens -> pay out via ASP (Ark
//!   offboarding or LN through the ASP's gateway) -> release VTXO mapping.
//! - Swap (`/v1/swap`): pure blind-signature refresh; no Ark interaction.
//!
//! Blind signatures use [`crate::blind_signer::BlindSigner`] (mock, remote CDK
//! signatory, or local dev dhke).

use crate::ark_client::{ArkClient, PayoutResult};
use crate::blind_signer::BlindSigner;
use crate::bolt11_util::{decode_invoice, mock_amount_from_request};
use crate::config::AppConfig;
use crate::error::{MintError, Result};
use crate::keyset_cache::MintKeysetState;
use crate::ots::{digest_from_root_hex, OtsStamper};
use crate::pol::PolLedger;
use crate::receipts::{ReceiptSigner, ReceiptStore};
use crate::signatory::{DefaultSignatoryPolicy, MintSignRequest, SignatoryPolicy, SwapSignRequest};
use crate::spent_store::{MeltQuoteRecord, SpentSecretStore};
use crate::types::*;
use crate::vtxo_inventory::VtxoInventory;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;
use uuid::Uuid;

const QUOTE_TTL_SECS: u64 = 600;
const MSAT_PER_SAT: u64 = 1000;
/// Upper bound on NUT-08 blank outputs accepted per melt (the spec's count
/// is `ceil(log2(fee_reserve))`, small in practice).
const MAX_BLANK_OUTPUTS: usize = 64;

/// Re-export for tests and external callers.
pub use crate::types::KEYSET_ID;

#[derive(Debug, Clone)]
struct MintQuote {
    amount_sat: u64,
    state: QuoteState,
    expiry: u64,
}

pub struct MintBackend {
    config: AppConfig,
    ark: Arc<dyn ArkClient>,
    signer: Arc<dyn BlindSigner>,
    inventory: VtxoInventory,
    pol: PolLedger,
    ots: Option<Arc<dyn OtsStamper>>,
    policy: DefaultSignatoryPolicy,
    policy_enforced: bool,
    keyset_state: RwLock<MintKeysetState>,
    mint_quotes: Mutex<HashMap<String, MintQuote>>,
    spent: SpentSecretStore,
    receipts: ReceiptStore,
    receipt_signer: ReceiptSigner,
}

impl MintBackend {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: AppConfig,
        ark: Arc<dyn ArkClient>,
        signer: Arc<dyn BlindSigner>,
        inventory: VtxoInventory,
        pol: PolLedger,
        spent: SpentSecretStore,
        receipts: ReceiptStore,
        receipt_signer: ReceiptSigner,
        ots: Option<Arc<dyn OtsStamper>>,
    ) -> Self {
        let policy = DefaultSignatoryPolicy::new(config.trust.max_mint_sat);
        MintBackend {
            policy_enforced: config.trust.signatory_policy_enforced,
            policy,
            pol,
            ots,
            config,
            ark,
            signer,
            inventory,
            spent,
            receipts,
            receipt_signer,
            keyset_state: RwLock::new(MintKeysetState::mock_default()),
            mint_quotes: Mutex::new(HashMap::new()),
        }
    }

    /// Load pubkey and keyset metadata from the signatory (call at startup).
    pub async fn init_keysets(&self) -> Result<()> {
        let state = self.signer.keyset_state().await?;
        tracing::info!(
            active_keyset_id = %state.active_keyset_id,
            keysets = state.keysets.len(),
            "signatory keysets loaded"
        );
        *self.keyset_state.write().await = state;
        Ok(())
    }

    pub fn active_keyset_id(&self) -> String {
        self.keyset_state
            .try_read()
            .ok()
            .map(|s| s.active_keyset_id.clone())
            .unwrap_or_else(|| KEYSET_ID.to_string())
    }

    pub async fn keysets(&self) -> KeysetsResponse {
        let state = self.keyset_state.read().await;
        KeysetsResponse {
            keysets: state.keysets.clone(),
        }
    }

    /// NUT-02 `GET /v1/keys`: public keys of all active keysets.
    pub async fn keys(&self) -> KeysResponse {
        let state = self.keyset_state.read().await;
        let active: std::collections::HashSet<&str> = state
            .keysets
            .iter()
            .filter(|k| k.active)
            .map(|k| k.id.as_str())
            .collect();
        KeysResponse {
            keysets: state
                .keys
                .iter()
                .filter(|k| active.contains(k.id.as_str()))
                .cloned()
                .collect(),
        }
    }

    /// NUT-02 `GET /v1/keys/{keyset_id}`: public keys of one keyset (active or not).
    pub async fn keys_for(&self, keyset_id: &str) -> Result<KeysResponse> {
        let state = self.keyset_state.read().await;
        let keyset = state
            .keys
            .iter()
            .find(|k| k.id == keyset_id)
            .cloned()
            .ok_or_else(|| MintError::KeysetNotFound(keyset_id.to_string()))?;
        Ok(KeysResponse {
            keysets: vec![keyset],
        })
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    pub fn ark(&self) -> &Arc<dyn ArkClient> {
        &self.ark
    }

    pub fn inventory(&self) -> &VtxoInventory {
        &self.inventory
    }

    pub fn pol(&self) -> &PolLedger {
        &self.pol
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn validate_outputs(&self, outputs: &[BlindedMessage]) -> Result<u64> {
        if outputs.is_empty() {
            return Err(MintError::InvalidRequest("no outputs provided".into()));
        }
        let keyset_id = self.active_keyset_id();
        for o in outputs {
            if o.id != keyset_id {
                return Err(MintError::InvalidRequest(format!(
                    "unknown keyset id: {} (active {})",
                    o.id, keyset_id
                )));
            }
            if !o.amount.is_power_of_two() {
                return Err(MintError::InvalidRequest(format!(
                    "output amount {} is not a power of two",
                    o.amount
                )));
            }
        }
        Ok(outputs.iter().map(|o| o.amount).sum())
    }

    /// Validate NUT-08 blank outputs: keyset ids only. Their `amount`
    /// fields are undetermined by definition (wallets set 0) and are ignored;
    /// the mint imprints the return amounts at settle.
    fn validate_blank_outputs(&self, outputs: &[BlindedMessage]) -> Result<()> {
        if outputs.len() > MAX_BLANK_OUTPUTS {
            return Err(MintError::InvalidRequest(format!(
                "too many blank change outputs: {} (max {MAX_BLANK_OUTPUTS})",
                outputs.len()
            )));
        }
        let keyset_id = self.active_keyset_id();
        for o in outputs {
            if o.id != keyset_id {
                return Err(MintError::InvalidRequest(format!(
                    "unknown keyset id: {} (active {})",
                    o.id, keyset_id
                )));
            }
        }
        Ok(())
    }

    /// Validate proofs (keyset ids and, unless the signatory is mocked, CDK
    /// signatures) and return their total. Does not touch the spent store.
    async fn verify_inputs(&self, inputs: &[Proof]) -> Result<u64> {
        if inputs.is_empty() {
            return Err(MintError::InvalidRequest("no inputs provided".into()));
        }
        let keyset_id = self.active_keyset_id();
        for p in inputs {
            if p.id != keyset_id && p.id != KEYSET_ID {
                return Err(MintError::InvalidRequest(format!(
                    "unknown keyset id: {}",
                    p.id
                )));
            }
        }
        if self.config.signatory.backend != "mock" {
            self.signer.verify_proofs(inputs).await?;
        }
        Ok(inputs.iter().map(|p| p.amount).sum())
    }

    /// Verify proofs (CDK when not mock), mark secrets spent. Does not update PoL (swap is net-zero).
    async fn verify_and_spend_inputs(&self, inputs: &[Proof]) -> Result<u64> {
        let input_total = self.verify_inputs(inputs).await?;
        let secrets: Vec<String> = inputs.iter().map(|p| p.secret.clone()).collect();
        if self.spent.any_spent(&secrets)? {
            return Err(MintError::TokenAlreadySpent);
        }
        self.spent
            .mark_spent_batch(&self.active_keyset_id(), inputs)?;
        Ok(input_total)
    }

    fn record_melt_burns(&self, inputs: &[Proof]) -> Result<()> {
        let keyset_id = self.active_keyset_id();
        for p in inputs {
            self.pol.record_burn(&keyset_id, &p.secret, p.amount)?;
        }
        Ok(())
    }

    // -- NUT-06 ------------------------------------------------------------

    pub async fn info(&self) -> MintInfo {
        let state = self.keyset_state.read().await;
        let active_id = state.active_keyset_id.clone();
        MintInfo {
            name: self.config.mint.name.clone(),
            pubkey: state.pubkey.clone(),
            version: format!("minerva-mint/{}", env!("CARGO_PKG_VERSION")),
            description: self.config.mint.description.clone(),
            contact: vec![],
            motd: None,
            nuts: serde_json::json!({
                "4": { "methods": [{"method": "bolt11", "unit": "sat", "id": active_id}], "disabled": false },
                "5": { "methods": [{"method": "bolt11", "unit": "sat", "id": active_id}], "disabled": false },
                "8": { "supported": true },
            }),
            receipt_pubkey: Some(self.receipt_signer.pubkey_hex()),
        }
    }

    // -- NUT-04: deposit ----------------------------------------------------

    pub async fn mint_quote(&self, req: MintQuoteBolt11Request) -> Result<MintQuoteBolt11Response> {
        if req.unit != "sat" {
            return Err(MintError::InvalidRequest(format!(
                "unsupported unit: {}",
                req.unit
            )));
        }
        if req.amount == 0 {
            return Err(MintError::InvalidRequest("amount must be > 0".into()));
        }

        let quote_id = Uuid::new_v4().to_string();
        let expiry = Self::now() + QUOTE_TTL_SECS;

        // The scaffold has no LN invoice generation; the mock ASP "settles"
        // quotes immediately, so they are born PAID. Real flow: ASP issues a
        // bolt11 invoice (or an Ark boarding address) and a watcher flips the
        // state to PAID on settlement.
        let quote = MintQuote {
            amount_sat: req.amount,
            state: QuoteState::Paid,
            expiry,
        };
        self.mint_quotes
            .lock()
            .unwrap()
            .insert(quote_id.clone(), quote);

        Ok(MintQuoteBolt11Response {
            quote: quote_id.clone(),
            request: format!("lnbcmock1{quote_id}"),
            amount: req.amount,
            unit: "sat".into(),
            state: QuoteState::Paid,
            expiry,
        })
    }

    /// NUT-04: poll mint quote state by id.
    pub async fn get_mint_quote(&self, quote_id: &str) -> Result<MintQuoteBolt11Response> {
        let quotes = self.mint_quotes.lock().unwrap();
        let quote = quotes
            .get(quote_id)
            .ok_or_else(|| MintError::QuoteNotFound(quote_id.into()))?;
        Ok(MintQuoteBolt11Response {
            quote: quote_id.into(),
            request: format!("lnbcmock1{quote_id}"),
            amount: quote.amount_sat,
            unit: "sat".into(),
            state: quote.state,
            expiry: quote.expiry,
        })
    }

    pub async fn mint(&self, req: MintBolt11Request) -> Result<MintBolt11Response> {
        let amount_sat = {
            let quotes = self.mint_quotes.lock().unwrap();
            let quote = quotes
                .get(&req.quote)
                .ok_or_else(|| MintError::QuoteNotFound(req.quote.clone()))?;
            match quote.state {
                QuoteState::Issued => return Err(MintError::QuoteAlreadyIssued(req.quote.clone())),
                QuoteState::Paid => {}
                _ => return Err(MintError::QuoteNotPaid(req.quote.clone())),
            }
            if Self::now() > quote.expiry {
                return Err(MintError::InvalidRequest(format!(
                    "quote expired: {}",
                    req.quote
                )));
            }
            quote.amount_sat
        };

        let output_total = self.validate_outputs(&req.outputs)?;
        if output_total != amount_sat {
            return Err(MintError::Unbalanced {
                inputs: amount_sat,
                outputs: output_total,
            });
        }

        let amount_msat = amount_sat * MSAT_PER_SAT;

        // Ensure VTXO liquidity, boarding more sats if the free reserve
        // can't absorb this issuance.
        self.ensure_liquidity(amount_msat).await?;

        // Map this issuance batch to a backing VTXO. The batch (quote) id is
        // the token_id; individual proofs aren't linkable to it (blinding).
        self.inventory
            .allocate_vtxo_for_tokens(&req.quote, amount_msat)?;

        let mapping = self
            .inventory
            .get_mapping(&req.quote)?
            .ok_or_else(|| MintError::MappingNotFound(req.quote.clone()))?;

        if self.policy_enforced {
            let quote_state = self
                .mint_quotes
                .lock()
                .unwrap()
                .get(&req.quote)
                .map(|q| q.state)
                .unwrap_or(QuoteState::Unpaid);
            let quote_expiry = self
                .mint_quotes
                .lock()
                .unwrap()
                .get(&req.quote)
                .map(|q| q.expiry)
                .unwrap_or(0);
            self.policy.can_sign_mint(&MintSignRequest {
                quote_id: &req.quote,
                amount_sat,
                outputs: &req.outputs,
                quote_state,
                quote_expiry,
                now: Self::now(),
                keyset_id: &self.active_keyset_id(),
                vtxo_id: Some(&mapping.vtxo_id),
            })?;
        }

        let signatures = self.signer.blind_sign(&req.outputs).await?;

        let b_points: Vec<String> = req.outputs.iter().map(|o| o.b.clone()).collect();
        self.pol
            .record_mint(&self.active_keyset_id(), &req.quote, amount_sat, &b_points)?;

        self.mint_quotes
            .lock()
            .unwrap()
            .get_mut(&req.quote)
            .expect("quote exists")
            .state = QuoteState::Issued;

        if let Err(e) =
            self.issue_receipt("mint", &req.quote, amount_sat, None, None, None, "issued")
        {
            // The issuance state transition already committed; the failure
            // is logged and the PoL anchor (if that was the failure) is
            // rewritten by recover_receipt_anchors.
            tracing::error!(error = %e, "receipt issuance failed");
        }

        Ok(MintBolt11Response { signatures })
    }

    /// Board sats with the ASP when free reserve is insufficient for an
    /// issuance, or below the configured auto-board threshold.
    async fn ensure_liquidity(&self, needed_msat: u64) -> Result<()> {
        let free = self.inventory.free_reserve_msat()?;
        let lq = &self.config.liquidity;
        let threshold_msat = (lq.min_vtxo_reserve_msat as f64 * lq.auto_board_threshold) as u64;

        if free >= needed_msat && free >= threshold_msat {
            return Ok(());
        }

        let deficit = needed_msat.saturating_sub(free);
        let target = deficit
            .max(lq.min_vtxo_reserve_msat)
            .min(lq.max_single_vtxo_msat);

        tracing::info!(
            target_msat = target,
            free_msat = free,
            "boarding sats with ASP"
        );
        let vtxo = self.ark.board_sats(target).await?;
        self.inventory.insert_vtxo(&vtxo)?;
        Ok(())
    }

    // -- NUT-05: withdrawal ---------------------------------------------------

    pub async fn melt_quote(&self, req: MeltQuoteBolt11Request) -> Result<MeltQuoteBolt11Response> {
        if req.unit != "sat" {
            return Err(MintError::InvalidRequest(format!(
                "unsupported unit: {}",
                req.unit
            )));
        }
        if req.request.is_empty() {
            return Err(MintError::InvalidRequest("empty payment request".into()));
        }

        let live = self.config.melt_uses_live_payout();
        let (amount_sat, payment_hash) = match decode_invoice(&req.request) {
            Ok((amount, hash)) => (amount, Some(hash)),
            Err(e) if live => return Err(e),
            Err(_) => (mock_amount_from_request(&req.request), None),
        };

        // An invoice this mint already settled cannot be paid again; refuse
        // the quote up front instead of letting a melt attempt discover it.
        if let Some(hash) = &payment_hash {
            if self.spent.has_settled_melt_for_hash(hash)? {
                return Err(MintError::InvalidRequest(
                    "invoice already paid by a settled melt".into(),
                ));
            }
        }

        let fee_reserve_sat = self
            .ark
            .estimate_lightning_send_fee_sat(amount_sat)
            .await?;

        let quote_id = Uuid::new_v4().to_string();
        let expiry = Self::now() + QUOTE_TTL_SECS;
        self.spent.insert_melt_quote(&MeltQuoteRecord {
            quote_id: quote_id.clone(),
            amount_sat,
            fee_reserve_sat,
            request: req.request,
            payment_hash,
            state: MeltQuoteState::Unpaid,
            expiry,
            preimage: None,
            change_outputs: vec![],
            fee_paid_sat: None,
            input_total_sat: None,
            change_signatures: vec![],
            token_ids: vec![],
            backing_released: false,
        })?;

        Ok(MeltQuoteBolt11Response {
            quote: quote_id.clone(),
            amount: amount_sat,
            fee_reserve: fee_reserve_sat,
            unit: "sat".into(),
            state: QuoteState::Unpaid,
            expiry,
            payment_preimage: None,
            change: None,
        })
    }

    /// NUT-05: poll melt quote state by id.
    pub async fn get_melt_quote(&self, quote_id: &str) -> Result<MeltQuoteBolt11Response> {
        let mut quote = self
            .spent
            .melt_quote(quote_id)?
            .ok_or_else(|| MintError::QuoteNotFound(quote_id.into()))?;
        // Recovery: finalization after the settle commit (change signing,
        // backing release, receipt) is idempotent, so a settled quote is
        // finalized here on demand - this is how a wallet that lost the
        // POST melt response, or whose melt was settled by an operator
        // reconcile, recovers its NUT-08 change. The GET stays safe:
        // repeated calls converge to the same persisted state.
        if matches!(quote.state, MeltQuoteState::Settled) {
            // Wallet-critical: the change must be served. Operator
            // bookkeeping is retried here and at startup; its failure
            // leaves a pending status and never blocks the wallet.
            self.ensure_melt_change(quote_id).await?;
            if let Err(e) = self.finalize_melt_bookkeeping(quote_id) {
                tracing::error!(
                    quote = %quote_id,
                    error = %e,
                    "melt finalization pending: backing release or receipt incomplete; will retry"
                );
            }
            quote = self
                .spent
                .melt_quote(quote_id)?
                .ok_or_else(|| MintError::QuoteNotFound(quote_id.into()))?;
        } else if matches!(quote.state, MeltQuoteState::Released) {
            // Same idempotent repair for the release receipt: a crash
            // between the release commit and the receipt write is
            // completed here, once. Operator bookkeeping only: a receipt
            // failure is logged as pending and retried by the next quote
            // check, never blocking the wallet's poll.
            let already = self
                .receipts
                .for_quote(quote_id)?
                .iter()
                .any(|r| r.kind == "melt" && r.outcome == "released");
            if !already {
                if let Err(e) = self.issue_receipt(
                    "melt",
                    quote_id,
                    quote.amount_sat,
                    None,
                    Some(0),
                    None,
                    "released",
                ) {
                    tracing::error!(
                        quote = %quote_id,
                        error = %e,
                        "released-melt receipt pending; will retry"
                    );
                }
            }
        }
        Ok(MeltQuoteBolt11Response {
            quote: quote_id.into(),
            amount: quote.amount_sat,
            fee_reserve: quote.fee_reserve_sat,
            unit: "sat".into(),
            state: public_melt_state(quote.state),
            expiry: quote.expiry,
            payment_preimage: quote.preimage,
            change: if quote.change_signatures.is_empty() {
                None
            } else {
                Some(quote.change_signatures)
            },
        })
    }

    /// NUT-05 melt as reserve -> settle | release (docs/reserve-settle-receipts.md).
    ///
    /// Inputs are marked pending (reserved) before any payout is attempted.
    /// A confirmed payout settles the reservation (inputs burned, PoL burns
    /// recorded). A payout that never started or definitively failed releases
    /// it (inputs spendable again). Any other error leaves the outcome
    /// unknown: the reservation is held - never charged, never released -
    /// until an operator reconciles it with
    /// [`Self::reconcile_release_reserved_melt`].
    pub async fn melt(&self, req: MeltBolt11Request) -> Result<MeltBolt11Response> {
        let quote = self
            .spent
            .melt_quote(&req.quote)?
            .ok_or_else(|| MintError::QuoteNotFound(req.quote.clone()))?;
        if Self::now() > quote.expiry {
            return Err(MintError::InvalidRequest(format!(
                "quote expired: {}",
                req.quote
            )));
        }

        // NUT-08 blank outputs are validated up front so a malformed request
        // changes no state; the amounts are imprinted by the mint at settle.
        let blank_outputs = req.outputs.clone().unwrap_or_default();
        self.validate_blank_outputs(&blank_outputs)?;
        // User-supplied backing tokens are validated before any state
        // change or payout: an unknown token id fails the request here,
        // not the post-settle release.
        if let Some(ids) = &req.token_ids {
            for id in ids {
                if self.inventory.get_mapping(id)?.is_none() {
                    return Err(MintError::MappingNotFound(id.clone()));
                }
            }
        }

        // Verify before reserving so an invalid or undersized request changes
        // no state.
        let input_total = self.verify_inputs(&req.inputs).await?;
        if input_total < quote.amount_sat + quote.fee_reserve_sat {
            return Err(MintError::Unbalanced {
                inputs: input_total,
                outputs: quote.amount_sat + quote.fee_reserve_sat,
            });
        }

        // Reserve: quote UNPAID -> RESERVED, inputs marked pending and blank
        // change outputs persisted, atomically. Rejects already-spent/reserved
        // proofs, non-UNPAID quotes, and a second in-flight or already-settled
        // payout for the same invoice.
        let reserve_token_ids = req.token_ids.clone().unwrap_or_default();
        let attempted_before = self.spent.reserve_melt(
            &req.quote,
            &self.active_keyset_id(),
            &req.inputs,
            &blank_outputs,
            &reserve_token_ids,
        )?;

        let payout = if self.config.melt_uses_live_payout() {
            self.ark
                .pay_lightning_invoice(&quote.request, quote.amount_sat, attempted_before)
                .await
        } else {
            // Mock payout: deterministic preimage, no funds move, measured
            // fee zero.
            let mut hasher = Sha256::new();
            hasher.update(quote.request.as_bytes());
            Ok(PayoutResult {
                preimage: hex::encode(hasher.finalize()),
                fee_paid_sat: Some(0),
            })
        };

        match payout {
            Ok(payout) => {
                // Settle: burn the reserved inputs, then record the burns.
                self.spent
                    .settle_melt(&req.quote, &payout.preimage, payout.fee_paid_sat)?;
                self.record_melt_burns(&req.inputs)?;
                // Wallet-critical: the NUT-08 change must be signed. If
                // this fails the payout already settled and the inputs stay
                // burned; the quote check endpoint completes it.
                self.ensure_melt_change(&req.quote).await?;
                // Operator bookkeeping (backing release, receipt) never
                // blocks the wallet's change: failures leave a pending
                // status retried by quote checks and the startup sweep.
                if let Err(e) = self.finalize_melt_bookkeeping(&req.quote) {
                    tracing::error!(
                        quote = %req.quote,
                        error = %e,
                        "melt finalization pending: backing release or receipt incomplete; will retry"
                    );
                }
                let change = self.spent.change_signatures(&req.quote)?;
                Ok(MeltBolt11Response {
                    quote: req.quote,
                    state: QuoteState::Paid,
                    payment_preimage: Some(payout.preimage),
                    change: if change.is_empty() {
                        None
                    } else {
                        Some(change)
                    },
                })
            }
            Err(e) if payout_error_is_releasable(&e) => {
                // The payout never started or definitively failed: release the
                // reservation so the inputs are spendable again.
                self.spent.release_melt(&req.quote)?;
                if let Err(err) = self.issue_receipt(
                    "melt",
                    &req.quote,
                    quote.amount_sat,
                    None,
                    Some(0),
                    None,
                    "released",
                ) {
                    tracing::error!(error = %err, "receipt issuance failed");
                }
                Err(e)
            }
            Err(e) => {
                // Unknown outcome (timeout, transport error, ambiguous ASP
                // response). The payment may still complete or may already
                // have completed: hold the reservation, charge nothing,
                // release nothing, until reconciled.
                tracing::warn!(
                    quote = %req.quote,
                    error = %e,
                    "melt payout outcome unknown; reservation held for reconciliation"
                );
                Err(e)
            }
        }
    }

    /// Operator reconciliation for a melt stuck RESERVED after an unknown
    /// payout outcome.
    ///
    /// The caller MUST first verify out-of-band that the payout definitively
    /// failed or never started (for example via the wallet's Lightning
    /// payment history for the quote's payment hash). Releasing a
    /// reservation whose payout actually succeeded hands the client back
    /// spendable proofs for a payment the mint already made, spending the
    /// mint's backing twice.
    pub fn reconcile_release_reserved_melt(&self, quote_id: &str) -> Result<()> {
        let quote = self
            .spent
            .melt_quote(quote_id)?
            .ok_or_else(|| MintError::QuoteNotFound(quote_id.into()))?;
        self.spent.release_melt(quote_id)?;
        self.issue_receipt("melt", quote_id, quote.amount_sat, None, Some(0), None, "released")?;
        Ok(())
    }

    /// Operator reconciliation for a melt stuck RESERVED whose payout the
    /// operator has verified DID complete - for example a crash or storage
    /// error between the confirmed payout and the settle commit, which the
    /// release path must never touch (releasing there would hand back
    /// spendable proofs for a payment the mint already made).
    ///
    /// The preimage is checked against the quote's stored payment hash when
    /// one exists, so settling requires proof the payment happened. If the
    /// burn recording fails after the settle commit, the PoL ledger
    /// overstates liabilities (conservative) and the epoch can be
    /// reconciled from the spent store.
    pub async fn reconcile_settle_reserved_melt(
        &self,
        quote_id: &str,
        preimage: &str,
    ) -> Result<()> {
        let quote = self
            .spent
            .melt_quote(quote_id)?
            .ok_or_else(|| MintError::QuoteNotFound(quote_id.into()))?;
        verify_melt_preimage(preimage, quote.payment_hash.as_deref())?;
        // A preimage verifies against the payment HASH, not against this
        // quote's payout: if a sibling quote already settled this invoice,
        // settling here would burn a second input set for one payment.
        if let Some(hash) = &quote.payment_hash {
            if self
                .spent
                .has_settled_melt_for_hash_excluding(hash, quote_id)?
            {
                return Err(MintError::InvalidRequest(format!(
                    "invoice for quote {quote_id} was already settled by another melt; refusing to settle a second quote for the same payment"
                )));
            }
        }
        let inputs = self.spent.reserved_inputs(quote_id)?;
        self.spent.settle_melt(quote_id, preimage, None)?;
        for (secret, keyset_id, amount) in &inputs {
            self.pol.record_burn(keyset_id, secret, *amount)?;
        }
        // Finalize exactly like the happy path: NUT-08 change on the
        // persisted blank outputs (no fee measurement exists on the
        // operator path, so the charge is zero and the full reserve
        // returns), backing release for the net burn, and the settled
        // receipt. The wallet recovers the change from the quote check
        // endpoint. A wallet that sent no blank outputs opted out of
        // change; the surplus stays with the mint.
        self.finalize_settled_melt(quote_id).await?;
        Ok(())
    }

    /// Melt quotes currently RESERVED (payout in flight or outcome unknown),
    /// for operator reconciliation.
    pub fn list_reserved_melts(&self) -> Result<Vec<MeltQuoteRecord>> {
        self.spent.list_reserved_melts()
    }

    fn release_backing_for_melt(
        &self,
        quote_id: &str,
        token_ids: Option<&Vec<String>>,
        burned_sat: u64,
    ) -> Result<()> {
        if !self.config.trust.release_backing_on_melt && token_ids.is_none() {
            return Ok(());
        }
        let burned_msat = burned_sat.saturating_mul(MSAT_PER_SAT);
        if burned_msat == 0 {
            return Ok(());
        }

        // Idempotent per quote: the inventory logs each quote's release in
        // the same transaction as the deallocation, so retries after a
        // crash and racing finalizations replay the recorded amounts
        // instead of releasing twice (docs/reserve-settle-receipts.md).
        if let Some(ids) = token_ids {
            self.inventory
                .release_backing_for_quote(quote_id, ids, burned_msat)?;
            return Ok(());
        }

        if self.config.trust.release_backing_on_melt_fifo {
            let released = self
                .inventory
                .release_backing_for_quote_fifo(quote_id, burned_msat)?;
            if released < burned_msat {
                tracing::warn!(
                    released_msat = released,
                    needed_msat = burned_msat,
                    "FIFO melt backing release partial"
                );
            }
        }
        Ok(())
    }

    /// NUT-08: imprint the overpaid-fee return onto the wallet's blank
    /// outputs and sign it. Returns the signatures in the same order as the
    /// blank outputs they were imprinted on; empty when there is nothing to
    /// return or the wallet provided no outputs.
    async fn sign_melt_change(
        &self,
        quote_id: &str,
        blank_outputs: &[BlindedMessage],
        input_total: u64,
        amount_sat: u64,
        fee_charged_sat: u64,
    ) -> Result<Vec<BlindSignature>> {
        let change_total = input_total.saturating_sub(amount_sat + fee_charged_sat);
        if change_total == 0 || blank_outputs.is_empty() {
            return Ok(Vec::new());
        }
        // Decompose the return into ascending powers of two (NUT-00 amounts).
        // When the wallet sent fewer blank outputs than the decomposition
        // needs, the largest parts are signed and the unrepresentable dust
        // stays with the mint as fee; the wallet controls the output count
        // (NUT-08: `max(ceil(log2(fee_reserve)), 1)`).
        let mut parts = decompose_pow2(change_total);
        if parts.len() > blank_outputs.len() {
            let dropped: u64 = parts.drain(..parts.len() - blank_outputs.len()).sum();
            tracing::warn!(
                quote = %quote_id,
                dropped_sat = dropped,
                "wallet sent too few blank outputs; unreturnable dust kept as fee"
            );
        }
        let imprinted: Vec<BlindedMessage> = blank_outputs
            .iter()
            .zip(parts.iter())
            .map(|(o, amount)| BlindedMessage {
                amount: *amount,
                id: o.id.clone(),
                b: o.b.clone(),
            })
            .collect();
        let signatures = self.signer.blind_sign(&imprinted).await?;
        // The returned change is newly issued ecash: record it as a mint so
        // PoL liabilities stay exact against the full-input burn. The event
        // id is deterministic, so a replayed finalization (crash recovery,
        // a concurrent quote check) records it once, never twice.
        let signed_total: u64 = signatures.iter().map(|s| s.amount).sum();
        if signed_total > 0 {
            let b_points: Vec<String> = imprinted.iter().map(|o| o.b.clone()).collect();
            self.pol.record_mint_once(
                &self.active_keyset_id(),
                &format!("{quote_id}#change"),
                signed_total,
                &b_points,
            )?;
        }
        // Persist the signatures: they are the idempotency anchor for the
        // quote-check recovery path, which re-signs only while none are
        // stored. A crash between the PoL record above and this write
        // re-records the same mint event on recovery (PoL overstates,
        // conservative, and the epoch can be reconciled from the store).
        self.spent.store_change_signatures(quote_id, &signatures)?;
        Ok(signatures)
    }

    /// Issue a signed receipt and anchor it in the PoL epoch log. The two
    /// writes are not atomic; a PoL anchor missed here is rewritten by
    /// [`Self::recover_receipt_anchors`] (the anchor record is idempotent),
    /// so every receipt ends up anchored.
    #[allow(clippy::too_many_arguments)]
    fn issue_receipt(
        &self,
        kind: &str,
        quote_id: &str,
        amount_sat: u64,
        fee_charged_sat: Option<u64>,
        retained_sat: Option<u64>,
        change_sat: Option<u64>,
        outcome: &str,
    ) -> Result<()> {
        let keyset_id = self.active_keyset_id();
        let receipt = self.receipts.append(
            &self.receipt_signer,
            kind,
            quote_id,
            amount_sat,
            fee_charged_sat,
            retained_sat,
            change_sat,
            outcome,
            &keyset_id,
        )?;
        if let Err(e) = self
            .pol
            .record_receipt(&keyset_id, &receipt.receipt_hash, kind)
        {
            // Recoverable: recover_receipt_anchors rewrites missing anchors.
            tracing::error!(error = %e, "receipt issued but PoL anchor failed; recovery will rewrite it");
        }
        Ok(())
    }

    /// Post-settle finalization for a melt has two halves with different
    /// failure handling:
    ///
    /// - [`Self::ensure_melt_change`] is wallet-critical: the NUT-08 change
    ///   must be signed and served. Its errors propagate; the next call
    ///   retries.
    /// - [`Self::finalize_melt_bookkeeping`] is operator bookkeeping:
    ///   backing release for the NET burn (the change is reissued ecash
    ///   and keeps its backing) and the settled receipt. Both steps are
    ///   durable-idempotent (the inventory logs releases per quote in the
    ///   deallocation transaction; a settled receipt is unique per quote),
    ///   so callers log failures as a pending status and every quote check
    ///   and the startup sweep retry them - a bookkeeping failure never
    ///   makes the wallet's change inaccessible.
    ///
    /// `finalize_settled_melt` runs both halves and propagates everything;
    /// the operator reconcile path uses it.
    async fn finalize_settled_melt(&self, quote_id: &str) -> Result<()> {
        self.ensure_melt_change(quote_id).await?;
        self.finalize_melt_bookkeeping(quote_id)
    }

    /// Sign the NUT-08 change onto the persisted blank outputs when it has
    /// not been signed yet. Persisted signatures are the idempotency
    /// anchor.
    async fn ensure_melt_change(&self, quote_id: &str) -> Result<()> {
        let quote = self
            .spent
            .melt_quote(quote_id)?
            .ok_or_else(|| MintError::QuoteNotFound(quote_id.into()))?;
        if !matches!(quote.state, MeltQuoteState::Settled) {
            return Err(MintError::InvalidRequest(format!(
                "melt quote {quote_id} is not settled"
            )));
        }
        if quote.change_outputs.is_empty() || !quote.change_signatures.is_empty() {
            return Ok(());
        }
        // Charge only the measured fee, capped at the reserve. An
        // unmeasured fee is charged as zero: the missing measurement never
        // turns the reservation into the charge
        // (docs/reserve-settle-receipts.md).
        let fee_charged = quote.fee_reserve_sat.min(quote.fee_paid_sat.unwrap_or(0));
        let input_total = quote
            .input_total_sat
            .unwrap_or(quote.amount_sat + quote.fee_reserve_sat);
        self.sign_melt_change(
            quote_id,
            &quote.change_outputs,
            input_total,
            quote.amount_sat,
            fee_charged,
        )
        .await?;
        Ok(())
    }

    /// Backing release for the net burn + the settled receipt. Idempotent;
    /// skipped while the change is still unsigned, because the net burn is
    /// only known once the change exists (releasing first would release
    /// the change's backing too).
    fn finalize_melt_bookkeeping(&self, quote_id: &str) -> Result<()> {
        let quote = self
            .spent
            .melt_quote(quote_id)?
            .ok_or_else(|| MintError::QuoteNotFound(quote_id.into()))?;
        if !matches!(quote.state, MeltQuoteState::Settled) {
            return Err(MintError::InvalidRequest(format!(
                "melt quote {quote_id} is not settled"
            )));
        }
        if !quote.change_outputs.is_empty() && quote.change_signatures.is_empty() {
            // Change first: the net burn is unknown until it exists.
            return Ok(());
        }
        let change_total: u64 = quote.change_signatures.iter().map(|c| c.amount).sum();
        let input_total = quote
            .input_total_sat
            .unwrap_or(quote.amount_sat + quote.fee_reserve_sat);
        if !quote.backing_released {
            let token_ids = if quote.token_ids.is_empty() {
                None
            } else {
                Some(&quote.token_ids)
            };
            self.release_backing_for_melt(
                quote_id,
                token_ids,
                input_total.saturating_sub(change_total),
            )?;
            self.spent.mark_backing_released(quote_id)?;
        }
        // Settled receipt: unique per (quote, kind, outcome) in the store,
        // so a racing or repeated finalization appends once.
        let already = self
            .receipts
            .for_quote(quote_id)?
            .iter()
            .any(|r| r.kind == "melt" && r.outcome == "settled");
        if !already {
            let fee_charged = quote.fee_reserve_sat.min(quote.fee_paid_sat.unwrap_or(0));
            let retained = input_total.saturating_sub(quote.amount_sat + change_total);
            self.issue_receipt(
                "melt",
                quote_id,
                quote.amount_sat,
                Some(fee_charged),
                Some(retained),
                Some(change_total),
                "settled",
            )?;
        }
        Ok(())
    }

    /// Retry pending post-settle finalization (unsigned change, backing
    /// release, receipt) for every settled melt that is not fully
    /// finalized. Idempotent; pairs with the on-demand finalization in the
    /// quote check endpoint. Returns how many quotes completed.
    pub async fn recover_pending_finalize(&self) -> Result<usize> {
        let mut completed = 0;
        for quote in self.spent.list_pending_finalize_melts()? {
            match self.finalize_settled_melt(&quote.quote_id).await {
                Ok(()) => completed += 1,
                Err(e) => tracing::error!(
                    quote = %quote.quote_id,
                    error = %e,
                    "melt finalization still pending after recovery pass"
                ),
            }
        }
        Ok(completed)
    }

    /// Re-anchor any receipts missing their PoL epoch event (a crash or a
    /// logged failure between the two writes in [`Self::issue_receipt`]).
    /// Anchoring is idempotent. Returns how many anchors were rewritten.
    pub fn recover_receipt_anchors(&self) -> Result<usize> {
        let mut recovered = 0;
        for receipt in self.receipts.all_receipts()? {
            if !self.pol.has_receipt_event(&receipt.receipt_hash)? {
                self.pol
                    .record_receipt(&receipt.keyset_id, &receipt.receipt_hash, &receipt.kind)?;
                recovered += 1;
            }
        }
        if recovered > 0 {
            tracing::warn!(recovered, "re-anchored receipts missing their PoL epoch event");
        }
        Ok(recovered)
    }

    /// All receipts issued for a quote (oldest first), for
    /// `GET /v1/receipts/{quote_id}`.
    pub fn receipts_for_quote(&self, quote_id: &str) -> Result<ReceiptsResponse> {
        Ok(ReceiptsResponse {
            receipts: self.receipts.for_quote(quote_id)?,
        })
    }

    // -- NUT-03: swap ---------------------------------------------------------

    pub async fn swap(&self, req: SwapRequest) -> Result<SwapResponse> {
        let output_total = self.validate_outputs(&req.outputs)?;
        // Verify before spending so an unbalanced request doesn't burn inputs.
        {
            let secrets: Vec<String> = req.inputs.iter().map(|p| p.secret.clone()).collect();
            if self.spent.any_spent(&secrets)? {
                return Err(MintError::TokenAlreadySpent);
            }
        }
        let input_total: u64 = req.inputs.iter().map(|p| p.amount).sum();
        if req.inputs.is_empty() {
            return Err(MintError::InvalidRequest("no inputs provided".into()));
        }
        if input_total != output_total {
            return Err(MintError::Unbalanced {
                inputs: input_total,
                outputs: output_total,
            });
        }
        if self.policy_enforced {
            self.policy.can_sign_swap(&SwapSignRequest {
                input_total_sat: input_total,
                outputs: &req.outputs,
                keyset_id: &self.active_keyset_id(),
            })?;
        }
        self.verify_and_spend_inputs(&req.inputs).await?;
        let signatures = self.signer.blind_sign(&req.outputs).await?;
        Ok(SwapResponse { signatures })
    }

    // -- Ark extensions ---------------------------------------------------------

    pub async fn token_vtxo(&self, token_id: &str) -> Result<TokenVtxoResponse> {
        let mapping = self
            .inventory
            .get_mapping(token_id)?
            .ok_or_else(|| MintError::MappingNotFound(token_id.to_string()))?;
        let record = self
            .inventory
            .get_vtxo(&mapping.vtxo_id)?
            .ok_or_else(|| MintError::MappingNotFound(mapping.vtxo_id.clone()))?;
        Ok(TokenVtxoResponse {
            token_id: token_id.to_string(),
            vtxo: record.vtxo,
            status: record.status,
        })
    }

    pub async fn unilateral_exit(&self, token_id: &str) -> Result<ExitResponse> {
        let mapping = self
            .inventory
            .get_mapping(token_id)?
            .ok_or_else(|| MintError::MappingNotFound(token_id.to_string()))?;
        let record = self
            .inventory
            .get_vtxo(&mapping.vtxo_id)?
            .ok_or_else(|| MintError::MappingNotFound(mapping.vtxo_id.clone()))?;

        let exit = self.ark.unilateral_exit(&record.vtxo).await?;
        self.inventory
            .set_status(&mapping.vtxo_id, crate::types::VtxoStatus::Exited)?;
        self.inventory.release_vtxo_mapping(token_id)?;

        let txid = exit
            .claim_txid
            .clone()
            .unwrap_or_else(|| exit.exit_txid.clone());

        Ok(ExitResponse {
            token_id: token_id.to_string(),
            vtxo_id: mapping.vtxo_id,
            txid,
            phase: exit.phase,
            claim_txid: exit.claim_txid,
        })
    }

    pub async fn refresh_status(&self) -> Result<RefreshStatusResponse> {
        let height = self.ark.current_block_height().await?;
        let threshold = self.config.ark.refresh_threshold_blocks;
        let queue = self.inventory.get_refresh_queue(height, threshold)?;
        Ok(RefreshStatusResponse {
            pending_refreshes: queue.len(),
            next_expiry_height: self.inventory.next_expiry_height()?,
            refresh_threshold_blocks: threshold,
            current_block_height: height,
        })
    }

    // -- Transparency + PoL ---------------------------------------------------

    pub async fn transparency_summary(&self) -> Result<TransparencySummary> {
        let pol = self.pol.status(&self.active_keyset_id())?;
        let height = self.ark.current_block_height().await?;
        let threshold = self.config.ark.refresh_threshold_blocks;
        let refresh_pending = self.inventory.get_refresh_queue(height, threshold)?.len();
        let active_vtxo_msat = self.inventory.total_active_vtxo_msat()?;
        let allocated_vtxo_msat = self.inventory.total_allocated_msat()?;
        let free_vtxo_msat = self.inventory.free_reserve_msat()?;
        let outstanding_msat = pol.outstanding_sat * MSAT_PER_SAT;
        let solvency_ok = allocated_vtxo_msat >= outstanding_msat;

        Ok(TransparencySummary {
            mint_name: self.config.mint.name.clone(),
            mint_url: self.config.mint.url.clone(),
            version: format!("minerva-mint/{}", env!("CARGO_PKG_VERSION")),
            git_commit: option_env!("MINERVA_GIT_COMMIT").map(str::to_string),
            active_keysets: vec![self.active_keyset_id()],
            vtxo_verify_mode: self.config.trust.vtxo_verify_mode.clone(),
            signatory_policy_enforced: self.policy_enforced,
            outstanding_ecash_sat: pol.outstanding_sat,
            active_vtxo_msat,
            allocated_vtxo_msat,
            free_vtxo_msat,
            pol_current_epoch: pol.current_epoch_day,
            pol_last_closed_epoch: pol.last_closed_epoch,
            pol_last_closed_root: pol.last_closed_root,
            pol_last_ots_epoch: pol.last_ots_stamped_epoch,
            pol_last_ots_stamped_at: pol.last_ots_stamped_at,
            ots_enabled: self.config.trust.ots.enabled,
            refresh_pending,
            next_vtxo_expiry_height: self.inventory.next_expiry_height()?,
            solvency_ok,
        })
    }

    pub fn pol_status(&self) -> Result<PolStatusResponse> {
        let keyset_id = self.active_keyset_id();
        let s = self.pol.status(&keyset_id)?;
        Ok(PolStatusResponse {
            active_keyset_id: keyset_id,
            current_epoch_day: s.current_epoch_day,
            open_mint_total_sat: s.open_mint_total_sat,
            open_burn_total_sat: s.open_burn_total_sat,
            outstanding_sat: s.outstanding_sat,
            last_closed_epoch: s.last_closed_epoch,
            last_closed_root: s.last_closed_root,
            last_ots_stamped_epoch: s.last_ots_stamped_epoch,
            last_ots_stamped_at: s.last_ots_stamped_at,
            ots_enabled: self.config.trust.ots.enabled,
        })
    }

    pub async fn stamp_epoch_ots(&self, epoch_day: &str) -> Result<()> {
        if !self.config.trust.ots.enabled {
            return Ok(());
        }
        let Some(ots) = &self.ots else {
            tracing::warn!("OTS enabled but no stamper configured");
            return Ok(());
        };
        let keyset_id = self.active_keyset_id();
        if self.pol.has_ots_stamp(epoch_day, &keyset_id)? {
            return Ok(());
        }
        let Some(root) = self.pol.epoch_root_hash(epoch_day, &keyset_id)? else {
            return Ok(());
        };
        let digest = digest_from_root_hex(&root)?;
        let stamp = ots.stamp_digest(digest).await?;
        self.pol
            .save_ots_stamp(epoch_day, &keyset_id, &stamp.proof_hex, &stamp.calendar_url)?;
        tracing::info!(epoch = %epoch_day, calendar = %stamp.calendar_url, "PoL epoch stamped with OpenTimestamps");
        Ok(())
    }

    pub async fn stamp_pending_ots_epochs(&self) -> Result<usize> {
        let pending = self.pol.epochs_pending_ots(&self.active_keyset_id())?;
        let mut done = 0;
        for day in pending {
            if self.stamp_epoch_ots(&day).await.is_ok() {
                done += 1;
            }
        }
        Ok(done)
    }

    pub fn pol_ots_proof(&self, epoch_day: &str) -> Result<PolOtsResponse> {
        let keyset_id = self.active_keyset_id();
        let root = self
            .pol
            .epoch_root_hash(epoch_day, &keyset_id)?
            .ok_or_else(|| MintError::InvalidRequest(format!("unknown epoch {epoch_day}")))?;
        let (proof_hex, calendar_url) = self
            .pol
            .ots_proof(epoch_day, &keyset_id)?
            .ok_or_else(|| MintError::InvalidRequest(format!("no OTS proof for {epoch_day}")))?;
        let roots = self.pol.roots_for_keyset(&keyset_id)?;
        let stamped_at = roots
            .iter()
            .find(|r| r.epoch_day == epoch_day)
            .and_then(|r| r.ots_stamped_at)
            .unwrap_or(0);
        Ok(PolOtsResponse {
            epoch_day: epoch_day.to_string(),
            root_hash: root,
            proof_hex,
            calendar_url,
            stamped_at,
        })
    }

    pub fn pol_roots(&self, keyset_id: &str) -> Result<PolRootsResponse> {
        let roots = self
            .pol
            .roots_for_keyset(keyset_id)?
            .into_iter()
            .map(|r| PolEpochRootResponse {
                epoch_day: r.epoch_day,
                mint_total_sat: r.mint_total_sat,
                burn_total_sat: r.burn_total_sat,
                outstanding_sat: r.outstanding_sat,
                root_hash: r.root_hash,
                prev_hash: r.prev_hash,
                ots_stamped: r.ots_proof_hex.is_some(),
                ots_calendar_url: r.ots_calendar_url,
                ots_stamped_at: r.ots_stamped_at,
            })
            .collect();
        Ok(PolRootsResponse {
            keyset_id: keyset_id.to_string(),
            roots,
        })
    }
}

/// Decompose `amount` into ascending powers of two (NUT-00 output amounts).
fn decompose_pow2(mut amount: u64) -> Vec<u64> {
    let mut parts = Vec::new();
    let mut bit = 0;
    while amount > 0 {
        if amount & 1 == 1 {
            parts.push(1 << bit);
        }
        amount >>= 1;
        bit += 1;
    }
    parts
}

/// Map the internal melt reservation state onto the public NUT-05 state.
/// RESERVED reports PENDING (inputs locked, payout unresolved); SETTLED
/// reports PAID; RELEASED reports UNPAID because the inputs are spendable
/// again, though the released quote itself is terminal and a retry needs a
/// fresh quote.
fn public_melt_state(state: MeltQuoteState) -> QuoteState {
    match state {
        MeltQuoteState::Unpaid | MeltQuoteState::Released => QuoteState::Unpaid,
        MeltQuoteState::Reserved => QuoteState::Pending,
        MeltQuoteState::Settled => QuoteState::Paid,
    }
}

/// A payout error is safe to release only when the wallet observed the
/// current attempt's terminal "failed" state ([`MintError::PaymentFailed`]).
/// Anything else - timeouts, transport errors, ambiguous ASP responses,
/// history rows that cannot be scoped to this attempt - leaves the outcome
/// unknown, and an unknown outcome is never treated as success or as a clean
/// failure.
fn payout_error_is_releasable(e: &MintError) -> bool {
    matches!(e, MintError::PaymentFailed(_))
}

/// Verify that a hex preimage matches a melt quote's stored payment hash
/// (sha256). Operator settle-reconciliation must not settle a quote whose
/// payment cannot be proven. Quotes without a payment hash (mock payout
/// mode) cannot be checked this way and pass.
pub fn verify_melt_preimage(preimage_hex: &str, payment_hash: Option<&str>) -> Result<()> {
    let Some(hash) = payment_hash else {
        return Ok(());
    };
    let bytes = hex::decode(preimage_hex)
        .map_err(|e| MintError::InvalidRequest(format!("invalid preimage hex: {e}")))?;
    let digest = Sha256::digest(&bytes);
    if !hex::encode(digest).eq_ignore_ascii_case(hash) {
        return Err(MintError::InvalidRequest(
            "preimage does not match the quote's payment hash".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ark_client::MockArkClient;
    use crate::blind_signer::build_blind_signer;
    use crate::receipts::{verify_receipt, ReceiptSigner, ReceiptStore};
    use crate::spent_store::SpentSecretStore;

    fn test_config() -> AppConfig {
        let raw = include_str!("../config.toml");
        toml::from_str(raw).unwrap()
    }

    fn backend() -> MintBackend {
        let config = test_config();
        let ark = Arc::new(MockArkClient::new(config.ark.default_vtxo_expiry));
        let signer = build_blind_signer(&config.signatory).unwrap();
        let inventory = VtxoInventory::open_in_memory().unwrap();
        let pol = PolLedger::open_in_memory().unwrap();
        let spent = SpentSecretStore::open_in_memory().unwrap();
        MintBackend::new(
            config,
            ark,
            signer,
            inventory,
            pol,
            spent,
            ReceiptStore::open_in_memory().unwrap(),
            ReceiptSigner::dev(),
            None,
        )
    }

    /// Decompose `amount` into powers of two per NUT-00. `tag` keeps blinded
    /// messages (and thus mock signatures / derived secrets) distinct across
    /// separate issuances in a test.
    fn outputs_for(amount: u64, tag: &str) -> Vec<BlindedMessage> {
        let mut out = Vec::new();
        let mut rem = amount;
        let mut bit = 0;
        while rem > 0 {
            if rem & 1 == 1 {
                out.push(BlindedMessage {
                    amount: 1 << bit,
                    id: KEYSET_ID.to_string(),
                    b: format!("02{tag}b{}{}", bit, "ab".repeat(16)),
                });
            }
            rem >>= 1;
            bit += 1;
        }
        out
    }

    fn proofs_from(sigs: &[BlindSignature]) -> Vec<Proof> {
        sigs.iter()
            .enumerate()
            .map(|(i, s)| Proof {
                amount: s.amount,
                id: s.id.clone(),
                secret: format!("secret-{i}-{}", s.c),
                c: s.c.clone(),
            })
            .collect()
    }

    #[tokio::test]
    async fn full_mint_swap_melt_flow() {
        let b = backend();

        // 1. Mint quote + issue.
        let quote = b
            .mint_quote(MintQuoteBolt11Request {
                amount: 1000,
                unit: "sat".into(),
            })
            .await
            .unwrap();
        assert_eq!(quote.state, QuoteState::Paid);

        let minted = b
            .mint(MintBolt11Request {
                quote: quote.quote.clone(),
                outputs: outputs_for(1000, "mint"),
            })
            .await
            .unwrap();
        assert_eq!(
            minted.signatures.iter().map(|s| s.amount).sum::<u64>(),
            1000
        );

        // Token batch is mapped to a backing VTXO.
        let mapped = b.token_vtxo(&quote.quote).await.unwrap();
        assert_eq!(mapped.vtxo.amount_msat >= 1_000_000, true);

        // Double-issue rejected.
        let err = b
            .mint(MintBolt11Request {
                quote: quote.quote.clone(),
                outputs: outputs_for(1000, "mint"),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::QuoteAlreadyIssued(_)));

        // 2. Swap: refresh proofs 1:1, no Ark involvement.
        let proofs = proofs_from(&minted.signatures);
        let swapped = b
            .swap(SwapRequest {
                inputs: proofs.clone(),
                outputs: outputs_for(1000, "swap"),
            })
            .await
            .unwrap();
        assert_eq!(
            swapped.signatures.iter().map(|s| s.amount).sum::<u64>(),
            1000
        );

        // Old proofs are now spent.
        let err = b
            .swap(SwapRequest {
                inputs: proofs,
                outputs: outputs_for(1000, "swap2"),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::TokenAlreadySpent));

        // 3. Melt with the new proofs. Mock mode uses scaffold amount from
        // request length: "lnbc1" -> 5 * 100 = 500 sat + fee reserve.
        let melt_quote = b
            .melt_quote(MeltQuoteBolt11Request {
                request: "lnbc1".into(),
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let needed = melt_quote.amount + melt_quote.fee_reserve;
        let new_proofs = proofs_from(&swapped.signatures);
        let total: u64 = new_proofs.iter().map(|p| p.amount).sum();
        assert!(total >= needed, "test proofs must cover melt amount");

        let melted = b
            .melt(MeltBolt11Request {
                quote: melt_quote.quote.clone(),
                inputs: new_proofs,
                token_ids: None,
                outputs: None,
            })
            .await
            .unwrap();
        assert_eq!(melted.state, QuoteState::Paid);
        assert!(melted.payment_preimage.is_some());
    }

    #[tokio::test]
    async fn mint_auto_boards_liquidity() {
        let b = backend();
        assert_eq!(b.inventory().free_reserve_msat().unwrap(), 0);

        let quote = b
            .mint_quote(MintQuoteBolt11Request {
                amount: 64,
                unit: "sat".into(),
            })
            .await
            .unwrap();
        b.mint(MintBolt11Request {
            quote: quote.quote,
            outputs: outputs_for(64, "board"),
        })
        .await
        .unwrap();

        // Boarding happened: reserve was created and 64k msat allocated.
        let free = b.inventory().free_reserve_msat().unwrap();
        assert!(free > 0);
    }

    #[tokio::test]
    async fn unbalanced_mint_rejected() {
        let b = backend();
        let quote = b
            .mint_quote(MintQuoteBolt11Request {
                amount: 100,
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let err = b
            .mint(MintBolt11Request {
                quote: quote.quote,
                outputs: outputs_for(99, "unbal"),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::Unbalanced { .. }));
    }

    #[tokio::test]
    async fn exit_releases_mapping() {
        let b = backend();
        let quote = b
            .mint_quote(MintQuoteBolt11Request {
                amount: 32,
                unit: "sat".into(),
            })
            .await
            .unwrap();
        b.mint(MintBolt11Request {
            quote: quote.quote.clone(),
            outputs: outputs_for(32, "exit"),
        })
        .await
        .unwrap();

        let exit = b.unilateral_exit(&quote.quote).await.unwrap();
        assert_eq!(exit.txid.len(), 64);

        // Mapping is gone afterwards.
        let err = b.token_vtxo(&quote.quote).await.unwrap_err();
        assert!(matches!(err, MintError::MappingNotFound(_)));
    }

    #[tokio::test]
    async fn transparency_reports_solvency() {
        let b = backend();
        let quote = b
            .mint_quote(MintQuoteBolt11Request {
                amount: 128,
                unit: "sat".into(),
            })
            .await
            .unwrap();
        b.mint(MintBolt11Request {
            quote: quote.quote.clone(),
            outputs: outputs_for(128, "tr"),
        })
        .await
        .unwrap();

        let summary = b.transparency_summary().await.unwrap();
        assert_eq!(summary.outstanding_ecash_sat, 128);
        assert!(summary.allocated_vtxo_msat >= 128_000);
        assert!(summary.solvency_ok);
        assert_eq!(summary.vtxo_verify_mode, "scaffold");
    }

    #[tokio::test]
    async fn melt_releases_backing_with_token_ids() {
        let mut config = test_config();
        config.trust.release_backing_on_melt = true;
        let ark = Arc::new(MockArkClient::new(config.ark.default_vtxo_expiry));
        let signer = build_blind_signer(&config.signatory).unwrap();
        let b = MintBackend::new(
            config,
            ark,
            signer,
            VtxoInventory::open_in_memory().unwrap(),
            PolLedger::open_in_memory().unwrap(),
            SpentSecretStore::open_in_memory().unwrap(),
            ReceiptStore::open_in_memory().unwrap(),
            ReceiptSigner::dev(),
            None,
        );

        let quote_a = b
            .mint_quote(MintQuoteBolt11Request {
                amount: 64,
                unit: "sat".into(),
            })
            .await
            .unwrap();
        b.mint(MintBolt11Request {
            quote: quote_a.quote.clone(),
            outputs: outputs_for(64, "a"),
        })
        .await
        .unwrap();

        let quote_b = b
            .mint_quote(MintQuoteBolt11Request {
                amount: 512,
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let minted_b = b
            .mint(MintBolt11Request {
                quote: quote_b.quote.clone(),
                outputs: outputs_for(512, "b"),
            })
            .await
            .unwrap();

        let melt_quote = b
            .melt_quote(MeltQuoteBolt11Request {
                request: "lnbc1".into(),
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let needed = melt_quote.amount + melt_quote.fee_reserve;
        let proofs = proofs_from(&minted_b.signatures);
        assert!(proofs.iter().map(|p| p.amount).sum::<u64>() >= needed);

        b.melt(MeltBolt11Request {
            quote: melt_quote.quote,
            inputs: proofs,
            token_ids: Some(vec![quote_b.quote.clone()]),
            outputs: None,
        })
        .await
        .unwrap();

        assert!(b.token_vtxo(&quote_b.quote).await.is_err());
        assert!(b.token_vtxo(&quote_a.quote).await.is_ok());
    }

    #[tokio::test]
    async fn spent_secrets_persist_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mint.sqlite");
        let config = test_config();
        let ark = Arc::new(MockArkClient::new(config.ark.default_vtxo_expiry));
        let signer = build_blind_signer(&config.signatory).unwrap();

        let b = MintBackend::new(
            config.clone(),
            ark.clone(),
            signer.clone(),
            VtxoInventory::open(&db).unwrap(),
            PolLedger::open(&db).unwrap(),
            SpentSecretStore::open(&db).unwrap(),
            ReceiptStore::open_in_memory().unwrap(),
            ReceiptSigner::dev(),
            None,
        );

        let quote = b
            .mint_quote(MintQuoteBolt11Request {
                amount: 64,
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let minted = b
            .mint(MintBolt11Request {
                quote: quote.quote,
                outputs: outputs_for(64, "persist"),
            })
            .await
            .unwrap();
        let proofs = proofs_from(&minted.signatures);

        b.swap(SwapRequest {
            inputs: proofs.clone(),
            outputs: outputs_for(64, "persist2"),
        })
        .await
        .unwrap();

        drop(b);

        let b2 = MintBackend::new(
            config,
            ark,
            signer,
            VtxoInventory::open(&db).unwrap(),
            PolLedger::open(&db).unwrap(),
            SpentSecretStore::open(&db).unwrap(),
            ReceiptStore::open_in_memory().unwrap(),
            ReceiptSigner::dev(),
            None,
        );
        let err = b2
            .swap(SwapRequest {
                inputs: proofs,
                outputs: outputs_for(64, "replay"),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::TokenAlreadySpent));
    }

    #[tokio::test]
    async fn stamp_epoch_ots_with_mock() {
        let config = test_config();
        let ark = Arc::new(MockArkClient::new(config.ark.default_vtxo_expiry));
        let inventory = VtxoInventory::open_in_memory().unwrap();
        let pol = PolLedger::open_in_memory().unwrap();
        let signer = build_blind_signer(&config.signatory).unwrap();
        let spent = SpentSecretStore::open_in_memory().unwrap();
        let b = MintBackend::new(
            config,
            ark,
            signer,
            inventory,
            pol,
            spent,
            ReceiptStore::open_in_memory().unwrap(),
            ReceiptSigner::dev(),
            Some(Arc::new(crate::ots::MockOtsStamper)),
        );
        let day = PolLedger::current_epoch_day();
        b.pol()
            .record_mint(KEYSET_ID, "q1", 8, &["02b".into()])
            .unwrap();
        b.pol().close_epoch(&day, KEYSET_ID).unwrap();
        b.stamp_epoch_ots(&day).await.unwrap();
        assert!(b.pol().has_ots_stamp(&day, KEYSET_ID).unwrap());
    }

    // -- Reserve / settle / release (docs/reserve-settle-receipts.md) --------

    /// Ark client whose Lightning payout always fails: either definitively
    /// (terminal "failed" state, as the wallet reports it) or ambiguously
    /// (timeout, outcome unknown).
    struct FailingPayoutArk {
        inner: MockArkClient,
        definitive: bool,
    }

    #[async_trait::async_trait]
    impl ArkClient for FailingPayoutArk {
        async fn board_sats(&self, amount_msat: u64) -> Result<Vtxo> {
            self.inner.board_sats(amount_msat).await
        }
        async fn refresh_vtxo(&self, vtxo: &Vtxo) -> Result<Vtxo> {
            self.inner.refresh_vtxo(vtxo).await
        }
        async fn unilateral_exit(&self, vtxo: &Vtxo) -> Result<ExitResult> {
            self.inner.unilateral_exit(vtxo).await
        }
        async fn get_vtxo_expiry(&self, vtxo: &Vtxo) -> Result<std::time::Duration> {
            self.inner.get_vtxo_expiry(vtxo).await
        }
        async fn current_block_height(&self) -> Result<u64> {
            self.inner.current_block_height().await
        }
        async fn ping(&self) -> Result<()> {
            self.inner.ping().await
        }
        async fn estimate_lightning_send_fee_sat(&self, amount_sat: u64) -> Result<u64> {
            self.inner.estimate_lightning_send_fee_sat(amount_sat).await
        }
        async fn pay_lightning_invoice(
            &self,
            invoice: &str,
            _amount_sat: u64,
            _attempted_before: bool,
        ) -> Result<PayoutResult> {
            let hash = crate::bolt11_util::decode_invoice(invoice)
                .map(|(_, h)| h)
                .unwrap_or_else(|_| invoice.to_string());
            if self.definitive {
                Err(MintError::PaymentFailed(format!(
                    "lightning payment {hash} failed"
                )))
            } else {
                Err(MintError::Ark(format!(
                    "timed out waiting for lightning payment status ({hash})"
                )))
            }
        }
    }

    /// Live payout mode (real bolt11 invoices, payout via the Ark client).
    fn live_config() -> AppConfig {
        let mut config = test_config();
        config.melt.backend = "barkd".into();
        config
    }

    fn live_backend(definitive: bool) -> MintBackend {
        let config = live_config();
        let ark = Arc::new(FailingPayoutArk {
            inner: MockArkClient::new(config.ark.default_vtxo_expiry),
            definitive,
        });
        let signer = build_blind_signer(&config.signatory).unwrap();
        MintBackend::new(
            config,
            ark,
            signer,
            VtxoInventory::open_in_memory().unwrap(),
            PolLedger::open_in_memory().unwrap(),
            SpentSecretStore::open_in_memory().unwrap(),
            ReceiptStore::open_in_memory().unwrap(),
            ReceiptSigner::dev(),
            None,
        )
    }

    /// Fixed test payment: preimage whose sha256 is the invoice's payment
    /// hash, so reconcile-settle can be tested with a verifiable preimage.
    const TEST_PREIMAGE: &[u8; 32] = b"minerva reserve-settle test!!!!!";
    const _: () = assert!(TEST_PREIMAGE.len() == 32);

    /// A real, signed regtest bolt11 invoice so live-mode quote validation passes.
    fn test_invoice(amount_sat: u64) -> String {
        use cdk_common::bitcoin::hashes::{sha256, Hash};
        crate::bolt11_util::signed_test_invoice(
            amount_sat,
            sha256::Hash::hash(TEST_PREIMAGE).to_byte_array(),
        )
    }

    /// Mint 1024 sat of proofs against `b`.
    async fn mint_proofs(b: &MintBackend, tag: &str) -> Vec<Proof> {
        let quote = b
            .mint_quote(MintQuoteBolt11Request {
                amount: 1024,
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let minted = b
            .mint(MintBolt11Request {
                quote: quote.quote,
                outputs: outputs_for(1024, tag),
            })
            .await
            .unwrap();
        proofs_from(&minted.signatures)
    }

    async fn melt_quote_500(b: &MintBackend) -> MeltQuoteBolt11Response {
        b.melt_quote(MeltQuoteBolt11Request {
            request: test_invoice(500),
            unit: "sat".into(),
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn melt_payout_failure_releases_inputs() {
        let b = live_backend(true);
        let proofs = mint_proofs(&b, "relm").await;
        let melt_quote = melt_quote_500(&b).await;

        let err = b
            .melt(MeltBolt11Request {
                quote: melt_quote.quote.clone(),
                inputs: proofs.clone(),
                token_ids: None,
                outputs: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::PaymentFailed(_)));

        // The quote is terminally released: it reports UNPAID (inputs are
        // spendable again) but cannot itself be re-melted.
        let q = b.get_melt_quote(&melt_quote.quote).await.unwrap();
        assert_eq!(q.state, QuoteState::Unpaid);
        let err = b
            .melt(MeltBolt11Request {
                quote: melt_quote.quote.clone(),
                inputs: proofs.clone(),
                token_ids: None,
                outputs: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::InvalidRequest(_)));

        // Nothing was burned: outstanding liability is unchanged.
        assert_eq!(b.pol_status().unwrap().outstanding_sat, 1024);

        // The inputs were released and can be swapped.
        let swapped = b
            .swap(SwapRequest {
                inputs: proofs,
                outputs: outputs_for(1024, "relm2"),
            })
            .await
            .unwrap();
        assert_eq!(
            swapped.signatures.iter().map(|s| s.amount).sum::<u64>(),
            1024
        );
    }

    #[tokio::test]
    async fn melt_unknown_outcome_holds_reservation_until_reconciled() {
        let b = live_backend(false);
        let proofs = mint_proofs(&b, "unkm").await;
        let melt_quote = melt_quote_500(&b).await;

        let err = b
            .melt(MeltBolt11Request {
                quote: melt_quote.quote.clone(),
                inputs: proofs.clone(),
                token_ids: None,
                outputs: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::Ark(_)));

        // Unknown outcome: the quote reports PENDING and the inputs stay
        // locked - neither charged nor released.
        let q = b.get_melt_quote(&melt_quote.quote).await.unwrap();
        assert_eq!(q.state, QuoteState::Pending);
        let err = b
            .swap(SwapRequest {
                inputs: proofs.clone(),
                outputs: outputs_for(1024, "unkm2"),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::TokenAlreadySpent));
        let err = b
            .melt(MeltBolt11Request {
                quote: melt_quote.quote.clone(),
                inputs: proofs.clone(),
                token_ids: None,
                outputs: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::InvalidRequest(_)));
        assert_eq!(b.pol_status().unwrap().outstanding_sat, 1024);

        // After (simulated) out-of-band verification that the payout
        // definitively failed, the operator releases the reservation.
        b.reconcile_release_reserved_melt(&melt_quote.quote).unwrap();
        let q = b.get_melt_quote(&melt_quote.quote).await.unwrap();
        assert_eq!(q.state, QuoteState::Unpaid);
        let swapped = b
            .swap(SwapRequest {
                inputs: proofs,
                outputs: outputs_for(1024, "unkm3"),
            })
            .await
            .unwrap();
        assert_eq!(
            swapped.signatures.iter().map(|s| s.amount).sum::<u64>(),
            1024
        );
    }

    #[tokio::test]
    async fn reserved_melt_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mint.sqlite");
        let config = live_config();
        let ark = Arc::new(FailingPayoutArk {
            inner: MockArkClient::new(config.ark.default_vtxo_expiry),
            definitive: false,
        });
        let signer = build_blind_signer(&config.signatory).unwrap();

        let (melt_quote_id, proofs);
        {
            let b = MintBackend::new(
                config.clone(),
                ark.clone(),
                signer.clone(),
                VtxoInventory::open(&db).unwrap(),
                PolLedger::open(&db).unwrap(),
                SpentSecretStore::open(&db).unwrap(),
                ReceiptStore::open_in_memory().unwrap(),
                ReceiptSigner::dev(),
                None,
            );
            proofs = mint_proofs(&b, "rst").await;
            let melt_quote = melt_quote_500(&b).await;
            melt_quote_id = melt_quote.quote.clone();
            let err = b
                .melt(MeltBolt11Request {
                    quote: melt_quote.quote,
                    inputs: proofs.clone(),
                    token_ids: None,
                    outputs: None,
                })
                .await
                .unwrap_err();
            assert!(matches!(err, MintError::Ark(_)));
        }

        // After a restart the quote and its reservation are still there.
        let b2 = MintBackend::new(
            config,
            ark,
            signer,
            VtxoInventory::open(&db).unwrap(),
            PolLedger::open(&db).unwrap(),
            SpentSecretStore::open(&db).unwrap(),
            ReceiptStore::open_in_memory().unwrap(),
            ReceiptSigner::dev(),
            None,
        );
        let q = b2.get_melt_quote(&melt_quote_id).await.unwrap();
        assert_eq!(q.state, QuoteState::Pending);
        assert_eq!(q.amount, 500);
        let err = b2
            .swap(SwapRequest {
                inputs: proofs.clone(),
                outputs: outputs_for(1024, "rst2"),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::TokenAlreadySpent));

        b2.reconcile_release_reserved_melt(&melt_quote_id).unwrap();
        b2.swap(SwapRequest {
            inputs: proofs,
            outputs: outputs_for(1024, "rst3"),
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn successful_melt_settles_persistently() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mint.sqlite");
        let config = test_config(); // mock payout always succeeds
        let ark = Arc::new(MockArkClient::new(config.ark.default_vtxo_expiry));
        let signer = build_blind_signer(&config.signatory).unwrap();
        let b = MintBackend::new(
            config,
            ark,
            signer,
            VtxoInventory::open(&db).unwrap(),
            PolLedger::open(&db).unwrap(),
            SpentSecretStore::open(&db).unwrap(),
            ReceiptStore::open_in_memory().unwrap(),
            ReceiptSigner::dev(),
            None,
        );

        let proofs = mint_proofs(&b, "stl").await;
        let melt_quote = b
            .melt_quote(MeltQuoteBolt11Request {
                request: "lnbc1".into(),
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let melted = b
            .melt(MeltBolt11Request {
                quote: melt_quote.quote.clone(),
                inputs: proofs.clone(),
                token_ids: None,
                outputs: None,
            })
            .await
            .unwrap();
        assert_eq!(melted.state, QuoteState::Paid);
        let preimage = melted.payment_preimage.clone().unwrap();

        // Settled quote reports PAID across a restart; inputs stay burned;
        // PoL outstanding reflects the melt.
        let b2 = MintBackend::new(
            test_config(),
            Arc::new(MockArkClient::new(144)),
            build_blind_signer(&test_config().signatory).unwrap(),
            VtxoInventory::open(&db).unwrap(),
            PolLedger::open(&db).unwrap(),
            SpentSecretStore::open(&db).unwrap(),
            ReceiptStore::open_in_memory().unwrap(),
            ReceiptSigner::dev(),
            None,
        );
        let q = b2.get_melt_quote(&melt_quote.quote).await.unwrap();
        assert_eq!(q.state, QuoteState::Paid);
        let err = b2
            .swap(SwapRequest {
                inputs: proofs,
                outputs: outputs_for(1024, "stl2"),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::TokenAlreadySpent));
        // record_melt_burns burns the full input set; no blank outputs were
        // provided, so no NUT-08 change is minted back.
        assert_eq!(b2.pol_status().unwrap().outstanding_sat, 0);
        let _ = preimage;
    }

    #[tokio::test]
    async fn reconcile_settle_completes_melt_after_crash() {
        // A payout that succeeded but crashed/errored before the settle
        // commit leaves the quote RESERVED; the operator settles it with the
        // payment's preimage.
        let b = live_backend(false);
        let proofs = mint_proofs(&b, "crs").await;
        let melt_quote = melt_quote_500(&b).await;
        let _ = b
            .melt(MeltBolt11Request {
                quote: melt_quote.quote.clone(),
                inputs: proofs.clone(),
                token_ids: None,
                outputs: None,
            })
            .await
            .unwrap_err();
        assert_eq!(
            b.get_melt_quote(&melt_quote.quote).await.unwrap().state,
            QuoteState::Pending
        );
        assert_eq!(b.list_reserved_melts().unwrap().len(), 1);

        // A wrong preimage is rejected and changes nothing.
        let err = b
            .reconcile_settle_reserved_melt(&melt_quote.quote, &hex::encode([0u8; 32]))
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::InvalidRequest(_)));
        assert_eq!(
            b.get_melt_quote(&melt_quote.quote).await.unwrap().state,
            QuoteState::Pending
        );

        // The real preimage settles: quote reports PAID with the preimage
        // served, inputs are burned, PoL reflects the burn.
        b.reconcile_settle_reserved_melt(&melt_quote.quote, &hex::encode(TEST_PREIMAGE))
            .await
            .unwrap();
        let q = b.get_melt_quote(&melt_quote.quote).await.unwrap();
        assert_eq!(q.state, QuoteState::Paid);
        assert_eq!(q.payment_preimage, Some(hex::encode(TEST_PREIMAGE)));
        assert!(b.list_reserved_melts().unwrap().is_empty());
        let err = b
            .swap(SwapRequest {
                inputs: proofs,
                outputs: outputs_for(1024, "crs2"),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::TokenAlreadySpent));
        assert_eq!(b.pol_status().unwrap().outstanding_sat, 0);
    }

    #[tokio::test]
    async fn one_melt_in_flight_per_invoice() {
        let b = live_backend(false); // payout outcome stays unknown
        let proofs_a = mint_proofs(&b, "ifa").await;
        let proofs_b = mint_proofs(&b, "ifb").await;
        let invoice = test_invoice(500);
        let quote_a = b
            .melt_quote(MeltQuoteBolt11Request {
                request: invoice.clone(),
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let quote_b = b
            .melt_quote(MeltQuoteBolt11Request {
                request: invoice,
                unit: "sat".into(),
            })
            .await
            .unwrap();

        let _ = b
            .melt(MeltBolt11Request {
                quote: quote_a.quote.clone(),
                inputs: proofs_a,
                token_ids: None,
                outputs: None,
            })
            .await
            .unwrap_err();
        assert_eq!(
            b.get_melt_quote(&quote_a.quote).await.unwrap().state,
            QuoteState::Pending
        );

        // A second melt for the same invoice is refused while the first is
        // in flight; quote B and its inputs are untouched.
        let err = b
            .melt(MeltBolt11Request {
                quote: quote_b.quote.clone(),
                inputs: proofs_b.clone(),
                token_ids: None,
                outputs: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::InvalidRequest(_)));
        assert_eq!(
            b.get_melt_quote(&quote_b.quote).await.unwrap().state,
            QuoteState::Unpaid
        );
        b.swap(SwapRequest {
            inputs: proofs_b,
            outputs: outputs_for(1024, "ifb2"),
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn requote_of_settled_invoice_rejected() {
        // Live payout mode with a succeeding (mock) Ark payout: the melt
        // settles, and re-quoting the same invoice is refused.
        let config = live_config();
        let ark = Arc::new(MockArkClient::new(config.ark.default_vtxo_expiry));
        let signer = build_blind_signer(&config.signatory).unwrap();
        let b = MintBackend::new(
            config,
            ark,
            signer,
            VtxoInventory::open_in_memory().unwrap(),
            PolLedger::open_in_memory().unwrap(),
            SpentSecretStore::open_in_memory().unwrap(),
            ReceiptStore::open_in_memory().unwrap(),
            ReceiptSigner::dev(),
            None,
        );
        let proofs = mint_proofs(&b, "stlq").await;
        let invoice = test_invoice(500);
        let quote = b
            .melt_quote(MeltQuoteBolt11Request {
                request: invoice.clone(),
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let melted = b
            .melt(MeltBolt11Request {
                quote: quote.quote,
                inputs: proofs,
                token_ids: None,
                outputs: None,
            })
            .await
            .unwrap();
        assert_eq!(melted.state, QuoteState::Paid);

        let err = b
            .melt_quote(MeltQuoteBolt11Request {
                request: invoice,
                unit: "sat".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn concurrent_melts_and_swaps_on_shared_file_db() {
        // Several stores share one SQLite file over separate connections;
        // concurrent melts and swaps must not deadlock or die with
        // SQLITE_BUSY mid-settle.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mint.sqlite");
        let config = test_config();
        let ark = Arc::new(MockArkClient::new(config.ark.default_vtxo_expiry));
        let signer = build_blind_signer(&config.signatory).unwrap();
        let b = Arc::new(MintBackend::new(
            config,
            ark,
            signer,
            VtxoInventory::open(&db).unwrap(),
            PolLedger::open(&db).unwrap(),
            SpentSecretStore::open(&db).unwrap(),
            ReceiptStore::open_in_memory().unwrap(),
            ReceiptSigner::dev(),
            None,
        ));

        let mut handles = Vec::new();
        for i in 0..4 {
            let b = b.clone();
            handles.push(tokio::spawn(async move {
                let tag = format!("cc{i}");
                let proofs = mint_proofs(&b, &tag).await;
                let quote = b
                    .melt_quote(MeltQuoteBolt11Request {
                        request: format!("lnbc1{i}"),
                        unit: "sat".into(),
                    })
                    .await
                    .unwrap();
                b.melt(MeltBolt11Request {
                    quote: quote.quote,
                    inputs: proofs,
                    token_ids: None,
                    outputs: None,
                })
                .await
                .unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        // Two concurrent melts of the SAME quote: exactly one wins.
        let proofs = mint_proofs(&b, "ccd").await;
        let quote = b
            .melt_quote(MeltQuoteBolt11Request {
                request: "lnbc1x".into(),
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let (b1, b2) = (b.clone(), b.clone());
        let (q1, q2) = (quote.quote.clone(), quote.quote.clone());
        let (p1, p2) = (proofs.clone(), proofs.clone());
        let (r1, r2) = tokio::join!(
            b1.melt(MeltBolt11Request {
                quote: q1,
                inputs: p1,
                token_ids: None,
                outputs: None,
            }),
            b2.melt(MeltBolt11Request {
                quote: q2,
                inputs: p2,
                token_ids: None,
                outputs: None,
            })
        );
        let wins = [&r1, &r2].iter().filter(|r| r.is_ok()).count();
        assert_eq!(
            wins, 1,
            "exactly one concurrent melt may settle: {r1:?} {r2:?}"
        );
    }

    #[tokio::test]
    async fn settled_sibling_rejected_at_reserve_and_reconcile() {
        // Two quotes for the SAME invoice created before any settle. The
        // first melts and settles; the second must be refused at reserve
        // (inputs untouched) and at reconcile-settle.
        let config = live_config();
        let ark = Arc::new(MockArkClient::new(config.ark.default_vtxo_expiry));
        let signer = build_blind_signer(&config.signatory).unwrap();
        let b = MintBackend::new(
            config,
            ark,
            signer,
            VtxoInventory::open_in_memory().unwrap(),
            PolLedger::open_in_memory().unwrap(),
            SpentSecretStore::open_in_memory().unwrap(),
            ReceiptStore::open_in_memory().unwrap(),
            ReceiptSigner::dev(),
            None,
        );
        let proofs_a = mint_proofs(&b, "sba").await;
        let proofs_b = mint_proofs(&b, "sbb").await;
        let invoice = test_invoice(500);
        let quote_a = b
            .melt_quote(MeltQuoteBolt11Request {
                request: invoice.clone(),
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let quote_b = b
            .melt_quote(MeltQuoteBolt11Request {
                request: invoice,
                unit: "sat".into(),
            })
            .await
            .unwrap();

        let melted = b
            .melt(MeltBolt11Request {
                quote: quote_a.quote,
                inputs: proofs_a,
                token_ids: None,
                outputs: None,
            })
            .await
            .unwrap();
        assert_eq!(melted.state, QuoteState::Paid);

        // Reserve-time: quote B cannot dispatch a second payout for the
        // already-paid invoice; its quote and inputs are untouched.
        let err = b
            .melt(MeltBolt11Request {
                quote: quote_b.quote.clone(),
                inputs: proofs_b.clone(),
                token_ids: None,
                outputs: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::InvalidRequest(_)));
        assert_eq!(
            b.get_melt_quote(&quote_b.quote).await.unwrap().state,
            QuoteState::Unpaid
        );
        b.swap(SwapRequest {
            inputs: proofs_b,
            outputs: outputs_for(1024, "sbb2"),
        })
        .await
        .unwrap();

        // Reconcile-settle: a preimage valid for the hash must not settle
        // quote B - the hash was paid by quote A, not by B's payout.
        let err = b
            .reconcile_settle_reserved_melt(&quote_b.quote, &hex::encode(TEST_PREIMAGE))
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn reconcile_settle_returns_full_reserve_as_change() {
        // Payout outcome unknown -> RESERVED; the operator verifies the
        // payment completed out-of-band and settles with its preimage. No
        // fee measurement exists on that path, so nothing is charged and
        // the full reserve comes back as NUT-08 change on the persisted
        // blank outputs.
        let b = live_backend(false);
        let proofs = mint_proofs(&b, "rsc").await;
        let melt_quote = melt_quote_500(&b).await;
        let _ = b
            .melt(MeltBolt11Request {
                quote: melt_quote.quote.clone(),
                inputs: proofs,
                token_ids: None,
                outputs: Some(blank_outputs(10, "rsc")),
            })
            .await
            .unwrap_err();
        assert_eq!(b.list_reserved_melts().unwrap().len(), 1);

        b.reconcile_settle_reserved_melt(&melt_quote.quote, &hex::encode(TEST_PREIMAGE))
            .await
            .unwrap();

        // 1024 in, 500 invoice, no fee measurement -> 524 change, served
        // by the quote check endpoint and identical on repeat.
        let q = b.get_melt_quote(&melt_quote.quote).await.unwrap();
        assert_eq!(q.state, QuoteState::Paid);
        let first = serde_json::to_string(&q.change).unwrap();
        let change = q.change.expect("change served after reconcile settle");
        assert_eq!(
            change.iter().map(|c| c.amount).collect::<Vec<_>>(),
            vec![4, 8, 512]
        );
        let again = b.get_melt_quote(&melt_quote.quote).await.unwrap();
        assert_eq!(serde_json::to_string(&again.change).unwrap(), first);
        // PoL: the full 1024 input burn against the 524 change mint,
        // counted once.
        assert_eq!(b.pol_status().unwrap().outstanding_sat, 524);
        // Receipt: nothing charged, 524 returned.
        let receipts = b.receipts_for_quote(&melt_quote.quote).unwrap().receipts;
        let settled = receipts.iter().find(|r| r.outcome == "settled").unwrap();
        assert_eq!(settled.fee_charged_sat, Some(0));
        assert_eq!(settled.change_sat, Some(524));
    }

    #[tokio::test]
    async fn settled_change_recovered_via_quote_check_after_lost_response() {
        // Crash window: the settle committed but the change signatures were
        // never persisted (simulated by settling straight through the
        // store). The next quote check completes the change, once.
        let b = backend();
        let proofs = mint_proofs(&b, "rec").await;
        let melt_quote = b
            .melt_quote(MeltQuoteBolt11Request {
                request: "lnbc1".into(),
                unit: "sat".into(),
            })
            .await
            .unwrap();
        b.spent
            .reserve_melt(
                &melt_quote.quote,
                &b.active_keyset_id(),
                &proofs,
                &blank_outputs(10, "rec"),
                &[],
            )
            .unwrap();
        b.spent
            .settle_melt(&melt_quote.quote, &hex::encode(TEST_PREIMAGE), None)
            .unwrap();

        let before = b.pol_status().unwrap().outstanding_sat;
        let q = b.get_melt_quote(&melt_quote.quote).await.unwrap();
        let first = serde_json::to_string(&q.change).unwrap();
        let change = q.change.expect("lazy change completion");
        assert_eq!(
            change.iter().map(|c| c.amount).collect::<Vec<_>>(),
            vec![4, 8, 512]
        );
        assert_eq!(b.pol_status().unwrap().outstanding_sat, before + 524);
        // Idempotent: the same signatures again, no second PoL mint.
        let again = b.get_melt_quote(&melt_quote.quote).await.unwrap();
        assert_eq!(serde_json::to_string(&again.change).unwrap(), first);
        assert_eq!(b.pol_status().unwrap().outstanding_sat, before + 524);
    }

    #[tokio::test]
    async fn melt_with_change_releases_backing_for_net_burn_only() {
        // The reissued change is still outstanding ecash and keeps its
        // backing: only input_total - change is released. Releasing the
        // full input total would leave the change unbacked.
        let mut config = test_config();
        config.trust.release_backing_on_melt = true;
        let ark = Arc::new(MockArkClient::new(config.ark.default_vtxo_expiry));
        let signer = build_blind_signer(&config.signatory).unwrap();
        let b = MintBackend::new(
            config,
            ark,
            signer,
            VtxoInventory::open_in_memory().unwrap(),
            PolLedger::open_in_memory().unwrap(),
            SpentSecretStore::open_in_memory().unwrap(),
            ReceiptStore::open_in_memory().unwrap(),
            ReceiptSigner::dev(),
            None,
        );
        let quote_b = b
            .mint_quote(MintQuoteBolt11Request {
                amount: 1024,
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let minted = b
            .mint(MintBolt11Request {
                quote: quote_b.quote.clone(),
                outputs: outputs_for(1024, "net"),
            })
            .await
            .unwrap();
        let proofs = proofs_from(&minted.signatures);
        let melt_quote = b
            .melt_quote(MeltQuoteBolt11Request {
                request: "lnbc1".into(),
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let melted = b
            .melt(MeltBolt11Request {
                quote: melt_quote.quote.clone(),
                inputs: proofs,
                token_ids: Some(vec![quote_b.quote.clone()]),
                outputs: Some(blank_outputs(10, "net")),
            })
            .await
            .unwrap();
        let change = melted.change.expect("change returned");
        assert_eq!(change.iter().map(|c| c.amount).sum::<u64>(), 524);
        // Net burn is 500 sat: the token's backing is released only for
        // that, so the token still has the change's 524 sat backing it.
        assert!(b.token_vtxo(&quote_b.quote).await.is_ok());
        assert_eq!(b.pol_status().unwrap().outstanding_sat, 524);
        let receipts = b.receipts_for_quote(&melt_quote.quote).unwrap().receipts;
        assert_eq!(receipts[0].fee_charged_sat, Some(0));
        assert_eq!(receipts[0].retained_sat, Some(0));
        assert_eq!(receipts[0].change_sat, Some(524));
    }

    #[tokio::test]
    async fn reconcile_settle_releases_backing_for_net_burn() {
        // The operator path finalizes identically: persisted token_ids drive
        // the same net-backing release after the unknown payout settles.
        let mut config = live_config();
        config.trust.release_backing_on_melt = true;
        let ark: Arc<dyn ArkClient> = Arc::new(FailingPayoutArk {
            inner: MockArkClient::new(config.ark.default_vtxo_expiry),
            definitive: false,
        });
        let signer = build_blind_signer(&config.signatory).unwrap();
        let b = MintBackend::new(
            config,
            ark,
            signer,
            VtxoInventory::open_in_memory().unwrap(),
            PolLedger::open_in_memory().unwrap(),
            SpentSecretStore::open_in_memory().unwrap(),
            ReceiptStore::open_in_memory().unwrap(),
            ReceiptSigner::dev(),
            None,
        );
        let quote_b = b
            .mint_quote(MintQuoteBolt11Request {
                amount: 1024,
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let minted = b
            .mint(MintBolt11Request {
                quote: quote_b.quote.clone(),
                outputs: outputs_for(1024, "rnb"),
            })
            .await
            .unwrap();
        let proofs = proofs_from(&minted.signatures);
        let melt_quote = melt_quote_500(&b).await;
        let _ = b
            .melt(MeltBolt11Request {
                quote: melt_quote.quote.clone(),
                inputs: proofs,
                token_ids: Some(vec![quote_b.quote.clone()]),
                outputs: Some(blank_outputs(10, "rnb")),
            })
            .await
            .unwrap_err();
        b.reconcile_settle_reserved_melt(&melt_quote.quote, &hex::encode(TEST_PREIMAGE))
            .await
            .unwrap();
        let q = b.get_melt_quote(&melt_quote.quote).await.unwrap();
        assert_eq!(
            q.change.unwrap().iter().map(|c| c.amount).sum::<u64>(),
            524
        );
        // Net burn 500 sat released; the change's 524 sat of backing stays.
        assert!(b.token_vtxo(&quote_b.quote).await.is_ok());
        assert_eq!(b.pol_status().unwrap().outstanding_sat, 524);
    }

    #[tokio::test]
    async fn crash_settled_quote_fully_finalized_by_quote_check() {
        // Crash after the settle commit: nothing finalized. The quote check
        // completes change signing, backing release and the receipt, once.
        let b = backend();
        let proofs = mint_proofs(&b, "ffn").await;
        let melt_quote = b
            .melt_quote(MeltQuoteBolt11Request {
                request: "lnbc1".into(),
                unit: "sat".into(),
            })
            .await
            .unwrap();
        b.spent
            .reserve_melt(
                &melt_quote.quote,
                &b.active_keyset_id(),
                &proofs,
                &blank_outputs(10, "ffn"),
                &[],
            )
            .unwrap();
        b.spent
            .settle_melt(&melt_quote.quote, &hex::encode(TEST_PREIMAGE), None)
            .unwrap();
        assert!(b.receipts_for_quote(&melt_quote.quote).unwrap().receipts.is_empty());
        assert!(
            !b.spent
                .melt_quote(&melt_quote.quote)
                .unwrap()
                .unwrap()
                .backing_released
        );

        let q = b.get_melt_quote(&melt_quote.quote).await.unwrap();
        assert_eq!(
            q.change.unwrap().iter().map(|c| c.amount).sum::<u64>(),
            524
        );
        assert!(
            b.spent
                .melt_quote(&melt_quote.quote)
                .unwrap()
                .unwrap()
                .backing_released
        );
        let receipts = b.receipts_for_quote(&melt_quote.quote).unwrap().receipts;
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].outcome, "settled");
        assert_eq!(receipts[0].retained_sat, Some(0));
        // Second pass: nothing new, no duplicate receipt.
        b.get_melt_quote(&melt_quote.quote).await.unwrap();
        assert_eq!(
            b.receipts_for_quote(&melt_quote.quote).unwrap().receipts.len(),
            1
        );
    }

    #[tokio::test]
    async fn concurrent_quote_checks_finalize_once() {
        // A quote check racing another finalization records the change mint
        // and the signatures exactly once.
        let b = backend();
        let proofs = mint_proofs(&b, "ccr").await;
        let melt_quote = b
            .melt_quote(MeltQuoteBolt11Request {
                request: "lnbc1".into(),
                unit: "sat".into(),
            })
            .await
            .unwrap();
        b.spent
            .reserve_melt(
                &melt_quote.quote,
                &b.active_keyset_id(),
                &proofs,
                &blank_outputs(10, "ccr"),
                &[],
            )
            .unwrap();
        b.spent
            .settle_melt(&melt_quote.quote, &hex::encode(TEST_PREIMAGE), None)
            .unwrap();

        let before = b.pol_status().unwrap().outstanding_sat;
        let (r1, r2) = tokio::join!(
            b.get_melt_quote(&melt_quote.quote),
            b.get_melt_quote(&melt_quote.quote)
        );
        let (q1, q2) = (r1.unwrap(), r2.unwrap());
        assert_eq!(
            serde_json::to_string(&q1.change).unwrap(),
            serde_json::to_string(&q2.change).unwrap()
        );
        assert_eq!(b.pol_status().unwrap().outstanding_sat, before + 524);
        assert_eq!(
            b.receipts_for_quote(&melt_quote.quote).unwrap().receipts.len(),
            1
        );
    }

    #[tokio::test]
    async fn concurrent_quote_checks_release_backing_for_net_burn_once() {
        // Two racing finalizations of a crash-settled, token-backed melt:
        // the backing release is keyed by quote in the inventory, so the
        // net burn is released exactly once, never twice.
        let b = release_backend();
        let (_token, melt_quote) = crash_settled_token_backed(&b, "cn1").await;
        assert_eq!(b.inventory().total_allocated_msat().unwrap(), 1_024_000);

        let (r1, r2) = tokio::join!(
            b.get_melt_quote(&melt_quote),
            b.get_melt_quote(&melt_quote)
        );
        let (q1, q2) = (r1.unwrap(), r2.unwrap());
        assert_eq!(
            serde_json::to_string(&q1.change).unwrap(),
            serde_json::to_string(&q2.change).unwrap()
        );
        // Net burn is 500 sat; the change's 524 sat keeps its backing.
        assert_eq!(b.inventory().total_allocated_msat().unwrap(), 524_000);
        // A repeated finalization releases nothing more.
        b.get_melt_quote(&melt_quote).await.unwrap();
        assert_eq!(b.inventory().total_allocated_msat().unwrap(), 524_000);
        assert_eq!(
            b.receipts_for_quote(&melt_quote).unwrap().receipts.len(),
            1
        );
    }

    #[tokio::test]
    async fn crash_after_inventory_release_replays_without_double_release() {
        // A crash between the inventory release commit and the flag
        // write: the next finalization replays the quote's recorded
        // release instead of releasing the net burn a second time.
        let b = release_backend();
        let (token, melt_quote) = crash_settled_token_backed(&b, "cn2").await;
        // The inventory half of a finalization that crashed before the
        // backing_released flag write.
        let released = b
            .inventory()
            .release_backing_for_quote(&melt_quote, &[token], 500_000)
            .unwrap();
        assert_eq!(released, 500_000);
        assert_eq!(b.inventory().total_allocated_msat().unwrap(), 524_000);
        assert!(
            !b.spent
                .melt_quote(&melt_quote)
                .unwrap()
                .unwrap()
                .backing_released
        );

        b.get_melt_quote(&melt_quote).await.unwrap();
        assert_eq!(b.inventory().total_allocated_msat().unwrap(), 524_000);
        assert!(
            b.spent
                .melt_quote(&melt_quote)
                .unwrap()
                .unwrap()
                .backing_released
        );
        assert_eq!(
            b.receipts_for_quote(&melt_quote).unwrap().receipts.len(),
            1
        );
    }

    #[tokio::test]
    async fn change_served_despite_bookkeeping_failure() {
        // A settled quote whose persisted backing token no longer exists
        // (legacy row, operator edit): the backing release fails, but the
        // wallet's signed change must still be served, and the pending
        // bookkeeping is retried by the sweep without ever blocking the
        // wallet.
        let b = backend();
        let proofs = mint_proofs(&b, "bkf").await;
        let melt_quote = b
            .melt_quote(MeltQuoteBolt11Request {
                request: "lnbc1".into(),
                unit: "sat".into(),
            })
            .await
            .unwrap();
        b.spent
            .reserve_melt(
                &melt_quote.quote,
                &b.active_keyset_id(),
                &proofs,
                &blank_outputs(10, "bkf"),
                &["bogus-token".to_string()],
            )
            .unwrap();
        b.spent
            .settle_melt(&melt_quote.quote, &hex::encode(TEST_PREIMAGE), None)
            .unwrap();

        let q = b.get_melt_quote(&melt_quote.quote).await.unwrap();
        assert_eq!(
            q.change.unwrap().iter().map(|c| c.amount).sum::<u64>(),
            524
        );
        // Bookkeeping stays pending (no flag, no receipt) and the sweep
        // cannot complete it - none of which blocks the wallet's change.
        assert!(
            !b.spent
                .melt_quote(&melt_quote.quote)
                .unwrap()
                .unwrap()
                .backing_released
        );
        assert!(b
            .receipts_for_quote(&melt_quote.quote)
            .unwrap()
            .receipts
            .is_empty());
        assert_eq!(b.recover_pending_finalize().await.unwrap(), 0);
        let q = b.get_melt_quote(&melt_quote.quote).await.unwrap();
        assert!(q.change.is_some());
    }

    #[tokio::test]
    async fn melt_rejects_unknown_token_id_before_payout() {
        // An unknown backing token fails the request up front: no state
        // changes, the quote stays UNPAID and the inputs stay spendable.
        let b = backend();
        let proofs = mint_proofs(&b, "utx").await;
        let melt_quote = b
            .melt_quote(MeltQuoteBolt11Request {
                request: "lnbc1".into(),
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let err = b
            .melt(MeltBolt11Request {
                quote: melt_quote.quote.clone(),
                inputs: proofs.clone(),
                token_ids: Some(vec!["no-such-token".to_string()]),
                outputs: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::MappingNotFound(_)));
        assert!(matches!(
            b.spent
                .melt_quote(&melt_quote.quote)
                .unwrap()
                .unwrap()
                .state,
            MeltQuoteState::Unpaid
        ));
        b.swap(SwapRequest {
            inputs: proofs,
            outputs: outputs_for(1024, "utx2"),
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn recover_pending_finalize_sweeps_crash_settled_quote() {
        // The startup/CLI sweep finalizes a crash-settled quote end to
        // end: change signed, backing released for the net burn, receipt
        // issued - and then leaves it alone.
        let b = release_backend();
        let (_token, melt_quote) = crash_settled_token_backed(&b, "swp").await;
        assert_eq!(b.spent.list_pending_finalize_melts().unwrap().len(), 1);

        assert_eq!(b.recover_pending_finalize().await.unwrap(), 1);
        let quote = b.spent.melt_quote(&melt_quote).unwrap().unwrap();
        assert!(quote.backing_released);
        assert_eq!(
            quote.change_signatures.iter().map(|c| c.amount).sum::<u64>(),
            524
        );
        assert_eq!(b.inventory().total_allocated_msat().unwrap(), 524_000);
        assert_eq!(b.receipts_for_quote(&melt_quote).unwrap().receipts.len(), 1);
        assert!(b.spent.list_pending_finalize_melts().unwrap().is_empty());
        assert_eq!(b.recover_pending_finalize().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn released_quote_check_serves_state_despite_receipt_failure() {
        // A released melt whose receipt write keeps failing (here: the
        // receipts database is write-locked past its busy timeout) still
        // answers the wallet's quote check; the receipt is logged as
        // pending and issued by a later check once the store recovers.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("receipts.sqlite");
        let config = live_config();
        let ark = Arc::new(FailingPayoutArk {
            inner: MockArkClient::new(config.ark.default_vtxo_expiry),
            definitive: true,
        });
        let signer = build_blind_signer(&config.signatory).unwrap();
        let b = MintBackend::new(
            config,
            ark,
            signer,
            VtxoInventory::open_in_memory().unwrap(),
            PolLedger::open_in_memory().unwrap(),
            SpentSecretStore::open_in_memory().unwrap(),
            ReceiptStore::open(&db).unwrap(),
            ReceiptSigner::dev(),
            None,
        );
        let proofs = mint_proofs(&b, "rlf").await;
        let melt_quote = melt_quote_500(&b).await;
        b.spent
            .reserve_melt(
                &melt_quote.quote,
                &b.active_keyset_id(),
                &proofs,
                &[],
                &[],
            )
            .unwrap();
        b.spent.release_melt(&melt_quote.quote).unwrap();

        // Write-lock the receipts file from another connection so the
        // append fails after its busy timeout.
        let blocker = rusqlite::Connection::open(&db).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let q = b.get_melt_quote(&melt_quote.quote).await.unwrap();
        assert!(matches!(q.state, QuoteState::Unpaid));
        assert!(b
            .receipts_for_quote(&melt_quote.quote)
            .unwrap()
            .receipts
            .is_empty());

        // Store recovered: the next quote check issues the receipt, once.
        blocker.execute_batch("ROLLBACK").unwrap();
        drop(blocker);
        let q = b.get_melt_quote(&melt_quote.quote).await.unwrap();
        assert!(matches!(q.state, QuoteState::Unpaid));
        assert_eq!(
            b.receipts_for_quote(&melt_quote.quote)
                .unwrap()
                .receipts
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn crash_released_quote_gets_receipt_via_quote_check() {
        // A crash between the release commit and the receipt write: the
        // quote check issues the released receipt, once.
        let b = live_backend(true);
        let proofs = mint_proofs(&b, "crl").await;
        let melt_quote = melt_quote_500(&b).await;
        b.spent
            .reserve_melt(
                &melt_quote.quote,
                &b.active_keyset_id(),
                &proofs,
                &[],
                &[],
            )
            .unwrap();
        b.spent.release_melt(&melt_quote.quote).unwrap();
        assert!(b.receipts_for_quote(&melt_quote.quote).unwrap().receipts.is_empty());

        let q = b.get_melt_quote(&melt_quote.quote).await.unwrap();
        // A released quote reports Unpaid publicly (NUT-05 has no
        // released state); the inputs are spendable again.
        assert_eq!(q.state, QuoteState::Unpaid);
        let receipts = b.receipts_for_quote(&melt_quote.quote).unwrap().receipts;
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].outcome, "released");
        b.get_melt_quote(&melt_quote.quote).await.unwrap();
        assert_eq!(
            b.receipts_for_quote(&melt_quote.quote).unwrap().receipts.len(),
            1
        );
    }

    #[tokio::test]
    async fn receipt_anchor_recovery_rewrites_missing_pol_events() {
        // A receipt written without its PoL anchor (a crash between the two
        // writes) is anchored by the recovery sweep, idempotently.
        let b = backend();
        let receipt = b
            .receipts
            .append(
                &b.receipt_signer,
                "mint",
                "q-anchor",
                8,
                None,
                None,
                None,
                "issued",
                &b.active_keyset_id(),
            )
            .unwrap();
        assert!(!b.pol.has_receipt_event(&receipt.receipt_hash).unwrap());
        assert_eq!(b.recover_receipt_anchors().unwrap(), 1);
        assert!(b.pol.has_receipt_event(&receipt.receipt_hash).unwrap());
        assert_eq!(b.recover_receipt_anchors().unwrap(), 0);
    }

    #[tokio::test]
    async fn reconcile_release_still_guarded_after_settle_paths_added() {
        // Release only works from RESERVED; a settled quote cannot be
        // released (which would unburn proofs for a completed payout).
        let b = live_backend(false);
        let proofs = mint_proofs(&b, "grd").await;
        let melt_quote = melt_quote_500(&b).await;
        let _ = b
            .melt(MeltBolt11Request {
                quote: melt_quote.quote.clone(),
                inputs: proofs,
                token_ids: None,
                outputs: None,
            })
            .await
            .unwrap_err();
        b.reconcile_settle_reserved_melt(&melt_quote.quote, &hex::encode(TEST_PREIMAGE))
            .await
            .unwrap();
        assert!(b
            .reconcile_release_reserved_melt(&melt_quote.quote)
            .is_err());
    }

    // -- NUT-08 change outputs + signed receipts ----------------------------

    /// `n` NUT-08 blank outputs (amount undetermined; the mint imprints it).
    /// Backend with backing release on melt enabled (token-backed melts
    /// release the net burn from the named token mappings).
    fn release_backend() -> MintBackend {
        let mut config = test_config();
        config.trust.release_backing_on_melt = true;
        let ark = Arc::new(MockArkClient::new(config.ark.default_vtxo_expiry));
        let signer = build_blind_signer(&config.signatory).unwrap();
        MintBackend::new(
            config,
            ark,
            signer,
            VtxoInventory::open_in_memory().unwrap(),
            PolLedger::open_in_memory().unwrap(),
            SpentSecretStore::open_in_memory().unwrap(),
            ReceiptStore::open_in_memory().unwrap(),
            ReceiptSigner::dev(),
            None,
        )
    }

    /// Mint 1024 sat and crash-settle a 500 sat melt against it with the
    /// mint's token as backing: reserved and settled directly in the
    /// store, with nothing finalized. Returns (backend, mint quote id,
    /// melt quote id).
    async fn crash_settled_token_backed(b: &MintBackend, tag: &str) -> (String, String) {
        let quote_b = b
            .mint_quote(MintQuoteBolt11Request {
                amount: 1024,
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let minted = b
            .mint(MintBolt11Request {
                quote: quote_b.quote.clone(),
                outputs: outputs_for(1024, tag),
            })
            .await
            .unwrap();
        let proofs = proofs_from(&minted.signatures);
        let melt_quote = b
            .melt_quote(MeltQuoteBolt11Request {
                request: "lnbc1".into(),
                unit: "sat".into(),
            })
            .await
            .unwrap();
        b.spent
            .reserve_melt(
                &melt_quote.quote,
                &b.active_keyset_id(),
                &proofs,
                &blank_outputs(10, tag),
                &[quote_b.quote.clone()],
            )
            .unwrap();
        b.spent
            .settle_melt(&melt_quote.quote, &hex::encode(TEST_PREIMAGE), None)
            .unwrap();
        (quote_b.quote, melt_quote.quote)
    }

    fn blank_outputs(n: usize, tag: &str) -> Vec<BlindedMessage> {
        (0..n)
            .map(|i| BlindedMessage {
                amount: 0,
                id: KEYSET_ID.to_string(),
                b: format!("02{tag}blank{i}{}", "cd".repeat(16)),
            })
            .collect()
    }

    /// Ark client whose payout succeeds, reporting `fee_sat` as the
    /// measured fee (or no measurement when None).
    struct FeeMeasuringArk {
        inner: MockArkClient,
        fee_sat: Option<u64>,
    }

    #[async_trait::async_trait]
    impl ArkClient for FeeMeasuringArk {
        async fn board_sats(&self, amount_msat: u64) -> Result<Vtxo> {
            self.inner.board_sats(amount_msat).await
        }
        async fn refresh_vtxo(&self, vtxo: &Vtxo) -> Result<Vtxo> {
            self.inner.refresh_vtxo(vtxo).await
        }
        async fn unilateral_exit(&self, vtxo: &Vtxo) -> Result<ExitResult> {
            self.inner.unilateral_exit(vtxo).await
        }
        async fn get_vtxo_expiry(&self, vtxo: &Vtxo) -> Result<std::time::Duration> {
            self.inner.get_vtxo_expiry(vtxo).await
        }
        async fn current_block_height(&self) -> Result<u64> {
            self.inner.current_block_height().await
        }
        async fn ping(&self) -> Result<()> {
            self.inner.ping().await
        }
        async fn estimate_lightning_send_fee_sat(&self, amount_sat: u64) -> Result<u64> {
            self.inner.estimate_lightning_send_fee_sat(amount_sat).await
        }
        async fn pay_lightning_invoice(
            &self,
            _invoice: &str,
            _amount_sat: u64,
            _attempted_before: bool,
        ) -> Result<PayoutResult> {
            Ok(PayoutResult {
                preimage: hex::encode(TEST_PREIMAGE),
                fee_paid_sat: self.fee_sat,
            })
        }
    }

    fn live_fee_backend(fee_sat: Option<u64>) -> MintBackend {
        let config = live_config();
        let ark = Arc::new(FeeMeasuringArk {
            inner: MockArkClient::new(config.ark.default_vtxo_expiry),
            fee_sat,
        });
        let signer = build_blind_signer(&config.signatory).unwrap();
        MintBackend::new(
            config,
            ark,
            signer,
            VtxoInventory::open_in_memory().unwrap(),
            PolLedger::open_in_memory().unwrap(),
            SpentSecretStore::open_in_memory().unwrap(),
            ReceiptStore::open_in_memory().unwrap(),
            ReceiptSigner::dev(),
            None,
        )
    }

    #[tokio::test]
    async fn nut08_change_returned_on_blank_outputs() {
        // Mock payout measures a zero fee: everything above the invoice
        // amount comes back as change. 1024 in, 500 invoice -> 524 returned.
        let b = backend();
        let proofs = mint_proofs(&b, "cng").await;
        let melt_quote = b
            .melt_quote(MeltQuoteBolt11Request {
                request: "lnbc1".into(),
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let blanks = blank_outputs(10, "cng");
        let melted = b
            .melt(MeltBolt11Request {
                quote: melt_quote.quote.clone(),
                inputs: proofs,
                token_ids: None,
                outputs: Some(blanks),
            })
            .await
            .unwrap();
        assert_eq!(melted.state, QuoteState::Paid);

        // 524 decomposes to 4 + 8 + 512; signatures come back ascending, in
        // the order of the blank outputs they were imprinted on, with the
        // unused trailing outputs omitted.
        let change = melted.change.expect("change returned");
        assert_eq!(
            change.iter().map(|c| c.amount).collect::<Vec<_>>(),
            vec![4, 8, 512]
        );

        // The returned change is minted ecash: outstanding liability is the
        // change, not zero.
        assert_eq!(b.pol_status().unwrap().outstanding_sat, 524);

        // The settled receipt records the fee charged and the change.
        let receipts = b.receipts_for_quote(&melt_quote.quote).unwrap().receipts;
        assert_eq!(receipts.len(), 1);
        let receipt = &receipts[0];
        assert_eq!(receipt.kind, "melt");
        assert_eq!(receipt.outcome, "settled");
        assert_eq!(receipt.amount_sat, 500);
        assert_eq!(receipt.fee_charged_sat, Some(0));
        assert_eq!(receipt.change_sat, Some(524));
        let pubkey = b.info().await.receipt_pubkey.unwrap();
        assert!(verify_receipt(receipt, &pubkey));
    }

    #[tokio::test]
    async fn nut08_measured_fee_reduces_change() {
        // Live payout reporting an actual fee of 2 sat against a 5 sat
        // reserve: 1024 in, 500 invoice, 2 fee -> 522 change.
        let b = live_fee_backend(Some(2));
        let proofs = mint_proofs(&b, "fee").await;
        let melt_quote = melt_quote_500(&b).await;
        let melted = b
            .melt(MeltBolt11Request {
                quote: melt_quote.quote.clone(),
                inputs: proofs,
                token_ids: None,
                outputs: Some(blank_outputs(10, "fee")),
            })
            .await
            .unwrap();
        // 522 decomposes to 2 + 8 + 512.
        let change = melted.change.expect("change returned");
        assert_eq!(
            change.iter().map(|c| c.amount).collect::<Vec<_>>(),
            vec![2, 8, 512]
        );
        assert_eq!(b.pol_status().unwrap().outstanding_sat, 522);
        let receipts = b.receipts_for_quote(&melt_quote.quote).unwrap().receipts;
        assert_eq!(receipts[0].fee_charged_sat, Some(2));
        assert_eq!(receipts[0].change_sat, Some(522));
        // The quote record keeps the measured fee.
        let q = b.get_melt_quote(&melt_quote.quote).await.unwrap();
        assert_eq!(q.state, QuoteState::Paid);
    }

    #[tokio::test]
    async fn nut08_unmeasured_fee_returns_full_reserve() {
        // Live payout with no fee measurement (the barkd wallet reports
        // none): the mint charges nothing and returns the whole reserve.
        let b = live_fee_backend(None);
        let proofs = mint_proofs(&b, "unm").await;
        let melt_quote = melt_quote_500(&b).await;
        let melted = b
            .melt(MeltBolt11Request {
                quote: melt_quote.quote.clone(),
                inputs: proofs,
                token_ids: None,
                outputs: Some(blank_outputs(10, "unm")),
            })
            .await
            .unwrap();
        let change_total: u64 = melted.change.unwrap().iter().map(|c| c.amount).sum();
        assert_eq!(change_total, 1024 - 500);
        let receipts = b.receipts_for_quote(&melt_quote.quote).unwrap().receipts;
        assert_eq!(receipts[0].fee_charged_sat, Some(0));
    }

    #[tokio::test]
    async fn nut08_no_blank_outputs_no_change() {
        // Without blank outputs there is nowhere to return the overpayment:
        // the mint keeps it and the receipt says so.
        let b = backend();
        let proofs = mint_proofs(&b, "noc").await;
        let melt_quote = b
            .melt_quote(MeltQuoteBolt11Request {
                request: "lnbc1".into(),
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let melted = b
            .melt(MeltBolt11Request {
                quote: melt_quote.quote.clone(),
                inputs: proofs,
                token_ids: None,
                outputs: None,
            })
            .await
            .unwrap();
        assert!(melted.change.is_none());
        assert_eq!(b.pol_status().unwrap().outstanding_sat, 0);
        // No measured fee (0 charged) and no blank outputs: the whole
        // reserve is retained by the mint, and the receipt says both
        // distinctly.
        let receipts = b.receipts_for_quote(&melt_quote.quote).unwrap().receipts;
        assert_eq!(receipts[0].fee_charged_sat, Some(0));
        assert_eq!(receipts[0].retained_sat, Some(524));
        assert_eq!(receipts[0].change_sat, Some(0));
    }

    #[tokio::test]
    async fn nut08_blank_outputs_wrong_keyset_rejected() {
        let b = backend();
        let proofs = mint_proofs(&b, "bad").await;
        let melt_quote = b
            .melt_quote(MeltQuoteBolt11Request {
                request: "lnbc1".into(),
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let mut blanks = blank_outputs(2, "bad");
        blanks[0].id = "00deadbeef00dead".into();
        let err = b
            .melt(MeltBolt11Request {
                quote: melt_quote.quote.clone(),
                inputs: proofs.clone(),
                token_ids: None,
                outputs: Some(blanks),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::InvalidRequest(_)));
        // Nothing was reserved: the inputs are still spendable.
        b.swap(SwapRequest {
            inputs: proofs,
            outputs: outputs_for(1024, "bad2"),
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn receipts_chain_across_operations_and_info_publishes_key() {
        let b = backend();
        let mint_quote = b
            .mint_quote(MintQuoteBolt11Request {
                amount: 1024,
                unit: "sat".into(),
            })
            .await
            .unwrap();
        let minted = b
            .mint(MintBolt11Request {
                quote: mint_quote.quote.clone(),
                outputs: outputs_for(1024, "chn"),
            })
            .await
            .unwrap();
        let mint_receipts = b.receipts_for_quote(&mint_quote.quote).unwrap().receipts;
        assert_eq!(mint_receipts.len(), 1);
        let mint_receipt = &mint_receipts[0];
        assert_eq!(mint_receipt.kind, "mint");
        assert_eq!(mint_receipt.outcome, "issued");
        assert_eq!(mint_receipt.amount_sat, 1024);
        assert_eq!(mint_receipt.prev_hash, "genesis");

        let info = b.info().await;
        let pubkey = info.receipt_pubkey.clone().unwrap();
        assert_eq!(info.nuts["8"]["supported"], serde_json::json!(true));
        assert!(verify_receipt(mint_receipt, &pubkey));

        let melt_quote = b
            .melt_quote(MeltQuoteBolt11Request {
                request: "lnbc1".into(),
                unit: "sat".into(),
            })
            .await
            .unwrap();
        b.melt(MeltBolt11Request {
            quote: melt_quote.quote.clone(),
            inputs: proofs_from(&minted.signatures),
            token_ids: None,
            outputs: None,
        })
        .await
        .unwrap();
        let melt_receipts = b.receipts_for_quote(&melt_quote.quote).unwrap().receipts;
        assert_eq!(melt_receipts.len(), 1);
        // The melt receipt chains onto the mint receipt.
        assert_eq!(melt_receipts[0].prev_hash, mint_receipt.receipt_hash);
        assert!(verify_receipt(&melt_receipts[0], &pubkey));

        // Receipt hashes are committed into the PoL epoch: closing the
        // current epoch yields a root that commits to them.
        let day = crate::pol::PolLedger::current_epoch_day();
        let root = b
            .pol()
            .close_epoch(&day, KEYSET_ID)
            .unwrap()
            .expect("epoch closes");
        assert_eq!(root.root_hash.len(), 64);
    }

    #[tokio::test]
    async fn released_melt_issues_receipt() {
        let b = live_backend(true); // definitive payout failure
        let proofs = mint_proofs(&b, "rlr").await;
        let melt_quote = melt_quote_500(&b).await;
        let err = b
            .melt(MeltBolt11Request {
                quote: melt_quote.quote.clone(),
                inputs: proofs,
                token_ids: None,
                outputs: Some(blank_outputs(4, "rlr")),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MintError::PaymentFailed(_)));
        let receipts = b.receipts_for_quote(&melt_quote.quote).unwrap().receipts;
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].outcome, "released");
        assert_eq!(receipts[0].fee_charged_sat, None);
        let pubkey = b.info().await.receipt_pubkey.unwrap();
        assert!(verify_receipt(&receipts[0], &pubkey));
    }

    #[tokio::test]
    async fn blank_outputs_persisted_with_reservation_across_restart() {
        // The blank outputs ride along with the reservation, so a restart
        // loses neither the quote nor where its change would go.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mint.sqlite");
        let config = live_config();
        let ark = Arc::new(FailingPayoutArk {
            inner: MockArkClient::new(config.ark.default_vtxo_expiry),
            definitive: false,
        });
        let signer = build_blind_signer(&config.signatory).unwrap();
        let melt_quote_id;
        {
            let b = MintBackend::new(
                config.clone(),
                ark.clone(),
                signer.clone(),
                VtxoInventory::open(&db).unwrap(),
                PolLedger::open(&db).unwrap(),
                SpentSecretStore::open(&db).unwrap(),
                ReceiptStore::open(&db).unwrap(),
                ReceiptSigner::dev(),
                None,
            );
            let proofs = mint_proofs(&b, "per").await;
            let melt_quote = melt_quote_500(&b).await;
            melt_quote_id = melt_quote.quote.clone();
            let _ = b
                .melt(MeltBolt11Request {
                    quote: melt_quote.quote,
                    inputs: proofs,
                    token_ids: None,
                    outputs: Some(blank_outputs(3, "per")),
                })
                .await
                .unwrap_err();
        }
        let b2 = MintBackend::new(
            config,
            ark,
            signer,
            VtxoInventory::open(&db).unwrap(),
            PolLedger::open(&db).unwrap(),
            SpentSecretStore::open(&db).unwrap(),
            ReceiptStore::open(&db).unwrap(),
            ReceiptSigner::dev(),
            None,
        );
        let q = b2.get_melt_quote(&melt_quote_id).await.unwrap();
        assert_eq!(q.state, QuoteState::Pending);
        let stored = b2.spent.change_outputs(&melt_quote_id).unwrap();
        assert_eq!(stored.len(), 3);
    }
}
