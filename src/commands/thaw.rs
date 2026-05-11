use anyhow::Result;
use colored::Colorize;
use comfy_table::{Table, presets::UTF8_BORDERS_ONLY};

use crate::client::NetworkClient;
use crate::config::Config;
use crate::output::table::*;
use crate::types::grt::fmt_comma;

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
        println!("  Run: {}", "sik thaw withdraw".bold());
        println!("\n  {}", "NOTE: deprovision requires the INDEXER COLD WALLET (not operator).".yellow());
        let cmd = format!(
            "cast send 0x00669A4CF01450B64E8A2A20E9b1FCB71E61eF03 \\\n    \"deprovision(address,address,uint256)\" \\\n    {} \\\n    <DATA_SERVICE_ADDRESS> 1 \\\n    --private-key <INDEXER_COLD_WALLET_KEY> --rpc-url <ARB_RPC>",
            cfg.indexer.address,
        );
        println!("\n  Manual cast command:\n  {}", cmd.dimmed());
    }

    Ok(())
}
