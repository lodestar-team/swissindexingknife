use anyhow::Result;
use colored::Colorize;

use crate::client::{GraphNodeClient, IpfsClient, NetworkClient};
use crate::config::Config;
use crate::executor::RemoteExecutor;
use crate::output::table::*;
use crate::types::grt::fmt_comma;

pub async fn run(cfg: &Config, deployment: &str, json: bool) -> Result<()> {
    let exec = RemoteExecutor::from_config(&cfg.server, &cfg.docker, &cfg.api);
    let gn = GraphNodeClient::new(exec);
    let net = NetworkClient::new(cfg.network.subgraph_url.clone(), cfg.indexer.address.clone());
    let ipfs = IpfsClient::new(cfg.network.ipfs_url.clone());

    eprintln!("{}", "Checking deployment...".dimmed());

    let (net_info, sync_status, graft_info, ipfs_ok) = tokio::join!(
        net.deployment(deployment),
        gn.status_for(deployment),
        ipfs.graft_info(deployment),
        ipfs.is_available(deployment),
    );

    let net_info = net_info?;
    let sync_status = sync_status.unwrap_or(None);
    let graft_info = graft_info.ok().flatten();
    let net_stats = net.network_stats().await.ok();

    // Check graft base status if needed
    let graft_base_status = if let Some(ref g) = graft_info {
        gn.status_for(&g.base_hash).await.ok().flatten()
    } else {
        None
    };

    if json {
        let out = serde_json::json!({
            "deployment": deployment,
            "on_chain": net_info.is_some(),
            "denied_at": net_info.as_ref().map(|d| d.denied_at),
            "signal_grt": net_info.as_ref().map(|d| d.signal.0),
            "staked_grt": net_info.as_ref().map(|d| d.staked.0),
            "ratio": net_info.as_ref().map(|d| d.ratio),
            "ipfs_available": ipfs_ok,
            "graft": graft_info,
            "graft_base_status": graft_base_status,
            "local_sync": sync_status,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("\n{}", format!("Deployment Verify: {}", deployment).bold());

    // On-chain
    match &net_info {
        None => kv_err("On-chain:", "NOT FOUND in network subgraph"),
        Some(d) => {
            kv_ok("On-chain:", "found");
            if d.denied_at > 0 {
                kv_err("deniedAt:", &format!("{} — REWARDS PERMANENTLY DENIED", d.denied_at));
                println!("  {} Do not allocate to this deployment.", "STOP".red().bold());
            } else {
                kv_ok("deniedAt:", "0 (eligible)");
            }
            kv("Signal:", &format!("{} GRT", fmt_comma(d.signal.0)));
            kv("Total staked:", &format!("{} GRT", fmt_comma(d.staked.0)));
            kv("Signal/stake ratio:", &fmt_ratio(d.ratio));
            if let Some(ns) = &net_stats {
                let est100k = d.est_monthly_grt(crate::types::Grt(100_000.0), ns);
                kv("Est GRT/mo at 100K:", &format!("{} GRT", fmt_comma(est100k.0)));
            }
        }
    }

    rule();

    // IPFS
    if ipfs_ok {
        kv_ok("IPFS manifest:", "accessible");
    } else {
        kv_err("IPFS manifest:", "NOT ACCESSIBLE — zombie subgraph?");
    }

    // Graft
    match &graft_info {
        None => kv("Graft dependency:", "none"),
        Some(g) => {
            kv_warn("Graft dependency:", &format!("YES — base {} required", short_hash(&g.base_hash)));
            kv("Graft base hash:", &g.base_hash);
            kv("Graft at block:", &g.block.to_string());
            match &graft_base_status {
                None => kv_err("Graft base local:", "NOT deployed on this node"),
                Some(s) => {
                    if s.synced {
                        kv_ok("Graft base local:", &format!("synced ({} {})", s.subgraph, s.network));
                    } else {
                        kv_warn("Graft base local:", &format!(
                            "syncing — block {} / {} ({:.1}%)",
                            fmt_comma(s.latest_block as f64),
                            fmt_comma(s.chain_head_block as f64),
                            s.pct_synced(),
                        ));
                        println!("  {} Run: {} for detailed ETA",
                            "→".yellow(),
                            format!("sik graft-status {}", deployment).bold(),
                        );
                    }
                }
            }
        }
    }

    rule();

    // Local sync
    match &sync_status {
        None => kv("Local deployment:", "not deployed on this node"),
        Some(s) => {
            if s.synced {
                kv_ok("Local deployment:", "synced");
            } else {
                kv_warn("Local deployment:", &format!(
                    "syncing — {:.1}% ({} blocks behind)",
                    s.pct_synced(),
                    fmt_comma(s.blocks_behind() as f64),
                ));
            }
        }
    }

    // Recommendation
    rule();
    let can_allocate = net_info.as_ref().map(|d| d.denied_at == 0).unwrap_or(false)
        && ipfs_ok
        && graft_base_status.as_ref().map(|s| s.synced).unwrap_or(graft_info.is_none())
        && sync_status.as_ref().map(|s| s.synced).unwrap_or(false);

    if can_allocate {
        println!("  {} Ready to allocate. Run: {}",
            "✓".green().bold(),
            format!("sik allocate {} <GRT>", deployment).bold(),
        );
    } else if net_info.as_ref().map(|d| d.denied_at > 0).unwrap_or(false) {
        println!("  {} DO NOT allocate — deniedAt > 0.", "✗".red().bold());
    } else {
        println!("  {} Not ready — resolve issues above first.", "~".yellow().bold());
        if graft_info.is_some() && graft_base_status.as_ref().map(|s| !s.synced).unwrap_or(true) {
            println!("  {} Graft base must finish syncing before you can deploy.", "→".yellow());
        }
    }

    Ok(())
}
