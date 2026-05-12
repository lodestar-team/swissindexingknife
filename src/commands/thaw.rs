use anyhow::{Context, Result};
use colored::Colorize;
use comfy_table::{Table, presets::UTF8_BORDERS_ONLY};

use crate::client::{ManagementClient, NetworkClient};
use crate::config::Config;
use crate::executor::RemoteExecutor;
use crate::output::table::*;
use crate::types::{grt::fmt_comma, Grt};

pub async fn run(cfg: &Config, json: bool) -> Result<()> {
    let net = NetworkClient::new(cfg.network.subgraph_url.clone(), cfg.indexer.address.clone());
    let thaws = net.thaw_requests().await?;

    if json {
        let out: Vec<_> = thaws.iter().map(|t| serde_json::json!({
            "data_service": t.data_service,
            "shares_grt": t.shares.0,
            "thawing_until_unix": t.thawing_until_unix,
            "thawing_until_iso": t.thawing_until_iso(),
            "mature": t.mature,
            "hours_remaining": t.hours_remaining(),
        })).collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("\n{}", "Thaw Requests".bold());
    println!("{}", format!("Indexer: {}", cfg.indexer.address).dimmed());

    if thaws.is_empty() {
        println!("  No pending thaw requests.");
        return Ok(());
    }

    let mut table = Table::new();
    table.load_preset(UTF8_BORDERS_ONLY);
    table.set_header(["Data Service", "Shares GRT", "Thaws At", "Status"]);

    let mut has_mature = false;
    for t in &thaws {
        let svc = t.data_service.split('/').last().unwrap_or(&t.data_service);
        let status = fmt_thaw(t.mature, t.hours_remaining());
        if t.mature { has_mature = true; }
        table.add_row([
            svc,
            &fmt_comma(t.shares.0),
            &t.thawing_until_iso(),
            &status,
        ]);
    }
    println!("{table}");

    if has_mature {
        println!("\n  {} Matured thaws ready to withdraw!", "→".green().bold());
        println!("  Run: {}", "sik thaw withdraw --yes".bold());
        println!("  Requires: {}", "INDEXER_COLD_WALLET_KEY env var (indexer cold wallet, not operator)".yellow());
    }

    Ok(())
}

pub async fn withdraw(
    cfg: &Config,
    yes: bool,
    cold_wallet_key: Option<String>,
    rpc_url: Option<String>,
) -> Result<()> {
    let net = NetworkClient::new(cfg.network.subgraph_url.clone(), cfg.indexer.address.clone());
    let thaws = net.thaw_requests().await?;
    let mature: Vec<_> = thaws.iter().filter(|t| t.mature).collect();

    if mature.is_empty() {
        println!("No matured thaws ready to withdraw.");
        return Ok(());
    }

    // Resolve cold wallet key: CLI arg > env var
    let cold_key = cold_wallet_key
        .or_else(|| std::env::var("INDEXER_COLD_WALLET_KEY").ok())
        .context("Indexer cold wallet key required.\nPass --cold-wallet-key or set INDEXER_COLD_WALLET_KEY env var.")?;

    let rpc = rpc_url
        .or_else(|| std::env::var("ARB_RPC_URL").ok())
        .unwrap_or_else(|| "https://arb1.arbitrum.io/rpc".into());

    println!("\nMatured thaws ready to withdraw:");
    let mut total_grt = 0.0f64;
    for t in &mature {
        let svc = t.data_service.split('/').last().unwrap_or(&t.data_service);
        println!("  {} — {} GRT", svc, fmt_comma(t.shares.0));
        total_grt += t.shares.0;
    }
    println!("  Total: {} GRT", fmt_comma(total_grt));
    println!("\n  {}", "NOTE: deprovision requires the indexer cold wallet, not the operator key.".yellow());

    let need_confirm = !yes && std::io::IsTerminal::is_terminal(&std::io::stdin());
    if need_confirm {
        print!("\nProceed with on-chain deprovision + re-provision? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    let horizon = "0x00669A4CF01450B64E8A2A20E9b1FCB71E61eF03";

    for t in &mature {
        println!("\n→ deprovision({}, {}, 1)...", cfg.indexer.address, t.data_service);
        let status = std::process::Command::new("cast")
            .args([
                "send", horizon,
                "deprovision(address,address,uint256)",
                &cfg.indexer.address,
                &t.data_service,
                "1",
                "--private-key", &cold_key,
                "--rpc-url", &rpc,
            ])
            .status()
            .context("cast not found — install foundry: https://getfoundry.sh")?;

        if !status.success() {
            anyhow::bail!("deprovision failed for {} (exit code: {})", t.data_service, status);
        }
        println!("{} deprovision OK", "✓".green().bold());
    }

    // Re-provision freed GRT via management API
    println!("\n→ addToProvision({} GRT)...", fmt_comma(total_grt));
    let exec = RemoteExecutor::from_config(&cfg.server, &cfg.docker, &cfg.api);
    let mgmt = ManagementClient::new(exec);
    mgmt.add_to_provision(Grt(total_grt), &cfg.network.protocol_network).await?;

    println!("{} {} GRT re-provisioned to SubgraphService.", "✓".green().bold(), fmt_comma(total_grt));
    println!("  {} Allocation capacity increased — check with {}.", "→".yellow(), "sik status".bold());

    Ok(())
}
