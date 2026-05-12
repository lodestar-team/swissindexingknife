# Swiss Indexing Knife (`sik`)

A CLI for Graph Protocol indexer operations. Wraps all known API quirks so you never have to remember them. Designed to be used directly by humans and AI agents alike.

<img width="3024" height="1582" alt="image" src="https://github.com/user-attachments/assets/b1965d42-5410-42bf-9426-b2271ca7a6cc" />

## Install

```bash
git clone https://github.com/lodestar-team/swissindexingknife
cd swissindexingknife
cargo build --release
# binary at: ./target/release/sik
# optionally: cp target/release/sik /usr/local/bin/sik
```

Requires: Rust 1.75+, `ssh` in PATH, a running Graph Protocol indexer stack.

## Setup

```bash
sik init > ~/.lodestar/config.toml
# edit ~/.lodestar/config.toml with your values
```

Example config:

```toml
[indexer]
address = "0xYOUR_INDEXER_ADDRESS"
operator_address = "0xYOUR_OPERATOR_ADDRESS"

[server]
host = "YOUR_SERVER_IP"
user = "root"
ssh_key = "~/.ssh/id_ed25519"

[docker]
indexer_agent_container = "indexer-agent"
graph_node_index_container = "index-node-0"
graph_node_query_container = "query-node-0"

[api]
access_method = "ssh_docker"   # ssh_docker | local_docker | host_port
management_api_port = 8000
graph_node_admin_port = 8020
graph_node_status_port = 8030

[network]
subgraph_url = "https://gateway-arbitrum.thegraph.com/api/YOUR_API_KEY/subgraphs/id/C8RdPboBCkPxRthYiFHnX6BxQLeckY5FeDzJfHNsU6x1"
ipfs_url = "https://api.thegraph.com/ipfs/api/v0"
protocol_network = "eip155:42161"

[economics]
monthly_costs_usd = 368.0
delegation_cut_bps = 1000   # 10%

[grt_price]
# Uncomment to override CoinGecko price:
# manual_price_usd = 0.025
```

### Access method

| Value | When to use |
|---|---|
| `ssh_docker` | sik runs on your laptop, indexer is on a remote VPS (default) |
| `local_docker` | sik runs on the indexer server itself |
| `host_port` | management API port is exposed to host (non-standard) |

## Commands

```bash
sik status                              # full human-readable status
sik status --json                       # machine-readable (AI agent mode)

sik allocations                         # efficiency table: signal/stake ratios, est rewards
sik discover --top 20 --alloc 100000    # find allocation opportunities
sik verify <Qm...>                      # pre-flight check before allocating
sik graft-status <Qm...>               # sync progress for a grafted deployment
sik graft-status <Qm...> --watch        # poll every 60s

sik thaw                                # list thaw requests + maturity
sik actions                             # pending agent actions
sik actions --status approved           # filter by status
sik actions approve <id>                # approve a queued action
sik pnl                                 # month-to-date P&L estimate

sik rule list                           # indexing rules vs on-chain state
sik rule set <Qm...> always --amount 100000   # allocate (GRT, not wei)
sik rule set <Qm...> never              # stop allocating

sik context                             # AI situational-awareness dump (JSON)
sik serve                               # live web dashboard on http://localhost:7777
sik serve --port 8888 --open            # custom port + auto-open browser

sik init                                # print example config
```

All commands support `--json` for structured output.

## Live Dashboard

`sik serve` launches a local web dashboard with live data:

- Stake / delegation / capacity / utilisation
- Estimated rewards and P&L
- Active allocations with signal/stake ratios and sync progress
- Server metrics: CPU, RAM, disk, load average, uptime
- Container health for all docker services
- Pending actions and thaw requests
- Zombie deployments (syncing with no allocation)
- Signal/stake ratio and sync progress charts

Data refreshes every 30 seconds automatically.

## AI Agent Usage

Start every session with:

```bash
sik context --json
```

Returns a single JSON payload with full indexer state + a `recommendations[]` array identifying what needs attention. Designed to give an AI agent complete situational awareness in one call.

## API Quirks (all handled internally)

These quirks are baked into `sik`. You don't need to remember them unless you're debugging `sik` itself.

| Quirk | Notes |
|---|---|
| Management API at `POST /` not `/graphql` | Standard GraphQL clients won't work |
| Management API ports not exposed to host | Must use `docker exec indexer-agent curl` |
| `AllocationFilter.status` is a quoted String, not an enum | `"active"` not `active` |
| `ActionFilter.status` is singular, no multi-status filter | Fetch all, filter client-side |
| `subgraphDeployment` in allocations is a flat IPFS hash string | Not a nested object |
| `thawingUntil` is a BigInt returned as string | `as_str().parse()` not `as_i64()` |
| `setIndexingRule` amount in wei; `queueActions` amount in GRT | Different units for the same concept |
| `closeAllocation(blockNumber:)` must be Int not String | Causes silent failure otherwise |
| `closeAllocation` returns `{}` on success | Must verify via separate allocations query |
| Queued actions don't execute — must be `approved` | Use `updateActions` mutation to approve |
| graph-node containers lack curl | Route via indexer-agent container |
| L2 network subgraph lacks `issuancePerBlock` | Monthly issuance is hardcoded |

## License

MIT
