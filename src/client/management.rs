use anyhow::Result;
use serde_json::Value;

use crate::executor::RemoteExecutor;
use crate::types::Grt;

pub struct ManagementClient {
    pub exec: RemoteExecutor,
}

impl ManagementClient {
    pub fn new(exec: RemoteExecutor) -> Self {
        Self { exec }
    }

    pub async fn query(&self, gql: &str) -> Result<Value> {
        self.exec.management_graphql(gql).await
    }

    // ── Allocations ──────────────────────────────────────────────────────────

    pub async fn active_allocations(&self) -> Result<Vec<Allocation>> {
        // QUIRK: status is a String filter (quoted "active"), not an enum.
        // QUIRK: subgraphDeployment is a flat string (IPFS hash), not a nested object.
        let data = self.query(r#"{
          allocations(filter: {status: "active"}) {
            id
            subgraphDeployment
            allocatedTokens
            createdAtEpoch
            status
          }
        }"#).await?;

        let allocs = data["allocations"].as_array()
            .cloned()
            .unwrap_or_default();

        Ok(allocs.into_iter()
            .map(|a| Allocation {
                id: a["id"].as_str().unwrap_or("").to_string(),
                // subgraphDeployment is a flat string (IPFS hash), not nested
                ipfs_hash: a["subgraphDeployment"].as_str().unwrap_or("").to_string(),
                allocated_tokens: Grt::from_wei(a["allocatedTokens"].as_str().unwrap_or("0")),
                created_at_epoch: a["createdAtEpoch"].as_i64().unwrap_or(0),
                status: a["status"].as_str().unwrap_or("").to_string(),
            })
            .filter(|a| a.status.eq_ignore_ascii_case("active"))
        .collect())
    }

    // ── Indexing rules ────────────────────────────────────────────────────────

    pub async fn indexing_rules(&self) -> Result<Vec<IndexingRule>> {
        // NOTE: `deployment` field is absent from response even when requested — known quirk.
        // We identify rules by allocationAmount + decisionBasis.
        let data = self.query(r#"{
          indexingRules(merged: false) {
            identifier
            identifierType
            allocationAmount
            decisionBasis
            protocolNetwork
          }
        }"#).await?;

        let rules = data["indexingRules"].as_array()
            .cloned()
            .unwrap_or_default();

        Ok(rules.into_iter().map(|r| IndexingRule {
            identifier: r["identifier"].as_str().unwrap_or("").to_string(),
            identifier_type: r["identifierType"].as_str().unwrap_or("").to_string(),
            allocation_amount: r["allocationAmount"].as_str()
                .map(Grt::from_wei)
                .unwrap_or(Grt::zero()),
            decision_basis: r["decisionBasis"].as_str().unwrap_or("").to_string(),
            protocol_network: r["protocolNetwork"].as_str().unwrap_or("").to_string(),
        }).collect())
    }

    // ── Actions ───────────────────────────────────────────────────────────────

    /// Active actions only (queued + approved + pending) — default for status display.
    pub async fn actions(&self) -> Result<Vec<Action>> {
        // ActionFilter.status is a String (singular) — fetch each status and merge.
        // There is no multi-status filter; fetch all and filter client-side.
        self.actions_with_filter(None).await
            .map(|v| v.into_iter().filter(|a| {
                matches!(a.status.as_str(), "queued" | "approved" | "pending")
            }).collect())
    }

    pub async fn actions_all(&self) -> Result<Vec<Action>> {
        self.actions_with_filter(None).await
    }

    pub async fn actions_by_status(&self, status: &str) -> Result<Vec<Action>> {
        self.actions_with_filter(Some(status)).await
    }

    async fn actions_with_filter(&self, status: Option<&str>) -> Result<Vec<Action>> {
        // QUIRK: ActionFilter.status is String (quoted), not an enum.
        // QUIRK: No multi-status filter — single status or no filter (returns all).
        let filter_arg = match status {
            Some(s) => format!(r#"(filter: {{status: "{}"}})"#, s),
            None => String::new(),
        };
        let gql = format!(r#"{{
          actions{filter} {{
            id
            type
            deploymentID
            amount
            status
            priority
            reason
            source
          }}
        }}"#, filter = filter_arg);

        // QUIRK: management API returns "Cannot convert undefined or null to object"
        // when the actions table is empty or in certain states. Treat as empty list.
        let data = match self.query(&gql).await {
            Ok(d) => d,
            Err(e) if e.to_string().contains("Cannot convert undefined or null") => {
                return Ok(vec![]);
            }
            Err(e) => return Err(e),
        };

        let actions = match data["actions"].as_array() {
            Some(a) => a.clone(),
            None => return Ok(vec![]),
        };

        Ok(actions.into_iter().map(|a| Action {
            id: a["id"].as_i64().unwrap_or(0),
            action_type: a["type"].as_str().unwrap_or("").to_string(),
            deployment_id: a["deploymentID"].as_str().unwrap_or("").to_string(),
            amount: a["amount"].as_str().map(|s| s.parse::<f64>().unwrap_or(0.0)).unwrap_or(0.0),
            status: a["status"].as_str().unwrap_or("").to_string(),
            priority: a["priority"].as_i64().unwrap_or(0),
            reason: a["reason"].as_str().unwrap_or("").to_string(),
        }).collect())
    }

    // ── Write mutations (Phase 2) ─────────────────────────────────────────────

    pub async fn set_indexing_rule(
        &self,
        ipfs_hash: &str,
        basis: &str,
        amount_grt: Option<Grt>,
        protocol_network: &str,
    ) -> Result<()> {
        let amount_wei = amount_grt
            .map(|g| format!("\"{}\"", g.to_wei_str()))
            .unwrap_or_else(|| "null".into());

        let mutation = format!(r#"mutation {{
          setIndexingRule(rule: {{
            identifier: "{hash}"
            identifierType: deployment
            decisionBasis: {basis}
            allocationAmount: {amount}
            protocolNetwork: "{network}"
          }}) {{
            identifier
            decisionBasis
            allocationAmount
          }}
        }}"#,
            hash = ipfs_hash,
            basis = basis,
            amount = amount_wei,
            network = protocol_network,
        );

        self.query(&mutation).await?;
        Ok(())
    }

    /// Queue an allocation action in `approved` state.
    /// QUIRK: amount is GRT (not wei) for queueActions.
    /// QUIRK: status must be `approved` or it will sit indefinitely without executing.
    pub async fn queue_allocate(&self, ipfs_hash: &str, amount_grt: Grt) -> Result<i64> {
        let mutation = format!(r#"mutation {{
          queueActions(actions: [{{
            type: allocate
            deploymentID: "{hash}"
            amount: "{amount}"
            status: approved
            priority: 0
            isLegacy: false
            source: "sik"
            reason: "manual via sik"
          }}]) {{
            id
            status
          }}
        }}"#,
            hash = ipfs_hash,
            amount = amount_grt.0,
        );

        let data = self.query(&mutation).await?;
        let id = data["queueActions"][0]["id"].as_i64().unwrap_or(-1);
        Ok(id)
    }

    /// Present a Proof of Indexing (POI) for a specific allocation.
    /// QUIRK: amount is GRT (not wei) for queueActions.
    /// QUIRK: status must be `approved` or it will sit indefinitely without executing.
    /// If `poi` is None, sends the zero-hash (tells agent to compute automatically).
    pub async fn queue_present_poi(
        &self,
        deployment_id: &str,
        allocation_id: &str,
        poi: Option<&str>,
    ) -> Result<i64> {
        let poi_val = poi.unwrap_or(
            "0x0000000000000000000000000000000000000000000000000000000000000000"
        );
        let mutation = format!(r#"mutation {{
          queueActions(actions: [{{
            type: presentPOI
            deploymentID: "{deployment}"
            allocationID: "{allocation}"
            poi: "{poi}"
            status: approved
            priority: 0
            isLegacy: false
            source: "sik"
            reason: "manual via sik"
          }}]) {{
            id
            status
          }}
        }}"#,
            deployment = deployment_id,
            allocation = allocation_id,
            poi = poi_val,
        );
        let data = self.query(&mutation).await?;
        let id = data["queueActions"][0]["id"].as_i64().unwrap_or(-1);
        Ok(id)
    }

    /// Queue a resize action (queued state — caller must approve when capacity is free).
    /// QUIRK: amount is GRT (not wei) for queueActions.
    pub async fn queue_resize(
        &self,
        deployment_id: &str,
        allocation_id: &str,
        amount_grt: Grt,
        protocol_network: &str,
    ) -> Result<i64> {
        let mutation = format!(r#"mutation {{
          queueActions(actions: [{{
            type: resize
            deploymentID: "{deployment}"
            allocationID: "{allocation}"
            amount: "{amount}"
            status: queued
            priority: 0
            isLegacy: false
            source: "sik"
            reason: "manual resize via sik"
            protocolNetwork: "{network}"
          }}]) {{
            id
            status
          }}
        }}"#,
            deployment = deployment_id,
            allocation = allocation_id,
            amount = amount_grt.0,
            network = protocol_network,
        );
        let data = self.query(&mutation).await?;
        let id = data["queueActions"][0]["id"].as_i64().unwrap_or(-1);
        Ok(id)
    }

    /// Provision additional GRT to SubgraphService (increases allocation capacity).
    pub async fn add_to_provision(&self, amount_grt: Grt, protocol_network: &str) -> Result<()> {
        let mutation = format!(r#"mutation {{
          addToProvision(protocolNetwork: "{network}", amount: "{amount}")
        }}"#,
            network = protocol_network,
            amount = amount_grt.0,
        );
        self.query(&mutation).await?;
        Ok(())
    }

    /// Close an allocation.
    /// QUIRK: blockNumber must be Int (not String).
    /// QUIRK: returns {} on success — caller must verify via separate allocations query.
    pub async fn close_allocation(
        &self,
        allocation_id: &str,
        block_number: u64,
        poi: &str,
        force: bool,
    ) -> Result<()> {
        let mutation = format!(r#"mutation {{
          closeAllocation(
            allocation: "{id}"
            poi: "{poi}"
            blockNumber: {block}
            publicPOI: true
            force: {force}
            protocolNetwork: "eip155:42161"
          )
        }}"#,
            id = allocation_id,
            poi = poi,
            block = block_number,   // Int, not String — critical
            force = force,
        );
        self.query(&mutation).await?;
        Ok(())
    }
}

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct Allocation {
    pub id: String,
    pub ipfs_hash: String,
    pub allocated_tokens: Grt,
    pub created_at_epoch: i64,
    pub status: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexingRule {
    pub identifier: String,
    pub identifier_type: String,
    pub allocation_amount: Grt,
    pub decision_basis: String,
    pub protocol_network: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Action {
    pub id: i64,
    pub action_type: String,
    pub deployment_id: String,
    pub amount: f64,
    pub status: String,
    pub priority: i64,
    pub reason: String,
}
