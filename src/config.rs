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
    pub refresh_threshold_blocks: u64,
    pub default_vtxo_expiry: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BitcoinConfig {
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

    fn apply_env_overrides(&mut self) {
        if let Ok(url) = std::env::var("BITCOIN_RPC_URL") {
            self.bitcoin.rpc_url = url;
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
