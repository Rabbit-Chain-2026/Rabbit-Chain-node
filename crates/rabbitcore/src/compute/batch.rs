//! Batched compute planning and execution.

use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

use super::{
    domain::DomainRegistry,
    error::{ComputeError, ComputeResult},
    execution::{BasicTxExecutor, BasicTxValidator, ObjectStore, ValidationReport},
    policy::{AuthorizationPolicy, ResourcePolicy},
    primitives::{DomainId, ObjectId, OutputId, TxId},
    scheduler::{ComputeScheduleError, ComputeScheduler, PendingComputeTx},
    tx::ComputeTx,
};

const MAX_COMPLETED_OUTCOMES: usize = 50_000;
const DEFAULT_BATCH_WORKERS: usize = 8;
const POST_FLUSH_STABILITY_MIN_MS: u64 = 5;
const POST_FLUSH_STABILITY_MAX_MS: u64 = 25;

/// Failure fallback mode for batch execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeFallbackMode {
    /// Never fallback to serial execution.
    Disabled,
    /// Fallback to serial execution when batch admission or outcome lookup fails.
    SerialOnFailure,
}

impl Default for ComputeFallbackMode {
    fn default() -> Self {
        Self::SerialOnFailure
    }
}

impl ComputeFallbackMode {
    /// Builds the runtime policy object for this mode.
    pub fn build_policy(self) -> Arc<dyn ComputeFallbackPolicy> {
        match self {
            Self::Disabled => Arc::new(DisabledComputeFallbackPolicy),
            Self::SerialOnFailure => Arc::new(SerialComputeFallbackPolicy),
        }
    }
}

/// Fallback disposition for a compute execution failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComputeFallbackDisposition {
    /// Reject the tx or batch.
    Reject,
    /// Execute the affected txs serially.
    RunSerial,
}

/// Fallback policy hook.
pub trait ComputeFallbackPolicy: Send + Sync {
    /// Fallback choice when scheduler admission fails.
    fn on_schedule_reject(
        &self,
        tx: &ComputeTx,
        err: &ComputeScheduleError,
    ) -> ComputeFallbackDisposition;
    /// Fallback choice when batch planning fails.
    fn on_plan_error(
        &self,
        pending: &[PendingComputeTx],
        err: &ComputeError,
    ) -> ComputeFallbackDisposition;
    /// Fallback choice when a submitted tx cannot be resolved in the outcome cache.
    fn on_missing_outcome(&self, tx: &ComputeTx) -> ComputeFallbackDisposition;
}

/// Strict fallback policy.
#[derive(Clone, Copy, Debug, Default)]
pub struct DisabledComputeFallbackPolicy;

impl ComputeFallbackPolicy for DisabledComputeFallbackPolicy {
    fn on_schedule_reject(
        &self,
        _tx: &ComputeTx,
        _err: &ComputeScheduleError,
    ) -> ComputeFallbackDisposition {
        ComputeFallbackDisposition::Reject
    }

    fn on_plan_error(
        &self,
        _pending: &[PendingComputeTx],
        _err: &ComputeError,
    ) -> ComputeFallbackDisposition {
        ComputeFallbackDisposition::Reject
    }

    fn on_missing_outcome(&self, _tx: &ComputeTx) -> ComputeFallbackDisposition {
        ComputeFallbackDisposition::Reject
    }
}

/// Serial fallback policy used by the default runtime mode.
#[derive(Clone, Copy, Debug, Default)]
pub struct SerialComputeFallbackPolicy;

impl ComputeFallbackPolicy for SerialComputeFallbackPolicy {
    fn on_schedule_reject(
        &self,
        _tx: &ComputeTx,
        _err: &ComputeScheduleError,
    ) -> ComputeFallbackDisposition {
        ComputeFallbackDisposition::RunSerial
    }

    fn on_plan_error(
        &self,
        _pending: &[PendingComputeTx],
        _err: &ComputeError,
    ) -> ComputeFallbackDisposition {
        ComputeFallbackDisposition::RunSerial
    }

    fn on_missing_outcome(&self, _tx: &ComputeTx) -> ComputeFallbackDisposition {
        ComputeFallbackDisposition::RunSerial
    }
}

/// Access-set summary used for conflict detection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComputeAccessSet {
    /// Domain executed by this tx.
    pub domain_id: DomainId,
    /// Outputs read by the tx.
    pub read_output_ids: BTreeSet<OutputId>,
    /// Logical objects read by the tx.
    pub read_object_ids: BTreeSet<ObjectId>,
    /// Outputs written or consumed by the tx.
    pub write_output_ids: BTreeSet<OutputId>,
    /// Logical objects written or consumed by the tx.
    pub write_object_ids: BTreeSet<ObjectId>,
}

impl Default for ComputeAccessSet {
    fn default() -> Self {
        Self {
            domain_id: DomainId(0),
            read_output_ids: BTreeSet::new(),
            read_object_ids: BTreeSet::new(),
            write_output_ids: BTreeSet::new(),
            write_object_ids: BTreeSet::new(),
        }
    }
}

/// Planned transaction with its resolved access set.
#[derive(Clone, Debug)]
pub struct PlannedComputeTx {
    /// Pending tx wrapper.
    pub pending: PendingComputeTx,
    /// Resolved access set.
    pub access: ComputeAccessSet,
}

/// Batch execution group.
#[derive(Clone, Debug)]
pub struct ComputeBatchGroup {
    /// Domain id shared by the group.
    pub domain_id: DomainId,
    /// Planned txs in execution order.
    pub txs: Vec<PlannedComputeTx>,
}

/// Batch plan emitted by the planner.
#[derive(Clone, Debug, Default)]
pub struct ComputeBatchPlan {
    /// Batchable groups.
    pub groups: Vec<ComputeBatchGroup>,
    /// Transactions that were not batchable and should be run one-by-one.
    pub fallback: Vec<PendingComputeTx>,
}

/// Per-tx execution outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComputeBatchOutcome {
    /// Transaction id.
    pub tx_id: TxId,
    /// Whether execution committed successfully.
    pub accepted: bool,
    /// Validation / execution report on success.
    pub report: Option<ValidationReport>,
    /// Number of outputs created by this tx.
    pub created_outputs: usize,
    /// Error on failure.
    pub error: Option<ComputeError>,
}

struct InFlightExecution {
    result: tokio::sync::watch::Sender<Option<ComputeResult<ComputeBatchOutcome>>>,
}

impl InFlightExecution {
    fn new() -> Self {
        let (result, _) = tokio::sync::watch::channel(None);
        Self { result }
    }

    fn complete(&self, outcome: ComputeResult<ComputeBatchOutcome>) {
        self.result.send_replace(Some(outcome));
    }

    async fn wait(&self) -> ComputeResult<ComputeBatchOutcome> {
        let mut receiver = self.result.subscribe();
        loop {
            if let Some(outcome) = receiver.borrow().clone() {
                return outcome;
            }
            if receiver.changed().await.is_err() {
                return Err(ComputeError::InvalidOperation(
                    "compute execution stopped before publishing a result".to_string(),
                ));
            }
        }
    }
}

/// Conflict policy used by the batch planner.
pub trait ComputeConflictPolicy: Send + Sync {
    /// Builds a resolved access set from the tx and current store state.
    fn access_set(
        &self,
        tx: &ComputeTx,
        store: &dyn ObjectStore,
    ) -> ComputeResult<ComputeAccessSet>;

    /// Returns true when two txs cannot be placed in the same batch group.
    fn conflicts(&self, left: &ComputeAccessSet, right: &ComputeAccessSet) -> bool;

    /// Whether this tx is eligible for batched execution.
    fn can_batch(&self, _tx: &ComputeTx) -> bool {
        true
    }
}

/// Default conservative conflict policy.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultComputeConflictPolicy;

impl DefaultComputeConflictPolicy {
    fn intersects<T: Ord>(left: &BTreeSet<T>, right: &BTreeSet<T>) -> bool {
        left.iter().any(|item| right.contains(item))
    }
}

impl ComputeConflictPolicy for DefaultComputeConflictPolicy {
    fn access_set(
        &self,
        tx: &ComputeTx,
        store: &dyn ObjectStore,
    ) -> ComputeResult<ComputeAccessSet> {
        let mut access = ComputeAccessSet {
            domain_id: tx.domain_id,
            ..Default::default()
        };

        for output_id in &tx.input_set {
            let Some(output) = store.get_output(*output_id) else {
                return Err(ComputeError::ObjectNotFound(output_id.0));
            };
            access.write_output_ids.insert(*output_id);
            access.write_object_ids.insert(output.object_id);
        }

        for read_ref in &tx.read_set {
            let Some(output) = store.get_output(read_ref.output_id) else {
                return Err(ComputeError::ObjectNotFound(read_ref.output_id.0));
            };
            access.read_output_ids.insert(read_ref.output_id);
            access.read_object_ids.insert(output.object_id);
        }

        for proposal in &tx.output_proposals {
            access.write_output_ids.insert(proposal.output_id);
            access.write_object_ids.insert(proposal.object_id);

            if let Some(predecessor) = proposal.predecessor {
                let Some(output) = store.get_output(predecessor) else {
                    return Err(ComputeError::ObjectNotFound(predecessor.0));
                };
                access.write_output_ids.insert(predecessor);
                access.write_object_ids.insert(output.object_id);
            }
        }

        Ok(access)
    }

    fn conflicts(&self, left: &ComputeAccessSet, right: &ComputeAccessSet) -> bool {
        Self::intersects(&left.write_output_ids, &right.write_output_ids)
            || Self::intersects(&left.write_output_ids, &right.read_output_ids)
            || Self::intersects(&left.read_output_ids, &right.write_output_ids)
            || Self::intersects(&left.write_object_ids, &right.write_object_ids)
            || Self::intersects(&left.write_object_ids, &right.read_object_ids)
            || Self::intersects(&left.read_object_ids, &right.write_object_ids)
    }
}

/// Planner turns a pending queue into executable groups.
pub trait ComputeBatchPlanner: Send + Sync {
    /// Plans a batch from pending transactions.
    fn plan(
        &self,
        pending: &[PendingComputeTx],
        store: &dyn ObjectStore,
    ) -> ComputeResult<ComputeBatchPlan>;
}

/// Default greedy planner.
pub struct DefaultComputeBatchPlanner<P> {
    conflict_policy: P,
}

impl<P> DefaultComputeBatchPlanner<P> {
    /// Creates a new planner.
    pub fn new(conflict_policy: P) -> Self {
        Self { conflict_policy }
    }
}

impl<P: ComputeConflictPolicy> ComputeBatchPlanner for DefaultComputeBatchPlanner<P> {
    fn plan(
        &self,
        pending: &[PendingComputeTx],
        store: &dyn ObjectStore,
    ) -> ComputeResult<ComputeBatchPlan> {
        let mut groups: Vec<ComputeBatchGroup> = Vec::new();
        let mut fallback: Vec<PendingComputeTx> = Vec::new();

        for item in pending.iter().cloned() {
            if !self.conflict_policy.can_batch(&item.tx) {
                fallback.push(item);
                continue;
            }

            let access = match self.conflict_policy.access_set(&item.tx, store) {
                Ok(access) => access,
                Err(_) => {
                    fallback.push(item);
                    continue;
                }
            };

            let planned = PlannedComputeTx {
                pending: item,
                access,
            };

            let mut placed = false;
            for group in &mut groups {
                if group.domain_id != planned.pending.domain_id {
                    continue;
                }
                if group.txs.iter().all(|existing| {
                    !self
                        .conflict_policy
                        .conflicts(&existing.access, &planned.access)
                }) {
                    group.txs.push(planned.clone());
                    placed = true;
                    break;
                }
            }

            if !placed {
                groups.push(ComputeBatchGroup {
                    domain_id: planned.pending.domain_id,
                    txs: vec![planned],
                });
            }
        }

        Ok(ComputeBatchPlan { groups, fallback })
    }
}

/// Batch runner interface.
pub trait ComputeBatchRunner: Send + Sync {
    /// Executes a whole plan.
    fn run_plan(&self, plan: ComputeBatchPlan) -> Vec<ComputeBatchOutcome>;
    /// Executes one group.
    fn run_group(&self, group: ComputeBatchGroup) -> Vec<ComputeBatchOutcome>;
    /// Executes one tx serially as a fallback path.
    fn run_serial(&self, pending: PendingComputeTx) -> ComputeBatchOutcome;
}

/// Parallel validator and serial committer.
pub struct ParallelComputeBatchRunner {
    store: Arc<dyn ObjectStore>,
    authorization: Arc<dyn AuthorizationPolicy>,
    resources: Arc<dyn ResourcePolicy>,
    domains: Arc<dyn DomainRegistry>,
    max_workers: usize,
}

impl ParallelComputeBatchRunner {
    /// Creates a runner over shared compute backends.
    pub fn new(
        store: Arc<dyn ObjectStore>,
        authorization: Arc<dyn AuthorizationPolicy>,
        resources: Arc<dyn ResourcePolicy>,
        domains: Arc<dyn DomainRegistry>,
    ) -> Self {
        Self {
            store,
            authorization,
            resources,
            domains,
            max_workers: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(DEFAULT_BATCH_WORKERS)
                .max(1),
        }
    }

    fn run_single(&self, tx: &ComputeTx) -> ComputeBatchOutcome {
        let executor = BasicTxExecutor::new(
            self.store.clone(),
            self.authorization.clone(),
            self.resources.clone(),
            self.domains.clone(),
        );

        match executor.execute(tx) {
            Ok(report) => ComputeBatchOutcome {
                tx_id: tx.tx_id,
                accepted: true,
                report: Some(report),
                created_outputs: tx.output_proposals.len(),
                error: None,
            },
            Err(err) => ComputeBatchOutcome {
                tx_id: tx.tx_id,
                accepted: false,
                report: None,
                created_outputs: tx.output_proposals.len(),
                error: Some(err),
            },
        }
    }
}

impl ComputeBatchRunner for ParallelComputeBatchRunner {
    fn run_plan(&self, plan: ComputeBatchPlan) -> Vec<ComputeBatchOutcome> {
        let mut outcomes = Vec::new();
        for group in plan.groups {
            outcomes.extend(self.run_group(group));
        }
        for pending in plan.fallback {
            outcomes.push(self.run_serial(pending));
        }
        outcomes
    }

    fn run_group(&self, group: ComputeBatchGroup) -> Vec<ComputeBatchOutcome> {
        if group.txs.is_empty() {
            return Vec::new();
        }

        if group.txs.len() == 1 {
            return vec![self.run_serial(group.txs[0].pending.clone())];
        }

        let group_len = group.txs.len();
        let results = Arc::new(parking_lot::Mutex::new(vec![None; group_len]));
        let queue = Arc::new(parking_lot::Mutex::new(
            group.txs.into_iter().enumerate().collect::<VecDeque<_>>(),
        ));
        let worker_count = self.max_workers.min(group_len).max(1);

        let mut workers = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let results = Arc::clone(&results);
            let queue = Arc::clone(&queue);
            let store = self.store.clone();
            let authorization = self.authorization.clone();
            let resources = self.resources.clone();
            let domains = self.domains.clone();

            workers.push(std::thread::spawn(move || loop {
                let item = queue.lock().pop_front();
                let Some((idx, planned)) = item else {
                    break;
                };

                let validator = BasicTxValidator {
                    store: &store,
                    authorization: &authorization,
                    resources: &resources,
                    domains: &domains,
                };

                let outcome = match validator.validate(&planned.pending.tx) {
                    Ok(report) => {
                        let executor = BasicTxExecutor::new(
                            store.clone(),
                            authorization.clone(),
                            resources.clone(),
                            domains.clone(),
                        );
                        match executor.commit_prevalidated(&planned.pending.tx, report) {
                            Ok(report) => ComputeBatchOutcome {
                                tx_id: planned.pending.tx_id,
                                accepted: true,
                                report: Some(report),
                                created_outputs: planned.pending.tx.output_proposals.len(),
                                error: None,
                            },
                            Err(err) => ComputeBatchOutcome {
                                tx_id: planned.pending.tx_id,
                                accepted: false,
                                report: None,
                                created_outputs: planned.pending.tx.output_proposals.len(),
                                error: Some(err),
                            },
                        }
                    }
                    Err(err) => ComputeBatchOutcome {
                        tx_id: planned.pending.tx_id,
                        accepted: false,
                        report: None,
                        created_outputs: planned.pending.tx.output_proposals.len(),
                        error: Some(err),
                    },
                };

                results.lock()[idx] = Some(outcome);
            }));
        }

        for worker in workers {
            worker.join().expect("batch worker panicked");
        }

        let mut guard = results.lock();
        let mut outcomes = Vec::with_capacity(group_len);
        for outcome in guard.iter_mut() {
            outcomes.push(outcome.take().expect("batch outcome missing"));
        }
        outcomes
    }

    fn run_serial(&self, pending: PendingComputeTx) -> ComputeBatchOutcome {
        self.run_single(&pending.tx)
    }
}

/// Execution service combining scheduler, planner and runner.
pub struct ComputeExecutionService {
    store: Arc<dyn ObjectStore>,
    scheduler: Arc<dyn ComputeScheduler>,
    planner: Arc<dyn ComputeBatchPlanner>,
    runner: Arc<dyn ComputeBatchRunner>,
    fallback_policy: Arc<dyn ComputeFallbackPolicy>,
    in_flight: Mutex<HashMap<TxId, Arc<InFlightExecution>>>,
    completed: RwLock<HashMap<TxId, ComputeBatchOutcome>>,
    completed_order: RwLock<VecDeque<TxId>>,
}

impl ComputeExecutionService {
    /// Creates a service.
    pub fn new(
        store: Arc<dyn ObjectStore>,
        scheduler: Arc<dyn ComputeScheduler>,
        planner: Arc<dyn ComputeBatchPlanner>,
        runner: Arc<dyn ComputeBatchRunner>,
        fallback_policy: Arc<dyn ComputeFallbackPolicy>,
    ) -> Self {
        Self {
            store,
            scheduler,
            planner,
            runner,
            fallback_policy,
            in_flight: Mutex::new(HashMap::new()),
            completed: RwLock::new(HashMap::new()),
            completed_order: RwLock::new(VecDeque::new()),
        }
    }

    /// Enqueues a tx.
    pub fn submit(
        &self,
        tx: ComputeTx,
    ) -> Result<super::scheduler::ComputeScheduleTicket, super::scheduler::ComputeScheduleError>
    {
        self.scheduler.submit(tx)
    }

    /// Flushes ready batches through planner and runner.
    pub fn flush_ready(&self) -> ComputeResult<Vec<ComputeBatchOutcome>> {
        let pending = self.scheduler.drain_ready();
        if pending.is_empty() {
            return Ok(Vec::new());
        }

        let mut outcomes = Vec::new();
        let batch_size = self.scheduler.config().max_batch_size.max(1);

        for chunk in pending.chunks(batch_size) {
            let chunk_outcomes = match self.planner.plan(chunk, self.store.as_ref()) {
                Ok(plan) => self.runner.run_plan(plan),
                Err(err) => match self.fallback_policy.on_plan_error(chunk, &err) {
                    ComputeFallbackDisposition::Reject => return Err(err),
                    ComputeFallbackDisposition::RunSerial => chunk
                        .iter()
                        .cloned()
                        .map(|pending| self.runner.run_serial(pending))
                        .collect(),
                },
            };
            self.record_outcomes(&chunk_outcomes);
            outcomes.extend(chunk_outcomes);
        }

        Ok(outcomes)
    }

    /// Submit one tx, wait for the batch window, then return its outcome.
    pub async fn submit_and_run(&self, tx: ComputeTx) -> ComputeResult<ComputeBatchOutcome> {
        let tx_id = tx.tx_id;
        if let Some(outcome) = self.completed.read().get(&tx_id).cloned() {
            return Ok(outcome);
        }

        let lane_key = self.scheduler.config().lane_strategy.lane_key(&tx);
        let pending = PendingComputeTx::new(tx.clone(), lane_key, Instant::now());
        let inflight = self.inflight_entry(tx_id);
        if !inflight.1 {
            return inflight.0.wait().await;
        }

        if let Some(outcome) = self.completed.read().get(&tx_id).cloned() {
            self.complete_inflight(tx_id, &inflight.0, Ok(outcome.clone()));
            return Ok(outcome);
        }

        let ticket = match self.submit(tx.clone()) {
            Ok(ticket) => ticket,
            Err(err) => match self.fallback_policy.on_schedule_reject(&tx, &err) {
                ComputeFallbackDisposition::Reject => {
                    let error = ComputeError::InvalidOperation(err.to_string());
                    self.complete_inflight(tx_id, &inflight.0, Err(error.clone()));
                    return Err(error);
                }
                ComputeFallbackDisposition::RunSerial => {
                    let outcome = self.runner.run_serial(pending);
                    self.record_outcomes(std::slice::from_ref(&outcome));
                    return Ok(outcome);
                }
            },
        };

        let wait_ms = self.scheduler.config().batch_window_ms;
        if wait_ms > 0 {
            tokio::time::sleep(Duration::from_millis(wait_ms)).await;
        }

        if let Err(err) = self.flush_ready() {
            if let Some(outcome) = self.completed.read().get(&tx_id).cloned() {
                self.complete_inflight(tx_id, &inflight.0, Ok(outcome.clone()));
                return Ok(outcome);
            }
            self.complete_inflight(tx_id, &inflight.0, Err(err.clone()));
            return Err(err);
        }

        if let Some(outcome) = self.completed.read().get(&tx_id).cloned() {
            self.complete_inflight(tx_id, &inflight.0, Ok(outcome.clone()));
            return Ok(outcome);
        }

        let stability_wait = self.stability_wait_duration();
        if !stability_wait.is_zero() {
            tokio::time::sleep(stability_wait).await;
        }

        if let Some(outcome) = self.completed.read().get(&tx_id).cloned() {
            self.complete_inflight(tx_id, &inflight.0, Ok(outcome.clone()));
            return Ok(outcome);
        }

        if self.scheduler.take_pending(tx_id).is_none() {
            let waited = inflight.0.wait().await?;
            self.complete_inflight(tx_id, &inflight.0, Ok(waited.clone()));
            return Ok(waited);
        }

        match self.fallback_policy.on_missing_outcome(&tx) {
            ComputeFallbackDisposition::Reject => {
                let error = ComputeError::InvalidOperation(format!(
                    "compute outcome missing after submit for {}",
                    hex::encode(ticket.tx_id.0.as_bytes())
                ));
                self.complete_inflight(tx_id, &inflight.0, Err(error.clone()));
                Err(error)
            }
            ComputeFallbackDisposition::RunSerial => {
                let outcome = self.runner.run_serial(pending);
                self.record_outcomes(std::slice::from_ref(&outcome));
                self.complete_inflight(tx_id, &inflight.0, Ok(outcome.clone()));
                Ok(outcome)
            }
        }
    }

    fn inflight_entry(&self, tx_id: TxId) -> (Arc<InFlightExecution>, bool) {
        let mut inflight = self.in_flight.lock();
        if let Some(entry) = inflight.get(&tx_id) {
            return (entry.clone(), false);
        }

        let entry = Arc::new(InFlightExecution::new());
        inflight.insert(tx_id, entry.clone());
        (entry, true)
    }

    fn complete_inflight(
        &self,
        tx_id: TxId,
        entry: &Arc<InFlightExecution>,
        outcome: ComputeResult<ComputeBatchOutcome>,
    ) {
        entry.complete(outcome);
        let mut inflight = self.in_flight.lock();
        if let Some(current) = inflight.get(&tx_id) {
            if Arc::ptr_eq(current, entry) {
                inflight.remove(&tx_id);
            }
        }
    }

    fn stability_wait_duration(&self) -> Duration {
        let wait_ms = self.scheduler.config().batch_window_ms;
        Duration::from_millis(
            wait_ms.clamp(POST_FLUSH_STABILITY_MIN_MS, POST_FLUSH_STABILITY_MAX_MS),
        )
    }

    fn record_outcomes(&self, outcomes: &[ComputeBatchOutcome]) {
        for outcome in outcomes {
            let _ = self.scheduler.take_pending(outcome.tx_id);
        }

        let completions: Vec<_> = {
            let inflight = self.in_flight.lock();
            outcomes
                .iter()
                .filter_map(|outcome| {
                    inflight
                        .get(&outcome.tx_id)
                        .cloned()
                        .map(|entry| (outcome.tx_id, entry, outcome.clone()))
                })
                .collect()
        };

        {
            let mut map = self.completed.write();
            let mut order = self.completed_order.write();
            for outcome in outcomes {
                order.retain(|tx_id| tx_id != &outcome.tx_id);
                order.push_back(outcome.tx_id);
                map.insert(outcome.tx_id, outcome.clone());
            }
            while order.len() > MAX_COMPLETED_OUTCOMES {
                if let Some(tx_id) = order.pop_front() {
                    map.remove(&tx_id);
                }
            }
        }

        for (tx_id, entry, outcome) in completions {
            self.complete_inflight(tx_id, &entry, Ok(outcome));
        }
    }

    #[cfg(test)]
    fn take_pending_for_test(&self, tx_id: TxId) -> Option<PendingComputeTx> {
        self.scheduler.take_pending(tx_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compute::{
        domain::{DomainConfig, InMemoryDomainRegistry},
        object::{ObjectKind, Ownership, Script},
        primitives::{DomainId, ObjectId, OutputId, Version},
        tx::{Command, OutputProposal, TxSignature, TxWitness},
        ComputeLaneStrategy, ComputeScheduleError, ComputeScheduleTicket, ComputeScheduler,
        ComputeSchedulerConfig, InMemoryObjectStore,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc,
    };
    use tokio::sync::oneshot;

    fn build_output(
        domain_id: DomainId,
        object_seed: u8,
        output_seed: u8,
    ) -> crate::compute::ObjectOutput {
        crate::compute::ObjectOutput {
            output_id: OutputId(crate::crypto::Hash::from_bytes([output_seed; 32])),
            object_id: ObjectId(crate::crypto::Hash::from_bytes([object_seed; 32])),
            version: Version(1),
            domain_id,
            kind: ObjectKind::Asset,
            owner: Ownership::Shared,
            predecessor: None,
            state: vec![output_seed],
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
        }
    }

    fn build_tx(
        tx_seed: u8,
        domain_id: DomainId,
        input: OutputId,
        input_object: ObjectId,
        output_seed: u8,
    ) -> ComputeTx {
        ComputeTx {
            tx_id: crate::TxId(crate::crypto::Hash::from_bytes([tx_seed; 32])),
            domain_id,
            command: Command::Transfer,
            input_set: vec![input],
            read_set: vec![],
            output_proposals: vec![OutputProposal {
                output_id: OutputId(crate::crypto::Hash::from_bytes([output_seed; 32])),
                object_id: input_object,
                domain_id,
                kind: ObjectKind::Asset,
                owner: Ownership::Shared,
                predecessor: Some(input),
                version: Version(2),
                state: vec![output_seed],
                state_root: None,
                resources: vec![],
                lock: Script::default(),
                logic: None,
                created_at: 1,
                ttl: None,
                rent_reserve: None,
                flags: 0,
                extensions: vec![],
            }],
            fee: 0,
            nonce: Some(1),
            metadata: vec![],
            payload: vec![],
            deadline_unix_secs: None,
            chain_id: None,
            network_id: None,
            witness: TxWitness {
                signatures: vec![TxSignature::ed25519([1; 64], [2; 32])],
                threshold: None,
            },
        }
    }

    fn build_mint_tx(seed: u8) -> ComputeTx {
        let mut tx = ComputeTx {
            tx_id: crate::TxId(crate::crypto::Hash::from_bytes([seed; 32])),
            domain_id: DomainId(0),
            command: Command::Mint,
            input_set: vec![],
            read_set: vec![],
            output_proposals: vec![OutputProposal {
                output_id: OutputId(crate::crypto::Hash::from_bytes([seed.wrapping_add(20); 32])),
                object_id: ObjectId(crate::crypto::Hash::from_bytes([seed.wrapping_add(10); 32])),
                domain_id: DomainId(0),
                kind: ObjectKind::State,
                owner: Ownership::Shared,
                predecessor: None,
                version: Version(1),
                state: vec![seed],
                state_root: None,
                resources: vec![],
                lock: Script::default(),
                logic: None,
                created_at: 1,
                ttl: None,
                rent_reserve: None,
                flags: 0,
                extensions: vec![],
            }],
            fee: 0,
            nonce: Some(u64::from(seed) + 1),
            metadata: vec![],
            payload: vec![],
            deadline_unix_secs: None,
            chain_id: None,
            network_id: None,
            witness: TxWitness {
                signatures: vec![TxSignature::ed25519([1; 64], [2; 32])],
                threshold: Some(1),
            },
        };
        tx.assign_expected_tx_id();
        tx
    }

    fn build_store() -> (
        Arc<InMemoryObjectStore>,
        Arc<InMemoryDomainRegistry>,
        ComputeAccessSet,
    ) {
        let store = Arc::new(InMemoryObjectStore::new());
        let domains = Arc::new(InMemoryDomainRegistry::new());
        domains.upsert_domain(DomainConfig {
            domain_id: DomainId(0),
            name: "main".to_string(),
            vm: "wasm".to_string(),
            public: true,
        });

        let out = build_output(DomainId(0), 9, 1);
        store.insert_output(out.clone()).unwrap();

        let mut access = ComputeAccessSet::default();
        access.domain_id = DomainId(0);
        access.write_output_ids.insert(out.output_id);
        access.write_object_ids.insert(out.object_id);

        (store, domains, access)
    }

    #[test]
    fn conflict_policy_flags_same_object_as_conflict() {
        let (store, _domains, _access) = build_store();
        let policy = DefaultComputeConflictPolicy;
        let tx_a = build_tx(
            1,
            DomainId(0),
            OutputId(crate::crypto::Hash::from_bytes([1; 32])),
            ObjectId(crate::crypto::Hash::from_bytes([9; 32])),
            2,
        );
        let tx_b = build_tx(
            2,
            DomainId(0),
            OutputId(crate::crypto::Hash::from_bytes([1; 32])),
            ObjectId(crate::crypto::Hash::from_bytes([9; 32])),
            3,
        );

        let access_a = policy.access_set(&tx_a, store.as_ref()).unwrap();
        let access_b = policy.access_set(&tx_b, store.as_ref()).unwrap();
        assert!(policy.conflicts(&access_a, &access_b));
    }

    #[test]
    fn fallback_policy_distinguishes_reject_and_serial() {
        let tx = build_tx(
            3,
            DomainId(0),
            OutputId(crate::crypto::Hash::from_bytes([4; 32])),
            ObjectId(crate::crypto::Hash::from_bytes([5; 32])),
            6,
        );
        let err = ComputeScheduleError::QueueFull;

        assert_eq!(
            DisabledComputeFallbackPolicy.on_schedule_reject(&tx, &err),
            ComputeFallbackDisposition::Reject
        );
        assert_eq!(
            SerialComputeFallbackPolicy.on_schedule_reject(&tx, &err),
            ComputeFallbackDisposition::RunSerial
        );
    }

    #[test]
    fn parallel_batch_runner_executes_non_conflicting_group() {
        let store = Arc::new(InMemoryObjectStore::new());
        let domains = Arc::new(InMemoryDomainRegistry::new());
        domains.upsert_domain(DomainConfig {
            domain_id: DomainId(0),
            name: "main".to_string(),
            vm: "wasm".to_string(),
            public: true,
        });

        let authorization = Arc::new(crate::compute::policy::DefaultAuthorizationPolicy);
        let resources = Arc::new(crate::compute::policy::NoopResourcePolicy);

        let mut txs = Vec::new();
        let mut pending = Vec::new();
        for seed in 1u8..=4 {
            let tx = build_mint_tx(seed);
            pending.push(PendingComputeTx::new(
                tx.clone(),
                ComputeLaneStrategy::ByDomain.lane_key(&tx),
                Instant::now(),
            ));
            txs.push(tx);
        }

        let planner = DefaultComputeBatchPlanner::new(DefaultComputeConflictPolicy);
        let plan = planner.plan(&pending, store.as_ref()).unwrap();
        assert_eq!(plan.groups.len(), 1);

        let runner =
            ParallelComputeBatchRunner::new(store.clone(), authorization, resources, domains);
        let outcomes = runner.run_plan(plan);

        assert_eq!(outcomes.len(), txs.len());
        assert!(outcomes.iter().all(|outcome| outcome.accepted));
        for tx in txs {
            assert!(
                store.get_output(tx.output_proposals[0].output_id).is_some(),
                "missing output for tx {}",
                hex::encode(tx.tx_id.0.as_bytes())
            );
        }
    }

    struct StalledScheduler {
        config: ComputeSchedulerConfig,
        submitted: AtomicUsize,
        pending: Mutex<VecDeque<PendingComputeTx>>,
    }

    impl StalledScheduler {
        fn new(batch_window_ms: u64) -> Self {
            Self {
                config: ComputeSchedulerConfig {
                    batch_window_ms,
                    max_batch_size: 8,
                    max_pending: 16,
                    lane_strategy: Arc::new(ComputeLaneStrategy::ByDomain),
                },
                submitted: AtomicUsize::new(0),
                pending: Mutex::new(VecDeque::new()),
            }
        }
    }

    impl ComputeScheduler for StalledScheduler {
        fn submit(&self, tx: ComputeTx) -> Result<ComputeScheduleTicket, ComputeScheduleError> {
            self.submitted.fetch_add(1, Ordering::SeqCst);
            let lane_key = self.config.lane_strategy.lane_key(&tx);
            let pending = PendingComputeTx::new(tx.clone(), lane_key.clone(), Instant::now());
            self.pending.lock().push_back(pending.clone());
            Ok(ComputeScheduleTicket {
                tx_id: tx.tx_id,
                domain_id: tx.domain_id,
                lane_key,
                accepted_at_unix_secs: 0,
                queue_depth: 1,
            })
        }

        fn drain_ready(&self) -> Vec<PendingComputeTx> {
            Vec::new()
        }

        fn take_pending(&self, tx_id: TxId) -> Option<PendingComputeTx> {
            let mut pending = self.pending.lock();
            let pos = pending.iter().position(|item| item.tx_id == tx_id)?;
            pending.remove(pos)
        }

        fn pending_len(&self) -> usize {
            0
        }

        fn config(&self) -> ComputeSchedulerConfig {
            self.config.clone()
        }
    }

    struct NoopPlanner;

    impl ComputeBatchPlanner for NoopPlanner {
        fn plan(
            &self,
            _pending: &[PendingComputeTx],
            _store: &dyn ObjectStore,
        ) -> ComputeResult<ComputeBatchPlan> {
            Ok(ComputeBatchPlan::default())
        }
    }

    struct CountingRunner {
        serial_calls: AtomicUsize,
    }

    impl CountingRunner {
        fn new() -> Self {
            Self {
                serial_calls: AtomicUsize::new(0),
            }
        }
    }

    impl ComputeBatchRunner for CountingRunner {
        fn run_plan(&self, _plan: ComputeBatchPlan) -> Vec<ComputeBatchOutcome> {
            Vec::new()
        }

        fn run_group(&self, _group: ComputeBatchGroup) -> Vec<ComputeBatchOutcome> {
            Vec::new()
        }

        fn run_serial(&self, pending: PendingComputeTx) -> ComputeBatchOutcome {
            self.serial_calls.fetch_add(1, Ordering::SeqCst);
            ComputeBatchOutcome {
                tx_id: pending.tx_id,
                accepted: true,
                report: Some(ValidationReport {
                    inputs: Vec::new(),
                    reads: Vec::new(),
                }),
                created_outputs: pending.tx.output_proposals.len(),
                error: None,
            }
        }
    }

    struct CommitCountingRunner {
        serial_calls: AtomicUsize,
        committed_calls: AtomicUsize,
    }

    impl CommitCountingRunner {
        fn new() -> Self {
            Self {
                serial_calls: AtomicUsize::new(0),
                committed_calls: AtomicUsize::new(0),
            }
        }
    }

    impl ComputeBatchRunner for CommitCountingRunner {
        fn run_plan(&self, _plan: ComputeBatchPlan) -> Vec<ComputeBatchOutcome> {
            Vec::new()
        }

        fn run_group(&self, _group: ComputeBatchGroup) -> Vec<ComputeBatchOutcome> {
            Vec::new()
        }

        fn run_serial(&self, pending: PendingComputeTx) -> ComputeBatchOutcome {
            self.serial_calls.fetch_add(1, Ordering::SeqCst);
            let outcome = ComputeBatchOutcome {
                tx_id: pending.tx_id,
                accepted: true,
                report: Some(ValidationReport {
                    inputs: Vec::new(),
                    reads: Vec::new(),
                }),
                created_outputs: pending.tx.output_proposals.len(),
                error: None,
            };
            self.committed_calls.fetch_add(1, Ordering::SeqCst);
            outcome
        }
    }

    struct BlockingCountingRunner {
        serial_calls: AtomicUsize,
        started: Mutex<Option<oneshot::Sender<()>>>,
        release_rx: Mutex<mpsc::Receiver<()>>,
    }

    impl BlockingCountingRunner {
        fn new(started: oneshot::Sender<()>, release_rx: mpsc::Receiver<()>) -> Self {
            Self {
                serial_calls: AtomicUsize::new(0),
                started: Mutex::new(Some(started)),
                release_rx: Mutex::new(release_rx),
            }
        }
    }

    impl ComputeBatchRunner for BlockingCountingRunner {
        fn run_plan(&self, _plan: ComputeBatchPlan) -> Vec<ComputeBatchOutcome> {
            Vec::new()
        }

        fn run_group(&self, _group: ComputeBatchGroup) -> Vec<ComputeBatchOutcome> {
            Vec::new()
        }

        fn run_serial(&self, pending: PendingComputeTx) -> ComputeBatchOutcome {
            self.serial_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(started) = self.started.lock().take() {
                let _ = started.send(());
            }
            self.release_rx
                .lock()
                .recv()
                .expect("missing release signal for blocking runner");
            ComputeBatchOutcome {
                tx_id: pending.tx_id,
                accepted: true,
                report: Some(ValidationReport {
                    inputs: Vec::new(),
                    reads: Vec::new(),
                }),
                created_outputs: pending.tx.output_proposals.len(),
                error: None,
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn submit_and_run_only_falls_back_once_for_concurrent_same_tx() {
        let store = Arc::new(InMemoryObjectStore::new());
        let scheduler = Arc::new(StalledScheduler::new(20));
        let planner = Arc::new(NoopPlanner);
        let runner = Arc::new(CountingRunner::new());
        let service = Arc::new(ComputeExecutionService::new(
            store,
            scheduler.clone(),
            planner,
            runner.clone(),
            ComputeFallbackMode::SerialOnFailure.build_policy(),
        ));
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let tx = build_mint_tx(9);

        let service_a = service.clone();
        let barrier_a = barrier.clone();
        let tx_a = tx.clone();
        let task_a = tokio::spawn(async move {
            barrier_a.wait().await;
            service_a.submit_and_run(tx_a).await
        });

        let service_b = service.clone();
        let barrier_b = barrier.clone();
        let tx_b = tx.clone();
        let task_b = tokio::spawn(async move {
            barrier_b.wait().await;
            service_b.submit_and_run(tx_b).await
        });

        let (res_a, res_b) = tokio::time::timeout(Duration::from_secs(5), async move {
            let res_a = task_a.await.expect("task a panicked");
            let res_b = task_b.await.expect("task b panicked");
            (res_a, res_b)
        })
        .await
        .expect("submit_and_run tasks timed out");

        let outcome_a = res_a.expect("submit_and_run a failed");
        let outcome_b = res_b.expect("submit_and_run b failed");

        assert!(outcome_a.accepted);
        assert!(outcome_b.accepted);
        assert_eq!(outcome_a.tx_id, tx.tx_id);
        assert_eq!(outcome_b.tx_id, tx.tx_id);
        assert_eq!(scheduler.submitted.load(Ordering::SeqCst), 1);
        assert_eq!(runner.serial_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn submit_and_run_survives_high_concurrency_same_tx_storm() {
        let store = Arc::new(InMemoryObjectStore::new());
        let scheduler = Arc::new(StalledScheduler::new(20));
        let planner = Arc::new(NoopPlanner);
        let (release_tx, release_rx) = mpsc::channel();
        let (started_tx, started_rx) = oneshot::channel();
        let runner = Arc::new(BlockingCountingRunner::new(started_tx, release_rx));
        let service = Arc::new(ComputeExecutionService::new(
            store,
            scheduler.clone(),
            planner,
            runner.clone(),
            ComputeFallbackMode::SerialOnFailure.build_policy(),
        ));

        let tx = build_mint_tx(11);
        let task_count = 32usize;
        let barrier = Arc::new(tokio::sync::Barrier::new(task_count + 1));
        let mut handles = Vec::with_capacity(task_count);

        for _ in 0..task_count {
            let service = service.clone();
            let barrier = barrier.clone();
            let tx = tx.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                service.submit_and_run(tx).await
            }));
        }

        barrier.wait().await;
        started_rx
            .await
            .expect("runner never entered serial fallback");

        assert_eq!(scheduler.submitted.load(Ordering::SeqCst), 1);
        assert_eq!(runner.serial_calls.load(Ordering::SeqCst), 1);

        release_tx
            .send(())
            .expect("failed to release blocking serial runner");

        let results = tokio::time::timeout(Duration::from_secs(5), async {
            let mut results = Vec::with_capacity(task_count);
            for handle in handles {
                results.push(handle.await.expect("task panicked"));
            }
            results
        })
        .await
        .expect("submit_and_run storm timed out");

        assert_eq!(scheduler.submitted.load(Ordering::SeqCst), 1);
        assert_eq!(runner.serial_calls.load(Ordering::SeqCst), 1);
        assert_eq!(results.len(), task_count);
        for outcome in results {
            let outcome = outcome.expect("submit_and_run failed");
            assert!(outcome.accepted);
            assert_eq!(outcome.tx_id, tx.tx_id);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn submit_and_run_does_not_double_commit_after_serial_fallback_window() {
        let store = Arc::new(InMemoryObjectStore::new());
        let scheduler = Arc::new(StalledScheduler::new(20));
        let planner = Arc::new(NoopPlanner);
        let runner = Arc::new(CommitCountingRunner::new());
        let service = Arc::new(ComputeExecutionService::new(
            store,
            scheduler.clone(),
            planner,
            runner.clone(),
            ComputeFallbackMode::SerialOnFailure.build_policy(),
        ));

        let tx = build_mint_tx(13);
        let tx_id = tx.tx_id;
        let pending = PendingComputeTx::new(
            tx.clone(),
            ComputeLaneStrategy::ByDomain.lane_key(&tx),
            Instant::now(),
        );

        scheduler
            .submit(tx.clone())
            .expect("scheduler submit should work");
        let serial_outcome = runner.run_serial(pending);
        service.record_outcomes(std::slice::from_ref(&serial_outcome));

        let outcome = service
            .submit_and_run(tx.clone())
            .await
            .expect("submit_and_run should resolve from completed cache");

        assert!(outcome.accepted);
        assert_eq!(outcome.tx_id, tx_id);
        assert_eq!(runner.serial_calls.load(Ordering::SeqCst), 1);
        assert_eq!(runner.committed_calls.load(Ordering::SeqCst), 1);
        assert!(service.take_pending_for_test(tx_id).is_none());
    }
}
