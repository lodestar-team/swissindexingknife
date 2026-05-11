/// RemoteExecutor: abstracts over SSH+docker-exec, local docker-exec, and direct HTTP.
/// Every API client routes through here so callers never deal with ssh / docker plumbing.
use anyhow::{Context, Result};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::config::{ApiConfig, ServerConfig, DockerConfig};

#[derive(Clone)]
pub enum AccessMethod {
    /// SSH into server, then `docker exec -i <container> <cmd>`
    SshDocker {
        host: String,
        user: String,
        key_path: String,
    },
    /// Local `docker exec -i <container> <cmd>`
    LocalDocker,
    /// Direct HTTP — ports exposed to host (non-standard setups)
    HostPort { base_url: String },
}

#[derive(Clone)]
pub struct RemoteExecutor {
    pub method: AccessMethod,
    pub agent_container: String,
    pub query_node_host: String,
    pub mgmt_port: u16,
    pub status_port: u16,
}

impl RemoteExecutor {
    pub fn from_config(server: &ServerConfig, docker: &DockerConfig, api: &ApiConfig) -> Self {
        let method = match api.access_method.as_str() {
            "local_docker" => AccessMethod::LocalDocker,
            "host_port" => AccessMethod::HostPort {
                base_url: format!("http://localhost:{}", api.management_api_port),
            },
            _ => AccessMethod::SshDocker {
                host: server.host.clone(),
                user: server.user.clone(),
                key_path: shellexpand::tilde(&server.ssh_key).to_string(),
            },
        };
        Self {
            method,
            agent_container: docker.indexer_agent_container.clone(),
            query_node_host: docker.graph_node_query_container.clone(),
            mgmt_port: api.management_api_port,
            status_port: api.graph_node_status_port,
        }
    }

    /// Run a command inside the indexer-agent container, piping `stdin_data` to stdin.
    /// Returns stdout bytes.
    pub async fn exec_in_agent(&self, cmd: &str, stdin_data: &[u8]) -> Result<Vec<u8>> {
        self.exec_in_container(&self.agent_container, cmd, stdin_data).await
    }

    async fn exec_in_container(&self, container: &str, cmd: &str, stdin_data: &[u8]) -> Result<Vec<u8>> {
        match &self.method {
            AccessMethod::SshDocker { host, user, key_path } => {
                let remote_cmd = format!("docker exec -i {} {}", container, cmd);
                let mut child = Command::new("ssh")
                    .args([
                        "-i", key_path,
                        "-o", "BatchMode=yes",
                        "-o", "StrictHostKeyChecking=accept-new",
                        "-o", "ConnectTimeout=10",
                        &format!("{}@{}", user, host),
                        &remote_cmd,
                    ])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .context("failed to spawn ssh — is ssh installed and key accessible?")?;

                if let Some(mut stdin) = child.stdin.take() {
                    stdin.write_all(stdin_data).await?;
                }
                let out = child.wait_with_output().await?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    anyhow::bail!("SSH exec failed: {}", stderr.trim());
                }
                Ok(out.stdout)
            }

            AccessMethod::LocalDocker => {
                let docker_cmd = format!("docker exec -i {} {}", container, cmd);
                let parts: Vec<&str> = docker_cmd.split_whitespace().collect();
                let mut child = Command::new(parts[0])
                    .args(&parts[1..])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .context("failed to spawn docker exec")?;

                if let Some(mut stdin) = child.stdin.take() {
                    stdin.write_all(stdin_data).await?;
                }
                let out = child.wait_with_output().await?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    anyhow::bail!("docker exec failed: {}", stderr.trim());
                }
                Ok(out.stdout)
            }

            AccessMethod::HostPort { base_url } => {
                // Direct HTTP — no docker exec needed
                let client = reqwest::Client::new();
                let resp = client
                    .post(base_url)
                    .header("Content-Type", "application/json")
                    .body(stdin_data.to_vec())
                    .send()
                    .await
                    .context("HTTP request failed")?;
                Ok(resp.bytes().await?.to_vec())
            }
        }
    }

    /// GraphQL POST to the management API (POST /).
    /// NOTE: management API is at POST / not POST /graphql
    pub async fn management_graphql(&self, query: &str) -> Result<serde_json::Value> {
        let body = serde_json::json!({ "query": query });
        let body_str = serde_json::to_string(&body)?;

        let cmd = match &self.method {
            AccessMethod::HostPort { base_url } => {
                // handled differently below
                let _ = base_url;
                String::new()
            }
            _ => format!(
                "curl -s -X POST -H 'Content-Type: application/json' -d @- http://localhost:{}/",
                self.mgmt_port
            ),
        };

        let raw = if matches!(&self.method, AccessMethod::HostPort { .. }) {
            self.exec_in_agent(&cmd, body_str.as_bytes()).await?
        } else {
            self.exec_in_agent(&cmd, body_str.as_bytes()).await?
        };

        parse_graphql_response(&raw)
    }

    /// GraphQL POST to the graph-node status API (port 8030) via the agent container.
    pub async fn graph_node_graphql(&self, query: &str) -> Result<serde_json::Value> {
        let body = serde_json::json!({ "query": query });
        let body_str = serde_json::to_string(&body)?;

        let cmd = match &self.method {
            AccessMethod::HostPort { .. } => format!(
                "curl -s -X POST -H 'Content-Type: application/json' -d @- http://{}:{}/graphql",
                self.query_node_host, self.status_port
            ),
            _ => format!(
                "curl -s -X POST -H 'Content-Type: application/json' -d @- http://{}:{}/graphql",
                self.query_node_host, self.status_port
            ),
        };

        let raw = self.exec_in_agent(&cmd, body_str.as_bytes()).await?;
        parse_graphql_response(&raw)
    }
}

fn parse_graphql_response(raw: &[u8]) -> Result<serde_json::Value> {
    let s = String::from_utf8_lossy(raw);
    let value: serde_json::Value = serde_json::from_str(&s)
        .with_context(|| format!("invalid JSON response: {}", &s[..s.len().min(200)]))?;

    if let Some(errors) = value.get("errors") {
        if !errors.is_null() {
            if let Some(arr) = errors.as_array() {
                if !arr.is_empty() {
                    let msg = arr
                        .iter()
                        .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                        .collect::<Vec<_>>()
                        .join("; ");
                    anyhow::bail!("GraphQL error: {}", msg);
                }
            }
        }
    }

    Ok(value["data"].clone())
}

/// Expand a leading `~/` to the user's home directory.
pub fn shellexpand_tilde(s: &str) -> String {
    if s.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return format!("{}{}", home.display(), &s[1..]);
        }
    }
    s.to_string()
}

// Private alias used inside this module
mod shellexpand {
    pub fn tilde(s: &str) -> std::borrow::Cow<str> {
        if s.starts_with("~/") {
            if let Some(home) = dirs::home_dir() {
                return std::borrow::Cow::Owned(format!("{}{}", home.display(), &s[1..]));
            }
        }
        std::borrow::Cow::Borrowed(s)
    }
}
