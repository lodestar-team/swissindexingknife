use anyhow::Result;
use colored::Colorize;

use crate::client::ManagementClient;
use crate::config::Config;
use crate::executor::RemoteExecutor;

pub async fn run(
    cfg: &Config,
    deployment_id: &str,
    allocation_id: &str,
    poi: Option<&str>,
    yes: bool,
) -> Result<()> {
    let exec = RemoteExecutor::from_config(&cfg.server, &cfg.docker, &cfg.api);
    let mgmt = ManagementClient::new(exec);

    println!("\nPresenting POI:");
    println!("  Deployment:    {}", deployment_id);
    println!("  Allocation ID: {}", allocation_id);
    println!("  POI:           {}", poi.unwrap_or("(automatic — zero hash)"));
    println!("\nQueues a presentPOI action (status: approved) for the agent to execute.");

    if !yes {
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

    let action_id = mgmt.queue_present_poi(deployment_id, allocation_id, poi).await?;
    println!("{} Action #{} created (presentPOI, status: approved).", "✓".green().bold(), action_id);
    println!("  Agent will execute on next reconcile loop.");

    Ok(())
}
