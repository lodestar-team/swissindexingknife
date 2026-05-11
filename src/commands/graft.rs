use anyhow::Result;
use colored::Colorize;
use std::path::PathBuf;

use crate::client::{GraphNodeClient, IpfsClient};
use crate::config::Config;
use crate::executor::RemoteExecutor;
use crate::output::table::*;
use crate::types::grt::fmt_comma;

#[derive(serde::Serialize, serde::Deserialize)]
struct GraftCache {
    timestamp: i64,
    subgraph: String,
    latest_block: u64,
}

pub async fn run(cfg: &Config, deployment: &str, watch: bool, json: bool) -> Result<()> {
    let exec = RemoteExecutor::from_config(&cfg.server, &cfg.docker, &cfg.api);
    let gn = GraphNodeClient::new(exec);
    let ipfs = IpfsClient::new(cfg.network.ipfs_url.clone());

    if watch {
        loop {
            do_graft_status(cfg, deployment, &gn, &ipfs, json).await?;
            if !watch { break; }
            println!("\n{}", format!("Refreshing in 60s... (Ctrl-C to stop)").dimmed());
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            print!("\x1B[2J\x1B[1;1H"); // clear screen
        }
    } else {
        do_graft_status(cfg, deployment, &gn, &ipfs, json).await?;
    }
    Ok(())
}

async fn do_graft_status(
    cfg: &Config,
    deployment: &str,
    gn: &GraphNodeClient,
    ipfs: &IpfsClient,
    json: bool,
) -> Result<()> {
    // 1. Get graft info from manifest
    let graft = ipfs.graft_info(deployment).await
        .ok()
        .flatten();

    let graft = match graft {
        Some(g) => g,
        None => {
            if json {
                println!("{}", serde_json::json!({ "error": "no graft dependency found", "deployment": deployment }));
            } else {
                println!("{} has no graft dependency (or IPFS unreachable).", deployment);
            }
            return Ok(());
        }
    };

    // 2. Get current sync status of the graft BASE
    let base_status = gn.status_for(&graft.base_hash).await
        .ok()
        .flatten();

    // 3. Load previous poll from cache for rate estimation
    let cache_path = cache_path(deployment);
    let prev_poll = load_cache(&cache_path);

    let now = chrono::Utc::now().timestamp();
    let (current_block, chain_head) = match &base_status {
        Some(s) => (s.latest_block, s.chain_head_block),
        None => (0, 0),
    };

    // Compute rate from previous poll
    let rate_blocks_per_day = if let Some(prev) = &prev_poll {
        let elapsed_secs = (now - prev.timestamp).max(1);
        let block_diff = current_block.saturating_sub(prev.latest_block);
        if elapsed_secs > 60 && block_diff > 0 {
            Some(block_diff as f64 / elapsed_secs as f64 * 86400.0)
        } else {
            None
        }
    } else {
        None
    };

    // Save current state to cache
    save_cache(&cache_path, GraftCache {
        timestamp: now,
        subgraph: graft.base_hash.clone(),
        latest_block: current_block,
    });

    let blocks_remaining = graft.block.saturating_sub(current_block);
    let eta_days = rate_blocks_per_day.map(|r| {
        if r > 0.0 { blocks_remaining as f64 / r } else { f64::INFINITY }
    });

    if json {
        let out = serde_json::json!({
            "target_deployment": deployment,
            "graft_base_hash": graft.base_hash,
            "graft_at_block": graft.block,
            "base_current_block": current_block,
            "base_chain_head": chain_head,
            "blocks_remaining": blocks_remaining,
            "rate_blocks_per_day": rate_blocks_per_day,
            "eta_days": eta_days,
            "base_synced": base_status.as_ref().map(|s| s.synced),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("\n{}", format!("Graft Status: {}", short_hash(deployment)).bold());

    kv("Target deployment:", deployment);
    kv("Graft base hash:", &graft.base_hash);
    kv("Required base block:", &fmt_comma(graft.block as f64));

    rule();

    match &base_status {
        None => {
            kv_err("Graft base:", "NOT DEPLOYED on this node");
            println!("  {} Deploy the graft base first: {}", "→".yellow(),
                format!("sik deploy {}", graft.base_hash).bold());
        }
        Some(s) if s.synced && current_block >= graft.block => {
            kv_ok("Graft base:", "READY — target block reached");
            println!("  {} You can now deploy {} on this node.", "✓".green().bold(), deployment);
        }
        Some(s) => {
            kv("Current block:", &fmt_comma(current_block as f64));
            kv("Target block:", &fmt_comma(graft.block as f64));
            kv("Blocks remaining:", &fmt_comma(blocks_remaining as f64));

            match rate_blocks_per_day {
                None => {
                    kv("Rate:", "first poll — no history yet (run again in a minute)");
                }
                Some(r) if r < 100.0 => {
                    kv_err("Rate:", &format!("{:.0} blocks/day (VERY SLOW — resource contention?)", r));
                    println!("  {} Check for other Base subgraphs syncing simultaneously.", "⚠".yellow());
                }
                Some(r) => {
                    kv("Rate:", &format!("{} blocks/day", fmt_comma(r)));
                }
            }

            if let Some(days) = eta_days {
                if days.is_infinite() || days > 1000.0 {
                    kv_err("ETA:", "unknown — rate too slow");
                } else if days < 1.0 {
                    kv_ok("ETA:", &format!("{:.0} hours", days * 24.0));
                } else {
                    let colour = if days > 30.0 { "warn" } else { "ok" };
                    let eta_str = format!("{:.0} days", days);
                    if colour == "warn" {
                        kv_warn("ETA:", &eta_str);
                    } else {
                        kv("ETA:", &eta_str);
                    }
                }
            }

            kv("Health:", &s.health);
            kv("Network:", &s.network);

            if prev_poll.is_none() {
                println!("\n  {}", "First poll recorded. Rate will be available on next run.".dimmed());
            }
        }
    }

    Ok(())
}

fn cache_path(deployment: &str) -> PathBuf {
    let key = &deployment[..deployment.len().min(20)];
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".lodestar")
        .join("cache")
        .join(format!("graft_{}.json", key))
}

fn load_cache(path: &PathBuf) -> Option<GraftCache> {
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

fn save_cache(path: &PathBuf, data: GraftCache) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(s) = serde_json::to_string_pretty(&data) {
        let _ = std::fs::write(path, s);
    }
}
