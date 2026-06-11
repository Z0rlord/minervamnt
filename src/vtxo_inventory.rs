use crate::ark_client::Vtxo;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VtxoStatus {
    Active,
    Refreshing,
    Spent,
    Exited,
}

impl VtxoStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Refreshing => "refreshing",
            Self::Spent => "spent",
            Self::Exited => "exited",
        }
    }

    fn from_str(value: &str) -> Result<Self, InventoryError> {
        match value {
            "active" => Ok(Self::Active),
            "refreshing" => Ok(Self::Refreshing),
            "spent" => Ok(Self::Spent),
            "exited" => Ok(Self::Exited),
            other => Err(InventoryError::InvalidStatus(other.to_string())),
        }
    }
}

#[derive(Debug, Error)]
pub enum InventoryError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("vtxo not found: {0}")]
    NotFound(String),
    #[error("insufficient active liquidity")]
    InsufficientLiquidity,
    #[error("invalid status: {0}")]
    InvalidStatus(String),
}

#[derive(Debug, Clone)]
pub struct InventoryVtxo {
    pub id: String,
    pub amount_msat: u64,
    pub status: VtxoStatus,
    pub expires_at: DateTime<Utc>,
    pub branch_tx_hex: String,
    pub leaf_tx_hex: String,
}

pub struct VtxoInventory {
    conn: Mutex<Connection>,
    refresh_threshold: chrono::Duration,
}

impl VtxoInventory {
    pub fn new(path: impl AsRef<Path>, refresh_threshold_blocks: u64) -> Result<Self, InventoryError> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                InventoryError::Database(rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
            })?;
        }

        let inventory = Self {
            conn: Mutex::new(Connection::open(path)?),
            refresh_threshold: chrono::Duration::minutes(refresh_threshold_blocks as i64),
        };
        inventory.migrate()?;
        Ok(inventory)
    }

    pub fn in_memory(refresh_threshold_blocks: u64) -> Result<Self, InventoryError> {
        let inventory = Self {
            conn: Mutex::new(Connection::open_in_memory()?),
            refresh_threshold: chrono::Duration::minutes(refresh_threshold_blocks as i64),
        };
        inventory.migrate()?;
        Ok(inventory)
    }

    fn migrate(&self) -> Result<(), InventoryError> {
        let conn = self.conn.lock().expect("inventory mutex");
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS vtxo_inventory (
                id TEXT PRIMARY KEY,
                amount_msat BIGINT NOT NULL,
                status TEXT NOT NULL CHECK(status IN ('active', 'refreshing', 'spent', 'exited')),
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                expires_at TEXT NOT NULL,
                branch_tx_hex TEXT,
                leaf_tx_hex TEXT
            );

            CREATE TABLE IF NOT EXISTS token_vtxo_map (
                token_id TEXT PRIMARY KEY,
                vtxo_id TEXT NOT NULL,
                amount_msat BIGINT NOT NULL,
                issued_at TEXT NOT NULL DEFAULT (datetime('now')),
                expires_at TEXT NOT NULL,
                FOREIGN KEY (vtxo_id) REFERENCES vtxo_inventory(id)
            );
            "#,
        )?;
        Ok(())
    }

    pub fn insert_vtxo(&self, vtxo: &Vtxo, expires_at: DateTime<Utc>) -> Result<(), InventoryError> {
        let conn = self.conn.lock().expect("inventory mutex");
        conn.execute(
            "INSERT INTO vtxo_inventory (id, amount_msat, status, expires_at, branch_tx_hex, leaf_tx_hex)
             VALUES (?1, ?2, 'active', ?3, ?4, ?5)",
            params![
                vtxo.id,
                vtxo.amount_msat,
                expires_at.to_rfc3339(),
                vtxo.branch_tx_hex,
                vtxo.leaf_tx_hex
            ],
        )?;
        Ok(())
    }

    pub fn allocate_vtxo_for_tokens(&self, amount_msat: u64) -> Result<InventoryVtxo, InventoryError> {
        let conn = self.conn.lock().expect("inventory mutex");
        let mut stmt = conn.prepare(
            "SELECT id, amount_msat, status, expires_at, branch_tx_hex, leaf_tx_hex
             FROM vtxo_inventory
             WHERE status = 'active' AND amount_msat >= ?1
             ORDER BY amount_msat ASC
             LIMIT 1",
        )?;

        let mut rows = stmt.query(params![amount_msat])?;
        if let Some(row) = rows.next()? {
            return Ok(InventoryVtxo {
                id: row.get(0)?,
                amount_msat: row.get(1)?,
                status: VtxoStatus::from_str(&row.get::<_, String>(2)?)?,
                expires_at: parse_ts(&row.get::<_, String>(3)?),
                branch_tx_hex: row.get(4)?,
                leaf_tx_hex: row.get(5)?,
            });
        }

        Err(InventoryError::InsufficientLiquidity)
    }

    pub fn map_token_to_vtxo(
        &self,
        token_id: &str,
        vtxo_id: &str,
        amount_msat: u64,
        expires_at: DateTime<Utc>,
    ) -> Result<(), InventoryError> {
        let conn = self.conn.lock().expect("inventory mutex");
        conn.execute(
            "INSERT INTO token_vtxo_map (token_id, vtxo_id, amount_msat, expires_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![token_id, vtxo_id, amount_msat, expires_at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn release_vtxo_mapping(&self, token_id: &str) -> Result<(), InventoryError> {
        let conn = self.conn.lock().expect("inventory mutex");
        let vtxo_id: Option<String> = match conn.query_row(
            "SELECT vtxo_id FROM token_vtxo_map WHERE token_id = ?1",
            params![token_id],
            |row| row.get(0),
        ) {
            Ok(value) => Some(value),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(err) => return Err(err.into()),
        };

        let Some(vtxo_id) = vtxo_id else {
            return Err(InventoryError::NotFound(token_id.to_string()));
        };

        conn.execute(
            "DELETE FROM token_vtxo_map WHERE token_id = ?1",
            params![token_id],
        )?;
        conn.execute(
            "UPDATE vtxo_inventory SET status = 'spent' WHERE id = ?1",
            params![vtxo_id],
        )?;
        Ok(())
    }

    pub fn get_refresh_queue(&self) -> Result<Vec<InventoryVtxo>, InventoryError> {
        let threshold = (Utc::now() + self.refresh_threshold).to_rfc3339();
        let conn = self.conn.lock().expect("inventory mutex");
        let mut stmt = conn.prepare(
            "SELECT id, amount_msat, status, expires_at, branch_tx_hex, leaf_tx_hex
             FROM vtxo_inventory
             WHERE status = 'active' AND expires_at <= ?1
             ORDER BY expires_at ASC",
        )?;
        let rows = stmt.query_map(params![threshold], |row| {
            Ok(InventoryVtxo {
                id: row.get(0)?,
                amount_msat: row.get(1)?,
                status: VtxoStatus::from_str(&row.get::<_, String>(2)?).unwrap_or(VtxoStatus::Active),
                expires_at: parse_ts(&row.get::<_, String>(3)?),
                branch_tx_hex: row.get(4)?,
                leaf_tx_hex: row.get(5)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>().map_err(InventoryError::from)
    }

    pub fn update_vtxo_after_refresh(&self, old_id: &str, refreshed: &Vtxo, expires_at: DateTime<Utc>) -> Result<(), InventoryError> {
        {
            let conn = self.conn.lock().expect("inventory mutex");
            conn.execute(
                "UPDATE vtxo_inventory SET status = 'spent' WHERE id = ?1",
                params![old_id],
            )?;
            conn.execute(
                "UPDATE token_vtxo_map SET vtxo_id = ?1 WHERE vtxo_id = ?2",
                params![refreshed.id, old_id],
            )?;
        }
        self.insert_vtxo(refreshed, expires_at)?;
        Ok(())
    }

    pub fn mark_refreshing(&self, vtxo_id: &str) -> Result<(), InventoryError> {
        let conn = self.conn.lock().expect("inventory mutex");
        let updated = conn.execute(
            "UPDATE vtxo_inventory SET status = 'refreshing' WHERE id = ?1 AND status = 'active'",
            params![vtxo_id],
        )?;
        if updated == 0 {
            return Err(InventoryError::NotFound(vtxo_id.to_string()));
        }
        Ok(())
    }

    pub fn active_reserve_msat(&self) -> Result<u64, InventoryError> {
        let conn = self.conn.lock().expect("inventory mutex");
        let total: i64 = conn.query_row(
            "SELECT COALESCE(SUM(amount_msat), 0) FROM vtxo_inventory WHERE status = 'active'",
            [],
            |row| row.get(0),
        )?;
        Ok(total as u64)
    }

    pub fn get_vtxo_for_token(&self, token_id: &str) -> Result<InventoryVtxo, InventoryError> {
        let conn = self.conn.lock().expect("inventory mutex");
        let mut stmt = conn.prepare(
            "SELECT v.id, v.amount_msat, v.status, v.expires_at, v.branch_tx_hex, v.leaf_tx_hex
             FROM token_vtxo_map t
             JOIN vtxo_inventory v ON v.id = t.vtxo_id
             WHERE t.token_id = ?1",
        )?;
        let mut rows = stmt.query(params![token_id])?;
        let Some(row) = rows.next()? else {
            return Err(InventoryError::NotFound(token_id.to_string()));
        };
        Ok(InventoryVtxo {
            id: row.get(0)?,
            amount_msat: row.get(1)?,
            status: VtxoStatus::from_str(&row.get::<_, String>(2)?)?,
            expires_at: parse_ts(&row.get::<_, String>(3)?),
            branch_tx_hex: row.get(4)?,
            leaf_tx_hex: row.get(5)?,
        })
    }
}

fn parse_ts(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ark_client::MockArkClient;

    fn sample_vtxo(amount: u64) -> Vtxo {
        MockArkClient::new("02abc", 800_000)
            .make_vtxo(amount)
    }

    #[test]
    fn allocate_release_and_refresh_queue() {
        let inv = VtxoInventory::in_memory(10).unwrap();
        let expires = Utc::now() + chrono::Duration::hours(1);
        let vtxo = sample_vtxo(100_000);
        inv.insert_vtxo(&vtxo, expires).unwrap();

        let allocated = inv.allocate_vtxo_for_tokens(50_000).unwrap();
        assert_eq!(allocated.id, vtxo.id);

        let token_id = Uuid::new_v4().to_string();
        inv.map_token_to_vtxo(&token_id, &vtxo.id, 50_000, expires).unwrap();
        inv.release_vtxo_mapping(&token_id).unwrap();

        let near_expiry = Utc::now() + chrono::Duration::minutes(5);
        let refresh_candidate = sample_vtxo(200_000);
        inv.insert_vtxo(&refresh_candidate, near_expiry).unwrap();
        let queue = inv.get_refresh_queue().unwrap();
        assert!(!queue.is_empty());
    }
}
