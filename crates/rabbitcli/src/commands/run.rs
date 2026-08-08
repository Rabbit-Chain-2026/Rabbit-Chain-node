//! Run command - Full node implementation

use crate::Result;
use anyhow::{anyhow, bail};
use rabbitapi::rpc::RpcConfig;
use rabbitapi::rpc::{set_block_broadcaster, wire_reorg_notifications};
use rabbitapi::{ApiConfig, ApiService};
use rabbitcore::account::U256;
use rabbitcore::block::{
    create_genesis_block, pow_hash_meets_target, pow_target_from_hex, pow_target_to_hex,
};

use rabbitnet::{
    configure_global_block_persistence, global_highest_peer_height, global_latest_block,
    global_store_block, NetworkConfig, NetworkService,
};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

const LOCAL_MINER_MAX_HASHES_PER_JOB_ATTEMPT: u64 = 2_000_000;
const LOCAL_MINER_MAX_JOB_AGE_SECS: u64 = 2;
const LOCAL_MINER_STARTUP_WAIT_SECS: u64 = 120;
const LOCAL_MINER_STARTUP_POLL_SECS: u64 = 2;

pub struct RunNodeConfig {
    pub mine: bool,
    pub enable_mining_rpc: bool,
    pub disable_local_miner: bool,
    pub coinbase: Option<String>,
    pub coinbase_count: usize,
    pub http_port: u16,
    pub ws_port: u16,
    pub data_dir: String,
    pub rpc_config: Option<RpcConfig>,
    pub p2p_listen_addr: String,
    pub p2p_listen_port: u16,
    pub p2p_tcp_enabled: bool,
    pub p2p_ws_enabled: bool,
    pub p2p_ws_listen_addr: Option<String>,
    pub p2p_ws_listen_port: Option<u16>,
    pub p2p_ws_external_url: Option<String>,
    pub bootnodes: Vec<String>,
    pub p2p_peer_id: Option<String>,
    pub p2p_peer_id_path: Option<String>,
    pub p2p_sync_blocks_path: Option<String>,
    pub max_peers: u32,
    pub enable_discovery: bool,
    pub enable_sync: bool,
    pub p2p_banlist_path: Option<String>,
    pub p2p_ban_duration_secs: u64,
    pub p2p_max_inbound_per_ip: u32,
    pub p2p_max_inbound_rate_per_minute: u32,
    pub p2p_max_gossip_per_peer_per_minute: u32,
    pub p2p_bootnode_retry_interval_secs: u64,
}

pub async fn run_remote_miner(
    rpc_url: String,
    rpc_token: Option<String>,
    miner_label: String,
) -> Result<()> {
    println!("⛏️  Starting remote RPC miner");
    println!("   RPC URL: {}", rpc_url);
    println!(
        "   RPC auth: {}",
        if rpc_token.is_some() {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!("   Miner label: {}", miner_label);
    run_rpc_backed_miner(rpc_url, rpc_token, miner_label).await;
    Ok(())
}

pub async fn run_node(cfg: RunNodeConfig) -> Result<()> {
    let RunNodeConfig {
        mine,
        enable_mining_rpc,
        disable_local_miner,
        coinbase,
        coinbase_count,
        http_port,
        ws_port,
        data_dir,
        rpc_config,
        p2p_listen_addr,
        p2p_listen_port,
        p2p_tcp_enabled,
        p2p_ws_enabled,
        p2p_ws_listen_addr,
        p2p_ws_listen_port,
        p2p_ws_external_url,
        bootnodes,
        p2p_peer_id,
        p2p_peer_id_path,
        p2p_sync_blocks_path,
        max_peers,
        enable_discovery,
        enable_sync,
        p2p_banlist_path,
        p2p_ban_duration_secs,
        p2p_max_inbound_per_ip,
        p2p_max_inbound_rate_per_minute,
        p2p_max_gossip_per_peer_per_minute,
        p2p_bootnode_retry_interval_secs,
    } = cfg;

    println!("🚀 Starting RabbitChain node...");
    println!("   Data directory: {}", data_dir);
    println!("   HTTP RPC port: {}", http_port);
    println!("   WebSocket port: {}", ws_port);
    let mining_rpc_enabled = mine || enable_mining_rpc;
    println!(
        "   Mining RPC: {}",
        if mining_rpc_enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "   Local mining worker: {}",
        if mine { "enabled" } else { "disabled" }
    );
    if p2p_tcp_enabled {
        println!("   P2P TCP listen: {}:{}", p2p_listen_addr, p2p_listen_port);
    } else {
        println!("   P2P TCP: disabled");
    }
    println!(
        "   P2P WebSocket transport: {}",
        if p2p_ws_enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    if p2p_ws_enabled && p2p_ws_listen_port.is_some() {
        let port = p2p_ws_listen_port.unwrap_or_default();
        println!(
            "   P2P WebSocket listen: {}:{}",
            p2p_ws_listen_addr.as_deref().unwrap_or("127.0.0.1"),
            port
        );
    }
    if p2p_ws_enabled {
        if let Some(url) = &p2p_ws_external_url {
            println!("   P2P WebSocket external URL: {}", url);
        }
    }
    println!("   P2P max peers: {}", max_peers);
    println!(
        "   Discovery: {}",
        if enable_discovery {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "   Sync: {}",
        if enable_sync { "enabled" } else { "disabled" }
    );
    println!(
        "   P2P DoS guard: max_per_ip={}, inbound_rate/min={}, gossip_rate/min={}",
        p2p_max_inbound_per_ip, p2p_max_inbound_rate_per_minute, p2p_max_gossip_per_peer_per_minute
    );
    println!("   P2P ban duration: {}s", p2p_ban_duration_secs);
    println!(
        "   Bootnode reconnect interval: {}s",
        p2p_bootnode_retry_interval_secs
    );
    if !bootnodes.is_empty() {
        println!("   Bootnodes: {}", bootnodes.join(", "));
    }
    if let Some(ref banlist_path) = p2p_banlist_path {
        println!("   P2P banlist: {}", banlist_path);
    }
    let p2p_peer_id_path = p2p_peer_id_path.unwrap_or_else(|| format!("{data_dir}/p2p-peer-id"));
    let p2p_sync_blocks_path =
        p2p_sync_blocks_path.unwrap_or_else(|| format!("{data_dir}/p2p-blocks.jsonl"));
    if let Some(peer_id) = &p2p_peer_id {
        println!("   P2P peer id: {}", peer_id);
    } else {
        println!("   P2P peer id path: {}", p2p_peer_id_path);
    }
    println!("   P2P sync block store: {}", p2p_sync_blocks_path);
    if mining_rpc_enabled {
        let display_coinbase = coinbase
            .clone()
            .unwrap_or_else(|| "0x0000000000000000000000000000000000000000".to_string());
        println!("   🎯 Mining RPC coinbase: {}", display_coinbase);
        if coinbase_count > 0 {
            println!(
                "   🎯 Mining RPC coinbase rotation: {} addresses",
                coinbase_count
            );
        }
        println!(
            "   Local mining worker: {}",
            if mine && !disable_local_miner {
                "enabled"
            } else {
                "disabled"
            }
        );
    } else if let Some(ref cb) = coinbase {
        println!("   Coinbase: {}", cb);
    }

    // Initialize genesis block
    let genesis = create_genesis_block();
    println!(
        "   Genesis block hash: 0x{}",
        hex::encode(genesis.header.hash.as_bytes())
    );
    println!("   Genesis difficulty: {}", genesis.header.difficulty);
    println!("✅ Node started successfully!");
    println!("   Press Ctrl+C to stop");

    let network_id = rpc_config
        .as_ref()
        .map(|cfg| cfg.network_id)
        .unwrap_or(10086);
    let spawn_local_miner = mine && !disable_local_miner;
    let rpc_token_for_miner = rpc_config
        .as_ref()
        .and_then(|cfg| cfg.auth_token.clone());
    configure_global_block_persistence(Some(PathBuf::from(&p2p_sync_blocks_path)))?;
    if global_latest_block().is_none() {
        global_store_block(genesis.clone())?;
        println!("   Genesis block seeded into local chain cache");
    }

    let api_service = if let Some(mut cfg) = rpc_config.clone() {
        let rpc_token = cfg.auth_token.clone();
        cfg.port = http_port;
        cfg.mining_enabled = mining_rpc_enabled;
        let mut api_cfg = ApiConfig {
            http_rpc: cfg,
            ..ApiConfig::default()
        };
        api_cfg.ws.port = ws_port;
        api_cfg.rest.port = http_port.saturating_add(10);

        let api = ApiService::try_new(api_cfg)
            .map_err(|e| anyhow::anyhow!("failed to create API service: {e}"))?;
        api.start()
            .await
            .map_err(|e| anyhow::anyhow!("failed to start API service: {e}"))?;
        println!("   HTTP RPC service started on port {}", http_port);

        Some(api)
    } else {
        if mining_rpc_enabled {
            println!("   ⚠️ Mining requested but HTTP RPC is disabled; mining worker not started");
        }
        None
    };

    println!("   🧭 Building network service config");
    let network_cfg = NetworkConfig {
        network_id,
        listen_addr: p2p_listen_addr,
        listen_port: p2p_listen_port,
        enable_tcp_transport: p2p_tcp_enabled,
        enable_ws_transport: p2p_ws_enabled,
        ws_listen_addr: p2p_ws_listen_addr,
        ws_listen_port: p2p_ws_listen_port,
        ws_external_url: p2p_ws_external_url,
        local_peer_id: p2p_peer_id,
        peer_id_path: Some(PathBuf::from(p2p_peer_id_path)),
        sync_blocks_path: Some(PathBuf::from(p2p_sync_blocks_path)),
        max_peers,
        min_peers: max_peers.min(25),
        bootnodes,
        enable_discovery,
        enable_sync,
        banlist_path: p2p_banlist_path,
        ban_duration_secs: p2p_ban_duration_secs,
        max_inbound_per_ip: p2p_max_inbound_per_ip,
        max_inbound_rate_per_minute: p2p_max_inbound_rate_per_minute,
        max_gossip_per_peer_per_minute: p2p_max_gossip_per_peer_per_minute,
        bootnode_retry_interval_secs: p2p_bootnode_retry_interval_secs,
        sync_auto_advance: false,
        sync_auto_advance_interval_secs: 2,
        ..NetworkConfig::default()
    };
    println!("   🧭 Creating network service");
    let network_service = Arc::new(NetworkService::new(network_cfg)?);
    println!("   🧭 Starting network service");
    network_service.start().await?;
    set_block_broadcaster(Some(Arc::new({
        let network_service = network_service.clone();
        move |block: &rabbitcore::block::Block| {
            let _ = network_service.broadcast_block(block.clone());
        }
    })));
    // Wire sync/fork-choice tip updates into RPC mining queues (H6 reorg hook).
    if let Some(api) = api_service.as_ref().and_then(|svc| svc.rpc_api()) {
        wire_reorg_notifications(&api);
        println!("   🔗 Canonical tip listener wired to RPC reorg hook");
    }
    if spawn_local_miner {
        wait_for_mining_alignment(network_service.clone()).await?;
        let rpc_url = format!("http://127.0.0.1:{http_port}");
        println!(
            "   🎯 Starting RPC-backed mining worker thread at {}",
            rpc_url
        );
        std::thread::Builder::new()
            .name("rabbitchain-local-miner".to_string())
            .spawn({
                let rpc_token = rpc_token_for_miner.clone();
                move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("failed to build local mining runtime");
                    runtime.block_on(run_rpc_backed_miner(
                        rpc_url,
                        rpc_token,
                        "rabbitchain-local".to_string(),
                    ));
                }
            })
            .map_err(|err| anyhow::anyhow!("failed to spawn mining worker thread: {err}"))?;
    } else if mining_rpc_enabled {
        println!("   ⛏️  Mining RPC enabled without local mining worker");
    }
    println!(
        "   P2P service started (tcp={}, websocket={})",
        p2p_tcp_enabled, p2p_ws_enabled
    );

    // Keep running
    loop {
        println!("   Peers connected: {}", network_service.peer_count());
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
    }
}

async fn wait_for_mining_alignment(network_service: Arc<NetworkService>) -> Result<()> {
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(LOCAL_MINER_STARTUP_WAIT_SECS);
    loop {
        let peer_count = network_service.peer_count();
        let local_head = rabbitnet::global_latest_block()
            .map(|block| block.header.number.as_u64())
            .unwrap_or(0);
        let peer_head = rabbitnet::global_highest_peer_height();

        if peer_count > 0 && peer_head > 0 && local_head >= peer_head.saturating_sub(1) {
            println!(
                "   ⛏️  Mining alignment ready: peers={}, local_head={}, peer_head={}",
                peer_count, local_head, peer_head
            );
            return Ok(());
        }

        if std::time::Instant::now() >= deadline {
            println!(
                "   ⚠️ Mining alignment timeout: peers={}, local_head={}, peer_head={}",
                peer_count, local_head, peer_head
            );
            return Ok(());
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(
            LOCAL_MINER_STARTUP_POLL_SECS,
        ))
        .await;
    }
}

#[derive(Debug, Deserialize)]
struct RpcResponseEnvelope<T> {
    result: Option<T>,
    error: Option<RpcErrorEnvelope>,
}

#[derive(Debug, Deserialize)]
struct RpcErrorEnvelope {
    message: String,
}

#[derive(Debug, Deserialize)]
struct WorkPayload {
    work_id: String,
    prev_hash: String,
    height: u64,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    target_leading_rabbit_bytes: Option<usize>,
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    timestamp: Option<u64>,
    #[serde(default)]
    gas_limit: Option<u64>,
    #[serde(default)]
    base_fee_per_gas: Option<String>,
    #[serde(default)]
    difficulty: Option<String>,
    #[serde(default)]
    coinbase: Option<String>,
}

#[derive(Debug, Clone)]
struct WorkCursor {
    prev_hash: String,
    height: u64,
    target: U256,
    next_nonce: u64,
}

#[derive(Debug, Deserialize)]
struct SubmitWorkPayload {
    accepted: bool,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    block_hash: Option<String>,
    #[serde(default)]
    height: Option<u64>,
}

async fn rpc_call<T: DeserializeOwned>(
    client: &reqwest::Client,
    rpc_url: &str,
    rpc_token: Option<&str>,
    method: &str,
    params: serde_json::Value,
) -> anyhow::Result<T> {
    let mut request = client.post(rpc_url).json(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    }));
    if let Some(token) = rpc_token {
        request = request.bearer_auth(token).header("x-rabbit-token", token);
    }

    let response = request.send().await?;

    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        bail!("rpc {} failed with status {}: {}", method, status, body);
    }

    let envelope: RpcResponseEnvelope<T> = serde_json::from_str(&body).map_err(|e| {
        anyhow!(
            "decode rpc {} response failed: {} (body={})",
            method,
            e,
            body
        )
    })?;

    if let Some(err) = envelope.error {
        bail!("rpc {} returned error: {}", method, err.message);
    }

    envelope
        .result
        .ok_or_else(|| anyhow!("rpc {} returned empty result", method))
}

fn parse_hex_u256(raw: &str) -> anyhow::Result<U256> {
    let mut trimmed = raw.trim().strip_prefix("0x").unwrap_or(raw.trim()).to_string();
    if trimmed.is_empty() {
        return Ok(U256::zero());
    }
    // hex::decode 要求偶数位（如 "0x0" 会报 Odd number of digits）
    if trimmed.len() % 2 == 1 {
        trimmed.insert(0, '0');
    }
    let bytes = hex::decode(&trimmed).map_err(|e| anyhow!("invalid hex u256: {e}"))?;
    if bytes.len() > 32 {
        bail!("u256 hex too long");
    }
    let mut buf = [0u8; 32];
    buf[32 - bytes.len()..].copy_from_slice(&bytes);
    Ok(U256::from_big_endian(&buf))
}

fn parse_address20(raw: &str) -> anyhow::Result<[u8; 20]> {
    let trimmed = raw.trim().strip_prefix("0x").unwrap_or(raw.trim());
    let bytes = hex::decode(trimmed).map_err(|e| anyhow!("invalid address hex: {e}"))?;
    if bytes.len() != 20 {
        bail!("address must be 20 bytes");
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn compute_pow_hash_from_work(
    work: &WorkPayload,
    prev_hash: &[u8; 32],
    nonce: u64,
    miner_label: &str,
) -> anyhow::Result<[u8; 32]> {
    use rabbitcore::block::BlockHeader;
    use rabbitcore::crypto::{Address, Hash};

    let difficulty = work
        .difficulty
        .as_deref()
        .map(parse_hex_u256)
        .transpose()?
        .unwrap_or_else(|| U256::from_u128(1));
    let base_fee = work
        .base_fee_per_gas
        .as_deref()
        .map(parse_hex_u256)
        .transpose()?
        .unwrap_or_else(|| U256::from(1_000_000_000u64));
    let coinbase = work
        .coinbase
        .as_deref()
        .map(parse_address20)
        .transpose()?
        .map(Address::from_bytes)
        .unwrap_or_else(Address::zero);

    let header = BlockHeader {
        version: work.version.unwrap_or(3),
        parent_hash: Hash::from_bytes(*prev_hash),
        uncle_hashes: Vec::new(),
        coinbase,
        state_root: Hash::zero(),
        transactions_root: Hash::zero(),
        receipts_root: Hash::zero(),
        number: U256::from(work.height),
        gas_limit: work.gas_limit.unwrap_or(30_000_000),
        gas_used: 0,
        timestamp: work.timestamp.unwrap_or(0),
        difficulty,
        nonce,
        // 与节点 submit_work 的 canonical 校验一致：extra_data = miner label
        extra_data: miner_label.as_bytes().to_vec(),
        mix_hash: Hash::zero(),
        base_fee_per_gas: base_fee,
        hash: Hash::zero(),
    };
    let digest = rabbitcore::block::compute_pow_hash(&header, nonce);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_bytes());
    Ok(out)
}

fn legacy_target_from_leading_rabbit_bytes(bytes: usize) -> U256 {
    let mut target = [0xFFu8; 32];
    let prefix = bytes.min(32);
    target[..prefix].fill(0);
    U256::from_big_endian(&target)
}

fn resolve_work_target(work: &WorkPayload) -> anyhow::Result<U256> {
    if let Some(target) = &work.target {
        return pow_target_from_hex(target).map_err(|err| anyhow!(err));
    }
    let leading = work
        .target_leading_rabbit_bytes
        .ok_or_else(|| anyhow!("mining work missing target"))?;
    Ok(legacy_target_from_leading_rabbit_bytes(leading))
}

async fn run_rpc_backed_miner(rpc_url: String, rpc_token: Option<String>, miner_label: String) {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            println!("   ⚠️ Failed to build mining RPC client: {}", err);
            return;
        }
    };

    let mut next_work: Option<WorkPayload> = None;
    let mut cursor: Option<WorkCursor> = None;
    loop {
        // Before fetching new work, check if the local node is synced with the network.
        // If the peer has announced a much higher head, the local chain is still catching
        // up and mining would create a fork that wastes work. Skip this cycle and wait.
        let local_head = rabbitnet::global_latest_block()
            .map(|b| b.header.number.as_u64())
            .unwrap_or(0);
        let peer_head = rabbitnet::global_highest_peer_height();
        if peer_head > 0 && local_head + 8 < peer_head {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            continue;
        }

        let work = if let Some(work) = next_work.take() {
            work
        } else {
            match fetch_mining_work(&client, &rpc_url, rpc_token.as_deref(), None, None, false)
                .await
            {
                Ok(work) => work,
                Err(err) => {
                    println!("   ⚠️ Failed to fetch mining work: {}", err);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }
            }
        };

        let prev_hash_hex = work.prev_hash.trim_start_matches("0x");
        let prev_hash_bytes = match hex::decode(prev_hash_hex) {
            Ok(bytes) if bytes.len() == 32 => bytes,
            Ok(_) => {
                println!("   ⚠️ Invalid prev_hash length in work {}", work.work_id);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
            Err(err) => {
                println!(
                    "   ⚠️ Invalid prev_hash hex in work {}: {}",
                    work.work_id, err
                );
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
        };

        let mut prev_hash = [0u8; 32];
        prev_hash.copy_from_slice(&prev_hash_bytes);
        let target = match resolve_work_target(&work) {
            Ok(target) => target,
            Err(err) => {
                println!(
                    "   ⚠️ Invalid mining target in work {}: {}",
                    work.work_id, err
                );
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
        };

        let mut nonce = cursor
            .as_ref()
            .filter(|saved| {
                saved.prev_hash == work.prev_hash
                    && saved.height == work.height
                    && saved.target == target
            })
            .map(|saved| saved.next_nonce)
            .unwrap_or(0);
        let start_nonce = nonce;
        if nonce > 0 {
            println!(
                "   ↻ Resuming mining work at height {} from nonce {}",
                work.height, nonce
            );
        }
        let (watch_tx, mut watch_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut watch_handle = spawn_work_watch(
            client.clone(),
            rpc_url.clone(),
            rpc_token.clone(),
            work.prev_hash.clone(),
            work.height,
            watch_tx.clone(),
        );
        let job_started = std::time::Instant::now();
        loop {
            let hash = match compute_pow_hash_from_work(&work, &prev_hash, nonce, &miner_label) {
                Ok(h) => h,
                Err(err) => {
                    println!("   ⚠️ Invalid mining work template: {}", err);
                    break;
                }
            };

            if pow_hash_meets_target(&hash, target) {
                let hash_hex = format!("0x{}", hex::encode(hash));
                let submit = rpc_call::<SubmitWorkPayload>(
                    &client,
                    &rpc_url,
                    rpc_token.as_deref(),
                    "rabbit_submitWork",
                    serde_json::json!([{
                        "work_id": work.work_id,
                        "nonce": nonce,
                        "hash_hex": hash_hex,
                        "miner": miner_label.clone(),
                    }]),
                )
                .await;

                match submit {
                    Ok(result) if result.accepted => {
                        let block_hash = result.block_hash.unwrap_or_else(|| "0x".to_string());
                        let hash_body = block_hash.trim_start_matches("0x");
                        let short_hash = if hash_body.is_empty() {
                            "unknown".to_string()
                        } else {
                            hash_body.chars().take(16).collect::<String>()
                        };
                        let display_height = result.height.unwrap_or(work.height);
                        println!(
                            "   ⛏️  Block #{} mined! Hash: 0x{}... Nonce: {}",
                            display_height, short_hash, nonce
                        );
                    }
                    Ok(result) => {
                        println!(
                            "   ⚠️ Share rejected at height {}: {}",
                            work.height,
                            result
                                .reason
                                .unwrap_or_else(|| "unknown_reason".to_string())
                        );
                    }
                    Err(err) => {
                        println!(
                            "   ⚠️ Failed to submit share at height {}: {}",
                            work.height, err
                        );
                    }
                }

                cursor = None;
                break;
            }

            nonce = nonce.saturating_add(1);
            if nonce.is_multiple_of(50_000) {
                let leading_rabbit_bits = U256::from_big_endian(&hash).leading_zeros();
                println!(
                    "   Mining block #{}... nonce: {} (leading zero bits: {}, target: {})",
                    work.height,
                    nonce,
                    leading_rabbit_bits,
                    pow_target_to_hex(target)
                );
            }
            if nonce.is_multiple_of(10_000) {
                // Keep RPC/network tasks responsive while this CPU-heavy loop runs.
                tokio::task::yield_now().await;
            }
            if let Ok(result) = watch_rx.try_recv() {
                match result {
                    Ok(pushed_work)
                        if pushed_work.prev_hash != work.prev_hash
                            || pushed_work.height != work.height =>
                    {
                        println!(
                            "   ↻ Switching to pushed work at height {} -> {}",
                            work.height, pushed_work.height
                        );
                        cursor = None;
                        next_work = Some(pushed_work);
                        break;
                    }
                    Ok(_) => {
                        watch_handle = spawn_work_watch(
                            client.clone(),
                            rpc_url.clone(),
                            rpc_token.clone(),
                            work.prev_hash.clone(),
                            work.height,
                            watch_tx.clone(),
                        );
                    }
                    Err(err) => {
                        println!("   ⚠️ Mining work watch ended: {}", err);
                        watch_handle = spawn_work_watch(
                            client.clone(),
                            rpc_url.clone(),
                            rpc_token.clone(),
                            work.prev_hash.clone(),
                            work.height,
                            watch_tx.clone(),
                        );
                    }
                }
            }
            if nonce.saturating_sub(start_nonce) >= LOCAL_MINER_MAX_HASHES_PER_JOB_ATTEMPT
                || job_started.elapsed().as_secs() >= LOCAL_MINER_MAX_JOB_AGE_SECS
            {
                println!(
                    "   ↻ Refreshing mining work at height {} after {} new nonces / {}s",
                    work.height,
                    nonce.saturating_sub(start_nonce),
                    job_started.elapsed().as_secs()
                );
                cursor = Some(WorkCursor {
                    prev_hash: work.prev_hash.clone(),
                    height: work.height,
                    target,
                    next_nonce: nonce,
                });
                break;
            }
        }
        if !watch_handle.is_finished() {
            watch_handle.abort();
        }
    }
}

async fn fetch_mining_work(
    client: &reqwest::Client,
    rpc_url: &str,
    rpc_token: Option<&str>,
    known_prev_hash: Option<&str>,
    known_height: Option<u64>,
    wait: bool,
) -> anyhow::Result<WorkPayload> {
    let params = if wait || known_prev_hash.is_some() || known_height.is_some() {
        serde_json::json!([{
            "known_prev_hash": known_prev_hash,
            "known_height": known_height,
            "wait": wait,
            "timeout_secs": LOCAL_MINER_MAX_JOB_AGE_SECS,
        }])
    } else {
        serde_json::json!([])
    };
    rpc_call::<WorkPayload>(client, rpc_url, rpc_token, "rabbit_getWork", params).await
}

fn spawn_work_watch(
    client: reqwest::Client,
    rpc_url: String,
    rpc_token: Option<String>,
    known_prev_hash: String,
    known_height: u64,
    tx: tokio::sync::mpsc::UnboundedSender<std::result::Result<WorkPayload, String>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let result = fetch_mining_work(
            &client,
            &rpc_url,
            rpc_token.as_deref(),
            Some(known_prev_hash.as_str()),
            Some(known_height),
            true,
        )
        .await
        .map_err(|err| err.to_string());
        let _ = tx.send(result);
    })
}
