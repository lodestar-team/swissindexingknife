use anyhow::Result;
use colored::Colorize;
use comfy_table::{Table, presets::UTF8_BORDERS_ONLY};

use crate::client::ManagementClient;
use crate::config::Config;
use crate::executor::RemoteExecutor;
use crate::output::table::*;
use crate::types::grt::fmt_comma;

pub async fn list(cfg: &Config, status_filter: Option<&str>, json: bool) -> Result<()> {
    let exec = RemoteExecutor::from_config(&cfg.server, &cfg.docker, &cfg.api);
    let mgmt = ManagementClient::new(exec);

    // ActionFilter.status is a String (singular). Fetch by specific status or all.
    let mut actions = match status_filter {
        Some(f) => mgmt.actions_by_status(f).await?,
        None => mgmt.actions().await?,   // active only (queued|approved|pending)
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&actions)?);
        return Ok(());
    }

    let title = match status_filter {
        Some(f) => format!("Actions ({})", f),
        None => "Actions (all pending)".into(),
    };
    println!("\n{}", title.bold());

    if actions.is_empty() {
        println!("  No actions in queue.");
        return Ok(());
    }

    let mut table = Table::new();
    table.load_preset(UTF8_BORDERS_ONLY);
    table.set_header(["ID", "Type", "Deployment", "Amount GRT", "Status", "Reason"]);

    for a in &actions {
        let status_str = match a.status.as_str() {
            "approved" => a.status.green().to_string(),
            "queued" => a.status.yellow().to_string(),
            "failed" => a.status.red().to_string(),
            _ => a.status.normal().to_string(),
        };
        table.add_row([
            &a.id.to_string(),
            &a.action_type,
            &short_hash(&a.deployment_id),
            &fmt_comma(a.amount),
            &status_str,
            &a.reason,
        ]);
    }
    println!("{table}");

    let queued: Vec<_> = actions.iter().filter(|a| a.status == "queued").collect();
    if !queued.is_empty() {
        println!("\n  {} {} action(s) in 'queued' status will NOT execute until approved.",
            "⚠".yellow(), queued.len());
        println!("  Run: {} to unblock.", "sik actions approve <id>".bold());
    }

    Ok(())
}

pub async fn approve(cfg: &Config, action_id: i64) -> Result<()> {
    let exec = RemoteExecutor::from_config(&cfg.server, &cfg.docker, &cfg.api);
    let mgmt = ManagementClient::new(exec);

    // QUIRK: must use updateActions mutation, not a dedicated approve mutation
    let mutation = format!(r#"mutation {{
      updateActions(
        filter: {{ id: {} }}
        action: {{ status: approved }}
      ) {{
        id
        status
      }}
    }}"#, action_id);

    let data = mgmt.query(&mutation).await?;
    let updated = &data["updateActions"];
    println!("{} Action {} approved.", "✓".green().bold(), action_id);
    println!("  Agent will execute on next reconcile loop.");
    println!("  State: {}", updated);

    Ok(())
}
