use anyhow::Result;
use colored::Colorize;

use crate::client::{ManagementClient, NetworkClient};
use crate::config::Config;
use crate::executor::RemoteExecutor;
use crate::output::table::*;
use crate::types::grt::fmt_comma;
use crate::types::Grt;

pub async fn run(cfg: &Config, json: bool) -> Result<()> {
    let exec = RemoteExecutor::from_config(&cfg.server, &cfg.docker, &cfg.api);
    let mgmt = ManagementClient::new(exec);
    let net = NetworkClient::new(cfg.network.subgraph_url.clone(), cfg.indexer.address.clone());

    let grt_price = match cfg.grt_price.manual_price_usd {
        Some(p) => p,
        None => net.grt_price_usd().await.unwrap_or(0.02646),
    };

    let (closed_res, active_res, net_stats_res) = tokio::join!(
        net.closed_allocations_this_month(),
        mgmt.active_allocations(),
        net.network_stats(),
    );

    let closed = closed_res.unwrap_or_default();
    let active = active_res.unwrap_or_default();
    let net_stats = net_stats_res?;

    // Revenue from closed allocations
    let closed_rewards: f64 = closed.iter().map(|a| a.indexing_rewards.0).sum();

    // Estimated revenue from active allocations
    // Rough: assume ~15 days elapsed out of 30 for current active allocations
    let days_in_month = 30.0f64;
    let today = chrono::Utc::now();
    let month_start = chrono::NaiveDate::from_ymd_opt(today.year(), today.month(), 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let month_start_ts = month_start.and_utc().timestamp();
    let days_elapsed = ((today.timestamp() - month_start_ts) as f64 / 86400.0).min(days_in_month);
    let days_remaining = (days_in_month - days_elapsed).max(0.0);

    // Active allocation estimated remaining rewards (simplified)
    let total_active_alloc: f64 = active.iter().map(|a| a.allocated_tokens.0).sum();
    let active_est_remaining = if net_stats.total_signal.0 > 0.0 && total_active_alloc > 0.0 {
        // Very rough estimate based on proportion of network stake — would need per-deployment signal
        0.0 // TODO: per-allocation estimate requires network subgraph enrichment
    } else {
        0.0
    };

    let gross_grt = closed_rewards + active_est_remaining;
    let delegation_cut = gross_grt * (cfg.economics.delegation_cut_bps as f64 / 10000.0);
    let net_grt = gross_grt - delegation_cut;
    let gross_usd = gross_grt * grt_price;
    let net_usd = net_grt * grt_price;
    let costs = cfg.economics.monthly_costs_usd;
    let profit = net_usd - costs;

    if json {
        let out = serde_json::json!({
            "grt_price_usd": grt_price,
            "closed_allocation_rewards_grt": closed_rewards,
            "active_allocations_est_remaining_grt": active_est_remaining,
            "gross_grt_mtd": gross_grt,
            "delegation_cut_grt": delegation_cut,
            "net_indexer_grt_mtd": net_grt,
            "gross_usd_mtd": gross_usd,
            "net_usd_mtd": net_usd,
            "monthly_costs_usd": costs,
            "net_profit_usd_mtd": profit,
            "closed_allocations": closed,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("\n{}", "P&L — Month to Date".bold());
    println!("{}", format!("GRT price: ${:.5}   Days elapsed: {:.0}/{:.0}",
        grt_price, days_elapsed, days_in_month).dimmed());

    section("REVENUE (GRT)");
    kv("Closed alloc rewards:", &format!("{} GRT", fmt_comma(closed_rewards)));
    kv("Active allocs (est.):", &format!("{} GRT (see `sik allocations`)", fmt_comma(active_est_remaining)));
    kv("Gross total:", &format!("{} GRT", fmt_comma(gross_grt)));
    kv(&format!("Delegation cut ({:.0}%):", cfg.economics.delegation_cut_bps as f64 / 100.0),
        &format!("−{} GRT", fmt_comma(delegation_cut)));
    kv_ok("Net to indexer:", &format!("{} GRT  ({})", fmt_comma(net_grt), fmt_usd(net_usd)));

    section("COSTS");
    kv("Infra (monthly):", &fmt_usd(costs));

    section("NET PROFIT (MTD)");
    if profit >= 0.0 {
        kv_ok("Net:", &fmt_usd(profit));
    } else {
        kv_err("Net:", &fmt_usd(profit));
    }

    if !closed.is_empty() {
        section("CLOSED ALLOCATIONS THIS MONTH");
        for a in &closed {
            println!("  {} → {} GRT rewards",
                short_hash(&a.ipfs_hash),
                fmt_comma(a.indexing_rewards.0),
            );
        }
    }

    Ok(())
}

use chrono::Datelike;

fn fmt_usd(v: f64) -> String {
    crate::types::grt::fmt_usd(v)
}
