//! Minerva Mint — an Ark-backed Cashu mint.
//!
//! Ecash tokens issued by this mint are backed by Ark VTXOs held with an Ark
//! Service Provider, giving holders Chaumian privacy plus a unilateral exit
//! path to Bitcoin L1.

pub mod api;
pub mod ark_client;
pub mod bitcoin;
pub mod config;
pub mod error;
pub mod health;
pub mod mint_backend;
pub mod tasks;
pub mod types;
pub mod vtxo_inventory;

pub use config::AppConfig;
