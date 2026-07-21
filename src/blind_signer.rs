//! Blind signing backends for Cashu NUT-00 (BDHKE).
//!
//! | Backend | Use case |
//! | ------- | -------- |
//! | `mock`  | Tests and local dev without keys |
//! | `remote`| Production — `cdk-signatory` gRPC service |
//! | `local` | Dev signet — single mint key via `SIGNATORY_MINT_SECRET` (cashu dhke) |

use crate::config::SignatoryConfig;
use crate::error::{MintError, Result};
use crate::keyset_cache::{single_key_keyset, MintKeysetState};
use crate::types::{BlindSignature, BlindedMessage, KeysetInfo, KeysetKeys, Proof, KEYSET_ID};
use async_trait::async_trait;
use cdk_common::{
    secret::Secret, Amount, BlindedMessage as CdkBlindedMessage, Id, Proof as CdkProof, PublicKey,
};
use cdk_signatory::signatory::Signatory;
use cdk_signatory::SignatoryRpcClient;
use sha2::{Digest, Sha256};
use std::str::FromStr;
use std::sync::Arc;

#[async_trait]
pub trait BlindSigner: Send + Sync {
    async fn blind_sign(&self, outputs: &[BlindedMessage]) -> Result<Vec<BlindSignature>>;
    async fn verify_proofs(&self, proofs: &[crate::types::Proof]) -> Result<()>;
    async fn mint_pubkey_hex(&self) -> Result<String>;
    async fn keyset_state(&self) -> Result<MintKeysetState>;
}

pub fn build_blind_signer(config: &SignatoryConfig) -> Result<Arc<dyn BlindSigner>> {
    match config.backend.as_str() {
        "mock" => Ok(Arc::new(MockBlindSigner)),
        "remote" => {
            let url = config
                .url
                .as_ref()
                .ok_or_else(|| MintError::InvalidRequest("signatory.url required for remote".into()))?
                .clone();
            let tls_dir = config.tls_dir.as_deref();
            tracing::info!(%url, "using remote CDK signatory");
            Ok(Arc::new(RemoteCdkSigner::new(url, tls_dir)))
        }
        "local" => {
            let secret = std::env::var("SIGNATORY_MINT_SECRET").map_err(|_| {
                MintError::InvalidRequest("SIGNATORY_MINT_SECRET required for local signatory".into())
            })?;
            tracing::warn!("using LOCAL signatory — dev only; not interoperable with production keysets");
            Ok(Arc::new(LocalDhkeSigner::from_hex_secret(&secret)?))
        }
        other => Err(MintError::InvalidRequest(format!(
            "unknown signatory.backend {other:?}; expected mock|remote|local"
        ))),
    }
}

pub struct MockBlindSigner;

#[async_trait]
impl BlindSigner for MockBlindSigner {
    async fn blind_sign(&self, outputs: &[BlindedMessage]) -> Result<Vec<BlindSignature>> {
        Ok(outputs.iter().map(mock_blind_sign).collect())
    }

    async fn verify_proofs(&self, _proofs: &[Proof]) -> Result<()> {
        Ok(())
    }

    async fn mint_pubkey_hex(&self) -> Result<String> {
        Ok(self.keyset_state().await?.pubkey)
    }

    async fn keyset_state(&self) -> Result<MintKeysetState> {
        Ok(MintKeysetState::mock_default())
    }
}

fn mock_blind_sign(output: &BlindedMessage) -> BlindSignature {
    let mut hasher = Sha256::new();
    hasher.update(b"minerva-mock-sig");
    hasher.update(output.id.as_bytes());
    hasher.update(output.amount.to_be_bytes());
    hasher.update(output.b.as_bytes());
    BlindSignature {
        amount: output.amount,
        id: output.id.clone(),
        c: hex::encode(hasher.finalize()),
    }
}

/// gRPC client to a running `cdk-signatory` process.
pub struct RemoteCdkSigner {
    url: String,
    tls_dir: Option<std::path::PathBuf>,
    client: tokio::sync::Mutex<Option<SignatoryRpcClient>>,
}

impl RemoteCdkSigner {
    pub fn new(url: String, tls_dir: Option<&str>) -> Self {
        Self {
            url,
            tls_dir: tls_dir.map(std::path::PathBuf::from),
            client: tokio::sync::Mutex::new(None),
        }
    }

    async fn connect(&self) -> Result<()> {
        let mut guard = self.client.lock().await;
        if guard.is_none() {
            let url = normalize_signatory_grpc_url(&self.url, self.tls_dir.is_some())?;
            let client = SignatoryRpcClient::new(url, self.tls_dir.as_deref())
                .await
                .map_err(|e| MintError::InvalidRequest(format!("signatory connect: {e}")))?;
            *guard = Some(client);
        }
        Ok(())
    }
}

/// Tonic expects `https://` when mTLS client certs are configured.
fn normalize_signatory_grpc_url(url: &str, tls: bool) -> Result<String> {
    if tls {
        if let Some(rest) = url.strip_prefix("http://") {
            Ok(format!("https://{rest}"))
        } else if url.starts_with("https://") {
            Ok(url.to_string())
        } else {
            Err(MintError::InvalidRequest(format!(
                "invalid signatory.url {url:?}; expected http(s)://host:port"
            )))
        }
    } else if url.starts_with("http://") || url.starts_with("https://") {
        Ok(url.to_string())
    } else {
        Err(MintError::InvalidRequest(format!(
            "invalid signatory.url {url:?}; expected http(s)://host:port"
        )))
    }
}

#[async_trait]
impl BlindSigner for RemoteCdkSigner {
    async fn blind_sign(&self, outputs: &[BlindedMessage]) -> Result<Vec<BlindSignature>> {
        self.connect().await?;
        let cdk_msgs: Vec<CdkBlindedMessage> = outputs
            .iter()
            .map(to_cdk_blinded_message)
            .collect::<Result<_>>()?;
        let guard = self.client.lock().await;
        let client = guard.as_ref().expect("connected");
        let sigs = Signatory::blind_sign(client, cdk_msgs)
            .await
            .map_err(|e| MintError::InvalidRequest(format!("signatory blind_sign: {e}")))?;
        sigs.into_iter().map(from_cdk_blind_signature).collect()
    }

    async fn verify_proofs(&self, proofs: &[Proof]) -> Result<()> {
        if proofs.is_empty() {
            return Ok(());
        }
        self.connect().await?;
        let cdk_proofs: Vec<CdkProof> = proofs.iter().map(to_cdk_proof).collect::<Result<_>>()?;
        let guard = self.client.lock().await;
        let client = guard.as_ref().expect("connected");
        Signatory::verify_proofs(client, cdk_proofs)
            .await
            .map_err(|e| MintError::InvalidRequest(format!("signatory verify_proofs: {e}")))?;
        Ok(())
    }

    async fn mint_pubkey_hex(&self) -> Result<String> {
        Ok(self.keyset_state().await?.pubkey)
    }

    async fn keyset_state(&self) -> Result<MintKeysetState> {
        self.connect().await?;
        let guard = self.client.lock().await;
        let client = guard.as_ref().expect("connected");
        let keysets = Signatory::keysets(client)
            .await
            .map_err(|e| MintError::InvalidRequest(format!("signatory keysets: {e}")))?;
        Ok(cdk_keysets_to_state(&keysets))
    }
}

/// Dev-only signer using cashu dhke with a single active secret key.
pub struct LocalDhkeSigner {
    secret: cdk_common::SecretKey,
    pubkey: PublicKey,
}

impl LocalDhkeSigner {
    pub fn from_hex_secret(hex_secret: &str) -> Result<Self> {
        let secret = cdk_common::SecretKey::from_str(hex_secret)
            .map_err(|e| MintError::InvalidRequest(format!("invalid SIGNATORY_MINT_SECRET: {e}")))?;
        let pubkey = secret.public_key();
        Ok(Self { secret, pubkey })
    }
}

#[async_trait]
impl BlindSigner for LocalDhkeSigner {
    async fn blind_sign(&self, outputs: &[BlindedMessage]) -> Result<Vec<BlindSignature>> {
        let mut sigs = Vec::with_capacity(outputs.len());
        for output in outputs {
            let b = PublicKey::from_str(&output.b)
                .map_err(|e| MintError::InvalidRequest(format!("invalid B_: {e}")))?;
            let c = cdk_common::dhke::sign_message(&self.secret, &b)
                .map_err(|e| MintError::InvalidRequest(format!("dhke sign: {e}")))?;
            sigs.push(BlindSignature {
                amount: output.amount,
                id: output.id.clone(),
                c: c.to_string(),
            });
        }
        Ok(sigs)
    }

    async fn verify_proofs(&self, proofs: &[Proof]) -> Result<()> {
        for proof in proofs {
            let c = PublicKey::from_str(&proof.c)
                .map_err(|e| MintError::InvalidRequest(format!("invalid proof C: {e}")))?;
            cdk_common::dhke::verify_message(&self.secret, c, proof.secret.as_bytes())
                .map_err(|e| MintError::InvalidRequest(format!("invalid proof: {e}")))?;
        }
        Ok(())
    }

    async fn mint_pubkey_hex(&self) -> Result<String> {
        Ok(self.keyset_state().await?.pubkey)
    }

    async fn keyset_state(&self) -> Result<MintKeysetState> {
        let pubkey = self.pubkey.to_string();
        Ok(MintKeysetState {
            active_keyset_id: KEYSET_ID.to_string(),
            keysets: vec![KeysetInfo {
                id: KEYSET_ID.to_string(),
                unit: "sat".to_string(),
                active: true,
                input_fee_ppk: Some(0),
            }],
            keys: vec![single_key_keyset(KEYSET_ID, &pubkey)],
            pubkey,
        })
    }
}

fn to_cdk_proof(proof: &Proof) -> Result<CdkProof> {
    let keyset_id = Id::from_str(&proof.id)
        .map_err(|e| MintError::InvalidRequest(format!("invalid keyset id: {e}")))?;
    let c = PublicKey::from_str(&proof.c)
        .map_err(|e| MintError::InvalidRequest(format!("invalid proof C: {e}")))?;
    Ok(CdkProof {
        amount: Amount::from(proof.amount),
        keyset_id,
        secret: Secret::new(proof.secret.clone()),
        c,
        witness: None,
        dleq: None,
        p2pk_e: None,
    })
}

fn to_cdk_blinded_message(msg: &BlindedMessage) -> Result<CdkBlindedMessage> {
    let amount = Amount::from(msg.amount);
    let keyset_id = Id::from_str(&msg.id)
        .map_err(|e| MintError::InvalidRequest(format!("invalid keyset id: {e}")))?;
    let blinded_secret = PublicKey::from_str(&msg.b)
        .map_err(|e| MintError::InvalidRequest(format!("invalid B_: {e}")))?;
    Ok(CdkBlindedMessage::new(amount, keyset_id, blinded_secret))
}

fn from_cdk_blind_signature(sig: cdk_common::BlindSignature) -> Result<BlindSignature> {
    Ok(BlindSignature {
        amount: u64::from(sig.amount),
        id: sig.keyset_id.to_string(),
        c: sig.c.to_string(),
    })
}

fn cdk_keysets_to_state(keysets: &cdk_signatory::signatory::SignatoryKeysets) -> MintKeysetState {
    let entries: Vec<KeysetInfo> = keysets
        .keysets
        .iter()
        .map(|ks| KeysetInfo {
            id: ks.id.to_string(),
            unit: ks.unit.to_string(),
            active: ks.active,
            input_fee_ppk: Some(ks.input_fee_ppk),
        })
        .collect();
    let keys: Vec<KeysetKeys> = keysets
        .keysets
        .iter()
        .map(|ks| KeysetKeys {
            id: ks.id.to_string(),
            unit: ks.unit.to_string(),
            keys: ks
                .keys
                .iter()
                .map(|(amount, pk)| (u64::from(*amount), pk.to_string()))
                .collect(),
        })
        .collect();
    let active_keyset_id = entries
        .iter()
        .find(|k| k.active)
        .or_else(|| entries.first())
        .map(|k| k.id.clone())
        .unwrap_or_else(|| KEYSET_ID.to_string());
    MintKeysetState {
        pubkey: keysets.pubkey.to_string(),
        active_keyset_id,
        keysets: entries,
        keys,
    }
}
