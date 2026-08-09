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
pub(crate) fn gate_game_tx(
    tx: &ComputeTx,
    store: &dyn ObjectStore,
    now_unix: u64,
) -> Result<(), RpcErrorObject> {
    if tx.domain_id != GAME_DOMAIN {
        return Ok(());
    }
    match tx.command {
        Command::Mint => gate_propose_mint(tx)?,
        Command::Invoke => {
            let op = GameOp::parse(&tx.payload)
                .map_err(|e| RpcErrorObject::invalid_params(format!("game payload invalid: {e}")))?;
            match &op {
                GameOp::Vote { .. } => gate_vote(tx, store, now_unix, &op)?,
                GameOp::Execute { .. } => gate_execute(tx, store, now_unix, &op)?,
                _ => {
                    rabbitcore::game::verify(&op).map_err(|e| {
                        RpcErrorObject::invalid_params(format!("game settlement rejected: {e}"))
                    })?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// 解析对象 state 为治理提案对象。
fn parse_proposal_state(state: &[u8]) -> Result<rabbitcore::governance::Proposal, RpcErrorObject> {
    serde_json::from_slice(state)
        .map_err(|e| RpcErrorObject::invalid_params(format!("invalid proposal object state: {e}")))
}

/// 治理 Propose（Mint 提案对象 v1）：校验创建规则（押金/窗口/初始状态），
/// 防止伪造提案对象绕过治理门槛。非提案对象（session 等）放行。
fn gate_propose_mint(tx: &ComputeTx) -> Result<(), RpcErrorObject> {
    let Some(out) = tx.output_proposals.first() else {
        return Ok(());
    };
    let proposal = match parse_proposal_state(&out.state) {
        Ok(p) => p,
        Err(_) => return Ok(()), // 非提案对象
    };
    if proposal.status != rabbitcore::governance::ProposalStatus::Active {
        return Err(RpcErrorObject::invalid_params(format!(
            "proposal must be created active (status={:?})",
            proposal.status
        )));
    }
    if proposal.deposit < rabbitcore::governance::DEPOSIT_JZ {
        return Err(RpcErrorObject::invalid_params(format!(
            "proposal deposit too low: {}, minimum {}",
            proposal.deposit,
            rabbitcore::governance::DEPOSIT_JZ
        )));
    }
    if proposal.deadline_unix
        != proposal
            .created_at_unix
            .saturating_add(rabbitcore::governance::VOTE_WINDOW_SECS)
    {
        return Err(RpcErrorObject::invalid_params(
            "proposal deadline mismatch".into(),
        ));
    }
    if proposal.votes_for != 0 || proposal.votes_against != 0 {
        return Err(RpcErrorObject::invalid_params(
            "fresh proposal must have zero votes".into(),
        ));
    }
    Ok(())
}

/// 治理 Vote：输入提案对象必须 Active 且未到期；输出提案对象的票数增量
/// 必须与负载（voter/stake/approve）一致（防伪造治理状态）。
fn gate_vote(
    tx: &ComputeTx,
    store: &dyn ObjectStore,
    now_unix: u64,
    op: &GameOp,
) -> Result<(), RpcErrorObject> {
    let GameOp::Vote {
        proposal_id,
        voter,
        stake,
        approve,
    } = op
    else {
        unreachable!("gate_vote called with non-vote op")
    };
    if voter.trim().is_empty() {
        return Err(RpcErrorObject::invalid_params(
            "vote voter must not be empty".into(),
        ));
    }
    if *stake == 0 {
        return Err(RpcErrorObject::invalid_params(
            "vote stake must be positive".into(),
        ));
    }
    let input = tx.input_set.first().ok_or_else(|| {
        RpcErrorObject::invalid_params("vote tx missing input proposal".into())
    })?;
    let input_obj = store.get_output(*input).ok_or_else(|| {
        RpcErrorObject::invalid_params("vote input proposal not found on chain".into())
    })?;
    let input_p = parse_proposal_state(&input_obj.state)?;
    if input_p.proposal_id != *proposal_id {
        return Err(RpcErrorObject::invalid_params("vote proposal_id mismatch".into()));
    }
    if input_p.status != rabbitcore::governance::ProposalStatus::Active {
        return Err(RpcErrorObject::invalid_params(format!(
            "proposal not active (status={:?})",
            input_p.status
        )));
    }
    if now_unix > input_p.deadline_unix {
        return Err(RpcErrorObject::invalid_params(
            "proposal voting window expired".into(),
        ));
    }
    let out = tx.output_proposals.first().ok_or_else(|| {
        RpcErrorObject::invalid_params("vote tx missing output proposal".into())
    })?;
    let out_p = parse_proposal_state(&out.state)?;
    if out_p.proposal_id != *proposal_id {
        return Err(RpcErrorObject::invalid_params(
            "vote output proposal_id mismatch".into(),
        ));
    }
    let stake = *stake as u128;
    let expected_for = if *approve {
        input_p.votes_for.saturating_add(stake)
    } else {
        input_p.votes_for
    };
    let expected_against = if *approve {
        input_p.votes_against
    } else {
        input_p.votes_against.saturating_add(stake)
    };
    if out_p.votes_for != expected_for || out_p.votes_against != expected_against {
        return Err(RpcErrorObject::invalid_params(
            "vote tally mismatch: forged proposal state".into(),
        ));
    }
    if out_p.status != rabbitcore::governance::ProposalStatus::Active {
        return Err(RpcErrorObject::invalid_params(
            "voted proposal must remain active".into(),
        ));
    }
    Ok(())
}

/// 治理 Execute：输入提案对象必须 tally = Passed（窗口到期 + 赞成 ≥50%），
/// 防止未通过提案生效；输出执行记录对象必须携带 Executed 状态。
fn gate_execute(
    tx: &ComputeTx,
    store: &dyn ObjectStore,
    now_unix: u64,
    op: &GameOp,
) -> Result<(), RpcErrorObject> {
    let GameOp::Execute { proposal_id } = op else {
        unreachable!("gate_execute called with non-execute op")
    };
    let input = tx.input_set.first().ok_or_else(|| {
        RpcErrorObject::invalid_params("execute tx missing input proposal".into())
    })?;
    let input_obj = store.get_output(*input).ok_or_else(|| {
        RpcErrorObject::invalid_params("execute input proposal not found on chain".into())
    })?;
    let mut input_p = parse_proposal_state(&input_obj.state)?;
    if input_p.proposal_id != *proposal_id {
        return Err(RpcErrorObject::invalid_params(
            "execute proposal_id mismatch".into(),
        ));
    }
    // 重算 tally：未通过（窗口未到期或赞成不足）→ 拒绝执行（防伪造治理生效）
    if rabbitcore::governance::tally(&mut input_p, now_unix)
        != rabbitcore::governance::ProposalStatus::Passed
    {
        return Err(RpcErrorObject::invalid_params(
            "proposal is not passed; cannot execute".into(),
        ));
    }
    let out = tx.output_proposals.first().ok_or_else(|| {
        RpcErrorObject::invalid_params("execute tx missing output record".into())
    })?;
    let out_p = parse_proposal_state(&out.state)?;
    if out_p.proposal_id != *proposal_id {
        return Err(RpcErrorObject::invalid_params(
            "execute output proposal_id mismatch".into(),
        ));
    }
    if out_p.status != rabbitcore::governance::ProposalStatus::Executed {
        return Err(RpcErrorObject::invalid_params(
            "execute output must be Executed".into(),
        ));
    }
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
        gate_game_tx(&tx, self.store.as_ref(), current_unix_secs())?;
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
        gate_game_tx(&tx, self.store.as_ref(), current_unix_secs())?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use rabbitcore::compute::{
        InMemoryObjectStore, ObjectId, ObjectKind, ObjectOutput, OutputId, OutputProposal, Ownership,
        Script, TxId, TxWitness, Version,
    };
    use rabbitcore::crypto::{Address, Hash, keccak256};
    use rabbitcore::governance::{Proposal, ProposalKind, ProposalStatus, VOTE_WINDOW_SECS};

    fn output_id_for(object_id: &ObjectId, version: u64) -> OutputId {
        let mut data = Vec::with_capacity(40);
        data.extend_from_slice(object_id.0.as_bytes());
        data.extend_from_slice(&version.to_be_bytes());
        OutputId(Hash::from_bytes(keccak256(&data)))
    }

    fn proposal_object_id(id: &str) -> ObjectId {
        ObjectId(Hash::from_bytes(keccak256(
            format!("jzz/proposal/{id}").as_bytes(),
        )))
    }

    fn proposal_state(p: &Proposal) -> Vec<u8> {
        serde_json::to_vec(p).unwrap()
    }

    fn insert_proposal(
        store: &InMemoryObjectStore,
        p: &Proposal,
        version: u64,
        predecessor: Option<OutputId>,
    ) -> OutputId {
        let object_id = proposal_object_id(&p.proposal_id);
        let output_id = output_id_for(&object_id, version);
        let out = ObjectOutput {
            output_id,
            object_id,
            version: Version(version),
            domain_id: GAME_DOMAIN,
            kind: ObjectKind::State,
            owner: Ownership::Address(Address::zero()),
            predecessor,
            state: proposal_state(p),
            state_root: None,
            resources: Default::default(),
            lock: Script::default(),
            logic: None,
            created_at: 0,
            ttl: None,
            rent_reserve: None,
            flags: 0,
            extensions: vec![],
            spent: false,
        };
        store.insert_output(out).unwrap();
        output_id
    }

    fn output_proposal(p: &Proposal, version: u64, predecessor: Option<OutputId>) -> OutputProposal {
        let object_id = proposal_object_id(&p.proposal_id);
        OutputProposal {
            output_id: output_id_for(&object_id, version),
            object_id,
            domain_id: GAME_DOMAIN,
            kind: ObjectKind::State,
            owner: Ownership::Address(Address::zero()),
            predecessor,
            version: Version(version),
            state: proposal_state(p),
            state_root: None,
            resources: Default::default(),
            lock: Script::default(),
            logic: None,
            created_at: 0,
            ttl: None,
            rent_reserve: None,
            flags: 0,
            extensions: vec![],
        }
    }

    fn tx(
        command: Command,
        input_set: Vec<OutputId>,
        payload: Vec<u8>,
        out: OutputProposal,
    ) -> ComputeTx {
        ComputeTx {
            tx_id: TxId(Hash::zero()),
            domain_id: GAME_DOMAIN,
            command,
            input_set,
            read_set: vec![],
            output_proposals: vec![out],
            fee: 0,
            nonce: Some(1),
            metadata: vec![],
            payload,
            deadline_unix_secs: None,
            chain_id: None,
            network_id: None,
            witness: TxWitness {
                signatures: vec![],
                threshold: None,
            },
            max_fee: 0,
            priority_fee: 0,
            gas_limit: 0,
        }
    }

    fn active_proposal(id: &str, created: u64) -> Proposal {
        rabbitcore::governance::create_proposal(
            id,
            ProposalKind::FundActivity {
                amount: 500,
                memo: "test".into(),
            },
            "0xaaa",
            1000,
            created,
        )
        .unwrap()
    }

    fn passed_proposal(id: &str) -> Proposal {
        let mut p = active_proposal(id, 0);
        rabbitcore::governance::cast_vote(&mut p, 500, true, 100).unwrap();
        p
    }

    #[test]
    fn propose_mint_valid_proposal_accepted() {
        let store = InMemoryObjectStore::new();
        let p = active_proposal("p1", 0);
        let t = tx(Command::Mint, vec![], vec![], output_proposal(&p, 1, None));
        gate_game_tx(&t, &store, 100).expect("valid proposal mint");
    }

    #[test]
    fn propose_mint_low_deposit_rejected() {
        let store = InMemoryObjectStore::new();
        let mut p = active_proposal("p2", 0);
        p.deposit = 999;
        let t = tx(Command::Mint, vec![], vec![], output_proposal(&p, 1, None));
        let err = gate_game_tx(&t, &store, 100).unwrap_err();
        assert!(err.data.as_ref().and_then(serde_json::Value::as_str).unwrap_or("").contains("deposit too low"), "{err:?}");
    }

    #[test]
    fn propose_mint_non_proposal_passthrough() {
        let store = InMemoryObjectStore::new();
        let out = OutputProposal {
            output_id: output_id_for(&proposal_object_id("s1"), 1),
            object_id: proposal_object_id("s1"),
            domain_id: GAME_DOMAIN,
            kind: ObjectKind::State,
            owner: Ownership::Address(Address::zero()),
            predecessor: None,
            version: Version(1),
            state: br#"{"kind":"battle_session"}"#.to_vec(),
            state_root: None,
            resources: Default::default(),
            lock: Script::default(),
            logic: None,
            created_at: 0,
            ttl: None,
            rent_reserve: None,
            flags: 0,
            extensions: vec![],
        };
        let t = tx(Command::Mint, vec![], vec![], out);
        gate_game_tx(&t, &store, 100).expect("non-proposal mint passthrough");
    }

    #[test]
    fn vote_updates_tally_from_input() {
        let store = InMemoryObjectStore::new();
        let mut p = active_proposal("v1", 0);
        let input_id = insert_proposal(&store, &p, 1, None);
        p.votes_for = 500; // 输出 = 输入 + 500 赞成
        let payload = serde_json::to_vec(&GameOp::Vote {
            proposal_id: "v1".into(),
            voter: "0xbbb".into(),
            stake: 500,
            approve: true,
        })
        .unwrap();
        let t = tx(
            Command::Invoke,
            vec![input_id],
            payload,
            output_proposal(&p, 2, Some(input_id)),
        );
        gate_game_tx(&t, &store, 100).expect("valid vote");
    }

    #[test]
    fn vote_forged_tally_rejected() {
        let store = InMemoryObjectStore::new();
        let p = active_proposal("v2", 0);
        let input_id = insert_proposal(&store, &p, 1, None);
        let mut forged = p.clone();
        forged.votes_for = 900; // 声称 900，与 500 增量不符
        let payload = serde_json::to_vec(&GameOp::Vote {
            proposal_id: "v2".into(),
            voter: "0xbbb".into(),
            stake: 500,
            approve: true,
        })
        .unwrap();
        let t = tx(
            Command::Invoke,
            vec![input_id],
            payload,
            output_proposal(&forged, 2, Some(input_id)),
        );
        let err = gate_game_tx(&t, &store, 100).unwrap_err();
        assert!(err.data.as_ref().and_then(serde_json::Value::as_str).unwrap_or("").contains("tally mismatch"), "{err:?}");
    }

    #[test]
    fn vote_expired_window_rejected() {
        let store = InMemoryObjectStore::new();
        let p = active_proposal("v3", 0);
        let input_id = insert_proposal(&store, &p, 1, None);
        let mut out = p.clone();
        out.votes_for = 500;
        let payload = serde_json::to_vec(&GameOp::Vote {
            proposal_id: "v3".into(),
            voter: "0xbbb".into(),
            stake: 500,
            approve: true,
        })
        .unwrap();
        let t = tx(
            Command::Invoke,
            vec![input_id],
            payload,
            output_proposal(&out, 2, Some(input_id)),
        );
        // now 已过投票窗口 → 拒绝
        let err = gate_game_tx(&t, &store, VOTE_WINDOW_SECS + 1).unwrap_err();
        assert!(err.data.as_ref().and_then(serde_json::Value::as_str).unwrap_or("").contains("window expired"), "{err:?}");
    }

    #[test]
    fn execute_passed_proposal_accepted() {
        let store = InMemoryObjectStore::new();
        let p = passed_proposal("e1");
        let input_id = insert_proposal(&store, &p, 1, None);
        let mut executed = p.clone();
        executed.status = ProposalStatus::Executed;
        let payload = serde_json::to_vec(&GameOp::Execute {
            proposal_id: "e1".into(),
        })
        .unwrap();
        let t = tx(
            Command::Invoke,
            vec![input_id],
            payload,
            output_proposal(&executed, 2, Some(input_id)),
        );
        // now 已过窗口 → tally Passed（赞成 500/500 = 100%）
        gate_game_tx(&t, &store, VOTE_WINDOW_SECS + 1).expect("execute passed proposal");
    }

    #[test]
    fn execute_minority_proposal_rejected() {
        let store = InMemoryObjectStore::new();
        let mut p = active_proposal("e2", 0);
        rabbitcore::governance::cast_vote(&mut p, 400, true, 10).unwrap();
        rabbitcore::governance::cast_vote(&mut p, 600, false, 20).unwrap();
        let input_id = insert_proposal(&store, &p, 1, None);
        let mut executed = p.clone();
        executed.status = ProposalStatus::Executed;
        let payload = serde_json::to_vec(&GameOp::Execute {
            proposal_id: "e2".into(),
        })
        .unwrap();
        let t = tx(
            Command::Invoke,
            vec![input_id],
            payload,
            output_proposal(&executed, 2, Some(input_id)),
        );
        let err = gate_game_tx(&t, &store, VOTE_WINDOW_SECS + 1).unwrap_err();
        assert!(err.data.as_ref().and_then(serde_json::Value::as_str).unwrap_or("").contains("not passed"), "{err:?}");
    }

    #[test]
    fn execute_forged_output_status_rejected() {
        let store = InMemoryObjectStore::new();
        let p = passed_proposal("e3");
        let input_id = insert_proposal(&store, &p, 1, None);
        // 输出仍是 Active（未标 Executed）→ 拒
        let payload = serde_json::to_vec(&GameOp::Execute {
            proposal_id: "e3".into(),
        })
        .unwrap();
        let t = tx(
            Command::Invoke,
            vec![input_id],
            payload,
            output_proposal(&p, 2, Some(input_id)),
        );
        let err = gate_game_tx(&t, &store, VOTE_WINDOW_SECS + 1).unwrap_err();
        assert!(
            err.data
                .as_ref()
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .contains("must be Executed"),
            "{err:?}"
        );
    }

    #[test]
    fn execute_missing_input_rejected() {
        let store = InMemoryObjectStore::new();
        let p = passed_proposal("e4");
        let mut executed = p.clone();
        executed.status = ProposalStatus::Executed;
        let payload = serde_json::to_vec(&GameOp::Execute {
            proposal_id: "e4".into(),
        })
        .unwrap();
        let t = tx(Command::Invoke, vec![], payload, output_proposal(&executed, 2, None));
        let err = gate_game_tx(&t, &store, VOTE_WINDOW_SECS + 1).unwrap_err();
        assert!(err.data.as_ref().and_then(serde_json::Value::as_str).unwrap_or("").contains("missing input"), "{err:?}");
    }
}
