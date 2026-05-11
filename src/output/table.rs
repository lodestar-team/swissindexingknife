use colored::Colorize;

/// Print a section header.
pub fn section(title: &str) {
    println!("\n{}", title.bold().underline());
}

/// Print a key/value line with optional colour based on value class.
pub fn kv(key: &str, value: &str) {
    println!("  {:<24} {}", key, value);
}

pub fn kv_warn(key: &str, value: &str) {
    println!("  {:<24} {}", key, value.yellow().bold());
}

pub fn kv_ok(key: &str, value: &str) {
    println!("  {:<24} {}", key, value.green());
}

pub fn kv_err(key: &str, value: &str) {
    println!("  {:<24} {}", key, value.red().bold());
}

pub fn warn(msg: &str) {
    eprintln!("{} {}", "WARN".yellow().bold(), msg);
}

pub fn ok(msg: &str) {
    println!("{} {}", "OK  ".green(), msg);
}

pub fn err(msg: &str) {
    eprintln!("{} {}", "ERR ".red().bold(), msg);
}

/// Print a horizontal rule.
pub fn rule() {
    println!("{}", "─".repeat(72).dimmed());
}

/// Truncate an IPFS hash for display: "QmVey...UzYR"
pub fn short_hash(h: &str) -> String {
    if h.len() > 14 {
        format!("{}...{}", &h[..8], &h[h.len()-4..])
    } else {
        h.to_string()
    }
}

/// Format ratio as coloured string.
pub fn fmt_ratio(r: f64) -> String {
    if r.is_infinite() {
        "∞".green().to_string()
    } else if r >= 0.1 {
        format!("{:.3}", r).green().to_string()
    } else if r >= 0.02 {
        format!("{:.3}", r).yellow().to_string()
    } else {
        format!("{:.3}", r).red().to_string()
    }
}

/// Format sync status.
pub fn fmt_sync(synced: bool, pct: f64) -> String {
    if synced {
        "synced".green().to_string()
    } else {
        format!("{:.1}%", pct).yellow().to_string()
    }
}

/// Format thaw maturity.
pub fn fmt_thaw(mature: bool, hours: f64) -> String {
    if mature {
        "MATURED — withdraw now".green().bold().to_string()
    } else if hours < 24.0 {
        format!("{:.1}h remaining", hours).yellow().to_string()
    } else {
        format!("{:.0}d {:.0}h remaining", hours / 24.0, hours % 24.0).normal().to_string()
    }
}
