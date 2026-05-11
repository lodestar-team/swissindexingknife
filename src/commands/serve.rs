use anyhow::Result;
use axum::{
    extract::State,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use std::sync::Arc;
use tower_http::cors::CorsLayer;

use crate::client::{GraphNodeClient, ManagementClient, NetworkClient};
use crate::config::Config;
use crate::executor::RemoteExecutor;

#[derive(Clone)]
struct AppState {
    cfg: Arc<Config>,
}

pub async fn run(cfg: Config, port: u16, open: bool) -> Result<()> {
    let state = AppState { cfg: Arc::new(cfg) };

    let app = Router::new()
        .route("/", get(serve_html))
        .route("/api/data", get(api_data))
        .route("/api/server", get(api_server))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    println!("Dashboard: http://{}", addr);
    if open {
        let _ = std::process::Command::new("open").arg(format!("http://{}", addr)).spawn();
    }

    axum::serve(listener, app).await?;
    Ok(())
}

async fn serve_html() -> impl IntoResponse {
    Html(DASHBOARD_HTML)
}

async fn api_data(State(state): State<AppState>) -> impl IntoResponse {
    let cfg = &state.cfg;
    let exec = RemoteExecutor::from_config(&cfg.server, &cfg.docker, &cfg.api);
    let mgmt = ManagementClient::new(exec.clone());
    let gn = GraphNodeClient::new(exec);
    let net = NetworkClient::new(cfg.network.subgraph_url.clone(), cfg.indexer.address.clone());

    let grt_price = cfg.grt_price.manual_price_usd.unwrap_or(0.02646);

    let (allocs_r, rules_r, actions_r, statuses_r, indexer_r, thaws_r, net_stats_r) = tokio::join!(
        mgmt.active_allocations(),
        mgmt.indexing_rules(),
        mgmt.actions(),
        gn.all_statuses(),
        net.indexer_details(),
        net.thaw_requests(),
        net.network_stats(),
    );

    let allocs = allocs_r.unwrap_or_default();
    let rules = rules_r.unwrap_or_default();
    let actions = actions_r.unwrap_or_default();
    let statuses = statuses_r.unwrap_or_default();
    let thaws = thaws_r.unwrap_or_default();
    let net_stats = net_stats_r.ok();

    // Enrich allocations with network data
    let mut enriched_allocs = Vec::new();
    for a in &allocs {
        let nd = net.deployment(&a.ipfs_hash).await.ok().flatten();
        let sync = statuses.iter().find(|s| s.subgraph == a.ipfs_hash);
        let est_monthly = nd.as_ref().and_then(|d| {
            net_stats.as_ref().map(|ns| d.est_monthly_grt(a.allocated_tokens, ns))
        });
        let our_share = nd.as_ref().map(|d| {
            if d.staked.0 > 0.0 { a.allocated_tokens.0 / d.staked.0 * 100.0 } else { 100.0 }
        });

        enriched_allocs.push(serde_json::json!({
            "id": a.id,
            "ipfs_hash": a.ipfs_hash,
            "short_hash": short_hash(&a.ipfs_hash),
            "allocated_grt": a.allocated_tokens.0,
            "signal_grt": nd.as_ref().map(|d| d.signal.0),
            "staked_grt": nd.as_ref().map(|d| d.staked.0),
            "ratio": nd.as_ref().map(|d| d.ratio),
            "denied_at": nd.as_ref().map(|d| d.denied_at),
            "our_share_pct": our_share,
            "est_grt_month": est_monthly.map(|g| g.0),
            "est_usd_month": est_monthly.map(|g| g.0 * grt_price),
            "synced": sync.map(|s| s.synced),
            "sync_pct": sync.map(|s| s.pct_synced()),
            "blocks_behind": sync.map(|s| s.blocks_behind()),
            "health": sync.map(|s| s.health.as_str()),
            "network": sync.map(|s| s.network.as_str()),
        }));
    }

    // Zombies — syncing with no allocation
    let alloc_hashes: std::collections::HashSet<&str> =
        allocs.iter().map(|a| a.ipfs_hash.as_str()).collect();
    let zombies: Vec<_> = statuses.iter()
        .filter(|s| !alloc_hashes.contains(s.subgraph.as_str()))
        .map(|s| serde_json::json!({
            "ipfs_hash": s.subgraph,
            "short_hash": short_hash(&s.subgraph),
            "network": s.network,
            "synced": s.synced,
            "sync_pct": s.pct_synced(),
            "health": s.health,
        }))
        .collect();

    // Thaw serialisation
    let thaws_json: Vec<_> = thaws.iter().map(|t| serde_json::json!({
        "data_service": t.data_service.split('/').last().unwrap_or(&t.data_service),
        "shares_grt": t.shares.0,
        "thawing_until_iso": t.thawing_until_iso(),
        "mature": t.mature,
        "hours_remaining": t.hours_remaining(),
    })).collect();

    // Estimated monthly
    let est_total_monthly: f64 = enriched_allocs.iter()
        .filter_map(|a| a["est_grt_month"].as_f64())
        .sum();

    let out = serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "grt_price_usd": grt_price,
        "network": cfg.network.protocol_network,
        "indexer": {
            "address": cfg.indexer.address,
            "operator": cfg.indexer.operator_address,
            "stake": indexer_r.as_ref().ok().map(|i| serde_json::json!({
                "own_grt": i.staked.0,
                "delegated_grt": i.delegated.0,
                "capacity_grt": i.capacity.0,
                "allocated_grt": i.allocated.0,
                "available_grt": i.available.0,
                "utilisation_pct": if i.capacity.0 > 0.0 { i.allocated.0 / i.capacity.0 * 100.0 } else { 0.0 },
            })),
        },
        "economics": {
            "est_grt_month": est_total_monthly,
            "est_usd_month": est_total_monthly * grt_price,
            "monthly_costs_usd": cfg.economics.monthly_costs_usd,
            "net_usd_month": est_total_monthly * grt_price - cfg.economics.monthly_costs_usd,
            "delegation_cut_pct": cfg.economics.delegation_cut_bps as f64 / 100.0,
        },
        "network_stats": net_stats.map(|ns| serde_json::json!({
            "total_signal_grt": ns.total_signal.0,
            "monthly_issuance_grt": ns.monthly_issuance.0,
        })),
        "allocations": enriched_allocs,
        "rules": rules,
        "actions": actions,
        "thaws": thaws_json,
        "zombies": zombies,
    });

    Json(out)
}

async fn api_server(State(state): State<AppState>) -> impl IntoResponse {
    let cfg = &state.cfg;
    let metrics = collect_server_metrics(cfg).await;
    Json(metrics)
}

async fn collect_server_metrics(cfg: &Config) -> serde_json::Value {
    // SSH in and collect system metrics + docker container stats
    let script = r#"
printf 'LOADAVG:%s\n' "$(cat /proc/loadavg)"
printf 'MEM:%s\n' "$(free -b | grep '^Mem:')"
printf 'DISK:%s\n' "$(df -B1 / | tail -1)"
printf 'NPROC:%s\n' "$(nproc)"
printf 'UPTIME:%s\n' "$(cat /proc/uptime | awk '{print $1}')"
docker ps --format '{"name":"{{.Names}}","status":"{{.Status}}","image":"{{.Image}}"}' 2>/dev/null
"#;
    let cmd = format!("bash -c '{}'", script.replace("'", "'\"'\"'"));

    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;

    let key = crate::executor::shellexpand_tilde(&cfg.server.ssh_key);
    let result = tokio::process::Command::new("ssh")
        .args([
            "-i", &key,
            "-o", "BatchMode=yes",
            "-o", "StrictHostKeyChecking=accept-new",
            "-o", "ConnectTimeout=8",
            &format!("{}@{}", cfg.server.user, cfg.server.host),
            &cmd,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;

    match result {
        Err(e) => serde_json::json!({ "error": e.to_string() }),
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            parse_server_metrics(&text, &cfg.server.host)
        }
    }
}

fn parse_server_metrics(text: &str, host: &str) -> serde_json::Value {
    let mut load: Option<(f64, f64, f64)> = None;
    let mut mem_total: u64 = 0;
    let mut mem_used: u64 = 0;
    let mut disk_total: u64 = 0;
    let mut disk_used: u64 = 0;
    let mut nproc: u32 = 0;
    let mut uptime_secs: f64 = 0.0;
    let mut containers: Vec<serde_json::Value> = Vec::new();

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("LOADAVG:") {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() >= 3 {
                load = Some((
                    parts[0].parse().unwrap_or(0.0),
                    parts[1].parse().unwrap_or(0.0),
                    parts[2].parse().unwrap_or(0.0),
                ));
            }
        } else if let Some(rest) = line.strip_prefix("MEM:") {
            // "Mem: total used free shared buff/cache available"
            let parts: Vec<u64> = rest.split_whitespace()
                .skip(1) // skip "Mem:" label if still present
                .filter_map(|s| s.parse().ok())
                .collect();
            if parts.len() >= 2 {
                mem_total = parts[0];
                mem_used = parts[1];
            }
        } else if let Some(rest) = line.strip_prefix("DISK:") {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() >= 3 {
                disk_total = parts[1].parse().unwrap_or(0);
                disk_used = parts[2].parse().unwrap_or(0);
            }
        } else if let Some(rest) = line.strip_prefix("NPROC:") {
            nproc = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("UPTIME:") {
            uptime_secs = rest.trim().parse().unwrap_or(0.0);
        } else if line.starts_with('{') {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                containers.push(v);
            }
        }
    }

    let mem_pct = if mem_total > 0 { mem_used as f64 / mem_total as f64 * 100.0 } else { 0.0 };
    let disk_pct = if disk_total > 0 { disk_used as f64 / disk_total as f64 * 100.0 } else { 0.0 };

    let uptime_h = (uptime_secs / 3600.0) as u64;
    let uptime_d = uptime_h / 24;
    let uptime_str = if uptime_d > 0 {
        format!("{}d {}h", uptime_d, uptime_h % 24)
    } else {
        format!("{}h", uptime_h)
    };

    serde_json::json!({
        "load": load.map(|(a,b,c)| serde_json::json!({ "1m": a, "5m": b, "15m": c })),
        "nproc": nproc,
        "load_pct": load.map(|(a,_,_)| a / nproc.max(1) as f64 * 100.0),
        "mem": {
            "total_gb": mem_total as f64 / 1e9,
            "used_gb": mem_used as f64 / 1e9,
            "pct": mem_pct,
        },
        "disk": {
            "total_gb": disk_total as f64 / 1e9,
            "used_gb": disk_used as f64 / 1e9,
            "pct": disk_pct,
        },
        "uptime": uptime_str,
        "host": host,
        "containers": containers,
    })
}

fn short_hash(h: &str) -> String {
    if h.len() > 14 {
        format!("{}…{}", &h[..8], &h[h.len()-4..])
    } else {
        h.to_string()
    }
}

// ─── Embedded Dashboard ───────────────────────────────────────────────────────

const DASHBOARD_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>LODESTAR // INDEXER</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link href="https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@300;400;500;700&display=swap" rel="stylesheet">
<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.0/dist/chart.umd.min.js"></script>
<style>
:root {
  --bg:       #080501;
  --surface:  #120c03;
  --surface2: #1a1105;
  --border:   #5c3d15;
  --border2:  #3a2709;
  --copper:   #c4922a;
  --gold:     #e8b84b;
  --brass:    #b8952a;
  --green:    #7eb87e;
  --red:      #cc4422;
  --amber:    #e8922a;
  --dim:      #7d6035;
  --text:     #d4c49a;
  --steam:    #f0e0b8;
  --font:     'JetBrains Mono', 'Courier New', monospace;
}

* { box-sizing: border-box; margin: 0; padding: 0; }

body {
  background: var(--bg);
  color: var(--text);
  font-family: var(--font);
  font-size: 13px;
  min-height: 100vh;
  overflow-x: hidden;
}

body::after {
  content: '';
  position: fixed;
  inset: 0;
  background: repeating-linear-gradient(0deg, transparent, transparent 3px, rgba(0,0,0,.05) 3px, rgba(0,0,0,.05) 6px);
  pointer-events: none;
  z-index: 9999;
}

/* ── Header ─────────────────────────────────────── */
header {
  display: flex;
  align-items: center;
  gap: 18px;
  padding: 12px 20px;
  border-bottom: 2px solid var(--border);
  background: linear-gradient(180deg, #1c1206 0%, var(--surface) 100%);
  position: sticky;
  top: 0;
  z-index: 100;
}

.logo {
  font-size: 18px;
  font-weight: 700;
  letter-spacing: 4px;
  color: var(--gold);
  text-shadow: 0 0 30px rgba(232,184,75,.35);
}

.logo span { color: var(--dim); }

.address {
  color: var(--dim);
  font-size: 11px;
  letter-spacing: 1px;
  flex: 1;
}

.live-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--green);
  box-shadow: 0 0 10px var(--green);
  animation: pulse 2s infinite;
}

@keyframes pulse {
  0%, 100% { opacity: 1; box-shadow: 0 0 10px var(--green); }
  50%       { opacity: .4; box-shadow: none; }
}

.badge {
  padding: 3px 10px;
  border: 1px solid currentColor;
  border-radius: 2px;
  font-size: 11px;
  letter-spacing: 1px;
  font-weight: 700;
}

.badge-gold   { color: var(--gold);   border-color: rgba(232,184,75,.4); }
.badge-copper { color: var(--copper); border-color: rgba(196,146,42,.4); }
.badge-green  { color: var(--green);  border-color: rgba(126,184,126,.3); }
.badge-red    { color: var(--red);    border-color: rgba(204,68,34,.4); }
.badge-dim    { color: var(--dim);    border-color: var(--border2); }

#ts { color: var(--dim); font-size: 11px; }
#grt-price { color: var(--gold); font-weight: 700; }

/* ── Layout ─────────────────────────────────────── */
main {
  padding: 16px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

/* ── Cards (stat blocks) ─────────────────────────── */
.cards { display: grid; gap: 10px; }
.cards-5 { grid-template-columns: repeat(5, 1fr); }
.cards-3 { grid-template-columns: repeat(3, 1fr); }

.card {
  background: var(--surface);
  border: 1px solid var(--border);
  border-top: 2px solid var(--copper);
  padding: 14px 16px;
  border-radius: 2px;
  position: relative;
}

.card::before {
  content: '';
  position: absolute;
  top: 0; left: 0; right: 0;
  height: 1px;
  background: linear-gradient(90deg, transparent, rgba(232,184,75,.2), transparent);
}

.card-label {
  color: var(--dim);
  font-size: 10px;
  letter-spacing: 2px;
  text-transform: uppercase;
  margin-bottom: 8px;
}

.card-value {
  font-size: 26px;
  font-weight: 700;
  line-height: 1;
  color: var(--copper);
}

.card-value.green { color: var(--green); }
.card-value.amber { color: var(--amber); }
.card-value.gold  { color: var(--gold); }
.card-value.red   { color: var(--red); }

.card-sub {
  color: var(--dim);
  font-size: 10px;
  margin-top: 6px;
}

/* ── Panels ──────────────────────────────────────── */
.panel {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 2px;
  overflow: hidden;
}

.panel-header {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 14px;
  border-bottom: 1px solid var(--border);
  background: linear-gradient(180deg, #1e1508 0%, var(--surface2) 100%);
  font-size: 11px;
  letter-spacing: 2px;
  color: var(--gold);
  text-transform: uppercase;
  font-weight: 700;
}

.panel-body { padding: 12px 14px; }

.panel-grid { display: grid; gap: 12px; }
.panel-grid-2 { grid-template-columns: 2fr 1fr; }
.panel-grid-3 { grid-template-columns: 1fr 1fr 1fr; }

/* ── Tables ──────────────────────────────────────── */
.tbl { width: 100%; border-collapse: collapse; }
.tbl th {
  color: var(--dim);
  font-size: 10px;
  letter-spacing: 1.5px;
  text-align: left;
  padding: 7px 10px;
  border-bottom: 1px solid var(--border);
  white-space: nowrap;
  font-weight: 400;
  background: var(--surface2);
}
.tbl td {
  padding: 8px 10px;
  border-bottom: 1px solid rgba(92,61,21,.3);
  white-space: nowrap;
  vertical-align: middle;
  font-size: 13px;
}
.tbl tr:last-child td { border-bottom: none; }
.tbl tr:hover td { background: rgba(196,146,42,.04); }

.mono { font-family: var(--font); }
.hash { color: var(--copper); font-size: 12px; }
.val      { color: var(--text); }
.val-green { color: var(--green); }
.val-amber { color: var(--amber); }
.val-gold  { color: var(--gold); }
.val-red   { color: var(--red); }
.val-dim   { color: var(--dim); }

/* ── Progress bars ───────────────────────────────── */
.bar-wrap {
  display: flex;
  align-items: center;
  gap: 8px;
}
.bar {
  flex: 1;
  height: 5px;
  background: var(--border2);
  border-radius: 2px;
  overflow: hidden;
  min-width: 70px;
}
.bar-fill {
  height: 100%;
  border-radius: 2px;
  transition: width .6s ease;
}
.bar-fill.green  { background: var(--green); }
.bar-fill.amber  { background: var(--amber); }
.bar-fill.red    { background: var(--red); }
.bar-fill.gold   { background: var(--gold); }
.bar-fill.copper { background: var(--copper); }

.bar-label { font-size: 11px; min-width: 42px; text-align: right; color: var(--text); }

/* ── Server metrics ──────────────────────────────── */
.metric-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 7px 0;
  border-bottom: 1px solid rgba(92,61,21,.3);
}
.metric-row:last-child { border-bottom: none; }
.metric-name { color: var(--dim); width: 60px; font-size: 11px; letter-spacing: 1px; }
.metric-val  { color: var(--text); width: 60px; font-size: 11px; text-align: right; }

/* Container statuses */
.container-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 0;
  font-size: 12px;
}
.dot { width: 6px; height: 6px; border-radius: 50%; flex-shrink: 0; }
.dot-green { background: var(--green); box-shadow: 0 0 5px var(--green); }
.dot-amber { background: var(--amber); }
.dot-red   { background: var(--red); }
.ct-name   { color: var(--text); flex: 1; }
.ct-status { color: var(--dim); font-size: 10px; }

/* ── Section label ───────────────────────────────── */
.section-label {
  font-size: 10px;
  letter-spacing: 2px;
  color: var(--brass);
  text-transform: uppercase;
  margin-bottom: 6px;
  border-bottom: 1px solid var(--border2);
  padding-bottom: 4px;
}

/* ── Empty state ─────────────────────────────────── */
.empty { color: var(--dim); padding: 14px; font-size: 12px; }

/* ── Chart canvas ────────────────────────────────── */
canvas { max-height: 200px; }

/* ── Ratio coloring ──────────────────────────────── */
.ratio-good { color: var(--green); }
.ratio-ok   { color: var(--amber); }
.ratio-bad  { color: var(--red); }

/* ── Thaw rows ───────────────────────────────────── */
.thaw-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 8px 0;
  border-bottom: 1px solid rgba(92,61,21,.3);
  font-size: 12px;
}
.thaw-row:last-child { border-bottom: none; }

/* ── Error banner ────────────────────────────────── */
#error-banner {
  display: none;
  padding: 10px 14px;
  background: rgba(204,68,34,.1);
  border: 1px solid rgba(204,68,34,.4);
  color: var(--red);
  font-size: 12px;
  margin: 10px 16px 0;
  border-radius: 2px;
}

/* ── Refresh indicator ───────────────────────────── */
.spin { animation: spin 1s linear infinite; display: inline-block; }
@keyframes spin { to { transform: rotate(360deg); } }
</style>
</head>
<body>

<header>
  <div class="logo">LODESTAR<span> // </span>SWISS INDEXING KNIFE 🔪</div>
  <div class="address" id="hdr-address">loading...</div>
  <div class="live-dot"></div>
  <span id="grt-price" class="badge badge-gold">GRT $—</span>
  <span id="reo-badge" class="badge badge-dim">REO —</span>
  <span id="hdr-net" class="badge badge-copper">—</span>
  <span id="ts">—</span>
  <span id="refresh-icon" style="color:var(--dim)"></span>
</header>

<div id="error-banner"></div>

<main>

  <!-- ── Stake cards ─────────────────────────────── -->
  <div class="cards cards-5" id="stake-cards">
    <div class="card"><div class="card-label">⚙ Own Stake</div><div class="card-value" id="s-own">—</div><div class="card-sub">GRT</div></div>
    <div class="card"><div class="card-label">⚙ Delegated</div><div class="card-value" id="s-del">—</div><div class="card-sub">GRT</div></div>
    <div class="card"><div class="card-label">⚙ Capacity</div><div class="card-value" id="s-cap">—</div><div class="card-sub">GRT</div></div>
    <div class="card"><div class="card-label">⚙ Allocated</div><div class="card-value" id="s-alloc">—</div><div class="card-sub" id="s-util">GRT</div></div>
    <div class="card"><div class="card-label">⚙ Free</div><div class="card-value green" id="s-free">—</div><div class="card-sub" id="s-free-pct">GRT</div></div>
  </div>

  <!-- ── Economics ──────────────────────────────── -->
  <div class="cards cards-3" id="econ-cards">
    <div class="card"><div class="card-label">⚡ Est. Rewards / Month</div><div class="card-value gold" id="e-rewards">—</div><div class="card-sub" id="e-rewards-usd">GRT</div></div>
    <div class="card"><div class="card-label">⚡ Monthly Costs</div><div class="card-value amber" id="e-costs">—</div><div class="card-sub">USD</div></div>
    <div class="card"><div class="card-label">⚡ Net P&amp;L / Month</div><div class="card-value" id="e-pnl">—</div><div class="card-sub" id="e-pnl-grt">USD</div></div>
  </div>

  <!-- ── Main: allocations + server ────────────── -->
  <div class="panel-grid panel-grid-2">

    <!-- Allocations table -->
    <div class="panel">
      <div class="panel-header">
        <span>⚙ ALLOCATIONS</span>
        <span id="alloc-count" class="val-dim" style="font-weight:400;font-size:10px"></span>
      </div>
      <div style="overflow-x:auto">
        <table class="tbl" id="alloc-table">
          <thead>
            <tr>
              <th>DEPLOYMENT</th>
              <th>AMOUNT GRT</th>
              <th>SIGNAL GRT</th>
              <th>RATIO</th>
              <th>SHARE</th>
              <th>EST GRT/MO</th>
              <th>EST USD/MO</th>
              <th>SYNC</th>
              <th>NET</th>
            </tr>
          </thead>
          <tbody id="alloc-body">
            <tr><td colspan="9" class="empty">Loading...</td></tr>
          </tbody>
        </table>
      </div>
    </div>

    <!-- Server metrics -->
    <div class="panel">
      <div class="panel-header">⚙ SERVER METRICS <span id="srv-host" class="val-dim" style="font-weight:400;font-size:10px"></span></div>
      <div class="panel-body">
        <div class="section-label" style="margin-bottom:10px">SYSTEM</div>
        <div class="metric-row">
          <div class="metric-name">CPU</div>
          <div class="bar-wrap" style="flex:1">
            <div class="bar"><div class="bar-fill" id="cpu-bar" style="width:0%"></div></div>
            <div class="bar-label" id="cpu-val">—</div>
          </div>
        </div>
        <div class="metric-row">
          <div class="metric-name">RAM</div>
          <div class="bar-wrap" style="flex:1">
            <div class="bar"><div class="bar-fill" id="ram-bar" style="width:0%"></div></div>
            <div class="bar-label" id="ram-val">—</div>
          </div>
        </div>
        <div class="metric-row">
          <div class="metric-name">DISK /</div>
          <div class="bar-wrap" style="flex:1">
            <div class="bar"><div class="bar-fill" id="disk-bar" style="width:0%"></div></div>
            <div class="bar-label" id="disk-val">—</div>
          </div>
        </div>
        <div class="metric-row">
          <div class="metric-name">LOAD</div>
          <div style="flex:1;color:var(--text);font-size:12px" id="load-val">—</div>
        </div>
        <div class="metric-row">
          <div class="metric-name">UPTIME</div>
          <div style="flex:1;color:var(--green);font-size:12px" id="uptime-val">—</div>
        </div>

        <div class="section-label" style="margin-top:16px;margin-bottom:10px">CONTAINERS</div>
        <div id="container-list"><div class="val-dim" style="font-size:10px">Loading...</div></div>
      </div>
    </div>

  </div>

  <!-- ── Charts ─────────────────────────────────── -->
  <div class="panel-grid panel-grid-2">
    <div class="panel">
      <div class="panel-header">⚙ SIGNAL / STAKE RATIO  <span class="val-dim" style="font-weight:400;font-size:10px">higher = better opportunity</span></div>
      <div class="panel-body"><canvas id="chart-ratio"></canvas></div>
    </div>
    <div class="panel">
      <div class="panel-header">⚙ SYNC PROGRESS</div>
      <div class="panel-body"><canvas id="chart-sync"></canvas></div>
    </div>
  </div>

  <!-- ── Lower row: thaws + actions + zombies ──── -->
  <div class="panel-grid panel-grid-3">

    <div class="panel">
      <div class="panel-header">⚙ THAW REQUESTS</div>
      <div class="panel-body" id="thaw-body"><div class="empty">Loading...</div></div>
    </div>

    <div class="panel">
      <div class="panel-header">⚙ PENDING ACTIONS</div>
      <div style="overflow-x:auto">
        <table class="tbl" id="actions-table">
          <thead><tr><th>ID</th><th>TYPE</th><th>DEPLOYMENT</th><th>AMOUNT</th><th>STATUS</th></tr></thead>
          <tbody id="actions-body"><tr><td colspan="5" class="empty">Loading...</td></tr></tbody>
        </table>
      </div>
    </div>

    <div class="panel">
      <div class="panel-header">⚙ ZOMBIE DEPLOYMENTS <span class="val-dim" style="font-weight:400;font-size:10px">syncing, no allocation</span></div>
      <div class="panel-body" id="zombie-body"><div class="empty">Loading...</div></div>
    </div>

  </div>

</main>

<script>
const fmt = {
  grt: v => v == null ? '—' : Number(v).toLocaleString('en', {maximumFractionDigits:0}),
  usd: v => v == null ? '—' : '$' + Math.abs(v).toLocaleString('en', {maximumFractionDigits:0}),
  pct: v => v == null ? '—' : v.toFixed(1) + '%',
  ratio: v => v == null ? '—' : Number(v).toFixed(3),
  short: h => h || '—',
};

function barColor(pct) {
  if (pct > 80) return 'red';
  if (pct > 60) return 'amber';
  return 'green';
}

function ratioClass(r) {
  if (r == null) return '';
  if (r >= 0.1) return 'val-green';
  if (r >= 0.02) return 'val-amber';
  return 'val-red';
}

function setBar(id, valId, pct, label) {
  const fill = document.getElementById(id);
  const val  = document.getElementById(valId);
  if (!fill || !val) return;
  const p = Math.min(pct || 0, 100);
  fill.style.width = p + '%';
  fill.className = 'bar-fill ' + barColor(p);
  val.textContent = label || fmt.pct(p);
}

let charts = {};

function initCharts() {
  Chart.defaults.color = '#7d6035';
  Chart.defaults.borderColor = '#3a2709';
  Chart.defaults.font.family = "'JetBrains Mono', monospace";
  Chart.defaults.font.size = 11;

  const ratioCtx = document.getElementById('chart-ratio').getContext('2d');
  charts.ratio = new Chart(ratioCtx, {
    type: 'bar',
    data: { labels: [], datasets: [{ data: [], backgroundColor: [], borderRadius: 2, borderSkipped: false }] },
    options: {
      indexAxis: 'y',
      plugins: { legend: { display: false } },
      scales: {
        x: { grid: { color: '#3a2709' }, ticks: { color: '#7d6035' } },
        y: { grid: { display: false }, ticks: { color: '#c4922a', font: { size: 11 } } }
      },
      animation: { duration: 600 },
    }
  });

  const syncCtx = document.getElementById('chart-sync').getContext('2d');
  charts.sync = new Chart(syncCtx, {
    type: 'bar',
    data: { labels: [], datasets: [{ data: [], backgroundColor: [], borderRadius: 2, borderSkipped: false, label: 'Sync %' }] },
    options: {
      indexAxis: 'y',
      plugins: { legend: { display: false } },
      scales: {
        x: { min: 0, max: 100, grid: { color: '#3a2709' }, ticks: { color: '#7d6035', callback: v => v + '%' } },
        y: { grid: { display: false }, ticks: { color: '#c4922a', font: { size: 11 } } }
      },
      animation: { duration: 600 },
    }
  });
}

function renderAllocations(allocs) {
  const body = document.getElementById('alloc-body');
  document.getElementById('alloc-count').textContent = allocs.length + ' active';
  if (!allocs.length) {
    body.innerHTML = '<tr><td colspan="9" class="empty">No active allocations</td></tr>';
    return;
  }

  body.innerHTML = allocs.map(a => {
    const syncPct = a.sync_pct ?? 0;
    const synced = a.synced;
    const syncColor = synced ? 'green' : syncPct > 50 ? 'amber' : 'red';
    const syncLabel = synced ? '<span class="val-green">SYNCED</span>' : `<span class="val-${syncColor}">${fmt.pct(syncPct)}</span>`;
    const ratioStr = a.ratio != null ? `<span class="${ratioClass(a.ratio)}">${fmt.ratio(a.ratio)}</span>` : '—';
    const denied = a.denied_at && a.denied_at > 0;
    const network = a.network ? `<span class="val-dim">${a.network}</span>` : '';

    return `<tr>
      <td><span class="hash">${a.short_hash}</span></td>
      <td class="val">${fmt.grt(a.allocated_grt)}</td>
      <td class="val">${fmt.grt(a.signal_grt)}</td>
      <td>${ratioStr}</td>
      <td class="val">${fmt.pct(a.our_share_pct)}</td>
      <td class="val-green">${fmt.grt(a.est_grt_month)}</td>
      <td class="val-amber">${fmt.usd(a.est_usd_month)}</td>
      <td>
        <div class="bar-wrap">
          <div class="bar" style="min-width:50px"><div class="bar-fill ${syncColor}" style="width:${Math.min(syncPct,100)}%"></div></div>
          <div class="bar-label">${synced ? '<span style="color:var(--green)">✓</span>' : fmt.pct(syncPct)}</div>
        </div>
      </td>
      <td>${network}</td>
    </tr>`;
  }).join('');

  // Update ratio chart
  const sorted = [...allocs].sort((a,b) => (b.ratio||0) - (a.ratio||0));
  charts.ratio.data.labels = sorted.map(a => a.short_hash);
  charts.ratio.data.datasets[0].data = sorted.map(a => a.ratio || 0);
  charts.ratio.data.datasets[0].backgroundColor = sorted.map(a => {
    const r = a.ratio || 0;
    if (r >= 0.1) return 'rgba(126,184,126,.75)';
    if (r >= 0.02) return 'rgba(232,146,42,.75)';
    return 'rgba(204,68,34,.75)';
  });
  charts.ratio.update();

  // Update sync chart
  const syncing = allocs.filter(a => !a.synced);
  charts.sync.data.labels = syncing.map(a => a.short_hash);
  charts.sync.data.datasets[0].data = syncing.map(a => a.sync_pct || 0);
  charts.sync.data.datasets[0].backgroundColor = syncing.map(a => {
    const p = a.sync_pct || 0;
    if (p > 80) return 'rgba(126,184,126,.75)';
    if (p > 40) return 'rgba(196,146,42,.75)';
    return 'rgba(204,68,34,.75)';
  });
  charts.sync.update();
}

function renderServer(srv) {
  if (!srv || srv.error) {
    document.getElementById('container-list').innerHTML =
      '<div class="val-dim" style="font-size:10px">' + (srv?.error || 'Unavailable') + '</div>';
    return;
  }

  document.getElementById('srv-host').textContent = srv.host || '';
  document.getElementById('uptime-val').textContent = srv.uptime || '—';

  if (srv.load) {
    const lp = srv.load_pct ?? 0;
    setBar('cpu-bar', 'cpu-val', lp, `${lp.toFixed(0)}%  (${srv.load['1m'].toFixed(2)})`);
    document.getElementById('load-val').textContent =
      `${srv.load['1m'].toFixed(2)}  ${srv.load['5m'].toFixed(2)}  ${srv.load['15m'].toFixed(2)}  (${srv.nproc} cores)`;
  }
  if (srv.mem) {
    setBar('ram-bar', 'ram-val',
      srv.mem.pct,
      `${srv.mem.used_gb.toFixed(1)}/${srv.mem.total_gb.toFixed(0)} GB`
    );
  }
  if (srv.disk) {
    setBar('disk-bar', 'disk-val',
      srv.disk.pct,
      `${srv.disk.used_gb.toFixed(0)}/${srv.disk.total_gb.toFixed(0)} GB`
    );
  }

  const containers = srv.containers || [];
  if (!containers.length) {
    document.getElementById('container-list').innerHTML =
      '<div class="val-dim" style="font-size:10px">No containers found</div>';
    return;
  }

  // Key containers we care about
  const priority = ['indexer-agent','indexer-service','indexer-tap-agent',
                    'index-node-0','query-node-0','prometheus','grafana','subgraph-radio'];

  const sorted = [...containers].sort((a,b) => {
    const ai = priority.indexOf(a.name), bi = priority.indexOf(b.name);
    if (ai === -1 && bi === -1) return a.name.localeCompare(b.name);
    if (ai === -1) return 1; if (bi === -1) return -1;
    return ai - bi;
  });

  document.getElementById('container-list').innerHTML = sorted.map(c => {
    const running = (c.status || '').toLowerCase().startsWith('up');
    const dotClass = running ? 'dot-green' : 'dot-red';
    const nameColor = running ? '' : 'style="color:var(--red)"';
    return `<div class="container-row">
      <div class="dot ${dotClass}"></div>
      <div class="ct-name" ${nameColor}>${c.name}</div>
      <div class="ct-status">${c.status || '?'}</div>
    </div>`;
  }).join('');
}

function renderThaws(thaws) {
  const body = document.getElementById('thaw-body');
  if (!thaws.length) {
    body.innerHTML = '<div class="empty">No pending thaws</div>';
    return;
  }
  body.innerHTML = thaws.map(t => {
    const color = t.mature ? 'val-green' : t.hours_remaining < 24 ? 'val-amber' : 'val-dim';
    const status = t.mature
      ? '<span class="val-gold">★ WITHDRAW NOW</span>'
      : t.hours_remaining < 24
        ? `<span class="val-amber">${t.hours_remaining.toFixed(1)}h</span>`
        : `<span class="val-dim">${(t.hours_remaining/24).toFixed(1)}d</span>`;
    return `<div class="thaw-row">
      <div>
        <div style="color:var(--text);font-size:10px">${fmt.grt(t.shares_grt)} GRT</div>
        <div style="color:var(--dim);font-size:9px;margin-top:2px">${t.data_service}</div>
      </div>
      <div style="text-align:right">
        ${status}
        <div style="color:var(--dim);font-size:9px;margin-top:2px">${t.thawing_until_iso}</div>
      </div>
    </div>`;
  }).join('');
}

function renderActions(actions) {
  const body = document.getElementById('actions-body');
  if (!actions.length) {
    body.innerHTML = '<tr><td colspan="5" class="empty">No pending actions</td></tr>';
    return;
  }
  body.innerHTML = actions.map(a => {
    const statusColor = a.status === 'approved' ? 'val-green' : a.status === 'queued' ? 'val-amber' : 'val-dim';
    return `<tr>
      <td class="val-dim">#${a.id}</td>
      <td class="val">${a.action_type}</td>
      <td class="hash">${a.deployment_id ? a.deployment_id.substring(0,12) + '…' : '—'}</td>
      <td class="val">${a.amount ? fmt.grt(a.amount) : '—'}</td>
      <td class="${statusColor}">${a.status}</td>
    </tr>`;
  }).join('');
}

function renderZombies(zombies) {
  const body = document.getElementById('zombie-body');
  if (!zombies.length) {
    body.innerHTML = '<div class="empty" style="color:var(--green);font-size:13px">✓ No zombies detected</div>';
    return;
  }
  body.innerHTML = zombies.map(z => {
    const syncColor = z.synced ? 'val-green' : z.sync_pct > 50 ? 'val-amber' : 'val-dim';
    return `<div style="display:flex;justify-content:space-between;padding:5px 0;border-bottom:1px solid rgba(255,255,255,.04);font-size:10px">
      <div>
        <span class="hash">${z.short_hash}</span>
        <span class="val-dim" style="font-size:9px;margin-left:6px">${z.network}</span>
      </div>
      <div class="${syncColor}">${z.synced ? 'synced' : fmt.pct(z.sync_pct)}</div>
    </div>`;
  }).join('');
}

function render(data) {
  // Header
  document.getElementById('hdr-address').textContent = data.indexer?.address || '—';
  document.getElementById('grt-price').textContent = 'GRT $' + (data.grt_price_usd || 0).toFixed(5);
  document.getElementById('hdr-net').textContent = data.network || '—';

  // Stake cards
  const s = data.indexer?.stake;
  if (s) {
    document.getElementById('s-own').textContent   = fmt.grt(s.own_grt);
    document.getElementById('s-del').textContent   = fmt.grt(s.delegated_grt);
    document.getElementById('s-cap').textContent   = fmt.grt(s.capacity_grt);
    document.getElementById('s-alloc').textContent = fmt.grt(s.allocated_grt);
    document.getElementById('s-util').textContent  = fmt.pct(s.utilisation_pct) + ' utilised';
    const free = s.available_grt;
    document.getElementById('s-free').textContent     = fmt.grt(free);
    document.getElementById('s-free-pct').textContent = fmt.pct(100 - s.utilisation_pct) + ' free';
    const freeEl = document.getElementById('s-free');
    freeEl.className = 'card-value ' + (free < 5000 ? 'red' : free < 20000 ? 'amber' : 'green');

  }

  // Economics
  const e = data.economics;
  if (e) {
    document.getElementById('e-rewards').textContent    = fmt.grt(e.est_grt_month);
    document.getElementById('e-rewards-usd').textContent = '$' + (e.est_usd_month || 0).toFixed(0) + ' USD';
    document.getElementById('e-costs').textContent       = '$' + (e.monthly_costs_usd || 0).toFixed(0);
    const pnl = e.net_usd_month || 0;
    const pnlEl = document.getElementById('e-pnl');
    pnlEl.textContent = (pnl >= 0 ? '+' : '') + '$' + Math.abs(pnl).toFixed(0);
    pnlEl.className = 'card-value ' + (pnl >= 0 ? 'green' : 'red');
    document.getElementById('e-pnl-grt').textContent = 'USD/mo  (' + fmt.grt(e.est_grt_month) + ' GRT gross)';
  }

  renderAllocations(data.allocations || []);
  renderThaws(data.thaws || []);
  renderActions(data.actions || []);
  renderZombies(data.zombies || []);
}

let lastUpdate = null;

function updateTimestamp() {
  if (!lastUpdate) return;
  const ago = Math.round((Date.now() - lastUpdate) / 1000);
  document.getElementById('ts').textContent = `↻ ${ago}s ago`;
}

async function fetchData() {
  const icon = document.getElementById('refresh-icon');
  icon.innerHTML = '<span class="spin">⟳</span>';
  try {
    const res = await fetch('/api/data');
    if (!res.ok) throw new Error('HTTP ' + res.status);
    const data = await res.json();
    render(data);
    lastUpdate = Date.now();
    document.getElementById('error-banner').style.display = 'none';
  } catch(e) {
    const banner = document.getElementById('error-banner');
    banner.textContent = '⚠ Failed to fetch data: ' + e.message;
    banner.style.display = 'block';
  } finally {
    icon.innerHTML = '';
  }
}

async function fetchServer() {
  try {
    const res = await fetch('/api/server');
    if (!res.ok) return;
    const srv = await res.json();
    renderServer(srv);
  } catch(e) {
    renderServer({ error: e.message });
  }
}

// Boot
initCharts();
fetchData();
fetchServer();
setInterval(fetchData, 30000);
setInterval(fetchServer, 60000);
setInterval(updateTimestamp, 1000);
</script>

</body>
</html>
"##;
