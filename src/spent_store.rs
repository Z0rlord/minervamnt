//! Persistent double-spend guard for spent Cashu proof secrets.

use crate::error::{MintError, Result};
use crate::types::Proof;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::{Arc, Mutex};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS spent_secrets (
    secret      TEXT PRIMARY KEY,
    keyset_id   TEXT NOT NULL,
    amount_sat  INTEGER NOT NULL,
    spent_at    INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_spent_keyset ON spent_secrets(keyset_id);
"#;

#[derive(Clone)]
pub struct SpentSecretStore {
    conn: Arc<Mutex<Connection>>,
}

impl SpentSecretStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_connection(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        Ok(SpentSecretStore {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

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
                "INSERT INTO spent_secrets (secret, keyset_id, amount_sat, spent_at)
                 VALUES (?1, ?2, ?3, ?4)",
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
            .mark_spent_batch(
                crate::types::KEYSET_ID,
                &[proof("s2", 32), proof("s1", 32)],
            )
            .unwrap_err();
        assert!(matches!(err, MintError::TokenAlreadySpent));
        assert!(!store.is_spent("s2").unwrap());
    }
}
