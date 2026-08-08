use std::{
    env, fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ed25519_dalek::Signer as _;
use futures_util::stream::{self, StreamExt};
use rabbitapi::rpc::{ComputeBackend, JsonRpcRequest, RpcConfig, RpcServer};
use rabbitcore::compute::primitives::TxId;
use rabbitcore::compute::{
    batch::{
        ComputeBatchPlanner, ComputeBatchRunner, ComputeExecutionService,
        DefaultComputeBatchPlanner, DefaultComputeConflictPolicy, ParallelComputeBatchRunner,
    },
    domain::{DomainConfig, DomainRegistry, InMemoryDomainRegistry},
    execution::{InMemoryObjectStore, ObjectStore},
    object::{ObjectKind, Ownership, Script},
    policy::{AuthorizationPolicy, DefaultAuthorizationPolicy, NoopResourcePolicy, ResourcePolicy},
    scheduler::{ComputeLaneStrategy, InMemoryComputeScheduler},
    tx::{Command, ComputeTx, OutputProposal, TxSignature, TxWitness},
    ComputeFallbackMode, DomainId, ObjectId, OutputId, Version,
};
use rabbitcore::crypto::{keccak256, Hash};
use rabbitstore::{db::MemDatabase, ComputeStore, KeyValueDB, RedbDatabase, RocksDb};

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_rpc_urls() -> Vec<String> {
    if let Ok(value) = env::var("RABBIT_TPS_RPC_URLS") {
        let urls: Vec<String> = value
            .split(',')
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        if !urls.is_empty() {
            return urls;
        }
    }

    if let Ok(url) = env::var("RABBIT_TPS_RPC_URL") {
        return vec![url];
    }

    panic!("RABBIT_TPS_RPC_URLS or RABBIT_TPS_RPC_URL must be set for the real RPC benchmark");
}

fn seed_hash(label: &str, index: u64) -> Hash {
    let mut data = Vec::with_capacity(label.len() + 8);
    data.extend_from_slice(label.as_bytes());
    data.extend_from_slice(&index.to_be_bytes());
    Hash::from_bytes(keccak256(&data))
}

fn build_signed_mint_tx(index: u64, signer: &ed25519_dalek::SigningKey) -> ComputeTx {
    let public_key = signer.verifying_key().to_bytes();
    let mut tx = ComputeTx {
        tx_id: TxId(Hash::zero()),
        domain_id: DomainId(0),
        command: Command::Mint,
        input_set: vec![],
        read_set: vec![],
        output_proposals: vec![OutputProposal {
            output_id: OutputId(seed_hash("output", index)),
            object_id: ObjectId(seed_hash("object", index)),
            domain_id: DomainId(0),
            kind: ObjectKind::State,
            owner: Ownership::Ed25519(public_key),
            predecessor: None,
            version: Version(1),
            state: vec![(index % 255) as u8],
            state_root: None,
            resources: vec![],
            lock: Script::default(),
            logic: None,
            created_at: index,
            ttl: None,
            rent_reserve: None,
            flags: 0,
            extensions: vec![],
        }],
        fee: 0,
        nonce: Some(index + 1),
        metadata: vec![],
        payload: vec![],
        deadline_unix_secs: None,
        chain_id: Some(10086),
        network_id: Some(10086),
        witness: TxWitness {
            signatures: vec![],
            threshold: Some(1),
        },
                    max_fee: 0,
                    priority_fee: 0,
                    gas_limit: 0,
    };

    tx.assign_expected_tx_id();
    let signature = signer.sign(&tx.signing_preimage()).to_bytes();
    tx.witness.signatures = vec![TxSignature::ed25519(signature, public_key)];
    tx
}

fn tx_to_rpc_request(tx: &ComputeTx) -> JsonRpcRequest {
    let signature = tx
        .witness
        .signatures
        .first()
        .expect("benchmark tx must carry a signature");
    let public_key = signature
        .public_key
        .as_ref()
        .expect("benchmark signature must carry a public key");
    let proposal = tx
        .output_proposals
        .first()
        .expect("benchmark tx must carry an output proposal");

    JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "rabbit_submitComputeTx".to_string(),
        params: Some(vec![serde_json::json!({
            "tx_id": format!("0x{}", tx.tx_id.0.to_hex()),
            "domain_id": tx.domain_id.0,
            "chain_id": tx.chain_id.expect("chain_id set"),
            "network_id": tx.network_id.expect("network_id set"),
            "command": "Mint",
            "nonce": tx.nonce.expect("nonce set"),
            "input_set": [],
            "read_set": [],
            "output_proposals": [{
                "output_id": format!("0x{}", proposal.output_id.0.to_hex()),
                "object_id": format!("0x{}", proposal.object_id.0.to_hex()),
                "domain_id": proposal.domain_id.0,
                "kind": "State",
                "owner": {
                    "type": "Ed25519",
                    "public_key": format!("0x{}", hex::encode(public_key))
                },
                "predecessor": null,
                "version": proposal.version.0,
                "state": format!("0x{}", hex::encode(&proposal.state)),
                "created_at": proposal.created_at,
                "logic": null
            }],
            "payload": "0x",
            "deadline_unix_secs": null,
            "witness": {
                "signatures": [{
                    "scheme": "ed25519",
                    "signature": format!("0x{}", hex::encode(&signature.bytes)),
                    "public_key": format!("0x{}", hex::encode(public_key))
                }],
                "threshold": 1
            }
        })]),
        id: serde_json::json!(tx.nonce.unwrap_or(0)),
    }
}

fn build_compute_rpc_request(
    index: u64,
    signer: &ed25519_dalek::SigningKey,
    method: &str,
) -> JsonRpcRequest {
    let tx = build_signed_mint_tx(index, signer);
    let mut request = tx_to_rpc_request(&tx);
    request.method = method.to_string();
    request
}

fn build_rpc_config(backend: ComputeBackend, db_path: String, max_pending: usize) -> RpcConfig {
    RpcConfig {
        compute_backend: backend,
        compute_db_path: db_path,
        compute_batch_window_ms: 0,
        compute_max_batch_size: 128,
        compute_max_pending: max_pending.max(256),
        compute_lane_strategy: ComputeLaneStrategy::ByDomain,
        compute_fallback_mode: ComputeFallbackMode::Disabled,
        rate_limit_per_minute: 0,
        ..RpcConfig::default()
    }
}

fn build_direct_service(config: &RpcConfig) -> ComputeExecutionService {
    let store: Arc<dyn ObjectStore> = Arc::new(InMemoryObjectStore::new());
    let domains: Arc<dyn DomainRegistry> = {
        let registry = Arc::new(InMemoryDomainRegistry::new());
        registry.upsert_domain(DomainConfig {
            domain_id: DomainId(0),
            name: "main".to_string(),
            vm: "wasm".to_string(),
            public: true,
        });
        registry
    };
    let authorization: Arc<dyn AuthorizationPolicy> = Arc::new(DefaultAuthorizationPolicy);
    let resources: Arc<dyn ResourcePolicy> = Arc::new(NoopResourcePolicy);
    let scheduler = Arc::new(InMemoryComputeScheduler::new(
        config.compute_scheduler_config(),
    ));
    let planner: Arc<dyn ComputeBatchPlanner> = Arc::new(DefaultComputeBatchPlanner::new(
        DefaultComputeConflictPolicy,
    ));
    let runner: Arc<dyn ComputeBatchRunner> = Arc::new(ParallelComputeBatchRunner::new(
        store.clone(),
        authorization,
        resources,
        domains,
    ));

    ComputeExecutionService::new(
        store,
        scheduler,
        planner,
        runner,
        config.compute_fallback_policy(),
    )
}

fn parse_ok(resp: &rabbitapi::rpc::JsonRpcResponse) -> bool {
    resp.result
        .as_ref()
        .and_then(|value| value.get("ok"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn response_debug(resp: &rabbitapi::rpc::JsonRpcResponse) -> String {
    serde_json::to_string(resp).unwrap_or_else(|_| "<unprintable response>".to_string())
}

struct RealRpcBenchmarkResult {
    elapsed: std::time::Duration,
    start_height: u64,
    end_height: u64,
}

fn parse_hex_u64(value: &str) -> Result<u64, String> {
    let trimmed = value.strip_prefix("0x").unwrap_or(value);
    u64::from_str_radix(trimmed, 16).map_err(|err| format!("invalid hex u64 {value:?}: {err}"))
}

fn rpc_response_value(
    resp: rabbitapi::rpc::JsonRpcResponse,
    context: &str,
) -> Result<serde_json::Value, String> {
    if let Some(error) = resp.error {
        return Err(format!(
            "{context} rpc error {}: {}",
            error.code, error.message
        ));
    }
    resp.result
        .ok_or_else(|| format!("{context} rpc response missing result"))
}

async fn rpc_latest_block_number(
    client: &reqwest::Client,
    rpc_url: &str,
    rpc_token: &str,
) -> Result<u64, String> {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "rabbit_getLatestBlock".to_string(),
        params: None,
        id: serde_json::json!("latest"),
    };
    let response = post_real_rpc_request(client, rpc_url, rpc_token, request).await?;
    let block = rpc_response_value(response, "rabbit_getLatestBlock")?;
    let number = block
        .get("number")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "rabbit_getLatestBlock missing block number".to_string())?;
    parse_hex_u64(number)
}

async fn wait_for_stable_latest_block_number(
    client: &reqwest::Client,
    rpc_url: &str,
    rpc_token: &str,
) -> Result<u64, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = rpc_latest_block_number(client, rpc_url, rpc_token).await?;
    let mut stable_reads = 0usize;

    loop {
        if Instant::now() >= deadline {
            return Ok(last);
        }

        tokio::time::sleep(Duration::from_millis(250)).await;
        let current = rpc_latest_block_number(client, rpc_url, rpc_token).await?;
        if current == last {
            stable_reads += 1;
            if stable_reads >= 2 {
                return Ok(current);
            }
        } else {
            last = current;
            stable_reads = 0;
        }
    }
}

async fn rpc_blocks_range_with_body(
    client: &reqwest::Client,
    rpc_url: &str,
    rpc_token: &str,
    from: u64,
    to: u64,
    limit: usize,
) -> Result<Vec<serde_json::Value>, String> {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "rabbit_getBlocksRange".to_string(),
        params: Some(vec![serde_json::json!({
            "from": from,
            "to": to,
            "limit": limit,
            "include_body": true,
        })]),
        id: serde_json::json!(from),
    };
    let response = post_real_rpc_request(client, rpc_url, rpc_token, request).await?;
    let range = rpc_response_value(response, "rabbit_getBlocksRange")?;
    let items = range
        .get("items")
        .and_then(|value| value.as_array())
        .cloned()
        .ok_or_else(|| "rabbit_getBlocksRange missing items".to_string())?;
    Ok(items)
}

async fn log_real_rpc_block_body_counts(
    client: &reqwest::Client,
    rpc_url: &str,
    rpc_token: &str,
    start_height: u64,
    end_height: u64,
) -> Result<(), String> {
    if end_height < start_height {
        return Err(format!(
            "block height moved backwards: start_height={}, end_height={}",
            start_height, end_height
        ));
    }

    println!(
        "[compute_tps] block body sampling: range={}..={} (inclusive)",
        start_height, end_height
    );

    if start_height == end_height {
        println!(
            "[compute_tps] block body sampling: no new blocks observed, sampling current head at height {}",
            start_height
        );
    }

    let mut cursor = start_height;
    let chunk_limit = 200usize;
    let mut block_count = 0u64;
    let mut total_txs = 0u64;
    let mut total_receipts = 0u64;
    let mut body_present_blocks = 0u64;
    let mut body_absent_blocks = 0u64;
    let mut min_txs = u64::MAX;
    let mut max_txs = 0u64;

    while cursor <= end_height {
        let chunk_end = cursor
            .saturating_add(chunk_limit as u64 - 1)
            .min(end_height);
        let items =
            rpc_blocks_range_with_body(client, rpc_url, rpc_token, cursor, chunk_end, chunk_limit)
                .await?;
        let expected = (chunk_end - cursor + 1) as usize;
        if items.len() != expected {
            return Err(format!(
                "rabbit_getBlocksRange returned {} items for heights {}..={}, expected {}",
                items.len(),
                cursor,
                chunk_end,
                expected
            ));
        }

        for item in items.iter().rev() {
            let number = item
                .get("number")
                .and_then(|value| value.as_str())
                .ok_or_else(|| "block sample missing number".to_string())
                .and_then(parse_hex_u64)?;
            let hash = item
                .get("hash")
                .and_then(|value| value.as_str())
                .ok_or_else(|| format!("block {} missing hash", number))?;
            let body = item.get("body").and_then(|value| value.as_object());
            if let Some(body) = body {
                let tx_count = body
                    .get("tx_count")
                    .and_then(|value| value.as_u64())
                    .ok_or_else(|| format!("block {} missing tx_count", number))?;
                let receipt_count = body
                    .get("receipt_count")
                    .and_then(|value| value.as_u64())
                    .ok_or_else(|| format!("block {} missing receipt_count", number))?;

                println!(
                    "[compute_tps] block body: height={}, hash={}, body_present=true, tx_count={}, receipt_count={}",
                    number, hash, tx_count, receipt_count
                );

                block_count += 1;
                total_txs += tx_count;
                total_receipts += receipt_count;
                body_present_blocks += 1;
                min_txs = min_txs.min(tx_count);
                max_txs = max_txs.max(tx_count);
            } else {
                println!(
                    "[compute_tps] block body: height={}, hash={}, body_present=false, tx_count=unavailable, receipt_count=unavailable",
                    number, hash
                );
                body_absent_blocks += 1;
            }
        }

        cursor = chunk_end.saturating_add(1);
    }

    let avg_txs = if block_count > 0 {
        total_txs as f64 / block_count as f64
    } else {
        0.0
    };
    println!(
        "[compute_tps] block body summary: start_height={}, end_height={}, blocks={}, body_present_blocks={}, body_absent_blocks={}, total_txs={}, total_receipts={}, min_txs={}, max_txs={}, avg_txs_per_block={:.2}",
        start_height,
        end_height,
        block_count,
        body_present_blocks,
        body_absent_blocks,
        total_txs,
        total_receipts,
        if block_count > 0 { min_txs } else { 0 },
        max_txs,
        avg_txs
    );

    Ok(())
}

fn temp_root(label: &str, tx_count: usize) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should move forward")
        .as_millis();
    let mut root = env::temp_dir();
    root.push(format!("rabbitchain-tps-{}-{}-{}", label, tx_count, ts));
    root
}

fn temp_backend_path(root: &PathBuf, backend: ComputeBackend) -> PathBuf {
    match backend {
        ComputeBackend::Mem => root.clone(),
        ComputeBackend::RocksDb => root.join("rocksdb"),
        ComputeBackend::Redb => root.join("redb.db"),
    }
}

fn build_persistence_store(backend: ComputeBackend, path: &PathBuf) -> Arc<dyn KeyValueDB> {
    match backend {
        ComputeBackend::Mem => Arc::new(MemDatabase::new()),
        ComputeBackend::RocksDb => {
            Arc::new(RocksDb::open(path.to_str().expect("path utf8")).unwrap())
        }
        ComputeBackend::Redb => {
            Arc::new(RedbDatabase::open(path.to_str().expect("path utf8")).unwrap())
        }
    }
}

fn build_persistence_result_json(tx_id: TxId, index: u64) -> String {
    serde_json::json!({
        "ok": true,
        "tx_id": format!("0x{}", tx_id.0.to_hex()),
        "consumed_inputs": 0,
        "read_objects": 0,
        "created_outputs": 1,
        "submitted_at_unix": index,
    })
    .to_string()
}

fn cleanup_temp_root(root: &PathBuf) {
    if let Err(err) = fs::remove_dir_all(root) {
        if err.kind() != std::io::ErrorKind::NotFound {
            eprintln!(
                "[compute_tps] cleanup warning for {}: {}",
                root.display(),
                err
            );
        }
    }
}

async fn run_rpc_ingress_benchmark(
    label: &str,
    tx_count: usize,
    concurrency: usize,
) -> std::time::Duration {
    let config = build_rpc_config(
        ComputeBackend::Mem,
        String::new(),
        concurrency.max(1).saturating_mul(32),
    );
    let server = RpcServer::new(config).expect("rpc server should initialize");
    let api = server.api().expect("api should be initialized");
    let signer = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);

    let started = Instant::now();
    let (responses_seen, failures, samples) = stream::iter(0..tx_count as u64)
        .map({
            let api = api.clone();
            move |index| {
                let api = api.clone();
                let request = build_compute_rpc_request(index, &signer, "rabbit_simulateComputeTx");
                async move { api.handle_request(request).await }
            }
        })
        .buffer_unordered(concurrency.max(1))
        .fold(
            (0usize, 0usize, Vec::new()),
            |(seen, failures, mut samples), resp| async move {
                let mut failures = failures;
                let seen = seen + 1;
                if !parse_ok(&resp) {
                    failures += 1;
                    if samples.len() < 5 {
                        samples.push(response_debug(&resp));
                    }
                }
                (seen, failures, samples)
            },
        )
        .await;
    let elapsed = started.elapsed();
    drop(api);
    drop(server);

    assert_eq!(responses_seen, tx_count);
    if failures > 0 {
        for sample in samples {
            eprintln!("[compute_tps] {} failure sample: {}", label, sample);
        }
        panic!("{} {} responses were not ok", failures, label);
    }

    elapsed
}

async fn post_real_rpc_request(
    client: &reqwest::Client,
    rpc_url: &str,
    rpc_token: &str,
    request: JsonRpcRequest,
) -> Result<rabbitapi::rpc::JsonRpcResponse, String> {
    let response = client
        .post(rpc_url)
        .bearer_auth(rpc_token)
        .json(&request)
        .send()
        .await
        .map_err(|err| format!("request failed: {err}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|err| format!("failed to read response body: {err}"))?;

    if !status.is_success() {
        return Err(format!("http status {} body {}", status, body));
    }

    serde_json::from_str(&body)
        .map_err(|err| format!("failed to decode rpc response: {err}; body={body}"))
}

async fn run_submit_benchmark(
    label: &str,
    rpc_urls: Vec<String>,
    rpc_token: &str,
    tx_count: usize,
    concurrency: usize,
) -> Result<RealRpcBenchmarkResult, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|err| format!("submit benchmark client should build: {err}"))?;
    let signer = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
    let rpc_urls = Arc::new(rpc_urls);
    let rpc_token = rpc_token.to_string();
    let sample_rpc_url = rpc_urls
        .first()
        .cloned()
        .ok_or_else(|| "submit benchmark requires at least one rpc url".to_string())?;
    let start_height = rpc_latest_block_number(&client, &sample_rpc_url, &rpc_token).await?;

    let started = Instant::now();
    let (responses_seen, failures, samples) = stream::iter(0..tx_count as u64)
        .map({
            let client = client.clone();
            let rpc_urls = rpc_urls.clone();
            let rpc_token = rpc_token.clone();
            move |index| {
                let client = client.clone();
                let rpc_urls = rpc_urls.clone();
                let rpc_token = rpc_token.clone();
                let rpc_url = rpc_urls[(index as usize) % rpc_urls.len()].clone();
                let request = build_compute_rpc_request(index, &signer, "rabbit_submitComputeTx");
                async move { post_real_rpc_request(&client, &rpc_url, &rpc_token, request).await }
            }
        })
        .buffer_unordered(concurrency.max(1))
        .fold(
            (0usize, 0usize, Vec::new()),
            |(seen, failures, mut samples), result| async move {
                let mut failures = failures;
                let seen = seen + 1;
                match result {
                    Ok(resp) if parse_ok(&resp) => {}
                    Ok(resp) => {
                        failures += 1;
                        if samples.len() < 5 {
                            samples.push(response_debug(&resp));
                        }
                    }
                    Err(err) => {
                        failures += 1;
                        if samples.len() < 5 {
                            samples.push(err);
                        }
                    }
                }
                (seen, failures, samples)
            },
        )
        .await;
    let elapsed = started.elapsed();
    let end_height =
        wait_for_stable_latest_block_number(&client, &sample_rpc_url, &rpc_token).await?;

    assert_eq!(responses_seen, tx_count);
    if failures > 0 {
        for sample in samples {
            eprintln!("[compute_tps] {} failure sample: {}", label, sample);
        }
        panic!("{} {} responses were not ok", failures, label);
    }

    println!(
        "[compute_tps] {}: txs={}, rpc_urls={}, concurrency={}, start_height={}, end_height={}, elapsed={:?}",
        label,
        tx_count,
        rpc_urls.len(),
        concurrency,
        start_height,
        end_height,
        elapsed
    );

    Ok(RealRpcBenchmarkResult {
        elapsed,
        start_height,
        end_height,
    })
}

fn run_direct_benchmark(
    tx_count: usize,
    config: &RpcConfig,
    flush_every: usize,
) -> std::time::Duration {
    let direct_service = build_direct_service(config);
    let signer = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
    let flush_every = flush_every.max(1);
    let mut accepted = 0usize;
    let started = Instant::now();

    for index in 0..tx_count as u64 {
        let tx = build_signed_mint_tx(index, &signer);
        direct_service
            .submit(tx)
            .expect("direct submit should succeed");
        if (index as usize + 1) % flush_every == 0 {
            let direct_outcomes = direct_service
                .flush_ready()
                .expect("direct flush should succeed");
            assert!(direct_outcomes.iter().all(|outcome| outcome.accepted));
            accepted += direct_outcomes.len();
        }
    }
    let direct_outcomes = direct_service
        .flush_ready()
        .expect("direct flush should succeed");
    let elapsed = started.elapsed();

    assert!(direct_outcomes.iter().all(|outcome| outcome.accepted));
    accepted += direct_outcomes.len();
    assert_eq!(accepted, tx_count);

    elapsed
}

fn run_persistence_benchmark(
    backend: ComputeBackend,
    tx_count: usize,
    batch_size: usize,
) -> std::time::Duration {
    let root = temp_root(backend.as_str(), tx_count);
    if let Err(err) = fs::create_dir_all(&root) {
        panic!(
            "failed to create temp persistence root {}: {}",
            root.display(),
            err
        );
    }

    let backend_path = temp_backend_path(&root, backend);
    let db: Arc<dyn KeyValueDB> = build_persistence_store(backend, &backend_path);
    let store = ComputeStore::new(db.clone());
    let batch_size = batch_size.max(1);
    let mut batch = Vec::with_capacity(batch_size);
    let started = Instant::now();

    for index in 0..tx_count as u64 {
        let tx_id = TxId(seed_hash("persist-tx", index));
        batch.push((tx_id, build_persistence_result_json(tx_id, index)));
        if batch.len() >= batch_size {
            store
                .put_tx_results_batch(&batch)
                .expect("persistence batch write should succeed");
            batch.clear();
        }
    }

    if !batch.is_empty() {
        store
            .put_tx_results_batch(&batch)
            .expect("final persistence batch write should succeed");
    }

    if tx_count > 0 {
        let sample_id = TxId(seed_hash("persist-tx", 0));
        assert!(store
            .get_tx_result(sample_id)
            .expect("persistence readback should succeed")
            .is_some());
    }

    let elapsed = started.elapsed();
    drop(store);
    drop(db);
    cleanup_temp_root(&root);
    elapsed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore]
async fn compute_tps_benchmark() {
    let tx_count = env_usize("RABBIT_TPS_TX_COUNT", 1024);
    let direct_flush_every = env_usize("RABBIT_TPS_DIRECT_FLUSH_EVERY", 2048);
    let ingress_concurrency = env_usize("RABBIT_TPS_INGRESS_CONCURRENCY", 256);
    let persist_batch_size = env_usize("RABBIT_TPS_PERSIST_BATCH_SIZE", 1);

    let ingress_elapsed =
        run_rpc_ingress_benchmark("rpc ingress", tx_count, ingress_concurrency).await;
    let ingress_tps = tx_count as f64 / ingress_elapsed.as_secs_f64();
    println!(
        "[compute_tps] rpc ingress (simulate): txs={}, concurrency={}, elapsed={:?}, tps={:.2}",
        tx_count, ingress_concurrency, ingress_elapsed, ingress_tps
    );

    let direct_max_pending = tx_count.max(direct_flush_every).max(256);
    let direct_config = build_rpc_config(ComputeBackend::Mem, String::new(), direct_max_pending);
    let direct_elapsed = run_direct_benchmark(tx_count, &direct_config, direct_flush_every);
    let direct_tps = tx_count as f64 / direct_elapsed.as_secs_f64();
    println!(
        "[compute_tps] execution (direct): txs={}, flush_every={}, max_pending={}, elapsed={:?}, tps={:.2}",
        tx_count, direct_flush_every, direct_max_pending, direct_elapsed, direct_tps
    );

    for backend in [ComputeBackend::RocksDb, ComputeBackend::Redb] {
        let elapsed = run_persistence_benchmark(backend, tx_count, persist_batch_size);
        let tps = tx_count as f64 / elapsed.as_secs_f64();
        println!(
            "[compute_tps] persistence ({}): txs={}, batch_size={}, elapsed={:?}, tps={:.2}",
            backend.as_str(),
            tx_count,
            persist_batch_size,
            elapsed,
            tps
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore]
async fn compute_tps_submit_benchmark() {
    let rpc_token = env::var("RABBIT_TPS_RPC_TOKEN")
        .expect("RABBIT_TPS_RPC_TOKEN must be set for the submit benchmark");
    let rpc_urls = env_rpc_urls();
    let sample_rpc_url = rpc_urls
        .first()
        .cloned()
        .expect("at least one RPC URL is required");
    let tx_count = env_usize("RABBIT_TPS_TX_COUNT", 1024);
    let ingress_concurrency = env_usize("RABBIT_TPS_INGRESS_CONCURRENCY", 256);

    let result = run_submit_benchmark(
        "submit benchmark",
        rpc_urls,
        &rpc_token,
        tx_count,
        ingress_concurrency,
    )
    .await
    .expect("submit benchmark should succeed");
    let tps = tx_count as f64 / result.elapsed.as_secs_f64();
    println!(
        "[compute_tps] rpc submit benchmark (real http): txs={}, concurrency={}, start_height={}, end_height={}, elapsed={:?}, tps={:.2}",
        tx_count,
        ingress_concurrency,
        result.start_height,
        result.end_height,
        result.elapsed,
        tps
    );

    log_real_rpc_block_body_counts(
        &reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("real rpc benchmark log client should build"),
        &sample_rpc_url,
        &rpc_token,
        result.start_height,
        result.end_height,
    )
    .await
    .expect("block body sampling should succeed");
}
