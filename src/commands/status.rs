use anyhow::Result;
use colored::Colorize;
use comfy_table::{Table, presets::UTF8_BORDERS_ONLY};

use crate::client::{GraphNodeClient, ManagementClient, NetworkClient};
use crate::config::Config;
use crate::output::table::*;
use crate::types::grt::{fmt_comma, fmt_usd};
use crate::types::Grt;

pub async fn run(cfg: &Config, json: bool) -> Result<()> {
    let exec = crate::executor::RemoteExecutor::from_config(
        &cfg.server, &cfg.docker, &cfg.api,
    );
    let mgmt = ManagementClient::new(exec.clone());
    let gn = GraphNodeClient::new(exec);
    let net = NetworkClient::new(
        cfg.network.subgraph_url.clone(),
        cfg.indexer.address.clone(),
    );

    // Fire everything in parallel
    let (allocs_res, rules_res, actions_res, statuses_res, indexer_res, thaws_res) = tokio::join!(
        mgmt.active_allocations(),
        mgmt.indexing_rules(),
        mgmt.actions(),
        gn.all_statuses(),
        net.indexer_details(),
        net.thaw_requests(),
    );

    let allocs = allocs_res.unwrap_or_default();
    let rules = rules_res.unwrap_or_default();
    let actions = actions_res.unwrap_or_default();
    let statuses = statuses_res.unwrap_or_default();
    let thaws = thaws_res.unwrap_or_default();

    // GRT price
    let grt_price = cfg.grt_price.manual_price_usd
        .unwrap_or_else(|| {
            // Try to fetch; use last known if unavailable
            0.02646
        });

    if json {
        let out = serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "indexer": indexer_res.as_ref().ok().map(|i| serde_json::json!({
                "address": cfg.indexer.address,
                "own_stake_grt": i.staked.0,
                "delegated_grt": i.delegated.0,
                "total_capacity_grt": i.capacity.0,
                "allocated_grt": i.allocated.0,
                "available_grt": i.available.0,
            })),
            "allocations": allocs,
            "rules": rules,
            "pending_actions": actions,
            "thaw_requests": thaws,
            "sync_statuses": statuses,
            "grt_price_usd": grt_price,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    // ── Human output ──────────────────────────────────────────────────────────

    println!("\n{}", format!(
        "Lodestar Status — {}",
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")
    ).bold());
    println!("{}", format!("Indexer: {}", cfg.indexer.address).dimmed());

    // Stake
    section("STAKE");
    match &indexer_res {
        Ok(i) => {
            kv("Own stake:", &format!("{} GRT", fmt_comma(i.staked.0)));
            kv("Delegated:", &format!("{} GRT", fmt_comma(i.delegated.0)));
            kv("Total capacity:", &format!("{} GRT", fmt_comma(i.capacity.0)));
            kv("Allocated:", &format!("{} GRT", fmt_comma(i.allocated.0)));
            let free_pct = if i.capacity.0 > 0.0 { i.available.0 / i.capacity.0 * 100.0 } else { 0.0 };
            if i.available.0 < 1000.0 {
                kv_warn("Free:", &format!("{} GRT ({:.1}%)", fmt_comma(i.available.0), free_pct));
            } else {
                kv("Free:", &format!("{} GRT ({:.1}%)", fmt_comma(i.available.0), free_pct));
            }
        }
        Err(e) => kv_err("Error fetching stake:", &e.to_string()),
    }

    // Thaw requests
    if !thaws.is_empty() {
        section("THAW REQUESTS");
        for t in &thaws {
            let status = fmt_thaw(t.mature, t.hours_remaining());
            kv(
                &format!("{}:", t.data_service.split('/').last().unwrap_or(&t.data_service)),
                &format!("{} GRT — {}", fmt_comma(t.shares.0), status),
            );
            if t.mature {
                println!("  {} Run: sik thaw withdraw", "→".green());
            }
        }
    }

    // Pending actions
    if !actions.is_empty() {
        section("PENDING ACTIONS");
        let mut table = Table::new();
        table.load_preset(UTF8_BORDERS_ONLY);
        table.set_header(["ID", "Type", "Deployment", "Amount GRT", "Status"]);
        for a in &actions {
            table.add_row([
                &a.id.to_string(),
                &a.action_type,
                &short_hash(&a.deployment_id),
                &fmt_comma(a.amount),
                &a.status,
            ]);
        }
        println!("{table}");
    }

    // Allocations
    section("ALLOCATIONS");
    if allocs.is_empty() {
        println!("  (none)");
    } else {
        let mut table = Table::new();
        table.load_preset(UTF8_BORDERS_ONLY);
        table.set_header(["Deployment", "Alloc GRT", "Sync", "Health"]);

        for a in &allocs {
            let sync_st = statuses.iter().find(|s| s.subgraph == a.ipfs_hash);
            let sync_str = match sync_st {
                Some(s) if s.synced => "synced".green().to_string(),
                Some(s) => format!("{:.1}%", s.pct_synced()).yellow().to_string(),
                None => "unknown".dimmed().to_string(),
            };
            let health = sync_st.map(|s| s.health.as_str()).unwrap_or("?");
            table.add_row([
                &short_hash(&a.ipfs_hash),
                &fmt_comma(a.allocated_tokens.0),
                &sync_str,
                health,
            ]);
        }
        println!("{table}");

        // Total estimated rewards
        let total_alloc: f64 = allocs.iter().map(|a| a.allocated_tokens.0).sum();
        println!("  Total allocated: {} GRT", fmt_comma(total_alloc));
    }

    // Deployments in graph-node not in active allocations (zombies)
    let alloc_hashes: std::collections::HashSet<&str> =
        allocs.iter().map(|a| a.ipfs_hash.as_str()).collect();
    let zombies: Vec<_> = statuses.iter()
        .filter(|s| !alloc_hashes.contains(s.subgraph.as_str()))
        .collect();
    if !zombies.is_empty() {
        section("ZOMBIE DEPLOYMENTS (syncing with no allocation)");
        for z in &zombies {
            println!("  {} {} {} — {}",
                "⚠".yellow(),
                short_hash(&z.subgraph),
                z.network,
                fmt_sync(z.synced, z.pct_synced()),
            );
        }
        println!("  → Run: sik rule set <hash> never  to stop syncing");
    }

    rule();
    println!("  GRT price: {}   Network: {}", fmt_usd(grt_price), cfg.network.protocol_network);

    Ok(())
}
