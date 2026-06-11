//! OpenTimestamps integration for PoL epoch roots.
//!
//! On epoch close the mint submits the 32-byte PoL root hash to public
//! [OpenTimestamps](https://opentimestamps.org/) calendar servers. The returned
//! `.ots` proof is stored and can later be upgraded to a Bitcoin attestation.

use crate::error::{MintError, Result};
use async_trait::async_trait;
use sha2::{Digest, Sha256};

/// Result of a successful calendar stamp.
#[derive(Debug, Clone)]
pub struct OtsStampResult {
    pub calendar_url: String,
    pub proof_hex: String,
}

/// Stamp a 32-byte digest with OpenTimestamps.
#[async_trait]
pub trait OtsStamper: Send + Sync {
    async fn stamp_digest(&self, digest: [u8; 32]) -> Result<OtsStampResult>;
}

/// HTTP client for public OTS calendar `/digest` endpoints.
pub struct HttpOtsStamper {
    client: reqwest::Client,
    calendar_urls: Vec<String>,
}

impl HttpOtsStamper {
    pub fn new(calendar_urls: Vec<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("minerva-mint/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| MintError::InvalidRequest(format!("OTS HTTP client: {e}")))?;
        if calendar_urls.is_empty() {
            return Err(MintError::InvalidRequest(
                "no OpenTimestamps calendar URLs configured".into(),
            ));
        }
        Ok(HttpOtsStamper {
            client,
            calendar_urls,
        })
    }
}

#[async_trait]
impl OtsStamper for HttpOtsStamper {
    async fn stamp_digest(&self, digest: [u8; 32]) -> Result<OtsStampResult> {
        let mut last_err = String::new();
        for url in &self.calendar_urls {
            let endpoint = format!("{}/digest", url.trim_end_matches('/'));
            match self
                .client
                .post(&endpoint)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(digest.to_vec())
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    let bytes = resp.bytes().await.map_err(|e| {
                        MintError::InvalidRequest(format!("OTS read body from {url}: {e}"))
                    })?;
                    if bytes.is_empty() {
                        last_err = format!("{url}: empty proof");
                        continue;
                    }
                    return Ok(OtsStampResult {
                        calendar_url: url.clone(),
                        proof_hex: hex::encode(bytes),
                    });
                }
                Ok(resp) => {
                    last_err = format!("{url}: HTTP {}", resp.status());
                }
                Err(e) => {
                    last_err = format!("{url}: {e}");
                }
            }
        }
        Err(MintError::InvalidRequest(format!(
            "OpenTimestamps stamp failed: {last_err}"
        )))
    }
}

/// Deterministic mock stamper for tests (no network).
pub struct MockOtsStamper;

#[async_trait]
impl OtsStamper for MockOtsStamper {
    async fn stamp_digest(&self, digest: [u8; 32]) -> Result<OtsStampResult> {
        let mut h = Sha256::new();
        h.update(b"minerva-mock-ots");
        h.update(digest);
        Ok(OtsStampResult {
            calendar_url: "mock://ots".into(),
            proof_hex: hex::encode(h.finalize()),
        })
    }
}

/// Parse a PoL epoch root hash (hex) into a 32-byte digest for stamping.
pub fn digest_from_root_hex(root_hex: &str) -> Result<[u8; 32]> {
    let bytes = hex::decode(root_hex).map_err(|e| {
        MintError::InvalidRequest(format!("invalid PoL root hash hex: {e}"))
    })?;
    if bytes.len() != 32 {
        return Err(MintError::InvalidRequest(format!(
            "PoL root hash must be 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_stamper_is_deterministic() {
        let digest = [0xab; 32];
        let s = MockOtsStamper;
        let a = s.stamp_digest(digest).await.unwrap();
        let b = s.stamp_digest(digest).await.unwrap();
        assert_eq!(a.proof_hex, b.proof_hex);
    }

    #[test]
    fn digest_from_root_hex_validates_length() {
        let d = digest_from_root_hex(&"aa".repeat(32)).unwrap();
        assert_eq!(d[0], 0xaa);
        assert!(digest_from_root_hex("abcd").is_err());
    }
}
