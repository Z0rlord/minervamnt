//! Core domain types: VTXOs and Cashu NUT wire formats.
//!
//! NUT request/response shapes follow https://github.com/cashubtc/nuts
//! (NUT-00 token formats, NUT-03 swap, NUT-04 mint, NUT-05 melt, NUT-06 info).

use serde::{Deserialize, Serialize};

/// Active mock keyset id (scaffold). Real deployment uses CDK-managed keysets.
pub const KEYSET_ID: &str = "00minerva0mock01";

// ---------------------------------------------------------------------------
// Ark types
// ---------------------------------------------------------------------------

/// A Virtual Transaction Output held with an Ark Service Provider (ASP).
///
/// `branch_tx` / `leaf_tx` are the pre-signed transactions that allow a
/// unilateral exit to L1 without ASP cooperation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vtxo {
    /// Unique VTXO identifier (outpoint-style id assigned by the ASP).
    pub id: String,
    /// Amount in millisatoshis.
    pub amount_msat: u64,
    /// Expiry as an absolute block height.
    pub expiry: u64,
    /// Pre-signed branch transaction (hex).
    pub branch_tx: String,
    /// Pre-signed leaf transaction (hex).
    pub leaf_tx: String,
    /// The ASP's public key (hex).
    pub asp_pubkey: String,
    /// Optional V-PACK bundle (hex) for libvpack verification in production.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vpack_hex: Option<String>,
}

/// Lifecycle status of a VTXO in the inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VtxoStatus {
    Active,
    Refreshing,
    Spent,
    Exited,
}

impl VtxoStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            VtxoStatus::Active => "active",
            VtxoStatus::Refreshing => "refreshing",
            VtxoStatus::Spent => "spent",
            VtxoStatus::Exited => "exited",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(VtxoStatus::Active),
            "refreshing" => Some(VtxoStatus::Refreshing),
            "spent" => Some(VtxoStatus::Spent),
            "exited" => Some(VtxoStatus::Exited),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Cashu NUT-00 primitives
// ---------------------------------------------------------------------------

/// NUT-00 `BlindedMessage`: an output the wallet wants signed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlindedMessage {
    /// Amount in the keyset unit (sats).
    pub amount: u64,
    /// Keyset id.
    pub id: String,
    /// Blinded secret point `B_` (33-byte compressed point, hex).
    #[serde(rename = "B_")]
    pub b: String,
}

/// NUT-00 `BlindSignature`: the mint's signature on a blinded message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlindSignature {
    pub amount: u64,
    /// Keyset id.
    pub id: String,
    /// Blinded signature point `C_` (hex).
    #[serde(rename = "C_")]
    pub c: String,
}

/// NUT-00 `Proof`: an unblinded token presented for melt/swap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proof {
    pub amount: u64,
    /// Keyset id.
    pub id: String,
    /// The secret message.
    pub secret: String,
    /// Unblinded signature point `C` (hex).
    #[serde(rename = "C")]
    pub c: String,
}

// ---------------------------------------------------------------------------
// NUT-04 mint (deposit)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MintQuoteBolt11Request {
    pub amount: u64,
    pub unit: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum QuoteState {
    Unpaid,
    Paid,
    Issued,
    Pending,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MintQuoteBolt11Response {
    pub quote: String,
    /// Payment request (bolt11 invoice; mocked in the scaffold).
    pub request: String,
    pub amount: u64,
    pub unit: String,
    pub state: QuoteState,
    /// Unix timestamp the quote expires.
    pub expiry: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MintBolt11Request {
    pub quote: String,
    pub outputs: Vec<BlindedMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MintBolt11Response {
    pub signatures: Vec<BlindSignature>,
}

// ---------------------------------------------------------------------------
// NUT-05 melt (withdrawal)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeltQuoteBolt11Request {
    /// bolt11 invoice (or, for Ark offboarding, an `ark:` address in future).
    pub request: String,
    pub unit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeltQuoteBolt11Response {
    pub quote: String,
    pub amount: u64,
    pub fee_reserve: u64,
    pub unit: String,
    pub state: QuoteState,
    pub expiry: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeltBolt11Request {
    pub quote: String,
    pub inputs: Vec<Proof>,
    /// Mint quote UUIDs whose VTXO backing should be released on successful melt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_ids: Option<Vec<String>>,
}

/// NUT-02 keyset entry returned by `GET /v1/keysets`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeysetInfo {
    pub id: String,
    pub unit: String,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_fee_ppk: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeysetsResponse {
    pub keysets: Vec<KeysetInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeltBolt11Response {
    pub quote: String,
    pub state: QuoteState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_preimage: Option<String>,
}

// ---------------------------------------------------------------------------
// NUT-03 swap
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapRequest {
    pub inputs: Vec<Proof>,
    pub outputs: Vec<BlindedMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapResponse {
    pub signatures: Vec<BlindSignature>,
}

// ---------------------------------------------------------------------------
// NUT-06 info
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MintInfo {
    pub name: String,
    pub pubkey: String,
    pub version: String,
    pub description: String,
    pub contact: Vec<ContactInfo>,
    pub motd: Option<String>,
    pub nuts: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactInfo {
    pub method: String,
    pub info: String,
}

// ---------------------------------------------------------------------------
// Ark extension endpoints
// ---------------------------------------------------------------------------

/// `GET /ark/vtxo/{token_id}` — which VTXO backs a given token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenVtxoResponse {
    pub token_id: String,
    pub vtxo: Vtxo,
    pub status: VtxoStatus,
}

/// `POST /ark/exit` — request a unilateral exit for a backing VTXO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitRequest {
    pub token_id: String,
}

/// Result of a unilateral exit through the Ark wallet daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitResult {
    /// First on-chain exit txid, or VTXO id while in progress.
    pub exit_txid: String,
    /// Exit phase: `started`, `processing`, `awaiting-delta`, `claimable`, `claimed`.
    pub phase: String,
    /// On-chain claim txid when `auto_claim` swept funds to `exit_claim_address`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_txid: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitResponse {
    pub token_id: String,
    pub vtxo_id: String,
    /// Primary exit identifier (claim txid when available, else exit txid).
    pub txid: String,
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_txid: Option<String>,
}

/// `GET /ark/refresh/status`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshStatusResponse {
    pub pending_refreshes: usize,
    pub next_expiry_height: Option<u64>,
    pub refresh_threshold_blocks: u64,
    pub current_block_height: u64,
}

// ---------------------------------------------------------------------------
// Transparency + PoL public responses
// ---------------------------------------------------------------------------

/// `GET /transparency/summary`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransparencySummary {
    pub mint_name: String,
    pub mint_url: String,
    pub version: String,
    pub git_commit: Option<String>,
    pub active_keysets: Vec<String>,
    pub vtxo_verify_mode: String,
    pub signatory_policy_enforced: bool,
    /// PoL cumulative outstanding ecash (sat).
    pub outstanding_ecash_sat: u64,
    /// Sum of active VTXO amounts (msat).
    pub active_vtxo_msat: u64,
    /// Sum mapped to outstanding tokens (msat).
    pub allocated_vtxo_msat: u64,
    /// Unallocated active reserve (msat).
    pub free_vtxo_msat: u64,
    pub pol_current_epoch: String,
    pub pol_last_closed_epoch: Option<String>,
    pub pol_last_closed_root: Option<String>,
    pub pol_last_ots_epoch: Option<String>,
    pub pol_last_ots_stamped_at: Option<u64>,
    pub ots_enabled: bool,
    pub refresh_pending: usize,
    pub next_vtxo_expiry_height: Option<u64>,
    pub solvency_ok: bool,
}

/// `GET /v1/pol/status`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolStatusResponse {
    pub current_epoch_day: String,
    pub open_mint_total_sat: u64,
    pub open_burn_total_sat: u64,
    pub outstanding_sat: u64,
    pub last_closed_epoch: Option<String>,
    pub last_closed_root: Option<String>,
    pub last_ots_stamped_epoch: Option<String>,
    pub last_ots_stamped_at: Option<u64>,
    pub ots_enabled: bool,
}

/// `GET /v1/pol/roots/{keyset_id}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolRootsResponse {
    pub keyset_id: String,
    pub roots: Vec<PolEpochRootResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolEpochRootResponse {
    pub epoch_day: String,
    pub mint_total_sat: u64,
    pub burn_total_sat: u64,
    pub outstanding_sat: u64,
    pub root_hash: String,
    pub prev_hash: Option<String>,
    pub ots_stamped: bool,
    pub ots_calendar_url: Option<String>,
    pub ots_stamped_at: Option<u64>,
}

/// `GET /v1/pol/ots/{epoch_day}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolOtsResponse {
    pub epoch_day: String,
    pub root_hash: String,
    pub proof_hex: String,
    pub calendar_url: String,
    pub stamped_at: u64,
}
