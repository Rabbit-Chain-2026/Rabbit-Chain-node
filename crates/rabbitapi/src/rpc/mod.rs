//! JSON-RPC Server Implementation

mod compute_adapter;

use axum::{
    extract::{ConnectInfo, DefaultBodyLimit, State},
    http::HeaderMap,
    routing::post,
    Json, Router,
};
use once_cell::sync::OnceCell;
use parking_lot::RwLock;
use prometheus::{Encoder, IntCounterVec, IntGauge, Registry, TextEncoder};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::oneshot;
use tower_http::cors::{Any, CorsLayer};
use rabbitcore::account::{Account, AccountState, U256};
use rabbitcore::block::{
    compute_receipts_root, compute_transactions_root, create_genesis_block, pow_hash_meets_target,
    pow_target_from_difficulty, pow_target_from_hex, pow_target_to_hex, Block, BlockBody,
    BlockBodyRecord, BlockHeader, Receipt, ReceiptLog, ReceiptStatus, TxEnvelope,
    CANONICAL_BLOCK_VERSION,
};
use rabbitcore::compute::domain::DomainRegistry;
use rabbitcore::compute::{
    batch::ComputeFallbackMode, scheduler::ComputeLaneStrategy, Command, ComputeTx, DomainConfig,
    DomainId, GAME_DOMAIN, InMemoryDomainRegistry, InMemoryObjectStore, ObjectId, ObjectKind,
    ObjectOutput, ObjectStore, OutputId, OutputProposal, Ownership, ResourceMap, ResourceValue,
    Script, SignatureScheme, TxSignature, TxWitness, Version,
};
use rabbitcore::crypto::{Address, Hash};
use rabbitcore::state::{StateDb, executor::StateExecutor};
use rabbitnet::{
    configure_global_block_activation_height, global_block_activation_height,
    global_block_body_by_hash, global_block_body_by_number, global_block_by_hash,
    global_block_by_number, global_block_receipt_by_tx_hash, global_block_receipts_by_hash,
    global_block_requires_body, global_block_version_for_height, global_latest_block,
    global_peer_count, global_peers, global_record_account, global_record_compute_tx,
    global_store_block, global_store_block_with_body, global_synced_account,
    global_synced_compute_tx, global_synced_compute_txs, global_synced_height,
    set_global_synced_height, SyncComputeTxRecord,
};
use rabbitstore::db::{KeyValueDB, MemDatabase, RedbDatabase, RocksDb};
use rabbitstore::ComputeStore;

use compute_adapter::RpcComputeAdapter;

static RPC_METRICS: RpcMetricsHandle = RpcMetricsHandle(OnceCell::new());

/// Callback type for broadcasting a block to the P2P network.
pub type BlockBroadcaster = Arc<dyn Fn(&rabbitcore::block::Block) + Send + Sync + 'static>;
static BLOCK_BROADCASTER: OnceCell<parking_lot::RwLock<Option<BlockBroadcaster>>> = OnceCell::new();

/// Register the block broadcaster callback.
pub fn set_block_broadcaster(broadcaster: Option<BlockBroadcaster>) {
    let cell = BLOCK_BROADCASTER.get_or_init(|| parking_lot::RwLock::new(None));
    *cell.write() = broadcaster;
}
const MAX_MINING_JOBS: usize = 2_048;
const MAX_MINING_JOB_AGE_SECS: u64 = 300;
const MAX_MINER_EXTRA_DATA_BYTES: usize = 64;
const MAX_SEEN_MINING_SUBMISSIONS: usize = 16_384;
const MAX_BLOCK_HISTORY: usize = 50_000;
const MAX_SUBMITTED_COMPUTE_RESULTS: usize = 50_000;
const MAX_GET_WORK_WAIT_SECS: u64 = 15;
const GET_WORK_WAIT_POLL_MILLIS: u64 = 100;
const TARGET_BLOCK_INTERVAL_SECS: u64 = 10;
// Keep the minimum difficulty floor low (1) so tests can mine blocks quickly.
// The production minimum is set at the chain level via MIN_MINING_DIFFICULTY
// in rabbitnet::sync; this constant only guards the RPC-side difficulty clamp.
const MIN_MINING_DIFFICULTY: u128 = 1;
const BASE_MINING_DIFFICULTY: u128 = 1;
const MAX_MINING_DIFFICULTY: u128 = 1_000_000_000;
const POW_TARGET_HEADER_VERSION: u32 = 2;

struct RpcMetrics {
    registry: Registry,
    method_calls: IntCounterVec,
    method_errors: IntCounterVec,
    mining_shares_accepted: IntCounterVec,
    mining_shares_rejected: IntCounterVec,
    latest_block_height: IntGauge,
}

struct RpcMetricsHandle(OnceCell<RpcMetrics>);

impl RpcMetricsHandle {
    fn get(&self) -> Result<&RpcMetrics, RpcErrorObject> {
        self.0.get_or_try_init(RpcMetrics::try_new).map_err(|err| {
            RpcErrorObject::internal_error(format!("rpc metrics initialization failed: {err}"))
        })
    }

    fn init(&self) -> std::result::Result<(), crate::ApiError> {
        self.0
            .get_or_try_init(RpcMetrics::try_new)
            .map(|_| ())
            .map_err(|err| {
                crate::ApiError::Internal(format!("rpc metrics initialization failed: {err}"))
            })
    }
}

impl RpcMetrics {
    fn try_new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();
        let method_calls = IntCounterVec::new(
            prometheus::Opts::new("rabbit_rpc_method_calls_total", "RPC method call count"),
            &["method"],
        )?;
        let method_errors = IntCounterVec::new(
            prometheus::Opts::new("rabbit_rpc_method_errors_total", "RPC method error count"),
            &["method"],
        )?;
        let mining_shares_accepted = IntCounterVec::new(
            prometheus::Opts::new(
                "rabbit_mining_shares_accepted_total",
                "Accepted mining shares",
            ),
            &["source"],
        )?;
        let mining_shares_rejected = IntCounterVec::new(
            prometheus::Opts::new(
                "rabbit_mining_shares_rejected_total",
                "Rejected mining shares",
            ),
            &["reason"],
        )?;
        let latest_block_height = IntGauge::new(
            "rabbit_latest_block_height",
            "Latest block height observed by RPC",
        )?;

        registry.register(Box::new(method_calls.clone()))?;
        registry.register(Box::new(method_errors.clone()))?;
        registry.register(Box::new(mining_shares_accepted.clone()))?;
        registry.register(Box::new(mining_shares_rejected.clone()))?;
        registry.register(Box::new(latest_block_height.clone()))?;

        Ok(Self {
            registry,
            method_calls,
            method_errors,
            mining_shares_accepted,
            mining_shares_rejected,
            latest_block_height,
        })
    }

    fn render(&self) -> Result<String, RpcErrorObject> {
        let families = self.registry.gather();
        let mut out = Vec::new();
        TextEncoder::new()
            .encode(&families, &mut out)
            .map_err(|e| RpcErrorObject::internal_error(format!("encode metrics failed: {e}")))?;
        String::from_utf8(out)
            .map_err(|e| RpcErrorObject::internal_error(format!("metrics utf8 failed: {e}")))
    }
}

/// RPC configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RpcConfig {
    /// Listen address
    pub address: String,
    /// Port
    pub port: u16,
    /// Max connections
    pub max_connections: u32,
    /// Max request body size
    pub max_request_size: usize,
    /// Enabled modules
    pub modules: Vec<String>,
    /// Compute persistent backend kind.
    pub compute_backend: ComputeBackend,
    /// Database path for file-based backends (rocksdb/redb)
    pub compute_db_path: String,
    /// Chain identifier used by compute context.
    pub chain_id: u64,
    /// Network id returned by net_version.
    pub network_id: u64,
    /// Mining reward address.
    pub coinbase: String,
    /// Optional rotation set of mining reward addresses. When populated, each
    /// work template is bound to one address from this set in round-robin order.
    #[serde(default)]
    pub coinbase_addresses: Vec<String>,
    /// Whether mining RPC methods are enabled (`rabbit_getWork` / `rabbit_submitWork`).
    pub mining_enabled: bool,
    /// Optional legacy override for the mining work target as a count of leading zero bytes.
    pub mining_work_target_leading_rabbit_bytes: Option<usize>,
    /// Optional activation height for canonical block-body blocks.
    pub canonical_block_activation_height: Option<u64>,
    /// Compute batch window in milliseconds.
    pub compute_batch_window_ms: u64,
    /// Maximum number of compute txs per batch slice.
    pub compute_max_batch_size: usize,
    /// Maximum number of pending compute txs admitted before backpressure.
    pub compute_max_pending: usize,
    /// Lane partitioning strategy for compute batching.
    pub compute_lane_strategy: ComputeLaneStrategy,
    /// Failure fallback mode for compute batching.
    pub compute_fallback_mode: ComputeFallbackMode,
    /// Optional static auth token for all JSON-RPC requests.
    pub auth_token: Option<String>,
    /// Per-client request budget per rolling minute. `0` means disabled.
    pub rate_limit_per_minute: u32,
    /// When enabled, `rabbit_submitComputeTx` rejects transactions that carry no
    /// fee fields at all (max_fee=0, priority_fee=0, gas_limit=0). Legacy clients
    /// that submit fee-less txs must set this to `false` (the default).
    #[serde(default)]
    pub require_fee_for_compute_tx: bool,
}

/// Persistent backend for compute storage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComputeBackend {
    /// In-memory backend.
    #[default]
    Mem,
    /// RocksDB backend.
    RocksDb,
    /// Redb backend.
    Redb,
}

impl ComputeBackend {
    /// Returns a stable string representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mem => "mem",
            Self::RocksDb => "rocksdb",
            Self::Redb => "redb",
        }
    }
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            address: "127.0.0.1".to_string(),
            port: 8545,
            max_connections: 100,
            max_request_size: 15 * 1024 * 1024, // 15MB
            modules: vec!["rabbit".into(), "net".into(), "web3".into()],
            compute_backend: ComputeBackend::Mem,
            compute_db_path: "./data/compute-db".to_string(),
            chain_id: 10086,
            network_id: 10086,
            coinbase: "0x0000000000000000000000000000000000000000".to_string(),
            coinbase_addresses: Vec::new(),
            mining_enabled: false,
            mining_work_target_leading_rabbit_bytes: None,
            canonical_block_activation_height: None,
            compute_batch_window_ms: 15,
            compute_max_batch_size: 64,
            compute_max_pending: 4_096,
            compute_lane_strategy: ComputeLaneStrategy::ByDomain,
            compute_fallback_mode: ComputeFallbackMode::SerialOnFailure,
            auth_token: None,
            rate_limit_per_minute: 600,
            require_fee_for_compute_tx: false,
        }
    }
}

impl RpcConfig {
    /// Validates RPC configuration consistency.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.chain_id == 0 {
            return Err("chain_id must be non-zero".to_string());
        }
        if self.network_id == 0 {
            return Err("network_id must be non-zero".to_string());
        }
        if parse_address(&self.coinbase).is_err() {
            return Err("coinbase must be a valid 20-byte address with 0x prefix".to_string());
        }
        for address in &self.coinbase_addresses {
            if parse_address(address).is_err() {
                return Err(format!(
                    "coinbase_addresses contains invalid address: {}",
                    address
                ));
            }
        }
        if let Some(target) = self.mining_work_target_leading_rabbit_bytes {
            if target > 32 {
                return Err(
                    "mining_work_target_leading_rabbit_bytes must be between 0 and 32".to_string(),
                );
            }
        }
        if let Some(height) = self.canonical_block_activation_height {
            if height == 0 {
                return Err(
                    "canonical_block_activation_height must be greater than zero".to_string(),
                );
            }
        }
        if self.compute_batch_window_ms > 3_600_000 {
            return Err("compute_batch_window_ms is unreasonably large".to_string());
        }
        if self.compute_max_batch_size == 0 {
            return Err("compute_max_batch_size must be non-zero".to_string());
        }
        if self.compute_max_pending == 0 {
            return Err("compute_max_pending must be non-zero".to_string());
        }
        if let Some(token) = &self.auth_token {
            if token.trim().is_empty() {
                return Err("auth_token cannot be empty".to_string());
            }
        }
        match self.compute_backend {
            ComputeBackend::Mem => Ok(()),
            ComputeBackend::RocksDb | ComputeBackend::Redb => {
                if self.compute_db_path.trim().is_empty() {
                    return Err(format!(
                        "compute_db_path cannot be empty when compute_backend={}",
                        self.compute_backend.as_str()
                    ));
                }
                Ok(())
            }
        }
    }

    /// Returns a compute scheduler config derived from this RPC config.
    pub fn compute_scheduler_config(&self) -> rabbitcore::compute::scheduler::ComputeSchedulerConfig {
        rabbitcore::compute::scheduler::ComputeSchedulerConfig {
            batch_window_ms: self.compute_batch_window_ms,
            max_batch_size: self.compute_max_batch_size,
            max_pending: self.compute_max_pending,
            lane_strategy: std::sync::Arc::new(self.compute_lane_strategy),
        }
    }

    /// Returns the compute fallback policy selected by this config.
    pub fn compute_fallback_policy(
        &self,
    ) -> std::sync::Arc<dyn rabbitcore::compute::batch::ComputeFallbackPolicy> {
        self.compute_fallback_mode.build_policy()
    }
}

/// JSON-RPC request
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Option<Vec<serde_json::Value>>,
    pub id: serde_json::Value,
}

/// JSON-RPC response
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcErrorObject>,
    pub id: serde_json::Value,
}

/// RPC error object
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RpcErrorObject {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl RpcErrorObject {
    pub fn parse_error() -> Self {
        Self {
            code: -32700,
            message: "Parse error".into(),
            data: None,
        }
    }

    pub fn invalid_request() -> Self {
        Self {
            code: -32600,
            message: "Invalid Request".into(),
            data: None,
        }
    }

    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("Method not found: {}", method),
            data: None,
        }
    }

    pub fn invalid_params(message: String) -> Self {
        Self {
            code: -32602,
            message: "Invalid params".into(),
            data: Some(serde_json::Value::String(message)),
        }
    }

    pub fn internal_error(message: String) -> Self {
        Self {
            code: -32603,
            message: "Internal error".into(),
            data: Some(serde_json::Value::String(message)),
        }
    }
}

impl std::fmt::Display for RpcErrorObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for RpcErrorObject {}

/// A transaction in the fee-priority pool, ordered by effective tip rate.
#[derive(Clone, Debug)]
struct PendingTx {
    tx: ComputeTx,
    /// effective_tip / estimated_gas (prioritization score)
    tip_rate: u64,
    /// 提交序号：同 tip 费率按提交序 FIFO（依赖交易如 mint→settle 顺序打包）
    seq: u64,
}

impl PendingTx {
    fn new(tx: ComputeTx, base_fee: u64, seq: u64) -> Self {
        let tip_rate = rabbitcore::compute::effective_tip_rate(&tx, base_fee);
        Self { tx, tip_rate, seq }
    }
}

impl PartialEq for PendingTx {
    fn eq(&self, other: &Self) -> bool {
        self.tip_rate == other.tip_rate && self.seq == other.seq
    }
}
impl Eq for PendingTx {}
impl PartialOrd for PendingTx {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for PendingTx {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // BinaryHeap is max-heap: 高 tip 优先；同 tip 早提交（小 seq）优先
        self.tip_rate
            .cmp(&other.tip_rate)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

/// RPC API handler
pub struct RpcApi {
    config: RpcConfig,
    state_db: Arc<StateDb>,
    latest_block: RwLock<Option<Block>>,
    block_history: RwLock<BTreeMap<u64, Block>>,
    block_bodies: RwLock<BTreeMap<u64, BlockBodyRecord>>,
    compute_store: Arc<dyn ObjectStore>,
    domain_registry: Arc<InMemoryDomainRegistry>,
    compute_adapter: Arc<RpcComputeAdapter>,
    submitted_compute_results: RwLock<HashMap<Hash, serde_json::Value>>,
    submitted_compute_order: RwLock<VecDeque<Hash>>,
    persistent_compute_store: Option<Arc<ComputeStore>>,
    mining_jobs: RwLock<HashMap<String, MiningWork>>,
    mining_job_order: RwLock<VecDeque<String>>,
    mining_seen_submissions: RwLock<HashSet<SeenShareKey>>,
    mining_seen_submission_order: RwLock<VecDeque<SeenShareKey>>,
    hashrate_counter: RwLock<u64>,
    next_coinbase_index: AtomicUsize,
    /// Fee-priority transaction pool for miner selection (EIP-1559).
    tx_fee_pool: RwLock<BinaryHeap<PendingTx>>,
    /// 提交序号（同 tip 费率按提交序 FIFO 打包）
    pending_seq: AtomicUsize,
    /// Current base fee (carrots) updated after each block.
    current_base_fee: RwLock<u64>,
    /// BlockTime 状态执行器（产块时执行打包交易 → 真 receipts + 国库）
    state_executor: Arc<StateExecutor>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MiningWork {
    work_id: String,
    prev_hash: Hash,
    height: u64,
    target: U256,
    /// 新块难度（adjust 后）：矿工必须用它哈希
    difficulty: U256,
    created_at_secs: u64,
    /// 区块头时间戳（getWork 时固定）：矿工与校验必须一致
    header_timestamp: u64,
    coinbase: Address,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SeenShareKey {
    work_id: String,
    nonce: u64,
    hash: [u8; 32],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SubmitWorkRequest {
    work_id: String,
    nonce: u64,
    hash_hex: String,
    miner: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct GetWorkRequest {
    #[serde(default)]
    known_prev_hash: Option<String>,
    #[serde(default)]
    known_height: Option<u64>,
    #[serde(default)]
    wait: bool,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

impl RpcApi {
    pub fn new(config: RpcConfig, state_db: Arc<StateDb>) -> Self {
        configure_global_block_activation_height(config.canonical_block_activation_height);
        let compute_store: Arc<dyn ObjectStore> = Arc::new(InMemoryObjectStore::new());
        let domain_registry = Arc::new(InMemoryDomainRegistry::new());
        domain_registry.upsert_domain(DomainConfig {
            domain_id: DomainId(0),
            name: "main".to_string(),
            vm: "wasm".to_string(),
            public: true,
        });
        // 游戏域（"jzz"）：山海等 RabbitChain 原生应用的游戏交易/对象域。
        domain_registry.upsert_domain(DomainConfig {
            domain_id: GAME_DOMAIN,
            name: "jzz".to_string(),
            vm: "shanhai".to_string(),
            public: true,
        });
        let compute_adapter = Arc::new(RpcComputeAdapter::new_with_config(
            compute_store.clone(),
            domain_registry.clone(),
            &config,
        ));

        let state_executor = Arc::new(StateExecutor::new_with_compute(
            state_db.clone(),
            config.chain_id,
            compute_store.clone(),
            domain_registry.clone(),
            rabbitcore::governance::treasury_address(),
        ));

        Self {
            config,
            state_db,
            latest_block: RwLock::new(None),
            block_history: RwLock::new(BTreeMap::new()),
            block_bodies: RwLock::new(BTreeMap::new()),
            compute_store,
            domain_registry,
            compute_adapter,
            submitted_compute_results: RwLock::new(HashMap::new()),
            submitted_compute_order: RwLock::new(VecDeque::new()),
            persistent_compute_store: None,
            mining_jobs: RwLock::new(HashMap::new()),
            mining_job_order: RwLock::new(VecDeque::new()),
            mining_seen_submissions: RwLock::new(HashSet::new()),
            mining_seen_submission_order: RwLock::new(VecDeque::new()),
            hashrate_counter: RwLock::new(0),
            next_coinbase_index: AtomicUsize::new(0),
            tx_fee_pool: RwLock::new(BinaryHeap::new()),
            pending_seq: AtomicUsize::new(0),
            current_base_fee: RwLock::new(rabbitcore::compute::INITIAL_BASE_FEE),
            state_executor,
        }
    }

    /// Construct RPC API with injected compute backends.
    pub fn with_compute(
        config: RpcConfig,
        state_db: Arc<StateDb>,
        compute_store: Arc<dyn ObjectStore>,
        domain_registry: Arc<InMemoryDomainRegistry>,
    ) -> Self {
        configure_global_block_activation_height(config.canonical_block_activation_height);
        let compute_adapter = Arc::new(RpcComputeAdapter::new_with_config(
            compute_store.clone(),
            domain_registry.clone(),
            &config,
        ));
        let state_executor = Arc::new(StateExecutor::new_with_compute(
            state_db.clone(),
            config.chain_id,
            compute_store.clone(),
            domain_registry.clone(),
            rabbitcore::governance::treasury_address(),
        ));
        Self {
            config,
            state_db,
            latest_block: RwLock::new(None),
            block_history: RwLock::new(BTreeMap::new()),
            block_bodies: RwLock::new(BTreeMap::new()),
            compute_store,
            domain_registry,
            compute_adapter,
            submitted_compute_results: RwLock::new(HashMap::new()),
            submitted_compute_order: RwLock::new(VecDeque::new()),
            persistent_compute_store: None,
            mining_jobs: RwLock::new(HashMap::new()),
            mining_job_order: RwLock::new(VecDeque::new()),
            mining_seen_submissions: RwLock::new(HashSet::new()),
            mining_seen_submission_order: RwLock::new(VecDeque::new()),
            hashrate_counter: RwLock::new(0),
            next_coinbase_index: AtomicUsize::new(0),
            tx_fee_pool: RwLock::new(BinaryHeap::new()),
            pending_seq: AtomicUsize::new(0),
            current_base_fee: RwLock::new(rabbitcore::compute::INITIAL_BASE_FEE),
            state_executor,
        }
    }

    /// Construct RPC API with durable compute store.
    pub fn with_persistent_compute(
        config: RpcConfig,
        state_db: Arc<StateDb>,
        compute_store: Arc<ComputeStore>,
        domain_registry: Arc<InMemoryDomainRegistry>,
    ) -> Self {
        configure_global_block_activation_height(config.canonical_block_activation_height);
        let compute_store_dyn: Arc<dyn ObjectStore> = compute_store.clone();
        let compute_adapter = Arc::new(RpcComputeAdapter::new_with_config(
            compute_store_dyn.clone(),
            domain_registry.clone(),
            &config,
        ));
        let state_executor = Arc::new(StateExecutor::new_with_compute(
            state_db.clone(),
            config.chain_id,
            compute_store.clone(),
            domain_registry.clone(),
            rabbitcore::governance::treasury_address(),
        ));
        Self {
            config,
            state_db,
            latest_block: RwLock::new(None),
            block_history: RwLock::new(BTreeMap::new()),
            block_bodies: RwLock::new(BTreeMap::new()),
            compute_store: compute_store_dyn,
            domain_registry,
            compute_adapter,
            submitted_compute_results: RwLock::new(HashMap::new()),
            submitted_compute_order: RwLock::new(VecDeque::new()),
            persistent_compute_store: Some(compute_store),
            mining_jobs: RwLock::new(HashMap::new()),
            mining_job_order: RwLock::new(VecDeque::new()),
            mining_seen_submissions: RwLock::new(HashSet::new()),
            mining_seen_submission_order: RwLock::new(VecDeque::new()),
            hashrate_counter: RwLock::new(0),
            next_coinbase_index: AtomicUsize::new(0),
            tx_fee_pool: RwLock::new(BinaryHeap::new()),
            pending_seq: AtomicUsize::new(0),
            current_base_fee: RwLock::new(rabbitcore::compute::INITIAL_BASE_FEE),
            state_executor,
        }
    }

    /// Handle RPC request
    pub async fn handle_request(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let metrics = match RPC_METRICS.get() {
            Ok(metrics) => metrics,
            Err(error) => {
                return JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    result: None,
                    error: Some(error),
                    id: request.id,
                };
            }
        };
        metrics
            .method_calls
            .with_label_values(&[request.method.as_str()])
            .inc();
        let result = self.dispatch_method(&request.method, request.params).await;

        match result {
            Ok(value) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                result: Some(value),
                error: None,
                id: request.id,
            },
            Err(error) => {
                metrics
                    .method_errors
                    .with_label_values(&[request.method.as_str()])
                    .inc();
                JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    result: None,
                    error: Some(error),
                    id: request.id,
                }
            }
        }
    }

    /// Dispatch method call
    async fn dispatch_method(
        &self,
        method: &str,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        match method {
            // RabbitChain info methods
            "rabbit_clientVersion" => self.rabbit_client_version(params),
            "rabbit_keccak256" => self.rabbit_keccak256(params),

            // net_* methods
            "net_version" => self.net_version(params),
            "net_peerCount" => self.net_peer_count(params),
            "net_listening" => self.net_listening(params),

            // RabbitChain extensions
            "rabbit_getAccount" => self.rabbit_get_account(params),
            "rabbit_getUtxos" => self.rabbit_get_utxos(params),
            "rabbit_getObject" => self.rabbit_get_object(params),
            "rabbit_getOutput" => self.rabbit_get_output(params),
            "rabbit_getDomain" => self.rabbit_get_domain(params),
            "rabbit_listDomains" => self.rabbit_list_domains(params),
            "rabbit_simulateComputeTx" => self.rabbit_simulate_compute_tx(params),
            "rabbit_submitComputeTx" => self.rabbit_submit_compute_tx(params).await,
            "rabbit_getComputeTxResult" => self.rabbit_get_compute_tx_result(params),
            "rabbit_listComputeTxResults" => self.rabbit_list_compute_tx_results(params),
            "rabbit_getOperationByHash" => self.rabbit_get_operation_by_hash(params),
            "rabbit_listOperations" => self.rabbit_list_operations(params),
            "rabbit_getOperationsByAddress" => self.rabbit_get_operations_by_address(params),
            "rabbit_gasPrice" => self.rabbit_gas_price(params),
            "rabbit_estimateGas" => self.rabbit_estimate_gas(params),
            "rabbit_pendingTransactions" => self.rabbit_pending_transactions(params),
            "rabbit_getWork" => self.rabbit_get_work(params).await,
            "rabbit_submitWork" => self.rabbit_submit_work(params),
            "rabbit_getLatestBlock" => self.rabbit_get_latest_block(params),
            "rabbit_syncStatus" => self.rabbit_sync_status(params),
            "rabbit_getBlockByHash" => self.rabbit_get_block_by_hash(params),
            "rabbit_getBlockByNumber" => self.rabbit_get_block_by_number(params),
            "rabbit_getBlockBody" => self.rabbit_get_block_body(params),
            "rabbit_getBlockReceipts" => self.rabbit_get_block_receipts(params),
            "rabbit_getReceipt" => self.rabbit_get_receipt(params),
            "rabbit_getBlocksRange" => self.rabbit_get_blocks_range(params),
            "rabbit_importBlock" => self.rabbit_import_block(params),
            "rabbit_getMetrics" => self.rabbit_get_metrics(params),
            "rabbit_peers" => self.rabbit_peers(params),

            _ => Err(RpcErrorObject::method_not_found(method)),
        }
    }

    // ============ RabbitChain info methods ============

    fn rabbit_client_version(
        &self,
        _params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        Ok(serde_json::json!("RabbitChain/v0.1.0"))
    }

    fn rabbit_keccak256(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let params = params.ok_or(RpcErrorObject::invalid_params("Missing params".to_string()))?;
        let data = params
            .first()
            .ok_or_else(|| RpcErrorObject::invalid_params("Missing data".to_string()))?
            .as_str()
            .ok_or_else(|| RpcErrorObject::invalid_params("Data must be string".to_string()))?;

        let bytes = hex::decode(data.strip_prefix("0x").unwrap_or(data))
            .map_err(|e| RpcErrorObject::invalid_params(format!("Invalid hex: {}", e)))?;

        let hash = rabbitcore::crypto::keccak256(&bytes);

        Ok(serde_json::json!(format!("0x{}", hex::encode(hash))))
    }

    // ============ net_* methods ============

    fn net_version(
        &self,
        _params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        Ok(serde_json::json!(self.config.network_id.to_string()))
    }

    fn net_peer_count(
        &self,
        _params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        Ok(serde_json::json!(format!("0x{:x}", global_peer_count())))
    }

    fn net_listening(
        &self,
        _params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        Ok(serde_json::json!(true))
    }

    // ============ RabbitChain extensions ============

    fn rabbit_get_account(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let params = params.ok_or(RpcErrorObject::invalid_params("Missing params".to_string()))?;

        let address_str = params
            .first()
            .ok_or_else(|| RpcErrorObject::invalid_params("Missing address".to_string()))?
            .as_str()
            .ok_or_else(|| RpcErrorObject::invalid_params("Address must be string".to_string()))?;

        let address = parse_address(address_str)?;
        let local = self.state_db.get_account(&address);
        let synced = global_synced_account(&address);
        let account = match (local, synced) {
            (Some(local), Some(synced)) => {
                if synced.updated_at > local.updated_at {
                    Some(synced)
                } else {
                    Some(local)
                }
            }
            (Some(local), None) => Some(local),
            (None, Some(synced)) => Some(synced),
            (None, None) => None,
        };
        let (balance, nonce) = match account {
            Some(ref account) => (account.balance, account.nonce),
            None => (U256::zero(), 0),
        };

        Ok(serde_json::json!({
            "address": format_rabbit_address(address),
            "balance": format_u256_hex(balance),
            "nonce": format!("0x{:x}", nonce),
        }))
    }

    fn rabbit_get_utxos(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        Ok(serde_json::json!([]))
    }

    fn rabbit_get_object(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let params = params.ok_or(RpcErrorObject::invalid_params("Missing params".to_string()))?;
        let object_id = params
            .first()
            .ok_or_else(|| RpcErrorObject::invalid_params("Missing object_id".to_string()))?
            .as_str()
            .ok_or_else(|| {
                RpcErrorObject::invalid_params("object_id must be string".to_string())
            })?;

        let object_id = parse_object_id(object_id)?;
        let maybe_output = self.compute_store.get_latest_output_by_object(object_id);
        Ok(serde_json::json!(maybe_output.map(object_output_to_json)))
    }

    fn rabbit_get_output(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let params = params.ok_or(RpcErrorObject::invalid_params("Missing params".to_string()))?;
        let output_id = params
            .first()
            .ok_or_else(|| RpcErrorObject::invalid_params("Missing output_id".to_string()))?
            .as_str()
            .ok_or_else(|| {
                RpcErrorObject::invalid_params("output_id must be string".to_string())
            })?;

        let output_id = parse_output_id(output_id)?;
        let maybe_output = self.compute_store.get_output(output_id);
        Ok(serde_json::json!(maybe_output.map(object_output_to_json)))
    }

    fn rabbit_get_domain(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let params = params.ok_or(RpcErrorObject::invalid_params("Missing params".to_string()))?;
        let id = params
            .first()
            .ok_or_else(|| RpcErrorObject::invalid_params("Missing domain_id".to_string()))?
            .as_u64()
            .ok_or_else(|| RpcErrorObject::invalid_params("domain_id must be u64".to_string()))?;

        let id_u32 = u32::try_from(id)
            .map_err(|_| RpcErrorObject::invalid_params("domain_id overflow".to_string()))?;

        let maybe_domain = self.domain_registry.get_domain(DomainId(id_u32));
        Ok(serde_json::json!(maybe_domain.map(|d| {
            serde_json::json!({
                "domain_id": d.domain_id.0,
                "name": d.name,
                "vm": d.vm,
                "public": d.public,
            })
        })))
    }

    fn rabbit_list_domains(
        &self,
        _params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let domains = self
            .domain_registry
            .list_domains()
            .into_iter()
            .map(|d| {
                serde_json::json!({
                    "domain_id": d.domain_id.0,
                    "name": d.name,
                    "vm": d.vm,
                    "public": d.public,
                })
            })
            .collect::<Vec<_>>();

        Ok(serde_json::json!(domains))
    }

    fn rabbit_simulate_compute_tx(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let params = params.ok_or(RpcErrorObject::invalid_params("Missing params".to_string()))?;
        let tx_value = params
            .first()
            .ok_or_else(|| RpcErrorObject::invalid_params("Missing tx".to_string()))?
            .clone();

        let tx = parse_compute_tx(tx_value)?;
        validate_compute_tx_network(&tx, &self.config)?;
        self.compute_adapter.simulate_compute_tx(tx)
    }

    async fn rabbit_submit_compute_tx(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let params = params.ok_or(RpcErrorObject::invalid_params("Missing params".to_string()))?;
        let tx_value = params
            .first()
            .ok_or_else(|| RpcErrorObject::invalid_params("Missing tx".to_string()))?
            .clone();

        let tx = parse_compute_tx(tx_value)?;
        validate_compute_tx_network(&tx, &self.config)?;

        // EIP-1559 fee validation (skip for legacy tx with zero fee fields,
        // unless require_fee_for_compute_tx is enabled).
        // Mint 是价值创造命令，执行器策略明令禁止携带任何费用
        // （execution/policy: Mint 要求 fee/max_fee/priority_fee 全 0），
        // 因此 fee-required 校验对 Mint 不适用，否则生产 profile 下 Mint 永不可提交。
        let fee_absent = tx.max_fee == 0 && tx.priority_fee == 0 && tx.gas_limit == 0;
        let is_mint = tx.command == Command::Mint;
        if !is_mint && fee_absent && self.config.require_fee_for_compute_tx {
            return Err(RpcErrorObject::invalid_params(
                "fee required: max_fee/priority_fee/gas_limit must be set".to_string(),
            ));
        }
        if !is_mint && !fee_absent {
            let base_fee = *self.current_base_fee.read();
            if let Err(err) = rabbitcore::compute::validate_tx_fee(&tx, base_fee) {
                return Err(RpcErrorObject::invalid_params(format!("fee validation failed: {err}")));
            }
        }

        if let Some(persistent) = &self.persistent_compute_store {
            if let Ok(Some(existing)) = persistent.get_tx_result(tx.tx_id) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&existing) {
                    return Ok(serde_json::json!({
                        "ok": true,
                        "duplicate": true,
                        "result": v,
                    }));
                }
            }
        }

        if let Some(existing) = self
            .submitted_compute_results
            .read()
            .get(&tx.tx_id.0)
            .cloned()
        {
            return Ok(serde_json::json!({
                "ok": true,
                "duplicate": true,
                "result": existing,
            }));
        }

        // BlockTime：提交只入队（真实执行在区块打包时，见 rabbit_submit_work）。
        // 提交时轻量预检：游戏域门禁（GAME_DOMAIN Invoke 负载语义重算验证）。
        crate::rpc::compute_adapter::gate_game_tx(&tx)?;

        let seq = self
            .pending_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst) as u64;
        let base_fee = *self.current_base_fee.read();
        let queued_result = serde_json::json!({
            "ok": true,
            "queued": true,
            "tx_id": format!("0x{}", hex::encode(tx.tx_id.0.as_bytes())),
            "note": "queued; executed at block time",
        });
        {
            let mut pool = self.tx_fee_pool.write();
            pool.push(PendingTx::new(tx.clone(), base_fee, seq));
            // Bound pool growth; keep only the highest-priority entries.
            const MAX_TX_POOL: usize = 4096;
            while pool.len() > MAX_TX_POOL {
                let mut items: Vec<PendingTx> = pool.drain().collect();
                items.sort_by(|a, b| b.tip_rate.cmp(&a.tip_rate));
                items.truncate(MAX_TX_POOL);
                pool.extend(items);
            }
        }
        // 记录"待处理"占位；区块执行后由 submit_work 覆盖为真实结果
        {
            let mut results = self.submitted_compute_results.write();
            let mut order = self.submitted_compute_order.write();
            let tx_hash = tx.tx_id.0;
            results.insert(tx_hash, queued_result.clone());
            order.retain(|existing| existing != &tx_hash);
            order.push_back(tx_hash);
            while order.len() > MAX_SUBMITTED_COMPUTE_RESULTS {
                if let Some(stale) = order.pop_front() {
                    results.remove(&stale);
                }
            }
        }
        Ok(queued_result)
    }

    /// Returns current base fee and suggested priority fee.
    fn rabbit_gas_price(
        &self,
        _params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let base_fee_hopps = *self.current_base_fee.read();
        let base_fee_carrots = rabbitcore::compute::hopps_to_carrots(base_fee_hopps);
        let suggested_priority_fee_hopps = 1_000_000_000u64; // 1 carrot default tip

        Ok(serde_json::json!({
            "base_fee": format!("0x{:x}", base_fee_hopps),
            "base_fee_hopps": base_fee_hopps,
            "base_fee_carrots": base_fee_carrots,
            "suggested_priority_fee": format!("0x{:x}", suggested_priority_fee_hopps),
            "suggested_priority_fee_hopps": suggested_priority_fee_hopps,
            "suggested_priority_fee_carrots": 1,
            "unit": "hopps",
            "note": "1 carrot = 10⁹ hopps, 1 Rbit = 10¹⁸ hopps. Send max_fee/priority_fee in hopps.",
        }))
    }

    /// List pending transactions in the fee-priority pool (highest tip first).
    fn rabbit_pending_transactions(
        &self,
        _params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let pool = self.tx_fee_pool.read();
        let mut items: Vec<&PendingTx> = pool.iter().collect();
        items.sort_by(|a, b| b.tip_rate.cmp(&a.tip_rate));
        let pending: Vec<serde_json::Value> = items
            .iter()
            .take(100)
            .map(|p| {
                serde_json::json!({
                    "tx_id": format!("0x{}", hex::encode(p.tx.tx_id.0.as_bytes())),
                    "tip_rate": p.tip_rate,
                    "priority_fee_hopps": p.tx.priority_fee,
                    "max_fee_hopps": p.tx.max_fee,
                    "gas_limit": p.tx.gas_limit,
                })
            })
            .collect();
        Ok(serde_json::json!({
            "total": pool.len(),
            "items": pending,
        }))
    }

    /// Estimate gas for a ComputeTx using static analysis.
    fn rabbit_estimate_gas(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let params = params.ok_or(RpcErrorObject::invalid_params("Missing params".to_string()))?;
        let tx_value = params
            .first()
            .ok_or_else(|| RpcErrorObject::invalid_params("Missing tx".to_string()))?
            .clone();

        let tx = parse_compute_tx(tx_value)?;
        let estimated = rabbitcore::compute::estimate_tx_gas(&tx);
        let base_fee = *self.current_base_fee.read();
        let max_cost = rabbitcore::compute::hopps_to_carrots(estimated.saturating_mul(base_fee));

        Ok(serde_json::json!({
            "gas_estimated": estimated,
            "base_fee_hopps": base_fee,
            "base_fee_carrots": rabbitcore::compute::hopps_to_carrots(base_fee),
            "max_cost_carrots": max_cost,
            "unit": "gas",
        }))
    }

    fn rabbit_get_compute_tx_result(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let params = params.ok_or(RpcErrorObject::invalid_params("Missing params".to_string()))?;
        let tx_id_s = params
            .first()
            .ok_or_else(|| RpcErrorObject::invalid_params("Missing tx_id".to_string()))?
            .as_str()
            .ok_or_else(|| RpcErrorObject::invalid_params("tx_id must be string".to_string()))?;
        let tx_id = rabbitcore::compute::TxId(parse_hash(tx_id_s)?);

        if let Some(value) = self.submitted_compute_results.read().get(&tx_id.0).cloned() {
            return Ok(value);
        }

        if let Some(persistent) = &self.persistent_compute_store {
            let maybe = persistent.get_tx_result(tx_id).map_err(|e| {
                RpcErrorObject::internal_error(format!("load tx result failed: {e}"))
            })?;
            if let Some(raw) = maybe {
                let value = serde_json::from_str::<serde_json::Value>(&raw).map_err(|e| {
                    RpcErrorObject::internal_error(format!("decode tx result failed: {e}"))
                })?;
                return Ok(value);
            }
        }
        if let Some(record) = global_synced_compute_tx(&tx_id.0) {
            return Ok(record.result);
        }

        Ok(serde_json::Value::Null)
    }

    fn rabbit_list_compute_tx_results(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let mut page: usize = 1;
        let mut limit: usize = 20;
        if let Some(values) = params {
            if let Some(first) = values.first() {
                let obj = first.as_object().ok_or_else(|| {
                    RpcErrorObject::invalid_params(
                        "query object required for rabbit_listComputeTxResults".to_string(),
                    )
                })?;
                if let Some(parsed_page) = parse_u64_opt(obj.get("page"))? {
                    page = usize::try_from(parsed_page)
                        .map_err(|_| RpcErrorObject::invalid_params("page overflow".to_string()))?;
                }
                if let Some(parsed_limit) = parse_u64_opt(obj.get("limit"))? {
                    limit = usize::try_from(parsed_limit).map_err(|_| {
                        RpcErrorObject::invalid_params("limit overflow".to_string())
                    })?;
                }
            }
        }
        page = page.max(1);
        limit = limit.clamp(1, 200);
        let skip = page.saturating_sub(1).saturating_mul(limit);

        let order = self.submitted_compute_order.read();
        let results = self.submitted_compute_results.read();
        let mut seen = HashSet::new();
        let mut merged: Vec<(u64, Hash, serde_json::Value)> = Vec::new();
        let local_has_data = !order.is_empty();
        for tx_hash in order.iter().rev() {
            if let Some(result) = results.get(tx_hash) {
                let ts = result
                    .get("submitted_at_unix")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if seen.insert(*tx_hash) {
                    merged.push((ts, *tx_hash, result.clone()));
                }
            }
        }
        if !local_has_data {
            for record in global_synced_compute_txs().into_iter().rev() {
                if seen.insert(record.tx_hash) {
                    let ts = record
                        .result
                        .get("submitted_at_unix")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    merged.push((ts, record.tx_hash, record.result));
                }
            }
        }
        merged.sort_by(|a, b| b.0.cmp(&a.0));
        let total = merged.len();
        let items = merged
            .into_iter()
            .skip(skip)
            .take(limit)
            .map(|(_, tx_hash, result)| {
                serde_json::json!({
                    "tx_id": format!("0x{}", hex::encode(tx_hash.as_bytes())),
                    "result": result,
                })
            })
            .collect::<Vec<_>>();
        let has_more = skip.saturating_add(items.len()) < total;
        Ok(serde_json::json!({
            "page": page,
            "limit": limit,
            "total": total,
            "has_more": has_more,
            "items": items,
        }))
    }

    fn rabbit_get_operation_by_hash(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let params = params.ok_or(RpcErrorObject::invalid_params("Missing params".to_string()))?;
        let tx_hash = params
            .first()
            .ok_or_else(|| RpcErrorObject::invalid_params("Missing tx hash".to_string()))?
            .as_str()
            .ok_or_else(|| RpcErrorObject::invalid_params("tx hash must be string".to_string()))
            .and_then(parse_hash)?;

        if let Some(result) = self.submitted_compute_results.read().get(&tx_hash).cloned() {
            return Ok(compute_tx_to_json(tx_hash, &result));
        }
        if let Some(persistent) = &self.persistent_compute_store {
            let maybe = persistent
                .get_tx_result(rabbitcore::compute::TxId(tx_hash))
                .map_err(|e| {
                    RpcErrorObject::internal_error(format!("load tx result failed: {e}"))
                })?;
            if let Some(raw) = maybe {
                let value = serde_json::from_str::<serde_json::Value>(&raw).map_err(|e| {
                    RpcErrorObject::internal_error(format!("decode tx result failed: {e}"))
                })?;
                return Ok(compute_tx_to_json(tx_hash, &value));
            }
        }
        if let Some(record) = global_synced_compute_tx(&tx_hash) {
            return Ok(compute_tx_to_json(record.tx_hash, &record.result));
        }

        Ok(serde_json::Value::Null)
    }

    fn rabbit_list_operations(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let mut page: usize = 1;
        let mut limit: usize = 20;
        let mut include_compute = true;
        if let Some(values) = params {
            if let Some(first) = values.first() {
                let obj = first.as_object().ok_or_else(|| {
                    RpcErrorObject::invalid_params(
                        "query object required for rabbit_listOperations".to_string(),
                    )
                })?;
                if let Some(parsed_page) = parse_u64_opt(obj.get("page"))? {
                    page = usize::try_from(parsed_page)
                        .map_err(|_| RpcErrorObject::invalid_params("page overflow".to_string()))?;
                }
                if let Some(parsed_limit) = parse_u64_opt(obj.get("limit"))? {
                    limit = usize::try_from(parsed_limit).map_err(|_| {
                        RpcErrorObject::invalid_params("limit overflow".to_string())
                    })?;
                }
                if let Some(kind) = obj.get("kind").and_then(|v| v.as_str()) {
                    match kind {
                        "all" | "compute" => include_compute = true,
                        _ => {
                            return Err(RpcErrorObject::invalid_params(
                                "kind must be one of all|compute".to_string(),
                            ));
                        }
                    }
                }
            }
        }

        page = page.max(1);
        limit = limit.clamp(1, 200);
        let skip = page.saturating_sub(1).saturating_mul(limit);
        let mut items: Vec<(u64, serde_json::Value)> = Vec::new();

        if include_compute {
            let order = self.submitted_compute_order.read();
            let results = self.submitted_compute_results.read();
            let mut seen = HashSet::new();
            for tx_hash in order.iter().rev() {
                if let Some(result) = results.get(tx_hash) {
                    let ts = result
                        .get("submitted_at_unix")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    items.push((ts, compute_tx_to_json(*tx_hash, result)));
                    seen.insert(*tx_hash);
                }
            }
            for record in global_synced_compute_txs().into_iter().rev() {
                if seen.insert(record.tx_hash) {
                    let ts = record
                        .result
                        .get("submitted_at_unix")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    items.push((ts, compute_tx_to_json(record.tx_hash, &record.result)));
                }
            }
        }

        items.sort_by(|a, b| b.0.cmp(&a.0));
        let total = items.len();
        let page_items = items
            .into_iter()
            .skip(skip)
            .take(limit)
            .map(|(_, item)| item)
            .collect::<Vec<_>>();
        let has_more = skip.saturating_add(page_items.len()) < total;

        Ok(serde_json::json!({
            "page": page,
            "limit": limit,
            "total": total,
            "has_more": has_more,
            "items": page_items,
        }))
    }

    fn rabbit_get_operations_by_address(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let params = params.ok_or(RpcErrorObject::invalid_params("Missing params".to_string()))?;
        let mut address: Option<Address> = None;
        let mut page: usize = 1;
        let mut limit: usize = 20;

        if let Some(first) = params.first() {
            match first {
                serde_json::Value::String(s) => {
                    address = Some(parse_address(s)?);
                    if let Some(second) = params.get(1).and_then(|v| v.as_object()) {
                        if let Some(parsed_page) = parse_u64_opt(second.get("page"))? {
                            page = usize::try_from(parsed_page).map_err(|_| {
                                RpcErrorObject::invalid_params("page overflow".to_string())
                            })?;
                        }
                        if let Some(parsed_limit) = parse_u64_opt(second.get("limit"))? {
                            limit = usize::try_from(parsed_limit).map_err(|_| {
                                RpcErrorObject::invalid_params("limit overflow".to_string())
                            })?;
                        }
                    }
                }
                serde_json::Value::Object(obj) => {
                    let addr = obj.get("address").and_then(|v| v.as_str()).ok_or_else(|| {
                        RpcErrorObject::invalid_params("address is required".to_string())
                    })?;
                    address = Some(parse_address(addr)?);
                    if let Some(parsed_page) = parse_u64_opt(obj.get("page"))? {
                        page = usize::try_from(parsed_page).map_err(|_| {
                            RpcErrorObject::invalid_params("page overflow".to_string())
                        })?;
                    }
                    if let Some(parsed_limit) = parse_u64_opt(obj.get("limit"))? {
                        limit = usize::try_from(parsed_limit).map_err(|_| {
                            RpcErrorObject::invalid_params("limit overflow".to_string())
                        })?;
                    }
                }
                _ => {
                    return Err(RpcErrorObject::invalid_params(
                        "address query must be string or object".to_string(),
                    ));
                }
            }
        }

        let address = address
            .ok_or_else(|| RpcErrorObject::invalid_params("address is required".to_string()))?;
        page = page.max(1);
        limit = limit.clamp(1, 200);
        Ok(serde_json::json!({
            "address": format_rabbit_address(address),
            "page": page,
            "limit": limit,
            "total": 0,
            "has_more": false,
            "items": [],
            "unsupported": true,
            "reason": "address operation history is not supported on compute-only nodes",
        }))
    }

    async fn rabbit_get_work(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        if !self.config.mining_enabled {
            return Err(RpcErrorObject::invalid_params(
                "mining rpc disabled on this node".to_string(),
            ));
        }
        let req = parse_get_work_request(params)?;
        let latest = self.wait_for_work_change(&req).await?;
        let now = current_unix_secs();
        let target = mining_target_for_difficulty(
            latest.header.difficulty,
            self.config.mining_work_target_leading_rabbit_bytes,
        );
        let prev_hash = latest.header.hash;
        let height = latest.header.number.as_u64().saturating_add(1);
        let coinbase = self.select_coinbase_for_work()?;

        let work_id = format!("work-{}-{}", height, now);
        let block_difficulty =
            adjust_mining_difficulty(latest.header.difficulty, latest.header.timestamp, now);
        let work = MiningWork {
            work_id: work_id.clone(),
            prev_hash,
            height,
            target,
            difficulty: block_difficulty,
            created_at_secs: now,
            header_timestamp: now,
            coinbase,
        };
        {
            let mut jobs = self.mining_jobs.write();
            let mut order = self.mining_job_order.write();

            while let Some(front) = order.front().cloned() {
                let should_drop = match jobs.get(&front) {
                    Some(existing) => {
                        now.saturating_sub(existing.created_at_secs) > MAX_MINING_JOB_AGE_SECS
                    }
                    None => true,
                };
                if !should_drop {
                    break;
                }
                order.pop_front();
                jobs.remove(&front);
            }

            jobs.insert(work_id.clone(), work.clone());
            order.push_back(work_id.clone());

            while order.len() > MAX_MINING_JOBS {
                if let Some(stale_work_id) = order.pop_front() {
                    jobs.remove(&stale_work_id);
                }
            }
        }

        // Report the fee-pool state so miners can gauge pending demand.
        let pool_snapshot = {
            let pool = self.tx_fee_pool.read();
            let top_tip = pool.iter().map(|p| p.tip_rate).max().unwrap_or(0);
            (pool.len(), top_tip)
        };

        Ok(serde_json::json!({
            "work_id": work.work_id,
            "prev_hash": format!("0x{}", hex::encode(work.prev_hash.as_bytes())),
            "height": work.height,
            "version": global_block_version_for_height(work.height),
            "difficulty": format!("0x{:x}", work.difficulty.as_u128()),
            "timestamp": work.header_timestamp,
            "gas_limit": 30_000_000u64,
            "target": pow_target_to_hex(work.target),
            "target_leading_rabbit_bytes": leading_rabbit_bytes_for_target(work.target),
            "coinbase": format_rabbit_address(work.coinbase),
            "pending_tx_count": pool_snapshot.0,
            "top_tip_rate": pool_snapshot.1,
            "base_fee_hopps": latest.header.base_fee_per_gas.as_u64(),
        }))
    }

    fn rabbit_submit_work(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let metrics = RPC_METRICS.get()?;
        if !self.config.mining_enabled {
            return Err(RpcErrorObject::invalid_params(
                "mining rpc disabled on this node".to_string(),
            ));
        }
        let params = params.ok_or(RpcErrorObject::invalid_params("Missing params".to_string()))?;
        let req_value = params
            .first()
            .ok_or_else(|| RpcErrorObject::invalid_params("Missing work payload".to_string()))?
            .clone();
        let req: SubmitWorkRequest = serde_json::from_value(req_value).map_err(|e| {
            RpcErrorObject::invalid_params(format!("invalid submit work payload: {e}"))
        })?;

        let work = self
            .mining_jobs
            .read()
            .get(&req.work_id)
            .cloned()
            .ok_or_else(|| {
                RpcErrorObject::invalid_params("unknown or stale work_id".to_string())
            })?;

        let parent = self.current_head_block().header;
        let parent_number = parent.number;
        let parent_base_fee = parent.base_fee_per_gas.as_u64();
        // 用 getWork 时固定的模板字段（时间戳/难度），保证矿工哈希与校验一致
        let timestamp = work.header_timestamp;
        let difficulty = work.difficulty;
        let version = global_block_version_for_height(parent_number.as_u64().saturating_add(1));
        let miner_label = req.miner.clone().unwrap_or_else(|| "rabbit-miner".to_string());
        if miner_label.len() > MAX_MINER_EXTRA_DATA_BYTES {
            metrics
                .mining_shares_rejected
                .with_label_values(&["invalid_miner_label"])
                .inc();
            return Ok(serde_json::json!({
                "accepted": false,
                "reason": "invalid_miner_label"
            }));
        }
        let hash_bytes = hex::decode(req.hash_hex.strip_prefix("0x").unwrap_or(&req.hash_hex))
            .map_err(|e| RpcErrorObject::invalid_params(format!("invalid hash hex: {e}")))?;
        if hash_bytes.len() != 32 {
            return Err(RpcErrorObject::invalid_params(
                "hash must be 32 bytes".to_string(),
            ));
        }
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&hash_bytes);

        // Verify PoW: accept the canonical SHA-256d digest (computed over the
        // full header) or the legacy simplified keccak digest for backward
        // compatibility with older miners.
        let canonical_pow = rabbitcore::block::compute_pow_hash(
            &BlockHeader {
                version,
                parent_hash: work.prev_hash,
                uncle_hashes: Vec::new(),
                coinbase: work.coinbase,
                state_root: Hash::zero(),
                transactions_root: Hash::zero(),
                receipts_root: Hash::zero(),
                number: parent_number + U256::one(),
                gas_limit: 30_000_000,
                gas_used: 0,
                timestamp,
                difficulty,
                nonce: req.nonce,
                extra_data: miner_label.as_bytes().to_vec(),
                mix_hash: Hash::zero(),
                base_fee_per_gas: U256::from(parent_base_fee.max(rabbitcore::compute::INITIAL_BASE_FEE)),
                hash: Hash::zero(),
            },
            req.nonce,
        );
        let legacy_pow = {
            let mut data = Vec::new();
            data.extend_from_slice(work.prev_hash.as_bytes());
            data.extend_from_slice(&work.height.to_be_bytes());
            data.extend_from_slice(&req.nonce.to_be_bytes());
            Hash::from_bytes(rabbitcore::crypto::keccak256(&data))
        };
        let submitted = Hash::from_bytes(hash);
        let use_canonical = submitted == canonical_pow;
        let use_legacy = submitted == legacy_pow;
        if !use_canonical && !use_legacy {
            metrics
                .mining_shares_rejected
                .with_label_values(&["invalid_pow_hash"])
                .inc();
            return Ok(serde_json::json!({
                "accepted": false,
                "reason": "invalid_pow_hash"
            }));
        }
        if !pow_hash_meets_target(&hash, work.target) {
            metrics
                .mining_shares_rejected
                .with_label_values(&["low_difficulty_share"])
                .inc();
            return Ok(serde_json::json!({
                "accepted": false,
                "reason": "low_difficulty_share"
            }));
        }

        let seen_key = SeenShareKey {
            work_id: req.work_id.clone(),
            nonce: req.nonce,
            hash,
        };
        {
            let mut seen = self.mining_seen_submissions.write();
            let mut order = self.mining_seen_submission_order.write();
            if !seen.insert(seen_key.clone()) {
                metrics
                    .mining_shares_rejected
                    .with_label_values(&["duplicate_share"])
                    .inc();
                return Ok(serde_json::json!({
                    "accepted": false,
                    "reason": "duplicate_share"
                }));
            }
            order.push_back(seen_key);
            while order.len() > MAX_SEEN_MINING_SUBMISSIONS {
                if let Some(stale) = order.pop_front() {
                    seen.remove(&stale);
                }
            }
        }

        let consumed = self.mining_jobs.write().remove(&req.work_id).is_some();
        self.mining_job_order
            .write()
            .retain(|work_id| work_id != &req.work_id);
        if !consumed {
            metrics
                .mining_shares_rejected
                .with_label_values(&["stale_or_duplicate_work"])
                .inc();
            return Ok(serde_json::json!({
                "accepted": false,
                "reason": "stale_or_duplicate_work"
            }));
        }

        let parent = self.current_head_block().header;
        let expected_work_height = parent.number.as_u64().saturating_add(1);
        if work.height != expected_work_height || work.prev_hash != parent.hash {
            metrics
                .mining_shares_rejected
                .with_label_values(&["stale_work_template"])
                .inc();
            return Ok(serde_json::json!({
                "accepted": false,
                "reason": "stale_work_template"
            }));
        }

        {
            let mut counter = self.hashrate_counter.write();
            *counter = counter.saturating_add(1);
        }
        metrics
            .mining_shares_accepted
            .with_label_values(&["rabbit_submitWork"])
            .inc();

        // Build and publish a synthetic block header into latest_block for MVP chain progress.
        let parent_hash = work.prev_hash;
        let extra_data = miner_label.as_bytes().to_vec();

        // ── EIP-1559 fee accounting + tx packing ───────────────────────────
        // The parent block's base fee applies to transactions in THIS block.
        // Select transactions from the fee-priority pool by tip rate until the
        // block gas target is reached. In SubmitTime mode the txs were already
        // executed at submit time, so packing them here is a reference into the
        // canonical body: receipts carry the final block hash as an annotation.
        // The receipt.block_hash ↔ header.hash circular dependency is broken
        // because compute_receipts_root strips block_hash from the commitment.
        let (packed_txs, gas_used) = {
            let mut pool = self.tx_fee_pool.write();
            let gas_limit = 30_000_000u64;
            let target_gas = gas_limit / 2;
            let mut used = 0u64;
            let mut txs = Vec::new();
            while let Some(p) = pool.peek() {
                let tx_gas = rabbitcore::compute::estimate_tx_gas(&p.tx);
                if used.saturating_add(tx_gas) > gas_limit {
                    break;
                }
                if let Some(popped) = pool.pop() {
                    used = used.saturating_add(tx_gas);
                    txs.push(popped.tx);
                }
                if used >= target_gas {
                    break; // enough demand; leave the rest for the next block
                }
            }
            (txs, used)
        };
        // Next block's base fee adjusts by this block's utilization (EIP-1559).
        let next_base_fee = rabbitcore::compute::calculate_base_fee(
            parent_base_fee.max(rabbitcore::compute::INITIAL_BASE_FEE),
            gas_used,
            30_000_000,
        );
        *self.current_base_fee.write() = next_base_fee;

        let mut header = BlockHeader {
            version,
            parent_hash,
            uncle_hashes: Vec::new(),
            coinbase: work.coinbase,
            state_root: Hash::zero(),
            transactions_root: Hash::zero(),
            receipts_root: Hash::zero(),
            number: parent_number + U256::one(),
            gas_limit: 30_000_000,
            gas_used,
            timestamp,
            difficulty,
            nonce: req.nonce,
            extra_data: miner_label.into_bytes(),
            mix_hash: if use_canonical { canonical_pow } else { legacy_pow },
            base_fee_per_gas: U256::from(parent_base_fee.max(rabbitcore::compute::INITIAL_BASE_FEE)),
            hash: Hash::zero(),
        };

        // Build the body and reconcile header commitments. Receipts come from
        // REAL block-time execution (BlockTime 结算：产块时执行打包交易）。
        let body = if packed_txs.is_empty() {
            if global_block_requires_body(header.number.as_u64(), header.version) {
                Some(BlockBody::default())
            } else {
                None
            }
        } else {
            let block_base_fee = parent_base_fee.max(rabbitcore::compute::INITIAL_BASE_FEE);
            let (mut receipts, total_gas) = self
                .state_executor
                .execute_txs(&packed_txs, block_base_fee, &self.state_executor.new_basic_executor())
                .map_err(|e| {
                    RpcErrorObject::internal_error(format!("block-time execution failed: {e}"))
                })?;
            // 区块执行后的账户状态根（含国库费用效果）写入 header
            header.state_root = self.state_db.state_root();
            header.gas_used = total_gas;
            let mut body = BlockBody::new(packed_txs, receipts);
            header.apply_body_commitments(&body);
            header.hash = header.compute_hash();
            // Back-fill the final block hash into receipts (annotation only).
            for r in body.receipts.iter_mut() {
                r.block_hash = header.hash;
            }
            // 记录每笔交易的真实结果（先算 header.hash，result.block_hash 才有真实值）
            {
                let mut results = self.submitted_compute_results.write();
                let mut order = self.submitted_compute_order.write();
                for (tx, receipt) in body.transactions.iter().zip(body.receipts.iter()) {
                    let value = serde_json::json!({
                        "ok": receipt.status == rabbitcore::block::ReceiptStatus::Success,
                        "tx_id": format!("0x{}", hex::encode(tx.tx_id.0.as_bytes())),
                        "status": format!("{:?}", receipt.status),
                        "gas_used": receipt.gas_used,
                        "output_refs": receipt.output_refs.iter().map(|o| format!("0x{}", hex::encode(o.0.as_bytes()))).collect::<Vec<_>>(),
                        "error": receipt.error,
                        "block_hash": format!("0x{}", hex::encode(header.hash.as_bytes())),
                    });
                    let tx_hash = tx.tx_id.0;
                    results.insert(tx_hash, value.clone());
                    order.retain(|existing| existing != &tx_hash);
                    order.push_back(tx_hash);
                    // 结果持久化到 redb：重启后 rabbit_getComputeTxResult 仍可查
                    if let Some(persistent) = &self.persistent_compute_store {
                        let persistent = persistent.clone();
                        let tid = tx.tx_id;
                        let serialized = serde_json::to_string(&value)
                            .unwrap_or_else(|_| "{\"ok\":false}".to_string());
                        let _ = tokio::task::spawn_blocking(move || {
                            if let Err(err) = persistent.put_tx_result(tid, &serialized) {
                                tracing::error!("failed to persist compute tx result: {}", err);
                            }
                        });
                    }
                }
                while order.len() > MAX_SUBMITTED_COMPUTE_RESULTS {
                    if let Some(stale) = order.pop_front() {
                        results.remove(&stale);
                    }
                }
            }
            Some(body)
        };
        if header.hash.is_zero() {
            header.hash = header.compute_hash();
        }

        let block = Block {
            header: header.clone(),
            body,
            uncles: Vec::new(),
        };
        self.store_block(block.clone(), None).map_err(|err| {
            RpcErrorObject::internal_error(format!("failed to persist mined block: {err}"))
        })?;
        *self.latest_block.write() = Some(block);
        set_global_synced_height(header.number.as_u64());
        self.credit_block_reward(header.coinbase, header.number);
        metrics
            .latest_block_height
            .set(header.number.as_u64() as i64);

        Ok(serde_json::json!({
            "accepted": true,
            "block_hash": format!("0x{}", hex::encode(header.hash.as_bytes())),
            "height": header.number.as_u64(),
        }))
    }

    /// BlockTime 状态执行器访问器（产块/测试驱动区块执行）。
    pub fn state_executor(&self) -> Arc<StateExecutor> {
        self.state_executor.clone()
    }

    fn configured_coinbases(&self) -> Result<Vec<Address>, RpcErrorObject> {
        if self.config.coinbase_addresses.is_empty() {
            return Ok(vec![parse_address(&self.config.coinbase)?]);
        }

        self.config
            .coinbase_addresses
            .iter()
            .map(|address| parse_address(address))
            .collect()
    }

    fn select_coinbase_for_work(&self) -> Result<Address, RpcErrorObject> {
        let coinbases = self.configured_coinbases()?;
        let idx = self.next_coinbase_index.fetch_add(1, Ordering::Relaxed) % coinbases.len();
        Ok(coinbases[idx])
    }

    fn rabbit_get_metrics(
        &self,
        _params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let text = RPC_METRICS.get()?.render()?;
        Ok(serde_json::json!({ "text": text }))
    }

    fn rabbit_peers(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        if let Some(values) = params {
            if !values.is_empty() {
                return Err(RpcErrorObject::invalid_params(
                    "rabbit_peers does not accept params".to_string(),
                ));
            }
        }

        let now = current_unix_secs();
        let peers = global_peers()
            .into_iter()
            .map(|peer| {
                let idle_secs = now.saturating_sub(peer.last_activity);
                serde_json::json!({
                    "peer_id": peer.peer_id,
                    "network_id": peer.network_id,
                    "protocol_version": peer.protocol_version,
                    "client_version": peer.client_version,
                    "remote_addr": peer.remote_addr.to_string(),
                    "local_addr": peer.local_addr.to_string(),
                    "connected_at": peer.connected_at,
                    "last_activity": peer.last_activity,
                    "idle_secs": idle_secs,
                    "reputation": peer.reputation,
                    "capabilities": peer.capabilities,
                })
            })
            .collect::<Vec<_>>();

        Ok(serde_json::json!(peers))
    }

    fn rabbit_get_latest_block(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let include_body = parse_include_body_flag(params.as_ref())?;
        Ok(self.block_to_rabbit_block_json(&self.current_head_block(), include_body))
    }

    fn rabbit_sync_status(
        &self,
        _params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let local_head = self.current_head_block().header.number.as_u64();
        let network_head = global_synced_height();
        Ok(serde_json::json!({
            "local_head": local_head,
            "network_head": network_head,
            "syncing": network_head > local_head,
        }))
    }

    fn rabbit_get_block_by_number(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let params = params.ok_or(RpcErrorObject::invalid_params("Missing params".to_string()))?;
        let number = parse_u64_opt(params.first())?
            .ok_or_else(|| RpcErrorObject::invalid_params("number is required".to_string()))?;
        let include_body = parse_bool_opt(params.get(1))?.unwrap_or(false);
        let block = self.block_by_number(number);
        Ok(block
            .as_ref()
            .map(|block| self.block_to_rabbit_block_json(block, include_body))
            .unwrap_or(serde_json::Value::Null))
    }

    fn rabbit_get_block_by_hash(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let params = params.ok_or(RpcErrorObject::invalid_params("Missing params".to_string()))?;
        let hash_s = params
            .first()
            .ok_or_else(|| RpcErrorObject::invalid_params("hash is required".to_string()))?
            .as_str()
            .ok_or_else(|| RpcErrorObject::invalid_params("hash must be string".to_string()))?;
        let hash = parse_hash(hash_s)?;
        let include_body = parse_bool_opt(params.get(1))?.unwrap_or(false);
        let block = self.block_by_hash(&hash);
        Ok(block
            .as_ref()
            .map(|block| self.block_to_rabbit_block_json(block, include_body))
            .unwrap_or(serde_json::Value::Null))
    }

    fn rabbit_get_block_body(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let params = params.ok_or(RpcErrorObject::invalid_params("Missing params".to_string()))?;
        let hash_s = params
            .first()
            .ok_or_else(|| RpcErrorObject::invalid_params("block_hash is required".to_string()))?
            .as_str()
            .ok_or_else(|| {
                RpcErrorObject::invalid_params("block_hash must be string".to_string())
            })?;
        let block_hash = parse_hash(hash_s)?;
        let body = self.block_body_record_by_hash(&block_hash);
        Ok(body
            .as_ref()
            .map(block_body_record_to_json)
            .unwrap_or(serde_json::Value::Null))
    }

    fn rabbit_get_block_receipts(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let params = params.ok_or(RpcErrorObject::invalid_params("Missing params".to_string()))?;
        let hash_s = params
            .first()
            .ok_or_else(|| RpcErrorObject::invalid_params("block_hash is required".to_string()))?
            .as_str()
            .ok_or_else(|| {
                RpcErrorObject::invalid_params("block_hash must be string".to_string())
            })?;
        let block_hash = parse_hash(hash_s)?;
        Ok(self
            .block_receipts_by_hash(&block_hash)
            .map(|receipts| {
                serde_json::Value::Array(receipts.iter().map(receipt_to_json).collect())
            })
            .unwrap_or(serde_json::Value::Null))
    }

    fn rabbit_get_receipt(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let params = params.ok_or(RpcErrorObject::invalid_params("Missing params".to_string()))?;
        let tx_s = params
            .first()
            .ok_or_else(|| RpcErrorObject::invalid_params("tx_id is required".to_string()))?
            .as_str()
            .ok_or_else(|| RpcErrorObject::invalid_params("tx_id must be string".to_string()))?;
        let tx_hash = parse_hash(tx_s)?;
        Ok(self
            .receipt_by_tx_hash(&tx_hash)
            .map(|receipt| receipt_to_json(&receipt))
            .unwrap_or(serde_json::Value::Null))
    }

    fn rabbit_get_blocks_range(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let latest = self.current_head_block().header.number.as_u64();
        let mut from: Option<u64> = None;
        let mut to: Option<u64> = None;
        let mut limit: usize = 20;
        let mut include_body = false;

        if let Some(values) = params {
            if let Some(first) = values.first() {
                let obj = first.as_object().ok_or_else(|| {
                    RpcErrorObject::invalid_params(
                        "query object required for rabbit_getBlocksRange".to_string(),
                    )
                })?;
                from = parse_u64_opt(obj.get("from"))?;
                to = parse_u64_opt(obj.get("to"))?;
                if let Some(parsed_limit) = parse_u64_opt(obj.get("limit"))? {
                    limit = usize::try_from(parsed_limit).map_err(|_| {
                        RpcErrorObject::invalid_params("limit overflow".to_string())
                    })?;
                }
                include_body = parse_bool_opt(obj.get("include_body"))?.unwrap_or(false);
            }
        }
        limit = limit.clamp(1, 500);
        let to = to.unwrap_or(latest).min(latest);
        let from = from
            .unwrap_or_else(|| to.saturating_sub(limit as u64).saturating_add(1))
            .min(to);

        let mut items = Vec::new();
        for number in (from..=to).rev() {
            if items.len() >= limit {
                break;
            }
            if let Some(block) = self.block_by_number(number) {
                items.push(self.block_to_rabbit_block_json(&block, include_body));
            }
        }

        Ok(serde_json::json!({
            "from": from,
            "to": to,
            "limit": limit,
            "items": items,
        }))
    }

    fn current_head_block(&self) -> Block {
        match (self.latest_block.read().clone(), global_latest_block()) {
            (Some(local), Some(global)) => {
                if global.header.number >= local.header.number {
                    global
                } else {
                    local
                }
            }
            (Some(local), None) => local,
            (None, Some(global)) => global,
            (None, None) => create_genesis_block(),
        }
    }

    fn block_body_for_block(&self, block: &Block) -> Option<BlockBodyRecord> {
        if let Some(body) = block.body.clone() {
            return Some(BlockBodyRecord::new(
                block.header.number.as_u64(),
                block.header.hash,
                body,
            ));
        }
        let height = block.header.number.as_u64();
        if let Some(local) = self.block_bodies.read().get(&height).cloned() {
            return Some(local);
        }
        global_block_body_by_hash(&block.header.hash)
    }

    fn block_body_record_by_hash(&self, block_hash: &Hash) -> Option<BlockBodyRecord> {
        if let Some(block) = self
            .block_history
            .read()
            .values()
            .find(|block| block.header.hash == *block_hash)
            .cloned()
            .or_else(|| {
                self.latest_block
                    .read()
                    .clone()
                    .filter(|block| block.header.hash == *block_hash)
            })
            .or_else(|| global_block_by_hash(block_hash))
        {
            if let Some(body) = block.body {
                return Some(BlockBodyRecord::new(
                    block.header.number.as_u64(),
                    block.header.hash,
                    body,
                ));
            }
        }
        self.block_bodies
            .read()
            .values()
            .find(|record| record.block_hash == *block_hash)
            .cloned()
            .or_else(|| global_block_body_by_hash(block_hash))
    }

    fn block_receipts_by_hash(&self, block_hash: &Hash) -> Option<Vec<Receipt>> {
        self.block_body_record_by_hash(block_hash)
            .map(|record| record.body.receipts)
    }

    fn receipt_by_tx_hash(&self, tx_hash: &Hash) -> Option<Receipt> {
        if let Some(receipt) = self
            .block_history
            .read()
            .values()
            .filter_map(|block| block.body.as_ref())
            .flat_map(|body| body.receipts.iter())
            .find(|receipt| receipt.tx_id.0 == *tx_hash)
            .cloned()
        {
            return Some(receipt);
        }
        self.block_bodies
            .read()
            .values()
            .flat_map(|record| record.body.receipts.iter())
            .find(|receipt| receipt.tx_id.0 == *tx_hash)
            .cloned()
            .or_else(|| global_block_receipt_by_tx_hash(tx_hash))
    }

    fn block_to_rabbit_block_json(&self, block: &Block, include_body: bool) -> serde_json::Value {
        let mut json = block_to_rabbit_block_json(block);
        let body = block
            .body
            .as_ref()
            .map(|body| {
                BlockBodyRecord::new(
                    block.header.number.as_u64(),
                    block.header.hash,
                    body.clone(),
                )
            })
            .or_else(|| self.block_body_for_block(block))
            .or_else(|| global_block_body_by_number(block.header.number.as_u64()));
        if include_body || block.body.is_some() || body.is_some() {
            json["body"] = body
                .as_ref()
                .map(block_body_record_to_json)
                .unwrap_or(serde_json::Value::Null);
        }
        json
    }

    fn parse_body_include_flag_from_params(
        params: Option<&Vec<serde_json::Value>>,
    ) -> Result<bool, RpcErrorObject> {
        parse_include_body_flag(params)
    }

    async fn wait_for_work_change(&self, req: &GetWorkRequest) -> Result<Block, RpcErrorObject> {
        let mut latest = self.current_head_block();
        if !req.wait || !work_matches_known_head(&latest, req)? {
            return Ok(latest);
        }

        let timeout_secs = req.timeout_secs.unwrap_or(MAX_GET_WORK_WAIT_SECS);
        let timeout_secs = timeout_secs.clamp(1, MAX_GET_WORK_WAIT_SECS);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

        loop {
            if std::time::Instant::now() >= deadline {
                return Ok(self.current_head_block());
            }
            tokio::time::sleep(std::time::Duration::from_millis(GET_WORK_WAIT_POLL_MILLIS)).await;
            latest = self.current_head_block();
            if !work_matches_known_head(&latest, req)? {
                return Ok(latest);
            }
        }
    }

    fn block_by_number(&self, number: u64) -> Option<Block> {
        if number == 0 {
            return Some(create_genesis_block());
        }

        if let Some(found) = self.block_history.read().get(&number).cloned() {
            return Some(found);
        }

        if let Some(found) = global_block_by_number(number) {
            return Some(found);
        }

        self.latest_block
            .read()
            .as_ref()
            .filter(|block| block.header.number.as_u64() == number)
            .cloned()
            .or_else(|| {
                let head = self.current_head_block();
                (head.header.number.as_u64() == number).then_some(head)
            })
    }

    fn block_by_hash(&self, hash: &Hash) -> Option<Block> {
        if let Some(found) = self
            .block_history
            .read()
            .values()
            .find(|block| block.header.hash == *hash)
            .cloned()
        {
            return Some(found);
        }

        if let Some(found) = self
            .latest_block
            .read()
            .as_ref()
            .filter(|block| block.header.hash == *hash)
            .cloned()
        {
            return Some(found);
        }

        global_block_by_hash(hash)
    }

    fn store_block(
        &self,
        block: Block,
        body: Option<BlockBodyRecord>,
    ) -> Result<(), rabbitnet::NetworkError> {
        let height = block.header.number.as_u64();
        let effective_body = body.or_else(|| {
            block.body.as_ref().map(|body| {
                BlockBodyRecord::new(
                    block.header.number.as_u64(),
                    block.header.hash,
                    body.clone(),
                )
            })
        });
        // An explicit mining target override is a dev/test mode knob used by
        // smoke scripts. In that mode the local node may intentionally accept
        // non-consensus PoW for fast progression, so we keep those blocks local
        // instead of pushing them into the globally validated sync cache.
        if self.config.mining_work_target_leading_rabbit_bytes.is_none() {
            if let Some(body_record) = effective_body.clone() {
                global_store_block_with_body(block.clone(), body_record.body)?;
            } else {
                global_store_block(block.clone())?;
            }
        }
        let mut history = self.block_history.write();
        history.insert(height, block);
        while history.len() > MAX_BLOCK_HISTORY {
            let Some(oldest) = history.keys().next().copied() else {
                break;
            };
            history.remove(&oldest);
        }

        if let Some(body_record) = effective_body {
            let mut bodies = self.block_bodies.write();
            bodies.insert(height, body_record);
            while bodies.len() > MAX_BLOCK_HISTORY {
                let Some(oldest) = bodies.keys().next().copied() else {
                    break;
                };
                bodies.remove(&oldest);
            }
        }
        Ok(())
    }

    fn credit_block_reward(&self, coinbase: Address, block_number: U256) {
        let reward = block_reward_for_height(block_number.as_u64());
        if reward.is_zero() {
            return;
        }

        let now = current_unix_secs();
        let mut account = self
            .state_db
            .get_account(&coinbase)
            .unwrap_or_else(|| Account {
                address: coinbase,
                state: AccountState::Active,
                created_at: now,
                updated_at: now,
                ..Account::default()
            });

        account.balance = account.balance.saturating_add(reward);
        account.updated_at = now;
        self.state_db.insert_account(coinbase, account.clone());
        global_record_account(account);
    }

    fn rabbit_import_block(
        &self,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<serde_json::Value, RpcErrorObject> {
        let params = params.ok_or(RpcErrorObject::invalid_params("Missing params".to_string()))?;
        let block_obj = params
            .first()
            .ok_or_else(|| RpcErrorObject::invalid_params("Missing block".to_string()))?
            .as_object()
            .ok_or_else(|| RpcErrorObject::invalid_params("block must be object".to_string()))?;

        let hash = parse_hash_field(block_obj, "hash")?;
        let version = block_obj
            .get("version")
            .and_then(|v| v.as_u64())
            .map(|v| {
                u32::try_from(v)
                    .map_err(|_| RpcErrorObject::invalid_params("version overflow".to_string()))
            })
            .transpose()?
            .unwrap_or(1);
        let parent_hash = parse_hash_field(block_obj, "parent_hash")?;
        let number = parse_u64_hex_field(block_obj, "number")?;
        let timestamp = block_obj
            .get("timestamp")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                RpcErrorObject::invalid_params("timestamp missing or invalid".to_string())
            })?;
        let difficulty_u64 = parse_u64_hex_field(block_obj, "difficulty")?;
        let nonce = block_obj
            .get("nonce")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                RpcErrorObject::invalid_params("nonce missing or invalid".to_string())
            })?;
        let coinbase = block_obj
            .get("coinbase")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                RpcErrorObject::invalid_params("coinbase missing or invalid".to_string())
            })?;
        let coinbase = parse_address(coinbase)?;
        let mix_hash = parse_hash_field(block_obj, "mix_hash")?;
        let extra_data = parse_bytes_hex_opt(block_obj.get("extra_data"))?.unwrap_or_default();
        let state_root = parse_hash_opt(block_obj.get("state_root"))?;
        let transactions_root = parse_hash_opt(block_obj.get("transactions_root"))?;
        let receipts_root = parse_hash_opt(block_obj.get("receipts_root"))?;
        let transactions = block_obj
            .get("transactions")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        if !transactions.is_empty() {
            return Err(RpcErrorObject::invalid_params(
                "legacy block transactions are not supported".to_string(),
            ));
        }
        let body =
            match block_obj.get("body") {
                None | Some(serde_json::Value::Null) => None,
                Some(value) => Some(serde_json::from_value::<BlockBody>(value.clone()).map_err(
                    |err| RpcErrorObject::invalid_params(format!("invalid body: {err}")),
                )?),
            };
        if body.is_none() && (transactions_root.is_some() || receipts_root.is_some()) {
            let zero = Hash::zero();
            if transactions_root != Some(zero) || receipts_root != Some(zero) {
                return Err(RpcErrorObject::invalid_params(
                    "body is required when transactions_root or receipts_root is non-zero"
                        .to_string(),
                ));
            }
        }
        if let Some(body) = &body {
            if body.transactions.len() != body.receipts.len() {
                return Err(RpcErrorObject::invalid_params(
                    "body transactions/receipts length mismatch".to_string(),
                ));
            }
            let derived_tx_root = compute_transactions_root(&body.transactions);
            let derived_receipts_root = compute_receipts_root(&body.receipts);
            if let Some(expected_tx_root) = transactions_root {
                if expected_tx_root != derived_tx_root {
                    return Err(RpcErrorObject::invalid_params(
                        "transactions_root does not match body".to_string(),
                    ));
                }
            }
            if let Some(expected_receipts_root) = receipts_root {
                if expected_receipts_root != derived_receipts_root {
                    return Err(RpcErrorObject::invalid_params(
                        "receipts_root does not match body".to_string(),
                    ));
                }
            }
        }

        let current = self.current_head_block();
        let current_num = current.header.number.as_u64();
        if number <= current_num {
            return Ok(serde_json::json!({
                "imported": false,
                "reason": "stale_or_duplicate"
            }));
        }
        if number != current_num.saturating_add(1) || parent_hash != current.header.hash {
            return Ok(serde_json::json!({
                "imported": false,
                "reason": "parent_mismatch"
            }));
        }

        if let Some(activation_height) = global_block_activation_height() {
            if number >= activation_height && version < CANONICAL_BLOCK_VERSION {
                return Err(RpcErrorObject::invalid_params(format!(
                    "block version {} is below canonical version {} at activation height {}",
                    version, CANONICAL_BLOCK_VERSION, activation_height
                )));
            }
        }

        let mut header = BlockHeader {
            version,
            parent_hash,
            uncle_hashes: Vec::new(),
            coinbase,
            state_root: state_root.unwrap_or_else(Hash::zero),
            transactions_root: transactions_root.unwrap_or_else(Hash::zero),
            receipts_root: receipts_root.unwrap_or_else(Hash::zero),
            number: U256::from(number),
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp,
            difficulty: U256::from(difficulty_u64),
            nonce,
            extra_data,
            mix_hash,
            base_fee_per_gas: U256::from(1_000_000_000u64),
            hash,
        };
        if let Some(body) = &body {
            header
                .reconcile_body_commitments(body)
                .map_err(|err| RpcErrorObject::invalid_params(format!("{err}")))?;
        }
        validate_imported_block_header(&current.header, &mut header)?;
        let block = Block {
            header: header.clone(),
            body,
            uncles: Vec::new(),
        };
        let mut latest = self.latest_block.write();
        self.store_block(block.clone(), None).map_err(|err| {
            RpcErrorObject::internal_error(format!("failed to persist imported block: {err}"))
        })?;
        *latest = Some(block);
        set_global_synced_height(number);

        Ok(serde_json::json!({
            "imported": true,
            "height": number,
            "hash": format!("0x{}", hex::encode(header.hash.as_bytes())),
        }))
    }
}

fn leading_rabbit_target_from_difficulty(difficulty: U256) -> usize {
    let raw = difficulty.as_u64() as u128;
    if raw >= 8_000_000 {
        4
    } else if raw >= 2_000_000 {
        3
    } else {
        2
    }
}

fn legacy_target_from_leading_rabbit_bytes(bytes: usize) -> U256 {
    let mut target = [0xFFu8; 32];
    let prefix = bytes.min(32);
    target[..prefix].fill(0);
    U256::from_big_endian(&target)
}

fn mining_target_for_difficulty(
    difficulty: U256,
    target_leading_rabbit_bytes_override: Option<usize>,
) -> U256 {
    target_leading_rabbit_bytes_override
        .map(legacy_target_from_leading_rabbit_bytes)
        .unwrap_or_else(|| pow_target_from_difficulty(difficulty))
}

fn leading_rabbit_bytes_for_target(target: U256) -> usize {
    (target.leading_zeros() / 8) as usize
}

fn compute_mining_digest(parent_hash: Hash, height: u64, nonce: u64) -> [u8; 32] {
    let mut data = Vec::new();
    data.extend_from_slice(parent_hash.as_bytes());
    data.extend_from_slice(&height.to_be_bytes());
    data.extend_from_slice(&nonce.to_be_bytes());
    rabbitcore::crypto::keccak256(&data)
}

fn legacy_pow_meets_difficulty(digest: &[u8; 32], difficulty: U256) -> bool {
    digest.iter().take_while(|b| **b == 0).count()
        >= leading_rabbit_target_from_difficulty(difficulty)
}

fn pow_meets_block_rule(header_version: u32, digest: &[u8; 32], parent_difficulty: U256) -> bool {
    if header_version >= POW_TARGET_HEADER_VERSION {
        pow_hash_meets_target(digest, pow_target_from_difficulty(parent_difficulty))
    } else {
        legacy_pow_meets_difficulty(digest, parent_difficulty)
    }
}

fn adjust_mining_difficulty(parent_difficulty: U256, parent_timestamp: u64, now: u64) -> U256 {
    let elapsed = now.saturating_sub(parent_timestamp);
    let mut next = parent_difficulty.as_u64() as u128;
    if next == 0 {
        next = BASE_MINING_DIFFICULTY;
    }

    if elapsed <= TARGET_BLOCK_INTERVAL_SECS / 2 {
        next = next.saturating_mul(110) / 100;
    } else if elapsed >= TARGET_BLOCK_INTERVAL_SECS.saturating_mul(2) {
        next = next.saturating_mul(90) / 100;
    }

    let bounded = next.clamp(MIN_MINING_DIFFICULTY, MAX_MINING_DIFFICULTY);
    U256::from_u128(bounded)
}

pub(super) fn current_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(super) fn compute_error_to_json(err: &rabbitcore::compute::ComputeError) -> serde_json::Value {
    let (numeric_code, code, category) = match err {
        rabbitcore::compute::ComputeError::DomainNotRegistered(_)
        | rabbitcore::compute::ComputeError::DomainNotPublic(_)
        | rabbitcore::compute::ComputeError::DomainMismatch { .. } => {
            (1001, "domain_error", "domain")
        }
        rabbitcore::compute::ComputeError::ReadVersionMismatch { .. }
        | rabbitcore::compute::ComputeError::ReadSetValidationFailed => {
            (2001, "readset_error", "readset")
        }
        rabbitcore::compute::ComputeError::AuthorizationDenied => {
            (3001, "authorization_error", "authorization")
        }
        rabbitcore::compute::ComputeError::OwnershipCheckFailed => {
            (3002, "ownership_check_failed", "authorization")
        }
        rabbitcore::compute::ComputeError::InvalidSignature => {
            (3003, "invalid_signature", "authorization")
        }
        rabbitcore::compute::ComputeError::SignatureOwnerMismatch => {
            (3004, "signature_owner_mismatch", "authorization")
        }
        rabbitcore::compute::ComputeError::TxIdMismatch => (3005, "tx_id_mismatch", "authorization"),
        rabbitcore::compute::ComputeError::UnsupportedSignatureScheme => {
            (3006, "unsupported_signature_scheme", "authorization")
        }
        rabbitcore::compute::ComputeError::InvalidPredecessor
        | rabbitcore::compute::ComputeError::InvalidVersionProgression
        | rabbitcore::compute::ComputeError::DuplicateOutputId
        | rabbitcore::compute::ComputeError::ObjectNotFound(_) => (4001, "state_error", "state"),
        rabbitcore::compute::ComputeError::ResourcePolicyViolation => {
            (5001, "resource_error", "resource")
        }
        rabbitcore::compute::ComputeError::InvalidObjectKind
        | rabbitcore::compute::ComputeError::InvalidOperation(_) => (6001, "op_error", "operation"),
    };

    serde_json::json!({
        "numeric_code": numeric_code,
        "code": code,
        "category": category,
        "message": err.to_string(),
    })
}

/// RPC Server
pub struct RpcServer {
    config: RpcConfig,
    api: Option<Arc<RpcApi>>,
    server_task: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
    shutdown_tx: parking_lot::Mutex<Option<oneshot::Sender<()>>>,
}

#[derive(Clone)]
struct RpcServerState {
    api: Arc<RpcApi>,
    security: Arc<RpcSecurityContext>,
}

struct RpcSecurityContext {
    auth_token: Option<String>,
    rate_limit_per_minute: u32,
    buckets: parking_lot::Mutex<HashMap<String, VecDeque<u64>>>,
}

impl RpcSecurityContext {
    fn new(config: &RpcConfig) -> Self {
        Self {
            auth_token: config.auth_token.clone(),
            rate_limit_per_minute: config.rate_limit_per_minute,
            buckets: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    fn allow_request(&self, client: &str) -> bool {
        if self.rate_limit_per_minute == 0 {
            return true;
        }

        let now = current_unix_secs();
        let mut buckets = self.buckets.lock();
        let window = buckets.entry(client.to_string()).or_default();
        while let Some(ts) = window.front() {
            if now.saturating_sub(*ts) > 60 {
                window.pop_front();
            } else {
                break;
            }
        }

        if window.len() >= self.rate_limit_per_minute as usize {
            return false;
        }
        window.push_back(now);
        true
    }
}

impl RpcServer {
    /// Creates server with validation and returns detailed error on invalid config.
    pub fn try_new(config: RpcConfig) -> Result<Self, crate::ApiError> {
        config.validate().map_err(crate::ApiError::InvalidRequest)?;

        let api = Some(Arc::new(build_default_rpc_api(config.clone())?));
        Ok(Self {
            config,
            api,
            server_task: parking_lot::Mutex::new(None),
            shutdown_tx: parking_lot::Mutex::new(None),
        })
    }

    /// Creates server with validation.
    pub fn new(config: RpcConfig) -> Result<Self, crate::ApiError> {
        Self::try_new(config)
    }

    /// Create server with externally provided RPC API instance.
    pub fn with_api(config: RpcConfig, api: Arc<RpcApi>) -> Self {
        Self {
            config,
            api: Some(api),
            server_task: parking_lot::Mutex::new(None),
            shutdown_tx: parking_lot::Mutex::new(None),
        }
    }

    /// Returns the RPC API instance if initialized.
    pub fn api(&self) -> Option<Arc<RpcApi>> {
        self.api.clone()
    }

    pub async fn start(&self) -> Result<(), crate::ApiError> {
        if self.server_task.lock().is_some() {
            return Ok(());
        }

        let api = self
            .api
            .as_ref()
            .cloned()
            .ok_or_else(|| crate::ApiError::Internal("RPC API not initialized".to_string()))?;

        let state = RpcServerState {
            api,
            security: Arc::new(RpcSecurityContext::new(&self.config)),
        };

        let app = Router::new()
            .route("/", post(handle_rpc_request))
            .layer(DefaultBodyLimit::max(self.config.max_request_size))
            .layer(
                CorsLayer::new()
                    .allow_origin(Any)
                    .allow_headers(Any)
                    .allow_methods(Any),
            )
            .with_state(state);

        let bind_addr = format!("{}:{}", self.config.address, self.config.port);
        let listener = tokio::net::TcpListener::bind(&bind_addr)
            .await
            .map_err(|e| {
                crate::ApiError::IO(std::io::Error::new(std::io::ErrorKind::AddrInUse, e))
            })?;

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        *self.shutdown_tx.lock() = Some(shutdown_tx);

        let task = tokio::spawn(async move {
            let server = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            });

            if let Err(err) = server.await {
                tracing::error!("RPC server exited with error: {}", err);
            }
        });

        *self.server_task.lock() = Some(task);
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), crate::ApiError> {
        if let Some(tx) = self.shutdown_tx.lock().take() {
            let _ = tx.send(());
        }
        let task = self.server_task.lock().take();
        if let Some(task) = task {
            let _ = task.await;
        }
        Ok(())
    }
}

async fn handle_rpc_request(
    State(state): State<RpcServerState>,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    if method_requires_auth_token(&request.method) && state.security.auth_token.is_none() {
        return Json(JsonRpcResponse {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(RpcErrorObject {
                code: -32001,
                message: "Unauthorized".into(),
                data: Some(serde_json::json!(
                    "stateful rpc methods require auth_token on this node"
                )),
            }),
            id: request.id,
        });
    }

    if !is_authorized(&headers, state.security.auth_token.as_deref()) {
        return Json(JsonRpcResponse {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(RpcErrorObject {
                code: -32001,
                message: "Unauthorized".into(),
                data: None,
            }),
            id: request.id,
        });
    }

    let client = remote_addr.ip().to_string();
    if !state.security.allow_request(&client) {
        return Json(JsonRpcResponse {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(RpcErrorObject {
                code: -32005,
                message: "Rate limit exceeded".into(),
                data: Some(serde_json::json!({
                    "client": client,
                    "limit_per_minute": state.security.rate_limit_per_minute
                })),
            }),
            id: request.id,
        });
    }

    Json(state.api.handle_request(request).await)
}

fn is_authorized(headers: &HeaderMap, expected_token: Option<&str>) -> bool {
    let Some(expected) = expected_token else {
        return true;
    };

    let bearer_ok = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.trim() == expected)
        .unwrap_or(false);
    if bearer_ok {
        return true;
    }

    headers
        .get("x-rabbit-token")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim() == expected)
        .unwrap_or(false)
}

fn method_requires_auth_token(method: &str) -> bool {
    matches!(
        method,
        "rabbit_submitComputeTx" | "rabbit_submitWork" | "rabbit_importBlock"
    )
}

fn validate_compute_tx_network(tx: &ComputeTx, config: &RpcConfig) -> Result<(), RpcErrorObject> {
    let tx_chain_id = tx.chain_id.ok_or_else(|| {
        RpcErrorObject::invalid_params("compute tx chain_id must be set".to_string())
    })?;
    let tx_network_id = tx.network_id.ok_or_else(|| {
        RpcErrorObject::invalid_params("compute tx network_id must be set".to_string())
    })?;

    if tx_chain_id != config.chain_id {
        return Err(RpcErrorObject::invalid_params(format!(
            "compute tx chain_id {} does not match node chain_id {}",
            tx_chain_id, config.chain_id
        )));
    }
    if u64::from(tx_network_id) != config.network_id {
        return Err(RpcErrorObject::invalid_params(format!(
            "compute tx network_id {} does not match node network_id {}",
            tx_network_id, config.network_id
        )));
    }

    Ok(())
}

fn validate_imported_block_header(
    parent: &BlockHeader,
    header: &mut BlockHeader,
) -> Result<(), RpcErrorObject> {
    let expected_hash = header.compute_hash();
    if header.hash != expected_hash {
        return Err(RpcErrorObject::invalid_params(
            "block hash does not match header contents".to_string(),
        ));
    }
    if header.version == 0 {
        return Err(RpcErrorObject::invalid_params(
            "block version must be non-zero".to_string(),
        ));
    }
    if let Some(activation_height) = global_block_activation_height() {
        if header.number.as_u64() >= activation_height && header.version < CANONICAL_BLOCK_VERSION {
            return Err(RpcErrorObject::invalid_params(format!(
                "block version {} is below canonical version {} at activation height {}",
                header.version, CANONICAL_BLOCK_VERSION, activation_height
            )));
        }
    }

    header
        .validate(parent)
        .map_err(|e| RpcErrorObject::invalid_params(format!("invalid block header: {e}")))?;

    let expected_difficulty =
        adjust_mining_difficulty(parent.difficulty, parent.timestamp, header.timestamp);
    if header.difficulty != expected_difficulty {
        return Err(RpcErrorObject::invalid_params(format!(
            "invalid difficulty: expected 0x{:x}, got 0x{:x}",
            expected_difficulty.as_u64(),
            header.difficulty.as_u64()
        )));
    }

    let expected_mix = compute_mining_digest(parent.hash, header.number.as_u64(), header.nonce);
    if header.mix_hash != Hash::from_bytes(expected_mix) {
        return Err(RpcErrorObject::invalid_params(
            "block mix_hash does not match expected mining digest".to_string(),
        ));
    }

    if !pow_meets_block_rule(header.version, &expected_mix, parent.difficulty) {
        return Err(RpcErrorObject::invalid_params(format!(
            "block pow below required target: hash=0x{} target={}",
            hex::encode(expected_mix),
            pow_target_to_hex(pow_target_from_difficulty(parent.difficulty))
        )));
    }

    Ok(())
}

fn build_default_rpc_api(config: RpcConfig) -> std::result::Result<RpcApi, crate::ApiError> {
    RPC_METRICS.init()?;

    let state_db = Arc::new(StateDb::new(Hash::zero()));

    let persistent_db = build_compute_kv_backend(&config)?;
    let compute_store = Arc::new(ComputeStore::new(persistent_db));

    let domains = Arc::new(InMemoryDomainRegistry::new());
    domains.upsert_domain(DomainConfig {
        domain_id: DomainId(0),
        name: "main".to_string(),
        vm: "wasm".to_string(),
        public: true,
    });
    domains.upsert_domain(DomainConfig {
        domain_id: GAME_DOMAIN,
        name: "jzz".to_string(),
        vm: "shanhai".to_string(),
        public: true,
    });

    Ok(RpcApi::with_persistent_compute(
        config,
        state_db,
        compute_store,
        domains,
    ))
}

fn build_compute_kv_backend(
    config: &RpcConfig,
) -> std::result::Result<Arc<dyn KeyValueDB>, crate::ApiError> {
    match config.compute_backend {
        ComputeBackend::Mem => Ok(Arc::new(MemDatabase::new())),
        ComputeBackend::RocksDb => {
            let db = RocksDb::open(&config.compute_db_path).map_err(|err| {
                crate::ApiError::InvalidRequest(format!(
                    "failed to open rocksdb at {}: {}",
                    config.compute_db_path, err
                ))
            })?;
            Ok(Arc::new(db))
        }
        ComputeBackend::Redb => {
            let db = RedbDatabase::open(&config.compute_db_path).map_err(|err| {
                crate::ApiError::InvalidRequest(format!(
                    "failed to open redb at {}: {}",
                    config.compute_db_path, err
                ))
            })?;
            Ok(Arc::new(db))
        }
    }
}

fn parse_address(s: &str) -> Result<Address, RpcErrorObject> {
    let raw = s.trim();
    if raw.len() != 42 {
        return Err(RpcErrorObject::invalid_params(
            "Address must be 20 bytes".to_string(),
        ));
    }
    let prefix = raw.get(..2).ok_or_else(|| {
        RpcErrorObject::invalid_params("Address must use 0x prefix".to_string())
    })?;
    if prefix != "0x" {
        return Err(RpcErrorObject::invalid_params(
            "Address must use 0x prefix".to_string(),
        ));
    }
    let body = raw
        .get(2..)
        .ok_or_else(|| RpcErrorObject::invalid_params("Address must be 20 bytes".to_string()))?;
    if body.len() != 40 || !body.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(RpcErrorObject::invalid_params(
            "Address must be 20 bytes".to_string(),
        ));
    }
    let bytes = hex::decode(body)
        .map_err(|e| RpcErrorObject::invalid_params(format!("Invalid address: {}", e)))?;

    Address::from_slice(&bytes)
        .map_err(|e| RpcErrorObject::invalid_params(format!("Invalid address: {e}")))
}

fn parse_hash(s: &str) -> Result<Hash, RpcErrorObject> {
    let bytes = hex::decode(s.strip_prefix("0x").unwrap_or(s))
        .map_err(|e| RpcErrorObject::invalid_params(format!("Invalid hash: {}", e)))?;

    if bytes.len() != 32 {
        return Err(RpcErrorObject::invalid_params(
            "Hash must be 32 bytes".into(),
        ));
    }

    Hash::from_slice(&bytes)
        .map_err(|e| RpcErrorObject::invalid_params(format!("Invalid hash: {e}")))
}

fn parse_u256_hex(s: &str) -> Result<U256, RpcErrorObject> {
    let raw = s.strip_prefix("0x").unwrap_or(s);
    let normalized = if raw.len().is_multiple_of(2) {
        raw.to_string()
    } else {
        format!("0{}", raw)
    };
    let bytes = hex::decode(normalized)
        .map_err(|e| RpcErrorObject::invalid_params(format!("Invalid u256 hex: {}", e)))?;
    if bytes.len() > 32 {
        return Err(RpcErrorObject::invalid_params(
            "u256 must be <= 32 bytes".to_string(),
        ));
    }
    Ok(U256::from_big_endian(&bytes))
}

fn parse_object_id(s: &str) -> Result<ObjectId, RpcErrorObject> {
    Ok(ObjectId(parse_hash(s)?))
}

fn parse_output_id(s: &str) -> Result<OutputId, RpcErrorObject> {
    Ok(OutputId(parse_hash(s)?))
}

fn block_reward_for_height(block_number: u64) -> U256 {
    let mut reward = rabbitcore::INITIAL_BLOCK_REWARD;
    let halving_count = block_number / rabbitcore::HALVING_PERIOD;
    for _ in 0..halving_count {
        reward /= 2;
    }
    U256::from_u128(reward)
}

fn format_u256_hex(value: U256) -> String {
    let bytes = value.to_big_endian();
    let first_non_zero = bytes.iter().position(|b| *b != 0);
    match first_non_zero {
        Some(idx) => {
            let encoded = hex::encode(&bytes[idx..]);
            let trimmed = encoded.trim_start_matches('0');
            if trimmed.is_empty() {
                "0x0".to_string()
            } else {
                format!("0x{}", trimmed)
            }
        }
        None => "0x0".to_string(),
    }
}

fn format_u128_hex(value: u128) -> String {
    format!("0x{:x}", value)
}

fn format_rabbit_address(address: Address) -> String {
    let lower_hex = hex::encode(address.as_bytes());
    let hash = rabbitcore::crypto::keccak256(lower_hex.as_bytes());
    let mut checksummed = String::with_capacity(40);

    for (idx, ch) in lower_hex.chars().enumerate() {
        let nibble = if idx % 2 == 0 {
            (hash[idx / 2] >> 4) & 0x0f
        } else {
            hash[idx / 2] & 0x0f
        };

        if ch.is_ascii_hexdigit() && ch.is_ascii_lowercase() && nibble >= 8 {
            checksummed.push(ch.to_ascii_uppercase());
        } else {
            checksummed.push(ch);
        }
    }

    format!("0x{}", checksummed)
}

fn compute_tx_to_json(tx_hash: Hash, result: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "kind": "compute",
        "tx_hash": format!("0x{}", hex::encode(tx_hash.as_bytes())),
        "hash": format!("0x{}", hex::encode(tx_hash.as_bytes())),
        "timestamp": result.get("submitted_at_unix").and_then(|v| v.as_u64()).unwrap_or(0),
        "result": result,
    })
}

fn block_to_rabbit_block_json(block: &Block) -> serde_json::Value {
    serde_json::json!({
        "version": block.header.version,
        "hash": format!("0x{}", hex::encode(block.header.hash.as_bytes())),
        "parent_hash": format!("0x{}", hex::encode(block.header.parent_hash.as_bytes())),
        "state_root": format!("0x{}", hex::encode(block.header.state_root.as_bytes())),
        "transactions_root": format!("0x{}", hex::encode(block.header.transactions_root.as_bytes())),
        "receipts_root": format!("0x{}", hex::encode(block.header.receipts_root.as_bytes())),
        "number": format!("0x{:x}", block.header.number.as_u64()),
        "gas_limit": format!("0x{:x}", block.header.gas_limit),
        "gas_used": format!("0x{:x}", block.header.gas_used),
        "timestamp": block.header.timestamp,
        "difficulty": format!("0x{:x}", block.header.difficulty.as_u64()),
        "nonce": block.header.nonce,
        "coinbase": format_rabbit_address(block.header.coinbase),
        "mix_hash": format!("0x{}", hex::encode(block.header.mix_hash.as_bytes())),
        "extra_data": format!("0x{}", hex::encode(&block.header.extra_data)),
        "base_fee_per_gas": format_u256_hex(block.header.base_fee_per_gas),
    })
}

fn block_body_record_to_json(record: &BlockBodyRecord) -> serde_json::Value {
    let roots = record.body.commitment_roots();
    serde_json::json!({
        "block_hash": format!("0x{}", hex::encode(record.block_hash.as_bytes())),
        "number": format!("0x{:x}", record.number),
        "version": record.body.version,
        "tx_count": record.body.tx_count(),
        "receipt_count": record.body.receipt_count(),
        "transactions_root": format!("0x{}", hex::encode(roots.transactions_root.as_bytes())),
        "receipts_root": format!("0x{}", hex::encode(roots.receipts_root.as_bytes())),
        "transactions": record.body.transactions.iter().map(tx_to_json).collect::<Vec<_>>(),
        "receipts": record.body.receipts.iter().map(receipt_to_json).collect::<Vec<_>>(),
    })
}

fn tx_to_json(tx: &rabbitcore::block::TxEnvelope) -> serde_json::Value {
    serde_json::json!({
        "tx_id": format!("0x{}", hex::encode(tx.tx_id.0.as_bytes())),
        "domain_id": tx.domain_id.0,
        "command": format!("{:?}", tx.command),
        "input_set": tx.input_set.iter().map(|id| format!("0x{}", hex::encode(id.0.as_bytes()))).collect::<Vec<_>>(),
        "read_set": tx.read_set.iter().map(|read| serde_json::json!({
            "output_id": format!("0x{}", hex::encode(read.output_id.0.as_bytes())),
            "domain_id": read.domain_id.0,
            "expected_version": read.expected_version.0,
        })).collect::<Vec<_>>(),
        "output_proposals": tx.output_proposals.iter().map(output_proposal_to_json).collect::<Vec<_>>(),
        "fee": tx.fee,
        "nonce": tx.nonce,
        "metadata": metadata_to_json(&tx.metadata),
        "payload": format!("0x{}", hex::encode(&tx.payload)),
        "deadline_unix_secs": tx.deadline_unix_secs,
        "chain_id": tx.chain_id,
        "network_id": tx.network_id,
        "witness": witness_to_json(&tx.witness),
    })
}

fn output_proposal_to_json(output: &OutputProposal) -> serde_json::Value {
    serde_json::json!({
        "output_id": format!("0x{}", hex::encode(output.output_id.0.as_bytes())),
        "object_id": format!("0x{}", hex::encode(output.object_id.0.as_bytes())),
        "domain_id": output.domain_id.0,
        "kind": format!("{:?}", output.kind),
        "owner": ownership_to_json(&output.owner),
        "predecessor": output.predecessor.map(|id| format!("0x{}", hex::encode(id.0.as_bytes()))),
        "version": output.version.0,
        "state": format!("0x{}", hex::encode(&output.state)),
        "state_root": output.state_root.map(|root| format!("0x{}", hex::encode(root.as_bytes()))),
        "resources": resource_map_to_json(&output.resources),
        "lock": script_to_json(&output.lock),
        "logic": output.logic.as_ref().map(script_to_json),
        "created_at": output.created_at,
        "ttl": output.ttl,
        "rent_reserve": output.rent_reserve.map(format_u128_hex),
        "flags": output.flags,
        "extensions": metadata_to_json(&output.extensions),
    })
}

fn witness_to_json(witness: &TxWitness) -> serde_json::Value {
    serde_json::json!({
        "threshold": witness.threshold,
        "signatures": witness.signatures.iter().map(|sig| serde_json::json!({
            "scheme": format!("{:?}", sig.scheme),
            "signature": format!("0x{}", hex::encode(&sig.bytes)),
            "public_key": sig.public_key.as_ref().map(|key| format!("0x{}", hex::encode(key))),
        })).collect::<Vec<_>>(),
    })
}

fn receipt_to_json(receipt: &rabbitcore::block::Receipt) -> serde_json::Value {
    serde_json::json!({
        "tx_id": format!("0x{}", hex::encode(receipt.tx_id.0.as_bytes())),
        "block_hash": format!("0x{}", hex::encode(receipt.block_hash.as_bytes())),
        "status": format!("{:?}", receipt.status),
        "gas_used": receipt.gas_used,
        "compute_units": receipt.compute_units,
        "output_refs": receipt.output_refs.iter().map(|id| format!("0x{}", hex::encode(id.0.as_bytes()))).collect::<Vec<_>>(),
        "logs": receipt.logs.iter().map(|log| serde_json::json!({
            "topic": log.topic,
            "data": format!("0x{}", hex::encode(&log.data)),
        })).collect::<Vec<_>>(),
        "error": receipt.error,
    })
}

fn object_output_to_json(output: ObjectOutput) -> serde_json::Value {
    serde_json::json!({
        "output_id": format!("0x{}", hex::encode(output.output_id.0.as_bytes())),
        "object_id": format!("0x{}", hex::encode(output.object_id.0.as_bytes())),
        "version": output.version.0,
        "domain_id": output.domain_id.0,
        "kind": format!("{:?}", output.kind),
        "owner": ownership_to_json(&output.owner),
        "spent": output.spent,
        "predecessor": output.predecessor.map(|p| format!("0x{}", hex::encode(p.0.as_bytes()))),
        "state": format!("0x{}", hex::encode(output.state)),
        "state_root": output
            .state_root
            .map(|root| format!("0x{}", hex::encode(root.as_bytes()))),
        "resources": resource_map_to_json(&output.resources),
        "lock": script_to_json(&output.lock),
        "logic": output.logic.as_ref().map(script_to_json),
        "created_at": output.created_at,
        "ttl": output.ttl,
        "rent_reserve": output.rent_reserve.map(format_u128_hex),
        "flags": output.flags,
        "extensions": metadata_to_json(&output.extensions),
    })
}

fn ownership_to_json(owner: &Ownership) -> serde_json::Value {
    match owner {
        Ownership::Shared => serde_json::json!({ "type": "Shared" }),
        Ownership::Address(address) => serde_json::json!({
            "type": "Address",
            "address": format_rabbit_address(*address),
        }),
        Ownership::Program(address) => serde_json::json!({
            "type": "Program",
            "address": format_rabbit_address(*address),
        }),
        Ownership::Ed25519(public_key) => serde_json::json!({
            "type": "Ed25519",
            "public_key": format!("0x{}", hex::encode(public_key)),
        }),
    }
}

fn script_to_json(script: &Script) -> serde_json::Value {
    serde_json::json!({
        "vm": script.vm,
        "code": format!("0x{}", hex::encode(&script.code)),
    })
}

fn resource_map_to_json(resources: &ResourceMap) -> serde_json::Value {
    let values = resources
        .iter()
        .map(|(asset_id, value)| {
            let value = match value {
                ResourceValue::Amount(amount) => serde_json::json!({
                    "type": "Amount",
                    "amount": format_u128_hex(*amount),
                }),
                ResourceValue::Data(data) => serde_json::json!({
                    "type": "Data",
                    "data": format!("0x{}", hex::encode(data)),
                }),
                ResourceValue::Ref(object_id) => serde_json::json!({
                    "type": "Ref",
                    "object_id": format!("0x{}", hex::encode(object_id.0.as_bytes())),
                }),
                ResourceValue::RefBatch(object_ids) => serde_json::json!({
                    "type": "RefBatch",
                    "object_ids": object_ids
                        .iter()
                        .map(|id| format!("0x{}", hex::encode(id.0.as_bytes())))
                        .collect::<Vec<_>>(),
                }),
            };
            serde_json::json!({
                "asset_id": format!("0x{}", hex::encode(asset_id.as_bytes())),
                "value": value,
            })
        })
        .collect::<Vec<_>>();
    serde_json::Value::Array(values)
}

fn metadata_to_json(metadata: &[(String, Vec<u8>)]) -> serde_json::Value {
    serde_json::Value::Array(
        metadata
            .iter()
            .map(|(key, value)| {
                serde_json::json!({
                    "key": key,
                    "value": format!("0x{}", hex::encode(value)),
                })
            })
            .collect::<Vec<_>>(),
    )
}

fn parse_compute_tx(value: serde_json::Value) -> Result<ComputeTx, RpcErrorObject> {
    let obj = value
        .as_object()
        .ok_or_else(|| RpcErrorObject::invalid_params("tx must be object".to_string()))?;

    let tx_id = parse_hash_field(obj, "tx_id").map(rabbitcore::compute::TxId)?;
    let domain_id = DomainId(parse_u32_field(obj, "domain_id")?);
    let command =
        parse_command(obj.get("command").and_then(|v| v.as_str()).ok_or_else(|| {
            RpcErrorObject::invalid_params("command must be string".to_string())
        })?)?;

    let input_set = parse_hash_array_field(obj, "input_set")?
        .into_iter()
        .map(OutputId)
        .collect::<Vec<_>>();

    let read_set = parse_read_set(obj.get("read_set"))?;
    let output_proposals = parse_output_proposals(obj.get("output_proposals"))?;
    let fee = parse_u64_opt(obj.get("fee"))?.unwrap_or(0);
    let nonce = parse_u64_opt(obj.get("nonce"))?;
    let metadata = parse_metadata(obj.get("metadata"))?;

    let payload = parse_bytes_hex_opt(obj.get("payload"))?.unwrap_or_default();
    let deadline_unix_secs = obj.get("deadline_unix_secs").and_then(|v| v.as_u64());
    let chain_id = obj.get("chain_id").and_then(|v| v.as_u64());
    let network_id = match obj.get("network_id").and_then(|v| v.as_u64()) {
        None => None,
        Some(v) => Some(
            u32::try_from(v)
                .map_err(|_| RpcErrorObject::invalid_params("network_id overflow".to_string()))?,
        ),
    };
    let witness = parse_witness(obj.get("witness"))?;

    Ok(ComputeTx {
        tx_id,
        domain_id,
        command,
        input_set,
        read_set,
        output_proposals,
        fee,
        nonce,
        metadata,
        payload,
        deadline_unix_secs,
        chain_id,
        network_id,
        witness,
        max_fee: obj.get("max_fee").and_then(|v| v.as_u64()).unwrap_or(0),
        priority_fee: obj.get("priority_fee").and_then(|v| v.as_u64()).unwrap_or(0),
        gas_limit: obj.get("gas_limit").and_then(|v| v.as_u64()).unwrap_or(0),
    })
}

pub fn canonicalize_compute_tx_json(
    mut tx_json: serde_json::Value,
) -> Result<serde_json::Value, RpcErrorObject> {
    let mut tx = parse_compute_tx(tx_json.clone())?;
    tx.assign_expected_tx_id();
    tx_json["tx_id"] = serde_json::Value::String(format!("0x{}", tx.tx_id.0.to_hex()));
    Ok(tx_json)
}

fn parse_witness(v: Option<&serde_json::Value>) -> Result<TxWitness, RpcErrorObject> {
    let obj = v
        .and_then(|x| x.as_object())
        .ok_or_else(|| RpcErrorObject::invalid_params("witness must be object".to_string()))?;
    let sig_arr = obj
        .get("signatures")
        .and_then(|x| x.as_array())
        .ok_or_else(|| {
            RpcErrorObject::invalid_params("witness.signatures must be array".to_string())
        })?;

    let mut signatures = Vec::with_capacity(sig_arr.len());
    for raw in sig_arr {
        let obj = raw.as_object().ok_or_else(|| {
            RpcErrorObject::invalid_params("signature must be object".to_string())
        })?;
        let scheme = obj.get("scheme").and_then(|x| x.as_str()).ok_or_else(|| {
            RpcErrorObject::invalid_params("signature.scheme must be string".to_string())
        })?;
        let sig_hex = obj
            .get("signature")
            .and_then(|x| x.as_str())
            .ok_or_else(|| {
                RpcErrorObject::invalid_params("signature.signature must be string".to_string())
            })?;
        let sig_bytes = hex::decode(sig_hex.strip_prefix("0x").unwrap_or(sig_hex))
            .map_err(|e| RpcErrorObject::invalid_params(format!("invalid signature hex: {e}")))?;

        match scheme {
            "ed25519" => {
                let pubkey_hex =
                    obj.get("public_key")
                        .and_then(|x| x.as_str())
                        .ok_or_else(|| {
                            RpcErrorObject::invalid_params(
                                "ed25519 signature requires public_key".to_string(),
                            )
                        })?;
                let pubkey = hex::decode(pubkey_hex.strip_prefix("0x").unwrap_or(pubkey_hex))
                    .map_err(|e| {
                        RpcErrorObject::invalid_params(format!("invalid public_key hex: {e}"))
                    })?;
                if sig_bytes.len() != 64 {
                    return Err(RpcErrorObject::invalid_params(
                        "ed25519 signature must be 64 bytes".to_string(),
                    ));
                }
                if pubkey.len() != 32 {
                    return Err(RpcErrorObject::invalid_params(
                        "ed25519 public_key must be 32 bytes".to_string(),
                    ));
                }
                signatures.push(TxSignature {
                    scheme: SignatureScheme::Ed25519,
                    bytes: sig_bytes,
                    public_key: Some(pubkey),
                });
            }
            other => {
                return Err(RpcErrorObject::invalid_params(format!(
                    "unsupported signature scheme: {other}; only ed25519 is supported"
                )));
            }
        }
    }

    let threshold = match obj.get("threshold") {
        None | Some(serde_json::Value::Null) => None,
        Some(raw) => {
            let v = raw.as_u64().ok_or_else(|| {
                RpcErrorObject::invalid_params("witness.threshold must be u64".to_string())
            })?;
            Some(u16::try_from(v).map_err(|_| {
                RpcErrorObject::invalid_params("witness.threshold overflow".to_string())
            })?)
        }
    };

    Ok(TxWitness {
        signatures,
        threshold,
    })
}

fn parse_command(s: &str) -> Result<Command, RpcErrorObject> {
    match s {
        "Transfer" => Ok(Command::Transfer),
        "Invoke" => Ok(Command::Invoke),
        "Mint" => Ok(Command::Mint),
        "Burn" => Ok(Command::Burn),
        "Anchor" => Ok(Command::Anchor),
        "Reveal" => Ok(Command::Reveal),
        "AgentTick" => Ok(Command::AgentTick),
        _ => Err(RpcErrorObject::invalid_params(format!(
            "unsupported command: {s}"
        ))),
    }
}

fn parse_object_kind(s: &str) -> Result<ObjectKind, RpcErrorObject> {
    match s {
        "Asset" => Ok(ObjectKind::Asset),
        "Code" => Ok(ObjectKind::Code),
        "State" => Ok(ObjectKind::State),
        "Capability" => Ok(ObjectKind::Capability),
        "Agent" => Ok(ObjectKind::Agent),
        "Anchor" => Ok(ObjectKind::Anchor),
        "Ticket" => Ok(ObjectKind::Ticket),
        _ => Err(RpcErrorObject::invalid_params(format!(
            "unsupported object kind: {s}"
        ))),
    }
}

fn parse_ownership(v: Option<&serde_json::Value>) -> Result<Ownership, RpcErrorObject> {
    let Some(v) = v else {
        return Ok(Ownership::Shared);
    };
    let obj = v
        .as_object()
        .ok_or_else(|| RpcErrorObject::invalid_params("owner must be object".to_string()))?;
    let typ = obj
        .get("type")
        .and_then(|x| x.as_str())
        .ok_or_else(|| RpcErrorObject::invalid_params("owner.type missing".to_string()))?;
    match typ {
        "Shared" => Ok(Ownership::Shared),
        "Address" => {
            let addr = obj.get("address").and_then(|x| x.as_str()).ok_or_else(|| {
                RpcErrorObject::invalid_params("owner.address missing".to_string())
            })?;
            Ok(Ownership::Address(parse_address(addr)?))
        }
        "Program" => {
            let addr = obj.get("address").and_then(|x| x.as_str()).ok_or_else(|| {
                RpcErrorObject::invalid_params("owner.address missing".to_string())
            })?;
            Ok(Ownership::Program(parse_address(addr)?))
        }
        "Ed25519" => {
            let pubkey = obj
                .get("public_key")
                .and_then(|x| x.as_str())
                .ok_or_else(|| {
                    RpcErrorObject::invalid_params("owner.public_key missing".to_string())
                })?;
            let bytes = hex::decode(pubkey.strip_prefix("0x").unwrap_or(pubkey)).map_err(|e| {
                RpcErrorObject::invalid_params(format!("invalid owner.public_key hex: {e}"))
            })?;
            if bytes.len() != 32 {
                return Err(RpcErrorObject::invalid_params(
                    "owner.public_key must be 32 bytes".to_string(),
                ));
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            Ok(Ownership::Ed25519(arr))
        }
        _ => Err(RpcErrorObject::invalid_params(format!(
            "unsupported owner type: {typ}"
        ))),
    }
}

fn parse_read_set(
    v: Option<&serde_json::Value>,
) -> Result<Vec<rabbitcore::compute::ObjectReadRef>, RpcErrorObject> {
    let Some(v) = v else {
        return Ok(vec![]);
    };
    let arr = v
        .as_array()
        .ok_or_else(|| RpcErrorObject::invalid_params("read_set must be array".to_string()))?;

    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let obj = item.as_object().ok_or_else(|| {
            RpcErrorObject::invalid_params("read_set item must be object".to_string())
        })?;
        let output_id = parse_hash_field(obj, "output_id").map(OutputId)?;
        let domain_id = DomainId(parse_u32_field(obj, "domain_id")?);
        let expected_version = Version(
            obj.get("expected_version")
                .and_then(|x| x.as_u64())
                .ok_or_else(|| {
                    RpcErrorObject::invalid_params("expected_version missing".to_string())
                })?,
        );
        out.push(rabbitcore::compute::ObjectReadRef {
            output_id,
            domain_id,
            expected_version,
        });
    }
    Ok(out)
}

fn parse_output_proposals(
    v: Option<&serde_json::Value>,
) -> Result<Vec<OutputProposal>, RpcErrorObject> {
    let Some(v) = v else {
        return Ok(vec![]);
    };
    let arr = v.as_array().ok_or_else(|| {
        RpcErrorObject::invalid_params("output_proposals must be array".to_string())
    })?;

    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let obj = item.as_object().ok_or_else(|| {
            RpcErrorObject::invalid_params("output proposal must be object".to_string())
        })?;
        let output_id = parse_hash_field(obj, "output_id").map(OutputId)?;
        let object_id = parse_hash_field(obj, "object_id").map(ObjectId)?;
        let domain_id = DomainId(parse_u32_field(obj, "domain_id")?);
        let kind = parse_object_kind(
            obj.get("kind")
                .and_then(|x| x.as_str())
                .ok_or_else(|| RpcErrorObject::invalid_params("kind missing".to_string()))?,
        )?;
        let owner = parse_ownership(obj.get("owner"))?;
        let predecessor = match obj.get("predecessor") {
            Some(serde_json::Value::String(s)) => Some(OutputId(parse_hash(s)?)),
            Some(serde_json::Value::Null) | None => None,
            _ => {
                return Err(RpcErrorObject::invalid_params(
                    "predecessor must be hex string or null".to_string(),
                ));
            }
        };
        let version = Version(
            obj.get("version")
                .and_then(|x| x.as_u64())
                .ok_or_else(|| RpcErrorObject::invalid_params("version missing".to_string()))?,
        );
        let state = parse_bytes_hex_opt(obj.get("state"))?.unwrap_or_default();
        let state_root = parse_hash_opt(obj.get("state_root"))?;
        let resources = parse_resource_map(obj.get("resources"))?;
        let lock = parse_script(obj.get("lock"))?.unwrap_or_default();
        let logic = parse_script(obj.get("logic"))?;
        let created_at = parse_u64_opt(obj.get("created_at"))?.unwrap_or(0);
        let ttl = parse_u64_opt(obj.get("ttl"))?;
        let rent_reserve = parse_u128_opt(obj.get("rent_reserve"))?;
        let flags = parse_u32_opt(obj.get("flags"))?.unwrap_or(0);
        let extensions = parse_metadata(obj.get("extensions"))?;
        out.push(OutputProposal {
            output_id,
            object_id,
            domain_id,
            kind,
            owner,
            predecessor,
            version,
            state,
            state_root,
            resources,
            lock,
            logic,
            created_at,
            ttl,
            rent_reserve,
            flags,
            extensions,
        });
    }

    Ok(out)
}

fn parse_hash_array_field(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Vec<Hash>, RpcErrorObject> {
    let Some(v) = obj.get(key) else {
        return Ok(vec![]);
    };
    let arr = v
        .as_array()
        .ok_or_else(|| RpcErrorObject::invalid_params(format!("{key} must be array")))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item.as_str().ok_or_else(|| {
            RpcErrorObject::invalid_params(format!("{key} items must be hex string"))
        })?;
        out.push(parse_hash(s)?);
    }
    Ok(out)
}

fn parse_hash_field(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Hash, RpcErrorObject> {
    let s = obj
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcErrorObject::invalid_params(format!("{key} missing")))?;
    parse_hash(s)
}

fn parse_u64_hex_field(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<u64, RpcErrorObject> {
    let raw = obj
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcErrorObject::invalid_params(format!("{key} must be hex string")))?;
    let s = raw.strip_prefix("0x").unwrap_or(raw);
    u64::from_str_radix(s, 16)
        .map_err(|e| RpcErrorObject::invalid_params(format!("invalid {key} hex: {e}")))
}

fn parse_u32_field(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<u32, RpcErrorObject> {
    let v = obj
        .get(key)
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcErrorObject::invalid_params(format!("{key} missing")))?;
    u32::try_from(v).map_err(|_| RpcErrorObject::invalid_params(format!("{key} overflow")))
}

fn parse_get_work_request(
    params: Option<Vec<serde_json::Value>>,
) -> Result<GetWorkRequest, RpcErrorObject> {
    let Some(params) = params else {
        return Ok(GetWorkRequest::default());
    };
    let Some(first) = params.first() else {
        return Ok(GetWorkRequest::default());
    };
    if first.is_null() {
        return Ok(GetWorkRequest::default());
    }
    serde_json::from_value(first.clone())
        .map_err(|e| RpcErrorObject::invalid_params(format!("invalid getWork payload: {e}")))
}

fn work_matches_known_head(latest: &Block, req: &GetWorkRequest) -> Result<bool, RpcErrorObject> {
    let expected_height = latest.header.number.as_u64().saturating_add(1);
    if let Some(known_height) = req.known_height {
        if known_height != expected_height {
            return Ok(false);
        }
    }
    if let Some(known_prev_hash) = &req.known_prev_hash {
        let parsed = parse_hash(known_prev_hash)?;
        if parsed != latest.header.hash {
            return Ok(false);
        }
    }
    Ok(req.known_height.is_some() || req.known_prev_hash.is_some())
}

fn parse_u64_opt(v: Option<&serde_json::Value>) -> Result<Option<u64>, RpcErrorObject> {
    match v {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(num)) => num
            .as_u64()
            .map(Some)
            .ok_or_else(|| RpcErrorObject::invalid_params("expected u64".to_string())),
        Some(serde_json::Value::String(s)) => {
            let raw = s.trim();
            let parsed = if let Some(hex) = raw.strip_prefix("0x") {
                u64::from_str_radix(hex, 16)
            } else {
                raw.parse::<u64>()
            }
            .map_err(|e| RpcErrorObject::invalid_params(format!("invalid u64 value: {e}")))?;
            Ok(Some(parsed))
        }
        _ => Err(RpcErrorObject::invalid_params(
            "expected u64/hex string/null".to_string(),
        )),
    }
}

fn parse_bool_opt(v: Option<&serde_json::Value>) -> Result<Option<bool>, RpcErrorObject> {
    match v {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Bool(value)) => Ok(Some(*value)),
        Some(serde_json::Value::Number(num)) => match num.as_u64() {
            Some(0) => Ok(Some(false)),
            Some(1) => Ok(Some(true)),
            _ => Err(RpcErrorObject::invalid_params(
                "expected bool/0/1/null".to_string(),
            )),
        },
        Some(serde_json::Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => Ok(Some(true)),
            "false" | "0" => Ok(Some(false)),
            _ => Err(RpcErrorObject::invalid_params(
                "expected bool/0/1/null".to_string(),
            )),
        },
        _ => Err(RpcErrorObject::invalid_params(
            "expected bool/0/1/null".to_string(),
        )),
    }
}

fn parse_include_body_flag(
    params: Option<&Vec<serde_json::Value>>,
) -> Result<bool, RpcErrorObject> {
    let Some(params) = params else {
        return Ok(false);
    };
    let Some(first) = params.first() else {
        return Ok(false);
    };
    match first {
        serde_json::Value::Object(obj) => {
            Ok(parse_bool_opt(obj.get("include_body"))?.unwrap_or(false))
        }
        _ => Ok(parse_bool_opt(Some(first))?.unwrap_or(false)),
    }
}

fn parse_u32_opt(v: Option<&serde_json::Value>) -> Result<Option<u32>, RpcErrorObject> {
    let Some(raw) = parse_u64_opt(v)? else {
        return Ok(None);
    };
    Ok(Some(u32::try_from(raw).map_err(|_| {
        RpcErrorObject::invalid_params("u32 overflow".to_string())
    })?))
}

fn parse_u128_opt(v: Option<&serde_json::Value>) -> Result<Option<u128>, RpcErrorObject> {
    match v {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(num)) => {
            if let Some(v) = num.as_u64() {
                return Ok(Some(v as u128));
            }
            Err(RpcErrorObject::invalid_params("expected u128".to_string()))
        }
        Some(serde_json::Value::String(s)) => {
            let raw = s.trim();
            let parsed = if let Some(hex) = raw.strip_prefix("0x") {
                u128::from_str_radix(hex, 16)
            } else {
                raw.parse::<u128>()
            }
            .map_err(|e| RpcErrorObject::invalid_params(format!("invalid u128 value: {e}")))?;
            Ok(Some(parsed))
        }
        _ => Err(RpcErrorObject::invalid_params(
            "expected u128/hex string/null".to_string(),
        )),
    }
}

fn parse_hash_opt(v: Option<&serde_json::Value>) -> Result<Option<Hash>, RpcErrorObject> {
    match v {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(parse_hash(s)?)),
        _ => Err(RpcErrorObject::invalid_params(
            "expected hash hex string or null".to_string(),
        )),
    }
}

fn parse_script(v: Option<&serde_json::Value>) -> Result<Option<Script>, RpcErrorObject> {
    let Some(v) = v else {
        return Ok(None);
    };
    if v.is_null() {
        return Ok(None);
    }
    let obj = v
        .as_object()
        .ok_or_else(|| RpcErrorObject::invalid_params("script must be object".to_string()))?;
    let vm = parse_u64_opt(obj.get("vm"))?.unwrap_or(1);
    let vm = u8::try_from(vm)
        .map_err(|_| RpcErrorObject::invalid_params("script.vm overflow".to_string()))?;
    let code = parse_bytes_hex_opt(obj.get("code"))?.unwrap_or_default();
    Ok(Some(Script { vm, code }))
}

fn parse_resource_map(v: Option<&serde_json::Value>) -> Result<ResourceMap, RpcErrorObject> {
    let Some(v) = v else {
        return Ok(vec![]);
    };
    let arr = v
        .as_array()
        .ok_or_else(|| RpcErrorObject::invalid_params("resources must be array".to_string()))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let obj = item.as_object().ok_or_else(|| {
            RpcErrorObject::invalid_params("resource item must be object".to_string())
        })?;
        let asset_id = parse_hash(
            obj.get("asset_id")
                .and_then(|x| x.as_str())
                .ok_or_else(|| RpcErrorObject::invalid_params("asset_id missing".to_string()))?,
        )?;
        let value_obj = obj
            .get("value")
            .and_then(|x| x.as_object())
            .ok_or_else(|| RpcErrorObject::invalid_params("resource.value missing".to_string()))?;
        let value_type = value_obj
            .get("type")
            .and_then(|x| x.as_str())
            .ok_or_else(|| {
                RpcErrorObject::invalid_params("resource.value.type missing".to_string())
            })?;
        let value = match value_type {
            "Amount" => {
                ResourceValue::Amount(parse_u128_opt(value_obj.get("amount"))?.ok_or_else(
                    || RpcErrorObject::invalid_params("resource amount missing".to_string()),
                )?)
            }
            "Data" => ResourceValue::Data(parse_bytes_hex_opt(value_obj.get("data"))?.ok_or_else(
                || RpcErrorObject::invalid_params("resource data missing".to_string()),
            )?),
            "Ref" => {
                let object_id = parse_hash(
                    value_obj
                        .get("object_id")
                        .and_then(|x| x.as_str())
                        .ok_or_else(|| {
                            RpcErrorObject::invalid_params("resource object_id missing".to_string())
                        })?,
                )?;
                ResourceValue::Ref(ObjectId(object_id))
            }
            "RefBatch" => {
                let refs = value_obj
                    .get("object_ids")
                    .and_then(|x| x.as_array())
                    .ok_or_else(|| {
                        RpcErrorObject::invalid_params(
                            "resource object_ids must be array".to_string(),
                        )
                    })?
                    .iter()
                    .map(|v| {
                        let s = v.as_str().ok_or_else(|| {
                            RpcErrorObject::invalid_params(
                                "resource object_ids item must be string".to_string(),
                            )
                        })?;
                        Ok(ObjectId(parse_hash(s)?))
                    })
                    .collect::<Result<Vec<_>, RpcErrorObject>>()?;
                ResourceValue::RefBatch(refs)
            }
            other => {
                return Err(RpcErrorObject::invalid_params(format!(
                    "unsupported resource value type: {other}"
                )));
            }
        };
        out.push((asset_id, value));
    }
    out.sort_by_key(|(asset_id, _)| *asset_id);
    for pair in out.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(RpcErrorObject::invalid_params(
                "duplicate asset_id in resources".to_string(),
            ));
        }
    }
    Ok(out)
}

fn parse_metadata(v: Option<&serde_json::Value>) -> Result<Vec<(String, Vec<u8>)>, RpcErrorObject> {
    let Some(v) = v else {
        return Ok(vec![]);
    };
    let arr = v
        .as_array()
        .ok_or_else(|| RpcErrorObject::invalid_params("metadata must be array".to_string()))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let obj = item.as_object().ok_or_else(|| {
            RpcErrorObject::invalid_params("metadata item must be object".to_string())
        })?;
        let key = obj
            .get("key")
            .and_then(|x| x.as_str())
            .ok_or_else(|| RpcErrorObject::invalid_params("metadata key missing".to_string()))?
            .to_string();
        let value = parse_bytes_hex_opt(obj.get("value"))?
            .ok_or_else(|| RpcErrorObject::invalid_params("metadata value missing".to_string()))?;
        out.push((key, value));
    }
    Ok(out)
}

fn parse_bytes_hex_opt(v: Option<&serde_json::Value>) -> Result<Option<Vec<u8>>, RpcErrorObject> {
    match v {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => {
            let bytes = hex::decode(s.strip_prefix("0x").unwrap_or(s))
                .map_err(|e| RpcErrorObject::invalid_params(format!("invalid hex bytes: {e}")))?;
            Ok(Some(bytes))
        }
        _ => Err(RpcErrorObject::invalid_params(
            "expected hex string or null".to_string(),
        )),
    }
}

/// Wire up the canonical-tip listener so that reorgs are logged.
pub fn wire_reorg_notifications(_rpc_api: &Arc<RpcApi>) {
    use rabbitnet::set_canonical_tip_listener;
    use std::sync::Arc;

    set_canonical_tip_listener(Some(Arc::new(move |height| {
        tracing::info!("Reorg detected, new canonical tip height: {}", height);
    })));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer as _;
    use std::ops::{Deref, DerefMut};
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
    use rabbitstore::db::MemDatabase;

    struct LockedTestApi {
        _guard: MutexGuard<'static, ()>,
        api: RpcApi,
    }

    impl Deref for LockedTestApi {
        type Target = RpcApi;

        fn deref(&self) -> &Self::Target {
            &self.api
        }
    }

    impl DerefMut for LockedTestApi {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.api
        }
    }

    fn test_guard() -> MutexGuard<'static, ()> {
        static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        // Recover from a poisoned lock (a previous test panicked while holding it).
        let lock = TEST_LOCK.get_or_init(|| Mutex::new(()));
        match lock.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn build_test_api_with_compute() -> LockedTestApi {
        let guard = test_guard();
        rabbitnet::global_reset_sync_cache();
        let state_db = Arc::new(StateDb::new(Hash::zero()));

        let store = Arc::new(InMemoryObjectStore::new());
        let domains = Arc::new(InMemoryDomainRegistry::new());
        domains.upsert_domain(DomainConfig {
            domain_id: DomainId(0),
            name: "main".to_string(),
            vm: "wasm".to_string(),
            public: true,
        });

        let mut config = RpcConfig::default();
        config.mining_enabled = true;
        let api = RpcApi::with_compute(config, state_db, store, domains);
        // Seed global state with the genesis block so it matches api.latest_block.
        let genesis = Block::new(legacy_rpc_test_root());
        rabbitnet::global_store_block(genesis.clone()).expect("test genesis should store");
        *api.latest_block.write() = Some(genesis);
        LockedTestApi { _guard: guard, api }
    }

    fn build_test_api_with_persistent_compute() -> LockedTestApi {
        let db = Arc::new(MemDatabase::new());
        build_test_api_with_persistent_compute_from_db(db)
    }

    fn build_test_api_with_persistent_compute_from_db(db: Arc<MemDatabase>) -> LockedTestApi {
        let guard = test_guard();
        rabbitnet::global_reset_sync_cache();
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let persistent_store = Arc::new(ComputeStore::new(db));

        let domains = Arc::new(InMemoryDomainRegistry::new());
        domains.upsert_domain(DomainConfig {
            domain_id: DomainId(0),
            name: "main".to_string(),
            vm: "wasm".to_string(),
            public: true,
        });

        let mut config = RpcConfig::default();
        config.mining_enabled = true;
        let api = RpcApi::with_persistent_compute(config, state_db, persistent_store, domains);
        let genesis = Block::new(legacy_rpc_test_root());
        rabbitnet::global_store_block(genesis.clone()).expect("test genesis should store");
        *api.latest_block.write() = Some(genesis);
        LockedTestApi { _guard: guard, api }
    }

    #[test]
    fn test_mining_rpc_disabled_by_default() {
        let _guard = test_guard();
        rabbitnet::global_reset_sync_cache();
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let store = Arc::new(InMemoryObjectStore::new());
        let domains = Arc::new(InMemoryDomainRegistry::new());
        domains.upsert_domain(DomainConfig {
            domain_id: DomainId(0),
            name: "main".to_string(),
            vm: "wasm".to_string(),
            public: true,
        });

        let api = RpcApi::with_compute(RpcConfig::default(), state_db, store, domains);
        let get_work_err = futures::executor::block_on(api.rabbit_get_work(None))
            .expect_err("rabbit_getWork should be disabled by default");
        assert_eq!(get_work_err.code, -32602);
        assert_eq!(
            get_work_err.data,
            Some(serde_json::Value::String(
                "mining rpc disabled on this node".to_string()
            ))
        );

        let submit_work_err = api
            .rabbit_submit_work(Some(vec![serde_json::json!({})]))
            .expect_err("rabbit_submitWork should be disabled by default");
        assert_eq!(submit_work_err.code, -32602);
        assert_eq!(
            submit_work_err.data,
            Some(serde_json::Value::String(
                "mining rpc disabled on this node".to_string()
            ))
        );
    }

    fn canonicalize_compute_tx_id(tx_json: serde_json::Value) -> serde_json::Value {
        canonicalize_compute_tx_json(tx_json).expect("tx json should parse")
    }

    fn canonicalize_and_sign_compute_tx(mut tx_json: serde_json::Value) -> serde_json::Value {
        tx_json = canonicalize_compute_tx_id(tx_json);
        attach_ed25519_signature(tx_json, 7)
    }

    fn ed25519_address_from_seed(seed: u8) -> Address {
        let signer = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let verify = signer.verifying_key();
        let hash = rabbitcore::crypto::keccak256(&verify.to_bytes());
        Address::from_slice(&hash[12..]).expect("address should derive from ed25519 key")
    }

    fn attach_ed25519_signature(mut tx_json: serde_json::Value, seed: u8) -> serde_json::Value {
        let tx =
            parse_compute_tx(tx_json.clone()).expect("tx json should parse after canonicalize");
        let signer = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let verify = signer.verifying_key();
        let sig = signer.sign(&tx.signing_preimage()).to_bytes();
        tx_json["witness"]["signatures"] = serde_json::json!([{
            "scheme": "ed25519",
            "signature": format!("0x{}", hex::encode(sig)),
            "public_key": format!("0x{}", hex::encode(verify.to_bytes()))
        }]);
        tx_json
    }

    fn mine_one_block(api: &RpcApi, nonce: u64, miner: &str) -> serde_json::Value {
        install_easy_pow_head_if_needed(api);
        let work = get_work(&api);
        let work_id = work["work_id"].as_str().unwrap().to_string();
        let prev_hash = work["prev_hash"].as_str().unwrap().to_string();
        let height = work["height"].as_u64().unwrap();
        let target = work_target(&work);
        let prev_hash_bytes =
            hex::decode(prev_hash.strip_prefix("0x").unwrap_or(&prev_hash)).unwrap();
        let (out_nonce, digest) = solve_work_digest(&prev_hash_bytes, height, nonce, target);
        api.rabbit_submit_work(Some(vec![serde_json::json!({
            "work_id": work_id,
            "nonce": out_nonce,
            "hash_hex": format!("0x{}", hex::encode(digest)),
            "miner": miner
        })]))
        .expect("submit work")
    }

    fn solve_work_digest(
        prev_hash_bytes: &[u8],
        height: u64,
        start_nonce: u64,
        target: U256,
    ) -> (u64, [u8; 32]) {
        let mut nonce = start_nonce;
        loop {
            let mut data = Vec::new();
            data.extend_from_slice(prev_hash_bytes);
            data.extend_from_slice(&height.to_be_bytes());
            data.extend_from_slice(&nonce.to_be_bytes());
            let digest = rabbitcore::crypto::keccak256(&data);
            if pow_hash_meets_target(&digest, target) {
                return (nonce, digest);
            }
            nonce = nonce.saturating_add(1);
        }
    }

    fn solve_bad_work_digest(
        prev_hash_bytes: &[u8],
        height: u64,
        start_nonce: u64,
        target: U256,
    ) -> (u64, [u8; 32]) {
        let mut nonce = start_nonce;
        loop {
            let mut data = Vec::new();
            data.extend_from_slice(prev_hash_bytes);
            data.extend_from_slice(&height.to_be_bytes());
            data.extend_from_slice(&nonce.to_be_bytes());
            let digest = rabbitcore::crypto::keccak256(&data);
            if !pow_hash_meets_target(&digest, target) {
                return (nonce, digest);
            }
            nonce = nonce.saturating_add(1);
        }
    }

    fn work_target(work: &serde_json::Value) -> U256 {
        let target = work["target"].as_str().expect("work target missing");
        pow_target_from_hex(target).expect("work target should decode")
    }

    fn set_work_target(api: &RpcApi, work_id: &str, target: U256) {
        let mut jobs = api.mining_jobs.write();
        jobs.get_mut(work_id).expect("work id should exist").target = target;
    }

    fn get_work(api: &RpcApi) -> serde_json::Value {
        futures::executor::block_on(api.rabbit_get_work(None)).expect("work should be available")
    }

    fn install_easy_pow_head_if_needed(api: &RpcApi) {
        // Check if the current chain already has at least one block (any block).
        // If the head block exists with the correct minimum difficulty, we're fine.
        // This avoids resetting the cache mid-test when mining multiple blocks.
        let current = api.current_head_block();
        if current.header.number > U256::zero() {
            return;  // Chain already progressing — don't touch it.
        }
        if current.header.difficulty >= U256::from_u128(MIN_MINING_DIFFICULTY) {
            return;  // Genesis with good difficulty already installed.
        }
        // Fall through: reset and install a proper genesis.
        rabbitnet::global_reset_sync_cache();
        let mut header = legacy_rpc_test_root();
        header.hash = header.compute_hash();
        let block = Block::new(header);
        rabbitnet::global_store_block(block.clone()).expect("easy genesis should store");
        *api.latest_block.write() = Some(block);
    }

    fn legacy_rpc_test_root() -> BlockHeader {
        BlockHeader {
            version: 1,
            parent_hash: Hash::zero(),
            uncle_hashes: Vec::new(),
            coinbase: Address::zero(),
            state_root: Hash::zero(),
            transactions_root: Hash::zero(),
            receipts_root: Hash::zero(),
            number: U256::zero(),
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp: 0,
            difficulty: U256::from_u128(MIN_MINING_DIFFICULTY),
            nonce: 0,
            extra_data: Vec::new(),
            mix_hash: Hash::zero(),
            base_fee_per_gas: U256::from(1_000_000_000u64),
            hash: Hash::zero(),
        }
    }

    fn make_rpc_test_block(number: u64, parent: &BlockHeader) -> Block {
        let timestamp = parent.timestamp.saturating_add(30);
        make_rpc_test_block_at_time(number, parent, timestamp)
    }

    fn make_rpc_test_block_at_time(number: u64, parent: &BlockHeader, timestamp: u64) -> Block {
        let difficulty = adjust_mining_difficulty(parent.difficulty, parent.timestamp, timestamp);
        // Resolve the parent hash (handle root headers with hash=zero).
        let parent_hash = if parent.hash.is_zero() {
            parent.canonical_hash()
        } else {
            parent.hash
        };
        let mut nonce = 0u64;
        let mix_hash = loop {
            // Use the canonical compute_pow_hash, not a simplified standalone digest.
            // This ensures the PoW binds the full header content (including state_root,
            // transactions_root, receipts_root, extra_data, etc.) via the
            // RABBIT-POW-V1 domain-separated preimage.
            let pow_hash = rabbitcore::block::compute_pow_hash(
                &BlockHeader {
                    version: POW_TARGET_HEADER_VERSION,
                    parent_hash,
                    uncle_hashes: Vec::new(),
                    coinbase: Address::zero(),
                    state_root: Hash::zero(),
                    transactions_root: Hash::zero(),
                    receipts_root: Hash::zero(),
                    number: U256::from(number),
                    gas_limit: 30_000_000,
                    gas_used: 0,
                    timestamp,
                    difficulty,
                    nonce,
                    extra_data: format!("rpc-head-test-{number}").into_bytes(),
                    mix_hash: Hash::zero(),
                    base_fee_per_gas: U256::from(1_000_000_000u64),
                    hash: Hash::zero(),
                },
                nonce,
            );
            if pow_hash_meets_target(pow_hash.as_bytes(), pow_target_from_difficulty(difficulty)) {
                break pow_hash;
            }
            nonce = nonce.saturating_add(1);
        };
        let mut header = BlockHeader {
            version: POW_TARGET_HEADER_VERSION,
            parent_hash,
            uncle_hashes: Vec::new(),
            coinbase: Address::zero(),
            state_root: Hash::zero(),
            transactions_root: Hash::zero(),
            receipts_root: Hash::zero(),
            number: U256::from(number),
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp,
            difficulty,
            nonce,
            extra_data: format!("rpc-head-test-{number}").into_bytes(),
            mix_hash,
            base_fee_per_gas: U256::from(1_000_000_000u64),
            hash: Hash::zero(),
        };
        header.hash = header.compute_hash();
        Block::new(header)
    }

    #[test]
    fn test_parse_compute_tx_accepts_ed25519_witness_and_owner() {
        let signer = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let verify = signer.verifying_key();
        let owner_pub_hex = format!("0x{}", hex::encode(verify.to_bytes()));

        let mut tx = serde_json::json!({
            "tx_id": format!("0x{}", hex::encode([0x91u8; 32])),
            "domain_id": 0,
            "chain_id": 10086,
            "network_id": 10086,
            "command": "Transfer",
            "nonce": 1,
            "input_set": [format!("0x{}", hex::encode([0x92u8; 32]))],
            "read_set": [],
            "output_proposals": [{
                "output_id": format!("0x{}", hex::encode([0x93u8; 32])),
                "object_id": format!("0x{}", hex::encode([0x94u8; 32])),
                "domain_id": 0,
                "kind": "Asset",
                "owner": { "type": "Ed25519", "public_key": owner_pub_hex },
                "predecessor": format!("0x{}", hex::encode([0x92u8; 32])),
                "version": 2,
                "state": "0x01",
                "logic": null
            }],
            "payload": "0x1234",
            "deadline_unix_secs": 1900000000u64,
            "witness": {"signatures": [], "threshold": 1}
        });

        let parsed = parse_compute_tx(tx.clone()).expect("tx should parse");
        let sig = signer.sign(&parsed.signing_preimage());
        tx["witness"]["signatures"] = serde_json::json!([{
            "scheme": "ed25519",
            "signature": format!("0x{}", hex::encode(sig.to_bytes())),
            "public_key": format!("0x{}", hex::encode(verify.to_bytes()))
        }]);

        let parsed = parse_compute_tx(tx).expect("ed25519 tx should parse");
        assert_eq!(parsed.witness.signatures.len(), 1);
        assert_eq!(
            parsed.witness.signatures[0].scheme,
            SignatureScheme::Ed25519
        );
    }

    #[test]
    fn compute_json_fixture_address_owner_mint_matches_protocol() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../fixtures/compute_json/address_owner_mint.json"
        ))
        .expect("fixture should parse");
        let input = fixture["input"].clone();
        let expected = fixture["expected"].clone();

        let canonical =
            canonicalize_compute_tx_json(input.clone()).expect("tx should canonicalize");
        assert_eq!(
            canonical["tx_id"].as_str(),
            expected["canonical_tx_id"].as_str()
        );

        let parsed = parse_compute_tx(canonical).expect("canonical tx should parse");
        assert!(matches!(
            parsed.output_proposals[0].owner,
            Ownership::Address(_)
        ));
        assert_eq!(
            parsed.output_proposals[0]
                .resources
                .iter()
                .map(|(asset_id, _)| format!("0x{}", hex::encode(asset_id.as_bytes())))
                .collect::<Vec<_>>(),
            expected["resource_asset_ids_sorted"]
                .as_array()
                .expect("sorted asset ids")
                .iter()
                .map(|value| value.as_str().expect("asset id").to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn compute_json_fixture_ed25519_owner_mint_matches_protocol() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../fixtures/compute_json/ed25519_owner_mint.json"
        ))
        .expect("fixture should parse");
        let input = fixture["input"].clone();
        let expected = fixture["expected"].clone();

        let canonical = canonicalize_compute_tx_json(input).expect("tx should canonicalize");
        assert_eq!(
            canonical["tx_id"].as_str(),
            expected["canonical_tx_id"].as_str()
        );

        let parsed = parse_compute_tx(canonical).expect("canonical tx should parse");
        assert!(matches!(
            parsed.output_proposals[0].owner,
            Ownership::Ed25519(_)
        ));
    }

    #[test]
    fn test_rabbit_get_work_returns_work_payload() {
        let api = build_test_api_with_persistent_compute();
        let work = futures::executor::block_on(api.rabbit_get_work(None))
            .expect("rabbit_getWork should succeed");
        assert!(work.get("work_id").and_then(|v| v.as_str()).is_some());
        assert!(work.get("height").and_then(|v| v.as_u64()).is_some());
        assert_eq!(work.get("version").and_then(|v| v.as_u64()), Some(2));
        assert!(work.get("target").and_then(|v| v.as_str()).is_some());
        assert!(work.get("difficulty").and_then(|v| v.as_str()).is_some());
        // With MIN_MINING_DIFFICULTY=1 the target is U256::MAX → 0 leading zero bytes.
        assert_eq!(
            work.get("target_leading_rabbit_bytes")
                .and_then(|v| v.as_u64()),
            Some(0)
        );
        // The work target (from difficulty) is much easier than the 2-byte override.
        assert!(work_target(&work) > legacy_target_from_leading_rabbit_bytes(2));
    }

    #[test]
    fn test_rabbit_get_block_body_returns_sidecar_payload() {
        let api = build_test_api_with_compute();
        let block = make_rpc_test_block(1, &legacy_rpc_test_root());
        let tx = ComputeTx {
            tx_id: rabbitcore::compute::TxId(Hash::from_bytes([0x11; 32])),
            domain_id: DomainId(0),
            command: Command::Mint,
            input_set: Vec::new(),
            read_set: Vec::new(),
            output_proposals: Vec::new(),
            fee: 0,
            nonce: None,
            metadata: Vec::new(),
            payload: Vec::new(),
            deadline_unix_secs: None,
            chain_id: Some(api.config.chain_id),
            network_id: Some(api.config.network_id as u32),
            witness: TxWitness {
                signatures: Vec::new(),
                threshold: None,
            },
                        max_fee: 0,
                        priority_fee: 0,
                        gas_limit: 0,
        };
        // Store body as a sidecar record (bypass full block validation since
        // the test's make_rpc_test_block_at_time doesn't pre-compute body roots).
        let body = rabbitcore::block::BlockBody::new(
            vec![tx.clone()],
            vec![rabbitcore::block::Receipt::success(
                tx.tx_id,
                block.header.hash,
                21_000,
                1,
                Vec::new(),
            )],
        );
        let record = rabbitcore::block::BlockBodyRecord::new(
            block.header.number.as_u64(),
            block.header.hash,
            body,
        );
        rabbitnet::global_store_block_body(record).expect("store body sidecar");

        let body_json = api
            .rabbit_get_block_body(Some(vec![serde_json::json!(format!(
                "0x{}",
                hex::encode(block.header.hash.as_bytes())
            ))]))
            .expect("body query should succeed");

        let expected_block_hash = format!("0x{}", hex::encode(block.header.hash.as_bytes()));
        assert_eq!(
            body_json.get("block_hash").and_then(|v| v.as_str()),
            Some(expected_block_hash.as_str())
        );
        assert_eq!(body_json.get("tx_count").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(
            body_json.get("receipt_count").and_then(|v| v.as_u64()),
            Some(1)
        );
        let expected_tx_id = format!("0x{}", hex::encode(tx.tx_id.0.as_bytes()));
        assert_eq!(
            body_json
                .get("transactions")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.get("tx_id"))
                .and_then(|v| v.as_str()),
            Some(expected_tx_id.as_str())
        );
    }

    #[test]
    fn test_rabbit_get_block_by_number_can_include_body() {
        let api = build_test_api_with_compute();
        let block = make_rpc_test_block(1, &legacy_rpc_test_root());
        let tx = ComputeTx {
            tx_id: rabbitcore::compute::TxId(Hash::from_bytes([0x22; 32])),
            domain_id: DomainId(0),
            command: Command::Mint,
            input_set: Vec::new(),
            read_set: Vec::new(),
            output_proposals: Vec::new(),
            fee: 0,
            nonce: None,
            metadata: Vec::new(),
            payload: Vec::new(),
            deadline_unix_secs: None,
            chain_id: Some(api.config.chain_id),
            network_id: Some(api.config.network_id as u32),
            witness: TxWitness {
                signatures: Vec::new(),
                threshold: None,
            },
                        max_fee: 0,
                        priority_fee: 0,
                        gas_limit: 0,
        };
        let receipt = rabbitcore::block::Receipt::success(
            tx.tx_id,
            block.header.hash,
            21_000,
            1,
            vec![OutputId(Hash::from_bytes([0x33; 32]))],
        );
        let body = BlockBody::new(vec![tx], vec![receipt]);
        // Reconcile header roots and recompute the PoW mix_hash for the final header.
        let mut reconciled = block.header.clone();
        reconciled.apply_body_commitments(&body);
        reconciled.hash = reconciled.compute_hash();
        // Update receipt block_hash to match the final hash.
        let mut fixed_body = body.clone();
        for r in fixed_body.receipts.iter_mut() {
            r.block_hash = reconciled.hash;
        }
        // Recompute receipts_root after fixing block_hash, then recompute mix_hash.
        reconciled.apply_body_commitments(&fixed_body);
        reconciled.mix_hash = rabbitcore::block::compute_pow_hash(&reconciled, reconciled.nonce);
        reconciled.hash = reconciled.compute_hash();
        // Fix receipt block_hash once more for the final hash.
        for r in fixed_body.receipts.iter_mut() {
            r.block_hash = reconciled.hash;
        }
        let expected_hash = reconciled.hash;
        let stored_block = Block {
            header: reconciled,
            body: None,
            uncles: Vec::new(),
        };
        rabbitnet::global_store_block_with_body(stored_block, fixed_body)
            .expect("store body sidecar");

        let plain = api
            .rabbit_get_block_by_number(Some(vec![serde_json::json!("0x1")]))
            .expect("block query should succeed");
        assert_eq!(
            plain
                .get("body")
                .and_then(|v| v.get("tx_count"))
                .and_then(|v| v.as_u64()),
            Some(1)
        );

        let rich = api
            .rabbit_get_block_by_number(Some(vec![
                serde_json::json!("0x1"),
                serde_json::json!(true),
            ]))
            .expect("block query should include body");
        assert_eq!(
            rich.get("hash").and_then(|v| v.as_str()),
            Some(format!("0x{}", hex::encode(expected_hash.as_bytes())).as_str())
        );
        assert_eq!(
            rich.get("body")
                .and_then(|v| v.get("tx_count"))
                .and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            rich.get("body")
                .and_then(|v| v.get("receipt_count"))
                .and_then(|v| v.as_u64()),
            Some(1)
        );
    }

    #[test]
    fn test_rabbit_get_receipt_and_block_receipts_return_sidecar_payload() {
        let api = build_test_api_with_compute();
        let block = make_rpc_test_block(1, &legacy_rpc_test_root());
        let tx = ComputeTx {
            tx_id: rabbitcore::compute::TxId(Hash::from_bytes([0x44; 32])),
            domain_id: DomainId(0),
            command: Command::Mint,
            input_set: Vec::new(),
            read_set: Vec::new(),
            output_proposals: Vec::new(),
            fee: 0,
            nonce: None,
            metadata: Vec::new(),
            payload: Vec::new(),
            deadline_unix_secs: None,
            chain_id: Some(api.config.chain_id),
            network_id: Some(api.config.network_id as u32),
            witness: TxWitness {
                signatures: Vec::new(),
                threshold: None,
            },
                        max_fee: 0,
                        priority_fee: 0,
                        gas_limit: 0,
        };
        let receipt = rabbitcore::block::Receipt::success(
            tx.tx_id,
            block.header.hash,
            42_000,
            7,
            vec![OutputId(Hash::from_bytes([0x55; 32]))],
        );
        let body = BlockBody::new(vec![tx.clone()], vec![receipt.clone()]);
        // Reconcile header roots, recompute mix_hash, and fix receipt block_hash.
        let mut reconciled = block.header.clone();
        reconciled.apply_body_commitments(&body);
        let mut fixed_body = body.clone();
        for r in fixed_body.receipts.iter_mut() {
            r.block_hash = reconciled.hash;
        }
        reconciled.apply_body_commitments(&fixed_body);
        reconciled.mix_hash = rabbitcore::block::compute_pow_hash(&reconciled, reconciled.nonce);
        reconciled.hash = reconciled.compute_hash();
        for r in fixed_body.receipts.iter_mut() {
            r.block_hash = reconciled.hash;
        }
        let expected_hash = reconciled.hash;
        let stored_block = Block {
            header: reconciled,
            body: None,
            uncles: Vec::new(),
        };
        rabbitnet::global_store_block_with_body(stored_block, fixed_body)
            .expect("store body sidecar");

        let tx_id_hex = format!("0x{}", hex::encode(tx.tx_id.0.as_bytes()));
        let receipt_json = api
            .rabbit_get_receipt(Some(vec![serde_json::json!(tx_id_hex.clone())]))
            .expect("receipt query should succeed");
        assert_eq!(
            receipt_json.get("tx_id").and_then(|v| v.as_str()),
            Some(tx_id_hex.as_str())
        );
        assert_eq!(
            receipt_json.get("block_hash").and_then(|v| v.as_str()),
            Some(format!("0x{}", hex::encode(expected_hash.as_bytes())).as_str())
        );

        let receipts_json = api
            .rabbit_get_block_receipts(Some(vec![serde_json::json!(format!(
                "0x{}",
                hex::encode(expected_hash.as_bytes())
            ))]))
            .expect("block receipts query should succeed");
        assert_eq!(receipts_json.as_array().map(|arr| arr.len()), Some(1));
        assert_eq!(
            receipts_json
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|v| v.get("tx_id"))
                .and_then(|v| v.as_str()),
            Some(tx_id_hex.as_str())
        );
    }

    #[test]
    fn test_rabbit_get_work_rotates_configured_coinbases() {
        let mut api = build_test_api_with_persistent_compute();
        api.config.coinbase = "0x1111111111111111111111111111111111111111".to_string();
        api.config.coinbase_addresses = vec![
            "0x2222222222222222222222222222222222222222".to_string(),
            "0x3333333333333333333333333333333333333333".to_string(),
        ];

        let first = get_work(&api);
        let second = get_work(&api);

        assert_eq!(
            first.get("coinbase").and_then(|v| v.as_str()),
            Some("0x2222222222222222222222222222222222222222")
        );
        assert_eq!(
            second.get("coinbase").and_then(|v| v.as_str()),
            Some("0x3333333333333333333333333333333333333333")
        );
    }

    #[test]
    fn test_rabbit_get_work_uses_higher_p2p_synced_head() {
        let api = build_test_api_with_persistent_compute();
        install_easy_pow_head_if_needed(&api);
        let mut parent = api.current_head_block().header;
        let mut synced_hash = Hash::zero();
        for number in 1..=7 {
            let block = make_rpc_test_block(number, &parent);
            synced_hash = block.header.hash;
            parent = block.header.clone();
            rabbitnet::global_store_block(block).expect("store synced head");
        }

        let work = get_work(&api);

        let expected_prev_hash = format!("0x{}", hex::encode(synced_hash.as_bytes()));
        assert_eq!(work["height"].as_u64(), Some(8));
        assert_eq!(
            work["prev_hash"].as_str(),
            Some(expected_prev_hash.as_str())
        );
    }

    #[test]
    fn test_rabbit_submit_work_accepts_valid_share() {
        let api = build_test_api_with_persistent_compute();
        install_easy_pow_head_if_needed(&api);
        let work = get_work(&api);
        let work_id = work
            .get("work_id")
            .and_then(|v| v.as_str())
            .expect("work_id missing")
            .to_string();
        let prev_hash = work
            .get("prev_hash")
            .and_then(|v| v.as_str())
            .expect("prev_hash missing")
            .to_string();
        let height = work
            .get("height")
            .and_then(|v| v.as_u64())
            .expect("height missing");
        let target = work_target(&work);

        let prev_hash_bytes =
            hex::decode(prev_hash.strip_prefix("0x").unwrap_or(&prev_hash)).expect("prev hash hex");
        let (nonce, digest) = solve_work_digest(&prev_hash_bytes, height, 42, target);
        let hash_hex = format!("0x{}", hex::encode(digest));
        let submit = api
            .rabbit_submit_work(Some(vec![serde_json::json!({
                "work_id": work_id,
                "nonce": nonce,
                "hash_hex": hash_hex,
                "miner": "test-miner"
            })]))
            .expect("submit should succeed");

        assert_eq!(submit.get("accepted").and_then(|v| v.as_bool()), Some(true));
        assert!(submit.get("block_hash").and_then(|v| v.as_str()).is_some());
    }

    #[test]
    fn test_rabbit_submit_work_credits_coinbase_balance() {
        let api = build_test_api_with_persistent_compute();
        install_easy_pow_head_if_needed(&api);
        let work = get_work(&api);
        let work_id = work
            .get("work_id")
            .and_then(|v| v.as_str())
            .expect("work_id missing")
            .to_string();
        let prev_hash = work
            .get("prev_hash")
            .and_then(|v| v.as_str())
            .expect("prev_hash missing")
            .to_string();
        let height = work
            .get("height")
            .and_then(|v| v.as_u64())
            .expect("height missing");
        let target = work_target(&work);

        let prev_hash_bytes =
            hex::decode(prev_hash.strip_prefix("0x").unwrap_or(&prev_hash)).expect("prev hash hex");
        let (nonce, digest) = solve_work_digest(&prev_hash_bytes, height, 123, target);
        let hash_hex = format!("0x{}", hex::encode(digest));

        let submit = api
            .rabbit_submit_work(Some(vec![serde_json::json!({
                "work_id": work_id,
                "nonce": nonce,
                "hash_hex": hash_hex,
                "miner": "reward-test-miner"
            })]))
            .expect("submit should succeed");
        assert_eq!(submit.get("accepted").and_then(|v| v.as_bool()), Some(true));

        let coinbase = api.config.coinbase.clone();
        let account = api
            .rabbit_get_account(Some(vec![serde_json::json!(coinbase)]))
            .expect("coinbase account");
        let balance = account
            .get("balance")
            .and_then(|v| v.as_str())
            .expect("balance string");
        let expected = format!("0x{:x}", rabbitcore::INITIAL_BLOCK_REWARD);

        assert_eq!(balance, expected);
    }

    #[tokio::test]
    async fn test_fee_txs_are_packed_into_mined_block_body() {
        let api = build_test_api_with_persistent_compute();
        install_easy_pow_head_if_needed(&api);

        // Submit a fee-less mint tx (legacy path, SubmitTime mode executes it
        // immediately and places it in the fee-priority pool).
        let tx = canonicalize_and_sign_compute_tx(serde_json::json!({
            "tx_id": format!("0x{}", hex::encode([0x51u8; 32])),
            "domain_id": 0,
            "chain_id": 10086,
            "network_id": 10086,
            "command": "Mint",
            "nonce": 1,
            "input_set": [],
            "read_set": [],
            "output_proposals": [{
                "output_id": format!("0x{}", hex::encode([0x52u8; 32])),
                "object_id": format!("0x{}", hex::encode([0x53u8; 32])),
                "domain_id": 0,
                "kind": "State",
                "owner": { "type": "Shared" },
                "predecessor": null,
                "version": 1,
                "state": "0x01",
                "logic": null
            }],
            "payload": "0x",
            "deadline_unix_secs": null,
            "witness": {"signatures": [], "threshold": 1}
        }));
        let submitted = api
            .rabbit_submit_compute_tx(Some(vec![tx]))
            .await
            .expect("fee tx should submit");
        assert_eq!(submitted.get("ok").and_then(|v| v.as_bool()), Some(true));

        // Pool should report 1 pending tx.
        let pending = api.rabbit_pending_transactions(None).expect("pending list");
        assert_eq!(pending.get("total").and_then(|v| v.as_u64()), Some(1));

        // Mine a block; the tx must be packed into the block body.
        let work = get_work(&api);
        let work_id = work.get("work_id").and_then(|v| v.as_str()).unwrap().to_string();
        let prev_hash = work.get("prev_hash").and_then(|v| v.as_str()).unwrap().to_string();
        let height = work.get("height").and_then(|v| v.as_u64()).unwrap();
        let target = work_target(&work);
        let prev_hash_bytes = hex::decode(prev_hash.strip_prefix("0x").unwrap_or(&prev_hash)).unwrap();
        let (nonce, digest) = solve_work_digest(&prev_hash_bytes, height, 55, target);
        let submit = api
            .rabbit_submit_work(Some(vec![serde_json::json!({
                "work_id": work_id,
                "nonce": nonce,
                "hash_hex": format!("0x{}", hex::encode(digest)),
                "miner": "pack-test-miner"
            })]))
            .expect("submit should succeed");
        assert_eq!(submit.get("accepted").and_then(|v| v.as_bool()), Some(true));

        // Pool drained after packing.
        let pending = api.rabbit_pending_transactions(None).expect("pending list");
        assert_eq!(pending.get("total").and_then(|v| v.as_u64()), Some(0));

        // The mined block body must contain the packed tx, and its receipt must
        // reference the block's final hash (annotation back-fill).
        let block_hash = submit.get("block_hash").and_then(|v| v.as_str()).unwrap().to_string();
        let block_json = api
            .rabbit_get_block_by_number(Some(vec![serde_json::json!("0x1")]))
            .expect("block query");
        assert_eq!(
            block_json.get("hash").and_then(|v| v.as_str()),
            Some(block_hash.as_str())
        );
        let body = block_json.get("body").cloned().unwrap_or(serde_json::json!({}));
        assert_eq!(body.get("tx_count").and_then(|v| v.as_u64()), Some(1));
        let receipts = body
            .get("receipts")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(receipts.len(), 1);
        assert_eq!(
            receipts[0].get("block_hash").and_then(|v| v.as_str()),
            Some(block_hash.as_str())
        );
    }

    #[test]
    fn test_rabbit_submit_work_uses_bound_work_coinbase() {
        let mut api = build_test_api_with_persistent_compute();
        install_easy_pow_head_if_needed(&api);
        api.config.coinbase = "0x1111111111111111111111111111111111111111".to_string();
        api.config.coinbase_addresses =
            vec!["0x2222222222222222222222222222222222222222".to_string()];

        let work = get_work(&api);
        assert_eq!(
            work.get("coinbase").and_then(|v| v.as_str()),
            Some("0x2222222222222222222222222222222222222222")
        );
        let work_id = work
            .get("work_id")
            .and_then(|v| v.as_str())
            .expect("work_id missing")
            .to_string();
        let prev_hash = work
            .get("prev_hash")
            .and_then(|v| v.as_str())
            .expect("prev_hash missing")
            .to_string();
        let height = work
            .get("height")
            .and_then(|v| v.as_u64())
            .expect("height missing");
        let target = work_target(&work);
        let prev_hash_bytes =
            hex::decode(prev_hash.strip_prefix("0x").unwrap_or(&prev_hash)).expect("prev hash hex");
        let (nonce, digest) = solve_work_digest(&prev_hash_bytes, height, 321, target);
        let submit = api
            .rabbit_submit_work(Some(vec![serde_json::json!({
                "work_id": work_id,
                "nonce": nonce,
                "hash_hex": format!("0x{}", hex::encode(digest)),
                "miner": "bound-coinbase-test"
            })]))
            .expect("submit should succeed");
        assert_eq!(submit.get("accepted").and_then(|v| v.as_bool()), Some(true));

        let latest = api.rabbit_get_latest_block(None).expect("latest block");
        assert_eq!(
            latest.get("coinbase").and_then(|v| v.as_str()),
            Some("0x2222222222222222222222222222222222222222")
        );
    }

    #[test]
    fn test_rabbit_submit_work_rejects_low_difficulty_share() {
        let api = build_test_api_with_persistent_compute();
        let work = get_work(&api);
        let work_id = work
            .get("work_id")
            .and_then(|v| v.as_str())
            .expect("work_id missing");
        let prev_hash = work
            .get("prev_hash")
            .and_then(|v| v.as_str())
            .expect("prev_hash missing")
            .to_string();
        let height = work
            .get("height")
            .and_then(|v| v.as_u64())
            .expect("height missing");
        set_work_target(&api, work_id, U256::zero());
        let prev_hash_bytes =
            hex::decode(prev_hash.strip_prefix("0x").unwrap_or(&prev_hash)).expect("prev hash hex");
        let (nonce, digest) = solve_bad_work_digest(&prev_hash_bytes, height, 1, U256::zero());

        let submit = api
            .rabbit_submit_work(Some(vec![serde_json::json!({
                "work_id": work_id,
                "nonce": nonce,
                "hash_hex": format!("0x{}", hex::encode(digest)),
                "miner": "test-miner"
            })]))
            .expect("submit should return result");

        assert_eq!(
            submit.get("accepted").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            submit.get("reason").and_then(|v| v.as_str()),
            Some("low_difficulty_share")
        );
    }

    #[test]
    fn test_rabbit_import_block_updates_latest_block() {
        let api = build_test_api_with_persistent_compute();
        install_easy_pow_head_if_needed(&api);

        let first = futures::executor::block_on(api.rabbit_get_work(None)).expect("get work");
        let work_id = first["work_id"].as_str().unwrap().to_string();
        let prev_hash = first["prev_hash"].as_str().unwrap().to_string();
        let height = first["height"].as_u64().unwrap();
        let target = work_target(&first);

        let prev_hash_bytes =
            hex::decode(prev_hash.strip_prefix("0x").unwrap_or(&prev_hash)).unwrap();
        let (nonce, digest) = solve_work_digest(&prev_hash_bytes, height, 7, target);

        let mined = api
            .rabbit_submit_work(Some(vec![serde_json::json!({
                "work_id": work_id,
                "nonce": nonce,
                "hash_hex": format!("0x{}", hex::encode(digest)),
                "miner": "test-miner"
            })]))
            .expect("submit work");
        assert_eq!(mined["accepted"].as_bool(), Some(true));

        let latest = api.rabbit_get_latest_block(None).expect("latest block");
        assert!(latest.get("hash").is_some());
        assert_eq!(latest.get("number").and_then(|v| v.as_str()), Some("0x1"));
    }

    #[test]
    fn test_rabbit_import_block_rejects_legacy_transactions() {
        let api = build_test_api_with_persistent_compute();
        let latest = api.rabbit_get_latest_block(None).expect("latest block");
        let parent_hash = latest
            .get("hash")
            .and_then(|v| v.as_str())
            .expect("parent hash");

        let err = api
            .rabbit_import_block(Some(vec![serde_json::json!({
                "hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "parent_hash": parent_hash,
                "number": "0x1",
                "timestamp": 1,
                "difficulty": "0x1",
                "nonce": 1,
                "coinbase": "0x526Dc404e751C7d52F6fFF75d563d8D0857C94E9",
                "mix_hash": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "extra_data": "0x",
                "transactions": [{"legacy": true}]
            })]))
            .expect_err("legacy transactions should be rejected");
        assert_eq!(err.code, -32602);
        assert_eq!(
            err.data,
            Some(serde_json::Value::String(
                "legacy block transactions are not supported".to_string(),
            ))
        );
    }

    #[test]
    fn test_rabbit_get_latest_block_defaults_to_genesis() {
        let api = build_test_api_with_persistent_compute();

        let latest = api.rabbit_get_latest_block(None).expect("latest block");
        assert_eq!(latest.get("number").and_then(|v| v.as_str()), Some("0x0"));
        assert!(latest.get("hash").and_then(|v| v.as_str()).is_some());
    }

    #[test]
    fn test_rabbit_sync_status_reports_gap_without_synthesizing_block() {
        let api = build_test_api_with_persistent_compute();
        set_global_synced_height(12);

        let status = api
            .rabbit_sync_status(None)
            .expect("sync status should succeed");
        let local_head = status
            .get("local_head")
            .and_then(|v| v.as_u64())
            .expect("local_head");
        let network_head = status
            .get("network_head")
            .and_then(|v| v.as_u64())
            .expect("network_head");
        let syncing = status
            .get("syncing")
            .and_then(|v| v.as_bool())
            .expect("syncing");
        assert_eq!(syncing, network_head > local_head);
        assert!(local_head <= network_head);

        let latest = api.rabbit_get_latest_block(None).expect("latest block");
        let latest_number = latest
            .get("number")
            .and_then(|v| v.as_str())
            .expect("latest.number");
        assert_eq!(latest_number, format!("0x{local_head:x}"));
        set_global_synced_height(0);
    }

    #[test]
    fn test_rabbit_get_blocks_range_rejects_non_object_params() {
        let api = build_test_api_with_persistent_compute();
        let err = api
            .rabbit_get_blocks_range(Some(vec![serde_json::json!("bad-query")]))
            .expect_err("non-object query should be rejected");
        assert_eq!(err.code, -32602);
        assert_eq!(err.message, "Invalid params");
        assert_eq!(
            err.data,
            Some(serde_json::Value::String(
                "query object required for rabbit_getBlocksRange".to_string(),
            ))
        );
    }

    #[test]
    fn test_rabbit_get_block_by_number_and_range() {
        let api = build_test_api_with_persistent_compute();

        assert_eq!(
            mine_one_block(&api, 101, "range-miner")["accepted"].as_bool(),
            Some(true)
        );
        assert_eq!(
            mine_one_block(&api, 102, "range-miner")["accepted"].as_bool(),
            Some(true)
        );
        assert_eq!(
            mine_one_block(&api, 103, "range-miner")["accepted"].as_bool(),
            Some(true)
        );

        let block_2 = api
            .rabbit_get_block_by_number(Some(vec![serde_json::json!("0x2")]))
            .expect("block by number should succeed");
        assert_eq!(block_2.get("number").and_then(|v| v.as_str()), Some("0x2"));

        let range = api
            .rabbit_get_blocks_range(Some(vec![serde_json::json!({
                "from": "0x1",
                "to": "0x3",
                "limit": 10
            })]))
            .expect("range should succeed");
        let items = range
            .get("items")
            .and_then(|v| v.as_array())
            .expect("items missing");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].get("number").and_then(|v| v.as_str()), Some("0x3"));
        assert_eq!(items[1].get("number").and_then(|v| v.as_str()), Some("0x2"));
        assert_eq!(items[2].get("number").and_then(|v| v.as_str()), Some("0x1"));
    }

    #[test]
    fn test_rabbit_get_block_by_number_requires_param() {
        let api = build_test_api_with_persistent_compute();
        let err = api
            .rabbit_get_block_by_number(None)
            .expect_err("missing number should fail");
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn test_rabbit_get_block_by_number_returns_null_for_missing_height() {
        let api = build_test_api_with_persistent_compute();
        let block = api
            .rabbit_get_block_by_number(Some(vec![serde_json::json!("0x9")]))
            .expect("request should succeed");
        assert!(block.is_null());
    }

    #[test]
    fn test_rabbit_get_block_by_number_accepts_numeric_param() {
        let api = build_test_api_with_persistent_compute();

        assert_eq!(
            mine_one_block(&api, 301, "numeric-block-miner")["accepted"].as_bool(),
            Some(true)
        );
        assert_eq!(
            mine_one_block(&api, 302, "numeric-block-miner")["accepted"].as_bool(),
            Some(true)
        );

        let block = api
            .rabbit_get_block_by_number(Some(vec![serde_json::json!(2)]))
            .expect("numeric block query should succeed");
        assert_eq!(block.get("number").and_then(|v| v.as_str()), Some("0x2"));
    }

    #[test]
    fn test_rabbit_get_blocks_range_clamps_inverted_window_to_single_height() {
        let api = build_test_api_with_persistent_compute();

        assert_eq!(
            mine_one_block(&api, 401, "clamp-miner")["accepted"].as_bool(),
            Some(true)
        );
        assert_eq!(
            mine_one_block(&api, 402, "clamp-miner")["accepted"].as_bool(),
            Some(true)
        );
        assert_eq!(
            mine_one_block(&api, 403, "clamp-miner")["accepted"].as_bool(),
            Some(true)
        );

        let range = api
            .rabbit_get_blocks_range(Some(vec![serde_json::json!({
                "from": 9,
                "to": 2,
                "limit": 5
            })]))
            .expect("range should clamp successfully");
        assert_eq!(range.get("from").and_then(|v| v.as_u64()), Some(2));
        assert_eq!(range.get("to").and_then(|v| v.as_u64()), Some(2));
        let items = range
            .get("items")
            .and_then(|v| v.as_array())
            .expect("items missing");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].get("number").and_then(|v| v.as_str()), Some("0x2"));
    }

    #[test]
    fn test_rabbit_get_blocks_range_rejects_invalid_hex_bounds() {
        let api = build_test_api_with_persistent_compute();
        let err = api
            .rabbit_get_blocks_range(Some(vec![serde_json::json!({ "from": "0xzz" })]))
            .expect_err("invalid hex bounds should fail");
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn test_rabbit_get_blocks_range_defaults_to_latest_and_clamps_limit() {
        let api = build_test_api_with_persistent_compute();
        assert_eq!(
            mine_one_block(&api, 201, "window-miner")["accepted"].as_bool(),
            Some(true)
        );
        assert_eq!(
            mine_one_block(&api, 202, "window-miner")["accepted"].as_bool(),
            Some(true)
        );
        assert_eq!(
            mine_one_block(&api, 203, "window-miner")["accepted"].as_bool(),
            Some(true)
        );

        let range = api
            .rabbit_get_blocks_range(Some(vec![serde_json::json!({ "limit": 9_999 })]))
            .expect("range should succeed");
        assert_eq!(range.get("limit").and_then(|v| v.as_u64()), Some(500));
        assert_eq!(range.get("to").and_then(|v| v.as_u64()), Some(3));
        assert_eq!(range.get("from").and_then(|v| v.as_u64()), Some(1));
        let items = range
            .get("items")
            .and_then(|v| v.as_array())
            .expect("items missing");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].get("number").and_then(|v| v.as_str()), Some("0x3"));
        assert_eq!(items[2].get("number").and_then(|v| v.as_str()), Some("0x1"));
    }

    #[test]
    fn test_rabbit_get_blocks_range_rejects_non_object_query() {
        let api = build_test_api_with_persistent_compute();
        let err = api
            .rabbit_get_blocks_range(Some(vec![serde_json::json!("bad")]))
            .expect_err("non-object query should fail");
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn test_rabbit_list_compute_tx_results_returns_empty_page_past_total() {
        let api = build_test_api_with_persistent_compute();
        let newer_hash = Hash::from_bytes([0x91u8; 32]);
        let older_hash = Hash::from_bytes([0x81u8; 32]);
        rabbitnet::global_replace_compute_txs(vec![
            SyncComputeTxRecord {
                tx_hash: older_hash,
                result: serde_json::json!({
                    "submitted_at_unix": 9_000_000_010u64,
                    "status": "ok"
                }),
            },
            SyncComputeTxRecord {
                tx_hash: newer_hash,
                result: serde_json::json!({
                    "submitted_at_unix": 9_000_000_020u64,
                    "status": "ok"
                }),
            },
        ]);

        let page = api
            .rabbit_list_compute_tx_results(Some(vec![serde_json::json!({
                "page": 3,
                "limit": 1
            })]))
            .expect("list tx results");
        assert_eq!(page.get("page").and_then(|v| v.as_u64()), Some(3));
        assert_eq!(page.get("limit").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(page.get("total").and_then(|v| v.as_u64()), Some(2));
        assert_eq!(page.get("has_more").and_then(|v| v.as_bool()), Some(false));
        let items = page
            .get("items")
            .and_then(|v| v.as_array())
            .expect("items missing");
        assert!(items.is_empty());

        rabbitnet::global_replace_compute_txs(Vec::new());
    }

    #[test]
    fn test_rabbit_list_compute_tx_results_falls_back_to_global_synced_records() {
        let api = build_test_api_with_persistent_compute();
        let newer_hash = Hash::from_bytes([0x81u8; 32]);
        let older_hash = Hash::from_bytes([0x71u8; 32]);
        rabbitnet::global_replace_compute_txs(vec![
            SyncComputeTxRecord {
                tx_hash: older_hash,
                result: serde_json::json!({
                    "submitted_at_unix": 9_000_000_010u64,
                    "status": "ok"
                }),
            },
            SyncComputeTxRecord {
                tx_hash: newer_hash,
                result: serde_json::json!({
                    "submitted_at_unix": 9_000_000_020u64,
                    "status": "ok"
                }),
            },
        ]);

        let page_one = api
            .rabbit_list_compute_tx_results(Some(vec![serde_json::json!({
                "page": 1,
                "limit": 1
            })]))
            .expect("page one should succeed");
        assert_eq!(page_one.get("total").and_then(|v| v.as_u64()), Some(2));
        assert_eq!(
            page_one.get("has_more").and_then(|v| v.as_bool()),
            Some(true)
        );
        let page_one_items = page_one
            .get("items")
            .and_then(|v| v.as_array())
            .expect("page one items missing");
        assert_eq!(page_one_items.len(), 1);
        let expected_newer_tx_id = format!("0x{}", hex::encode(newer_hash.as_bytes()));
        assert_eq!(
            page_one_items[0].get("tx_id").and_then(|v| v.as_str()),
            Some(expected_newer_tx_id.as_str()),
        );

        let page_two = api
            .rabbit_list_compute_tx_results(Some(vec![serde_json::json!({
                "page": 2,
                "limit": 1
            })]))
            .expect("page two should succeed");
        assert_eq!(
            page_two.get("has_more").and_then(|v| v.as_bool()),
            Some(false)
        );
        let page_two_items = page_two
            .get("items")
            .and_then(|v| v.as_array())
            .expect("page two items missing");
        assert_eq!(page_two_items.len(), 1);
        let expected_older_tx_id = format!("0x{}", hex::encode(older_hash.as_bytes()));
        assert_eq!(
            page_two_items[0].get("tx_id").and_then(|v| v.as_str()),
            Some(expected_older_tx_id.as_str()),
        );

        rabbitnet::global_reset_sync_cache();
    }

    #[tokio::test]
    async fn test_rabbit_list_compute_tx_results_returns_paginated_items() {
        let api = build_test_api_with_persistent_compute();
        let tx_a = canonicalize_and_sign_compute_tx(serde_json::json!({
            "tx_id": format!("0x{}", hex::encode([0x21u8; 32])),
            "domain_id": 0,
            "chain_id": 10086,
            "network_id": 10086,
            "command": "Mint",
            "nonce": 1,
            "input_set": [],
            "read_set": [],
            "output_proposals": [
                {
                    "output_id": format!("0x{}", hex::encode([0x31u8; 32])),
                    "object_id": format!("0x{}", hex::encode([0x41u8; 32])),
                    "domain_id": 0,
                    "kind": "State",
                    "owner": { "type": "Shared" },
                    "predecessor": null,
                    "version": 1,
                    "state": "0x01",
                    "logic": null
                }
            ],
            "payload": "0x",
            "deadline_unix_secs": null,
            "witness": {"signatures": [], "threshold": 1}
        }));
        let tx_b = canonicalize_and_sign_compute_tx(serde_json::json!({
            "tx_id": format!("0x{}", hex::encode([0x22u8; 32])),
            "domain_id": 0,
            "chain_id": 10086,
            "network_id": 10086,
            "command": "Mint",
            "nonce": 2,
            "input_set": [],
            "read_set": [],
            "output_proposals": [
                {
                    "output_id": format!("0x{}", hex::encode([0x32u8; 32])),
                    "object_id": format!("0x{}", hex::encode([0x42u8; 32])),
                    "domain_id": 0,
                    "kind": "State",
                    "owner": { "type": "Shared" },
                    "predecessor": null,
                    "version": 1,
                    "state": "0x02",
                    "logic": null
                }
            ],
            "payload": "0x",
            "deadline_unix_secs": null,
            "witness": {"signatures": [], "threshold": 1}
        }));

        let _ = api
            .rabbit_submit_compute_tx(Some(vec![tx_a]))
            .await
            .expect("submit compute tx a");
        let _ = api
            .rabbit_submit_compute_tx(Some(vec![tx_b]))
            .await
            .expect("submit compute tx b");

        let listed = api
            .rabbit_list_compute_tx_results(Some(vec![serde_json::json!({
                "page": 1,
                "limit": 1
            })]))
            .expect("list tx results");
        assert_eq!(listed.get("page").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(listed.get("limit").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(listed.get("total").and_then(|v| v.as_u64()), Some(2));
        assert_eq!(listed.get("has_more").and_then(|v| v.as_bool()), Some(true));
        let items = listed
            .get("items")
            .and_then(|v| v.as_array())
            .expect("items missing");
        assert_eq!(items.len(), 1);
        let tx_id = items[0]
            .get("tx_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(tx_id.starts_with("0x"));
    }

    #[test]
    fn test_rabbit_get_operations_by_address_returns_explicit_unsupported_response() {
        let api = build_test_api_with_persistent_compute();
        let result = api
            .rabbit_get_operations_by_address(Some(vec![serde_json::json!({
                "address": "0x1111111111111111111111111111111111111111",
                "page": 1,
                "limit": 10
            })]))
            .expect("address history should return explicit unsupported payload");
        assert_eq!(
            result.get("unsupported").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(result.get("total").and_then(|v| v.as_u64()), Some(0));
        assert_eq!(
            result.get("reason").and_then(|v| v.as_str()),
            Some("address operation history is not supported on compute-only nodes")
        );
    }

    #[test]
    fn test_rabbit_list_operations_compute_filter_and_pagination() {
        let api = build_test_api_with_persistent_compute();
        let older_hash = Hash::from_bytes([0x31; 32]);
        let newer_hash = Hash::from_bytes([0x32; 32]);

        {
            let mut results = api.submitted_compute_results.write();
            results.insert(
                older_hash,
                serde_json::json!({
                    "ok": true,
                    "submitted_at_unix": 9_000_000_010u64,
                    "created_outputs": 1,
                }),
            );
            results.insert(
                newer_hash,
                serde_json::json!({
                    "ok": false,
                    "submitted_at_unix": 9_000_000_020u64,
                    "created_outputs": 0,
                }),
            );
        }
        {
            let mut order = api.submitted_compute_order.write();
            order.push_back(older_hash);
            order.push_back(newer_hash);
        }

        let first_page = api
            .rabbit_list_operations(Some(vec![serde_json::json!({
                "page": 1,
                "limit": 1,
                "kind": "compute"
            })]))
            .expect("compute tx list should succeed");
        assert!(
            first_page
                .get("total")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                >= 2
        );
        assert_eq!(
            first_page.get("has_more").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            first_page
                .get("items")
                .and_then(|v| v.as_array())
                .map(|items| items.len()),
            Some(1)
        );
        assert_eq!(
            first_page
                .get("items")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("kind"))
                .and_then(|v| v.as_str()),
            Some("compute")
        );
        let newer_hash_hex = format!("0x{}", hex::encode(newer_hash.as_bytes()));
        assert_eq!(
            first_page
                .get("items")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("tx_hash"))
                .and_then(|v| v.as_str()),
            Some(newer_hash_hex.as_str())
        );

        let second_page = api
            .rabbit_list_operations(Some(vec![serde_json::json!({
                "page": 2,
                "limit": 1,
                "kind": "compute"
            })]))
            .expect("compute tx second page should succeed");
        let older_hash_hex = format!("0x{}", hex::encode(older_hash.as_bytes()));
        assert_eq!(
            second_page
                .get("items")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("tx_hash"))
                .and_then(|v| v.as_str()),
            Some(older_hash_hex.as_str())
        );
    }

    #[test]
    fn test_rabbit_list_operations_rejects_invalid_kind() {
        let api = build_test_api_with_persistent_compute();
        let err = api
            .rabbit_list_operations(Some(vec![serde_json::json!({
                "page": 1,
                "limit": 10,
                "kind": "legacy"
            })]))
            .expect_err("invalid kind should be rejected");
        assert_eq!(err.code, -32602);
        assert_eq!(
            err.data,
            Some(serde_json::Value::String(
                "kind must be one of all|compute".to_string(),
            ))
        );
    }

    #[test]
    fn test_rabbit_submit_work_rejects_stale_work_id() {
        let api = build_test_api_with_persistent_compute();
        let submit = api.rabbit_submit_work(Some(vec![serde_json::json!({
            "work_id": "work-stale-1",
            "nonce": 1,
            "hash_hex": "0x00",
            "miner": "test-miner"
        })]));
        let err = submit.expect_err("stale work id should error");
        assert_eq!(err.code, -32602);
        assert_eq!(err.message, "Invalid params");
    }

    #[test]
    fn test_rabbit_submit_work_replay_is_rejected_after_accept() {
        let api = build_test_api_with_persistent_compute();
        install_easy_pow_head_if_needed(&api);
        let work = get_work(&api);
        let work_id = work["work_id"].as_str().unwrap().to_string();
        let prev_hash = work["prev_hash"].as_str().unwrap().to_string();
        let height = work["height"].as_u64().unwrap();
        let target = work_target(&work);

        let prev_hash_bytes =
            hex::decode(prev_hash.strip_prefix("0x").unwrap_or(&prev_hash)).unwrap();
        let (nonce, digest) = solve_work_digest(&prev_hash_bytes, height, 77, target);
        let payload = serde_json::json!({
            "work_id": work_id,
            "nonce": nonce,
            "hash_hex": format!("0x{}", hex::encode(digest)),
            "miner": "test-miner"
        });

        let first = api
            .rabbit_submit_work(Some(vec![payload.clone()]))
            .expect("first submit should succeed");
        assert_eq!(first["accepted"].as_bool(), Some(true));

        let second = api.rabbit_submit_work(Some(vec![payload]));
        let err = second.expect_err("replay should be rejected as stale work_id");
        assert_eq!(err.code, -32602);
        assert_eq!(err.message, "Invalid params");
    }

    #[test]
    fn test_rabbit_submit_work_rejects_stale_template_after_head_advances() {
        let api = build_test_api_with_persistent_compute();
        install_easy_pow_head_if_needed(&api);
        let stale_work = get_work(&api);
        let work_id = stale_work["work_id"].as_str().unwrap().to_string();
        let prev_hash = stale_work["prev_hash"].as_str().unwrap().to_string();
        let height = stale_work["height"].as_u64().unwrap();
        let target = legacy_target_from_leading_rabbit_bytes(0);
        set_work_target(&api, &work_id, target);
        let prev_hash_bytes =
            hex::decode(prev_hash.strip_prefix("0x").unwrap_or(&prev_hash)).unwrap();
        let (nonce, digest) = solve_work_digest(&prev_hash_bytes, height, 91, target);

        let advanced = make_rpc_test_block(1, &api.current_head_block().header);
        rabbitnet::global_store_block(advanced).expect("advance global head");

        let stale_submit = api
            .rabbit_submit_work(Some(vec![serde_json::json!({
                "work_id": work_id,
                "nonce": nonce,
                "hash_hex": format!("0x{}", hex::encode(digest)),
                "miner": "stale-miner"
            })]))
            .expect("stale submit should return rejection");

        assert_eq!(stale_submit["accepted"].as_bool(), Some(false));
        assert_eq!(stale_submit["reason"].as_str(), Some("stale_work_template"));
    }

    #[test]
    fn test_rabbit_submit_work_rejects_oversized_miner_label() {
        let api = build_test_api_with_persistent_compute();
        let work = get_work(&api);
        let work_id = work["work_id"].as_str().unwrap().to_string();
        let prev_hash = work["prev_hash"].as_str().unwrap().to_string();
        let height = work["height"].as_u64().unwrap();

        {
            let mut jobs = api.mining_jobs.write();
            if let Some(job) = jobs.get_mut(&work_id) {
                job.target = legacy_target_from_leading_rabbit_bytes(0);
            }
        }

        let prev_hash_bytes =
            hex::decode(prev_hash.strip_prefix("0x").unwrap_or(&prev_hash)).unwrap();
        let nonce = 88u64;
        let mut data = Vec::new();
        data.extend_from_slice(&prev_hash_bytes);
        data.extend_from_slice(&height.to_be_bytes());
        data.extend_from_slice(&nonce.to_be_bytes());
        let digest = rabbitcore::crypto::keccak256(&data);
        let too_long_miner = "m".repeat(MAX_MINER_EXTRA_DATA_BYTES + 1);

        let submit = api
            .rabbit_submit_work(Some(vec![serde_json::json!({
                "work_id": work_id,
                "nonce": nonce,
                "hash_hex": format!("0x{}", hex::encode(digest)),
                "miner": too_long_miner
            })]))
            .expect("submit should return rejection");

        assert_eq!(submit["accepted"].as_bool(), Some(false));
        assert_eq!(submit["reason"].as_str(), Some("invalid_miner_label"));
    }

    #[test]
    fn test_rabbit_import_block_rejects_parent_mismatch() {
        let api = build_test_api_with_persistent_compute();
        install_easy_pow_head_if_needed(&api);

        // Mine one block first.
        let first = futures::executor::block_on(api.rabbit_get_work(None)).expect("get work");
        let work_id = first["work_id"].as_str().unwrap().to_string();
        let prev_hash = first["prev_hash"].as_str().unwrap().to_string();
        let height = first["height"].as_u64().unwrap();
        let target = work_target(&first);
        let prev_hash_bytes =
            hex::decode(prev_hash.strip_prefix("0x").unwrap_or(&prev_hash)).unwrap();
        let (nonce, digest) = solve_work_digest(&prev_hash_bytes, height, 9, target);
        let mined = api
            .rabbit_submit_work(Some(vec![serde_json::json!({
                "work_id": work_id,
                "nonce": nonce,
                "hash_hex": format!("0x{}", hex::encode(digest)),
                "miner": "test-miner"
            })]))
            .expect("submit work");
        assert_eq!(mined["accepted"].as_bool(), Some(true));

        // Import block with wrong parent hash should be rejected.
        let latest = api.rabbit_get_latest_block(None).expect("latest block");
        let bad_import = api
            .rabbit_import_block(Some(vec![serde_json::json!({
                "hash": "0x1111111111111111111111111111111111111111111111111111111111111111",
                "parent_hash": "0x2222222222222222222222222222222222222222222222222222222222222222",
                "number": "0x2",
                "timestamp": latest["timestamp"].as_u64().unwrap_or(1) + 1,
                "difficulty": latest["difficulty"].as_str().unwrap_or("0x1"),
                "nonce": 1,
                "coinbase": latest["coinbase"].as_str().unwrap_or("0x0000000000000000000000000000000000000000"),
                "mix_hash": latest["mix_hash"].as_str().unwrap_or("0x0000000000000000000000000000000000000000000000000000000000000000"),
                "extra_data": "0x"
            })]))
            .expect("import call should return result");
        assert_eq!(bad_import["imported"].as_bool(), Some(false));
        assert_eq!(bad_import["reason"].as_str(), Some("parent_mismatch"));
    }

    #[test]
    fn test_current_head_prefers_global_when_heights_match() {
        let api = build_test_api_with_persistent_compute();
        install_easy_pow_head_if_needed(&api);

        let parent = api.current_head_block();
        let local = make_rpc_test_block_at_time(
            1,
            &parent.header,
            parent.header.timestamp.saturating_add(30),
        );
        let global = make_rpc_test_block_at_time(
            1,
            &parent.header,
            parent.header.timestamp.saturating_add(31),
        );

        *api.latest_block.write() = Some(local);
        rabbitnet::global_store_block(global.clone()).expect("store global head");

        let current = api.current_head_block();
        assert_eq!(current.header.hash, global.header.hash);
        assert_eq!(current.header.number, global.header.number);
    }

    #[test]
    fn test_rabbit_import_block_rejects_invalid_header_hash() {
        let api = build_test_api_with_persistent_compute();
        let latest = api.rabbit_get_latest_block(None).expect("latest block");
        let parent_hash = latest["hash"].as_str().expect("parent hash");
        let timestamp = latest["timestamp"].as_u64().unwrap_or(0) + 10;
        let difficulty = adjust_mining_difficulty(
            rabbitcore::account::U256::from_u128(1_000_000_000_000_000u128),
            latest["timestamp"].as_u64().unwrap_or(0),
            timestamp,
        );
        let nonce = 11u64;
        let mut data = Vec::new();
        data.extend_from_slice(
            &hex::decode(parent_hash.strip_prefix("0x").unwrap_or(parent_hash)).unwrap(),
        );
        data.extend_from_slice(&1u64.to_be_bytes());
        data.extend_from_slice(&nonce.to_be_bytes());
        let mix_hash = rabbitcore::crypto::keccak256(&data);

        let err = api
            .rabbit_import_block(Some(vec![serde_json::json!({
                "hash": "0x1111111111111111111111111111111111111111111111111111111111111111",
                "parent_hash": parent_hash,
                "number": "0x1",
                "timestamp": timestamp,
                "difficulty": format!("0x{:x}", difficulty.as_u64()),
                "nonce": nonce,
                "coinbase": "0x526Dc404e751C7d52F6fFF75d563d8D0857C94E9",
                "mix_hash": format!("0x{}", hex::encode(mix_hash)),
                "extra_data": "0x"
            })]))
            .expect_err("invalid hash should be rejected");
        assert_eq!(err.code, -32602);
        assert_eq!(
            err.data,
            Some(serde_json::Value::String(
                "block hash does not match header contents".to_string(),
            ))
        );
    }

    #[test]
    fn test_rabbit_get_metrics_contains_rpc_and_mining_counters() {
        let api = build_test_api_with_persistent_compute();

        let _ = futures::executor::block_on(api.handle_request(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "rabbit_getWork".to_string(),
            params: Some(vec![]),
            id: serde_json::json!(1),
        }));
        let _ = futures::executor::block_on(api.handle_request(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "rabbit_submitWork".to_string(),
            params: Some(vec![serde_json::json!({
                "work_id": "work-stale-2",
                "nonce": 1,
                "hash_hex": "0x00",
                "miner": "metric-miner"
            })]),
            id: serde_json::json!(2),
        }));

        let metrics = api.rabbit_get_metrics(None).expect("metrics should render");
        let text = metrics
            .get("text")
            .and_then(|v| v.as_str())
            .expect("metrics text missing");

        assert!(text.contains("rabbit_rpc_method_calls_total"));
        assert!(text.contains("rabbit_rpc_method_errors_total"));
    }

    #[test]
    fn test_rabbit_peers_returns_array() {
        let api = build_test_api_with_compute();
        let peers = api.rabbit_peers(None).expect("rabbit_peers should succeed");
        assert!(peers.is_array());
    }

    #[test]
    fn test_rabbit_peers_rejects_params() {
        let api = build_test_api_with_compute();
        let err = api
            .rabbit_peers(Some(vec![serde_json::json!(1)]))
            .expect_err("rabbit_peers should reject params");
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn test_is_authorized_supports_bearer_and_header_token() {
        let mut headers = HeaderMap::new();
        assert!(!is_authorized(&headers, Some("abc")));

        headers.insert(
            "authorization",
            axum::http::HeaderValue::from_static("Bearer abc"),
        );
        assert!(is_authorized(&headers, Some("abc")));

        headers.remove("authorization");
        headers.insert("x-rabbit-token", axum::http::HeaderValue::from_static("abc"));
        assert!(is_authorized(&headers, Some("abc")));
        assert!(!is_authorized(&headers, Some("def")));
    }

    #[test]
    fn test_method_requires_auth_token_for_stateful_writes() {
        assert!(method_requires_auth_token("rabbit_submitComputeTx"));
        assert!(method_requires_auth_token("rabbit_submitWork"));
        assert!(method_requires_auth_token("rabbit_importBlock"));
        assert!(!method_requires_auth_token("rabbit_getLatestBlock"));
        assert!(!method_requires_auth_token("rabbit_getAccount"));
    }

    #[test]
    fn test_rate_limiter_enforces_budget() {
        let cfg = RpcConfig {
            rate_limit_per_minute: 2,
            ..RpcConfig::default()
        };
        let limiter = RpcSecurityContext::new(&cfg);
        assert!(limiter.allow_request("127.0.0.1"));
        assert!(limiter.allow_request("127.0.0.1"));
        assert!(!limiter.allow_request("127.0.0.1"));
        assert!(limiter.allow_request("10.0.0.1"));
    }

    #[tokio::test]
    async fn test_rabbit_submit_compute_tx_rejects_network_mismatch() {
        let api = build_test_api_with_compute();

        let tx = canonicalize_and_sign_compute_tx(serde_json::json!({
            "tx_id": format!("0x{}", hex::encode([0x21u8; 32])),
            "domain_id": 0,
            "chain_id": 10086,
            "network_id": 1,
            "command": "Mint",
            "nonce": 1,
            "input_set": [],
            "read_set": [],
            "output_proposals": [{
                "output_id": format!("0x{}", hex::encode([0x22u8; 32])),
                "object_id": format!("0x{}", hex::encode([0x23u8; 32])),
                "domain_id": 0,
                "kind": "State",
                "owner": { "type": "Shared" },
                "predecessor": null,
                "version": 1,
                "state": "0x01",
                "logic": null
            }],
            "payload": "0x",
            "deadline_unix_secs": null,
            "witness": {"signatures": [], "threshold": 1}
        }));

        let err = api
            .rabbit_submit_compute_tx(Some(vec![tx]))
            .await
            .expect_err("network mismatch should be rejected");
        assert_eq!(err.code, -32602);
        assert_eq!(
            err.data,
            Some(serde_json::Value::String(
                "compute tx network_id 1 does not match node network_id 10086".to_string(),
            ))
        );
    }

    #[tokio::test]
    async fn test_require_fee_for_compute_tx_rejects_fee_less_tx() {
        let mut config = RpcConfig::default();
        config.mining_enabled = true;
        config.require_fee_for_compute_tx = true;
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let store = Arc::new(InMemoryObjectStore::new());
        let domains = Arc::new(InMemoryDomainRegistry::new());
        domains.upsert_domain(DomainConfig {
            domain_id: DomainId(0),
            name: "main".to_string(),
            vm: "wasm".to_string(),
            public: true,
        });
        let api = RpcApi::with_compute(config, state_db, store, domains);

        // Mint 是价值创造命令，执行器策略禁止携带任何费用，fee-less Mint 必须被接受。
        let mint = canonicalize_and_sign_compute_tx(serde_json::json!({
            "tx_id": format!("0x{}", hex::encode([0x31u8; 32])),
            "domain_id": 0,
            "chain_id": 10086,
            "network_id": 10086,
            "command": "Mint",
            "nonce": 1,
            "input_set": [],
            "read_set": [],
            "output_proposals": [{
                "output_id": format!("0x{}", hex::encode([0x32u8; 32])),
                "object_id": format!("0x{}", hex::encode([0x33u8; 32])),
                "domain_id": 0,
                "kind": "State",
                "owner": { "type": "Shared" },
                "predecessor": null,
                "version": 1,
                "state": "0x01",
                "logic": null
            }],
            "payload": "0x",
            "deadline_unix_secs": null,
            "witness": {"signatures": [], "threshold": 1}
        }));
        let mint_res = api
            .rabbit_submit_compute_tx(Some(vec![mint]))
            .await
            .expect("fee-less mint must be accepted (mint cannot carry fees by policy)");
        assert_eq!(mint_res["ok"], true);

        // 非 Mint 命令（Transfer/Invoke）在 require_fee 下必须携带费用。
        let transfer = canonicalize_and_sign_compute_tx(serde_json::json!({
            "tx_id": format!("0x{}", hex::encode([0x41u8; 32])),
            "domain_id": 0,
            "chain_id": 10086,
            "network_id": 10086,
            "command": "Transfer",
            "nonce": 1,
            "input_set": [format!("0x{}", hex::encode([0x42u8; 32]))],
            "read_set": [],
            "output_proposals": [{
                "output_id": format!("0x{}", hex::encode([0x43u8; 32])),
                "object_id": format!("0x{}", hex::encode([0x44u8; 32])),
                "domain_id": 0,
                "kind": "State",
                "owner": { "type": "Shared" },
                "predecessor": null,
                "version": 1,
                "state": "0x01",
                "logic": null
            }],
            "payload": "0x",
            "deadline_unix_secs": null,
            "witness": {"signatures": [], "threshold": 1}
        }));

        let err = api
            .rabbit_submit_compute_tx(Some(vec![transfer]))
            .await
            .expect_err("fee-less transfer should be rejected when require_fee is set");
        assert_eq!(err.code, -32602);
        assert_eq!(
            err.data,
            Some(serde_json::Value::String(
                "fee required: max_fee/priority_fee/gas_limit must be set".to_string(),
            ))
        );
    }

    #[test]
    fn test_rabbit_simulate_compute_tx_rejects_network_mismatch() {
        let api = build_test_api_with_compute();

        let tx = canonicalize_and_sign_compute_tx(serde_json::json!({
            "tx_id": format!("0x{}", hex::encode([0x24u8; 32])),
            "domain_id": 0,
            "chain_id": 10086,
            "network_id": 1,
            "command": "Mint",
            "nonce": 1,
            "input_set": [],
            "read_set": [],
            "output_proposals": [{
                "output_id": format!("0x{}", hex::encode([0x25u8; 32])),
                "object_id": format!("0x{}", hex::encode([0x26u8; 32])),
                "domain_id": 0,
                "kind": "State",
                "owner": { "type": "Shared" },
                "predecessor": null,
                "version": 1,
                "state": "0x01",
                "logic": null
            }],
            "payload": "0x",
            "deadline_unix_secs": null,
            "witness": {"signatures": [], "threshold": 1}
        }));

        let err = api
            .rabbit_simulate_compute_tx(Some(vec![tx]))
            .expect_err("simulate must reject network mismatch");
        assert_eq!(err.code, -32602);
        assert_eq!(
            err.data,
            Some(serde_json::Value::String(
                "compute tx network_id 1 does not match node network_id 10086".to_string(),
            ))
        );
    }

    #[test]
    fn test_parse_address() {
        let addr = parse_address("0x0000000000000000000000000000000000000001").unwrap();
        assert!(!addr.is_zero());
    }

    #[test]
    fn test_parse_address_accepts_0x_prefix() {
        let addr = parse_address("0x0000000000000000000000000000000000000001").unwrap();
        assert!(!addr.is_zero());
    }

    #[test]
    fn test_parse_address_rejects_invalid_prefix() {
        let err = parse_address("BAD10000000000000000000000000000000000001");
        assert!(err.is_err());
    }

    #[test]
    fn test_parse_address_rejects_uppercase_prefix() {
        let err = parse_address("0X1111111111111111111111111111111111111111");
        assert!(err.is_err());
    }

    #[test]
    fn test_format_rabbit_address_prefix() {
        let addr = parse_address("0x1111111111111111111111111111111111111111").unwrap();
        let formatted = format_rabbit_address(addr);
        assert!(formatted.starts_with("0x"));
        assert_eq!(formatted.len(), 42);
    }

    #[test]
    fn test_parse_hash() {
        let hash = parse_hash("0x0000000000000000000000000000000000000000000000000000000000000001")
            .unwrap();
        assert!(!hash.is_zero());
    }

    #[test]
    fn test_format_u256_hex_preserves_values_above_u64() {
        let value = U256::from_big_endian(&[0x01, 0, 0, 0, 0, 0, 0, 0, 0x01]); // 2^64 + 1
        assert_eq!(format_u256_hex(value), "0x10000000000000001");
    }

    #[test]
    fn test_rabbit_get_output_object_domain() {
        let api = build_test_api_with_compute();

        let output = ObjectOutput {
            output_id: OutputId(Hash::from_bytes([11; 32])),
            object_id: ObjectId(Hash::from_bytes([22; 32])),
            version: Version(1),
            domain_id: DomainId(0),
            kind: ObjectKind::State,
            owner: Ownership::Shared,
            predecessor: None,
            state: vec![0xAA, 0xBB],
            state_root: None,
            resources: vec![],
            lock: Script::default(),
            logic: None,
            created_at: 0,
            ttl: None,
            rent_reserve: None,
            flags: 0,
            extensions: vec![],
            spent: false,
        };

        api.compute_store.insert_output(output).unwrap();

        let output_id_hex = format!("0x{}", hex::encode([11u8; 32]));
        let out_value = api
            .rabbit_get_output(Some(vec![serde_json::Value::String(output_id_hex)]))
            .unwrap();
        assert!(out_value.is_object());

        let object_id_hex = format!("0x{}", hex::encode([22u8; 32]));
        let obj_value = api
            .rabbit_get_object(Some(vec![serde_json::Value::String(object_id_hex)]))
            .unwrap();
        assert!(obj_value.is_object());

        let domain_value = api
            .rabbit_get_domain(Some(vec![serde_json::Value::from(0u64)]))
            .unwrap();
        assert_eq!(
            domain_value.get("domain_id").and_then(|v| v.as_u64()),
            Some(0)
        );
    }

    #[tokio::test]
    async fn test_rabbit_simulate_and_submit_compute_tx() {
        let api = build_test_api_with_compute();

        let tx = canonicalize_and_sign_compute_tx(serde_json::json!({
            "tx_id": format!("0x{}", hex::encode([0x55u8; 32])),
            "domain_id": 0,
            "chain_id": 10086,
            "network_id": 10086,
            "command": "Mint",
            "nonce": 1,
            "input_set": [],
            "read_set": [],
            "output_proposals": [
                {
                    "output_id": format!("0x{}", hex::encode([0x66u8; 32])),
                    "object_id": format!("0x{}", hex::encode([0x77u8; 32])),
                    "domain_id": 0,
                    "kind": "State",
                    "owner": { "type": "Shared" },
                    "predecessor": null,
                    "version": 1,
                    "state": "0x010203",
                    "logic": null
                }
            ],
            "payload": "0x",
            "deadline_unix_secs": null,
            "witness": {"signatures": [], "threshold": 1}
        }));

        let sim = api
            .rabbit_simulate_compute_tx(Some(vec![tx.clone()]))
            .expect("simulation should succeed");
        assert_eq!(sim.get("ok").and_then(|v| v.as_bool()), Some(true));

        let submit = api
            .rabbit_submit_compute_tx(Some(vec![tx.clone()]))
            .await
            .expect("submit should succeed");
        assert_eq!(submit.get("ok").and_then(|v| v.as_bool()), Some(true));

        // BlockTime：提交仅入队，对象在区块执行后才存在
        let out_before = api
            .rabbit_get_output(Some(vec![serde_json::Value::String(format!(
                "0x{}",
                hex::encode([0x66u8; 32])
            ))]))
            .expect("output query should succeed");
        assert!(out_before.is_null());

        // 模拟区块执行（与 rabbit_submit_work 相同路径）→ 对象创建
        let parsed_tx = parse_compute_tx(tx.clone()).expect("tx should parse");
        let executor = api.state_executor.new_basic_executor();
        let (receipts, _) = api
            .state_executor
            .execute_txs(
                std::slice::from_ref(&parsed_tx),
                rabbitcore::compute::INITIAL_BASE_FEE,
                &executor,
            )
            .expect("block-time execute");
        assert_eq!(receipts[0].status, rabbitcore::block::ReceiptStatus::Success);

        let out = api
            .rabbit_get_output(Some(vec![serde_json::Value::String(format!(
                "0x{}",
                hex::encode([0x66u8; 32])
            ))]))
            .expect("output query should succeed");
        assert!(out.is_object());

        let dup = api
            .rabbit_submit_compute_tx(Some(vec![tx]))
            .await
            .expect("duplicate submit should return cached result");
        assert_eq!(dup.get("duplicate").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn test_rabbit_simulate_returns_structured_domain_error() {
        let api = build_test_api_with_compute();
        let tx = canonicalize_and_sign_compute_tx(serde_json::json!({
            "tx_id": format!("0x{}", hex::encode([0x99u8; 32])),
            "domain_id": 9,
            "chain_id": 10086,
            "network_id": 10086,
            "command": "Mint",
            "nonce": 2,
            "input_set": [],
            "read_set": [],
            "output_proposals": [{
                "output_id": format!("0x{}", hex::encode([0x98u8; 32])),
                "object_id": format!("0x{}", hex::encode([0x97u8; 32])),
                "domain_id": 9,
                "kind": "State",
                "owner": { "type": "Shared" },
                "predecessor": null,
                "version": 1,
                "state": "0x01",
                "logic": null
            }],
            "payload": "0x",
            "deadline_unix_secs": null,
            "witness": {"signatures": [], "threshold": 1}
        }));

        let sim = api
            .rabbit_simulate_compute_tx(Some(vec![tx]))
            .expect("simulate should return result object");
        assert_eq!(sim.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            sim.get("error")
                .and_then(|v| v.get("category"))
                .and_then(|v| v.as_str()),
            Some("domain")
        );
        assert_eq!(
            sim.get("error")
                .and_then(|v| v.get("numeric_code"))
                .and_then(|v| v.as_i64()),
            Some(1001)
        );
    }

    #[test]
    fn test_rabbit_simulate_rejects_malformed_witness_signature() {
        let api = build_test_api_with_compute();

        let tx = serde_json::json!({
            "tx_id": format!("0x{}", hex::encode([0xD1u8; 32])),
            "domain_id": 0,
            "chain_id": 10086,
            "network_id": 10086,
            "command": "Transfer",
            "nonce": 2,
            "input_set": [format!("0x{}", hex::encode([0xD2u8; 32]))],
            "read_set": [],
            "output_proposals": [{
                "output_id": format!("0x{}", hex::encode([0xD3u8; 32])),
                "object_id": format!("0x{}", hex::encode([0xD4u8; 32])),
                "domain_id": 0,
                "kind": "Asset",
                "owner": { "type": "Address", "address": "0x1111111111111111111111111111111111111111" },
                "predecessor": null,
                "version": 1,
                "state": "0x01",
                "logic": null
            }],
            "payload": "0x",
            "deadline_unix_secs": null,
            "witness": {"signatures": [{
                "scheme": "ed25519",
                "signature": "0x00",
                "public_key": "0x0000000000000000000000000000000000000000000000000000000000000000"
            }], "threshold": 1}
        });

        // Prepare input object for transfer validation.
        let input = ObjectOutput {
            output_id: OutputId(Hash::from_bytes([0xD2; 32])),
            object_id: ObjectId(Hash::from_bytes([0xE1; 32])),
            version: Version(1),
            domain_id: DomainId(0),
            kind: ObjectKind::Asset,
            owner: Ownership::Address(
                Address::from_hex("0x1111111111111111111111111111111111111111").unwrap(),
            ),
            predecessor: None,
            state: vec![1],
            state_root: None,
            resources: vec![],
            lock: Script::default(),
            logic: None,
            created_at: 0,
            ttl: None,
            rent_reserve: None,
            flags: 0,
            extensions: vec![],
            spent: false,
        };
        api.compute_store.insert_output(input).unwrap();

        let err = api
            .rabbit_simulate_compute_tx(Some(vec![tx]))
            .expect_err("malformed witness should be rejected during parsing");
        assert_eq!(err.code, -32602);
        assert_eq!(
            err.data,
            Some(serde_json::Value::String(
                "ed25519 signature must be 64 bytes".to_string(),
            ))
        );
    }

    #[test]
    fn test_rabbit_simulate_returns_owner_mismatch_error() {
        let api = build_test_api_with_compute();

        let input = ObjectOutput {
            output_id: OutputId(Hash::from_bytes([0xF2; 32])),
            object_id: ObjectId(Hash::from_bytes([0xF3; 32])),
            version: Version(1),
            domain_id: DomainId(0),
            kind: ObjectKind::Asset,
            owner: Ownership::Address(
                Address::from_hex("0x2222222222222222222222222222222222222222").unwrap(),
            ),
            predecessor: None,
            state: vec![1],
            state_root: None,
            resources: vec![],
            lock: Script::default(),
            logic: None,
            created_at: 0,
            ttl: None,
            rent_reserve: None,
            flags: 0,
            extensions: vec![],
            spent: false,
        };
        api.compute_store.insert_output(input).unwrap();

        let mut tx = serde_json::json!({
            "tx_id": format!("0x{}", hex::encode([0xF1u8; 32])),
            "domain_id": 0,
            "chain_id": 10086,
            "network_id": 10086,
            "command": "Transfer",
            "nonce": 3,
            "input_set": [format!("0x{}", hex::encode([0xF2u8; 32]))],
            "read_set": [],
            "output_proposals": [{
                "output_id": format!("0x{}", hex::encode([0xF4u8; 32])),
                "object_id": format!("0x{}", hex::encode([0xF3u8; 32])),
                "domain_id": 0,
                "kind": "Asset",
                "owner": { "type": "Address", "address": "0x2222222222222222222222222222222222222222" },
                "predecessor": format!("0x{}", hex::encode([0xF2u8; 32])),
                "version": 2,
                "state": "0x01",
                "logic": null
            }],
            "payload": "0x",
            "deadline_unix_secs": null,
            "witness": {"signatures": [], "threshold": 1}
        });

        let tx = canonicalize_compute_tx_id(tx);
        let tx = attach_ed25519_signature(tx, 3);

        let sim = api
            .rabbit_simulate_compute_tx(Some(vec![tx]))
            .expect("simulate should return result object");
        assert_eq!(sim.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            sim.get("error")
                .and_then(|v| v.get("code"))
                .and_then(|v| v.as_str()),
            Some("signature_owner_mismatch")
        );
        assert_eq!(
            sim.get("error")
                .and_then(|v| v.get("numeric_code"))
                .and_then(|v| v.as_i64()),
            Some(3004)
        );
    }

    #[test]
    fn test_rabbit_simulate_returns_tx_id_mismatch_error() {
        let api = build_test_api_with_compute();
        let owner_addr = ed25519_address_from_seed(9);

        let input = ObjectOutput {
            output_id: OutputId(Hash::from_bytes([0xAB; 32])),
            object_id: ObjectId(Hash::from_bytes([0xAC; 32])),
            version: Version(1),
            domain_id: DomainId(0),
            kind: ObjectKind::Asset,
            owner: Ownership::Address(owner_addr),
            predecessor: None,
            state: vec![1],
            state_root: None,
            resources: vec![],
            lock: Script::default(),
            logic: None,
            created_at: 0,
            ttl: None,
            rent_reserve: None,
            flags: 0,
            extensions: vec![],
            spent: false,
        };
        api.compute_store.insert_output(input).unwrap();

        let mut tx = serde_json::json!({
            "tx_id": format!("0x{}", hex::encode([0xADu8; 32])),
            "domain_id": 0,
            "chain_id": 10086,
            "network_id": 10086,
            "command": "Transfer",
            "nonce": 4,
            "input_set": [format!("0x{}", hex::encode([0xABu8; 32]))],
            "read_set": [],
            "output_proposals": [{
                "output_id": format!("0x{}", hex::encode([0xAEu8; 32])),
                "object_id": format!("0x{}", hex::encode([0xACu8; 32])),
                "domain_id": 0,
                "kind": "Asset",
                "owner": { "type": "Address", "address": format_rabbit_address(owner_addr) },
                "predecessor": format!("0x{}", hex::encode([0xABu8; 32])),
                "version": 2,
                "state": "0x02",
                "logic": null
            }],
            "payload": "0x1234",
            "deadline_unix_secs": 1900000000u64,
            "witness": {"signatures": [], "threshold": 1}
        });

        let tx = attach_ed25519_signature(tx, 9);

        let sim = api
            .rabbit_simulate_compute_tx(Some(vec![tx]))
            .expect("simulate should return result object");
        assert_eq!(sim.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            sim.get("error")
                .and_then(|v| v.get("code"))
                .and_then(|v| v.as_str()),
            Some("tx_id_mismatch")
        );
        assert_eq!(
            sim.get("error")
                .and_then(|v| v.get("numeric_code"))
                .and_then(|v| v.as_i64()),
            Some(3005)
        );
    }

    #[test]
    fn test_parse_compute_tx_requires_witness() {
        let tx = serde_json::json!({
            "tx_id": format!("0x{}", hex::encode([0x11u8; 32])),
            "domain_id": 0,
            "chain_id": 10086,
            "network_id": 10086,
            "command": "Mint",
            "nonce": 3,
            "input_set": [],
            "read_set": [],
            "output_proposals": [],
            "payload": "0x",
            "deadline_unix_secs": null
        });

        let err = parse_compute_tx(tx).expect_err("witness should be required");
        assert_eq!(err.code, -32602);
    }

    #[tokio::test]
    async fn test_rabbit_get_compute_tx_result_with_persistent_store() {
        let api = build_test_api_with_persistent_compute();
        let tx = canonicalize_and_sign_compute_tx(serde_json::json!({
            "tx_id": format!("0x{}", hex::encode([0xA1u8; 32])),
            "domain_id": 0,
            "chain_id": 10086,
            "network_id": 10086,
            "command": "Mint",
            "nonce": 4,
            "input_set": [],
            "read_set": [],
            "output_proposals": [{
                "output_id": format!("0x{}", hex::encode([0xA2u8; 32])),
                "object_id": format!("0x{}", hex::encode([0xA3u8; 32])),
                "domain_id": 0,
                "kind": "State",
                "owner": { "type": "Shared" },
                "predecessor": null,
                "version": 1,
                "state": "0x01",
                "logic": null
            }],
            "payload": "0x",
            "deadline_unix_secs": null,
            "witness": {"signatures": [], "threshold": 1}
        }));
        let tx_id_hex = tx
            .get("tx_id")
            .and_then(|v| v.as_str())
            .expect("tx_id must exist after canonicalization")
            .to_string();

        let submit = api
            .rabbit_submit_compute_tx(Some(vec![tx]))
            .await
            .expect("submit should succeed");
        assert_eq!(submit.get("ok").and_then(|v| v.as_bool()), Some(true));

        let got = api
            .rabbit_get_compute_tx_result(Some(vec![serde_json::Value::String(tx_id_hex)]))
            .expect("get tx result should succeed");
        assert_eq!(got.get("ok").and_then(|v| v.as_bool()), Some(true));
    }

    #[tokio::test]
    async fn test_rabbit_submit_compute_tx_persists_result_for_new_api_instance() {
        let _guard = test_guard();
        rabbitnet::global_reset_sync_cache();
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let db = Arc::new(MemDatabase::new());
        let persistent_store = Arc::new(ComputeStore::new(db.clone()));
        let domains = Arc::new(InMemoryDomainRegistry::new());
        domains.upsert_domain(DomainConfig {
            domain_id: DomainId(0),
            name: "main".to_string(),
            vm: "wasm".to_string(),
            public: true,
        });

        let mut config = RpcConfig::default();
        config.mining_enabled = true;
        let api = RpcApi::with_persistent_compute(
            config.clone(),
            state_db.clone(),
            persistent_store.clone(),
            domains.clone(),
        );
        *api.latest_block.write() = Some(Block::new(legacy_rpc_test_root()));

        let tx = canonicalize_and_sign_compute_tx(serde_json::json!({
            "tx_id": format!("0x{}", hex::encode([0xA4u8; 32])),
            "domain_id": 0,
            "chain_id": 10086,
            "network_id": 10086,
            "command": "Mint",
            "nonce": 5,
            "input_set": [],
            "read_set": [],
            "output_proposals": [{
                "output_id": format!("0x{}", hex::encode([0xA5u8; 32])),
                "object_id": format!("0x{}", hex::encode([0xA6u8; 32])),
                "domain_id": 0,
                "kind": "State",
                "owner": { "type": "Shared" },
                "predecessor": null,
                "version": 1,
                "state": "0x01",
                "logic": null
            }],
            "payload": "0x",
            "deadline_unix_secs": null,
            "witness": {"signatures": [], "threshold": 1}
        }));
        let parsed_tx = parse_compute_tx(tx.clone()).expect("tx should parse");
        let tx_id = parsed_tx.tx_id;
        let tx_id_hex = format!("0x{}", hex::encode(tx_id.0.as_bytes()));

        let submit = api
            .rabbit_submit_compute_tx(Some(vec![tx]))
            .await
            .expect("submit should succeed");
        assert_eq!(submit.get("ok").and_then(|v| v.as_bool()), Some(true));

        // BlockTime：提交仅入队；真实结果/对象在区块执行时写入共享 compute store
        let executor = api.state_executor.new_basic_executor();
        let (receipts, _) = api
            .state_executor
            .execute_txs(
                std::slice::from_ref(&parsed_tx),
                rabbitcore::compute::INITIAL_BASE_FEE,
                &executor,
            )
            .expect("block-time execute");
        assert_eq!(receipts[0].status, rabbitcore::block::ReceiptStatus::Success);

        // 新实例共享同一持久化 store → 区块执行创建的对象跨实例可见
        let api2 = RpcApi::with_persistent_compute(config, state_db, persistent_store, domains);
        *api2.latest_block.write() = Some(Block::new(legacy_rpc_test_root()));

        let got = api2
            .rabbit_get_object(Some(vec![serde_json::Value::String(format!(
                "0x{}",
                hex::encode([0xA6u8; 32])
            ))]))
            .expect("get object should succeed");
        assert!(got.is_object());
        let _ = tx_id_hex;
    }

    #[test]
    fn test_get_compute_tx_result_returns_null_when_missing() {
        let api = build_test_api_with_persistent_compute();
        let missing = api
            .rabbit_get_compute_tx_result(Some(vec![serde_json::Value::String(format!(
                "0x{}",
                hex::encode([0xFEu8; 32])
            ))]))
            .expect("query should not fail");
        assert!(missing.is_null());
    }

    #[test]
    fn test_build_compute_backend_mem() {
        let cfg = RpcConfig {
            compute_backend: ComputeBackend::Mem,
            ..RpcConfig::default()
        };
        let db = build_compute_kv_backend(&cfg).expect("mem backend should initialize");
        db.put(b"k", b"v").unwrap();
        assert_eq!(db.get(b"k").unwrap(), Some(b"v".to_vec()));
    }

    #[test]
    fn test_build_compute_backend_file_open_failure_returns_error() {
        let cfg = RpcConfig {
            compute_backend: ComputeBackend::RocksDb,
            compute_db_path: "/dev/null/rabbitchain-db".to_string(),
            ..RpcConfig::default()
        };
        let err = match build_compute_kv_backend(&cfg) {
            Ok(_) => panic!("invalid path should fail"),
            Err(err) => err,
        };
        match err {
            crate::ApiError::InvalidRequest(msg) => {
                assert!(msg.contains("failed to open rocksdb"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn test_rpc_config_validate_rejects_empty_path_for_file_backend() {
        let cfg = RpcConfig {
            compute_backend: ComputeBackend::RocksDb,
            compute_db_path: "   ".to_string(),
            ..RpcConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_try_new_returns_error_for_invalid_config() {
        let cfg = RpcConfig {
            compute_backend: ComputeBackend::Redb,
            compute_db_path: "".to_string(),
            ..RpcConfig::default()
        };
        let err = match RpcServer::try_new(cfg) {
            Ok(_) => panic!("invalid config should fail"),
            Err(err) => err,
        };
        match err {
            crate::ApiError::InvalidRequest(_) => {}
            other => panic!("unexpected error: {other}"),
        }
    }
}
