//! Agent scheduling primitives.

use std::collections::VecDeque;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::primitives::{DomainId, ObjectId};

/// Agent specification persisted in agent object state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSpec {
    /// Agent object id.
    pub agent_id: ObjectId,
    /// Domain where the agent is scheduled.
    pub domain_id: DomainId,
    /// Trigger interval in seconds.
    pub interval_secs: u64,
    /// Next execution timestamp.
    pub next_tick_unix_secs: u64,
    /// Maximum retry count on failure.
    pub max_retries: u8,
}

/// Scheduled agent task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTask {
    /// Agent spec identifier.
    pub agent_id: ObjectId,
    /// Domain id.
    pub domain_id: DomainId,
    /// Scheduled fire time.
    pub execute_at_unix_secs: u64,
}

/// Agent scheduler abstraction.
pub trait AgentScheduler: Send + Sync {
    /// Enqueue a task.
    fn schedule(&self, task: AgentTask);
    /// Dequeue next task if any.
    fn pop_next(&self) -> Option<AgentTask>;
    /// Current queue size.
    fn len(&self) -> usize;

    /// Whether queue is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// In-memory FIFO scheduler for scaffolding.
#[derive(Default)]
pub struct InMemoryAgentScheduler {
    queue: Mutex<VecDeque<AgentTask>>,
}

impl InMemoryAgentScheduler {
    /// Creates an empty scheduler.
    pub fn new() -> Self {
        Self::default()
    }
}

impl AgentScheduler for InMemoryAgentScheduler {
    fn schedule(&self, task: AgentTask) {
        self.queue.lock().push_back(task);
    }

    fn pop_next(&self) -> Option<AgentTask> {
        self.queue.lock().pop_front()
    }

    fn len(&self) -> usize {
        self.queue.lock().len()
    }
}
