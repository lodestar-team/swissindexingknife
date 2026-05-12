use anyhow::Result;
use colored::Colorize;
use comfy_table::{Table, presets::UTF8_BORDERS_ONLY};

use crate::client::ManagementClient;
use crate::config::Config;
use crate::executor::RemoteExecutor;
use crate::output::table::*;
use crate::types::{grt::fmt_comma, Grt};

pub async fn list(cfg: &Config, json: bool) -> Result<()> {
    let exec = RemoteExecutor::from_config(&cfg.server, &cfg.docker, &cfg.api);
    let mgmt = ManagementClient::new(exec);

    let (rules, allocs) = tokio::join!(mgmt.indexing_rules(), mgmt.active_allocations());
    let rules = rules?;
    let allocs = allocs.unwrap_or_default();

    if json {
        println!("{}", serde_json::to_string_pretty(&rules)?);
        return Ok(());
    }

    println!("\n{}", "Indexing Rules".bold());
    println!("{}", "NOTE: `deployment` field is absent from indexingRules response (known agent quirk). Rules matched by amount.".dimmed());

    let mut table = Table::new();
    table.load_preset(UTF8_BORDERS_ONLY);
    table.set_header(["Identifier", "Basis", "Amount GRT", "Network", "On-chain?"]);

    for r in &rules {
        // Try to find matching on-chain allocation by amount (best we can do without deployment field)
        let on_chain = allocs.iter()
            .any(|a| (a.allocated_tokens.0 - r.allocation_amount.0).abs() < 1.0);

        let on_chain_str = if on_chain {
            "yes".green().to_string()
        } else {
            "pending".yellow().to_string()
        };

        table.add_row([
            &short_hash(&r.identifier),
            &r.decision_basis,
            &fmt_comma(r.allocation_amount.0),
            &r.protocol_network,
            &on_chain_str,
        ]);
    }
    println!("{table}");

    Ok(())
}

pub async fn set(
    cfg: &Config,
    deployment: &str,
    basis: &str,
    amount: Option<f64>,
    yes: bool,
) -> Result<()> {
    let amount_grt = amount.map(Grt);

    // Validate basis
    if basis != "always" && basis != "never" {
        anyhow::bail!("basis must be 'always' or 'never', got '{}'", basis);
    }

    // Show what we'll do
    println!("\nSetting indexing rule:");
    println!("  deployment:   {}", deployment);
    println!("  basis:        {}", basis);
    if let Some(a) = amount_grt {
        println!("  amount:       {} GRT (wei: {})", fmt_comma(a.0), a.to_wei_str());
    }
    println!("  network:      {}", cfg.network.protocol_network);

    // Skip prompt when --yes passed or stdin is not a terminal (e.g. scripted/piped).
    let need_confirm = !yes && std::io::IsTerminal::is_terminal(&std::io::stdin());
    if need_confirm {
        print!("\nProceed? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    let exec = RemoteExecutor::from_config(&cfg.server, &cfg.docker, &cfg.api);
    let mgmt = ManagementClient::new(exec);

    mgmt.set_indexing_rule(deployment, basis, amount_grt, &cfg.network.protocol_network).await?;

    println!("{} Rule set. Agent will act on next reconcile loop.", "✓".green().bold());
    if basis == "always" {
        println!("  {} If the deployment needs a graft base, ensure it is deployed first.", "→".yellow());
        println!("  {} The agent creates allocations automatically for `always` rules.", "→".yellow());
    } else {
        println!("  {} Agent will close the allocation on next reconcile loop.", "→".yellow());
    }

    Ok(())
}
