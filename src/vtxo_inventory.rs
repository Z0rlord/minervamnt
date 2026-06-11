//! VTXO inventory manager: SQLite-backed mapping between outstanding ecash
//! tokens and the Ark VTXOs that back them.
//!
//! Responsibilities:
//! - track every VTXO the mint holds (`vtxo_inventory`)
//! - map issued tokens to backing VTXOs (`token_vtxo_map`)
//! - surface a refresh queue of VTXOs nearing expiry
//! - report free (unallocated) reserve for liquidity decisions

use crate::error::{MintError, Result};
use crate::types::{Vtxo, VtxoStatus};
use crate::vtxo_verify::{verify_vtxo, VtxoVerifyMode};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::{Arc, Mutex};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS vtxo_inventory (
    id            TEXT PRIMARY KEY,
    amount_msat   INTEGER NOT NULL,
    status        TEXT NOT NULL CHECK (status IN ('active','refreshing','spent','exited')),
    created_at    INTEGER NOT NULL,
    expires_at    INTEGER NOT NULL,
    branch_tx_hex TEXT NOT NULL,
    leaf_tx_hex   TEXT NOT NULL,
    asp_pubkey    TEXT NOT NULL DEFAULT '',
    vpack_hex     TEXT
);

CREATE TABLE IF NOT EXISTS token_vtxo_map (
    token_id    TEXT PRIMARY KEY,
    vtxo_id     TEXT NOT NULL REFERENCES vtxo_inventory(id),
    amount_msat INTEGER NOT NULL,
    issued_at   INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_vtxo_status_expiry ON vtxo_inventory(status, expires_at);
CREATE INDEX IF NOT EXISTS idx_map_vtxo ON token_vtxo_map(vtxo_id);
"#;

#[derive(Debug, Clone)]
pub struct VtxoRecord {
    pub vtxo: Vtxo,
    pub status: VtxoStatus,
    pub created_at: u64,
}

#[derive(Debug, Clone)]
pub struct TokenMapping {
    pub token_id: String,
    pub vtxo_id: String,
    pub amount_msat: u64,
    pub issued_at: u64,
    pub expires_at: u64,
}

/// SQLite-backed inventory. `Connection` is not `Sync`, so it lives behind a
/// `Mutex`; all queries here are short-lived point lookups, which is fine for
/// a scaffold (swap for a pool / sqlx in production).
#[derive(Clone)]
pub struct VtxoInventory {
    conn: Arc<Mutex<Connection>>,
    verify_mode: VtxoVerifyMode,
}

impl VtxoInventory {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_mode(path, VtxoVerifyMode::default())
    }

    pub fn open_with_mode(path: impl AsRef<Path>, verify_mode: VtxoVerifyMode) -> Result<Self> {
        Self::from_connection(Connection::open(path)?, verify_mode)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::open_in_memory_with_mode(VtxoVerifyMode::default())
    }

    pub fn open_in_memory_with_mode(verify_mode: VtxoVerifyMode) -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?, verify_mode)
    }

    fn from_connection(conn: Connection, verify_mode: VtxoVerifyMode) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        Ok(VtxoInventory {
            conn: Arc::new(Mutex::new(conn)),
            verify_mode,
        })
    }

    pub fn verify_mode(&self) -> VtxoVerifyMode {
        self.verify_mode
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// Register a freshly boarded VTXO as active inventory.
    pub fn insert_vtxo(&self, vtxo: &Vtxo) -> Result<()> {
        verify_vtxo(vtxo, self.verify_mode)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO vtxo_inventory
               (id, amount_msat, status, created_at, expires_at, branch_tx_hex, leaf_tx_hex, asp_pubkey, vpack_hex)
             VALUES (?1, ?2, 'active', ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                vtxo.id,
                vtxo.amount_msat as i64,
                Self::now() as i64,
                vtxo.expiry as i64,
                vtxo.branch_tx,
                vtxo.leaf_tx,
                vtxo.asp_pubkey,
                vtxo.vpack_hex,
            ],
        )?;
        Ok(())
    }

    pub fn get_vtxo(&self, vtxo_id: &str) -> Result<Option<VtxoRecord>> {
        let conn = self.conn.lock().unwrap();
        let rec = conn
            .query_row(
                "SELECT id, amount_msat, status, created_at, expires_at, branch_tx_hex, leaf_tx_hex, asp_pubkey, vpack_hex
                 FROM vtxo_inventory WHERE id = ?1",
                params![vtxo_id],
                Self::row_to_record,
            )
            .optional()?;
        Ok(rec)
    }

    fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<VtxoRecord> {
        let status_str: String = row.get(2)?;
        Ok(VtxoRecord {
            vtxo: Vtxo {
                id: row.get(0)?,
                amount_msat: row.get::<_, i64>(1)? as u64,
                expiry: row.get::<_, i64>(4)? as u64,
                branch_tx: row.get(5)?,
                leaf_tx: row.get(6)?,
                asp_pubkey: row.get(7)?,
                vpack_hex: row.get(8)?,
            },
            status: VtxoStatus::parse(&status_str).unwrap_or(VtxoStatus::Spent),
            created_at: row.get::<_, i64>(3)? as u64,
        })
    }

    pub fn set_status(&self, vtxo_id: &str, status: VtxoStatus) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE vtxo_inventory SET status = ?1 WHERE id = ?2",
            params![status.as_str(), vtxo_id],
        )?;
        if n == 0 {
            return Err(MintError::MappingNotFound(vtxo_id.to_string()));
        }
        Ok(())
    }

    /// Replace an old VTXO with its refreshed successor atomically: the old
    /// row is marked spent, the new one inserted active, and all token
    /// mappings are repointed.
    pub fn replace_refreshed(&self, old_id: &str, fresh: &Vtxo) -> Result<()> {
        verify_vtxo(fresh, self.verify_mode)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE vtxo_inventory SET status = 'spent' WHERE id = ?1",
            params![old_id],
        )?;
        tx.execute(
            "INSERT INTO vtxo_inventory
               (id, amount_msat, status, created_at, expires_at, branch_tx_hex, leaf_tx_hex, asp_pubkey, vpack_hex)
             VALUES (?1, ?2, 'active', ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                fresh.id,
                fresh.amount_msat as i64,
                Self::now() as i64,
                fresh.expiry as i64,
                fresh.branch_tx,
                fresh.leaf_tx,
                fresh.asp_pubkey,
                fresh.vpack_hex,
            ],
        )?;
        tx.execute(
            "UPDATE token_vtxo_map SET vtxo_id = ?1, expires_at = ?2 WHERE vtxo_id = ?3",
            params![fresh.id, fresh.expiry as i64, old_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Map a newly issued token (batch) to a backing VTXO.
    ///
    /// Picks the active VTXO with the most free (unallocated) capacity that
    /// can absorb `amount_msat`. Returns the chosen VTXO id.
    pub fn allocate_vtxo_for_tokens(&self, token_id: &str, amount_msat: u64) -> Result<String> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT v.id, v.expires_at,
                        v.amount_msat - COALESCE((
                            SELECT SUM(m.amount_msat) FROM token_vtxo_map m WHERE m.vtxo_id = v.id
                        ), 0) AS free
                 FROM vtxo_inventory v
                 WHERE v.status = 'active'
                   AND free >= ?1
                 ORDER BY free DESC
                 LIMIT 1",
                params![amount_msat as i64],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64)),
            )
            .optional()?;

        let (vtxo_id, expires_at) = row.ok_or(MintError::InsufficientLiquidity {
            needed_msat: amount_msat,
        })?;

        conn.execute(
            "INSERT INTO token_vtxo_map (token_id, vtxo_id, amount_msat, issued_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                token_id,
                vtxo_id,
                amount_msat as i64,
                Self::now() as i64,
                expires_at as i64,
            ],
        )?;
        Ok(vtxo_id)
    }

    /// Release the token -> VTXO mapping (token melted or swapped away).
    pub fn release_vtxo_mapping(&self, token_id: &str) -> Result<TokenMapping> {
        let conn = self.conn.lock().unwrap();
        let mapping = conn
            .query_row(
                "SELECT token_id, vtxo_id, amount_msat, issued_at, expires_at
                 FROM token_vtxo_map WHERE token_id = ?1",
                params![token_id],
                |row| {
                    Ok(TokenMapping {
                        token_id: row.get(0)?,
                        vtxo_id: row.get(1)?,
                        amount_msat: row.get::<_, i64>(2)? as u64,
                        issued_at: row.get::<_, i64>(3)? as u64,
                        expires_at: row.get::<_, i64>(4)? as u64,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| MintError::MappingNotFound(token_id.to_string()))?;

        conn.execute(
            "DELETE FROM token_vtxo_map WHERE token_id = ?1",
            params![token_id],
        )?;
        Ok(mapping)
    }

    pub fn get_mapping(&self, token_id: &str) -> Result<Option<TokenMapping>> {
        let conn = self.conn.lock().unwrap();
        let mapping = conn
            .query_row(
                "SELECT token_id, vtxo_id, amount_msat, issued_at, expires_at
                 FROM token_vtxo_map WHERE token_id = ?1",
                params![token_id],
                |row| {
                    Ok(TokenMapping {
                        token_id: row.get(0)?,
                        vtxo_id: row.get(1)?,
                        amount_msat: row.get::<_, i64>(2)? as u64,
                        issued_at: row.get::<_, i64>(3)? as u64,
                        expires_at: row.get::<_, i64>(4)? as u64,
                    })
                },
            )
            .optional()?;
        Ok(mapping)
    }

    /// Active VTXOs that expire within `threshold_blocks` of `current_height`,
    /// soonest first. These need a refresh round.
    pub fn get_refresh_queue(
        &self,
        current_height: u64,
        threshold_blocks: u64,
    ) -> Result<Vec<VtxoRecord>> {
        let conn = self.conn.lock().unwrap();
        let cutoff = current_height + threshold_blocks;
        let mut stmt = conn.prepare(
            "SELECT id, amount_msat, status, created_at, expires_at, branch_tx_hex, leaf_tx_hex, asp_pubkey, vpack_hex
             FROM vtxo_inventory
             WHERE status = 'active' AND expires_at <= ?1
             ORDER BY expires_at ASC",
        )?;
        let rows = stmt.query_map(params![cutoff as i64], Self::row_to_record)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Total msat in active VTXOs not yet promised to outstanding tokens.
    pub fn free_reserve_msat(&self) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let free: i64 = conn.query_row(
            "SELECT COALESCE((SELECT SUM(amount_msat) FROM vtxo_inventory WHERE status = 'active'), 0)
                  - COALESCE((SELECT SUM(m.amount_msat) FROM token_vtxo_map m
                              JOIN vtxo_inventory v ON v.id = m.vtxo_id
                              WHERE v.status = 'active'), 0)",
            [],
            |row| row.get(0),
        )?;
        Ok(free.max(0) as u64)
    }

    /// Earliest expiry height among active VTXOs (for /ark/refresh/status).
    pub fn next_expiry_height(&self) -> Result<Option<u64>> {
        let conn = self.conn.lock().unwrap();
        let h: Option<i64> = conn.query_row(
            "SELECT MIN(expires_at) FROM vtxo_inventory WHERE status = 'active'",
            [],
            |row| row.get(0),
        )?;
        Ok(h.map(|v| v as u64))
    }

    /// Total msat in active VTXOs (gross, before allocation).
    pub fn total_active_vtxo_msat(&self) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let total: i64 = conn.query_row(
            "SELECT COALESCE(SUM(amount_msat), 0) FROM vtxo_inventory WHERE status = 'active'",
            [],
            |r| r.get(0),
        )?;
        Ok(total.max(0) as u64)
    }

    /// Total msat mapped to outstanding token batches on active VTXOs.
    pub fn total_allocated_msat(&self) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let total: i64 = conn.query_row(
            "SELECT COALESCE(SUM(m.amount_msat), 0) FROM token_vtxo_map m
             JOIN vtxo_inventory v ON v.id = m.vtxo_id
             WHERE v.status = 'active'",
            [],
            |r| r.get(0),
        )?;
        Ok(total.max(0) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vtxo(id: &str, amount_msat: u64, expiry: u64) -> Vtxo {
        Vtxo {
            id: id.to_string(),
            amount_msat,
            expiry,
            branch_tx: format!("{id}-branch"),
            leaf_tx: format!("{id}-leaf"),
            asp_pubkey: "02ab".to_string(),
            vpack_hex: None,
        }
    }

    fn inv_with(vtxos: &[Vtxo]) -> VtxoInventory {
        let inv = VtxoInventory::open_in_memory().unwrap();
        for v in vtxos {
            inv.insert_vtxo(v).unwrap();
        }
        inv
    }

    #[test]
    fn allocate_picks_vtxo_with_capacity_and_tracks_free_reserve() {
        let inv = inv_with(&[vtxo("a", 1_000_000, 900_000), vtxo("b", 5_000_000, 900_000)]);
        assert_eq!(inv.free_reserve_msat().unwrap(), 6_000_000);

        // 2M only fits in "b".
        let chosen = inv.allocate_vtxo_for_tokens("tok1", 2_000_000).unwrap();
        assert_eq!(chosen, "b");
        assert_eq!(inv.free_reserve_msat().unwrap(), 4_000_000);

        // "b" still has the most free capacity (3M vs 1M).
        let chosen = inv.allocate_vtxo_for_tokens("tok2", 3_000_000).unwrap();
        assert_eq!(chosen, "b");

        // Now only "a" (1M free) can take a small allocation.
        let chosen = inv.allocate_vtxo_for_tokens("tok3", 500_000).unwrap();
        assert_eq!(chosen, "a");

        // Nothing can take 2M anymore.
        let err = inv.allocate_vtxo_for_tokens("tok4", 2_000_000).unwrap_err();
        assert!(matches!(err, MintError::InsufficientLiquidity { .. }));
    }

    #[test]
    fn allocate_fails_on_empty_inventory() {
        let inv = inv_with(&[]);
        let err = inv.allocate_vtxo_for_tokens("tok", 1).unwrap_err();
        assert!(matches!(err, MintError::InsufficientLiquidity { .. }));
    }

    #[test]
    fn duplicate_token_id_rejected() {
        let inv = inv_with(&[vtxo("a", 1_000_000, 900_000)]);
        inv.allocate_vtxo_for_tokens("tok", 100).unwrap();
        assert!(inv.allocate_vtxo_for_tokens("tok", 100).is_err());
    }

    #[test]
    fn release_restores_capacity_and_is_idempotent_failure() {
        let inv = inv_with(&[vtxo("a", 1_000_000, 900_000)]);
        inv.allocate_vtxo_for_tokens("tok", 800_000).unwrap();
        assert_eq!(inv.free_reserve_msat().unwrap(), 200_000);

        let mapping = inv.release_vtxo_mapping("tok").unwrap();
        assert_eq!(mapping.vtxo_id, "a");
        assert_eq!(mapping.amount_msat, 800_000);
        assert_eq!(inv.free_reserve_msat().unwrap(), 1_000_000);

        let err = inv.release_vtxo_mapping("tok").unwrap_err();
        assert!(matches!(err, MintError::MappingNotFound(_)));
    }

    #[test]
    fn refresh_queue_orders_by_expiry_and_respects_threshold() {
        let inv = inv_with(&[
            vtxo("soon", 1_000, 850_100),
            vtxo("sooner", 1_000, 850_050),
            vtxo("later", 1_000, 880_000),
        ]);
        let queue = inv.get_refresh_queue(850_000, 144).unwrap();
        let ids: Vec<_> = queue.iter().map(|r| r.vtxo.id.as_str()).collect();
        assert_eq!(ids, vec!["sooner", "soon"]);

        // Spent VTXOs never appear in the queue.
        inv.set_status("sooner", VtxoStatus::Spent).unwrap();
        let queue = inv.get_refresh_queue(850_000, 144).unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].vtxo.id, "soon");
    }

    #[test]
    fn replace_refreshed_repoints_mappings() {
        let inv = inv_with(&[vtxo("old", 1_000_000, 850_010)]);
        inv.allocate_vtxo_for_tokens("tok", 600_000).unwrap();

        let fresh = vtxo("new", 1_000_000, 876_000);
        inv.replace_refreshed("old", &fresh).unwrap();

        let mapping = inv.get_mapping("tok").unwrap().unwrap();
        assert_eq!(mapping.vtxo_id, "new");
        assert_eq!(mapping.expires_at, 876_000);

        let old = inv.get_vtxo("old").unwrap().unwrap();
        assert_eq!(old.status, VtxoStatus::Spent);
        let new = inv.get_vtxo("new").unwrap().unwrap();
        assert_eq!(new.status, VtxoStatus::Active);

        // Free reserve unchanged by a refresh (1M active - 600k allocated).
        assert_eq!(inv.free_reserve_msat().unwrap(), 400_000);
    }

    #[test]
    fn status_check_constraint_enforced() {
        let inv = inv_with(&[vtxo("a", 1, 1)]);
        let conn = inv.conn.lock().unwrap();
        let res = conn.execute(
            "UPDATE vtxo_inventory SET status = 'bogus' WHERE id = 'a'",
            [],
        );
        assert!(res.is_err());
    }
}
