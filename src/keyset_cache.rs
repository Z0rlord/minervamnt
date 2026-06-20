//! Cached mint keyset metadata from the blind signatory (mock, remote CDK, or local).

use crate::types::{KeysetInfo, KEYSET_ID};

#[derive(Debug, Clone)]
pub struct MintKeysetState {
    pub pubkey: String,
    pub active_keyset_id: String,
    pub keysets: Vec<KeysetInfo>,
}

impl MintKeysetState {
    pub fn mock_default() -> Self {
        Self {
            pubkey: "02".to_string() + &"11".repeat(32),
            active_keyset_id: KEYSET_ID.to_string(),
            keysets: vec![KeysetInfo {
                id: KEYSET_ID.to_string(),
                unit: "sat".to_string(),
                active: true,
                input_fee_ppk: Some(0),
            }],
        }
    }
}
