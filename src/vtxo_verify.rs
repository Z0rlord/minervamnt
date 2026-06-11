//! VTXO verification before inventory insertion.
//!
//! Production uses [V-PACK](https://github.com/jgmcalpine/libvpack-rs) when the
//! ASP supplies a bundle; development uses structural checks against the mock ASP.

use crate::error::{MintError, Result};
use crate::types::Vtxo;
use std::str::FromStr;

/// How strictly to verify VTXOs at boarding time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VtxoVerifyMode {
    /// Structural sanity checks only (mock ASP / dev).
    #[default]
    Scaffold,
    /// Require `vpack_hex` and verify with the `vpack` crate.
    Vpack,
}

impl VtxoVerifyMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "scaffold" => Some(Self::Scaffold),
            "vpack" => Some(Self::Vpack),
            _ => None,
        }
    }
}

/// Verify a VTXO before it enters inventory.
pub fn verify_vtxo(vtxo: &Vtxo, mode: VtxoVerifyMode) -> Result<()> {
    verify_scaffold_structure(vtxo)?;
    match mode {
        VtxoVerifyMode::Scaffold => Ok(()),
        VtxoVerifyMode::Vpack => verify_vpack(vtxo),
    }
}

fn verify_scaffold_structure(vtxo: &Vtxo) -> Result<()> {
    if vtxo.id.is_empty() {
        return Err(MintError::InvalidRequest("VTXO id is empty".into()));
    }
    if vtxo.amount_msat == 0 {
        return Err(MintError::InvalidRequest("VTXO amount_msat is zero".into()));
    }
    if vtxo.branch_tx.is_empty() || vtxo.leaf_tx.is_empty() {
        return Err(MintError::InvalidRequest(
            "VTXO branch_tx and leaf_tx must be non-empty".into(),
        ));
    }
    if vtxo.asp_pubkey.is_empty() {
        return Err(MintError::InvalidRequest("VTXO asp_pubkey is empty".into()));
    }
    if vtxo.expiry == 0 {
        return Err(MintError::InvalidRequest("VTXO expiry height is zero".into()));
    }
    Ok(())
}

fn verify_vpack(vtxo: &Vtxo) -> Result<()> {
    let hex = vtxo.vpack_hex.as_ref().ok_or_else(|| {
        MintError::InvalidRequest(format!(
            "VTXO {} missing vpack_hex (required in vpack verify mode)",
            vtxo.id
        ))
    })?;
    let bytes = hex::decode(hex).map_err(|e| {
        MintError::InvalidRequest(format!("VTXO {} invalid vpack_hex: {e}", vtxo.id))
    })?;
    let expected_id = vpack::VtxoId::from_str(&vtxo.id).map_err(|e| {
        MintError::InvalidRequest(format!("VTXO {} invalid id for vpack: {e}", vtxo.id))
    })?;
    vpack::verify(&bytes, &expected_id).map_err(|e| {
        MintError::InvalidRequest(format!("VTXO {} vpack verification failed: {e:?}", vtxo.id))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_vtxo() -> Vtxo {
        Vtxo {
            id: "vtxo-1".into(),
            amount_msat: 1_000_000,
            expiry: 900_000,
            branch_tx: "abc123".into(),
            leaf_tx: "def456".into(),
            asp_pubkey: "02ab".into(),
            vpack_hex: None,
        }
    }

    #[test]
    fn scaffold_accepts_valid_mock_vtxo() {
        verify_vtxo(&sample_vtxo(), VtxoVerifyMode::Scaffold).unwrap();
    }

    #[test]
    fn scaffold_rejects_empty_branch_tx() {
        let mut v = sample_vtxo();
        v.branch_tx.clear();
        assert!(verify_vtxo(&v, VtxoVerifyMode::Scaffold).is_err());
    }

    #[test]
    fn vpack_mode_requires_vpack_hex() {
        assert!(verify_vtxo(&sample_vtxo(), VtxoVerifyMode::Vpack).is_err());
    }
}
