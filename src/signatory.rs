//! Signatory policy gate — mint-side checks before blind signing.
//!
//! In production the actual BDHKE signing happens in a remote `cdk-signatory`
//! service. This module enforces **when** signing is allowed so a compromised
//! API cannot inflate supply without a paid quote and VTXO backing.

use crate::error::{MintError, Result};
use crate::types::{BlindedMessage, QuoteState, KEYSET_ID};

/// Context for a mint (deposit) signing request.
#[derive(Debug, Clone)]
pub struct MintSignRequest<'a> {
    pub quote_id: &'a str,
    pub amount_sat: u64,
    pub outputs: &'a [BlindedMessage],
    pub quote_state: QuoteState,
    pub quote_expiry: u64,
    pub now: u64,
    /// VTXO id from `allocate_vtxo_for_tokens`, if allocation succeeded.
    pub vtxo_id: Option<&'a str>,
}

/// Context for a swap signing request (no VTXO interaction).
#[derive(Debug, Clone)]
pub struct SwapSignRequest<'a> {
    pub input_total_sat: u64,
    pub outputs: &'a [BlindedMessage],
}

/// Gate between mint logic and the signatory (local mock or remote CDK).
pub trait SignatoryPolicy: Send + Sync {
    fn can_sign_mint(&self, req: &MintSignRequest<'_>) -> Result<()>;
    fn can_sign_swap(&self, req: &SwapSignRequest<'_>) -> Result<()>;
}

/// Default production policy for the scaffold (mirrors planned CDK signatory rules).
pub struct DefaultSignatoryPolicy {
    max_mint_sat: Option<u64>,
}

impl DefaultSignatoryPolicy {
    pub fn new(max_mint_sat: Option<u64>) -> Self {
        Self { max_mint_sat }
    }
}

impl DefaultSignatoryPolicy {
    fn validate_outputs(outputs: &[BlindedMessage], expected_total: u64) -> Result<()> {
        if outputs.is_empty() {
            return Err(MintError::InvalidRequest("no outputs for signing".into()));
        }
        let mut total = 0u64;
        for o in outputs {
            if o.id != KEYSET_ID {
                return Err(MintError::InvalidRequest(format!(
                    "signing rejected: unknown keyset {}",
                    o.id
                )));
            }
            if !o.amount.is_power_of_two() {
                return Err(MintError::InvalidRequest(format!(
                    "signing rejected: amount {} not power of two",
                    o.amount
                )));
            }
            total += o.amount;
        }
        if total != expected_total {
            return Err(MintError::Unbalanced {
                inputs: expected_total,
                outputs: total,
            });
        }
        Ok(())
    }
}

impl SignatoryPolicy for DefaultSignatoryPolicy {
    fn can_sign_mint(&self, req: &MintSignRequest<'_>) -> Result<()> {
        if req.quote_state != QuoteState::Paid {
            return Err(MintError::QuoteNotPaid(req.quote_id.to_string()));
        }
        if req.now > req.quote_expiry {
            return Err(MintError::InvalidRequest(format!(
                "signing rejected: quote expired {}",
                req.quote_id
            )));
        }
        Self::validate_outputs(req.outputs, req.amount_sat)?;
        if req.vtxo_id.is_none() {
            return Err(MintError::InvalidRequest(format!(
                "signing rejected: no VTXO allocation for quote {}",
                req.quote_id
            )));
        }
        if let Some(max) = self.max_mint_sat {
            if req.amount_sat > max {
                return Err(MintError::InvalidRequest(format!(
                    "signing rejected: amount {} exceeds max_mint {}",
                    req.amount_sat, max
                )));
            }
        }
        Ok(())
    }

    fn can_sign_swap(&self, req: &SwapSignRequest<'_>) -> Result<()> {
        Self::validate_outputs(req.outputs, req.input_total_sat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::BlindedMessage;

    fn mint_outputs(amount: u64) -> Vec<BlindedMessage> {
        vec![BlindedMessage {
            amount,
            id: KEYSET_ID.to_string(),
            b: "02b".to_string(),
        }]
    }

    #[test]
    fn rejects_unpaid_quote() {
        let p = DefaultSignatoryPolicy::new(None);
        let outputs = mint_outputs(64);
        let req = MintSignRequest {
            quote_id: "q1",
            amount_sat: 64,
            outputs: &outputs,
            quote_state: QuoteState::Unpaid,
            quote_expiry: u64::MAX,
            now: 0,
            vtxo_id: Some("v1"),
        };
        assert!(p.can_sign_mint(&req).is_err());
    }

    #[test]
    fn rejects_missing_vtxo_allocation() {
        let p = DefaultSignatoryPolicy::new(None);
        let outputs = mint_outputs(64);
        let req = MintSignRequest {
            quote_id: "q1",
            amount_sat: 64,
            outputs: &outputs,
            quote_state: QuoteState::Paid,
            quote_expiry: u64::MAX,
            now: 0,
            vtxo_id: None,
        };
        assert!(p.can_sign_mint(&req).is_err());
    }

    #[test]
    fn accepts_paid_quote_with_vtxo() {
        let p = DefaultSignatoryPolicy::new(None);
        let outputs = mint_outputs(64);
        let req = MintSignRequest {
            quote_id: "q1",
            amount_sat: 64,
            outputs: &outputs,
            quote_state: QuoteState::Paid,
            quote_expiry: u64::MAX,
            now: 0,
            vtxo_id: Some("v1"),
        };
        p.can_sign_mint(&req).unwrap();
    }

    #[test]
    fn enforces_max_mint() {
        let p = DefaultSignatoryPolicy::new(Some(32));
        let outputs = mint_outputs(64);
        let req = MintSignRequest {
            quote_id: "q1",
            amount_sat: 64,
            outputs: &outputs,
            quote_state: QuoteState::Paid,
            quote_expiry: u64::MAX,
            now: 0,
            vtxo_id: Some("v1"),
        };
        assert!(p.can_sign_mint(&req).is_err());
    }
}
