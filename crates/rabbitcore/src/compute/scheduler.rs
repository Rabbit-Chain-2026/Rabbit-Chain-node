//! In-memory compute scheduler for batched execution.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    primitives::{DomainId, ObjectId, OutputId, TxId},
    tx::ComputeTx,
};

/// Lane key strategy abstraction.
pub trait ComputeLaneKeyStrategy: Send + Sync {
    /// Returns the lane key for a tx.
    fn lane_key(&self, tx: &ComputeTx) -> String;
}

/// Lane selection policy for batching.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeLaneStrategy {
    /// All transactions share the same lane.
    SingleLane,
    /// Transactions are partitioned by domain.
    ByDomain,
    /// Transactions are partitioned by domain and their first touched resource.
    ByDomainAndTouch,
}

impl Default for ComputeLaneStrategy {
    fn default() -> Self {
        Self::ByDomain
    }
}

impl ComputeLaneStrategy {
    /// Returns the lane key for a tx.
    pub fn lane_key(self, tx: &ComputeTx) -> String {
        match self {
            Self::SingleLane => "single".to_string(),
            Self::ByDomain => format!("domain:{}", tx.domain_id.0),
            Self::ByDomainAndTouch => {
                let touch = tx
                    .input_set
                    .first()
                    .map(|id| format_output_id(*id))
                    .or_else(|| {
                        tx.read_set
                            .first()
                            .map(|read| format_output_id(read.output_id))
                    })
                    .or_else(|| {
                        tx.output_proposals
                            .first()
                            .map(|proposal| format_object_id(proposal.object_id))
                    })
                    .unwrap_or_else(|| format!("tx:{}", hex::encode(tx.tx_id.0.as_bytes())));
                format!("domain:{}:{}", tx.domain_id.0, touch)
            }
        }
    }
}

impl ComputeLaneKeyStrategy for ComputeLaneStrategy {
    fn lane_key(&self, tx: &ComputeTx) -> String {
        (*self).lane_key(tx)
    }
}

/// Scheduler tuning knobs.
#[derive(Clone)]
pub struct ComputeSchedulerConfig {
    /// Wait this long before a pending tx becomes batch-ready.
    pub batch_window_ms: u64,
    /// Upper bound for one batch execution slice.
    pub max_batch_size: usize,
    /// Maximum number of pending txs held in memory.
    pub max_pending: usize,
    /// Lane partitioning strategy.
    pub lane_strategy: Arc<dyn ComputeLaneKeyStrategy>,
}

impl Default for ComputeSchedulerConfig {
    fn default() -> Self {
        Self {
            batch_window_ms: 15,
            max_batch_size: 64,
            max_pending: 4_096,
            lane_strategy: Arc::new(ComputeLaneStrategy::ByDomain),
        }
    }
}

/// Admission ticket returned by the scheduler.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeScheduleTicket {
    /// Transaction id.
    pub tx_id: TxId,
    /// Domain id.
    pub domain_id: DomainId,
    /// Lane key used for scheduling.
    pub lane_key: String,
    /// Accepted timestamp.
    pub accepted_at_unix_secs: u64,
    /// Queue depth after enqueue.
    pub queue_depth: usize,
}

/// Pending tx stored in the scheduler queue.
#[derive(Clone, Debug)]
pub struct PendingComputeTx {
    /// Transaction payload.
    pub tx: ComputeTx,
    /// Transaction id.
    pub tx_id: TxId,
    /// Domain id.
    pub domain_id: DomainId,
    /// Lane key used for scheduling.
    pub lane_key: String,
    /// Enqueue timestamp.
    pub enqueued_at: Instant,
}

impl PendingComputeTx {
    /// Creates a queued tx wrapper.
    pub fn new(tx: ComputeTx, lane_key: String, enqueued_at: Instant) -> Self {
        let tx_id = tx.tx_id;
        let domain_id = tx.domain_id;
        Self {
            tx,
            tx_id,
            domain_id,
            lane_key,
            enqueued_at,
        }
    }
}

/// Scheduler errors.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ComputeScheduleError {
    /// Pending queue is full.
    #[error("compute scheduler queue is full")]
    QueueFull,
}

/// In-memory scheduler interface.
pub trait ComputeScheduler: Send + Sync {
    /// Enqueues a tx.
    fn submit(&self, tx: ComputeTx) -> Result<ComputeScheduleTicket, ComputeScheduleError>;
    /// Drains ready txs in FIFO order.
    fn drain_ready(&self) -> Vec<PendingComputeTx>;
    /// Removes a pending tx by id if it is still queued.
    fn take_pending(&self, tx_id: TxId) -> Option<PendingComputeTx>;
    /// Returns the current pending length.
    fn pending_len(&self) -> usize;
    /// Returns the active scheduler config.
    fn config(&self) -> ComputeSchedulerConfig;
}

/// Simple in-memory FIFO scheduler.
pub struct InMemoryComputeScheduler {
    config: ComputeSchedulerConfig,
    queue: Mutex<BTreeMap<String, VecDeque<PendingComputeTx>>>,
}

impl InMemoryComputeScheduler {
    /// Creates a scheduler with the provided config.
    pub fn new(config: ComputeSchedulerConfig) -> Self {
        Self {
            config,
            queue: Mutex::new(BTreeMap::new()),
        }
    }

    fn batch_window(&self) -> Duration {
        Duration::from_millis(self.config.batch_window_ms)
    }

    fn lane_key(&self, tx: &ComputeTx) -> String {
        self.config.lane_strategy.lane_key(tx)
    }

    fn pending_len_locked(queue: &BTreeMap<String, VecDeque<PendingComputeTx>>) -> usize {
        queue.values().map(VecDeque::len).sum()
    }
}

fn current_unix_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl ComputeScheduler for InMemoryComputeScheduler {
    fn submit(&self, tx: ComputeTx) -> Result<ComputeScheduleTicket, ComputeScheduleError> {
        let mut queue = self.queue.lock();
        if Self::pending_len_locked(&queue) >= self.config.max_pending {
            return Err(ComputeScheduleError::QueueFull);
        }

        let lane_key = self.lane_key(&tx);
        let enqueued_at = Instant::now();
        let pending = PendingComputeTx::new(tx, lane_key.clone(), enqueued_at);
        queue
            .entry(lane_key.clone())
            .or_default()
            .push_back(pending.clone());

        Ok(ComputeScheduleTicket {
            tx_id: pending.tx_id,
            domain_id: pending.domain_id,
            lane_key,
            accepted_at_unix_secs: current_unix_secs(),
            queue_depth: Self::pending_len_locked(&queue),
        })
    }

    fn drain_ready(&self) -> Vec<PendingComputeTx> {
        let now = Instant::now();
        let batch_window = self.batch_window();
        let mut queue = self.queue.lock();
        let mut ready = Vec::new();

        let lane_keys = queue.keys().cloned().collect::<Vec<_>>();
        for lane_key in lane_keys {
            let Some(lane_queue) = queue.get_mut(&lane_key) else {
                continue;
            };

            while let Some(front) = lane_queue.front() {
                let aged_enough = now.duration_since(front.enqueued_at) >= batch_window;
                if !aged_enough {
                    break;
                }
                if let Some(item) = lane_queue.pop_front() {
                    ready.push(item);
                }
            }
        }

        queue.retain(|_, lane_queue| !lane_queue.is_empty());
        ready
    }

    fn take_pending(&self, tx_id: TxId) -> Option<PendingComputeTx> {
        let mut queue = self.queue.lock();
        let mut removed = None;

        for lane_queue in queue.values_mut() {
            if let Some(pos) = lane_queue.iter().position(|pending| pending.tx_id == tx_id) {
                removed = lane_queue.remove(pos);
                break;
            }
        }

        queue.retain(|_, lane_queue| !lane_queue.is_empty());
        removed
    }

    fn pending_len(&self) -> usize {
        Self::pending_len_locked(&self.queue.lock())
    }

    fn config(&self) -> ComputeSchedulerConfig {
        self.config.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_rejects_when_full() {
        let scheduler = InMemoryComputeScheduler::new(ComputeSchedulerConfig {
            batch_window_ms: 0,
            max_batch_size: 1,
            max_pending: 1,
            lane_strategy: Arc::new(ComputeLaneStrategy::ByDomain),
        });

        let tx = ComputeTx {
            tx_id: TxId(crate::crypto::Hash::from_bytes([1; 32])),
            domain_id: DomainId(0),
            command: super::super::tx::Command::Burn,
            input_set: vec![],
            read_set: vec![],
            output_proposals: vec![],
            fee: 0,
            nonce: Some(1),
            metadata: vec![],
            payload: vec![],
            deadline_unix_secs: None,
            chain_id: None,
            network_id: None,
            witness: super::super::tx::TxWitness {
                signatures: vec![],
                threshold: None,
            },
        };

        assert!(scheduler.submit(tx.clone()).is_ok());
        assert_eq!(
            scheduler.submit(tx).err(),
            Some(ComputeScheduleError::QueueFull)
        );
    }

    #[test]
    fn lane_strategy_uses_touch_key_when_enabled() {
        let strategy = ComputeLaneStrategy::ByDomainAndTouch;
        let tx = ComputeTx {
            tx_id: TxId(crate::crypto::Hash::from_bytes([2; 32])),
            domain_id: DomainId(7),
            command: super::super::tx::Command::Burn,
            input_set: vec![OutputId(crate::crypto::Hash::from_bytes([3; 32]))],
            read_set: vec![],
            output_proposals: vec![],
            fee: 0,
            nonce: Some(1),
            metadata: vec![],
            payload: vec![],
            deadline_unix_secs: None,
            chain_id: None,
            network_id: None,
            witness: super::super::tx::TxWitness {
                signatures: vec![],
                threshold: None,
            },
        };

        let lane_key = strategy.lane_key(&tx);
        assert!(lane_key.contains("domain:7"));
        assert!(lane_key.contains("out:") || lane_key.contains("tx:") || lane_key.contains("in:"));
    }

    #[derive(Default)]
    struct PrefixLaneStrategy;

    impl ComputeLaneKeyStrategy for PrefixLaneStrategy {
        fn lane_key(&self, tx: &ComputeTx) -> String {
            format!("custom:{}", hex::encode(tx.tx_id.0.as_bytes()))
        }
    }

    #[test]
    fn scheduler_accepts_custom_lane_strategy_object() {
        let scheduler = InMemoryComputeScheduler::new(ComputeSchedulerConfig {
            batch_window_ms: 0,
            max_batch_size: 8,
            max_pending: 8,
            lane_strategy: Arc::new(PrefixLaneStrategy),
        });

        let tx = ComputeTx {
            tx_id: TxId(crate::crypto::Hash::from_bytes([4; 32])),
            domain_id: DomainId(3),
            command: super::super::tx::Command::Burn,
            input_set: vec![],
            read_set: vec![],
            output_proposals: vec![],
            fee: 0,
            nonce: Some(1),
            metadata: vec![],
            payload: vec![],
            deadline_unix_secs: None,
            chain_id: None,
            network_id: None,
            witness: super::super::tx::TxWitness {
                signatures: vec![],
                threshold: None,
            },
        };

        let ticket = scheduler.submit(tx).expect("submit should work");
        assert!(ticket.lane_key.starts_with("custom:"));
    }
}

fn format_object_id(object_id: ObjectId) -> String {
    format!("obj:{}", hex::encode(object_id.0.as_bytes()))
}

fn format_output_id(output_id: OutputId) -> String {
    format!("out:{}", hex::encode(output_id.0.as_bytes()))
}
