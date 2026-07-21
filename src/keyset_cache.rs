//! Cached mint keyset metadata from the blind signatory (mock, remote CDK, or local).

use crate::types::{KeysetInfo, KeysetKeys, KEYSET_ID};
use std::collections::BTreeMap;

/// Amounts advertised for signers that use one key for every denomination
/// (mock and local dev backends). Powers of two per NUT-02 convention.
pub const DEV_AMOUNTS: [u64; 16] = [
    1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768,
];

#[derive(Debug, Clone)]
pub struct MintKeysetState {
    pub pubkey: String,
    pub active_keyset_id: String,
    pub keysets: Vec<KeysetInfo>,
    /// NUT-01 public keys per keyset (amount -> compressed pubkey hex).
    pub keys: Vec<KeysetKeys>,
}

impl MintKeysetState {
    pub fn mock_default() -> Self {
        let pubkey = "02".to_string() + &"11".repeat(32);
        Self {
            pubkey: pubkey.clone(),
            active_keyset_id: KEYSET_ID.to_string(),
            keysets: vec![KeysetInfo {
                id: KEYSET_ID.to_string(),
                unit: "sat".to_string(),
                active: true,
                input_fee_ppk: Some(0),
            }],
            keys: vec![single_key_keyset(KEYSET_ID, &pubkey)],
        }
    }
}

/// Build a NUT-01 keys entry where every dev amount maps to the same pubkey.
pub fn single_key_keyset(id: &str, pubkey_hex: &str) -> KeysetKeys {
    let keys: BTreeMap<u64, String> = DEV_AMOUNTS
        .iter()
        .map(|amount| (*amount, pubkey_hex.to_string()))
        .collect();
    KeysetKeys {
        id: id.to_string(),
        unit: "sat".to_string(),
        keys,
    }
}
