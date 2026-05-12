use anyhow::Result;
use colored::Colorize;
use comfy_table::{Table, presets::UTF8_BORDERS_ONLY};

use crate::client::{IpfsClient, NetworkClient};
use crate::config::Config;
use crate::output::table::*;
use crate::types::grt::fmt_comma;
use crate::types::Grt;

pub async fn run(
    cfg: &Config,
    chain: Option<String>,
    top: usize,
    min_ratio: f64,
    proposed_alloc: f64,
    json: bool,
) -> Result<()> {
    let net = NetworkClient::new(cfg.network.subgraph_url.clone(), cfg.indexer.address.clone());
    let ipfs = IpfsClient::new(cfg.network.ipfs_url.clone());

    eprintln!("{}", "Fetching deployments from network subgraph...".dimmed());
    let net_stats = net.network_stats().await?;

    // Fetch all deployments (up to 5000)
    let mut deployments = net.all_deployments(5000).await?;

    // Filter: eligible (deniedAt == 0), has signal, meets ratio threshold
    deployments.retain(|d| d.is_eligible() && d.signal.0 > 0.0 && d.ratio >= min_ratio);

    // Sort by ratio descending
    deployments.sort_by(|a, b| b.ratio.partial_cmp(&a.ratio).unwrap_or(std::cmp::Ordering::Equal));

    // Take the top N candidates and check IPFS + graft for them
    let candidates: Vec<_> = deployments.into_iter().take(top * 3).collect();

    eprintln!("{}", format!("Checking top {} candidates for IPFS availability and graft deps...",
        candidates.len().min(top * 2)).dimmed());

    let alloc_grt = Grt(proposed_alloc);

    let mut rows = Vec::new();
    for d in candidates.iter().take(top * 2) {
        let ipfs_hash = d.ipfs_hash.clone();

        let info = ipfs.manifest_info(&ipfs_hash).await;

        // Chain filter: exact match against manifest's indexed network
        if let Some(ref c) = chain {
            let matches = info.network.as_deref()
                .map(|n| n.eq_ignore_ascii_case(c))
                .unwrap_or(false);
            if !matches {
                continue;
            }
        }

        let graft = info.graft;
        let est = d.est_monthly_grt(alloc_grt, &net_stats);
        rows.push(DiscoverRow {
            ipfs_hash: ipfs_hash.clone(),
            signal: d.signal,
            staked: d.staked,
            ratio: d.ratio,
            denied_at: d.denied_at,
            ipfs_ok: info.accessible,
            network: info.network,
            has_graft: graft.is_some(),
            graft_base: graft.map(|g| g.base_hash),
            est_monthly_at_proposed: est,
        });

        if rows.len() >= top {
            break;
        }
    }

    rows.sort_by(|a, b| b.ratio.partial_cmp(&a.ratio).unwrap_or(std::cmp::Ordering::Equal));

    if json {
        let out: Vec<_> = rows.iter().map(|r| serde_json::json!({
            "ipfs_hash": r.ipfs_hash,
            "network": r.network,
            "signal_grt": r.signal.0,
            "staked_grt": r.staked.0,
            "ratio": r.ratio,
            "denied_at": r.denied_at,
            "ipfs_available": r.ipfs_ok,
            "has_graft": r.has_graft,
            "graft_base_hash": r.graft_base,
            "est_grt_month_at_proposed": r.est_monthly_at_proposed.0,
        })).collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("\n{}", "Allocation Opportunities".bold());
    println!("{}", format!(
        "Network: {}  Proposed alloc: {} GRT  Signal: {} GRT  Issuance: {} GRT/mo",
        cfg.network.protocol_network,
        fmt_comma(proposed_alloc),
        fmt_comma(net_stats.total_signal.0),
        fmt_comma(net_stats.monthly_issuance.0),
    ).dimmed());
    println!("{}", "Filters: deniedAt=0, signalledTokens>0, IPFS accessible".dimmed());

    let mut table = Table::new();
    table.load_preset(UTF8_BORDERS_ONLY);
    table.set_header(["Rank", "Deployment", "Network", "Signal GRT", "Staked GRT", "Ratio", "IPFS", "Graft", &format!("Est GRT/mo @ {}K", proposed_alloc / 1000.0)]);

    for (i, r) in rows.iter().enumerate() {
        let ipfs_str = if r.ipfs_ok { "yes".green().to_string() } else { "NO".red().to_string() };
        let graft_str = if r.has_graft {
            "yes*".yellow().to_string()
        } else {
            "no".normal().to_string()
        };
        let network_str = r.network.clone().unwrap_or_else(|| "?".into());
        table.add_row([
            &(i + 1).to_string(),
            &short_hash(&r.ipfs_hash),
            &network_str,
            &fmt_comma(r.signal.0),
            &fmt_comma(r.staked.0),
            &fmt_ratio(r.ratio),
            &ipfs_str,
            &graft_str,
            &fmt_comma(r.est_monthly_at_proposed.0),
        ]);
    }
    println!("{table}");

    let graft_count = rows.iter().filter(|r| r.has_graft).count();
    if graft_count > 0 {
        println!("\n  {} {} deployment(s) have graft dependencies.",
            "*".yellow(), graft_count);
        println!("    Run {} to see details.", "sik graft-status <hash>".bold());
    }

    println!("\n  Run {} to pre-flight check before allocating.",
        "sik verify <hash>".bold());

    Ok(())
}

struct DiscoverRow {
    ipfs_hash: String,
    signal: Grt,
    staked: Grt,
    ratio: f64,
    denied_at: i64,
    ipfs_ok: bool,
    network: Option<String>,
    has_graft: bool,
    graft_base: Option<String>,
    est_monthly_at_proposed: Grt,
}
