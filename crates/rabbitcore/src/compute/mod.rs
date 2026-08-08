//! UTXO Compute v1.1 core module.
//!
//! This module is the canonical L1 execution path for RabbitChain compute.
//! L1 consensus and state transitions are defined here.

pub mod agent;
pub mod batch;
pub mod domain;
pub mod error;
pub mod execution;
pub mod gas;
pub mod object;
pub mod policy;
pub mod primitives;
pub mod scheduler;
pub mod spec_json;
pub mod tx;

pub use agent::{AgentScheduler, AgentSpec, AgentTask, InMemoryAgentScheduler};
pub use batch::{
    ComputeAccessSet, ComputeBatchGroup, ComputeBatchOutcome, ComputeBatchPlan,
    ComputeBatchPlanner, ComputeBatchRunner, ComputeConflictPolicy, ComputeExecutionService,
    ComputeFallbackDisposition, ComputeFallbackMode, ComputeFallbackPolicy,
    DefaultComputeBatchPlanner, DefaultComputeConflictPolicy, DisabledComputeFallbackPolicy,
    ParallelComputeBatchRunner, PlannedComputeTx, SerialComputeFallbackPolicy,
};
pub use domain::{DomainConfig, DomainRegistry, InMemoryDomainRegistry};
pub use error::ComputeError;
pub use gas::{
    calculate_base_fee, carrots_to_hopps, effective_tip, effective_tip_rate, estimate_tx_gas,
    hopps_to_carrots, rbit_to_hopps, validate_tx_fee, FeeValidationError, INITIAL_BASE_FEE,
    DEFAULT_BLOCK_GAS_LIMIT, MAX_GAS_LIMIT_PER_TX, MAX_PRIORITY_FEE, TX_BASE_GAS,
};
pub use execution::{
    BasicTxExecutor, BasicTxValidator, InMemoryObjectStore, InMemoryReplayNonceRegistry,
    ObjectStore, ReplayNonceRegistry, ValidationReport, REPLAY_NONCE_WINDOW_SECS,
};
pub use object::{
    AssetId, ObjectKind, ObjectOutput, Ownership, ResourceMap, ResourceValue, Script,
};
pub use policy::{
    AuthorizationPolicy, DefaultAuthorizationPolicy, NoopResourcePolicy, ResourcePolicy,
};
pub use primitives::{DomainId, GAME_DOMAIN, ObjectId, ObjectPointer, OutputId, ResourceId, TxId, Version};
pub use scheduler::{
    ComputeLaneKeyStrategy, ComputeLaneStrategy, ComputeScheduleError, ComputeScheduleTicket,
    ComputeScheduler, ComputeSchedulerConfig, InMemoryComputeScheduler, PendingComputeTx,
};
pub use tx::{
    Command, ComputeTx, Metadata, ObjectReadRef, OutputProposal, SignatureScheme, TxSignature,
    TxWitness,
};
