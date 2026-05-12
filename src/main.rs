use anyhow::Result;
use clap::{Parser, Subcommand};

mod client;
mod commands;
mod config;
mod executor;
mod output;
mod types;

use config::Config;

#[derive(Parser)]
#[command(
    name = "sik",
    about = "Swiss Indexing Knife — Graph Protocol indexer operations CLI",
    version,
    long_about = "AI-centric CLI for Graph Protocol indexer operations.\n\
    Embeds all known API quirks so you never have to remember them.\n\
    Use `sik context` for a single-call situational awareness dump."
)]
struct Cli {
    /// Output JSON (machine-readable; primary mode for AI agents)
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Print an example config to stdout
    Init,

    /// Full indexer status: stake, allocations, thaws, containers, zombies
    Status,

    /// Allocation efficiency table with signal/stake ratios and estimated rewards
    Allocations,

    /// Discover allocation opportunities ranked by signal/stake ratio
    Discover {
        /// Filter by chain name (e.g. base, arbitrum, gnosis) — heuristic
        #[arg(long)]
        chain: Option<String>,
        /// Number of results to show
        #[arg(long, default_value = "20")]
        top: usize,
        /// Minimum signal/stake ratio to include
        #[arg(long, default_value = "0.01")]
        min_ratio: f64,
        /// Proposed allocation amount in GRT (for estimating returns)
        #[arg(long, default_value = "100000")]
        alloc: f64,
    },

    /// Pre-flight check for a deployment before allocating
    Verify {
        /// IPFS hash of the deployment (Qm...)
        deployment: String,
    },

    /// Show graft base sync progress for a deployment with a graft dependency
    GraftStatus {
        /// IPFS hash of the DEPENDENT (target) deployment, not the base
        deployment: String,
        /// Poll every 60 seconds
        #[arg(long)]
        watch: bool,
    },

    /// Indexing rule management
    Rule {
        #[command(subcommand)]
        cmd: RuleCmd,
    },

    /// Thaw request status and withdrawal
    Thaw {
        #[command(subcommand)]
        cmd: Option<ThawCmd>,
    },

    /// Month-to-date P&L estimate
    Pnl,

    /// Agent action queue
    Actions {
        /// Filter by status: queued, approved, failed, pending
        #[arg(long)]
        status: Option<String>,
        #[command(subcommand)]
        cmd: Option<ActionsCmd>,
    },

    /// Present a Proof of Indexing (POI) for an allocation
    PresentPoi {
        /// IPFS hash of the deployment (Qm...)
        deployment: String,
        /// Allocation ID (0x address — from `sik allocations`)
        allocation_id: String,
        /// POI hash (omit for automatic/zero POI)
        #[arg(long)]
        poi: Option<String>,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },

    /// Bounty workflow: check status or present POI to claim rewards
    Bounty {
        #[command(subcommand)]
        cmd: BountyCmd,
    },

    /// Full context dump for AI agents (JSON only).
    /// Aggregates all state + recommendations in one call.
    Context,

    /// Resize an active allocation to a new GRT amount
    Resize {
        /// IPFS hash of the deployment (Qm...)
        deployment: String,
        /// New allocation amount in GRT
        amount: f64,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },

    /// Launch the hacker-style web dashboard
    Serve {
        /// Port to listen on
        #[arg(long, default_value = "7777")]
        port: u16,
        /// Open browser automatically
        #[arg(long)]
        open: bool,
    },
}

#[derive(Subcommand)]
enum RuleCmd {
    /// List all indexing rules vs on-chain allocations
    List,
    /// Set an indexing rule
    Set {
        /// IPFS hash of the deployment
        deployment: String,
        /// always | never
        basis: String,
        /// Allocation amount in GRT (not wei — conversion is automatic)
        #[arg(long)]
        amount: Option<f64>,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum BountyCmd {
    /// Check bounty status: allocation open? synced? POI presented?
    Status {
        /// IPFS hash of the deployment
        deployment: String,
    },
    /// Present POI and prepare to claim rewards (auto-resolves allocation ID)
    Claim {
        /// IPFS hash of the deployment
        deployment: String,
        /// POI hash (omit for automatic/zero POI)
        #[arg(long)]
        poi: Option<String>,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum ThawCmd {
    /// Withdraw matured thaws (requires indexer cold wallet key)
    Withdraw {
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
        /// Indexer cold wallet private key (alternative: INDEXER_COLD_WALLET_KEY env var)
        #[arg(long)]
        cold_wallet_key: Option<String>,
        /// Arbitrum RPC URL (alternative: ARB_RPC_URL env var; defaults to public endpoint)
        #[arg(long)]
        rpc_url: Option<String>,
    },
}

#[derive(Subcommand)]
enum ActionsCmd {
    /// Approve a queued action so the agent will execute it
    Approve {
        /// Action ID
        id: i64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Init doesn't need config
    if let Commands::Init = &cli.command {
        println!("{}", config::example_config());
        eprintln!("Write the above to ~/.lodestar/config.toml and fill in your values.");
        return Ok(());
    }

    let cfg = Config::load()?;

    match cli.command {
        Commands::Init => unreachable!(),

        Commands::Status => {
            commands::status::run(&cfg, cli.json).await?;
        }

        Commands::Allocations => {
            commands::allocations::run(&cfg, cli.json).await?;
        }

        Commands::Discover { chain, top, min_ratio, alloc } => {
            commands::discover::run(&cfg, chain, top, min_ratio, alloc, cli.json).await?;
        }

        Commands::Verify { deployment } => {
            commands::verify::run(&cfg, &deployment, cli.json).await?;
        }

        Commands::GraftStatus { deployment, watch } => {
            commands::graft::run(&cfg, &deployment, watch, cli.json).await?;
        }

        Commands::Rule { cmd } => match cmd {
            RuleCmd::List => commands::rule::list(&cfg, cli.json).await?,
            RuleCmd::Set { deployment, basis, amount, yes } => {
                commands::rule::set(&cfg, &deployment, &basis, amount, yes).await?;
            }
        },

        Commands::Thaw { cmd } => {
            match cmd {
                None => commands::thaw::run(&cfg, cli.json).await?,
                Some(ThawCmd::Withdraw { yes, cold_wallet_key, rpc_url }) => {
                    commands::thaw::withdraw(&cfg, yes, cold_wallet_key, rpc_url).await?;
                }
            }
        }

        Commands::Pnl => {
            commands::pnl::run(&cfg, cli.json).await?;
        }

        Commands::Actions { status, cmd } => {
            match cmd {
                Some(ActionsCmd::Approve { id }) => {
                    commands::actions::approve(&cfg, id).await?;
                }
                None => {
                    commands::actions::list(&cfg, status.as_deref(), cli.json).await?;
                }
            }
        }

        Commands::PresentPoi { deployment, allocation_id, poi, yes } => {
            commands::present_poi::run(&cfg, &deployment, &allocation_id, poi.as_deref(), yes).await?;
        }

        Commands::Bounty { cmd } => match cmd {
            BountyCmd::Status { deployment } => {
                commands::bounty::status(&cfg, &deployment, cli.json).await?;
            }
            BountyCmd::Claim { deployment, poi, yes } => {
                commands::bounty::claim(&cfg, &deployment, poi.as_deref(), yes).await?;
            }
        },

        Commands::Resize { deployment, amount, yes } => {
            commands::resize::run(&cfg, &deployment, amount, yes).await?;
        }

        Commands::Context => {
            commands::context::run(&cfg).await?;
        }

        Commands::Serve { port, open } => {
            commands::serve::run(cfg, port, open).await?;
        }
    }

    Ok(())
}
