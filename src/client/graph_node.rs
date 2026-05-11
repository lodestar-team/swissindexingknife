use anyhow::Result;
use serde_json::Value;

use crate::executor::RemoteExecutor;

pub struct GraphNodeClient {
    exec: RemoteExecutor,
}

impl GraphNodeClient {
    pub fn new(exec: RemoteExecutor) -> Self {
        Self { exec }
    }

    async fn query(&self, gql: &str) -> Result<Value> {
        // Routes through indexer-agent container → query-node-0:8030
        // QUIRK: graph-node containers lack curl, so we route via the agent container
        self.exec.graph_node_graphql(gql).await
    }

    /// All deployment sync statuses.
    pub async fn all_statuses(&self) -> Result<Vec<SyncStatus>> {
        // QUIRK: field is `latestBlock` and `chainHeadBlock` — NOT `headBlock`
        let data = self.query(r#"{
          indexingStatuses {
            subgraph
            synced
            health
            node
            chains {
              network
              chainHeadBlock { number }
              latestBlock { number }
            }
          }
        }"#).await?;

        let statuses = data["indexingStatuses"].as_array()
            .cloned()
            .unwrap_or_default();

        Ok(statuses.into_iter().map(parse_sync_status).collect())
    }

    /// Sync status for a single deployment by IPFS hash.
    pub async fn status_for(&self, ipfs_hash: &str) -> Result<Option<SyncStatus>> {
        let gql = format!(r#"{{
          indexingStatuses(subgraphs: ["{}"] ) {{
            subgraph
            synced
            health
            node
            chains {{
              network
              chainHeadBlock {{ number }}
              latestBlock {{ number }}
            }}
          }}
        }}"#, ipfs_hash);

        let data = self.query(&gql).await?;
        let mut statuses = data["indexingStatuses"].as_array()
            .cloned()
            .unwrap_or_default();

        Ok(statuses.pop().map(|s| parse_sync_status(s)))
    }
}

fn parse_sync_status(s: Value) -> SyncStatus {
    let chain = s["chains"].as_array()
        .and_then(|c| c.first())
        .cloned()
        .unwrap_or(Value::Null);

    let latest = chain["latestBlock"]["number"]
        .as_str()
        .and_then(|n| n.parse::<u64>().ok())
        .or_else(|| chain["latestBlock"]["number"].as_u64())
        .unwrap_or(0);

    let head = chain["chainHeadBlock"]["number"]
        .as_str()
        .and_then(|n| n.parse::<u64>().ok())
        .or_else(|| chain["chainHeadBlock"]["number"].as_u64())
        .unwrap_or(0);

    SyncStatus {
        subgraph: s["subgraph"].as_str().unwrap_or("").to_string(),
        synced: s["synced"].as_bool().unwrap_or(false),
        health: s["health"].as_str().unwrap_or("unknown").to_string(),
        node: s["node"].as_str().unwrap_or("").to_string(),
        network: chain["network"].as_str().unwrap_or("").to_string(),
        latest_block: latest,
        chain_head_block: head,
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SyncStatus {
    pub subgraph: String,
    pub synced: bool,
    pub health: String,
    pub node: String,
    pub network: String,
    pub latest_block: u64,
    pub chain_head_block: u64,
}

impl SyncStatus {
    pub fn pct_synced(&self) -> f64 {
        if self.chain_head_block == 0 {
            return 0.0;
        }
        (self.latest_block as f64 / self.chain_head_block as f64 * 100.0).min(100.0)
    }

    pub fn blocks_behind(&self) -> u64 {
        self.chain_head_block.saturating_sub(self.latest_block)
    }
}
