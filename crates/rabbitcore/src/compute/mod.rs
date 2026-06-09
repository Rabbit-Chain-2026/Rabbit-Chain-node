//! UTXO Compute v1.1 core module.
//!
//! This module is the canonical L1 execution path for RabbitChain compute.
//! L1 consensus and state transitions are defined here.

pub mod agent;
pub mod batch;
pub mod domain;
pub mod error;
pub mod execution;
pub mod object;
pub mod policy;
pub mod primitives;
pub mod scheduler;
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
pub use execution::{
    BasicTxExecutor, BasicTxValidator, InMemoryObjectStore, ObjectStore, ValidationReport,
};
pub use object::{
    AssetId, ObjectKind, ObjectOutput, Ownership, ResourceMap, ResourceValue, Script,
};
pub use policy::{
    AuthorizationPolicy, DefaultAuthorizationPolicy, NoopResourcePolicy, ResourcePolicy,
};
pub use primitives::{DomainId, ObjectId, ObjectPointer, OutputId, ResourceId, TxId, Version};
pub use scheduler::{
    ComputeLaneKeyStrategy, ComputeLaneStrategy, ComputeScheduleError, ComputeScheduleTicket,
    ComputeScheduler, ComputeSchedulerConfig, InMemoryComputeScheduler, PendingComputeTx,
};
pub use tx::{
    Command, ComputeTx, Metadata, ObjectReadRef, OutputProposal, SignatureScheme, TxSignature,
    TxWitness,
};
