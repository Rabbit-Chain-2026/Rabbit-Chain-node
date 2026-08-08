//! RabbitChain CLI - Blockchain node and client

#![allow(unused)]

use crate::commands::wallet::{WalletCommand, WalletScheme};
use anyhow::Result;
use clap::{Parser, Subcommand};
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use rabbitapi::rpc::{ComputeBackend, RpcConfig};
use rabbitapi::ApiConfig;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod commands;

/// RabbitChain CLI
#[derive(Parser)]
#[command(name = "rabbitchain")]
#[command(author = "RabbitChain Team")]
#[command(version = "0.1.0")]
#[command(about = "RabbitChain blockchain node and client", long_about = None)]
struct Cli {
    /// Log level
    #[arg(short, long, default_value = "info")]
    log_level: String,

    /// Data directory override
    #[arg(short, long)]
    data_dir: Option<String>,

    /// Network profile (mainnet|testnet|devnet|local)
    #[arg(long, default_value = "local")]
    network: String,

    /// JSON-RPC URL used by account/compute/block query commands
    #[arg(long)]
    rpc_url: Option<String>,

    /// Optional auth token used by outbound JSON-RPC client commands
    #[arg(long)]
    rpc_token: Option<String>,

    /// Optional node config file (JSON)
    #[arg(long)]
    config: Option<String>,

    /// Enable OpenTelemetry tracing export (OTLP)
    #[arg(long, default_value_t = false)]
    otel_enabled: bool,

    /// OTLP endpoint, e.g. http://127.0.0.1:4317
    #[arg(long, default_value = "http://127.0.0.1:4317")]
    otel_endpoint: String,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a full node
    Run {
        /// Enable mining
        #[arg(long)]
        mine: bool,

        /// Enable mining RPC for external miners without starting a local miner
        #[arg(long, default_value_t = false)]
        enable_mining_rpc: bool,

        /// Do not start the built-in local mining worker when mining RPC is enabled
        #[arg(long, default_value_t = false)]
        disable_local_miner: bool,

        /// Coinbase address
        #[arg(long)]
        coinbase: Option<String>,

        /// File containing one mining coinbase address per line
        #[arg(long)]
        coinbase_file: Option<String>,

        /// HTTP RPC port override (default follows --network)
        #[arg(long)]
        http_port: Option<u16>,

        /// WebSocket RPC port override (default follows --network)
        #[arg(long)]
        ws_port: Option<u16>,

        /// Compute persistent backend override (default follows --network)
        #[arg(long)]
        compute_backend: Option<String>,

        /// Compute database path override for rocksdb/redb
        #[arg(long)]
        compute_db_path: Option<String>,

        /// Optional chain id override (hex or decimal)
        #[arg(long)]
        chain_id: Option<String>,

        /// Optional network id override (decimal)
        #[arg(long)]
        network_id: Option<u64>,

        /// Optional coinbase override
        #[arg(long)]
        rpc_coinbase: Option<String>,

        /// Optional static RPC auth token (Bearer or x-rabbit-token)
        #[arg(long)]
        rpc_auth_token: Option<String>,

        /// RPC rate limit budget per client per minute, 0 to disable
        #[arg(long, default_value = "600")]
        rpc_rate_limit_per_minute: u32,

        /// Optional legacy override for rabbit_getWork target as a count of leading zero bytes (0..=32)
        #[arg(long)]
        mining_work_target_leading_rabbit_bytes: Option<usize>,

        /// P2P listen address
        #[arg(long, default_value = "0.0.0.0")]
        p2p_listen_addr: String,

        /// P2P listen port
        #[arg(long, default_value = "30303")]
        p2p_listen_port: u16,

        /// Disable direct TCP P2P transport, including enode bootnodes and TCP listener
        #[arg(long, default_value_t = false)]
        disable_p2p_tcp: bool,

        /// Disable WebSocket P2P transport, including ws/wss bootnodes and WebSocket listener
        #[arg(long, default_value_t = false)]
        disable_p2p_ws: bool,

        /// Optional P2P WebSocket listen address for Cloudflare/CDN proxying
        #[arg(long)]
        p2p_ws_listen_addr: Option<String>,

        /// Optional P2P WebSocket listen port; when omitted, WebSocket listener is disabled
        #[arg(long)]
        p2p_ws_listen_port: Option<u16>,

        /// Optional public WebSocket bootnode URL to print in logs, e.g. wss://boot.example.org/p2p
        #[arg(long)]
        p2p_ws_external_url: Option<String>,

        /// Bootnode endpoint, repeatable: enode://... for TCP or ws(s)://... for WebSocket
        #[arg(long = "bootnode")]
        bootnodes: Vec<String>,

        /// Optional explicit stable local P2P peer id
        #[arg(long)]
        p2p_peer_id: Option<String>,

        /// Path used to load/create a stable local P2P peer id
        #[arg(long)]
        p2p_peer_id_path: Option<String>,

        /// JSON-lines block header store used for P2P sync restart recovery
        #[arg(long)]
        p2p_sync_blocks_path: Option<String>,

        /// Max connected peers
        #[arg(long, default_value = "50")]
        max_peers: u32,

        /// Disable discovery service
        #[arg(long, default_value_t = false)]
        disable_discovery: bool,

        /// Disable sync service
        #[arg(long, default_value_t = false)]
        disable_sync: bool,

        /// Optional persisted banlist path for P2P peers/IPs
        #[arg(long)]
        p2p_banlist_path: Option<String>,

        /// Default ban duration in seconds for abusive peers
        #[arg(long, default_value = "600")]
        p2p_ban_duration_secs: u64,

        /// Maximum active inbound peers per source IP
        #[arg(long, default_value = "8")]
        p2p_max_inbound_per_ip: u32,

        /// Maximum inbound connection attempts per source IP per minute
        #[arg(long, default_value = "120")]
        p2p_max_inbound_rate_per_minute: u32,

        /// Maximum inbound gossip frames per peer per minute
        #[arg(long, default_value = "240")]
        p2p_max_gossip_per_peer_per_minute: u32,

        /// Retry interval for reconnecting missing bootnodes
        #[arg(long, default_value = "15")]
        p2p_bootnode_retry_interval_secs: u64,
    },

    /// Initialize data directory
    Init,

    /// Account management
    Account {
        #[command(subcommand)]
        action: AccountAction,
    },

    /// Compute commands
    Compute {
        #[command(subcommand)]
        action: ComputeAction,
    },

    /// Block query commands
    Block {
        #[command(subcommand)]
        action: BlockAction,
    },

    /// Storage maintenance commands
    Storage {
        #[command(subcommand)]
        action: StorageAction,
    },

    /// Run a miner against a remote node RPC endpoint
    Mine {
        /// Node RPC URL, e.g. http://127.0.0.1:8545
        #[arg(long)]
        rpc_url: String,
        /// Optional Bearer/x-rabbit-token auth token
        #[arg(long)]
        rpc_token: Option<String>,
        /// Miner label sent in rabbit_submitWork
        #[arg(long, default_value = "rabbitchain-local")]
        miner_id: String,
    },

    /// Console placeholder (not implemented)
    Console,

    /// Version information
    Version,

    /// Wallet commands
    Wallet {
        #[command(subcommand)]
        action: WalletAction,
    },
}

#[derive(Subcommand, Debug)]
enum WalletAction {
    /// Create new wallet account
    New {
        /// Optional account name
        #[arg(long)]
        name: Option<String>,
        /// Signature scheme: ed25519
        #[arg(long, default_value = "ed25519")]
        scheme: String,
        /// Passphrase for encrypting private key; prompts securely if omitted
        #[arg(long)]
        passphrase: Option<String>,
    },
    /// Create many wallet accounts in one operation
    BatchNew {
        /// Wallet account name prefix, resulting names look like `<prefix>-1`
        #[arg(long, default_value = "miner")]
        name_prefix: String,
        /// Number of wallet accounts to create
        #[arg(long)]
        count: usize,
        /// Signature scheme: ed25519
        #[arg(long, default_value = "ed25519")]
        scheme: String,
        /// Passphrase for encrypting private keys; prompts securely if omitted
        #[arg(long)]
        passphrase: Option<String>,
        /// Optional output path for writing newly created addresses, one per line
        #[arg(long)]
        addresses_out: Option<String>,
    },
    /// List wallet accounts
    List,
    /// Show wallet account details
    Show {
        /// Wallet account name
        #[arg(long)]
        name: String,
    },
    /// Sign message with wallet account
    Sign {
        /// Wallet account name
        #[arg(long)]
        name: String,
        /// Message to sign
        #[arg(long)]
        message: String,
        /// Passphrase used to decrypt key material; prompts securely if omitted and no unlock session exists
        #[arg(long)]
        passphrase: Option<String>,
    },
    /// Verify signature with wallet account public key
    Verify {
        /// Wallet account name
        #[arg(long)]
        name: String,
        /// Original message
        #[arg(long)]
        message: String,
        /// Hex-encoded ed25519 signature
        #[arg(long)]
        signature: String,
    },
    /// Delete wallet account
    Delete {
        /// Wallet account name
        #[arg(long)]
        name: String,
    },

    /// Re-encrypt account with a new passphrase
    RotatePassphrase {
        /// Wallet account name
        #[arg(long)]
        name: String,
        /// Current wallet passphrase; prompts securely if omitted
        #[arg(long)]
        old_passphrase: Option<String>,
        /// New wallet passphrase; prompts securely if omitted
        #[arg(long)]
        new_passphrase: Option<String>,
    },

    /// Unlock account for a temporary signing session
    Unlock {
        /// Wallet account name
        #[arg(long)]
        name: String,
        /// Wallet passphrase used to unlock the account; prompts securely if omitted
        #[arg(long)]
        passphrase: Option<String>,
        /// Session token lifetime in seconds
        #[arg(long, default_value_t = 600)]
        ttl_secs: u64,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum AccountAction {
    /// Create new account
    New {
        /// Optional account name
        #[arg(long)]
        name: Option<String>,
        /// Signature scheme: ed25519
        #[arg(long, default_value = "ed25519")]
        scheme: String,
        /// Passphrase for encrypting private key; prompts securely if omitted
        #[arg(long)]
        passphrase: Option<String>,
    },
    /// List accounts
    List,
    /// Get account balance
    Balance {
        #[arg(short, long)]
        address: String,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum ComputeAction {
    /// Send compute operation
    Send {
        /// Compute operation JSON file path
        #[arg(long)]
        tx_file: String,
    },
    /// Get compute operation result by tx id
    Get {
        /// Compute operation id
        #[arg(short, long)]
        tx_id: String,
    },
}

#[derive(Subcommand, Debug)]
enum BlockAction {
    /// Get latest block
    Latest,
    /// Get latest block height only
    #[command(alias = "latest-height")]
    Height,
    /// Get block by number
    Get {
        /// Block height in decimal
        #[arg(short, long)]
        number: u64,
    },
}

#[derive(Subcommand, Debug)]
enum StorageAction {
    /// Rebuild compute DB into the current codec and backend compression format
    RebuildComputeDb {
        /// Compute persistent backend override: rocksdb|redb
        #[arg(long)]
        compute_backend: Option<String>,
        /// Compute database path override
        #[arg(long)]
        compute_db_path: Option<String>,
        /// Remove the old database backup after a successful rebuild
        #[arg(long, default_value_t = false)]
        discard_backup: bool,
        /// Build and verify a temporary database without replacing the original
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
    /// Prune old compute hot-state entries outside the retention window
    PruneComputeDb {
        /// Compute persistent backend override: rocksdb|redb
        #[arg(long)]
        compute_backend: Option<String>,
        /// Compute database path override
        #[arg(long)]
        compute_db_path: Option<String>,
        /// Retention profile: full|validator|mainnet prunes; archive|explorer|retain-all keeps all
        #[arg(long, default_value = "full")]
        retention_profile: String,
        /// Reorg/rollback retention window in seconds
        #[arg(long, default_value_t = 604_800)]
        retention_window_secs: u64,
        /// Override current unix timestamp for deterministic tests or maintenance windows
        #[arg(long)]
        now_unix_secs: Option<u64>,
        /// Scan and report candidates without deleting entries
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
    /// Generate a fixed workload disk-footprint report for compute storage formats
    BenchmarkComputeDb {
        /// Working directory for generated temporary databases
        #[arg(long, default_value = "artifacts/storage-savings-workload")]
        work_dir: String,
        /// Markdown report output path
        #[arg(long, default_value = "docs/STORAGE_SAVINGS_REPORT.md")]
        report: String,
        /// Number of compute outputs and tx results to generate
        #[arg(long, default_value_t = 10_000)]
        outputs: u64,
        /// Number of point lookups used for query latency measurement
        #[arg(long, default_value_t = 2_000)]
        queries: u64,
        /// Remove an existing work directory before running
        #[arg(long, default_value_t = false)]
        overwrite: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let Cli {
        log_level,
        data_dir,
        network,
        rpc_url,
        rpc_token,
        config,
        otel_enabled,
        otel_endpoint,
        command,
    } = Cli::parse();
    let profile = NetworkProfile::parse(&network)?;
    let rpc_url = rpc_url.unwrap_or_else(|| profile.default_rpc_url());
    let shared_data_dir = resolve_shared_data_dir(data_dir.as_deref());

    // Initialize logging / tracing
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        format!(
            "info,zeroccore={},rabbitnet={},rabbitapi={}",
            log_level, log_level, log_level
        )
        .into()
    });

    if otel_enabled {
        let tracer = init_otel_tracer(&otel_endpoint)?;
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer())
            .with(OpenTelemetryLayer::new(tracer))
            .init();
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer())
            .init();
    }

    match command {
        Some(Commands::Run {
            mine,
            enable_mining_rpc,
            disable_local_miner,
            coinbase,
            coinbase_file,
            http_port,
            ws_port,
            compute_backend,
            compute_db_path,
            chain_id,
            network_id,
            rpc_coinbase,
            rpc_auth_token,
            rpc_rate_limit_per_minute,
            mining_work_target_leading_rabbit_bytes,
            p2p_listen_addr,
            p2p_listen_port,
            disable_p2p_tcp,
            disable_p2p_ws,
            p2p_ws_listen_addr,
            p2p_ws_listen_port,
            p2p_ws_external_url,
            bootnodes,
            p2p_peer_id,
            p2p_peer_id_path,
            p2p_sync_blocks_path,
            max_peers,
            disable_discovery,
            disable_sync,
            p2p_banlist_path,
            p2p_ban_duration_secs,
            p2p_max_inbound_per_ip,
            p2p_max_inbound_rate_per_minute,
            p2p_max_gossip_per_peer_per_minute,
            p2p_bootnode_retry_interval_secs,
        }) => {
            let data_dir = resolve_node_data_dir(data_dir.as_deref(), profile);
            let mut api_config = if let Some(path) = &config {
                let mut cfg = commands::init::load_api_config(path)?;
                profile.apply_defaults(&mut cfg, &data_dir);
                cfg
            } else {
                profile.default_api_config(&data_dir)
            };

            // CLI flags override config file.
            let http_port = http_port.unwrap_or(api_config.http_rpc.port);
            let ws_port = ws_port.unwrap_or(api_config.ws.port);
            let backend = match compute_backend {
                Some(value) => parse_compute_backend(&value)?,
                None => api_config.http_rpc.compute_backend,
            };
            let compute_db_path =
                compute_db_path.unwrap_or_else(|| api_config.http_rpc.compute_db_path.clone());
            let chain_id = match chain_id {
                Some(value) => parse_u64_decimal_or_hex(&value)?,
                None => api_config.http_rpc.chain_id,
            };
            let network_id = network_id.unwrap_or(api_config.http_rpc.network_id);
            let coinbase_addresses = match coinbase_file.as_deref() {
                Some(path) => read_coinbase_file(path)?,
                None => api_config.http_rpc.coinbase_addresses.clone(),
            };
            let rpc_coinbase = rpc_coinbase
                .or_else(|| coinbase.clone())
                .or_else(|| coinbase_addresses.first().cloned())
                .unwrap_or_else(|| api_config.http_rpc.coinbase.clone());

            api_config.http_rpc = RpcConfig {
                address: "127.0.0.1".to_string(),
                port: http_port,
                compute_backend: backend,
                compute_db_path,
                chain_id,
                network_id,
                coinbase: rpc_coinbase,
                coinbase_addresses,
                mining_enabled: mine || enable_mining_rpc,
                auth_token: rpc_auth_token,
                rate_limit_per_minute: rpc_rate_limit_per_minute,
                mining_work_target_leading_rabbit_bytes,
                ..api_config.http_rpc
            };
            api_config.ws.port = ws_port;
            let enable_p2p_tcp = !disable_p2p_tcp;
            let enable_p2p_ws = !disable_p2p_ws;
            let enable_discovery = !disable_discovery && enable_p2p_tcp;

            println!("🌐 Network profile: {}", profile.as_str());
            println!("   chain_id: {}", api_config.http_rpc.chain_id);
            println!("   network_id: {}", api_config.http_rpc.network_id);
            println!("   coinbase: {}", api_config.http_rpc.coinbase);
            if !api_config.http_rpc.coinbase_addresses.is_empty() {
                println!(
                    "   mining coinbase rotation: {} addresses",
                    api_config.http_rpc.coinbase_addresses.len()
                );
            }
            println!(
                "   rpc: {}:{}",
                api_config.http_rpc.address, api_config.http_rpc.port
            );
            println!(
                "   rpc auth: {}",
                if api_config.http_rpc.auth_token.is_some() {
                    "enabled"
                } else {
                    "disabled"
                }
            );
            println!(
                "   rpc rate limit: {} req/min",
                api_config.http_rpc.rate_limit_per_minute
            );
            if enable_p2p_tcp {
                println!("   p2p tcp: {}:{}", p2p_listen_addr, p2p_listen_port);
            } else {
                println!("   p2p tcp: disabled");
            }
            if enable_p2p_ws {
                println!("   p2p websocket transport: enabled");
            } else {
                println!("   p2p websocket transport: disabled");
            }
            if enable_p2p_ws && p2p_ws_listen_port.is_some() {
                let port = p2p_ws_listen_port.unwrap_or_default();
                println!(
                    "   p2p websocket: {}:{}",
                    p2p_ws_listen_addr.as_deref().unwrap_or("127.0.0.1"),
                    port
                );
            }
            if enable_p2p_ws {
                if let Some(url) = &p2p_ws_external_url {
                    println!("   p2p websocket external: {}", url);
                }
            }
            if !enable_p2p_tcp && !disable_discovery {
                println!("   discovery: disabled (requires direct TCP P2P)");
            }
            if !bootnodes.is_empty() {
                println!("   bootnodes: {}", bootnodes.join(", "));
            }
            if let Some(peer_id) = &p2p_peer_id {
                println!("   p2p peer id: {}", peer_id);
            }
            if let Some(path) = &p2p_peer_id_path {
                println!("   p2p peer id path: {}", path);
            }
            if let Some(path) = &p2p_sync_blocks_path {
                println!("   p2p sync block store: {}", path);
            }

            commands::run::run_node(commands::run::RunNodeConfig {
                mine,
                enable_mining_rpc,
                disable_local_miner,
                coinbase,
                coinbase_count: api_config.http_rpc.coinbase_addresses.len(),
                http_port,
                ws_port,
                data_dir: data_dir.clone(),
                rpc_config: Some(api_config.http_rpc.clone()),
                p2p_listen_addr,
                p2p_listen_port,
                p2p_tcp_enabled: enable_p2p_tcp,
                p2p_ws_enabled: enable_p2p_ws,
                p2p_ws_listen_addr,
                p2p_ws_listen_port,
                p2p_ws_external_url,
                bootnodes,
                p2p_peer_id,
                p2p_peer_id_path,
                p2p_sync_blocks_path,
                max_peers,
                enable_discovery,
                enable_sync: !disable_sync,
                p2p_banlist_path,
                p2p_ban_duration_secs,
                p2p_max_inbound_per_ip,
                p2p_max_inbound_rate_per_minute,
                p2p_max_gossip_per_peer_per_minute,
                p2p_bootnode_retry_interval_secs,
            })
            .await?;
        }
        Some(Commands::Init) => {
            let data_dir = resolve_node_data_dir(data_dir.as_deref(), profile);
            commands::init::init_data_dir(&data_dir)?;
            let api_config = profile.default_api_config(&data_dir);
            let cfg_path = format!("{}/api-config.json", &data_dir);
            commands::init::write_api_config(&cfg_path, &api_config)?;
            println!(
                "Initialized {} profile under {}",
                profile.as_str(),
                data_dir
            );
            println!("API config written to {}", cfg_path);
        }
        Some(Commands::Account { action }) => {
            commands::account::handle_account(
                action,
                &shared_data_dir,
                &rpc_url,
                rpc_token.as_deref(),
            )
            .await?;
        }
        Some(Commands::Compute { action }) => {
            commands::compute::handle_compute(
                action,
                &shared_data_dir,
                &rpc_url,
                rpc_token.as_deref(),
            )
            .await?;
        }
        Some(Commands::Block { action }) => {
            commands::block::handle_block(action, &rpc_url, rpc_token.as_deref()).await?;
        }
        Some(Commands::Storage { action }) => {
            let node_data_dir = resolve_node_data_dir(data_dir.as_deref(), profile);
            let api_config = if let Some(path) = &config {
                let mut cfg = commands::init::load_api_config(path)?;
                profile.apply_defaults(&mut cfg, &node_data_dir);
                cfg
            } else {
                profile.default_api_config(&node_data_dir)
            };

            match action {
                StorageAction::RebuildComputeDb {
                    compute_backend,
                    compute_db_path,
                    discard_backup,
                    dry_run,
                } => {
                    let backend = match compute_backend {
                        Some(value) => parse_compute_backend(&value)?,
                        None => api_config.http_rpc.compute_backend,
                    };
                    let path = compute_db_path
                        .unwrap_or_else(|| api_config.http_rpc.compute_db_path.clone());
                    commands::storage::rebuild_compute_db(backend, &path, discard_backup, dry_run)?;
                }
                StorageAction::PruneComputeDb {
                    compute_backend,
                    compute_db_path,
                    retention_profile,
                    retention_window_secs,
                    now_unix_secs,
                    dry_run,
                } => {
                    let backend = match compute_backend {
                        Some(value) => parse_compute_backend(&value)?,
                        None => api_config.http_rpc.compute_backend,
                    };
                    let path = compute_db_path
                        .unwrap_or_else(|| api_config.http_rpc.compute_db_path.clone());
                    commands::storage::prune_compute_db(
                        backend,
                        &path,
                        &retention_profile,
                        retention_window_secs,
                        now_unix_secs,
                        dry_run,
                    )?;
                }
                StorageAction::BenchmarkComputeDb {
                    work_dir,
                    report,
                    outputs,
                    queries,
                    overwrite,
                } => {
                    commands::storage::benchmark_compute_storage(
                        &work_dir, &report, outputs, queries, overwrite,
                    )?;
                }
            }
        }
        Some(Commands::Mine {
            rpc_url,
            rpc_token,
            miner_id,
        }) => {
            commands::run::run_remote_miner(rpc_url, rpc_token, miner_id).await?;
        }
        Some(Commands::Console) => {
            commands::console::start_console().await?;
        }
        Some(Commands::Version) => {
            println!("RabbitChain v{}", env!("CARGO_PKG_VERSION"));
        }
        Some(Commands::Wallet { action }) => {
            let cmd = map_wallet_action(action)?;
            commands::wallet::handle_wallet(&shared_data_dir, cmd).await?;
        }
        None => {
            // Default: show help
            println!("RabbitChain v{}", env!("CARGO_PKG_VERSION"));
            println!("Use --help for usage information");
        }
    }

    Ok(())
}

fn init_otel_tracer(endpoint: &str) -> Result<opentelemetry_sdk::trace::Tracer> {
    let exporter = opentelemetry_otlp::new_exporter()
        .tonic()
        .with_endpoint(endpoint.to_string());

    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(exporter)
        .with_trace_config(opentelemetry_sdk::trace::Config::default().with_resource(
            opentelemetry_sdk::Resource::new(vec![KeyValue::new("service.name", "rabbitchain")]),
        ))
        .install_batch(opentelemetry_sdk::runtime::Tokio)
        .map_err(|e| anyhow::anyhow!("failed to init otel tracer: {e}"))?;

    Ok(tracer)
}

fn expand_data_dir(input: &str) -> String {
    if input == "~" {
        return std::env::var("HOME").unwrap_or_else(|_| input.to_string());
    }
    if let Some(rest) = input.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    input.to_string()
}

fn resolve_shared_data_dir(input: Option<&str>) -> String {
    let explicit = input.map(expand_data_dir);
    if let Some(dir) = explicit {
        return dir;
    }

    let primary = expand_data_dir("~/.rabbitchain");
    primary
}

fn resolve_node_data_dir(input: Option<&str>, profile: NetworkProfile) -> String {
    match input {
        Some(value) => expand_data_dir(value),
        None => format!("{}/{}", resolve_shared_data_dir(None), profile.as_str()),
    }
}

fn parse_wallet_scheme(value: &str) -> Result<WalletScheme> {
    match value.to_ascii_lowercase().as_str() {
        "ed25519" => Ok(WalletScheme::Ed25519),
        other => anyhow::bail!("unsupported wallet scheme: {other}"),
    }
}

fn map_wallet_action(action: WalletAction) -> Result<WalletCommand> {
    let cmd = match action {
        WalletAction::New {
            name,
            scheme,
            passphrase,
        } => WalletCommand::New {
            name,
            scheme: parse_wallet_scheme(&scheme)?,
            passphrase,
        },
        WalletAction::BatchNew {
            name_prefix,
            count,
            scheme,
            passphrase,
            addresses_out,
        } => WalletCommand::BatchNew {
            name_prefix,
            count,
            scheme: parse_wallet_scheme(&scheme)?,
            passphrase,
            addresses_out,
        },
        WalletAction::List => WalletCommand::List,
        WalletAction::Show { name } => WalletCommand::Show { name },
        WalletAction::Sign {
            name,
            message,
            passphrase,
        } => WalletCommand::Sign {
            name,
            message,
            passphrase,
        },
        WalletAction::Verify {
            name,
            message,
            signature,
        } => WalletCommand::Verify {
            name,
            message,
            signature_hex: signature,
        },
        WalletAction::Delete { name } => WalletCommand::Delete { name },
        WalletAction::RotatePassphrase {
            name,
            old_passphrase,
            new_passphrase,
        } => WalletCommand::RotatePassphrase {
            name,
            old_passphrase,
            new_passphrase,
        },
        WalletAction::Unlock {
            name,
            passphrase,
            ttl_secs,
        } => WalletCommand::Unlock {
            name,
            passphrase,
            ttl_secs,
        },
    };
    Ok(cmd)
}

fn parse_compute_backend(value: &str) -> Result<ComputeBackend> {
    match value.to_ascii_lowercase().as_str() {
        "mem" => Ok(ComputeBackend::Mem),
        "rocksdb" => Ok(ComputeBackend::RocksDb),
        "redb" => Ok(ComputeBackend::Redb),
        other => anyhow::bail!("unsupported compute backend: {other}"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NetworkProfile {
    Mainnet,
    Testnet,
    Devnet,
    Local,
}

impl NetworkProfile {
    fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "mainnet" => Ok(Self::Mainnet),
            "testnet" => Ok(Self::Testnet),
            "devnet" => Ok(Self::Devnet),
            "local" => Ok(Self::Local),
            other => anyhow::bail!("unsupported network profile: {other}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Mainnet => "mainnet",
            Self::Testnet => "testnet",
            Self::Devnet => "devnet",
            Self::Local => "local",
        }
    }

    fn default_http_port(self) -> u16 {
        match self {
            Self::Mainnet | Self::Local => 8545,
            Self::Testnet => 18545,
            Self::Devnet => 28545,
        }
    }

    fn default_ws_port(self) -> u16 {
        match self {
            Self::Mainnet | Self::Local => 8546,
            Self::Testnet => 18546,
            Self::Devnet => 28546,
        }
    }

    fn default_chain_id(self) -> u64 {
        match self {
            Self::Mainnet => 10086,
            Self::Testnet => 10087,
            Self::Devnet => 10088,
            Self::Local => 31337,
        }
    }

    fn default_network_id(self) -> u64 {
        self.default_chain_id()
    }

    fn default_compute_backend(self) -> ComputeBackend {
        match self {
            Self::Local => ComputeBackend::Mem,
            Self::Mainnet | Self::Testnet | Self::Devnet => ComputeBackend::RocksDb,
        }
    }

    fn default_rpc_url(self) -> String {
        format!("http://127.0.0.1:{}", self.default_http_port())
    }

    fn default_api_config(self, data_dir: &str) -> ApiConfig {
        let mut cfg = ApiConfig::default();
        self.apply_defaults(&mut cfg, data_dir);
        cfg.http_rpc.compute_backend = self.default_compute_backend();
        cfg
    }

    fn apply_defaults(self, cfg: &mut ApiConfig, data_dir: &str) {
        cfg.http_rpc.port = self.default_http_port();
        cfg.ws.port = self.default_ws_port();
        cfg.http_rpc.chain_id = self.default_chain_id();
        cfg.http_rpc.network_id = self.default_network_id();
        cfg.rest.port = self.default_http_port().saturating_add(10);
        if cfg.http_rpc.compute_db_path == "./data/compute-db" {
            cfg.http_rpc.compute_db_path = format!("{}/compute-db", data_dir);
        }
        // Production profiles (mainnet/testnet/devnet) require every compute tx
        // to carry explicit fee fields. Local development keeps the legacy
        // fee-less path for tooling convenience.
        match self {
            Self::Mainnet | Self::Testnet | Self::Devnet => {
                cfg.http_rpc.require_fee_for_compute_tx = true;
            }
            Self::Local => {}
        }
    }
}

fn parse_u64_decimal_or_hex(value: &str) -> Result<u64> {
    let v = value.trim();
    if let Some(hex) = v.strip_prefix("0x") {
        u64::from_str_radix(hex, 16)
            .map_err(|e| anyhow::anyhow!("invalid hex integer '{value}': {e}"))
    } else {
        v.parse::<u64>()
            .map_err(|e| anyhow::anyhow!("invalid integer '{value}': {e}"))
    }
}

fn read_coinbase_file(path: &str) -> Result<Vec<String>> {
    let path = expand_data_dir(path);
    let content = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("failed to read coinbase file {}: {}", path, e))?;
    let addresses: Vec<String> = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(ToOwned::to_owned)
        .collect();
    if addresses.is_empty() {
        anyhow::bail!("coinbase file {} contains no usable addresses", path);
    }
    Ok(addresses)
}
