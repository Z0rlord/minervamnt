//! Proof of Liabilities (PoL) ledger — epoch-based mint/burn event log.
//!
//! Implements the [PoL spec](https://gist.github.com/victorandre957/4f497d385e1fd9a47898480903f56b3e)
//! with daily epoch closure, chained root hashes, and OpenTimestamps storage.

use crate::error::{MintError, Result};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::{Arc, Mutex};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS pol_mint_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    keyset_id   TEXT NOT NULL,
    quote_id    TEXT NOT NULL UNIQUE,
    amount_sat  INTEGER NOT NULL,
    b_hash      TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    epoch_day   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS pol_burn_events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    keyset_id    TEXT NOT NULL,
    secret_hash  TEXT NOT NULL,
    amount_sat   INTEGER NOT NULL,
    created_at   INTEGER NOT NULL,
    epoch_day    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS pol_epochs (
    epoch_day    TEXT NOT NULL,
    keyset_id    TEXT NOT NULL,
    mint_total   INTEGER NOT NULL,
    burn_total   INTEGER NOT NULL,
    root_hash    TEXT NOT NULL,
    prev_hash    TEXT,
    closed_at    INTEGER NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('open','closed')),
    ots_proof_hex TEXT,
    ots_calendar_url TEXT,
    ots_stamped_at INTEGER,
    PRIMARY KEY (epoch_day, keyset_id)
);
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochStatus {
    Open,
    Closed,
}

#[derive(Debug, Clone)]
pub struct PolStatus {
    pub current_epoch_day: String,
    pub open_mint_total_sat: u64,
    pub open_burn_total_sat: u64,
    pub outstanding_sat: u64,
    pub last_closed_epoch: Option<String>,
    pub last_closed_root: Option<String>,
    pub last_ots_stamped_epoch: Option<String>,
    pub last_ots_stamped_at: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct PolEpochRoot {
    pub epoch_day: String,
    pub keyset_id: String,
    pub mint_total_sat: u64,
    pub burn_total_sat: u64,
    pub outstanding_sat: u64,
    pub root_hash: String,
    pub prev_hash: Option<String>,
    pub ots_proof_hex: Option<String>,
    pub ots_calendar_url: Option<String>,
    pub ots_stamped_at: Option<u64>,
}

#[derive(Clone)]
pub struct PolLedger {
    conn: Arc<Mutex<Connection>>,
}

impl PolLedger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_connection(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        Self::migrate(&conn)?;
        Ok(PolLedger {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn migrate(conn: &Connection) -> Result<()> {
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(pol_epochs)")?
            .query_map([], |r| r.get(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !cols.contains(&"ots_proof_hex".to_string()) {
            conn.execute("ALTER TABLE pol_epochs ADD COLUMN ots_proof_hex TEXT", [])?;
        }
        if !cols.contains(&"ots_calendar_url".to_string()) {
            conn.execute("ALTER TABLE pol_epochs ADD COLUMN ots_calendar_url TEXT", [])?;
        }
        if !cols.contains(&"ots_stamped_at".to_string()) {
            conn.execute("ALTER TABLE pol_epochs ADD COLUMN ots_stamped_at INTEGER", [])?;
        }
        Ok(())
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// UTC calendar day `YYYY-MM-DD` for epoch grouping.
    pub fn current_epoch_day() -> String {
        let secs = Self::now() as i64;
        // Simple UTC day bucket without chrono dependency.
        let day = secs / 86_400;
        format!("epoch-{day}")
    }

    fn hash_leaf(label: &str, payload: &str) -> String {
        let mut h = Sha256::new();
        h.update(label.as_bytes());
        h.update(payload.as_bytes());
        hex::encode(h.finalize())
    }

    /// Log a successful mint (blind signature issuance).
    pub fn record_mint(
        &self,
        keyset_id: &str,
        quote_id: &str,
        amount_sat: u64,
        outputs_b: &[String],
    ) -> Result<()> {
        let epoch_day = Self::current_epoch_day();
        let mut b_hasher = Sha256::new();
        for b in outputs_b {
            b_hasher.update(b.as_bytes());
        }
        let b_hash = hex::encode(b_hasher.finalize());

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO pol_mint_events (keyset_id, quote_id, amount_sat, b_hash, created_at, epoch_day)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                keyset_id,
                quote_id,
                amount_sat as i64,
                b_hash,
                Self::now() as i64,
                epoch_day,
            ],
        )?;
        Ok(())
    }

    /// Log a burn (secret spent in melt or swap).
    pub fn record_burn(&self, keyset_id: &str, secret: &str, amount_sat: u64) -> Result<()> {
        let epoch_day = Self::current_epoch_day();
        let secret_hash = Self::hash_leaf("burn", secret);
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO pol_burn_events (keyset_id, secret_hash, amount_sat, created_at, epoch_day)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                keyset_id,
                secret_hash,
                amount_sat as i64,
                Self::now() as i64,
                epoch_day,
            ],
        )?;
        Ok(())
    }

    fn open_totals(&self, epoch_day: &str, keyset_id: &str) -> Result<(u64, u64)> {
        let conn = self.conn.lock().unwrap();
        let mint: i64 = conn.query_row(
            "SELECT COALESCE(SUM(amount_sat), 0) FROM pol_mint_events
             WHERE epoch_day = ?1 AND keyset_id = ?2",
            params![epoch_day, keyset_id],
            |r| r.get(0),
        )?;
        let burn: i64 = conn.query_row(
            "SELECT COALESCE(SUM(amount_sat), 0) FROM pol_burn_events
             WHERE epoch_day = ?1 AND keyset_id = ?2",
            params![epoch_day, keyset_id],
            |r| r.get(0),
        )?;
        Ok((mint.max(0) as u64, burn.max(0) as u64))
    }

    fn cumulative_outstanding(&self, keyset_id: &str) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let mint: i64 = conn.query_row(
            "SELECT COALESCE(SUM(amount_sat), 0) FROM pol_mint_events WHERE keyset_id = ?1",
            params![keyset_id],
            |r| r.get(0),
        )?;
        let burn: i64 = conn.query_row(
            "SELECT COALESCE(SUM(amount_sat), 0) FROM pol_burn_events WHERE keyset_id = ?1",
            params![keyset_id],
            |r| r.get(0),
        )?;
        Ok(mint.saturating_sub(burn).max(0) as u64)
    }

    pub fn status(&self, keyset_id: &str) -> Result<PolStatus> {
        let epoch_day = Self::current_epoch_day();
        let (open_mint, open_burn) = self.open_totals(&epoch_day, keyset_id)?;
        let outstanding = self.cumulative_outstanding(keyset_id)?;

        let conn = self.conn.lock().unwrap();
        let last: Option<(String, String)> = conn
            .query_row(
                "SELECT epoch_day, root_hash FROM pol_epochs
                 WHERE status = 'closed' AND keyset_id = ?1
                 ORDER BY closed_at DESC LIMIT 1",
                params![keyset_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        let last_ots: Option<(String, i64)> = conn
            .query_row(
                "SELECT epoch_day, ots_stamped_at FROM pol_epochs
                 WHERE keyset_id = ?1 AND ots_proof_hex IS NOT NULL
                 ORDER BY ots_stamped_at DESC LIMIT 1",
                params![keyset_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        Ok(PolStatus {
            current_epoch_day: epoch_day,
            open_mint_total_sat: open_mint,
            open_burn_total_sat: open_burn,
            outstanding_sat: outstanding,
            last_closed_epoch: last.as_ref().map(|(d, _)| d.clone()),
            last_closed_root: last.map(|(_, h)| h),
            last_ots_stamped_epoch: last_ots.as_ref().map(|(d, _)| d.clone()),
            last_ots_stamped_at: last_ots.map(|(_, t)| t.max(0) as u64),
        })
    }

    /// Close the given UTC epoch day and compute a chained root hash.
    ///
    /// Idempotent: already-closed epochs are skipped.
    pub fn close_epoch(&self, epoch_day: &str, keyset_id: &str) -> Result<Option<PolEpochRoot>> {
        let conn = self.conn.lock().unwrap();
        let already: Option<String> = conn
            .query_row(
                "SELECT status FROM pol_epochs WHERE epoch_day = ?1 AND keyset_id = ?2",
                params![epoch_day, keyset_id],
                |r| r.get(0),
            )
            .optional()?;
        if already.as_deref() == Some("closed") {
            return Ok(None);
        }

        let mint_total: i64 = conn.query_row(
            "SELECT COALESCE(SUM(amount_sat), 0) FROM pol_mint_events
             WHERE epoch_day = ?1 AND keyset_id = ?2",
            params![epoch_day, keyset_id],
            |r| r.get(0),
        )?;
        let burn_total: i64 = conn.query_row(
            "SELECT COALESCE(SUM(amount_sat), 0) FROM pol_burn_events
             WHERE epoch_day = ?1 AND keyset_id = ?2",
            params![epoch_day, keyset_id],
            |r| r.get(0),
        )?;

        let mut event_hashes: Vec<String> = conn
            .prepare(
                "SELECT quote_id || ':' || amount_sat FROM pol_mint_events
                 WHERE epoch_day = ?1 AND keyset_id = ?2
                 UNION ALL
                 SELECT 'burn:' || secret_hash || ':' || amount_sat FROM pol_burn_events
                 WHERE epoch_day = ?1 AND keyset_id = ?2",
            )?
            .query_map(params![epoch_day, keyset_id], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        event_hashes.sort();

        let mut root_hasher = Sha256::new();
        root_hasher.update(epoch_day.as_bytes());
        root_hasher.update((mint_total as u64).to_be_bytes());
        root_hasher.update((burn_total as u64).to_be_bytes());
        for e in &event_hashes {
            root_hasher.update(e.as_bytes());
        }
        let root_hash = hex::encode(root_hasher.finalize());

        let prev_hash: Option<String> = conn
            .query_row(
                "SELECT root_hash FROM pol_epochs
                 WHERE status = 'closed' AND keyset_id = ?1
                 ORDER BY closed_at DESC LIMIT 1",
                params![keyset_id],
                |r| r.get(0),
            )
            .optional()?;

        let closed_at = Self::now();
        conn.execute(
            "INSERT OR REPLACE INTO pol_epochs
               (epoch_day, keyset_id, mint_total, burn_total, root_hash, prev_hash, closed_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'closed')",
            params![
                epoch_day,
                keyset_id,
                mint_total,
                burn_total,
                root_hash,
                prev_hash,
                closed_at as i64,
            ],
        )?;

        Ok(Some(PolEpochRoot {
            epoch_day: epoch_day.to_string(),
            keyset_id: keyset_id.to_string(),
            mint_total_sat: mint_total.max(0) as u64,
            burn_total_sat: burn_total.max(0) as u64,
            outstanding_sat: (mint_total - burn_total).max(0) as u64,
            root_hash,
            prev_hash,
            ots_proof_hex: None,
            ots_calendar_url: None,
            ots_stamped_at: None,
        }))
    }

    pub fn epoch_root_hash(&self, epoch_day: &str, keyset_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT root_hash FROM pol_epochs WHERE epoch_day = ?1 AND keyset_id = ?2",
            params![epoch_day, keyset_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn has_ots_stamp(&self, epoch_day: &str, keyset_id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(1) FROM pol_epochs
             WHERE epoch_day = ?1 AND keyset_id = ?2 AND ots_proof_hex IS NOT NULL",
            params![epoch_day, keyset_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn save_ots_stamp(
        &self,
        epoch_day: &str,
        keyset_id: &str,
        proof_hex: &str,
        calendar_url: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE pol_epochs SET ots_proof_hex = ?1, ots_calendar_url = ?2, ots_stamped_at = ?3
             WHERE epoch_day = ?4 AND keyset_id = ?5",
            params![
                proof_hex,
                calendar_url,
                Self::now() as i64,
                epoch_day,
                keyset_id,
            ],
        )?;
        if n == 0 {
            return Err(MintError::InvalidRequest(format!(
                "no closed epoch {epoch_day} to attach OTS proof"
            )));
        }
        Ok(())
    }

    /// Closed epochs that do not yet have an OTS proof attached.
    pub fn epochs_pending_ots(&self, keyset_id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT epoch_day FROM pol_epochs
             WHERE keyset_id = ?1 AND status = 'closed' AND ots_proof_hex IS NULL
             ORDER BY closed_at ASC",
        )?;
        let rows = stmt.query_map(params![keyset_id], |r| r.get(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn ots_proof(&self, epoch_day: &str, keyset_id: &str) -> Result<Option<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT ots_proof_hex, ots_calendar_url FROM pol_epochs
             WHERE epoch_day = ?1 AND keyset_id = ?2 AND ots_proof_hex IS NOT NULL",
            params![epoch_day, keyset_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn roots_for_keyset(&self, keyset_id: &str) -> Result<Vec<PolEpochRoot>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT epoch_day, keyset_id, mint_total, burn_total, root_hash, prev_hash,
                    ots_proof_hex, ots_calendar_url, ots_stamped_at
             FROM pol_epochs WHERE keyset_id = ?1 AND status = 'closed'
             ORDER BY closed_at ASC",
        )?;
        let rows = stmt.query_map(params![keyset_id], |row| {
            let mint: i64 = row.get(2)?;
            let burn: i64 = row.get(3)?;
            let ots_at: Option<i64> = row.get(8)?;
            Ok(PolEpochRoot {
                epoch_day: row.get(0)?,
                keyset_id: row.get(1)?,
                mint_total_sat: mint.max(0) as u64,
                burn_total_sat: burn.max(0) as u64,
                outstanding_sat: (mint - burn).max(0) as u64,
                root_hash: row.get(4)?,
                prev_hash: row.get(5)?,
                ots_proof_hex: row.get(6)?,
                ots_calendar_url: row.get(7)?,
                ots_stamped_at: ots_at.map(|t| t.max(0) as u64),
            })
        })?;
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
    use crate::types::KEYSET_ID;

    #[test]
    fn mint_and_burn_update_outstanding() {
        let pol = PolLedger::open_in_memory().unwrap();
        pol.record_mint(KEYSET_ID, "q1", 100, &["02b1".into()])
            .unwrap();
        pol.record_mint(KEYSET_ID, "q2", 50, &["02b2".into()])
            .unwrap();
        pol.record_burn(KEYSET_ID, "secret-a", 30).unwrap();

        let s = pol.status(KEYSET_ID).unwrap();
        assert_eq!(s.outstanding_sat, 120);
        assert_eq!(s.open_mint_total_sat, 150);
        assert_eq!(s.open_burn_total_sat, 30);
    }

    #[test]
    fn close_epoch_produces_root_and_chains() {
        let pol = PolLedger::open_in_memory().unwrap();
        let day = PolLedger::current_epoch_day();
        pol.record_mint(KEYSET_ID, "q1", 64, &["02b".into()]).unwrap();

        let closed = pol.close_epoch(&day, KEYSET_ID).unwrap().expect("epoch closed");
        assert_eq!(closed.mint_total_sat, 64);
        assert_eq!(closed.burn_total_sat, 0);
        assert_eq!(closed.root_hash.len(), 64);

        // Second close is idempotent.
        assert!(pol.close_epoch(&day, KEYSET_ID).unwrap().is_none());

        let roots = pol.roots_for_keyset(KEYSET_ID).unwrap();
        assert_eq!(roots.len(), 1);
    }

    #[test]
    fn duplicate_mint_quote_rejected() {
        let pol = PolLedger::open_in_memory().unwrap();
        pol.record_mint(KEYSET_ID, "q1", 10, &[]).unwrap();
        assert!(pol.record_mint(KEYSET_ID, "q1", 10, &[]).is_err());
    }

    #[test]
    fn save_ots_stamp_on_closed_epoch() {
        let pol = PolLedger::open_in_memory().unwrap();
        let day = PolLedger::current_epoch_day();
        pol.record_mint(KEYSET_ID, "q1", 10, &[]).unwrap();
        pol.close_epoch(&day, KEYSET_ID).unwrap();

        pol.save_ots_stamp(
            &day,
            KEYSET_ID,
            "deadbeef",
            "https://a.pool.opentimestamps.org",
        )
        .unwrap();
        assert!(pol.has_ots_stamp(&day, KEYSET_ID).unwrap());
        let (proof, cal) = pol.ots_proof(&day, KEYSET_ID).unwrap().unwrap();
        assert_eq!(proof, "deadbeef");
        assert!(cal.contains("opentimestamps"));
    }

    #[test]
    fn separate_keysets_have_independent_outstanding() {
        let pol = PolLedger::open_in_memory().unwrap();
        pol.record_mint(KEYSET_ID, "q1", 40, &[]).unwrap();
        pol.record_mint("00remotekeyset01", "q2", 25, &[]).unwrap();
        pol.record_burn(KEYSET_ID, "s1", 10).unwrap();

        assert_eq!(pol.status(KEYSET_ID).unwrap().outstanding_sat, 30);
        assert_eq!(
            pol.status("00remotekeyset01").unwrap().outstanding_sat,
            25
        );
    }

    #[test]
    fn epochs_pending_ots_lists_unstamped() {
        let pol = PolLedger::open_in_memory().unwrap();
        let day = PolLedger::current_epoch_day();
        pol.close_epoch(&day, KEYSET_ID).unwrap();
        assert_eq!(pol.epochs_pending_ots(KEYSET_ID).unwrap(), vec![day.clone()]);
        pol.save_ots_stamp(&day, KEYSET_ID, "aa", "mock://ots").unwrap();
        assert!(pol.epochs_pending_ots(KEYSET_ID).unwrap().is_empty());
    }
}
