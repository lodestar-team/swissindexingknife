use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::{json, Value};

use crate::types::Grt;

pub struct NetworkClient {
    http: Client,
    subgraph_url: String,
    indexer_address: String,
}

impl NetworkClient {
    pub fn new(subgraph_url: String, indexer_address: String) -> Self {
        Self {
            http: Client::new(),
            subgraph_url,
            indexer_address,
        }
    }

    async fn query(&self, gql: &str, variables: Option<Value>) -> Result<Value> {
        let body = if let Some(vars) = variables {
            json!({ "query": gql, "variables": vars })
        } else {
            json!({ "query": gql })
        };

        let resp = self.http
            .post(&self.subgraph_url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("network subgraph request failed")?;

        let value: Value = resp.json().await.context("failed to parse network subgraph response")?;

        if let Some(errors) = value.get("errors") {
            if !errors.is_null() {
                if let Some(arr) = errors.as_array() {
                    if !arr.is_empty() {
                        let msg = arr.iter()
                            .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                            .collect::<Vec<_>>()
                            .join("; ");
                        anyhow::bail!("network subgraph error: {}", msg);
                    }
                }
            }
        }

        Ok(value["data"].clone())
    }

    // ── Indexer ───────────────────────────────────────────────────────────────

    pub async fn indexer_details(&self) -> Result<IndexerDetails> {
        let data = self.query(r#"query($id: ID!) {
          indexer(id: $id) {
            stakedTokens
            delegatedTokens
            allocatedTokens
            tokenCapacity
            availableStake
          }
        }"#, Some(json!({ "id": self.indexer_address.to_lowercase() }))).await?;

        let i = &data["indexer"];
        Ok(IndexerDetails {
            staked: Grt::from_wei(i["stakedTokens"].as_str().unwrap_or("0")),
            delegated: Grt::from_wei(i["delegatedTokens"].as_str().unwrap_or("0")),
            allocated: Grt::from_wei(i["allocatedTokens"].as_str().unwrap_or("0")),
            capacity: Grt::from_wei(i["tokenCapacity"].as_str().unwrap_or("0")),
            available: Grt::from_wei(i["availableStake"].as_str().unwrap_or("0")),
        })
    }

    // ── Thaw requests ─────────────────────────────────────────────────────────

    pub async fn thaw_requests(&self) -> Result<Vec<ThawRequest>> {
        let data = self.query(r#"query($indexer: String!) {
          thawRequests(where: {indexer: $indexer, fulfilled: false}) {
            thawingUntil
            shares
            dataService { id }
          }
        }"#, Some(json!({ "indexer": self.indexer_address.to_lowercase() }))).await?;

        let reqs = data["thawRequests"].as_array()
            .cloned()
            .unwrap_or_default();

        Ok(reqs.into_iter().map(|r| {
            // thawingUntil is a BigInt in the subgraph — returned as string
            let thawing_until = r["thawingUntil"]
                .as_str().and_then(|s| s.parse::<i64>().ok())
                .or_else(|| r["thawingUntil"].as_i64())
                .unwrap_or(0);
            let now = chrono::Utc::now().timestamp();
            ThawRequest {
                data_service: r["dataService"]["id"].as_str().unwrap_or("").to_string(),
                shares: r["shares"].as_str().map(Grt::from_wei).unwrap_or(Grt::zero()),
                thawing_until_unix: thawing_until,
                mature: now >= thawing_until,
            }
        }).collect())
    }

    // ── Deployments ───────────────────────────────────────────────────────────

    /// Single deployment details by IPFS hash.
    pub async fn deployment(&self, ipfs_hash: &str) -> Result<Option<DeploymentInfo>> {
        let data = self.query(r#"query($hash: String!) {
          subgraphDeployments(where: { ipfsHash: $hash }, first: 1) {
            id
            ipfsHash
            signalledTokens
            stakedTokens
            deniedAt
          }
        }"#, Some(json!({ "hash": ipfs_hash }))).await?;

        let mut arr = data["subgraphDeployments"].as_array()
            .cloned()
            .unwrap_or_default();

        Ok(arr.pop().map(|d| parse_deployment(&d)))
    }

    /// All deployments, paginated, ordered by signalledTokens descending.
    pub async fn all_deployments(&self, limit: usize) -> Result<Vec<DeploymentInfo>> {
        let mut results = Vec::new();
        let page_size = 1000;
        let mut skip = 0usize;

        loop {
            let data = self.query(r#"query($first: Int!, $skip: Int!) {
              subgraphDeployments(
                first: $first
                skip: $skip
                orderBy: signalledTokens
                orderDirection: desc
                where: { signalledTokens_gt: "0" }
              ) {
                id
                ipfsHash
                signalledTokens
                stakedTokens
                deniedAt
              }
            }"#, Some(json!({ "first": page_size, "skip": skip }))).await?;

            let batch = data["subgraphDeployments"].as_array()
                .cloned()
                .unwrap_or_default();

            let batch_len = batch.len();
            results.extend(batch.iter().map(|d| parse_deployment(d)));

            if batch_len < page_size || results.len() >= limit {
                break;
            }
            skip += page_size;
        }

        results.truncate(limit);
        Ok(results)
    }

    /// Network-level stats (issuance, total signal).
    pub async fn network_stats(&self) -> Result<NetworkStats> {
        let data = self.query(r#"{
          graphNetwork(id: "1") {
            totalTokensSignalled
          }
        }"#, None).await?;

        let n = &data["graphNetwork"];
        // issuancePerBlock in GRT wei/block; Arbitrum ~2 sec blocks, ~15M blocks/year
        // We use a hardcoded monthly figure as fallback since it's stable
        let total_signal = Grt::from_wei(n["totalTokensSignalled"].as_str().unwrap_or("0"));

        Ok(NetworkStats {
            total_signal,
            monthly_issuance: Grt(26_100_000.0), // ~26.1M GRT/month (May 2026)
        })
    }

    // ── GRT price ─────────────────────────────────────────────────────────────

    pub async fn grt_price_usd(&self) -> Result<f64> {
        let resp = self.http
            .get("https://api.coingecko.com/api/v3/simple/price?ids=the-graph&vs_currencies=usd")
            .send()
            .await
            .context("CoinGecko request failed")?;
        let data: Value = resp.json().await?;
        data["the-graph"]["usd"].as_f64()
            .context("CoinGecko response missing price")
    }

    // ── Closed allocations for P&L ────────────────────────────────────────────

    pub async fn closed_allocations_this_month(&self) -> Result<Vec<ClosedAllocation>> {
        // Find allocations closed in the last 30 days
        let thirty_days_ago = chrono::Utc::now().timestamp() - 30 * 24 * 3600;

        let data = self.query(r#"query($indexer: String!, $since: Int!) {
          allocations(
            where: {
              indexer: $indexer
              status: Closed
              closedAt_gt: $since
            }
            first: 100
            orderBy: closedAt
            orderDirection: desc
          ) {
            id
            subgraphDeployment { ipfsHash }
            allocatedTokens
            createdAt
            closedAt
            indexingRewards
          }
        }"#, Some(json!({
            "indexer": self.indexer_address.to_lowercase(),
            "since": thirty_days_ago,
        }))).await?;

        let allocs = data["allocations"].as_array()
            .cloned()
            .unwrap_or_default();

        Ok(allocs.into_iter().map(|a| ClosedAllocation {
            id: a["id"].as_str().unwrap_or("").to_string(),
            ipfs_hash: a["subgraphDeployment"]["ipfsHash"].as_str().unwrap_or("").to_string(),
            allocated_tokens: Grt::from_wei(a["allocatedTokens"].as_str().unwrap_or("0")),
            closed_at: a["closedAt"].as_str().and_then(|s| s.parse().ok())
                .or_else(|| a["closedAt"].as_i64()).unwrap_or(0),
            indexing_rewards: Grt::from_wei(a["indexingRewards"].as_str().unwrap_or("0")),
        }).collect())
    }
}

fn parse_deployment(d: &Value) -> DeploymentInfo {
    let signal = Grt::from_wei(d["signalledTokens"].as_str().unwrap_or("0"));
    let staked = Grt::from_wei(d["stakedTokens"].as_str().unwrap_or("0"));
    let ratio = if staked.0 > 0.0 { signal.0 / staked.0 } else { f64::INFINITY };
    DeploymentInfo {
        id: d["id"].as_str().unwrap_or("").to_string(),
        ipfs_hash: d["ipfsHash"].as_str().unwrap_or("").to_string(),
        signal,
        staked,
        denied_at: d["deniedAt"].as_i64().unwrap_or(0),
        ratio,
    }
}

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexerDetails {
    pub staked: Grt,
    pub delegated: Grt,
    pub allocated: Grt,
    pub capacity: Grt,
    pub available: Grt,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ThawRequest {
    pub data_service: String,
    pub shares: Grt,
    pub thawing_until_unix: i64,
    pub mature: bool,
}

impl ThawRequest {
    pub fn thawing_until_iso(&self) -> String {
        use chrono::TimeZone;
        chrono::Utc.timestamp_opt(self.thawing_until_unix, 0)
            .single()
            .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
            .unwrap_or_else(|| "unknown".into())
    }

    pub fn hours_remaining(&self) -> f64 {
        let now = chrono::Utc::now().timestamp();
        let diff = self.thawing_until_unix - now;
        (diff as f64 / 3600.0).max(0.0)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DeploymentInfo {
    pub id: String,
    pub ipfs_hash: String,
    pub signal: Grt,
    pub staked: Grt,
    pub denied_at: i64,
    pub ratio: f64,
}

impl DeploymentInfo {
    pub fn is_eligible(&self) -> bool {
        self.denied_at == 0
    }

    /// Estimate GRT/month if we allocate `our_alloc` GRT.
    pub fn est_monthly_grt(&self, our_alloc: Grt, network: &NetworkStats) -> Grt {
        if network.total_signal.0 == 0.0 || self.signal.0 == 0.0 {
            return Grt::zero();
        }
        let total_staked = self.staked.0 + our_alloc.0;
        let our_share = our_alloc.0 / total_staked;
        let pool_share = self.signal.0 / network.total_signal.0;
        Grt(network.monthly_issuance.0 * pool_share * our_share)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct NetworkStats {
    pub total_signal: Grt,
    pub monthly_issuance: Grt,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClosedAllocation {
    pub id: String,
    pub ipfs_hash: String,
    pub allocated_tokens: Grt,
    pub closed_at: i64,
    pub indexing_rewards: Grt,
}
