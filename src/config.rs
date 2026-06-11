//! Configuration loading: `config.toml` + environment variable overrides.
//!
//! Secrets (Bitcoin RPC credentials, ASP auth tokens) are NEVER stored in
//! `config.toml`; they are read from the environment (populated by `.env`
//! locally, or injected by the deployment platform / Vault in production).

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub mint: MintConfig,
    pub ark: ArkConfig,
    pub bitcoin: BitcoinConfig,
    pub liquidity: LiquidityConfig,
    pub database: DatabaseConfig,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub scheduler: SchedulerConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MintConfig {
    pub name: String,
    pub description: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArkConfig {
    pub server_url: String,
    pub server_pubkey: String,
    /// Refresh VTXOs when they are within this many blocks of expiry.
    pub refresh_threshold_blocks: u64,
    /// Default VTXO lifetime in blocks (~6 months at 25920).
    pub default_vtxo_expiry: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BitcoinConfig {
    /// Bitcoin Core RPC endpoint. For Minerva Mint this is the Raspberry Pi 5
    /// full node reached over Tailscale, e.g. `http://100.x.y.z:8332`.
    pub rpc_url: String,
    #[serde(default)]
    pub rpc_user: Option<String>,
    #[serde(default)]
    pub rpc_password: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LiquidityConfig {
    pub min_vtxo_reserve_msat: u64,
    pub max_single_vtxo_msat: u64,
    pub auto_board_threshold: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 3338,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SchedulerConfig {
    pub refresh_interval_secs: u64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            refresh_interval_secs: 60,
        }
    }
}

impl AppConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let _ = dotenvy::dotenv();
        let contents = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("reading config {}", path.as_ref().display()))?;
        let mut config: AppConfig = toml::from_str(&contents).context("parsing config.toml")?;
        config.apply_env_overrides();
        Ok(config)
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.server.host, self.server.port)
    }

    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("MINERVA_MINT_URL") {
            self.mint.url = v;
        }
        if let Ok(v) = std::env::var("ARK_SERVER_URL") {
            self.ark.server_url = v;
        }
        if let Ok(v) = std::env::var("ARK_SERVER_PUBKEY") {
            self.ark.server_pubkey = v;
        }
        if let Ok(v) = std::env::var("BITCOIN_RPC_URL") {
            self.bitcoin.rpc_url = v;
        }
        if let Ok(v) = std::env::var("DATABASE_PATH") {
            self.database.path = v;
        }
        if let Ok(v) = std::env::var("BIND_ADDR") {
            if let Some((host, port)) = v.rsplit_once(':') {
                if let Ok(port) = port.parse() {
                    self.server.host = host.to_string();
                    self.server.port = port;
                }
            }
        }
        if let Ok(user) = std::env::var("BITCOIN_RPC_USER") {
            self.bitcoin.rpc_user = Some(user);
        }
        if let Ok(pass) = std::env::var("BITCOIN_RPC_PASSWORD") {
            self.bitcoin.rpc_password = Some(pass);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repo_config_toml() {
        let raw = include_str!("../config.toml");
        let cfg: AppConfig = toml::from_str(raw).expect("config.toml must parse");
        assert_eq!(cfg.mint.name, "Minerva Mint");
        assert_eq!(cfg.ark.refresh_threshold_blocks, 144);
        assert_eq!(cfg.ark.default_vtxo_expiry, 25920);
        assert!(cfg.liquidity.min_vtxo_reserve_msat > 0);
        assert_eq!(cfg.bitcoin.rpc_url, "http://100.75.188.125:8332");
    }

    #[test]
    fn loads_config_and_env_overrides() {
        std::env::set_var("BITCOIN_RPC_URL", "http://100.64.0.5:8332");
        std::env::set_var("BITCOIN_RPC_USER", "pi");
        std::env::set_var("BITCOIN_RPC_PASSWORD", "secret");

        let config = AppConfig::load("config.toml").expect("config should load");
        assert_eq!(config.mint.name, "Minerva Mint");
        assert_eq!(config.mint.url, "https://minervamnt.xyz");
        assert_eq!(config.bitcoin.rpc_url, "http://100.64.0.5:8332");
        assert_eq!(config.bitcoin.rpc_user.as_deref(), Some("pi"));
        assert_eq!(config.bitcoin.rpc_password.as_deref(), Some("secret"));

        std::env::remove_var("BITCOIN_RPC_URL");
        std::env::remove_var("BITCOIN_RPC_USER");
        std::env::remove_var("BITCOIN_RPC_PASSWORD");
    }
}
