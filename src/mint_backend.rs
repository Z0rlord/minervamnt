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
//! Blind signatures are MOCKED (deterministic SHA-256) in the scaffold; the
//! real implementation will use the cdk's NUT-00 BDHKE primitives.

use crate::ark_client::ArkClient;
use crate::config::AppConfig;
use crate::error::{MintError, Result};
use crate::ots::{digest_from_root_hex, OtsStamper};
use crate::pol::PolLedger;
use crate::signatory::{DefaultSignatoryPolicy, MintSignRequest, SignatoryPolicy, SwapSignRequest};
use crate::types::*;
use crate::vtxo_inventory::VtxoInventory;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const QUOTE_TTL_SECS: u64 = 600;
const MSAT_PER_SAT: u64 = 1000;

/// Re-export for tests and external callers.
pub use crate::types::KEYSET_ID;

#[derive(Debug, Clone)]
struct MintQuote {
    amount_sat: u64,
    state: QuoteState,
    expiry: u64,
}

#[derive(Debug, Clone)]
struct MeltQuote {
    amount_sat: u64,
    fee_reserve_sat: u64,
    state: QuoteState,
    expiry: u64,
}

pub struct MintBackend {
    config: AppConfig,
    ark: Arc<dyn ArkClient>,
    inventory: VtxoInventory,
    pol: PolLedger,
    ots: Option<Arc<dyn OtsStamper>>,
    policy: DefaultSignatoryPolicy,
    policy_enforced: bool,
    mint_quotes: Mutex<HashMap<String, MintQuote>>,
    melt_quotes: Mutex<HashMap<String, MeltQuote>>,
    /// Spent proof secrets (double-spend guard). In-memory for the scaffold;
    /// production must persist this alongside the keyset DB.
    spent_secrets: Mutex<HashSet<String>>,
}

impl MintBackend {
    pub fn new(
        config: AppConfig,
        ark: Arc<dyn ArkClient>,
        inventory: VtxoInventory,
        pol: PolLedger,
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
            inventory,
            mint_quotes: Mutex::new(HashMap::new()),
            melt_quotes: Mutex::new(HashMap::new()),
            spent_secrets: Mutex::new(HashSet::new()),
        }
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

    /// Deterministic mock blind signature. Real impl: BDHKE `C_ = k * B_`.
    fn mock_blind_sign(output: &BlindedMessage) -> BlindSignature {
        let mut hasher = Sha256::new();
        hasher.update(b"minerva-mock-sig");
        hasher.update(output.id.as_bytes());
        hasher.update(output.amount.to_be_bytes());
        hasher.update(output.b.as_bytes());
        BlindSignature {
            amount: output.amount,
            id: output.id.clone(),
            c: hex::encode(hasher.finalize()),
        }
    }

    fn validate_outputs(outputs: &[BlindedMessage]) -> Result<u64> {
        if outputs.is_empty() {
            return Err(MintError::InvalidRequest("no outputs provided".into()));
        }
        for o in outputs {
            if o.id != KEYSET_ID {
                return Err(MintError::InvalidRequest(format!(
                    "unknown keyset id: {}",
                    o.id
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

    /// Verify proofs and mark secrets spent. Does not update PoL (swap is net-zero).
    fn verify_and_spend_inputs(&self, inputs: &[Proof]) -> Result<u64> {
        if inputs.is_empty() {
            return Err(MintError::InvalidRequest("no inputs provided".into()));
        }
        let mut spent = self.spent_secrets.lock().unwrap();
        for p in inputs {
            if p.id != KEYSET_ID {
                return Err(MintError::InvalidRequest(format!(
                    "unknown keyset id: {}",
                    p.id
                )));
            }
            if spent.contains(&p.secret) {
                return Err(MintError::TokenAlreadySpent);
            }
        }
        let total: u64 = inputs.iter().map(|p| p.amount).sum();
        for p in inputs {
            spent.insert(p.secret.clone());
        }
        Ok(total)
    }

    fn record_melt_burns(&self, inputs: &[Proof]) -> Result<()> {
        for p in inputs {
            self.pol.record_burn(&p.secret, p.amount)?;
        }
        Ok(())
    }

    // -- NUT-06 ------------------------------------------------------------

    pub fn info(&self) -> MintInfo {
        MintInfo {
            name: self.config.mint.name.clone(),
            pubkey: "02".to_string() + &"11".repeat(32),
            version: format!("minerva-mint/{}", env!("CARGO_PKG_VERSION")),
            description: self.config.mint.description.clone(),
            contact: vec![],
            motd: Some("Ark-backed mint scaffold — not production ready".into()),
            nuts: serde_json::json!({
                "4": { "methods": [{"method": "bolt11", "unit": "sat"}], "disabled": false },
                "5": { "methods": [{"method": "bolt11", "unit": "sat"}], "disabled": false },
            }),
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

        let output_total = Self::validate_outputs(&req.outputs)?;
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
                vtxo_id: Some(&mapping.vtxo_id),
            })?;
        }

        let signatures = req.outputs.iter().map(Self::mock_blind_sign).collect();

        let b_points: Vec<String> = req.outputs.iter().map(|o| o.b.clone()).collect();
        self.pol
            .record_mint(&req.quote, amount_sat, &b_points)?;

        self.mint_quotes
            .lock()
            .unwrap()
            .get_mut(&req.quote)
            .expect("quote exists")
            .state = QuoteState::Issued;

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

        // No bolt11 decoding in the scaffold: derive a deterministic amount
        // from the request string so tests are stable. Real impl decodes the
        // invoice (or Ark address + amount).
        let amount_sat = (req.request.len() as u64) * 100;
        let fee_reserve_sat = (amount_sat / 100).max(1);

        let quote_id = Uuid::new_v4().to_string();
        let expiry = Self::now() + QUOTE_TTL_SECS;
        self.melt_quotes.lock().unwrap().insert(
            quote_id.clone(),
            MeltQuote {
                amount_sat,
                fee_reserve_sat,
                state: QuoteState::Unpaid,
                expiry,
            },
        );

        Ok(MeltQuoteBolt11Response {
            quote: quote_id.clone(),
            amount: amount_sat,
            fee_reserve: fee_reserve_sat,
            unit: "sat".into(),
            state: QuoteState::Unpaid,
            expiry,
        })
    }

    /// NUT-05: poll melt quote state by id.
    pub async fn get_melt_quote(&self, quote_id: &str) -> Result<MeltQuoteBolt11Response> {
        let quotes = self.melt_quotes.lock().unwrap();
        let quote = quotes
            .get(quote_id)
            .ok_or_else(|| MintError::QuoteNotFound(quote_id.into()))?;
        Ok(MeltQuoteBolt11Response {
            quote: quote_id.into(),
            amount: quote.amount_sat,
            fee_reserve: quote.fee_reserve_sat,
            unit: "sat".into(),
            state: quote.state,
            expiry: quote.expiry,
        })
    }

    pub async fn melt(&self, req: MeltBolt11Request) -> Result<MeltBolt11Response> {
        let (amount_sat, fee_reserve_sat) = {
            let quotes = self.melt_quotes.lock().unwrap();
            let q = quotes
                .get(&req.quote)
                .ok_or_else(|| MintError::QuoteNotFound(req.quote.clone()))?;
            if q.state == QuoteState::Paid {
                return Err(MintError::QuoteAlreadyIssued(req.quote.clone()));
            }
            if Self::now() > q.expiry {
                return Err(MintError::InvalidRequest(format!(
                    "quote expired: {}",
                    req.quote
                )));
            }
            (q.amount_sat, q.fee_reserve_sat)
        };

        let input_total = self.verify_and_spend_inputs(&req.inputs)?;
        if input_total < amount_sat + fee_reserve_sat {
            return Err(MintError::Unbalanced {
                inputs: input_total,
                outputs: amount_sat + fee_reserve_sat,
            });
        }
        self.record_melt_burns(&req.inputs)?;

        // Payout via the ASP. In the scaffold this is a no-op "preimage";
        // real impl: Ark offboarding tx or LN payment through the ASP.
        let mut hasher = Sha256::new();
        hasher.update(req.quote.as_bytes());
        let preimage = hex::encode(hasher.finalize());

        // Release any token batch mappings whose quote ids match melted
        // batches. (Scaffold simplification: melt requests don't carry the
        // original mint quote id, so unmapping happens via /ark endpoints or
        // operator tooling; documented in README open questions.)

        self.melt_quotes
            .lock()
            .unwrap()
            .get_mut(&req.quote)
            .expect("quote exists")
            .state = QuoteState::Paid;

        Ok(MeltBolt11Response {
            quote: req.quote,
            state: QuoteState::Paid,
            payment_preimage: Some(preimage),
        })
    }

    // -- NUT-03: swap ---------------------------------------------------------

    pub async fn swap(&self, req: SwapRequest) -> Result<SwapResponse> {
        let output_total = Self::validate_outputs(&req.outputs)?;
        // Verify before spending so an unbalanced request doesn't burn inputs.
        {
            let spent = self.spent_secrets.lock().unwrap();
            for p in &req.inputs {
                if spent.contains(&p.secret) {
                    return Err(MintError::TokenAlreadySpent);
                }
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
            })?;
        }
        self.verify_and_spend_inputs(&req.inputs)?;
        let signatures = req.outputs.iter().map(Self::mock_blind_sign).collect();
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

        let txid = self.ark.unilateral_exit(&record.vtxo).await?;
        self.inventory
            .set_status(&mapping.vtxo_id, crate::types::VtxoStatus::Exited)?;
        self.inventory.release_vtxo_mapping(token_id)?;

        Ok(ExitResponse {
            token_id: token_id.to_string(),
            vtxo_id: mapping.vtxo_id,
            txid,
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
        let pol = self.pol.status()?;
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
            active_keysets: vec![KEYSET_ID.to_string()],
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
        let s = self.pol.status()?;
        Ok(PolStatusResponse {
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
        if self.pol.has_ots_stamp(epoch_day)? {
            return Ok(());
        }
        let Some(root) = self.pol.epoch_root_hash(epoch_day)? else {
            return Ok(());
        };
        let digest = digest_from_root_hex(&root)?;
        let stamp = ots.stamp_digest(digest).await?;
        self.pol.save_ots_stamp(epoch_day, &stamp.proof_hex, &stamp.calendar_url)?;
        tracing::info!(epoch = %epoch_day, calendar = %stamp.calendar_url, "PoL epoch stamped with OpenTimestamps");
        Ok(())
    }

    pub async fn stamp_pending_ots_epochs(&self) -> Result<usize> {
        let pending = self.pol.epochs_pending_ots()?;
        let mut done = 0;
        for day in pending {
            if self.stamp_epoch_ots(&day).await.is_ok() {
                done += 1;
            }
        }
        Ok(done)
    }

    pub fn pol_ots_proof(&self, epoch_day: &str) -> Result<PolOtsResponse> {
        let root = self
            .pol
            .epoch_root_hash(epoch_day)?
            .ok_or_else(|| MintError::InvalidRequest(format!("unknown epoch {epoch_day}")))?;
        let (proof_hex, calendar_url) = self
            .pol
            .ots_proof(epoch_day)?
            .ok_or_else(|| MintError::InvalidRequest(format!("no OTS proof for {epoch_day}")))?;
        let roots = self.pol.roots_for_keyset(KEYSET_ID)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ark_client::MockArkClient;

    fn test_config() -> AppConfig {
        let raw = include_str!("../config.toml");
        toml::from_str(raw).unwrap()
    }

    fn backend() -> MintBackend {
        let config = test_config();
        let ark = Arc::new(MockArkClient::new(config.ark.default_vtxo_expiry));
        let inventory = VtxoInventory::open_in_memory().unwrap();
        let pol = PolLedger::open_in_memory().unwrap();
        MintBackend::new(config, ark, inventory, pol, None)
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

        // 3. Melt with the new proofs. The scaffold derives the melt amount
        // from the request length: "lnbc1" -> 5 * 100 = 500 sat + 5 fee,
        // which the 1000 sat of swapped proofs comfortably covers.
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
    async fn stamp_epoch_ots_with_mock() {
        let config = test_config();
        let ark = Arc::new(MockArkClient::new(config.ark.default_vtxo_expiry));
        let inventory = VtxoInventory::open_in_memory().unwrap();
        let pol = PolLedger::open_in_memory().unwrap();
        let b = MintBackend::new(
            config,
            ark,
            inventory,
            pol,
            Some(Arc::new(crate::ots::MockOtsStamper)),
        );
        let day = PolLedger::current_epoch_day();
        b.pol().record_mint("q1", 8, &["02b".into()]).unwrap();
        b.pol().close_epoch(&day).unwrap();
        b.stamp_epoch_ots(&day).await.unwrap();
        assert!(b.pol().has_ots_stamp(&day).unwrap());
    }
}
