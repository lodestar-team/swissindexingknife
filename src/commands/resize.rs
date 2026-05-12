use anyhow::Result;
use colored::Colorize;

use crate::client::ManagementClient;
use crate::config::Config;
use crate::executor::RemoteExecutor;
use crate::types::{grt::fmt_comma, Grt};

pub async fn run(cfg: &Config, deployment: &str, amount_grt: f64, yes: bool) -> Result<()> {
    let exec = RemoteExecutor::from_config(&cfg.server, &cfg.docker, &cfg.api);
    let mgmt = ManagementClient::new(exec);

    // Look up active allocation for this deployment
    let allocs = mgmt.active_allocations().await?;
    let alloc = allocs.iter()
        .find(|a| a.ipfs_hash == deployment)
        .ok_or_else(|| anyhow::anyhow!("No active allocation found for {}", deployment))?;

    let grt = Grt(amount_grt);

    println!("\nResizing allocation:");
    println!("  deployment:   {}", deployment);
    println!("  alloc_id:     {}", alloc.id);
    println!("  current:      {} GRT", fmt_comma(alloc.allocated_tokens.0));
    println!("  new amount:   {} GRT", fmt_comma(amount_grt));
    println!("  network:      {}", cfg.network.protocol_network);

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

    let action_id = mgmt.queue_resize(
        deployment,
        &alloc.id,
        grt,
        &cfg.network.protocol_network,
    ).await?;

    println!("{} Resize action #{} queued.", "✓".green().bold(), action_id);
    println!("  {} Action is in `queued` state — run {} to execute it.",
        "→".yellow(),
        format!("sik actions approve {}", action_id).bold());

    Ok(())
}
