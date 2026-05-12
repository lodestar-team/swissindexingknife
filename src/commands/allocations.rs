use anyhow::Result;
use comfy_table::{Table, presets::UTF8_BORDERS_ONLY};

use crate::client::{GraphNodeClient, ManagementClient, NetworkClient};
use crate::config::Config;
use crate::executor::RemoteExecutor;
use crate::output::table::*;
use crate::types::grt::fmt_comma;

pub async fn run(cfg: &Config, json: bool) -> Result<()> {
    let exec = RemoteExecutor::from_config(&cfg.server, &cfg.docker, &cfg.api);
    let mgmt = ManagementClient::new(exec.clone());
    let gn = GraphNodeClient::new(exec);
    let net = NetworkClient::new(cfg.network.subgraph_url.clone(), cfg.indexer.address.clone());

    let (allocs, statuses, net_stats) = tokio::join!(
        mgmt.active_allocations(),
        gn.all_statuses(),
        net.network_stats(),
    );

    let allocs = allocs?;
    let statuses = statuses.unwrap_or_default();
    let net_stats = net_stats?;

    let grt_price = cfg.grt_price.manual_price_usd.unwrap_or(0.02646);

    // Enrich with network subgraph data for each allocation
    let mut rows: Vec<AllocRow> = Vec::new();
    for a in &allocs {
        let deployment = net.deployment(&a.ipfs_hash).await.ok().flatten();
        let sync = statuses.iter().find(|s| s.subgraph == a.ipfs_hash);

        let (signal, staked, ratio, est_monthly) = match &deployment {
            Some(d) => {
                let est = d.est_monthly_grt(a.allocated_tokens, &net_stats);
                let our_share = if d.staked.0 > 0.0 {
                    a.allocated_tokens.0 / d.staked.0 * 100.0
                } else {
                    100.0
                };
                (Some(d.signal), Some(d.staked), d.ratio, Some((est, our_share)))
            }
            None => (None, None, 0.0, None),
        };

        rows.push(AllocRow {
            alloc_id: a.id.clone(),
            ipfs_hash: a.ipfs_hash.clone(),
            allocated: a.allocated_tokens,
            signal,
            staked,
            ratio,
            est_monthly,
            synced: sync.map(|s| s.synced).unwrap_or(false),
            sync_pct: sync.map(|s| s.pct_synced()).unwrap_or(0.0),
        });
    }

    if json {
        let out: Vec<_> = rows.iter().map(|r| serde_json::json!({
            "alloc_id": r.alloc_id,
            "ipfs_hash": r.ipfs_hash,
            "allocated_grt": r.allocated.0,
            "signal_grt": r.signal.map(|g| g.0),
            "staked_grt": r.staked.map(|g| g.0),
            "ratio": r.ratio,
            "est_grt_month": r.est_monthly.map(|(e, _)| e.0),
            "our_share_pct": r.est_monthly.map(|(_, s)| s),
            "synced": r.synced,
            "sync_pct": r.sync_pct,
        })).collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("\n{}", "Active Allocations".bold());
    println!("{}", format!("Network: {}  GRT: ${}  Issuance: {} GRT/mo  Signal: {} GRT",
        cfg.network.protocol_network,
        grt_price,
        fmt_comma(net_stats.monthly_issuance.0),
        fmt_comma(net_stats.total_signal.0),
    ).dimmed());

    let mut table = Table::new();
    table.load_preset(UTF8_BORDERS_ONLY);
    table.set_header(["Alloc ID", "Deployment", "Alloc GRT", "Signal GRT", "Share%", "Ratio", "Est GRT/mo", "Est $/mo", "Sync"]);

    let mut total_alloc = 0.0f64;
    let mut total_monthly = 0.0f64;

    for r in &rows {
        total_alloc += r.allocated.0;
        if let Some((est, share)) = r.est_monthly {
            total_monthly += est.0;
            table.add_row([
                &short_hash(&r.alloc_id),
                &short_hash(&r.ipfs_hash),
                &fmt_comma(r.allocated.0),
                &fmt_comma(r.signal.map(|g| g.0).unwrap_or(0.0)),
                &format!("{:.1}%", share),
                &fmt_ratio(r.ratio),
                &fmt_comma(est.0),
                &format!("${:.0}", est.0 * grt_price),
                &fmt_sync(r.synced, r.sync_pct),
            ]);
        } else {
            table.add_row([
                &short_hash(&r.alloc_id),
                &short_hash(&r.ipfs_hash),
                &fmt_comma(r.allocated.0),
                "–", "–", "–", "–", "–",
                &fmt_sync(r.synced, r.sync_pct),
            ]);
        }
    }
    println!("{table}");

    println!("  Total: {} GRT allocated  ~{} GRT/mo  ~${}/mo",
        fmt_comma(total_alloc),
        fmt_comma(total_monthly),
        fmt_comma(total_monthly * grt_price),
    );

    Ok(())
}

use crate::types::Grt;

struct AllocRow {
    alloc_id: String,
    ipfs_hash: String,
    allocated: Grt,
    signal: Option<Grt>,
    staked: Option<Grt>,
    ratio: f64,
    est_monthly: Option<(Grt, f64)>,  // (grt/mo, our_share_pct)
    synced: bool,
    sync_pct: f64,
}

use colored::Colorize;
