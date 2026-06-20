//! BOLT11 invoice parsing for melt quotes.

use crate::error::{MintError, Result};
use cdk_common::bitcoin::hashes::Hash;
use lightning_invoice::{Bolt11Invoice, Currency};

/// Decode a BOLT11 invoice and return `(amount_sat, payment_hash_hex)`.
pub fn decode_invoice(invoice_str: &str) -> Result<(u64, String)> {
    let invoice: Bolt11Invoice = invoice_str
        .trim()
        .parse()
        .map_err(|e| MintError::InvalidRequest(format!("invalid bolt11 invoice: {e}")))?;

    if !matches!(
        invoice.currency(),
        Currency::Bitcoin | Currency::BitcoinTestnet | Currency::Regtest | Currency::Signet
    ) {
        return Err(MintError::InvalidRequest(format!(
            "unsupported invoice currency: {:?}",
            invoice.currency()
        )));
    }

    let msat = invoice
        .amount_milli_satoshis()
        .ok_or_else(|| MintError::InvalidRequest("invoice has no amount".into()))?;
    let amount_sat = msat.div_ceil(1000);

    let payment_hash = hex::encode(invoice.payment_hash().to_byte_array());
    Ok((amount_sat, payment_hash))
}

/// Scaffold fallback when melt runs in mock mode and the request is not valid BOLT11.
pub fn mock_amount_from_request(request: &str) -> u64 {
    (request.len() as u64).saturating_mul(100).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_amount_is_stable() {
        assert_eq!(mock_amount_from_request("lnbc1"), 500);
        assert_eq!(mock_amount_from_request(""), 1);
    }

    #[test]
    fn rejects_garbage_invoice() {
        assert!(decode_invoice("not-an-invoice").is_err());
    }
}
