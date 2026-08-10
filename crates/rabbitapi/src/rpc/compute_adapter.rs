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

use super::{compute_error_to_json, current_unix_secs, RpcConfig, RpcErrorObject, VirtualClock};

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
        Command::Mint => gate_propose_mint(tx, store)?,
        Command::Invoke => {
            let op = GameOp::parse(&tx.payload)
                .map_err(|e| RpcErrorObject::invalid_params(format!("game payload invalid: {e}")))?;
            match &op {
                GameOp::Vote { .. } => gate_vote(tx, store, now_unix, &op)?,
                GameOp::Execute { .. } => gate_execute(tx, store, now_unix, &op)?,
                GameOp::ActionSettle { .. } => gate_action_settle(tx, store, &op)?,
                GameOp::SellDrop { .. } => gate_sell_drop(tx, store, &op)?,
                GameOp::PvpReveal { .. } => gate_pvp_reveal(tx, store, &op)?,
                GameOp::PvpSettle { .. } => gate_pvp_settle(tx, store, &op)?,
                GameOp::Enhance { rules_version, .. } | GameOp::ZkEnhance { rules_version, .. } => {
                    // 链上 EnhanceConfig 对象为权威规则：rules_version 必须匹配，
                    // 结果用该版本配置重算（治理 UpdateConfig 通过后新版本生效）。
                    // ZkEnhance 额外在验证中执行 zk 证明的原生验证（seed 不上链）。
                    let cfg = load_enhance_config(store)?;
                    if *rules_version != cfg.version {
                        return Err(RpcErrorObject::invalid_params(format!(
                            "stale enhance rules version: payload {rules_version}, on-chain config {}",
                            cfg.version
                        )));
                    }
                    rabbitcore::game::verify_with_config(&op, Some(&cfg)).map_err(|e| {
                        RpcErrorObject::invalid_params(format!("game settlement rejected: {e}"))
                    })?;
                }
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

/// 读取链上 EnhanceConfig 对象（权威强化规则）。
fn load_enhance_config(
    store: &dyn ObjectStore,
) -> Result<rabbitcore::game::EnhanceConfig, RpcErrorObject> {
    let config_id = rabbitcore::game::enhance_config_object_id();
    let obj = store.get_latest_output_by_object(config_id).ok_or_else(|| {
        RpcErrorObject::invalid_params("enhance config object not found on chain".into())
    })?;
    serde_json::from_slice(&obj.state).map_err(|e| {
        RpcErrorObject::invalid_params(format!("invalid enhance config object state: {e}"))
    })
}

/// 读取链上 MonsterTableConfig 对象（权威打怪规则；治理 UpdateConfig 可加新怪/调掉率）。
fn load_monster_config(
    store: &dyn ObjectStore,
) -> Result<rabbitcore::game::MonsterTableConfig, RpcErrorObject> {
    let config_id = rabbitcore::game::monster_config_object_id();
    let obj = store.get_latest_output_by_object(config_id).ok_or_else(|| {
        RpcErrorObject::invalid_params("monster config object not found on chain".into())
    })?;
    serde_json::from_slice(&obj.state).map_err(|e| {
        RpcErrorObject::invalid_params(format!("invalid monster config object state: {e}"))
    })
}

/// 读取链上 TokenRegistry 对象（代币注册元数据；治理 UpdateConfig 可注册新代币）。
fn load_token_registry(
    store: &dyn ObjectStore,
) -> Result<rabbitcore::assets::TokenRegistry, RpcErrorObject> {
    let config_id = rabbitcore::assets::token_registry_object_id();
    let obj = store.get_latest_output_by_object(config_id).ok_or_else(|| {
        RpcErrorObject::invalid_params("token registry object not found on chain".into())
    })?;
    serde_json::from_slice(&obj.state).map_err(|e| {
        RpcErrorObject::invalid_params(format!("invalid token registry state: {e}"))
    })
}

/// 读取链上铸币政策对象（权威铸币上限）；对象不存在时返回 None（由执行器确定性拒绝）。
fn load_mint_policy(store: &dyn ObjectStore) -> Option<rabbitcore::game::MintPolicy> {
    let config_id = rabbitcore::game::mint_policy_object_id();
    let obj = store.get_latest_output_by_object(config_id)?;
    serde_json::from_slice(&obj.state).ok()
}

/// 解析对象 state 为治理提案对象。
fn parse_proposal_state(state: &[u8]) -> Result<rabbitcore::governance::Proposal, RpcErrorObject> {
    serde_json::from_slice(state)
        .map_err(|e| RpcErrorObject::invalid_params(format!("invalid proposal object state: {e}")))
}

/// 治理 Propose（Mint 提案对象 v1）：校验创建规则（押金/窗口/初始状态），
/// 防止伪造提案对象绕过治理门槛。非提案对象（session 等）放行。
fn gate_propose_mint(
    tx: &ComputeTx,
    store: &dyn ObjectStore,
) -> Result<(), RpcErrorObject> {
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
    if proposal.deposit < rabbitcore::governance::DEPOSIT_SH {
        return Err(RpcErrorObject::invalid_params(format!(
            "proposal deposit too low: {}, minimum {}",
            proposal.deposit,
            rabbitcore::governance::DEPOSIT_SH
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
    // 山海币铸币提案（MintShc）：结构性校验 + 治理铸币上限（MintPolicy）。
    if let rabbitcore::governance::ProposalKind::MintShc { to, amount } = &proposal.kind {
        if to.trim().is_empty() {
            return Err(RpcErrorObject::invalid_params(
                "mint proposal target must not be empty".into(),
            ));
        }
        if *amount == 0 {
            return Err(RpcErrorObject::invalid_params(
                "mint proposal amount must be positive".into(),
            ));
        }
        if let Some(policy) = load_mint_policy(store) {
            if *amount > policy.per_mint_cap {
                return Err(RpcErrorObject::invalid_params(format!(
                    "mint denied: proposal amount {} exceeds per-mint cap {}",
                    amount, policy.per_mint_cap
                )));
            }
        }
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
    // UpdateConfig 生效：交易必须同时产出新版本配置对象，且与提案 params 一致。
    // 支持三类链上规则/资产对象：EnhanceConfig / MonsterTableConfig / TokenRegistry。
    if let rabbitcore::governance::ProposalKind::UpdateConfig {
        config_object,
        params,
    } = &input_p.kind
    {
        let enhance_id = rabbitcore::game::enhance_config_object_id();
        let monster_id = rabbitcore::game::monster_config_object_id();
        let registry_id = rabbitcore::assets::token_registry_object_id();
        let enhance_hex = format!("0x{}", hex::encode(enhance_id.0.as_bytes()));
        let monster_hex = format!("0x{}", hex::encode(monster_id.0.as_bytes()));
        let registry_hex = format!("0x{}", hex::encode(registry_id.0.as_bytes()));
        let target = config_object.trim().to_lowercase();
        let expected_id = if target == enhance_hex {
            enhance_id
        } else if target == monster_hex {
            monster_id
        } else if target == registry_hex {
            registry_id
        } else {
            return Err(RpcErrorObject::invalid_params(format!(
                "UpdateConfig: unsupported config object {config_object} (enhance/monster/token-registry)"
            )));
        };
        let out_cfg = tx
            .output_proposals
            .iter()
            .find(|o| o.object_id == expected_id)
            .ok_or_else(|| {
                RpcErrorObject::invalid_params(
                    "UpdateConfig: execute must output the new config object".into(),
                )
            })?;
        if target == enhance_hex {
            let cur_version = load_enhance_config(store)?.version;
            let parsed: rabbitcore::game::EnhanceConfig =
                serde_json::from_slice(&out_cfg.state).map_err(|e| {
                    RpcErrorObject::invalid_params(format!("invalid new config object state: {e}"))
                })?;
            if parsed.version != cur_version.saturating_add(1) {
                return Err(RpcErrorObject::invalid_params(format!(
                    "UpdateConfig: config version must advance {cur_version} -> {}",
                    cur_version.saturating_add(1)
                )));
            }
            let expected_cfg: rabbitcore::game::EnhanceConfig =
                serde_json::from_value(params.clone()).map_err(|e| {
                    RpcErrorObject::invalid_params(format!(
                        "UpdateConfig: proposal params not a valid enhance config: {e}"
                    ))
                })?;
            if expected_cfg != parsed {
                return Err(RpcErrorObject::invalid_params(
                    "UpdateConfig: output config object does not match proposal params".into(),
                ));
            }
        } else if target == monster_hex {
            let cur_version = load_monster_config(store)?.version;
            let parsed: rabbitcore::game::MonsterTableConfig =
                serde_json::from_slice(&out_cfg.state).map_err(|e| {
                    RpcErrorObject::invalid_params(format!("invalid new config object state: {e}"))
                })?;
            if parsed.version != cur_version.saturating_add(1) {
                return Err(RpcErrorObject::invalid_params(format!(
                    "UpdateConfig: config version must advance {cur_version} -> {}",
                    cur_version.saturating_add(1)
                )));
            }
            let expected_cfg: rabbitcore::game::MonsterTableConfig =
                serde_json::from_value(params.clone()).map_err(|e| {
                    RpcErrorObject::invalid_params(format!(
                        "UpdateConfig: proposal params not a valid monster config: {e}"
                    ))
                })?;
            if expected_cfg != parsed {
                return Err(RpcErrorObject::invalid_params(
                    "UpdateConfig: output config object does not match proposal params".into(),
                ));
            }
        } else {
            let cur_version = load_token_registry(store)?.version;
            let parsed: rabbitcore::assets::TokenRegistry =
                serde_json::from_slice(&out_cfg.state).map_err(|e| {
                    RpcErrorObject::invalid_params(format!("invalid new config object state: {e}"))
                })?;
            if parsed.version != cur_version.saturating_add(1) {
                return Err(RpcErrorObject::invalid_params(format!(
                    "UpdateConfig: config version must advance {cur_version} -> {}",
                    cur_version.saturating_add(1)
                )));
            }
            let expected_cfg: rabbitcore::assets::TokenRegistry =
                serde_json::from_value(params.clone()).map_err(|e| {
                    RpcErrorObject::invalid_params(format!(
                        "UpdateConfig: proposal params not a valid token registry: {e}"
                    ))
                })?;
            if expected_cfg != parsed {
                return Err(RpcErrorObject::invalid_params(
                    "UpdateConfig: output config object does not match proposal params".into(),
                ));
            }
        }
    }
    Ok(())
}

/// 解析 32 字节 hex 哈希。
fn parse_hash_hex(s: &str) -> Result<rabbitcore::crypto::Hash, RpcErrorObject> {
    let raw = hex::decode(s.trim().strip_prefix("0x").unwrap_or(s.trim()))
        .map_err(|e| RpcErrorObject::invalid_params(format!("invalid hash hex: {e}")))?;
    if raw.len() != 32 {
        return Err(RpcErrorObject::invalid_params("hash must be 32 bytes".into()));
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&raw);
    Ok(rabbitcore::crypto::Hash::from_bytes(buf))
}

/// 统一玩法动作结算门禁（先承诺后揭晓，防作弊协议）：
/// 1) 输入[0] 是 ActionSession（未结算）
/// 2) 随机揭晓：random_block_hash 是真实链块且时间 >= session 创建（提交前不可知），
///    seed = keccak(block_hash ‖ session_id)
/// 3) 确定性重算 execute_action → 声称结果 + 掉落一致
/// 4) 输出：session v2（settled + result）+ 掉落对象
fn gate_action_settle(
    tx: &ComputeTx,
    store: &dyn ObjectStore,
    op: &GameOp,
) -> Result<(), RpcErrorObject> {
    let GameOp::ActionSettle {
        session_id,
        seed,
        random_block_hash,
        claimed,
        drops,
    } = op
    else {
        unreachable!("gate_action_settle called with non-settle op")
    };
    // 1) session 输入
    let input = tx.input_set.first().ok_or_else(|| {
        RpcErrorObject::invalid_params("action settle missing input session".into())
    })?;
    let input_obj = store.get_output(*input).ok_or_else(|| {
        RpcErrorObject::invalid_params("action settle input session not found on chain".into())
    })?;
    let session: rabbitcore::game::ActionSession = serde_json::from_slice(&input_obj.state)
        .map_err(|e| RpcErrorObject::invalid_params(format!("invalid action session state: {e}")))?;
    if session.session_id != *session_id {
        return Err(RpcErrorObject::invalid_params("settle session_id mismatch".into()));
    }
    if session.settled {
        return Err(RpcErrorObject::invalid_params("session already settled".into()));
    }
    // 2) 随机揭晓：真实链块 + 时间 >= session 创建 + seed 推导
    let block_hash = parse_hash_hex(random_block_hash)?;
    let block = rabbitnet::global_block_by_hash(&block_hash).ok_or_else(|| {
        RpcErrorObject::invalid_params("random block hash not found on chain".into())
    })?;
    if block.header.timestamp < session.created_at_unix {
        return Err(RpcErrorObject::invalid_params(
            "random block predates session commitment (seed shopping)".into(),
        ));
    }
    let expected_seed = rabbitcore::game::derive_action_seed(&block_hash, &session.session_id);
    if expected_seed != *seed {
        return Err(RpcErrorObject::invalid_params("seed mismatch with random block".into()));
    }
    // 3) 确定性重算：链上规则对象为权威（session.rules_version 必须匹配配置版本）
    let (enhance_cfg, monsters) = match &session.action_type {
        rabbitcore::game::ActionKind::Enhance => {
            let cfg = load_enhance_config(store)?;
            if session.rules_version != cfg.version {
                return Err(RpcErrorObject::invalid_params(format!(
                    "stale enhance rules version: session {}, on-chain config {}",
                    session.rules_version, cfg.version
                )));
            }
            (cfg, rabbitcore::game::MonsterTableConfig::default())
        }
        rabbitcore::game::ActionKind::Battle => {
            let cfg = load_monster_config(store)?;
            if session.rules_version != cfg.version {
                return Err(RpcErrorObject::invalid_params(format!(
                    "stale monster rules version: session {}, on-chain config {}",
                    session.rules_version, cfg.version
                )));
            }
            (rabbitcore::game::EnhanceConfig::default(), cfg)
        }
    };
    let outcome = rabbitcore::game::execute_action(
        &session.action_type,
        &session.inputs,
        *seed,
        &enhance_cfg,
        &monsters,
    )
    .map_err(|e| RpcErrorObject::invalid_params(format!("action rejected: {e}")))?;
    if &outcome.result != claimed {
        return Err(RpcErrorObject::invalid_params("claimed result mismatch".into()));
    }
    if &outcome.drops != drops {
        return Err(RpcErrorObject::invalid_params("claimed drops mismatch".into()));
    }
    // 4) 输出校验：session v2（settled）+ 掉落对象（数量匹配）
    let out = tx.output_proposals.first().ok_or_else(|| {
        RpcErrorObject::invalid_params("action settle missing output record".into())
    })?;
    let out_session: rabbitcore::game::ActionSession = serde_json::from_slice(&out.state)
        .map_err(|e| RpcErrorObject::invalid_params(format!("invalid settle output: {e}")))?;
    if out_session.session_id != *session_id || !out_session.settled {
        return Err(RpcErrorObject::invalid_params(
            "settle output must be a settled session".into(),
        ));
    }
    if tx.output_proposals.len() - 1 != drops.len() {
        return Err(RpcErrorObject::invalid_params("drop object count mismatch".into()));
    }
    Ok(())
}

/// 打金闭环门禁（SellDrop）：消耗掉落对象 v1 → 国库 SHC 支付给签名者。
/// 1) 输入[0] 是未消费的 action_drop 对象（session_id/item_id 匹配、价格内嵌）
/// 2) 对象 owner == 签名者（否则授权层 SignatureOwnerMismatch；此处快速反馈）
pub(crate) fn gate_sell_drop(
    tx: &ComputeTx,
    store: &dyn ObjectStore,
    op: &GameOp,
) -> Result<(), RpcErrorObject> {
    let GameOp::SellDrop { session_id, item_id } = op else {
        unreachable!("gate_sell_drop called with non-sell op")
    };
    let input = tx.input_set.first().ok_or_else(|| {
        RpcErrorObject::invalid_params("sell drop missing input object".into())
    })?;
    let obj = store.get_output(*input).ok_or_else(|| {
        RpcErrorObject::invalid_params("sell drop input object not found on chain".into())
    })?;
    if obj.spent {
        return Err(RpcErrorObject::invalid_params("drop already sold (spent)".into()));
    }
    let state: serde_json::Value = serde_json::from_slice(&obj.state)
        .map_err(|e| RpcErrorObject::invalid_params(format!("invalid drop state: {e}")))?;
    if state.get("kind").and_then(|x| x.as_str()) != Some("action_drop")
        || state.get("session_id").and_then(|x| x.as_str()) != Some(session_id.as_str())
        || state.get("item_id").and_then(|x| x.as_str()) != Some(item_id.as_str())
    {
        return Err(RpcErrorObject::invalid_params("drop object mismatch".into()));
    }
    let price = state.get("price_shc").and_then(|x| x.as_u64()).unwrap_or(0);
    if price == 0 {
        return Err(RpcErrorObject::invalid_params("drop has no sell value".into()));
    }
    // 签名者必须拥有该掉落对象（授权层也会校验；此处尽早拒绝）
    let signer = rabbitcore::game::ed25519_signer_address(tx).ok_or_else(|| {
        RpcErrorObject::invalid_params("sell drop requires an ed25519 signer".into())
    })?;
    if !matches!(&obj.owner, rabbitcore::compute::Ownership::Address(a) if *a == signer) {
        return Err(RpcErrorObject::invalid_params(
            "sell drop: object not owned by signer".into(),
        ));
    }
    Ok(())
}

/// PvP 承诺对象 state 解析（v1 = 未揭晓，v2 = revealed）。
fn parse_pvp_commit(state: &[u8]) -> Result<serde_json::Value, RpcErrorObject> {
    serde_json::from_slice(state)
        .map_err(|e| RpcErrorObject::invalid_params(format!("invalid pvp commit state: {e}")))
}

/// PvP 双盲揭晓门禁：keccak(seed‖pvp_id‖committer‖team) 必须等于承诺。
pub(crate) fn gate_pvp_reveal(
    tx: &ComputeTx,
    store: &dyn ObjectStore,
    op: &GameOp,
) -> Result<(), RpcErrorObject> {
    let GameOp::PvpReveal { pvp_id, committer, seed } = op else {
        unreachable!("gate_pvp_reveal called with non-reveal op")
    };
    let input = tx.input_set.first().ok_or_else(|| {
        RpcErrorObject::invalid_params("pvp reveal missing input commitment".into())
    })?;
    let obj = store.get_output(*input).ok_or_else(|| {
        RpcErrorObject::invalid_params("pvp commitment not found on chain".into())
    })?;
    if obj.spent {
        return Err(RpcErrorObject::invalid_params("pvp commitment already revealed".into()));
    }
    let state = parse_pvp_commit(&obj.state)?;
    if state.get("kind").and_then(|x| x.as_str()) != Some("pvp_commit")
        || state.get("pvp_id").and_then(|x| x.as_str()) != Some(pvp_id.as_str())
        || state.get("committer").and_then(|x| x.as_str()) != Some(committer.as_str())
    {
        return Err(RpcErrorObject::invalid_params("pvp commitment mismatch".into()));
    }
    let teams: rabbitcore::game::BattleTeams =
        serde_json::from_value(state.get("team").cloned().unwrap_or(serde_json::Value::Null))
            .map_err(|e| RpcErrorObject::invalid_params(format!("invalid pvp team: {e}")))?;
    let committed = hex::decode(
        state
            .get("commit_hash")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .strip_prefix("0x")
            .unwrap_or(""),
    )
    .map_err(|e| RpcErrorObject::invalid_params(format!("commit hash hex: {e}")))?;
    let actual = rabbitcore::game::pvp_commit_hash(*seed, pvp_id, committer, &teams);
    if committed != actual {
        return Err(RpcErrorObject::invalid_params("pvp reveal: seed does not match commitment".into()));
    }
    // 输出 v2 必须标记 revealed 且 seed 一致
    let out = tx.output_proposals.first().ok_or_else(|| {
        RpcErrorObject::invalid_params("pvp reveal missing output record".into())
    })?;
    let out_state = parse_pvp_commit(&out.state)?;
    if out_state.get("revealed").and_then(|x| x.as_bool()) != Some(true)
        || out_state.get("seed").and_then(|x| x.as_u64()) != Some(*seed)
    {
        return Err(RpcErrorObject::invalid_params("pvp reveal output must mark revealed".into()));
    }
    Ok(())
}

/// PvP 结算门禁：双方承诺均已揭晓 → 组合种子 → 确定性战斗 → 验证声称结果。
pub(crate) fn gate_pvp_settle(
    tx: &ComputeTx,
    store: &dyn ObjectStore,
    op: &GameOp,
) -> Result<(), RpcErrorObject> {
    let GameOp::PvpSettle { pvp_id, claimed_winner, claimed_rounds, rules_version } = op else {
        unreachable!("gate_pvp_settle called with non-settle op")
    };
    let _ = rules_version;
    if tx.input_set.len() != 2 {
        return Err(RpcErrorObject::invalid_params(
            "pvp settle requires both revealed commitments".into(),
        ));
    }
    let mut seeds = Vec::new();
    let mut teams: Option<rabbitcore::game::BattleTeams> = None;
    for id in &tx.input_set {
        let obj = store.get_output(*id).ok_or_else(|| {
            RpcErrorObject::invalid_params("pvp commitment not found on chain".into())
        })?;
        let state = parse_pvp_commit(&obj.state)?;
        if state.get("kind").and_then(|x| x.as_str()) != Some("pvp_commit")
            || state.get("pvp_id").and_then(|x| x.as_str()) != Some(pvp_id.as_str())
            || state.get("revealed").and_then(|x| x.as_bool()) != Some(true)
        {
            return Err(RpcErrorObject::invalid_params(
                "pvp settle: both commitments must be revealed".into(),
            ));
        }
        let seed = state.get("seed").and_then(|x| x.as_u64()).unwrap_or(0);
        seeds.push(seed);
        let t: rabbitcore::game::BattleTeams =
            serde_json::from_value(state.get("team").cloned().unwrap_or(serde_json::Value::Null))
                .map_err(|e| RpcErrorObject::invalid_params(format!("invalid pvp team: {e}")))?;
        teams = Some(match teams {
            None => t.clone(),
            Some(existing) => {
                if existing != t {
                    return Err(RpcErrorObject::invalid_params(
                        "pvp settle: teams must be identical across both commitments".into(),
                    ));
                }
                existing
            }
        });
    }
    let teams = teams.ok_or_else(|| RpcErrorObject::invalid_params("pvp teams missing".into()))?;
    let seed = rabbitcore::game::pvp_combined_seed(seeds[0], seeds[1]);
    let report = rabbitcore::game::resolve_battle(&teams, seed);
    if report.winner != *claimed_winner {
        return Err(RpcErrorObject::invalid_params(format!(
            "pvp winner mismatch: claimed {claimed_winner}, computed {}",
            report.winner
        )));
    }
    if report.total_rounds != *claimed_rounds {
        return Err(RpcErrorObject::invalid_params(format!(
            "pvp rounds mismatch: claimed {claimed_rounds}, computed {}",
            report.total_rounds
        )));
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
    /// 虚拟时钟（与 RpcApi 共享；testkit 时间跳跃）
    virtual_clock: Arc<VirtualClock>,
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
            virtual_clock: Arc::new(VirtualClock::new()),
        }
    }

    /// 关联共享虚拟时钟（RpcApi 构造时注入；testkit 时间跳跃）。
    pub fn with_virtual_clock(mut self, clock: Arc<VirtualClock>) -> Self {
        self.virtual_clock = clock;
        self
    }

    /// Returns the underlying execution service for background orchestration.
    pub(crate) fn execution_service(&self) -> Arc<ComputeExecutionService> {
        self.service.clone()
    }

    /// Simulates a tx without mutating state.
    pub fn simulate_compute_tx(&self, tx: ComputeTx) -> Result<Value, RpcErrorObject> {
        gate_game_tx(&tx, self.store.as_ref(), self.virtual_clock.now())?;
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
        gate_game_tx(&tx, self.store.as_ref(), self.virtual_clock.now())?;
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
        InMemoryObjectStore, ObjectId, ObjectKind, ObjectOutput, OutputId, OutputProposal,
        Ownership, Script, TxId, TxWitness, Version,
    };
    use rabbitcore::crypto::{Address, Hash, keccak256};
    use rabbitcore::governance::{Proposal, ProposalKind, ProposalStatus, VOTE_WINDOW_SECS};

    fn output_id_for(object_id: &ObjectId, version: u64) -> OutputId {
        let mut data = Vec::with_capacity(40);
        data.extend_from_slice(object_id.0.as_bytes());
        data.extend_from_slice(&version.to_be_bytes());
        OutputId(Hash::from_bytes(keccak256(&data)))
    }

    pub(super) fn proposal_object_id(id: &str) -> ObjectId {
        ObjectId(Hash::from_bytes(keccak256(
            format!("shanhai/proposal/{id}").as_bytes(),
        )))
    }

    fn proposal_state(p: &Proposal) -> Vec<u8> {
        serde_json::to_vec(p).unwrap()
    }

    pub(super) fn insert_proposal(
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

    pub(super) fn output_proposal(
        p: &Proposal,
        version: u64,
        predecessor: Option<OutputId>,
    ) -> OutputProposal {
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

    pub(super) fn tx(
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

    pub(super) fn active_proposal(id: &str, created: u64) -> Proposal {
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

#[cfg(test)]
mod config_gate_tests {
    use super::tests::{insert_proposal, output_proposal, proposal_object_id, tx};
    use rabbitcore::compute::InMemoryObjectStore;
    use super::*;
    use rabbitcore::compute::{
        ObjectId, ObjectKind, ObjectOutput, OutputId, OutputProposal, TxId, TxWitness, Version,
    };
    use rabbitcore::compute::{Ownership, Script};
    use rabbitcore::crypto::{Address, Hash, keccak256};
    use rabbitcore::game::EnhanceConfig;
    use rabbitcore::governance::{ProposalStatus, VOTE_WINDOW_SECS};

    fn config_state(cfg: &EnhanceConfig) -> Vec<u8> {
        serde_json::to_vec(cfg).unwrap()
    }

    fn insert_config(store: &InMemoryObjectStore, cfg: &EnhanceConfig, version: u64) -> OutputId {
        let object_id = rabbitcore::game::enhance_config_object_id();
        let mut data = Vec::with_capacity(40);
        data.extend_from_slice(object_id.0.as_bytes());
        data.extend_from_slice(&version.to_be_bytes());
        let output_id = OutputId(Hash::from_bytes(keccak256(&data)));
        let out = ObjectOutput {
            output_id,
            object_id,
            version: Version(version),
            domain_id: GAME_DOMAIN,
            kind: ObjectKind::State,
            owner: Ownership::Address(Address::zero()),
            predecessor: None,
            state: config_state(cfg),
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

    fn enhance_tx(cfg: &EnhanceConfig, seed: u64, forged: bool) -> ComputeTx {
        let r = rabbitcore::game::verify_with_config(
            &GameOp::Enhance {
                object_id: "0x01".into(),
                current_level: 0,
                current_pity: 0,
                star_stones: 0,
                seed,
                claimed_success: true,
                claimed_new_level: 1,
                claimed_pity: 0,
                rules_version: cfg.version,
            },
            Some(cfg),
        );
        // 用真实 roll 结果构造合法负载
        let rr = rabbitcore::game::roll_with_config(0, 0, seed, 0, cfg);
        let _ = r;
        let op = GameOp::Enhance {
            object_id: "0x01".into(),
            current_level: 0,
            current_pity: 0,
            star_stones: 0,
            seed,
            claimed_success: if forged { !rr.success } else { rr.success },
            claimed_new_level: rr.new_level,
            claimed_pity: rr.pity,
            rules_version: cfg.version,
        };
        tx(Command::Invoke, vec![], serde_json::to_vec(&op).unwrap(), {
            // dummy output（gate 不校验对象流）
            let mut p = super::tests::active_proposal("dummy", 0);
            output_proposal(&p, 1, None)
        })
    }

    #[test]
    fn enhance_matching_config_accepted() {
        let store = InMemoryObjectStore::new();
        let cfg = EnhanceConfig::default();
        insert_config(&store, &cfg, 1);
        let t = enhance_tx(&cfg, 42, false);
        gate_game_tx(&t, &store, 100).expect("matching config accepted");
    }

    #[test]
    fn enhance_stale_rules_version_rejected() {
        let store = InMemoryObjectStore::new();
        let cfg = EnhanceConfig::default(); // v1
        insert_config(&store, &cfg, 1);
        let mut t = enhance_tx(&cfg, 42, false);
        // 声称 v2，链上仍 v1
        let mut op: GameOp =
            serde_json::from_slice(&t.payload).expect("op");
        match &mut op {
            GameOp::Enhance { rules_version, .. } => *rules_version = 2,
            _ => unreachable!(),
        }
        t.payload = serde_json::to_vec(&op).unwrap();
        let err = gate_game_tx(&t, &store, 100).unwrap_err();
        assert!(err.data.as_ref().and_then(serde_json::Value::as_str).unwrap_or("").contains("stale"), "{err:?}");
    }

    #[test]
    fn enhance_without_config_rejected() {
        let store = InMemoryObjectStore::new();
        let cfg = EnhanceConfig::default();
        let t = enhance_tx(&cfg, 42, false);
        let err = gate_game_tx(&t, &store, 100).unwrap_err();
        assert!(err.data.as_ref().and_then(serde_json::Value::as_str).unwrap_or("").contains("config object not found"), "{err:?}");
    }

    fn passed_update_config_proposal(id: &str, v2: &EnhanceConfig) -> rabbitcore::governance::Proposal {
        use rabbitcore::governance::{cast_vote, create_proposal, ProposalKind, ProposalStatus, tally};
        let mut p = create_proposal(
            id,
            ProposalKind::UpdateConfig {
                config_object: format!("0x{}", hex::encode(rabbitcore::game::enhance_config_object_id().0.as_bytes())),
                params: serde_json::to_value(v2.clone()).unwrap(),
            },
            "0xaaa",
            1000,
            0,
        )
        .unwrap();
        cast_vote(&mut p, 500, true, 100).unwrap();
        assert_eq!(tally(&mut p, VOTE_WINDOW_SECS + 1), ProposalStatus::Passed);
        p
    }

    #[test]
    fn execute_update_config_advances_config() {
        let store = InMemoryObjectStore::new();
        let mut v2 = EnhanceConfig::default();
        v2.version = 2;
        v2.success_permille[0] = 500; // 改规则：+0 成功率 90% → 50%
        let p = passed_update_config_proposal("uc1", &v2);
        let input_id = insert_proposal(&store, &p, 1, None);
        let cfg_v1 = EnhanceConfig::default();
        insert_config(&store, &cfg_v1, 1);

        // execute 交易：输入提案 v1，输出 Executed 提案 v2 + 新配置对象 v2
        let mut executed = p.clone();
        executed.status = ProposalStatus::Executed;
        let exec_out = output_proposal(&executed, 2, Some(input_id));
        let config_id = rabbitcore::game::enhance_config_object_id();
        let mut data = Vec::with_capacity(40);
        data.extend_from_slice(config_id.0.as_bytes());
        data.extend_from_slice(&2u64.to_be_bytes());
        let cfg_out_id = OutputId(Hash::from_bytes(keccak256(&data)));
        let cfg_out = OutputProposal {
            output_id: cfg_out_id,
            object_id: config_id,
            domain_id: GAME_DOMAIN,
            kind: ObjectKind::State,
            owner: Ownership::Address(Address::zero()),
            predecessor: None,
            version: Version(2),
            state: config_state(&v2),
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
        let payload = serde_json::to_vec(&GameOp::Execute {
            proposal_id: "uc1".into(),
        })
        .unwrap();
        let t = ComputeTx {
            tx_id: TxId(Hash::zero()),
            domain_id: GAME_DOMAIN,
            command: Command::Invoke,
            input_set: vec![input_id],
            read_set: vec![],
            output_proposals: vec![exec_out, cfg_out],
            fee: 0,
            nonce: Some(1),
            metadata: vec![],
            payload,
            deadline_unix_secs: None,
            chain_id: None,
            network_id: None,
            witness: TxWitness { signatures: vec![], threshold: None },
            max_fee: 0,
            priority_fee: 0,
            gas_limit: 0,
        };
        // 门禁接受 UpdateConfig 生效（新配置对象落库由执行器在区块执行时完成，见 e2e）
        gate_game_tx(&t, &store, VOTE_WINDOW_SECS + 1).expect("UpdateConfig execute accepted");
    }

    #[test]
    fn execute_update_config_mismatched_output_rejected() {
        let store = InMemoryObjectStore::new();
        let mut v2 = EnhanceConfig::default();
        v2.version = 2;
        v2.success_permille[0] = 500;
        let p = passed_update_config_proposal("uc2", &v2);
        let input_id = insert_proposal(&store, &p, 1, None);
        let cfg_v1 = EnhanceConfig::default();
        insert_config(&store, &cfg_v1, 1);

        let mut executed = p.clone();
        executed.status = ProposalStatus::Executed;
        let exec_out = output_proposal(&executed, 2, Some(input_id));
        let config_id = rabbitcore::game::enhance_config_object_id();
        let mut data = Vec::with_capacity(40);
        data.extend_from_slice(config_id.0.as_bytes());
        data.extend_from_slice(&2u64.to_be_bytes());
        let cfg_out_id = OutputId(Hash::from_bytes(keccak256(&data)));
        // 伪造：成功概率与提案不一致
        let mut wrong = v2.clone();
        wrong.success_permille[0] = 800;
        let cfg_out = OutputProposal {
            output_id: cfg_out_id,
            object_id: config_id,
            domain_id: GAME_DOMAIN,
            kind: ObjectKind::State,
            owner: Ownership::Address(Address::zero()),
            predecessor: None,
            version: Version(2),
            state: config_state(&wrong),
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
        let payload = serde_json::to_vec(&GameOp::Execute {
            proposal_id: "uc2".into(),
        })
        .unwrap();
        let t = ComputeTx {
            tx_id: TxId(Hash::zero()),
            domain_id: GAME_DOMAIN,
            command: Command::Invoke,
            input_set: vec![input_id],
            read_set: vec![],
            output_proposals: vec![exec_out, cfg_out],
            fee: 0,
            nonce: Some(1),
            metadata: vec![],
            payload,
            deadline_unix_secs: None,
            chain_id: None,
            network_id: None,
            witness: TxWitness { signatures: vec![], threshold: None },
            max_fee: 0,
            priority_fee: 0,
            gas_limit: 0,
        };
        let err = gate_game_tx(&t, &store, VOTE_WINDOW_SECS + 1).unwrap_err();
        assert!(err.data.as_ref().and_then(serde_json::Value::as_str).unwrap_or("").contains("does not match"), "{err:?}");
    }

    // ── 山海币治理铸币门控（MintShc 提案）────────────────────────────

    fn mint_shc_proposal_tx(_authority: &Address, to: Address, amount: u64) -> ComputeTx {
        let p = rabbitcore::governance::create_proposal(
            "gate-mint-shc",
            rabbitcore::governance::ProposalKind::MintShc {
                to: format!("0x{}", hex::encode(to.as_bytes())),
                amount,
            },
            "0xproposer",
            1000,
            0,
        )
        .expect("proposal");
        tx(Command::Mint, vec![], vec![], output_proposal(&p, 1, None))
    }

    fn test_target_address(seed: u8) -> Address {
        let k = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let pk = k.verifying_key().to_bytes();
        let h = keccak256(&pk);
        Address::from_slice(&h[12..]).unwrap()
    }

    fn insert_mint_policy(store: &InMemoryObjectStore, cap: u64, version: u64) -> OutputId {
        let object_id = rabbitcore::game::mint_policy_object_id();
        let mut data = Vec::with_capacity(40);
        data.extend_from_slice(object_id.0.as_bytes());
        data.extend_from_slice(&version.to_be_bytes());
        let output_id = OutputId(Hash::from_bytes(keccak256(&data)));
        let policy = rabbitcore::game::MintPolicy { version, per_mint_cap: cap };
        let out = ObjectOutput {
            output_id,
            object_id,
            version: Version(version),
            domain_id: GAME_DOMAIN,
            kind: ObjectKind::State,
            owner: Ownership::Address(Address::zero()),
            predecessor: None,
            state: serde_json::to_vec(&policy).unwrap(),
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

    #[test]
    fn mint_shc_proposal_within_cap_accepted() {
        let store = InMemoryObjectStore::new();
        let _ = insert_mint_policy(&store, 1_000_000, 1);
        let t = mint_shc_proposal_tx(&Address::zero(), test_target_address(0x33), 1_000);
        gate_game_tx(&t, &store, 100).expect("mint proposal within cap accepted");
    }

    #[test]
    fn mint_shc_proposal_over_cap_rejected() {
        let store = InMemoryObjectStore::new();
        let _ = insert_mint_policy(&store, 500, 1);
        let t = mint_shc_proposal_tx(&Address::zero(), test_target_address(0x55), 1_000);
        let err = gate_game_tx(&t, &store, 100).unwrap_err();
        assert!(
            err.data.as_ref().and_then(serde_json::Value::as_str).unwrap_or("").contains("exceeds per-mint cap"),
            "{err:?}"
        );
    }
}
