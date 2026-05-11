/// `sik context` — single-call situational awareness dump for AI agents.
/// Aggregates all state and appends a `recommendations` array.
use anyhow::Result;

use crate::client::{GraphNodeClient, ManagementClient, NetworkClient};
use crate::config::Config;
use crate::executor::RemoteExecutor;

pub async fn run(cfg: &Config) -> Result<()> {
    let exec = RemoteExecutor::from_config(&cfg.server, &cfg.docker, &cfg.api);
    let mgmt = ManagementClient::new(exec.clone());
    let gn = GraphNodeClient::new(exec);
    let net = NetworkClient::new(cfg.network.subgraph_url.clone(), cfg.indexer.address.clone());

    let (allocs_res, rules_res, actions_res, statuses_res, indexer_res, thaws_res) = tokio::join!(
        mgmt.active_allocations(),
        mgmt.indexing_rules(),
        mgmt.actions(),
        gn.all_statuses(),
        net.indexer_details(),
        net.thaw_requests(),
    );

    let grt_price = cfg.grt_price.manual_price_usd
        .unwrap_or_else(|| 0.02646);

    let allocs = allocs_res.unwrap_or_default();
    let rules = rules_res.unwrap_or_default();
    let actions = actions_res.unwrap_or_default();
    let statuses = statuses_res.unwrap_or_default();
    let thaws = thaws_res.unwrap_or_default();

    // Build recommendations
    let mut recs = Vec::new();

    // Mature thaws
    for t in thaws.iter().filter(|t| t.mature) {
        recs.push(serde_json::json!({
            "priority": "high",
            "action": "withdraw_thaw",
            "detail": format!("{} GRT thaw matured ({}). Run: sik thaw withdraw",
                t.shares.0, t.thawing_until_iso()),
            "command": "sik thaw withdraw --yes",
        }));
    }

    // Upcoming thaws (< 24h)
    for t in thaws.iter().filter(|t| !t.mature && t.hours_remaining() < 24.0) {
        recs.push(serde_json::json!({
            "priority": "medium",
            "action": "thaw_soon",
            "detail": format!("{} GRT thaw matures in {:.1}h ({})",
                t.shares.0, t.hours_remaining(), t.thawing_until_iso()),
            "command": null,
        }));
    }

    // Queued (not approved) actions
    let queued_actions: Vec<_> = actions.iter().filter(|a| a.status == "queued").collect();
    for a in &queued_actions {
        recs.push(serde_json::json!({
            "priority": "medium",
            "action": "approve_queued_action",
            "detail": format!("Action #{} ({} {}) is queued and will NOT execute until approved",
                a.id, a.action_type, a.deployment_id),
            "command": format!("sik actions approve {}", a.id),
        }));
    }

    // Zombie deployments (syncing with no allocation)
    let alloc_hashes: std::collections::HashSet<&str> =
        allocs.iter().map(|a| a.ipfs_hash.as_str()).collect();
    for s in statuses.iter().filter(|s| !alloc_hashes.contains(s.subgraph.as_str())) {
        recs.push(serde_json::json!({
            "priority": "low",
            "action": "zombie_deployment",
            "detail": format!("Deployment {} is syncing with no allocation (wasting RPC + CPU)",
                &s.subgraph[..s.subgraph.len().min(20)]),
            "command": format!("sik rule set {} never", s.subgraph),
        }));
    }

    // Low free stake
    if let Ok(ref i) = indexer_res {
        let free_pct = if i.capacity.0 > 0.0 { i.available.0 / i.capacity.0 * 100.0 } else { 0.0 };
        if free_pct < 5.0 && i.available.0 < 5000.0 {
            recs.push(serde_json::json!({
                "priority": "medium",
                "action": "low_free_stake",
                "detail": format!("Only {} GRT free ({:.1}% of capacity). Consider closing low-ratio allocations.",
                    i.available.0, free_pct),
                "command": "sik allocations",
            }));
        }
    }

    let out = serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "grt_price_usd": grt_price,
        "network": cfg.network.protocol_network,
        "indexer": {
            "address": cfg.indexer.address,
            "operator": cfg.indexer.operator_address,
            "stake": indexer_res.as_ref().ok().map(|i| serde_json::json!({
                "own_grt": i.staked.0,
                "delegated_grt": i.delegated.0,
                "capacity_grt": i.capacity.0,
                "allocated_grt": i.allocated.0,
                "available_grt": i.available.0,
            })),
        },
        "active_allocations": allocs,
        "indexing_rules": rules,
        "pending_actions": actions,
        "sync_statuses": statuses,
        "thaw_requests": thaws,
        "economics": {
            "monthly_costs_usd": cfg.economics.monthly_costs_usd,
            "delegation_cut_pct": cfg.economics.delegation_cut_bps as f64 / 100.0,
        },
        "recommendations": recs,
        "hints": {
            "management_api": "POST http://localhost:8000/ (NOT /graphql) via docker exec indexer-agent curl",
            "actions_filter": "use `statuses: [...]` (plural array), not `status:`",
            "grt_units": "setIndexingRule uses wei; queueActions uses GRT",
            "close_allocation": "blockNumber must be Int not String; returns {} on success — verify separately",
            "deprovision": "requires COLD INDEXER WALLET, not operator wallet",
            "agent_auto_mode": "auto creates allocations for `always` rules and closes `never` — but queued actions need approved status to execute",
        },
    });

    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
