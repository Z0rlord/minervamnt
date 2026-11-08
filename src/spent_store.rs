//! Persistent stores on the mint's SQLite database:
//!
//! - the double-spend guard for spent Cashu proof secrets;
//! - the melt quote ledger backing the reserve/settle/release melt flow
//!   (see `docs/reserve-settle-receipts.md`).
//!
//! Proof secrets are rows in `spent_secrets` with a `state` of `spent`
//! (burned) or `pending` (reserved by an in-flight melt). Pending rows count
//! as spent for the double-spend guard, so reserved proofs cannot be swapped
//! or re-melted while a payout is unresolved. Melt quotes are persisted in
//! `melt_quotes` so a restart drops no reservation.

use crate::error::{MintError, Result};
use crate::types::{BlindSignature, BlindedMessage, MeltQuoteState, Proof};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::{Arc, Mutex};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS spent_secrets (
    secret        TEXT PRIMARY KEY,
    keyset_id     TEXT NOT NULL,
    amount_sat    INTEGER NOT NULL,
    spent_at      INTEGER NOT NULL,
    state         TEXT NOT NULL DEFAULT 'spent',
    pending_quote TEXT
);

CREATE INDEX IF NOT EXISTS idx_spent_keyset ON spent_secrets(keyset_id);

CREATE TABLE IF NOT EXISTS melt_quotes (
    quote_id        TEXT PRIMARY KEY,
    amount_sat      INTEGER NOT NULL,
    fee_reserve_sat INTEGER NOT NULL,
    request         TEXT NOT NULL,
    payment_hash    TEXT,
    state           TEXT NOT NULL,
    expiry          INTEGER NOT NULL,
    preimage        TEXT,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
"#;

/// Persisted melt quote with its reservation state.
#[derive(Debug, Clone)]
pub struct MeltQuoteRecord {
    pub quote_id: String,
    pub amount_sat: u64,
    pub fee_reserve_sat: u64,
    pub request: String,
    pub payment_hash: Option<String>,
    pub state: MeltQuoteState,
    pub expiry: u64,
    /// Payment preimage once the payout settled.
    pub preimage: Option<String>,
    /// NUT-08 blank outputs provided at reserve time (JSON), awaiting the
    /// fee return imprint. Persisted so a restart or a reconcile path still
    /// knows where change would go.
    pub change_outputs: Vec<BlindedMessage>,
    /// Actual payout fee measured by the wallet at settle; `None` when the
    /// wallet cannot report it (an unmeasured fee is never charged).
    pub fee_paid_sat: Option<u64>,
    /// Total value of the inputs reserved for this melt, recorded at
    /// reserve time: the change computation needs it after the inputs are
    /// burned and no longer attributable to the quote.
    pub input_total_sat: Option<u64>,
    /// Mint quote ids whose VTXO backing is released when the melt
    /// settles, recorded at reserve time so the reconcile and recovery
    /// paths can finish the same release.
    pub token_ids: Vec<String>,
    /// Whether the VTXO backing release for the net burn has completed.
    /// Set only after it has, so a crash before it retries on recovery.
    pub backing_released: bool,
    /// NUT-08 change signatures issued at settle (JSON). Persisted so the
    /// quote check endpoint can serve the change again after a lost
    /// response, a restart, or an operator reconcile. First write wins;
    /// they are never re-signed into different values.
    pub change_signatures: Vec<BlindSignature>,
}

#[derive(Clone)]
pub struct SpentSecretStore {
    conn: Arc<Mutex<Connection>>,
}

impl SpentSecretStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_connection(Connection::open(path)?, true)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?, false)
    }

    fn from_connection(conn: Connection, file_backed: bool) -> Result<Self> {
        // Several stores share one database file over separate connections;
        // wait briefly on lock contention instead of failing a settle or a
        // burn recording with SQLITE_BUSY mid-flow.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        if file_backed {
            conn.pragma_update(None, "journal_mode", "WAL")?;
        }
        conn.execute_batch(SCHEMA)?;
        // Migrations for databases created before the reserve/settle flow:
        // add the pending-reservation columns to an existing spent_secrets.
        Self::ensure_column(
            &conn,
            "spent_secrets",
            "state",
            "state TEXT NOT NULL DEFAULT 'spent'",
        )?;
        Self::ensure_column(
            &conn,
            "spent_secrets",
            "pending_quote",
            "pending_quote TEXT",
        )?;
        // Migrations for databases created before NUT-08 change support.
        Self::ensure_column(
            &conn,
            "melt_quotes",
            "change_outputs",
            "change_outputs TEXT",
        )?;
        Self::ensure_column(&conn, "melt_quotes", "fee_paid_sat", "fee_paid_sat INTEGER")?;
        Self::ensure_column(
            &conn,
            "melt_quotes",
            "input_total_sat",
            "input_total_sat INTEGER",
        )?;
        Self::ensure_column(
            &conn,
            "melt_quotes",
            "change_signatures",
            "change_signatures TEXT",
        )?;
        Self::ensure_column(&conn, "melt_quotes", "token_ids", "token_ids TEXT")?;
        let added_backing_released = Self::ensure_column(
            &conn,
            "melt_quotes",
            "backing_released",
            "backing_released INTEGER NOT NULL DEFAULT 0",
        )?;
        if added_backing_released {
            // Quotes settled before the flag existed were finalized by the
            // code that settled them; do not re-release their backing.
            conn.execute(
                "UPDATE melt_quotes SET backing_released = 1 WHERE state = ?1",
                params![MeltQuoteState::Settled.as_str()],
            )?;
        }
        Ok(SpentSecretStore {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Add a column when missing; returns whether it was added.
    fn ensure_column(conn: &Connection, table: &str, column: &str, ddl: &str) -> Result<bool> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get(1))?
            .collect::<std::result::Result<_, _>>()?;
        if !cols.iter().any(|c| c == column) {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {ddl}"))?;
            return Ok(true);
        }
        Ok(false)
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// Any record of the secret, burned or reserved: both block reuse.
    pub fn is_spent(&self, secret: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(1) FROM spent_secrets WHERE secret = ?1",
            params![secret],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn any_spent(&self, secrets: &[String]) -> Result<bool> {
        if secrets.is_empty() {
            return Ok(false);
        }
        let conn = self.conn.lock().unwrap();
        for secret in secrets {
            let n: i64 = conn.query_row(
                "SELECT COUNT(1) FROM spent_secrets WHERE secret = ?1",
                params![secret],
                |r| r.get(0),
            )?;
            if n > 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Mark all proof secrets spent atomically. Fails if any secret is already spent.
    pub fn mark_spent_batch(&self, keyset_id: &str, proofs: &[Proof]) -> Result<()> {
        if proofs.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;
        for proof in proofs {
            let n: i64 = tx.query_row(
                "SELECT COUNT(1) FROM spent_secrets WHERE secret = ?1",
                params![proof.secret],
                |r| r.get(0),
            )?;
            if n > 0 {
                return Err(MintError::TokenAlreadySpent);
            }
        }
        let spent_at = Self::now() as i64;
        for proof in proofs {
            tx.execute(
                "INSERT INTO spent_secrets (secret, keyset_id, amount_sat, spent_at, state)
                 VALUES (?1, ?2, ?3, ?4, 'spent')",
                params![
                    proof.secret,
                    keyset_id,
                    proof.amount as i64,
                    spent_at,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    // -- Melt quotes: reserve / settle / release -----------------------------

    pub fn insert_melt_quote(&self, record: &MeltQuoteRecord) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = Self::now() as i64;
        conn.execute(
            "INSERT INTO melt_quotes
             (quote_id, amount_sat, fee_reserve_sat, request, payment_hash, state, expiry, preimage,
              change_outputs, fee_paid_sat, input_total_sat, change_signatures, token_ids,
              backing_released, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, NULL, NULL, NULL, NULL, 0, ?9, ?9)",
            params![
                record.quote_id,
                record.amount_sat as i64,
                record.fee_reserve_sat as i64,
                record.request,
                record.payment_hash,
                record.state.as_str(),
                record.expiry as i64,
                serde_json::to_string(&record.change_outputs).unwrap_or_else(|_| "[]".into()),
                now,
            ],
        )?;
        Ok(())
    }

    pub fn melt_quote(&self, quote_id: &str) -> Result<Option<MeltQuoteRecord>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT quote_id, amount_sat, fee_reserve_sat, request, payment_hash, state, expiry, preimage,
                    change_outputs, fee_paid_sat, input_total_sat, change_signatures,
                    token_ids, backing_released
             FROM melt_quotes WHERE quote_id = ?1",
            params![quote_id],
            Self::row_to_melt_quote,
        )
        .optional()
        .map_err(Into::into)
    }

    fn row_to_melt_quote(r: &rusqlite::Row) -> rusqlite::Result<MeltQuoteRecord> {
        let change_outputs: Option<String> = r.get(8)?;
        let change_outputs = change_outputs
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        let change_signatures: Option<String> = r.get(11)?;
        let change_signatures = change_signatures
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        let token_ids: Option<String> = r.get(12)?;
        let token_ids = token_ids
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        Ok(MeltQuoteRecord {
            quote_id: r.get(0)?,
            amount_sat: r.get::<_, i64>(1)? as u64,
            fee_reserve_sat: r.get::<_, i64>(2)? as u64,
            request: r.get(3)?,
            payment_hash: r.get(4)?,
            state: MeltQuoteState::from_str(&r.get::<_, String>(5)?).ok_or_else(|| {
                rusqlite::Error::InvalidColumnType(5, "state".into(), rusqlite::types::Type::Text)
            })?,
            expiry: r.get::<_, i64>(6)? as u64,
            preimage: r.get(7)?,
            change_outputs,
            fee_paid_sat: r.get::<_, Option<i64>>(9)?.map(|v| v as u64),
            input_total_sat: r.get::<_, Option<i64>>(10)?.map(|v| v as u64),
            change_signatures,
            token_ids,
            backing_released: r.get::<_, i64>(13)? != 0,
        })
    }

    /// All melt quotes currently RESERVED (payout in flight or outcome
    /// unknown), oldest first, for operator reconciliation.
    pub fn list_reserved_melts(&self) -> Result<Vec<MeltQuoteRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT quote_id, amount_sat, fee_reserve_sat, request, payment_hash, state, expiry, preimage,
                    change_outputs, fee_paid_sat, input_total_sat, change_signatures,
                    token_ids, backing_released
             FROM melt_quotes WHERE state = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(
            params![MeltQuoteState::Reserved.as_str()],
            Self::row_to_melt_quote,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Settled melt quotes whose post-settle finalization is incomplete
    /// (backing not released, or blank outputs still unsigned), for the
    /// startup recovery sweep.
    pub fn list_pending_finalize_melts(&self) -> Result<Vec<MeltQuoteRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT quote_id, amount_sat, fee_reserve_sat, request, payment_hash, state, expiry, preimage,
                    change_outputs, fee_paid_sat, input_total_sat, change_signatures,
                    token_ids, backing_released
             FROM melt_quotes
             WHERE state = ?1
               AND (backing_released = 0
                    OR (change_outputs IS NOT NULL AND change_outputs != '[]'
                        AND (change_signatures IS NULL OR change_signatures = '[]')))
             ORDER BY created_at",
        )?;
        let rows = stmt.query_map(
            params![MeltQuoteState::Settled.as_str()],
            Self::row_to_melt_quote,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Blank change outputs persisted at reserve time, for change signing
    /// after a settled payout (including after a restart).
    pub fn change_outputs(&self, quote_id: &str) -> Result<Vec<BlindedMessage>> {
        Ok(self
            .melt_quote(quote_id)?
            .map(|q| q.change_outputs)
            .unwrap_or_default())
    }

    /// Change signatures persisted at settle time, served by the quote
    /// check endpoint so a lost melt response does not lose the change.
    pub fn change_signatures(&self, quote_id: &str) -> Result<Vec<BlindSignature>> {
        Ok(self
            .melt_quote(quote_id)?
            .map(|q| q.change_signatures)
            .unwrap_or_default())
    }

    /// Mark the quote's VTXO backing release as completed. Set only after
    /// the release actually happened, so a crash before it retries.
    pub fn mark_backing_released(&self, quote_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE melt_quotes SET backing_released = 1, updated_at = ?2 WHERE quote_id = ?1",
            params![quote_id, Self::now() as i64],
        )?;
        Ok(())
    }

    /// Persist the NUT-08 change signatures issued for a quote. First
    /// write wins: re-signing the same blank outputs is deterministic, so
    /// an identical rewrite is a no-op, but a conflicting write would mean
    /// the same outputs signed twice with different values - double
    /// issuance - and is refused.
    pub fn store_change_signatures(
        &self,
        quote_id: &str,
        signatures: &[BlindSignature],
    ) -> Result<()> {
        let existing = self
            .melt_quote(quote_id)?
            .ok_or_else(|| MintError::QuoteNotFound(quote_id.to_string()))?;
        if !existing.change_signatures.is_empty() {
            let old = serde_json::to_string(&existing.change_signatures)
                .unwrap_or_else(|_| "[]".into());
            let new = serde_json::to_string(signatures).unwrap_or_else(|_| "[]".into());
            if old == new {
                return Ok(());
            }
            return Err(MintError::InvalidRequest(format!(
                "conflicting change signatures for quote {quote_id}; refusing to re-issue change"
            )));
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE melt_quotes SET change_signatures = ?2, updated_at = ?3 WHERE quote_id = ?1",
            params![
                quote_id,
                serde_json::to_string(signatures).unwrap_or_else(|_| "[]".into()),
                Self::now() as i64
            ],
        )?;
        Ok(())
    }

    /// Whether a SETTLED melt exists for this payment hash: an invoice the
    /// mint already paid cannot be paid again.
    pub fn has_settled_melt_for_hash(&self, payment_hash: &str) -> Result<bool> {
        self.has_settled_melt_for_hash_excluding(payment_hash, "")
    }

    /// Same as [`Self::has_settled_melt_for_hash`], excluding one quote.
    /// Reconcile-settle uses this so a settled sibling blocks settling a
    /// second quote for the same payment.
    pub fn has_settled_melt_for_hash_excluding(
        &self,
        payment_hash: &str,
        exclude_quote_id: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(1) FROM melt_quotes
             WHERE payment_hash = ?1 AND state = ?2 AND quote_id != ?3",
            params![
                payment_hash,
                MeltQuoteState::Settled.as_str(),
                exclude_quote_id
            ],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Inputs currently reserved to a melt quote, as
    /// (secret, keyset_id, amount_sat), for operator settle reconciliation.
    pub fn reserved_inputs(&self, quote_id: &str) -> Result<Vec<(String, String, u64)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT secret, keyset_id, amount_sat FROM spent_secrets
             WHERE pending_quote = ?1 AND state = 'pending'",
        )?;
        let rows = stmt.query_map(params![quote_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? as u64,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Reserve a melt: move the quote UNPAID -> RESERVED, mark the inputs
    /// pending, and persist the NUT-08 blank change outputs in one
    /// transaction, so a concurrent or repeated melt of the same quote or
    /// proofs cannot race into a second payout.
    ///
    /// Returns whether this payment hash was attempted before (a sibling
    /// quote reached RESERVED/SETTLED/RELEASED, all of which imply a
    /// dispatch). The caller uses this mint-side fact to classify a
    /// "failed" wallet status conservatively, without trusting the
    /// wallet's history to be complete.
    pub fn reserve_melt(
        &self,
        quote_id: &str,
        keyset_id: &str,
        proofs: &[Proof],
        change_outputs: &[BlindedMessage],
        token_ids: &[String],
    ) -> Result<bool> {
        if proofs.is_empty() {
            return Err(MintError::InvalidRequest("no inputs provided".into()));
        }
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;
        let (state, payment_hash): (String, Option<String>) = tx
            .query_row(
                "SELECT state, payment_hash FROM melt_quotes WHERE quote_id = ?1",
                params![quote_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| MintError::QuoteNotFound(quote_id.to_string()))?;
        match MeltQuoteState::from_str(&state) {
            Some(MeltQuoteState::Unpaid) => {}
            Some(MeltQuoteState::Settled) => {
                return Err(MintError::QuoteAlreadyIssued(quote_id.to_string()))
            }
            Some(MeltQuoteState::Reserved) => {
                return Err(MintError::InvalidRequest(format!(
                    "melt already in progress for quote {quote_id}; payout outcome is held for reconciliation"
                )))
            }
            Some(MeltQuoteState::Released) => {
                return Err(MintError::InvalidRequest(format!(
                    "quote {quote_id} was released after a failed payout; request a new quote"
                )))
            }
            None => {
                return Err(MintError::Database(rusqlite::Error::InvalidQuery))
            }
        }
        // Per-payment-hash guards. Two RESERVED quotes for the same invoice
        // could both settle for a single payment, and reserving on top of an
        // already-SETTLED sibling would dispatch a second payout for an
        // invoice the mint already paid. Mock-mode quotes carry no payment
        // hash and skip these checks.
        let mut attempted_before = false;
        if let Some(hash) = &payment_hash {
            let sibling_states: i64 = tx.query_row(
                "SELECT COUNT(1) FROM melt_quotes
                 WHERE payment_hash = ?1 AND quote_id != ?2
                   AND state IN (?3, ?4, ?5)",
                params![
                    hash,
                    quote_id,
                    MeltQuoteState::Reserved.as_str(),
                    MeltQuoteState::Settled.as_str(),
                    MeltQuoteState::Released.as_str(),
                ],
                |r| r.get(0),
            )?;
            attempted_before = sibling_states > 0;
            let settled_siblings: i64 = tx.query_row(
                "SELECT COUNT(1) FROM melt_quotes
                 WHERE payment_hash = ?1 AND quote_id != ?2 AND state = ?3",
                params![hash, quote_id, MeltQuoteState::Settled.as_str()],
                |r| r.get(0),
            )?;
            if settled_siblings > 0 {
                return Err(MintError::InvalidRequest(
                    "invoice already paid by a settled melt".into(),
                ));
            }
            let reserved_siblings: i64 = tx.query_row(
                "SELECT COUNT(1) FROM melt_quotes
                 WHERE payment_hash = ?1 AND quote_id != ?2 AND state = ?3",
                params![hash, quote_id, MeltQuoteState::Reserved.as_str()],
                |r| r.get(0),
            )?;
            if reserved_siblings > 0 {
                return Err(MintError::InvalidRequest(
                    "another melt for this invoice is already in flight".into(),
                ));
            }
        }
        for proof in proofs {
            let n: i64 = tx.query_row(
                "SELECT COUNT(1) FROM spent_secrets WHERE secret = ?1",
                params![proof.secret],
                |r| r.get(0),
            )?;
            if n > 0 {
                return Err(MintError::TokenAlreadySpent);
            }
        }
        let now = Self::now() as i64;
        for proof in proofs {
            tx.execute(
                "INSERT INTO spent_secrets (secret, keyset_id, amount_sat, spent_at, state, pending_quote)
                 VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
                params![proof.secret, keyset_id, proof.amount as i64, now, quote_id],
            )?;
        }
        let input_total: u64 = proofs.iter().map(|p| p.amount).sum();
        tx.execute(
            "UPDATE melt_quotes SET state = ?2, change_outputs = ?3, input_total_sat = ?4, token_ids = ?5, updated_at = ?6 WHERE quote_id = ?1",
            params![
                quote_id,
                MeltQuoteState::Reserved.as_str(),
                serde_json::to_string(change_outputs).unwrap_or_else(|_| "[]".into()),
                input_total as i64,
                serde_json::to_string(token_ids).unwrap_or_else(|_| "[]".into()),
                now
            ],
        )?;
        tx.commit()?;
        Ok(attempted_before)
    }

    /// Settle a RESERVED melt after a confirmed payout: burn the reserved
    /// inputs and record the preimage and the measured payout fee (when the
    /// wallet reports one). Only valid from RESERVED.
    pub fn settle_melt(
        &self,
        quote_id: &str,
        preimage: &str,
        fee_paid_sat: Option<u64>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;
        Self::require_reserved(&tx, quote_id)?;
        let now = Self::now() as i64;
        tx.execute(
            "UPDATE spent_secrets SET state = 'spent', pending_quote = NULL
             WHERE pending_quote = ?1 AND state = 'pending'",
            params![quote_id],
        )?;
        tx.execute(
            "UPDATE melt_quotes SET state = ?2, preimage = ?3, fee_paid_sat = ?4, updated_at = ?5 WHERE quote_id = ?1",
            params![
                quote_id,
                MeltQuoteState::Settled.as_str(),
                preimage,
                fee_paid_sat.map(|v| v as i64),
                now
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Release a RESERVED melt after the payout never started or definitively
    /// failed: unmark the reserved inputs so they are spendable again. Only
    /// valid from RESERVED.
    pub fn release_melt(&self, quote_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;
        Self::require_reserved(&tx, quote_id)?;
        let now = Self::now() as i64;
        tx.execute(
            "DELETE FROM spent_secrets WHERE pending_quote = ?1 AND state = 'pending'",
            params![quote_id],
        )?;
        tx.execute(
            "UPDATE melt_quotes SET state = ?2, updated_at = ?3 WHERE quote_id = ?1",
            params![quote_id, MeltQuoteState::Released.as_str(), now],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn require_reserved(tx: &Connection, quote_id: &str) -> Result<()> {
        let state: Option<String> = tx
            .query_row(
                "SELECT state FROM melt_quotes WHERE quote_id = ?1",
                params![quote_id],
                |r| r.get(0),
            )
            .optional()?;
        match state.as_deref().and_then(MeltQuoteState::from_str) {
            Some(MeltQuoteState::Reserved) => Ok(()),
            Some(_) => Err(MintError::InvalidRequest(format!(
                "melt quote {quote_id} is not reserved"
            ))),
            None => Err(MintError::QuoteNotFound(quote_id.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Proof;

    fn proof(secret: &str, amount: u64) -> Proof {
        Proof {
            amount,
            id: crate::types::KEYSET_ID.to_string(),
            secret: secret.into(),
            c: "02deadbeef".into(),
        }
    }

    fn quote(quote_id: &str) -> MeltQuoteRecord {
        MeltQuoteRecord {
            quote_id: quote_id.into(),
            amount_sat: 500,
            fee_reserve_sat: 5,
            request: "lnbc1".into(),
            payment_hash: None,
            state: MeltQuoteState::Unpaid,
            expiry: 9_999_999_999,
            preimage: None,
            change_outputs: vec![],
            fee_paid_sat: None,
            input_total_sat: None,
            change_signatures: vec![],
            token_ids: vec![],
            backing_released: false,
        }
    }

    #[test]
    fn mark_and_detect_spent_secret() {
        let store = SpentSecretStore::open_in_memory().unwrap();
        assert!(!store.is_spent("s1").unwrap());
        store
            .mark_spent_batch(crate::types::KEYSET_ID, &[proof("s1", 64)])
            .unwrap();
        assert!(store.is_spent("s1").unwrap());
        assert!(store
            .mark_spent_batch(crate::types::KEYSET_ID, &[proof("s1", 64)])
            .is_err());
    }

    #[test]
    fn batch_is_atomic() {
        let store = SpentSecretStore::open_in_memory().unwrap();
        store
            .mark_spent_batch(crate::types::KEYSET_ID, &[proof("s1", 32)])
            .unwrap();
        let err = store
            .mark_spent_batch(crate::types::KEYSET_ID, &[proof("s2", 32), proof("s1", 32)])
            .unwrap_err();
        assert!(matches!(err, MintError::TokenAlreadySpent));
        assert!(!store.is_spent("s2").unwrap());
    }

    #[test]
    fn reserve_settle_marks_inputs_spent() {
        let store = SpentSecretStore::open_in_memory().unwrap();
        store.insert_melt_quote(&quote("q1")).unwrap();
        store
            .reserve_melt("q1", crate::types::KEYSET_ID, &[proof("s1", 512)], &[], &[])
            .unwrap();
        // Reserved proofs count as spent for the double-spend guard.
        assert!(store.is_spent("s1").unwrap());
        // Settle only valid from RESERVED.
        store
            .settle_melt("q1", "aa".repeat(32).as_str(), None)
            .unwrap();
        let q = store.melt_quote("q1").unwrap().unwrap();
        assert_eq!(q.state, MeltQuoteState::Settled);
        assert!(q.preimage.is_some());
        // Still spent afterwards, and the reservation cannot be released now.
        assert!(store.is_spent("s1").unwrap());
        assert!(store.release_melt("q1").is_err());
    }

    #[test]
    fn reserve_release_frees_inputs() {
        let store = SpentSecretStore::open_in_memory().unwrap();
        store.insert_melt_quote(&quote("q1")).unwrap();
        store
            .reserve_melt("q1", crate::types::KEYSET_ID, &[proof("s1", 512)], &[], &[])
            .unwrap();
        store.release_melt("q1").unwrap();
        let q = store.melt_quote("q1").unwrap().unwrap();
        assert_eq!(q.state, MeltQuoteState::Released);
        assert!(!store.is_spent("s1").unwrap());
        // Released quote cannot be reserved again.
        assert!(store
            .reserve_melt("q1", crate::types::KEYSET_ID, &[proof("s2", 512)], &[], &[])
            .is_err());
    }

    #[test]
    fn double_reserve_rejected() {
        let store = SpentSecretStore::open_in_memory().unwrap();
        store.insert_melt_quote(&quote("q1")).unwrap();
        store
            .reserve_melt("q1", crate::types::KEYSET_ID, &[proof("s1", 512)], &[], &[])
            .unwrap();
        // Same quote again.
        assert!(store
            .reserve_melt("q1", crate::types::KEYSET_ID, &[proof("s2", 512)], &[], &[])
            .is_err());
        // Reserved proofs on another quote.
        store.insert_melt_quote(&quote("q2")).unwrap();
        assert!(matches!(
            store
                .reserve_melt("q2", crate::types::KEYSET_ID, &[proof("s1", 512)], &[], &[])
                .unwrap_err(),
            MintError::TokenAlreadySpent
        ));
    }

    #[test]
    fn change_outputs_and_fee_persist_with_quote() {
        let store = SpentSecretStore::open_in_memory().unwrap();
        store.insert_melt_quote(&quote("q1")).unwrap();
        let blanks = vec![crate::types::BlindedMessage {
            amount: 0,
            id: crate::types::KEYSET_ID.to_string(),
            b: "02blank1".into(),
        }];
        store
            .reserve_melt("q1", crate::types::KEYSET_ID, &[proof("s1", 512)], &blanks, &[])
            .unwrap();
        let q = store.melt_quote("q1").unwrap().unwrap();
        assert_eq!(q.change_outputs.len(), 1);
        assert_eq!(q.change_outputs[0].b, "02blank1");
        assert_eq!(q.fee_paid_sat, None);

        store.settle_melt("q1", "pre", Some(3)).unwrap();
        let q = store.melt_quote("q1").unwrap().unwrap();
        assert_eq!(q.fee_paid_sat, Some(3));
        // The blank outputs survive settlement for audit.
        assert_eq!(q.change_outputs.len(), 1);
    }

    #[test]
    fn reserve_records_input_total() {
        // The change computation needs the reserved input total after the
        // inputs are burned; it is recorded at reserve time.
        let store = SpentSecretStore::open_in_memory().unwrap();
        store.insert_melt_quote(&quote("q1")).unwrap();
        store
            .reserve_melt(
                "q1",
                crate::types::KEYSET_ID,
                &[proof("s1", 512), proof("s2", 512)],
                &[],
                &[],
            )
            .unwrap();
        let q = store.melt_quote("q1").unwrap().unwrap();
        assert_eq!(q.input_total_sat, Some(1024));
    }

    #[test]
    fn change_signatures_first_write_wins() {
        use crate::types::{BlindSignature, KEYSET_ID};
        let store = SpentSecretStore::open_in_memory().unwrap();
        store.insert_melt_quote(&quote("q1")).unwrap();
        let sig = BlindSignature {
            amount: 8,
            id: KEYSET_ID.to_string(),
            c: "02sig".into(),
        };
        store.store_change_signatures("q1", &[sig.clone()]).unwrap();
        // An identical rewrite (deterministic re-signing) is a no-op.
        store.store_change_signatures("q1", &[sig.clone()]).unwrap();
        assert_eq!(store.change_signatures("q1").unwrap().len(), 1);
        // A conflicting write would double issue: refused, first kept.
        let other = BlindSignature {
            amount: 16,
            ..sig
        };
        assert!(store.store_change_signatures("q1", &[other]).is_err());
        assert_eq!(store.change_signatures("q1").unwrap()[0].amount, 8);
    }

    #[test]
    fn opens_pre_reservation_schema_and_migrates_it() {
        // A database created before the reserve/settle flow has
        // spent_secrets without the state/pending_quote columns.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mint.sqlite");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE spent_secrets (
                    secret      TEXT PRIMARY KEY,
                    keyset_id   TEXT NOT NULL,
                    amount_sat  INTEGER NOT NULL,
                    spent_at    INTEGER NOT NULL
                );
                INSERT INTO spent_secrets (secret, keyset_id, amount_sat, spent_at)
                VALUES ('s0', 'k', 64, 1);",
            )
            .unwrap();
        }
        let store = SpentSecretStore::open(&db).unwrap();
        // Old rows still count as spent; reservations work on the migrated db.
        assert!(store.is_spent("s0").unwrap());
        store.insert_melt_quote(&quote("q1")).unwrap();
        store
            .reserve_melt("q1", crate::types::KEYSET_ID, &[proof("s1", 512)], &[], &[])
            .unwrap();
        store.release_melt("q1").unwrap();
        assert!(!store.is_spent("s1").unwrap());
    }

    #[test]
    fn one_reserved_melt_per_payment_hash() {
        let store = SpentSecretStore::open_in_memory().unwrap();
        let mut q1 = quote("q1");
        q1.payment_hash = Some("aa".repeat(32));
        let mut q2 = quote("q2");
        q2.payment_hash = Some("aa".repeat(32));
        store.insert_melt_quote(&q1).unwrap();
        store.insert_melt_quote(&q2).unwrap();
        assert!(
            !store
                .reserve_melt("q1", crate::types::KEYSET_ID, &[proof("s1", 512)], &[], &[])
                .unwrap(),
            "first attempt for a hash is not a re-attempt"
        );
        // Same payment hash, other quote: rejected while q1 is RESERVED.
        assert!(matches!(
            store
                .reserve_melt("q2", crate::types::KEYSET_ID, &[proof("s2", 512)], &[], &[])
                .unwrap_err(),
            MintError::InvalidRequest(_)
        ));
        // After release the hash is free again, but the re-attempt is
        // flagged: a "failed" wallet status must not auto-release it.
        store.release_melt("q1").unwrap();
        assert!(
            store
                .reserve_melt("q2", crate::types::KEYSET_ID, &[proof("s2", 512)], &[], &[])
                .unwrap(),
            "a released sibling means this hash was attempted before"
        );
        // Settled-hash detection.
        store.settle_melt("q2", "pre", None).unwrap();
        assert!(store.has_settled_melt_for_hash(&"aa".repeat(32)).unwrap());
        assert!(!store.has_settled_melt_for_hash(&"bb".repeat(32)).unwrap());
    }

    #[test]
    fn settled_sibling_rejected_at_reserve() {
        let store = SpentSecretStore::open_in_memory().unwrap();
        let mut q1 = quote("q1");
        q1.payment_hash = Some("cc".repeat(32));
        let mut q2 = quote("q2");
        q2.payment_hash = Some("cc".repeat(32));
        store.insert_melt_quote(&q1).unwrap();
        store.insert_melt_quote(&q2).unwrap();
        store
            .reserve_melt("q1", crate::types::KEYSET_ID, &[proof("s1", 512)], &[], &[])
            .unwrap();
        store.settle_melt("q1", "pre", None).unwrap();
        // Reserving q2 on top of a settled sibling is refused, inputs
        // untouched.
        assert!(matches!(
            store
                .reserve_melt("q2", crate::types::KEYSET_ID, &[proof("s2", 512)], &[], &[])
                .unwrap_err(),
            MintError::InvalidRequest(_)
        ));
        assert_eq!(
            store.melt_quote("q2").unwrap().unwrap().state,
            MeltQuoteState::Unpaid
        );
        assert!(!store.is_spent("s2").unwrap());
        // The excluding variant sees q1's settle from q2's perspective.
        assert!(store
            .has_settled_melt_for_hash_excluding(&"cc".repeat(32), "q2")
            .unwrap());
        assert!(!store
            .has_settled_melt_for_hash_excluding(&"cc".repeat(32), "q1")
            .unwrap());
    }

    #[test]
    fn melt_quotes_and_reservations_persist_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mint.sqlite");
        {
            let store = SpentSecretStore::open(&db).unwrap();
            store.insert_melt_quote(&quote("q1")).unwrap();
            store
                .reserve_melt("q1", crate::types::KEYSET_ID, &[proof("s1", 512)], &[], &[])
                .unwrap();
        }
        let store = SpentSecretStore::open(&db).unwrap();
        let q = store.melt_quote("q1").unwrap().unwrap();
        assert_eq!(q.state, MeltQuoteState::Reserved);
        assert_eq!(q.amount_sat, 500);
        assert!(store.is_spent("s1").unwrap());
    }
}
