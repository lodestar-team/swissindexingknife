use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::Value;

pub struct IpfsClient {
    http: Client,
    base_url: String,
}

impl IpfsClient {
    pub fn new(base_url: String) -> Self {
        Self {
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap(),
            base_url,
        }
    }

    /// Fetch a raw file from IPFS. Returns None if not found / timeout.
    pub async fn cat(&self, ipfs_hash: &str) -> Option<Vec<u8>> {
        let url = format!("{}/cat?arg={}&length=65536", self.base_url, ipfs_hash);
        self.http.post(&url).send().await.ok()?
            .bytes().await.ok()
            .map(|b| b.to_vec())
    }

    /// Check if an IPFS hash is accessible (returns true if reachable within timeout).
    pub async fn is_available(&self, ipfs_hash: &str) -> bool {
        let url = format!("{}/cat?arg={}&length=100", self.base_url, ipfs_hash);
        match self.http.post(&url).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    /// Fetch and parse a subgraph manifest, extracting graft info if present.
    pub async fn graft_info(&self, ipfs_hash: &str) -> Result<Option<GraftInfo>> {
        let bytes = self.cat(ipfs_hash).await
            .context("IPFS manifest not accessible")?;

        let content = String::from_utf8_lossy(&bytes);

        // Try JSON manifest first
        if let Ok(json) = serde_json::from_str::<Value>(&content) {
            return Ok(extract_graft_from_json(&json));
        }

        // Fall back to YAML parsing (basic string search)
        Ok(extract_graft_from_yaml(&content))
    }

    /// Fetch and parse a manifest — returns accessibility, indexed chain, and graft info in one call.
    /// Uses the same `specVersion:` gate as `sik verify` to avoid IPFS false positives.
    pub async fn manifest_info(&self, ipfs_hash: &str) -> ManifestInfo {
        let bytes = match self.cat(ipfs_hash).await {
            Some(b) => b,
            None => return ManifestInfo::inaccessible(),
        };
        let content = String::from_utf8_lossy(&bytes);

        // Same gate as sik verify — must have specVersion to be a real manifest
        if !content.contains("specVersion:") {
            return ManifestInfo::inaccessible();
        }

        let graft = if let Ok(json) = serde_json::from_str::<Value>(&content) {
            extract_graft_from_json(&json)
        } else {
            extract_graft_from_yaml(&content)
        };

        ManifestInfo {
            accessible: true,
            network: extract_network_from_yaml(&content),
            graft,
        }
    }
}

fn extract_graft_from_json(v: &Value) -> Option<GraftInfo> {
    let base = v.get("graft")?.get("base")?.as_str()?;
    let block = v["graft"]["block"].as_u64().unwrap_or(0);
    Some(GraftInfo {
        base_hash: base.to_string(),
        block,
    })
}

fn extract_graft_from_yaml(content: &str) -> Option<GraftInfo> {
    // Simple line-by-line YAML parsing for the graft section
    let mut in_graft = false;
    let mut base_hash = None;
    let mut block = 0u64;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "graft:" {
            in_graft = true;
            continue;
        }
        if in_graft {
            if trimmed.starts_with("base:") {
                base_hash = Some(trimmed["base:".len()..].trim().trim_matches('"').to_string());
            } else if trimmed.starts_with("block:") {
                block = trimmed["block:".len()..].trim().parse().unwrap_or(0);
            } else if !trimmed.is_empty() && !trimmed.starts_with(' ') && !trimmed.starts_with('#') {
                // Left the graft section
                in_graft = false;
            }
        }
    }

    base_hash.map(|h| GraftInfo { base_hash: h, block })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GraftInfo {
    /// IPFS hash of the base deployment
    pub base_hash: String,
    /// Block number at which the graft occurs
    pub block: u64,
}

fn extract_network_from_yaml(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("network:") {
            let val = trimmed["network:".len()..].trim().trim_matches('"').to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ManifestInfo {
    pub accessible: bool,
    pub network: Option<String>,
    pub graft: Option<GraftInfo>,
}

impl ManifestInfo {
    fn inaccessible() -> Self {
        Self { accessible: false, network: None, graft: None }
    }
}
