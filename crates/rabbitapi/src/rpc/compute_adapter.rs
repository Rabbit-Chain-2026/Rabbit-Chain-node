use std::sync::Arc;

use rabbitcore::compute::{
    batch::{
        ComputeBatchPlanner, ComputeBatchRunner, ComputeExecutionService,
        DefaultComputeBatchPlanner, DefaultComputeConflictPolicy, ParallelComputeBatchRunner,
    },
    domain::DomainRegistry,
    execution::{BasicTxValidator, ObjectStore},
    policy::{AuthorizationPolicy, DefaultAuthorizationPolicy, NoopResourcePolicy, ResourcePolicy},
    scheduler::InMemoryComputeScheduler,
    Command, ComputeError, ComputeTx, GAME_DOMAIN,
};
use rabbitcore::game::GameOp;
use serde_json::Value;

use super::{compute_error_to_json, current_unix_secs, RpcConfig, RpcErrorObject};

/// 游戏域（GAME_DOMAIN）`Invoke` 结算门禁：用 shanhai-core 规则重算负载中的声明结果，
/// 不一致即拒绝。客户端/服务器只能"提议"结算，结果由确定性规则裁决（防伪造战报）。
pub(crate) fn gate_game_tx(tx: &ComputeTx) -> Result<(), RpcErrorObject> {
    if tx.domain_id != GAME_DOMAIN || tx.command != Command::Invoke {
        return Ok(());
    }
    let op = GameOp::parse(&tx.payload)
        .map_err(|e| RpcErrorObject::invalid_params(format!("game payload invalid: {e}")))?;
    rabbitcore::game::verify(&op)
        .map_err(|e| RpcErrorObject::invalid_params(format!("game settlement rejected: {e}")))?;
    Ok(())
}

/// RPC-facing compute adapter.
pub struct RpcComputeAdapter {
    service: Arc<ComputeExecutionService>,
    store: Arc<dyn ObjectStore>,
    authorization: Arc<dyn AuthorizationPolicy>,
    resources: Arc<dyn ResourcePolicy>,
    domains: Arc<dyn DomainRegistry>,
}

impl RpcComputeAdapter {
    /// Builds an adapter with the default in-memory batch pipeline.
    pub fn new_with_config(
        store: Arc<dyn ObjectStore>,
        domains: Arc<dyn DomainRegistry>,
        config: &RpcConfig,
    ) -> Self {
        Self::new_with_config_and_replay(
            store,
            domains,
            config,
            Arc::new(rabbitcore::compute::InMemoryReplayNonceRegistry::new()),
        )
    }

    /// Builds an adapter with an explicit shared replay registry (memory or durable).
    pub fn new_with_config_and_replay(
        store: Arc<dyn ObjectStore>,
        domains: Arc<dyn DomainRegistry>,
        config: &RpcConfig,
        replay_registry: Arc<dyn rabbitcore::compute::ReplayNonceRegistry>,
    ) -> Self {
        let authorization: Arc<dyn AuthorizationPolicy> = Arc::new(DefaultAuthorizationPolicy);
        let resources: Arc<dyn ResourcePolicy> = Arc::new(NoopResourcePolicy);
        let scheduler = Arc::new(InMemoryComputeScheduler::new(
            config.compute_scheduler_config(),
        ));
        let planner: Arc<dyn ComputeBatchPlanner> = Arc::new(DefaultComputeBatchPlanner::new(
            DefaultComputeConflictPolicy,
        ));
        let runner: Arc<dyn ComputeBatchRunner> =
            Arc::new(ParallelComputeBatchRunner::with_replay_registry(
                store.clone(),
                authorization.clone(),
                resources.clone(),
                domains.clone(),
                replay_registry,
            ));

        let service = Arc::new(ComputeExecutionService::new(
            store.clone(),
            scheduler,
            planner,
            runner,
            config.compute_fallback_policy(),
        ));

        Self {
            service,
            store,
            authorization,
            resources,
            domains,
        }
    }

    /// Returns the underlying execution service for background orchestration.
    pub(crate) fn execution_service(&self) -> Arc<ComputeExecutionService> {
        self.service.clone()
    }

    /// Simulates a tx without mutating state.
    pub fn simulate_compute_tx(&self, tx: ComputeTx) -> Result<Value, RpcErrorObject> {
        gate_game_tx(&tx)?;
        let validator = BasicTxValidator {
            store: &self.store,
            authorization: &self.authorization,
            resources: &self.resources,
            domains: &self.domains,
        };

        match validator.validate(&tx) {
            Ok(report) => Ok(serde_json::json!({
                "ok": true,
                "inputs": report.inputs.len(),
                "reads": report.reads.len(),
                "outputs": tx.output_proposals.len(),
                "tx_id": format!("0x{}", hex::encode(tx.tx_id.0.as_bytes())),
            })),
            Err(err) => Ok(serde_json::json!({
                "ok": false,
                "error": compute_error_to_json(&err),
            })),
        }
    }

    /// Submits a tx and waits for the current batch window to flush.
    pub async fn submit_compute_tx(&self, tx: ComputeTx) -> Result<Value, RpcErrorObject> {
        gate_game_tx(&tx)?;
        let outcome = self
            .service
            .submit_and_run(tx.clone())
            .await
            .map_err(|err| {
                RpcErrorObject::invalid_params(format!("compute execute failed: {err}"))
            })?;

        if !outcome.accepted {
            let err = outcome.error.unwrap_or_else(|| {
                ComputeError::InvalidOperation("compute execution rejected".to_string())
            });
            return Err(RpcErrorObject::invalid_params(format!(
                "compute execute failed: {err}"
            )));
        }

        let report = outcome.report.ok_or_else(|| {
            RpcErrorObject::internal_error("compute outcome missing validation report".to_string())
        })?;

        Ok(serde_json::json!({
            "ok": true,
            "tx_id": format!("0x{}", hex::encode(tx.tx_id.0.as_bytes())),
            "consumed_inputs": report.inputs.len(),
            "read_objects": report.reads.len(),
            "created_outputs": tx.output_proposals.len(),
            "submitted_at_unix": current_unix_secs(),
        }))
    }

    /// Forces a flush of ready batches.
    pub fn flush_ready_batches(&self) -> Result<Value, RpcErrorObject> {
        let outcomes = self
            .service
            .flush_ready()
            .map_err(|err| RpcErrorObject::internal_error(format!("flush failed: {err}")))?;

        Ok(serde_json::json!({
            "ok": true,
            "items": outcomes
                .into_iter()
                .map(|outcome| serde_json::json!({
                    "tx_id": format!("0x{}", hex::encode(outcome.tx_id.0.as_bytes())),
                    "accepted": outcome.accepted,
                }))
                .collect::<Vec<_>>()
        }))
    }
}
