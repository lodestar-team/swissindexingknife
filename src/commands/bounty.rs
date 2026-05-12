/// `sik bounty` — bounty workflow: check status and claim rewards.
///
/// The full flow is: deploy → allocate → sync → present POI → claim on dashboard.
/// `bounty status` gives a single-glance view of where you are in that flow.
/// `bounty claim` auto-looks up the allocation ID and queues a presentPOI action.
use anyhow::Result;
use colored::Colorize;

use crate::client::{GraphNodeClient, ManagementClient};
use crate::config::Config;
use crate::executor::RemoteExecutor;
use crate::output::table::short_hash;

pub async fn status(cfg: &Config, deployment_id: &str, json: bool) -> Result<()> {
    let exec = RemoteExecutor::from_config(&cfg.server, &cfg.docker, &cfg.api);
    let mgmt = ManagementClient::new(exec.clone());
    let gn = GraphNodeClient::new(exec);

    let (allocs, statuses, actions) = tokio::join!(
        mgmt.active_allocations(),
        gn.all_statuses(),
        mgmt.actions_all(),
    );

    let allocs = allocs.unwrap_or_default();
    let statuses = statuses.unwrap_or_default();
    let actions = actions.unwrap_or_default();

    // Match by full hash or prefix
    let alloc = allocs.iter().find(|a| {
        a.ipfs_hash == deployment_id || a.ipfs_hash.starts_with(deployment_id)
    });
    let sync = statuses.iter().find(|s| {
        s.subgraph == deployment_id || s.subgraph.starts_with(deployment_id)
    });
    let poi_actions: Vec<_> = actions.iter().filter(|a| {
        a.action_type == "presentPOI"
            && (a.deployment_id == deployment_id
                || a.deployment_id.starts_with(deployment_id))
    }).collect();

    if json {
        let out = serde_json::json!({
            "deployment": deployment_id,
            "allocation": alloc.map(|a| serde_json::json!({
                "alloc_id": a.id,
                "allocated_grt": a.allocated_tokens.0,
                "open": true,
            })),
            "synced": sync.map(|s| s.synced).unwrap_or(false),
            "sync_pct": sync.map(|s| s.pct_synced()).unwrap_or(0.0),
            "poi_actions": poi_actions.iter().map(|a| serde_json::json!({
                "id": a.id,
                "status": a.status,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("\n{} — Bounty Status", short_hash(deployment_id).bold());

    let alloc_line = match alloc {
        Some(a) => format!(
            "{} open  {} GRT  (ID: {})",
            "✓".green(),
            a.allocated_tokens.0,
            short_hash(&a.id)
        ),
        None => format!("{} no active allocation", "✗".red()),
    };
    println!("  Allocation:  {}", alloc_line);

    let sync_line = match sync {
        Some(s) if s.synced => format!("{} synced", "✓".green()),
        Some(s) => format!("{} {:.1}% synced", "…".yellow(), s.pct_synced()),
        None => format!("{} not indexed on this node", "✗".red()),
    };
    println!("  Sync:        {}", sync_line);

    let poi_line = if poi_actions.is_empty() {
        format!("{} no presentPOI action found", "–".dimmed())
    } else {
        let last = poi_actions.last().unwrap();
        format!("{} action #{} ({})", "✓".green(), last.id, last.status)
    };
    println!("  POI action:  {}", poi_line);

    let is_synced = sync.map(|s| s.synced).unwrap_or(false);
    if alloc.is_some() && is_synced && poi_actions.is_empty() {
        println!(
            "\n  {} Ready to claim! Run: {}",
            "→".cyan().bold(),
            format!("sik bounty claim {}", deployment_id).bold()
        );
    } else if alloc.is_none() {
        println!("\n  {} Open an allocation first (sik rule set <Qm...> always --amount <GRT>).", "!".yellow());
    } else if !is_synced {
        println!("\n  {} Wait for sync to complete before presenting POI.", "…".yellow());
    } else {
        println!("\n  {} POI already queued — check `sik actions` for status.", "✓".green());
    }

    Ok(())
}

pub async fn claim(
    cfg: &Config,
    deployment_id: &str,
    poi: Option<&str>,
    yes: bool,
) -> Result<()> {
    let exec = RemoteExecutor::from_config(&cfg.server, &cfg.docker, &cfg.api);
    let mgmt = ManagementClient::new(exec);

    // Auto-resolve allocation ID from active allocations
    let allocs = mgmt.active_allocations().await?;
    let alloc = allocs.iter().find(|a| {
        a.ipfs_hash == deployment_id || a.ipfs_hash.starts_with(deployment_id)
    });

    let alloc = match alloc {
        Some(a) => a.clone(),
        None => {
            eprintln!(
                "{} No active allocation found for {}.",
                "ERR".red().bold(),
                deployment_id
            );
            eprintln!("  Cannot present POI without an open allocation.");
            eprintln!("  Run `sik allocations` to see current allocations.");
            return Ok(());
        }
    };

    println!("\nBounty claim — presentPOI:");
    println!("  Deployment:    {}", deployment_id);
    println!("  Allocation ID: {}", alloc.id);
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

    let action_id = mgmt.queue_present_poi(deployment_id, &alloc.id, poi).await?;
    println!("{} Action #{} created (presentPOI, status: approved).", "✓".green().bold(), action_id);
    println!("  Agent will execute on next reconcile loop.");
    println!("\n  {} Verify the POI was accepted, then claim rewards on the dashboard.", "→".cyan());

    Ok(())
}
