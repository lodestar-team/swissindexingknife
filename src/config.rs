use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub indexer: IndexerConfig,
    pub server: ServerConfig,
    pub docker: DockerConfig,
    pub api: ApiConfig,
    pub network: NetworkConfig,
    pub economics: EconomicsConfig,
    #[serde(default)]
    pub grt_price: GrtPriceConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IndexerConfig {
    pub address: String,
    pub operator_address: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    pub host: String,
    #[serde(default = "default_ssh_user")]
    pub user: String,
    pub ssh_key: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DockerConfig {
    #[serde(default = "default_agent_container")]
    pub indexer_agent_container: String,
    #[serde(default = "default_index_node")]
    pub graph_node_index_container: String,
    #[serde(default = "default_query_node")]
    pub graph_node_query_container: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ApiConfig {
    /// "ssh_docker" | "local_docker" | "host_port"
    #[serde(default = "default_access_method")]
    pub access_method: String,
    #[serde(default = "default_mgmt_port")]
    pub management_api_port: u16,
    #[serde(default = "default_admin_port")]
    pub graph_node_admin_port: u16,
    #[serde(default = "default_status_port")]
    pub graph_node_status_port: u16,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NetworkConfig {
    /// Full URL including API key
    pub subgraph_url: String,
    #[serde(default = "default_ipfs_url")]
    pub ipfs_url: String,
    #[serde(default = "default_protocol_network")]
    pub protocol_network: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EconomicsConfig {
    #[serde(default = "default_monthly_costs")]
    pub monthly_costs_usd: f64,
    /// Basis points out of 10000
    #[serde(default = "default_delegation_cut")]
    pub delegation_cut_bps: u32,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct GrtPriceConfig {
    /// Set manually to skip CoinGecko fetch
    pub manual_price_usd: Option<f64>,
}

// Defaults
fn default_ssh_user() -> String { "root".into() }
fn default_agent_container() -> String { "indexer-agent".into() }
fn default_index_node() -> String { "index-node-0".into() }
fn default_query_node() -> String { "query-node-0".into() }
fn default_access_method() -> String { "ssh_docker".into() }
fn default_mgmt_port() -> u16 { 8000 }
fn default_admin_port() -> u16 { 8020 }
fn default_status_port() -> u16 { 8030 }
fn default_ipfs_url() -> String { "https://api.thegraph.com/ipfs/api/v0".into() }
fn default_protocol_network() -> String { "eip155:42161".into() }
fn default_monthly_costs() -> f64 { 368.0 }
fn default_delegation_cut() -> u32 { 1000 }

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("config not found at {}\nRun `sik init` to create one.", path.display()))?;
        toml::from_str(&content)
            .with_context(|| format!("invalid config at {}", path.display()))
    }

    pub fn config_path() -> PathBuf {
        config_path()
    }
}

fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".lodestar")
        .join("config.toml")
}

/// Write an example config. Used by `sik init`.
pub fn example_config() -> &'static str {
    r#"[indexer]
address = "0xb43B2CCCceadA5292732a8C58ae134AdEFcE09Bb"
operator_address = "0xB70781305939A39e74Aa918416Df1b893e1Bd904"

[server]
host = "65.109.22.252"
user = "root"
ssh_key = "~/.ssh/id_ed25519_robotzner"

[docker]
indexer_agent_container = "indexer-agent"
graph_node_index_container = "index-node-0"
graph_node_query_container = "query-node-0"

[api]
access_method = "ssh_docker"   # ssh_docker | local_docker | host_port
management_api_port = 8000
graph_node_admin_port = 8020
graph_node_status_port = 8030

[network]
subgraph_url = "https://api.studio.thegraph.com/query/YOUR_ID/graph-network-arbitrum/version/latest"
ipfs_url = "https://api.thegraph.com/ipfs/api/v0"
protocol_network = "eip155:42161"

[economics]
monthly_costs_usd = 368.0
delegation_cut_bps = 1000   # 10%

[grt_price]
# Uncomment to override CoinGecko price fetch:
# manual_price_usd = 0.02646
"#
}
