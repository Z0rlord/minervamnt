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
    #[serde(default)]
    pub signatory: SignatoryConfig,
    pub bitcoin: BitcoinConfig,
    pub liquidity: LiquidityConfig,
    pub database: DatabaseConfig,
    #[serde(default)]
    pub trust: TrustConfig,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub scheduler: SchedulerConfig,
    #[serde(default)]
    pub melt: MeltConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MintConfig {
    pub name: String,
    pub description: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArkConfig {
    /// `mock` (default) or `barkd` (local barkd → live ASP).
    #[serde(default = "default_ark_backend")]
    pub backend: String,
    pub server_url: String,
    pub server_pubkey: String,
    /// barkd REST base URL when `backend = "barkd"`.
    #[serde(default = "default_barkd_url")]
    pub barkd_url: String,
    /// Max wait for board/refresh round completion.
    #[serde(default = "default_poll_timeout_secs")]
    pub poll_timeout_secs: u64,
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,
    /// Refresh VTXOs when they are within this many blocks of expiry.
    pub refresh_threshold_blocks: u64,
    /// Default VTXO lifetime in blocks (~6 months at 25920).
    pub default_vtxo_expiry: u64,
    /// On-chain address for auto-sweep after exit (`auto_claim_exits = true`).
    #[serde(default)]
    pub exit_claim_address: Option<String>,
    /// Poll exit status and call claim when claimable (barkd / wallet daemon).
    #[serde(default = "default_false")]
    pub auto_claim_exits: bool,
    /// Arkade (and other) wallet daemon REST URL (barkd-compatible API).
    #[serde(default)]
    pub wallet_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SignatoryConfig {
    /// `mock` (default), `remote` (cdk-signatory gRPC), or `local` (dev dhke).
    #[serde(default = "default_signatory_backend")]
    pub backend: String,
    /// gRPC URL for `remote`, e.g. `http://127.0.0.1:3340`.
    #[serde(default)]
    pub url: Option<String>,
    /// Optional mTLS cert directory for remote signatory.
    #[serde(default)]
    pub tls_dir: Option<String>,
}

fn default_signatory_backend() -> String {
    "mock".into()
}

fn default_false() -> bool {
    false
}

impl Default for SignatoryConfig {
    fn default() -> Self {
        SignatoryConfig {
            backend: default_signatory_backend(),
            url: None,
            tls_dir: None,
        }
    }
}

fn default_ark_backend() -> String {
    "mock".into()
}

fn default_barkd_url() -> String {
    "http://127.0.0.1:3535".into()
}

fn default_poll_timeout_secs() -> u64 {
    600
}

fn default_poll_interval_secs() -> u64 {
    5
}

#[derive(Debug, Clone, Deserialize)]
pub struct BitcoinConfig {
    /// Bitcoin Core RPC endpoint. For Minerva Mint this is the Raspberry Pi 5
    /// JSON-RPC URL for your Bitcoin Core node (localhost, VPN, or private network).
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
pub struct TrustConfig {
    pub vtxo_verify_mode: String,
    #[serde(default = "default_true")]
    pub signatory_policy_enforced: bool,
    pub max_mint_sat: Option<u64>,
    #[serde(default = "default_true")]
    pub pol_enabled: bool,
    #[serde(default)]
    pub ots: OtsConfig,
    /// Release VTXO backing when melt succeeds (requires `token_ids` or FIFO).
    #[serde(default)]
    pub release_backing_on_melt: bool,
    /// When `token_ids` are omitted, release oldest mappings FIFO up to melt amount.
    #[serde(default)]
    pub release_backing_on_melt_fifo: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OtsConfig {
    #[serde(default = "default_ots_enabled")]
    pub enabled: bool,
    #[serde(default = "default_ots_calendars")]
    pub calendar_urls: Vec<String>,
    #[serde(default = "default_ots_upgrade_interval")]
    pub upgrade_interval_secs: u64,
}

fn default_ots_enabled() -> bool {
    true
}

fn default_ots_upgrade_interval() -> u64 {
    3600
}

fn default_ots_calendars() -> Vec<String> {
    vec![
        "https://a.pool.opentimestamps.org".into(),
        "https://b.pool.opentimestamps.org".into(),
    ]
}

impl Default for OtsConfig {
    fn default() -> Self {
        OtsConfig {
            enabled: default_ots_enabled(),
            calendar_urls: default_ots_calendars(),
            upgrade_interval_secs: default_ots_upgrade_interval(),
        }
    }
}

fn default_true() -> bool {
    true
}

impl Default for TrustConfig {
    fn default() -> Self {
        TrustConfig {
            vtxo_verify_mode: "scaffold".to_string(),
            signatory_policy_enforced: true,
            max_mint_sat: None,
            pol_enabled: true,
            ots: OtsConfig::default(),
            release_backing_on_melt: false,
            release_backing_on_melt_fifo: false,
        }
    }
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

#[derive(Debug, Clone, Deserialize)]
pub struct MeltConfig {
    /// `inherit` (default) — follow `ark.backend` (`mock` → scaffold payout,
    /// `barkd`/`arkade` → live lightning pay via wallet daemon).
    /// `mock` — deterministic preimage, scaffold fee logic for non-BOLT11 requests.
    /// `barkd` — always pay via wallet daemon lightning API (requires barkd/arkade).
    #[serde(default = "default_melt_backend")]
    pub backend: String,
}

fn default_melt_backend() -> String {
    "inherit".into()
}

impl Default for MeltConfig {
    fn default() -> Self {
        MeltConfig {
            backend: default_melt_backend(),
        }
    }
}

impl AppConfig {
    /// Effective melt payout backend after resolving `inherit`.
    pub fn effective_melt_backend(&self) -> &str {
        match self.melt.backend.as_str() {
            "inherit" => self.ark.backend.as_str(),
            other => other,
        }
    }

    pub fn melt_uses_live_payout(&self) -> bool {
        matches!(self.effective_melt_backend(), "barkd" | "arkade")
    }

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
        if let Ok(v) = std::env::var("ARK_BACKEND") {
            self.ark.backend = v;
        }
        if let Ok(v) = std::env::var("ARK_SERVER_URL") {
            self.ark.server_url = v;
        }
        if let Ok(v) = std::env::var("ARK_SERVER_PUBKEY") {
            self.ark.server_pubkey = v;
        }
        if let Ok(v) = std::env::var("BARKD_URL") {
            self.ark.barkd_url = v;
        }
        if let Ok(v) = std::env::var("ARK_WALLET_URL") {
            self.ark.wallet_url = Some(v);
        }
        if let Ok(v) = std::env::var("ARK_EXIT_CLAIM_ADDRESS") {
            self.ark.exit_claim_address = Some(v);
        }
        if let Ok(v) = std::env::var("ARK_AUTO_CLAIM_EXITS") {
            self.ark.auto_claim_exits = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Ok(v) = std::env::var("SIGNATORY_BACKEND") {
            self.signatory.backend = v;
        }
        if let Ok(v) = std::env::var("SIGNATORY_URL") {
            self.signatory.url = Some(v);
        }
        if let Ok(v) = std::env::var("SIGNATORY_TLS_DIR") {
            self.signatory.tls_dir = Some(v);
        }
        if let Ok(v) = std::env::var("ARK_POLL_TIMEOUT_SECS") {
            if let Ok(n) = v.parse() {
                self.ark.poll_timeout_secs = n;
            }
        }
        if let Ok(v) = std::env::var("ARK_POLL_INTERVAL_SECS") {
            if let Ok(n) = v.parse() {
                self.ark.poll_interval_secs = n;
            }
        }
        if let Ok(v) = std::env::var("MELT_BACKEND") {
            self.melt.backend = v;
        }
        if let Ok(v) = std::env::var("BITCOIN_RPC_URL") {
            self.bitcoin.rpc_url = v;
        }
        if let Ok(v) = std::env::var("DATABASE_PATH") {
            self.database.path = v;
        }
        if let Ok(v) = std::env::var("MINERVA_VTXO_VERIFY_MODE") {
            self.trust.vtxo_verify_mode = v;
        }
        if let Ok(v) = std::env::var("MINERVA_SIGNATORY_POLICY_ENFORCED") {
            self.trust.signatory_policy_enforced = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Ok(v) = std::env::var("MINERVA_MAX_MINT_SAT") {
            self.trust.max_mint_sat = Some(v.parse().unwrap_or(0));
        }
        if let Ok(v) = std::env::var("MINERVA_POL_ENABLED") {
            self.trust.pol_enabled = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Ok(v) = std::env::var("MINERVA_OTS_ENABLED") {
            self.trust.ots.enabled = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Ok(v) = std::env::var("MINERVA_RELEASE_BACKING_ON_MELT") {
            self.trust.release_backing_on_melt = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Ok(v) = std::env::var("MINERVA_RELEASE_BACKING_ON_MELT_FIFO") {
            self.trust.release_backing_on_melt_fifo = v == "1" || v.eq_ignore_ascii_case("true");
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
        assert_eq!(cfg.ark.backend, "mock");
        assert_eq!(cfg.ark.refresh_threshold_blocks, 144);
        assert_eq!(cfg.ark.default_vtxo_expiry, 25920);
        assert!(cfg.liquidity.min_vtxo_reserve_msat > 0);
        assert_eq!(cfg.bitcoin.rpc_url, "http://127.0.0.1:8332");
        assert_eq!(cfg.trust.vtxo_verify_mode, "scaffold");
        assert!(cfg.trust.signatory_policy_enforced);
    }

    #[test]
    fn loads_config_and_env_overrides() {
        std::env::set_var("BITCOIN_RPC_URL", "http://100.64.0.5:8332");
        std::env::set_var("BITCOIN_RPC_USER", "pi");
        std::env::set_var("BITCOIN_RPC_PASSWORD", "secret");

        let config = AppConfig::load("config.toml").expect("config should load");
        assert_eq!(config.mint.name, "Minerva Mint");
        assert_eq!(config.mint.url, "https://mint.example.com");
        assert_eq!(config.bitcoin.rpc_url, "http://100.64.0.5:8332");
        assert_eq!(config.bitcoin.rpc_user.as_deref(), Some("pi"));
        assert_eq!(config.bitcoin.rpc_password.as_deref(), Some("secret"));

        std::env::remove_var("BITCOIN_RPC_URL");
        std::env::remove_var("BITCOIN_RPC_USER");
        std::env::remove_var("BITCOIN_RPC_PASSWORD");
    }
}
