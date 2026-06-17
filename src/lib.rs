//! Minerva Mint — an Ark-backed Cashu mint.
//!
//! Ecash tokens issued by this mint are backed by Ark VTXOs held with an Ark
//! Service Provider, giving holders Chaumian privacy plus a unilateral exit
//! path to Bitcoin L1.

pub mod api;
pub mod ark_arkade;
pub mod ark_barkd;
pub mod ark_client;
pub mod ark_wallet_http;
pub mod bitcoin;
pub mod blind_signer;
pub mod bolt11_util;
pub mod config;
pub mod error;
pub mod health;
pub mod mint_backend;
pub mod ots;
pub mod pol;
pub mod signatory;
pub mod tasks;
pub mod types;
pub mod vtxo_inventory;
pub mod vtxo_verify;

pub use config::AppConfig;
pub use types::KEYSET_ID;
