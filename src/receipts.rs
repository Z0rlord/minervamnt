//! Signed mint/melt receipts (docs/reserve-settle-receipts.md).
//!
//! A receipt is the mint's signed statement of one quote's outcome: a mint
//! issuance, a melt settlement (with the fee actually charged and the
//! NUT-08 change returned), or a melt release. Receipts form a hash chain
//! (`prev_hash`), and each receipt hash is recorded in the PoL ledger's
//! epoch events so a receipt can be checked against a published, optionally
//! OpenTimestamps-stamped epoch root.
//!
//! Receipts are evidence of what the mint says it did. They are not an
//! independent solvency attestation.
//!
//! Signing key: a BIP-340 schnorr key separate from the BDHKE mint keysets,
//! loaded from the environment variable named by `[receipts].key_env`
//! (default `RECEIPT_SIGNING_KEY_HEX`, 32-byte hex). When the variable is
//! unset, a deterministic dev key is used only against the mock ASP (tests
//! and local dev); any real backend fails closed, because the dev key's
//! secret is derived from a public constant. The public key is published
//! in `GET /v1/info` as `receipt_pubkey` and embedded in each receipt.

use crate::error::{MintError, Result};
use crate::types::MintReceipt;
use k256::schnorr::signature::{Signer, Verifier};
use k256::schnorr::{Signature, SigningKey, VerifyingKey};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS receipts (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    receipt_id      TEXT NOT NULL UNIQUE,
    kind            TEXT NOT NULL,
    quote_id        TEXT NOT NULL,
    unit            TEXT NOT NULL,
    amount_sat      INTEGER NOT NULL,
    fee_charged_sat INTEGER,
    retained_sat    INTEGER,
    change_sat      INTEGER,
    outcome         TEXT NOT NULL,
    keyset_id       TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    prev_hash       TEXT NOT NULL,
    receipt_hash    TEXT NOT NULL UNIQUE,
    signature       TEXT NOT NULL,
    pubkey          TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_receipts_quote ON receipts(quote_id);

-- A quote settles or releases exactly once: the terminal-outcome receipt is
-- unique per quote, so a racing or repeated finalization cannot append a
-- duplicate (append returns the existing receipt instead).
CREATE UNIQUE INDEX IF NOT EXISTS idx_receipts_terminal_outcome
    ON receipts(quote_id, kind, outcome) WHERE outcome IN ('settled','released');
"#;

/// Schnorr signing key for receipts.
pub struct ReceiptSigner {
    key: SigningKey,
}

impl ReceiptSigner {
    /// Load the key from the environment variable `env_var` (32-byte hex).
    /// The deterministic dev key fallback is only allowed when `allow_dev`
    /// is set (mock backends, tests, local dev): its secret is derived from
    /// a public constant, so in any real deployment a missing key must fail
    /// closed rather than sign receipts anyone can forge.
    pub fn from_env_or_dev(env_var: &str, allow_dev: bool) -> Result<Self> {
        match std::env::var(env_var) {
            Ok(hex_key) => Self::from_hex(&hex_key),
            Err(_) if allow_dev => {
                tracing::warn!(
                    env_var,
                    "no receipt signing key configured; using deterministic dev key"
                );
                Ok(Self::dev())
            }
            Err(_) => Err(MintError::InvalidRequest(format!(
                "receipt signing key env var {env_var} is not set; refusing the public dev key outside mock mode"
            ))),
        }
    }

    pub fn from_hex(hex_key: &str) -> Result<Self> {
        let bytes = hex::decode(hex_key.trim())
            .map_err(|e| MintError::InvalidRequest(format!("invalid receipt key hex: {e}")))?;
        let key = SigningKey::from_bytes(&bytes)
            .map_err(|e| MintError::InvalidRequest(format!("invalid receipt signing key: {e}")))?;
        Ok(Self { key })
    }

    /// Deterministic dev/test key. Its secret is derived from a public
    /// constant, so receipts signed with it prove nothing about a real mint.
    pub fn dev() -> Self {
        let seed = Sha256::digest(b"minerva-mint dev receipt signing key");
        Self {
            key: SigningKey::from_bytes(&seed).expect("sha256 output is a valid scalar"),
        }
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        *self.key.verifying_key()
    }

    /// X-only (32-byte) public key, hex (BIP-340).
    pub fn pubkey_hex(&self) -> String {
        hex::encode(self.verifying_key().to_bytes())
    }

    pub fn sign_hash(&self, hash: &[u8; 32]) -> String {
        let sig: Signature = self.key.sign(hash);
        hex::encode(sig.to_bytes())
    }

    /// Verify a receipt signature against this signer's key.
    pub fn verify_hash(&self, hash: &[u8; 32], sig_hex: &str) -> bool {
        let Ok(bytes) = hex::decode(sig_hex) else {
            return false;
        };
        let Ok(sig) = Signature::try_from(bytes.as_slice()) else {
            return false;
        };
        self.verifying_key().verify(hash, &sig).is_ok()
    }
}

/// Canonical receipt payload: the exact bytes hashed into `receipt_hash`.
/// Field order is fixed; optional numerics render as `-` when absent.
pub fn canonical_receipt_payload(
    kind: &str,
    quote_id: &str,
    unit: &str,
    amount_sat: u64,
    fee_charged_sat: Option<u64>,
    retained_sat: Option<u64>,
    change_sat: Option<u64>,
    outcome: &str,
    keyset_id: &str,
    created_at: u64,
    prev_hash: &str,
) -> String {
    let opt = |v: Option<u64>| v.map(|n| n.to_string()).unwrap_or_else(|| "-".into());
    format!(
        "minerva-receipt-v1\n{kind}\n{quote_id}\n{unit}\n{amount_sat}\n{}\n{}\n{}\n{outcome}\n{keyset_id}\n{created_at}\n{prev_hash}",
        opt(fee_charged_sat),
        opt(retained_sat),
        opt(change_sat),
    )
}

/// Verify a receipt's hash and signature against a verifier's public key.
pub fn verify_receipt(receipt: &MintReceipt, pubkey_hex: &str) -> bool {
    let Ok(pk_bytes) = hex::decode(pubkey_hex) else {
        return false;
    };
    let Ok(key) = VerifyingKey::from_bytes(&pk_bytes) else {
        return false;
    };
    let payload = canonical_receipt_payload(
        &receipt.kind,
        &receipt.quote_id,
        &receipt.unit,
        receipt.amount_sat,
        receipt.fee_charged_sat,
        receipt.retained_sat,
        receipt.change_sat,
        &receipt.outcome,
        &receipt.keyset_id,
        receipt.created_at,
        &receipt.prev_hash,
    );
    let hash: [u8; 32] = Sha256::digest(payload.as_bytes()).into();
    if hex::encode(hash) != receipt.receipt_hash {
        return false;
    }
    let Ok(sig_bytes) = hex::decode(&receipt.signature) else {
        return false;
    };
    let Ok(sig) = Signature::try_from(sig_bytes.as_slice()) else {
        return false;
    };
    key.verify(&hash, &sig).is_ok()
}

/// Persistent, append-only receipt store with a hash chain.
#[derive(Clone)]
pub struct ReceiptStore {
    conn: Arc<Mutex<Connection>>,
}

impl ReceiptStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_connection(Connection::open(path)?, true)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?, false)
    }

    fn from_connection(conn: Connection, file_backed: bool) -> Result<Self> {
        // Shares the mint's database file with the other stores; wait briefly
        // on lock contention instead of failing a write mid-flow.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        if file_backed {
            conn.pragma_update(None, "journal_mode", "WAL")?;
        }
        conn.execute_batch(SCHEMA)?;
        // Migration for databases created before retained_sat existed.
        {
            let mut stmt = conn.prepare("PRAGMA table_info(receipts)")?;
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get(1))?
                .collect::<std::result::Result<_, _>>()?;
            if !cols.iter().any(|c| c == "retained_sat") {
                drop(stmt);
                conn.execute_batch("ALTER TABLE receipts ADD COLUMN retained_sat INTEGER")?;
            }
        }
        Ok(ReceiptStore {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// Append a receipt: hash the canonical payload over the previous chain
    /// head, sign it, and store the result. The lookup of the previous head
    /// and the insert happen in one transaction so the chain cannot fork
    /// under concurrent issuance.
    #[allow(clippy::too_many_arguments)]
    pub fn append(
        &self,
        signer: &ReceiptSigner,
        kind: &str,
        quote_id: &str,
        amount_sat: u64,
        fee_charged_sat: Option<u64>,
        retained_sat: Option<u64>,
        change_sat: Option<u64>,
        outcome: &str,
        keyset_id: &str,
    ) -> Result<MintReceipt> {
        let mut conn = self.conn.lock().unwrap();
        // Immediate: the write lock is taken up front (the busy timeout
        // covers contention), so the chain-head read below always sees
        // the latest committed receipt and a racing appender hits the
        // terminal-outcome uniqueness constraint instead of a stale
        // snapshot.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // An empty chain starts at "genesis"; a query failure is an
        // error, not a genesis - treating one as the other would silently
        // fork the chain.
        let prev_hash: String = tx
            .query_row(
                "SELECT receipt_hash FROM receipts ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_else(|| "genesis".to_string());
        let created_at = Self::now();
        let payload = canonical_receipt_payload(
            kind,
            quote_id,
            "sat",
            amount_sat,
            fee_charged_sat,
            retained_sat,
            change_sat,
            outcome,
            keyset_id,
            created_at,
            &prev_hash,
        );
        let hash: [u8; 32] = Sha256::digest(payload.as_bytes()).into();
        let receipt_hash = hex::encode(hash);
        let signature = signer.sign_hash(&hash);
        let pubkey = signer.pubkey_hex();
        let receipt_id = Uuid::new_v4().to_string();
        let insert = tx.execute(
            "INSERT INTO receipts
             (receipt_id, kind, quote_id, unit, amount_sat, fee_charged_sat, retained_sat, change_sat,
              outcome, keyset_id, created_at, prev_hash, receipt_hash, signature, pubkey)
             VALUES (?1, ?2, ?3, 'sat', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                receipt_id,
                kind,
                quote_id,
                amount_sat as i64,
                fee_charged_sat.map(|v| v as i64),
                retained_sat.map(|v| v as i64),
                change_sat.map(|v| v as i64),
                outcome,
                keyset_id,
                created_at as i64,
                prev_hash,
                receipt_hash,
                signature,
                pubkey,
            ],
        );
        if let Err(e) = insert {
            // The terminal-outcome uniqueness constraint makes the
            // check-then-append race safe: a concurrent finalization
            // already wrote this quote's settled/released receipt, so
            // return it instead of duplicating.
            if matches!(
                &e,
                rusqlite::Error::SqliteFailure(se, _)
                    if se.code == rusqlite::ErrorCode::ConstraintViolation
            ) {
                let existing = tx
                    .query_row(
                        &format!(
                            "SELECT {} FROM receipts WHERE quote_id = ?1 AND kind = ?2 AND outcome = ?3",
                            Self::RECEIPT_COLUMNS
                        ),
                        params![quote_id, kind, outcome],
                        Self::row_to_receipt,
                    )
                    .optional()?;
                if let Some(existing) = existing {
                    return Ok(existing);
                }
            }
            return Err(e.into());
        }
        tx.commit()?;
        Ok(MintReceipt {
            receipt_id,
            kind: kind.to_string(),
            quote_id: quote_id.to_string(),
            unit: "sat".into(),
            amount_sat,
            fee_charged_sat,
            retained_sat,
            change_sat,
            outcome: outcome.to_string(),
            keyset_id: keyset_id.to_string(),
            created_at,
            prev_hash,
            receipt_hash,
            signature,
            pubkey,
        })
    }

    fn row_to_receipt(r: &rusqlite::Row) -> rusqlite::Result<MintReceipt> {
        Ok(MintReceipt {
            receipt_id: r.get(0)?,
            kind: r.get(1)?,
            quote_id: r.get(2)?,
            unit: r.get(3)?,
            amount_sat: r.get::<_, i64>(4)? as u64,
            fee_charged_sat: r.get::<_, Option<i64>>(5)?.map(|v| v as u64),
            retained_sat: r.get::<_, Option<i64>>(6)?.map(|v| v as u64),
            change_sat: r.get::<_, Option<i64>>(7)?.map(|v| v as u64),
            outcome: r.get(8)?,
            keyset_id: r.get(9)?,
            created_at: r.get::<_, i64>(10)? as u64,
            prev_hash: r.get(11)?,
            receipt_hash: r.get(12)?,
            signature: r.get(13)?,
            pubkey: r.get(14)?,
        })
    }

    const RECEIPT_COLUMNS: &'static str =
        "receipt_id, kind, quote_id, unit, amount_sat, fee_charged_sat, retained_sat, change_sat,
         outcome, keyset_id, created_at, prev_hash, receipt_hash, signature, pubkey";

    /// All receipts for a quote, oldest first.
    pub fn for_quote(&self, quote_id: &str) -> Result<Vec<MintReceipt>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM receipts WHERE quote_id = ?1 ORDER BY id",
            Self::RECEIPT_COLUMNS
        ))?;
        let rows = stmt.query_map(params![quote_id], Self::row_to_receipt)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Every receipt in chain order, for the PoL anchor recovery sweep.
    pub fn all_receipts(&self) -> Result<Vec<MintReceipt>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM receipts ORDER BY id",
            Self::RECEIPT_COLUMNS
        ))?;
        let rows = stmt.query_map([], Self::row_to_receipt)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_key_signs_and_verifies() {
        let signer = ReceiptSigner::dev();
        let hash: [u8; 32] = Sha256::digest(b"payload").into();
        let other: [u8; 32] = Sha256::digest(b"other").into();
        let sig = signer.sign_hash(&hash);
        assert!(signer.verify_hash(&hash, &sig));
        assert!(!signer.verify_hash(&other, &sig));
    }

    #[test]
    fn dev_key_refused_outside_mock_mode() {
        // A missing key must fail closed for a real deployment: the dev
        // key's secret is derived from a public constant, so receipts
        // signed with it are forgeable by anyone.
        let var = "MINERVA_TEST_RECEIPT_KEY_DELIBERATELY_UNSET";
        std::env::remove_var(var);
        assert!(ReceiptSigner::from_env_or_dev(var, false).is_err());
        assert!(ReceiptSigner::from_env_or_dev(var, true).is_ok());
    }

    #[test]
    fn receipts_chain_and_verify() {
        let store = ReceiptStore::open_in_memory().unwrap();
        let signer = ReceiptSigner::dev();
        let r1 = store
            .append(&signer, "mint", "q1", 64, None, None, None, "issued", "k1")
            .unwrap();
        assert_eq!(r1.prev_hash, "genesis");
        assert!(verify_receipt(&r1, &signer.pubkey_hex()));

        let r2 = store
            .append(
                &signer,
                "melt",
                "q2",
                500,
                Some(3),
                Some(3),
                Some(2),
                "settled",
                "k1",
            )
            .unwrap();
        assert_eq!(r2.prev_hash, r1.receipt_hash);
        assert!(verify_receipt(&r2, &signer.pubkey_hex()));

        // A tampered receipt does not verify.
        let mut forged = r2.clone();
        forged.amount_sat = 501;
        assert!(!verify_receipt(&forged, &signer.pubkey_hex()));

        let all = store.for_quote("q2").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].receipt_id, r2.receipt_id);
        assert!(store.for_quote("nope").unwrap().is_empty());
    }

    #[test]
    fn receipts_persist_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mint.sqlite");
        let signer = ReceiptSigner::dev();
        let r1 = {
            let store = ReceiptStore::open(&db).unwrap();
            store
                .append(&signer, "mint", "q1", 8, None, None, None, "issued", "k1")
                .unwrap()
        };
        let store = ReceiptStore::open(&db).unwrap();
        let all = store.for_quote("q1").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].receipt_hash, r1.receipt_hash);
        assert!(verify_receipt(&all[0], &signer.pubkey_hex()));
    }

    #[test]
    fn concurrent_settled_receipt_appended_once() {
        // Racing finalizations (a wallet quote check against an operator
        // reconcile, or a retry after a crash) must not append a second
        // settled receipt: the terminal outcome is unique per quote, and
        // the loser of the race gets the winner's receipt back.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("receipts.sqlite");
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let db = db.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                let store = ReceiptStore::open(&db).unwrap();
                let signer = ReceiptSigner::dev();
                barrier.wait();
                store
                    .append(
                        &signer,
                        "melt",
                        "q1",
                        500,
                        Some(0),
                        Some(0),
                        Some(524),
                        "settled",
                        "k1",
                    )
                    .unwrap()
            }));
        }
        let returned: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let store = ReceiptStore::open(&db).unwrap();
        let all = store.for_quote("q1").unwrap();
        assert_eq!(all.len(), 1, "duplicate settled receipt appended");
        assert!(
            returned
                .iter()
                .all(|r| r.receipt_hash == all[0].receipt_hash)
        );
    }
}
