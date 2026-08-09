//! State executor for block transitions.
//!
//! 结算进共识（BlockTime）：`execute_block` 对区块体内的计算交易逐笔执行，
//! 生成 receipts 并与区块体声明比对（任何节点重跑结果一致，伪造/篡改即拒绝）；
//! 非 Mint 交易的 gas 费用在区块执行时进入国库账户（`governance::treasury_address`）。
//!
//! 注意：RPC SubmitTime 路径（rabbitapi batch runner）仍保持现状（只校验不收集费用）；
//! 完全迁移到 BlockTime（提交只入队、区块统一结算）是后续步骤，本模块先提供
//! 共识侧的执行与校验机制。

use super::StateDb;
use crate::account::U256;
use crate::block::{Block, Receipt, ReceiptStatus};
use crate::compute::{
    domain::{DomainConfig, DomainRegistry, InMemoryDomainRegistry},
    execution::{BasicTxExecutor, ObjectStore},
    policy::{DefaultAuthorizationPolicy, NoopResourcePolicy},
    primitives::GAME_DOMAIN,
    Command, ComputeTx, DomainId, InMemoryObjectStore,
};
use crate::crypto::{Address, Hash};
use crate::game::GameOp;
use crate::governance::{tally, Proposal, ProposalStatus, TreasuryLedger};
use std::sync::{Arc, RwLock};
use thiserror::Error;

/// Execution errors
#[derive(Error, Debug, Clone)]
pub enum ExecutionError {
    #[error("Block error: {0}")]
    Block(String),
}

pub type Result<T> = std::result::Result<T, ExecutionError>;

/// State transition
#[derive(Clone, Debug)]
pub struct StateTransition {
    /// From state root
    pub from_root: Hash,
    /// To state root
    pub to_root: Hash,
}

/// State executor
pub struct StateExecutor {
    /// State database
    state_db: Arc<StateDb>,
    /// Chain ID
    chain_id: u64,
    /// Compute object store（区块内计算交易的状态后端）
    compute_store: Arc<dyn ObjectStore>,
    /// Domain registry（含 GAME_DOMAIN）
    domains: Arc<dyn DomainRegistry>,
    /// 国库账户地址（费用收集目标）
    treasury: Address,
    /// 国库账本（审计视图：收入/支出事件流；余额与 StateDb 国库账户一致）
    ledger: RwLock<TreasuryLedger>,
}

fn parse_address_hex(raw: &str) -> Result<Address> {
    let trimmed = raw.trim().strip_prefix("0x").unwrap_or(raw.trim());
    let bytes = hex::decode(trimmed)
        .map_err(|e| ExecutionError::Block(format!("invalid address hex: {e}")))?;
    if bytes.len() != 20 {
        return Err(ExecutionError::Block("address must be 20 bytes".to_string()));
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&bytes);
    Ok(Address::from_bytes(out))
}

/// 默认域注册：main + 游戏域(shanhai)。
pub fn default_game_domains() -> Arc<InMemoryDomainRegistry> {
    let domains = Arc::new(InMemoryDomainRegistry::new());
    domains.upsert_domain(DomainConfig {
        domain_id: DomainId(0),
        name: "main".to_string(),
        vm: "wasm".to_string(),
        public: true,
    });
    domains.upsert_domain(DomainConfig {
        domain_id: GAME_DOMAIN,
        name: "shanhai".to_string(),
        vm: "shanhai".to_string(),
        public: true,
    });
    domains
}

impl StateExecutor {
    /// Create new state executor（默认内存 compute store + 默认域）。
    pub fn new(state_db: Arc<StateDb>, chain_id: u64) -> Self {
        let store: Arc<dyn ObjectStore> = Arc::new(InMemoryObjectStore::new());
        Self::new_with_compute(
            state_db,
            chain_id,
            store,
            default_game_domains(),
            crate::governance::treasury_address(),
        )
    }

    /// 带共享 compute store 的构造（节点启动时接入真实存储）。
    pub fn new_with_compute(
        state_db: Arc<StateDb>,
        chain_id: u64,
        compute_store: Arc<dyn ObjectStore>,
        domains: Arc<dyn DomainRegistry>,
        treasury: Address,
    ) -> Self {
        Self {
            state_db,
            chain_id,
            compute_store,
            domains,
            treasury,
            ledger: RwLock::new(TreasuryLedger::new()),
        }
    }

    /// 国库账本快照（收入/支出事件流；余额应与 `rabbit_getAccount` 的国库账户一致）。
    pub fn treasury_ledger(&self) -> TreasuryLedger {
        self.ledger.read().expect("treasury ledger lock").clone()
    }

    /// 构造共享存储/域的执行器（BlockTime 产块方与区块校验共用同一后端）。
    pub fn new_basic_executor(
        &self,
    ) -> BasicTxExecutor<Arc<dyn ObjectStore>, DefaultAuthorizationPolicy, NoopResourcePolicy, Arc<dyn DomainRegistry>>
    {
        BasicTxExecutor::new(
            self.compute_store.clone(),
            DefaultAuthorizationPolicy,
            NoopResourcePolicy,
            self.domains.clone(),
        )
    }

    /// 区块执行：逐笔执行 body 计算交易 → 校验 receipts（共识结算）→ 收取费用进国库。
    /// 校验失败返回 Err；成功时 to_root 反映国库余额变更后的账户状态根。
    pub fn execute_block(&self, block: &Block, parent_state_root: Hash) -> Result<StateTransition> {
        tracing::info!(
            "Executing block #{} with body-aware state transition ({} compute txs)",
            block.header.number.as_u64(),
            block.body.as_ref().map(|b| b.transactions.len()).unwrap_or(0)
        );

        let body = block
            .body
            .as_ref()
            .ok_or_else(|| ExecutionError::Block("block body missing".to_string()))?;
        if body.transactions.len() != body.receipts.len() {
            return Err(ExecutionError::Block(
                "block body tx/receipt count mismatch".to_string(),
            ));
        }

        let executor = BasicTxExecutor::new(
            self.compute_store.clone(),
            DefaultAuthorizationPolicy,
            NoopResourcePolicy,
            self.domains.clone(),
        );
        let base_fee = block.header.base_fee_per_gas.as_u64();
        let block_timestamp = block.header.timestamp;
        let (computed, _) = self.execute_txs(&body.transactions, base_fee, block_timestamp, &executor)?;

        for ((tx, computed_receipt), declared_receipt) in body
            .transactions
            .iter()
            .zip(computed.iter())
            .zip(body.receipts.iter())
        {
            if tx.tx_id != declared_receipt.tx_id {
                return Err(ExecutionError::Block(
                    "receipt tx_id does not match body tx".to_string(),
                ));
            }
            self.validate_receipt(tx, computed_receipt, declared_receipt)?;
        }

        Ok(StateTransition {
            from_root: parent_state_root,
            to_root: self.state_db.state_root(),
        })
    }

    /// 执行一组计算交易并返回 receipts（不校验声明；产块方用）。费用收取同步进行。
    /// `block_timestamp` 用于治理生效的确定性判定（FundActivity 窗口/通过检查）。
    pub fn execute_txs(
        &self,
        txs: &[ComputeTx],
        base_fee: u64,
        block_timestamp: u64,
        executor: &BasicTxExecutor<Arc<dyn ObjectStore>, DefaultAuthorizationPolicy, NoopResourcePolicy, Arc<dyn DomainRegistry>>,
    ) -> Result<(Vec<Receipt>, u64)> {
        let mut receipts = Vec::with_capacity(txs.len());
        let mut total_gas = 0u64;
        for tx in txs {
            // 治理效果预检（确定性，失败不产生任何对象/账户变更）：
            // 仅 GAME_DOMAIN Execute（FundActivity）在产块时扣国库。
            let governance_result = self.apply_governance_effects(tx, block_timestamp);
            if let Err(e) = governance_result {
                receipts.push(Receipt {
                    tx_id: tx.tx_id,
                    block_hash: Hash::zero(),
                    status: ReceiptStatus::Failed,
                    gas_used: 0,
                    compute_units: 0,
                    output_refs: vec![],
                    logs: vec![],
                    error: Some(e.to_string()),
                });
                continue;
            }
            // 统一玩法动作预检（确定性）：ActionSettle 重算校验声称结果与掉落。
            if let Err(e) = self.apply_action_effects(tx) {
                receipts.push(Receipt {
                    tx_id: tx.tx_id,
                    block_hash: Hash::zero(),
                    status: ReceiptStatus::Failed,
                    gas_used: 0,
                    compute_units: 0,
                    output_refs: vec![],
                    logs: vec![],
                    error: Some(e.to_string()),
                });
                continue;
            }
            // 山海币经济效果预检（确定性）：转账余额 / 强化扣费余额。
            if let Err(e) = self.apply_economy_effects(tx, base_fee) {
                receipts.push(Receipt {
                    tx_id: tx.tx_id,
                    block_hash: Hash::zero(),
                    status: ReceiptStatus::Failed,
                    gas_used: 0,
                    compute_units: 0,
                    output_refs: vec![],
                    logs: vec![],
                    error: Some(e.to_string()),
                });
                continue;
            }
            match executor.execute(tx) {
                Ok(_) => {
                    let gas_used = crate::compute::estimate_tx_gas(tx);
                    total_gas = total_gas.saturating_add(gas_used);
                    // BlockTime 费用收集：非 Mint 交易按 gas_used × base_fee + priority_fee
                    // （交易税/小费，上限 max_fee）入国库。Mint 价值创造，免收。
                    // 山海币铸币走治理 Execute（普通 Invoke，照常收 gas）。
                    if tx.command != Command::Mint {
                        let fee = self.tx_gas_fee(tx, base_fee);
                        if fee > 0 {
                            self.credit_treasury(fee);
                        }
                    }
                    // 治理效果提交：已通过预检，这里真正生效（扣款/记账/记录产物/铸币）。
                    if let Err(e) = self.commit_governance_effects(tx, block_timestamp) {
                        receipts.push(Receipt {
                            tx_id: tx.tx_id,
                            block_hash: Hash::zero(),
                            status: ReceiptStatus::Failed,
                            gas_used: 0,
                            compute_units: 0,
                            output_refs: vec![],
                            logs: vec![],
                            error: Some(e.to_string()),
                        });
                        continue;
                    }
                    // 山海币经济效果提交（已预检）：转账/强化扣费进国库。
                    if let Err(e) = self.commit_economy_effects(tx, base_fee) {
                        receipts.push(Receipt {
                            tx_id: tx.tx_id,
                            block_hash: Hash::zero(),
                            status: ReceiptStatus::Failed,
                            gas_used: 0,
                            compute_units: 0,
                            output_refs: vec![],
                            logs: vec![],
                            error: Some(e.to_string()),
                        });
                        continue;
                    }
                    receipts.push(Receipt {
                        tx_id: tx.tx_id,
                        block_hash: Hash::zero(), // 产块方回填 header.hash；commitment 剥离该字段
                        status: ReceiptStatus::Success,
                        gas_used,
                        compute_units: 0,
                        output_refs: tx.output_proposals.iter().map(|p| p.output_id).collect(),
                        logs: vec![],
                        error: None,
                    });
                }
                Err(err) => {
                    receipts.push(Receipt {
                        tx_id: tx.tx_id,
                        block_hash: Hash::zero(),
                        status: ReceiptStatus::Failed,
                        gas_used: 0,
                        compute_units: 0,
                        output_refs: vec![],
                        logs: vec![],
                        error: Some(err.to_string()),
                    });
                }
            }
        }
        Ok((receipts, total_gas))
    }

    /// 治理效果预检（产块/校验两阶段一致）：GAME_DOMAIN `Invoke` Execute 交易，
    /// 输入提案对象必须 tally = Passed（窗口已过且赞成 ≥50%）；FundActivity 需国库余额充足。
    fn apply_governance_effects(&self, tx: &ComputeTx, block_timestamp: u64) -> Result<()> {
        if tx.domain_id != GAME_DOMAIN || tx.command != Command::Invoke {
            return Ok(());
        }
        // 非 GameOp 负载（或无效负载）不是治理交易，放行由常规执行路径处理。
        let Ok(op) = GameOp::parse(&tx.payload) else {
            return Ok(());
        };
        let GameOp::Execute { .. } = op else {
            return Ok(());
        };
        let proposal = self.input_proposal(tx)?;
        // 确定性重算 tally（块时间驱动，跨节点一致）。
        let mut p = proposal.clone();
        if tally(&mut p, block_timestamp) != ProposalStatus::Passed {
            return Err(ExecutionError::Block(
                "governance execute rejected: proposal is not passed at block time".to_string(),
            ));
        }
        if let crate::governance::ProposalKind::FundActivity { amount, .. } = &p.kind {
            let balance = self.treasury_balance();
            if balance < *amount as u128 {
                return Err(ExecutionError::Block(format!(
                    "governance execute rejected: insufficient treasury balance {} needed {}",
                    balance, amount
                )));
            }
        }
        // 山海币铸币提案：金额不得超过治理铸币上限（MintPolicy，链上可调）。
        if let crate::governance::ProposalKind::MintShc { amount, .. } = &p.kind {
            let cap = self.mint_policy()?.per_mint_cap;
            if *amount > cap {
                return Err(ExecutionError::Block(format!(
                    "mint denied: proposal amount {} exceeds per-mint cap {}",
                    amount, cap
                )));
            }
        }
        Ok(())
    }

    /// 治理效果提交：FundActivity 真正从国库账户扣款并记入账本（预检已通过）。
    fn commit_governance_effects(&self, tx: &ComputeTx, block_timestamp: u64) -> Result<()> {
        if tx.domain_id != GAME_DOMAIN || tx.command != Command::Invoke {
            return Ok(());
        }
        // 非 GameOp 负载（或无效负载）不是治理交易，放行由常规执行路径处理。
        let Ok(op) = GameOp::parse(&tx.payload) else {
            return Ok(());
        };
        let GameOp::Execute { .. } = op else {
            return Ok(());
        };
        let mut proposal = self.input_proposal(tx)?;
        // 链上提案对象 status 恒为 Active，Passed 是块时间派生的判定；
        // 与预检一致先 tally（Active -> Passed），再交给 execute_proposal 生效。
        if tally(&mut proposal, block_timestamp) != ProposalStatus::Passed {
            return Err(ExecutionError::Block(
                "governance execute commit: proposal not passed at block time".to_string(),
            ));
        }
        // 1) FundActivity：先从国库账户扣款（StateDb；预检已保证余额充足）。
        if let crate::governance::ProposalKind::FundActivity { amount, .. } = &proposal.kind {
            self.debit_treasury(*amount)?;
        }
        // 2) MintShc：投票批准的铸币入账（价值创造；预检已校验治理上限）。
        if let crate::governance::ProposalKind::MintShc { to, amount } = &proposal.kind {
            let target = parse_address_hex(to)?;
            self.credit_account(target, *amount);
        }
        // 3) 账本：应用完整治理效果（FundActivity 记支出；全部类型记生效产物），
        //    与 execute_proposal 同一纯函数，确定性跨节点一致。
        let mut ledger = self.ledger.write().expect("treasury ledger lock");
        let outcome = crate::governance::execute_proposal(&mut proposal, &mut ledger)
            .map_err(|e| {
                ExecutionError::Block(format!("governance execute commit failed: {e}"))
            })?;
        ledger.record_execution(outcome);
        Ok(())
    }

    /// 读取交易输入集中的提案对象（GAME_DOMAIN Execute 的 input_set[0]）。
    fn input_proposal(&self, tx: &ComputeTx) -> Result<Proposal> {
        let input = tx.input_set.first().ok_or_else(|| {
            ExecutionError::Block("governance execute tx missing input proposal".to_string())
        })?;
        let obj = self.compute_store.get_output(*input).ok_or_else(|| {
            ExecutionError::Block("governance execute input proposal not found".to_string())
        })?;
        serde_json::from_slice(&obj.state)
            .map_err(|e| ExecutionError::Block(format!("invalid proposal object state: {e}")))
    }

    /// 国库账户当前余额。
    fn treasury_balance(&self) -> u128 {
        self.state_db
            .get_account(&self.treasury)
            .map(|a| a.balance.as_u128())
            .unwrap_or(0)
    }

    /// 校验单笔 receipt 与重算一致（共识结算：任何节点重跑一致）。
    fn validate_receipt(&self, tx: &ComputeTx, computed: &Receipt, declared: &Receipt) -> Result<()> {
        if computed.status != declared.status
            || computed.gas_used != declared.gas_used
            || computed.output_refs != declared.output_refs
            || computed.error != declared.error
        {
            return Err(ExecutionError::Block(format!(
                "compute receipt mismatch for tx 0x{} (computed: {:?}, declared: {:?})",
                hex::encode(tx.tx_id.0.as_bytes()),
                (computed.status, computed.gas_used, computed.output_refs.len(), computed.error.clone()),
                (declared.status, declared.gas_used, declared.output_refs.len(), declared.error.clone())
            )));
        }
        Ok(())
    }

    /// 统一玩法动作预检（确定性，跨节点一致）：`ActionSettle` 重算
    /// `execute_action`（打怪/强化统一规则），校验声称结果与掉落。
    /// （seed 的"先承诺后揭晓"块校验由 RPC gate 把关——gate 有块存储；
    ///   执行器保证结果确定性：伪造结果/掉落在此拒绝。）
    fn apply_action_effects(&self, tx: &ComputeTx) -> Result<()> {
        if tx.domain_id != GAME_DOMAIN || tx.command != Command::Invoke {
            return Ok(());
        }
        let Ok(op) = GameOp::parse(&tx.payload) else {
            return Ok(());
        };
        let GameOp::ActionSettle { session_id, seed, claimed, drops, .. } = op else {
            return Ok(());
        };
        let input = tx.input_set.first().ok_or_else(|| {
            ExecutionError::Block("action settle missing input session".to_string())
        })?;
        let obj = self.compute_store.get_output(*input).ok_or_else(|| {
            ExecutionError::Block("action settle input session not found".to_string())
        })?;
        let session: crate::game::ActionSession = serde_json::from_slice(&obj.state)
            .map_err(|e| ExecutionError::Block(format!("invalid action session state: {e}")))?;
        if session.session_id != session_id {
            return Err(ExecutionError::Block("action settle session_id mismatch".to_string()));
        }
        if session.settled {
            return Err(ExecutionError::Block("action session already settled".to_string()));
        }
        let cfg = match &session.action_type {
            crate::game::ActionKind::Enhance => {
                let config_id = crate::game::enhance_config_object_id();
                let cfg_obj = self
                    .compute_store
                    .get_latest_output_by_object(config_id)
                    .ok_or_else(|| {
                        ExecutionError::Block("enhance config object not found".to_string())
                    })?;
                serde_json::from_slice(&cfg_obj.state).map_err(|e| {
                    ExecutionError::Block(format!("invalid enhance config state: {e}"))
                })?
            }
            _ => crate::game::EnhanceConfig::default(),
        };
        let outcome = crate::game::execute_action(&session.action_type, &session.inputs, seed, &cfg)
            .map_err(|e| ExecutionError::Block(format!("action rejected: {e}")))?;
        if outcome.result != claimed {
            return Err(ExecutionError::Block(
                "action claimed result mismatch".to_string(),
            ));
        }
        if outcome.drops != drops {
            return Err(ExecutionError::Block(
                "action claimed drops mismatch".to_string(),
            ));
        }
        Ok(())
    }

    /// 交易 gas 费（BlockTime 统一公式）：gas_used × base_fee + priority_fee，上限 max_fee。
    fn tx_gas_fee(&self, tx: &ComputeTx, base_fee: u64) -> u64 {
        crate::compute::estimate_tx_gas(tx)
            .saturating_mul(base_fee)
            .saturating_add(tx.priority_fee)
            .min(tx.max_fee)
    }

    /// 山海币经济效果预检（确定性，失败不产生任何状态变更）：
    /// - `Enhance`：强化成本（cost_sh，来自链上配置）+ gas 费必须由签名者余额承担
    /// - `TransferCoin`：发送者余额必须覆盖转账金额 + gas 费
    fn apply_economy_effects(&self, tx: &ComputeTx, base_fee: u64) -> Result<()> {
        if tx.domain_id != GAME_DOMAIN || tx.command != Command::Invoke {
            return Ok(());
        }
        let Ok(op) = GameOp::parse(&tx.payload) else {
            return Ok(());
        };
        match op {
            GameOp::TransferCoin { amount, .. } => {
                let gas = self.tx_gas_fee(tx, base_fee);
                let payer = crate::game::ed25519_signer_address(tx).ok_or_else(|| {
                    ExecutionError::Block("transfer requires an ed25519 signer".to_string())
                })?;
                let balance = self
                    .state_db
                    .get_account(&payer)
                    .map(|a| a.balance.as_u64())
                    .unwrap_or(0);
                if balance < amount.saturating_add(gas) {
                    return Err(ExecutionError::Block(format!(
                        "insufficient shc balance {} needed {} for transfer",
                        balance,
                        amount.saturating_add(gas)
                    )));
                }
                Ok(())
            }
            GameOp::Enhance { current_level, .. } | GameOp::ZkEnhance { current_level, .. } => {
                let cost = self.enhance_cost(current_level)?;
                let gas = self.tx_gas_fee(tx, base_fee);
                let payer = crate::game::ed25519_signer_address(tx).ok_or_else(|| {
                    ExecutionError::Block("enhance requires an ed25519 signer".to_string())
                })?;
                let balance = self
                    .state_db
                    .get_account(&payer)
                    .map(|a| a.balance.as_u64())
                    .unwrap_or(0);
                if balance < cost.saturating_add(gas) {
                    return Err(ExecutionError::Block(format!(
                        "insufficient shc balance {} needed {} for enhance",
                        balance,
                        cost.saturating_add(gas)
                    )));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// 山海币经济效果提交（已预检）：
    /// - `Enhance`：从签名者扣强化成本 + gas 费 → 国库（sink）
    /// - `TransferCoin`：从发送者扣转账金额 + gas 费，金额入接收者
    fn commit_economy_effects(&self, tx: &ComputeTx, base_fee: u64) -> Result<()> {
        if tx.domain_id != GAME_DOMAIN || tx.command != Command::Invoke {
            return Ok(());
        }
        let Ok(op) = GameOp::parse(&tx.payload) else {
            return Ok(());
        };
        match op {
            GameOp::TransferCoin { to, amount } => {
                let gas = self.tx_gas_fee(tx, base_fee);
                let payer = crate::game::ed25519_signer_address(tx).ok_or_else(|| {
                    ExecutionError::Block("transfer requires an ed25519 signer".to_string())
                })?;
                let target = parse_address_hex(&to)?;
                self.debit_account(payer, amount.saturating_add(gas))?;
                self.credit_account(target, amount);
                Ok(())
            }
            GameOp::Enhance { current_level, .. } | GameOp::ZkEnhance { current_level, .. } => {
                let cost = self.enhance_cost(current_level)?;
                let gas = self.tx_gas_fee(tx, base_fee);
                let payer = crate::game::ed25519_signer_address(tx).ok_or_else(|| {
                    ExecutionError::Block("enhance requires an ed25519 signer".to_string())
                })?;
                self.debit_account(payer, cost.saturating_add(gas))?;
                self.credit_account(self.treasury, cost);
                // 强化成本入国库账本（SHC sink，与账户余额一致：账本 = 账户）。
                self.ledger
                    .write()
                    .expect("treasury ledger lock")
                    .record_income("shc_enhance_cost", cost);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// 铸币政策（山海币）：读取链上 MintPolicy 对象的 per_mint_cap（治理可调）。
    fn mint_policy(&self) -> Result<crate::game::MintPolicy> {
        let config_id = crate::game::mint_policy_object_id();
        let obj = self
            .compute_store
            .get_latest_output_by_object(config_id)
            .ok_or_else(|| {
                ExecutionError::Block("mint policy object not found".to_string())
            })?;
        serde_json::from_slice(&obj.state).map_err(|e| {
            ExecutionError::Block(format!("invalid mint policy state: {e}"))
        })
    }

    /// 强化成本（山海币）：读取链上 EnhanceConfig 的 cost_sh[level]。
    fn enhance_cost(&self, current_level: u8) -> Result<u64> {
        let config_id = crate::game::enhance_config_object_id();
        let obj = self
            .compute_store
            .get_latest_output_by_object(config_id)
            .ok_or_else(|| {
                ExecutionError::Block("enhance config object not found".to_string())
            })?;
        let cfg: crate::game::EnhanceConfig = serde_json::from_slice(&obj.state).map_err(|e| {
            ExecutionError::Block(format!("invalid enhance config state: {e}"))
        })?;
        Ok(cfg.cost_sh[current_level.min(11) as usize] as u64)
    }

    /// 账户余额增加（山海币）。
    fn credit_account(&self, address: Address, amount: u64) {
        let mut account = self.state_db.get_account(&address).unwrap_or_default();
        account.balance = account.balance.saturating_add(U256::from(amount));
        self.state_db.insert_account(address, account);
    }

    /// 账户余额扣减（山海币），余额不足返回 Err（不改变状态）。
    fn debit_account(&self, address: Address, amount: u64) -> Result<()> {
        let mut account = self.state_db.get_account(&address).unwrap_or_default();
        if account.balance < U256::from(amount) {
            return Err(ExecutionError::Block(format!(
                "insufficient shc balance {} needed {}",
                account.balance, amount
            )));
        }
        account.balance = account.balance.saturating_sub(U256::from(amount));
        self.state_db.insert_account(address, account);
        Ok(())
    }

    /// 费用入国库账户（确定性，跨节点一致），并同步记入国库账本（审计视图）。
    fn credit_treasury(&self, fee: u64) {
        let mut account = self.state_db.get_account(&self.treasury).unwrap_or_default();
        account.balance = account.balance.saturating_add(U256::from(fee));
        self.state_db.insert_account(self.treasury, account);
        self.ledger
            .write()
            .expect("treasury ledger lock")
            .record_income("block_gas_fee", fee);
    }

    /// 从国库账户扣款（FundActivity 生效执行；确定性，跨节点一致）。
    fn debit_treasury(&self, amount: u64) -> Result<()> {
        let mut account = self.state_db.get_account(&self.treasury).unwrap_or_default();
        if account.balance < U256::from(amount) {
            return Err(ExecutionError::Block(format!(
                "insufficient treasury balance {} needed {}",
                account.balance, amount
            )));
        }
        account.balance = account.balance.saturating_sub(U256::from(amount));
        self.state_db.insert_account(self.treasury, account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{Block, BlockBody, BlockHeader};
    use crate::compute::{
        ObjectId, ObjectKind, OutputId, OutputProposal, Ownership, Script, TxId, TxSignature,
        TxWitness, Version,
    };

    fn authority() -> (crate::crypto::Address, ed25519_dalek::SigningKey) {
        use ed25519_dalek::SigningKey;
        let key = SigningKey::from_bytes(&[0x11; 32]);
        let public_key = key.verifying_key().to_bytes();
        let hash = crate::crypto::keccak256(&public_key);
        (
            crate::crypto::Address::from_slice(&hash[12..]).expect("address"),
            key,
        )
    }

    fn proposal(
        authority: &crate::crypto::Address,
        object_id: ObjectId,
        version: u64,
        predecessor: Option<OutputId>,
        state: Vec<u8>,
    ) -> OutputProposal {
        OutputProposal {
            output_id: OutputId(crate::crypto::Hash::from_bytes(crate::crypto::keccak256(
                &[object_id.0.as_bytes(), &version.to_be_bytes()].concat(),
            ))),
            object_id,
            domain_id: GAME_DOMAIN,
            kind: ObjectKind::State,
            owner: Ownership::Address(*authority),
            predecessor,
            version: Version(version),
            state,
            state_root: None,
            resources: vec![],
            lock: Script::default(),
            logic: None,
            created_at: 0,
            ttl: None,
            rent_reserve: None,
            flags: 0,
            extensions: vec![],
        }
    }

    fn proposal_with_resources(
        authority: &crate::crypto::Address,
        object_id: ObjectId,
        version: u64,
        predecessor: Option<OutputId>,
        state: Vec<u8>,
        native_amount: u128,
    ) -> OutputProposal {
        let mut p = proposal(authority, object_id, version, predecessor, state);
        p.resources = vec![(Hash::zero(), crate::compute::ResourceValue::Amount(native_amount))];
        p
    }

    fn output_id_for_obj(object_id: &ObjectId, version: u64) -> OutputId {
        let mut data = Vec::with_capacity(40);
        data.extend_from_slice(object_id.0.as_bytes());
        data.extend_from_slice(&version.to_be_bytes());
        OutputId(crate::crypto::Hash::from_bytes(crate::crypto::keccak256(&data)))
    }

    fn sign(tx: ComputeTx, key: &ed25519_dalek::SigningKey) -> ComputeTx {
        use ed25519_dalek::Signer as _;
        let signature = key.sign(&tx.signing_preimage()).to_bytes();
        let public_key = key.verifying_key().to_bytes();
        let mut tx = tx;
        tx.witness.signatures = vec![TxSignature::ed25519(signature, public_key)];
        tx.with_expected_tx_id()
    }

    fn mint_session_tx(
        authority: &crate::crypto::Address,
        key: &ed25519_dalek::SigningKey,
        object_id: ObjectId,
        nonce: u64,
    ) -> ComputeTx {
        let tx = ComputeTx {
            tx_id: TxId(crate::crypto::Hash::zero()),
            domain_id: GAME_DOMAIN,
            command: Command::Mint,
            input_set: vec![],
            read_set: vec![],
            output_proposals: vec![proposal(authority, object_id, 1, None, b"session".to_vec())],
            fee: 0,
            nonce: Some(nonce),
            metadata: vec![],
            payload: vec![],
            deadline_unix_secs: None,
            chain_id: Some(10088),
            network_id: Some(10088),
            witness: TxWitness { signatures: vec![], threshold: None },
            max_fee: 0,
            priority_fee: 0,
            gas_limit: 0,
        };
        sign(tx, key)
    }

    fn settle_tx(
        authority: &crate::crypto::Address,
        key: &ed25519_dalek::SigningKey,
        object_id: ObjectId,
        input: OutputId,
        nonce: u64,
    ) -> ComputeTx {
        settle_tx_with_tax(authority, key, object_id, input, nonce, 0)
    }

    fn settle_tx_with_tax(
        authority: &crate::crypto::Address,
        key: &ed25519_dalek::SigningKey,
        object_id: ObjectId,
        input: OutputId,
        nonce: u64,
        priority_fee: u64,
    ) -> ComputeTx {
        let tx = ComputeTx {
            tx_id: TxId(crate::crypto::Hash::zero()),
            domain_id: GAME_DOMAIN,
            command: Command::Invoke,
            input_set: vec![input],
            read_set: vec![],
            output_proposals: vec![proposal(authority, object_id, 2, Some(input), b"result".to_vec())],
            fee: 0,
            nonce: Some(nonce),
            metadata: vec![],
            payload: b"{}".to_vec(),
            deadline_unix_secs: None,
            chain_id: Some(10088),
            network_id: Some(10088),
            witness: TxWitness { signatures: vec![], threshold: None },
            max_fee: 1_000_000_000,
            priority_fee,
            gas_limit: 100_000,
        };
        sign(tx, key)
    }

    fn build_block(transactions: Vec<ComputeTx>, receipts: Vec<Receipt>) -> Block {
        let mut header = BlockHeader {
            version: 3,
            parent_hash: crate::crypto::Hash::zero(),
            uncle_hashes: vec![],
            coinbase: crate::crypto::Address::default(),
            state_root: crate::crypto::Hash::zero(),
            transactions_root: crate::crypto::Hash::zero(),
            receipts_root: crate::crypto::Hash::zero(),
            number: U256::from(1),
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp: 1_700_000_000,
            difficulty: U256::from(1),
            nonce: 0,
            extra_data: vec![],
            mix_hash: crate::crypto::Hash::zero(),
            base_fee_per_gas: U256::from(1),
            hash: crate::crypto::Hash::zero(),
        };
        let body = BlockBody {
            version: 1,
            transactions,
            receipts,
        };
        header.apply_body_commitments(&body);
        let mut block = Block::new_with_body(header, body);
        for r in block.body.as_mut().unwrap().receipts.iter_mut() {
            r.block_hash = block.header.hash;
        }
        block
    }

    fn make_receipts(txs: &[ComputeTx]) -> Vec<Receipt> {
        txs.iter()
            .map(|tx| Receipt {
                tx_id: tx.tx_id,
                block_hash: crate::crypto::Hash::zero(),
                status: ReceiptStatus::Success,
                gas_used: crate::compute::estimate_tx_gas(tx),
                compute_units: 0,
                output_refs: tx.output_proposals.iter().map(|p| p.output_id).collect(),
                logs: vec![],
                error: None,
            })
            .collect()
    }

    #[test]
    fn block_with_compute_txs_executes_and_collects_fee() {
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let executor = StateExecutor::new(state_db.clone(), 10088);
        let (authority, key) = authority();
        let object_id = ObjectId(crate::crypto::Hash::from_bytes(crate::crypto::keccak256(b"obj")));

        let mint = mint_session_tx(&authority, &key, object_id, 1);
        let input = mint.output_proposals[0].output_id;
        let settle = settle_tx(&authority, &key, object_id, input, 2);
        let txs = vec![mint, settle];
        let receipts = make_receipts(&txs);

        let block = build_block(txs, receipts);
        let transition = executor.execute_block(&block, Hash::zero()).expect("block executes");

        // 国库收到 settle 交易的 gas 费用（mint 免费用）
        let treasury_balance = state_db.get_balance(&crate::governance::treasury_address());
        let gas_used = crate::compute::estimate_tx_gas(&block.body.as_ref().unwrap().transactions[1]);
        assert_eq!(treasury_balance.as_u64(), gas_used); // base_fee=1
        assert_eq!(transition.to_root, state_db.state_root());
    }

    #[test]
    fn priority_fee_is_credited_to_treasury_as_tax() {
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let executor = StateExecutor::new(state_db.clone(), 10088);
        let (authority, key) = authority();
        let object_id = ObjectId(crate::crypto::Hash::from_bytes(crate::crypto::keccak256(b"taxobj")));

        // 会话对象携带原生价值 100_000，结果对象带走 95_000，留 5_000 作 priority_fee（税）
        let mint_tx = ComputeTx {
            tx_id: TxId(crate::crypto::Hash::zero()),
            domain_id: GAME_DOMAIN,
            command: Command::Mint,
            input_set: vec![],
            read_set: vec![],
            output_proposals: vec![proposal_with_resources(
                &authority,
                object_id,
                1,
                None,
                b"session".to_vec(),
                100_000,
            )],
            fee: 0,
            nonce: Some(1),
            metadata: vec![],
            payload: vec![],
            deadline_unix_secs: None,
            chain_id: Some(10088),
            network_id: Some(10088),
            witness: TxWitness { signatures: vec![], threshold: None },
            max_fee: 0,
            priority_fee: 0,
            gas_limit: 0,
        };
        let mint = sign(mint_tx, &key);
        let input = mint.output_proposals[0].output_id;
        // 交易税（市场税）via priority_fee，签名前设置；结果对象带走 95_000
        let settle_tx = ComputeTx {
            tx_id: TxId(crate::crypto::Hash::zero()),
            domain_id: GAME_DOMAIN,
            command: Command::Invoke,
            input_set: vec![input],
            read_set: vec![],
            output_proposals: vec![proposal_with_resources(
                &authority,
                object_id,
                2,
                Some(input),
                b"result".to_vec(),
                95_000,
            )],
            fee: 0,
            nonce: Some(2),
            metadata: vec![],
            payload: b"{}".to_vec(),
            deadline_unix_secs: None,
            chain_id: Some(10088),
            network_id: Some(10088),
            witness: TxWitness { signatures: vec![], threshold: None },
            max_fee: 1_000_000_000,
            priority_fee: 5_000,
            gas_limit: 100_000,
        };
        let settle = sign(settle_tx, &key);
        let txs = vec![mint, settle];
        let receipts = make_receipts(&txs);
        let block = build_block(txs, receipts);
        executor.execute_block(&block, Hash::zero()).expect("block executes");

        // 国库 = gas_used × base_fee(1) + priority_fee(5000)，封顶 max_fee
        let treasury = state_db.get_balance(&crate::governance::treasury_address()).as_u64();
        let gas_used = crate::compute::estimate_tx_gas(&block.body.as_ref().unwrap().transactions[1]);
        assert_eq!(treasury, gas_used.saturating_add(5_000));
        // 账本支出/收入不变量
        let ledger = executor.treasury_ledger();
        assert_eq!(ledger.balance(), treasury as u128);
    }

    #[test]
    fn tampered_receipt_is_rejected() {
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let executor = StateExecutor::new(state_db.clone(), 10088);
        let (authority, key) = authority();
        let object_id = ObjectId(crate::crypto::Hash::from_bytes(crate::crypto::keccak256(b"obj2")));

        let mint = mint_session_tx(&authority, &key, object_id, 1);
        let input = mint.output_proposals[0].output_id;
        let settle = settle_tx(&authority, &key, object_id, input, 2);
        let mut receipts = make_receipts(&[mint.clone(), settle.clone()]);
        // 篡改：声称 settle 失败
        receipts[1].status = ReceiptStatus::Failed;
        receipts[1].error = Some("forged".to_string());

        let block = build_block(vec![mint, settle], receipts);
        let err = executor.execute_block(&block, Hash::zero()).expect_err("tampered receipt rejected");
        assert!(err.to_string().contains("receipt mismatch"));
    }

    #[test]
    fn failed_tx_receipt_must_match_deterministically() {
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let executor = StateExecutor::new(state_db.clone(), 10088);
        let key = {
            use ed25519_dalek::SigningKey;
            SigningKey::from_bytes(&[0x12; 32])
        };

        // 引用不存在的输入 → 执行必然失败
        let tx = ComputeTx {
            tx_id: TxId(crate::crypto::Hash::zero()),
            domain_id: GAME_DOMAIN,
            command: Command::Invoke,
            input_set: vec![OutputId(crate::crypto::Hash::from_bytes([9; 32]))],
            read_set: vec![],
            output_proposals: vec![],
            fee: 0,
            nonce: Some(1),
            metadata: vec![],
            payload: vec![],
            deadline_unix_secs: None,
            chain_id: Some(10088),
            network_id: Some(10088),
            witness: TxWitness { signatures: vec![], threshold: None },
            max_fee: 0,
            priority_fee: 0,
            gas_limit: 0,
        };
        let tx = sign(tx, &key);

        // 先手动跑一次拿真实错误，再构造匹配的失败 receipt
        let probe_store = Arc::new(InMemoryObjectStore::new());
        let probe_executor = BasicTxExecutor::new(
            probe_store,
            DefaultAuthorizationPolicy,
            NoopResourcePolicy,
            default_game_domains(),
        );
        let err = probe_executor.execute(&tx).expect_err("must fail");
        let err_str = err.to_string();

        let receipts = vec![Receipt {
            tx_id: tx.tx_id,
            block_hash: crate::crypto::Hash::zero(),
            status: ReceiptStatus::Failed,
            gas_used: 0,
            compute_units: 0,
            output_refs: vec![],
            logs: vec![],
            error: Some(err_str),
        }];
        let block = build_block(vec![tx], receipts);
        executor.execute_block(&block, Hash::zero()).expect("failure receipt matches");
    }

    // ── 山海币经济（治理铸币 + 强化扣费 + 转账）────────────────────────

    /// 治理铸币（投票批准）：构造 [propose MintShc, vote, execute] 三笔交易
    /// （提案对象 v1→v2→v3），execute 时目标账户入账 amount。返回三笔交易。
    fn mint_shc_trip(
        authority: &crate::crypto::Address,
        key: &ed25519_dalek::SigningKey,
        to: crate::crypto::Address,
        amount: u64,
        nonce: u64,
    ) -> Vec<ComputeTx> {
        let id = format!("mint-{nonce}");
        let object_id = ObjectId(crate::crypto::Hash::from_bytes(crate::crypto::keccak256(
            format!("shanhai/proposal/{id}").as_bytes(),
        )));
        let proposer = format!("0x{}", hex::encode(authority.as_bytes()));
        let p = crate::governance::create_proposal(
            &id,
            crate::governance::ProposalKind::MintShc {
                to: format!("0x{}", hex::encode(to.as_bytes())),
                amount,
            },
            &proposer,
            1000,
            0, // created_at=0：块时间(大) 下窗口已过 → tally Passed
        )
        .expect("mint proposal");
        let propose = ComputeTx {
            tx_id: TxId(crate::crypto::Hash::zero()),
            domain_id: GAME_DOMAIN,
            command: Command::Mint,
            input_set: vec![],
            read_set: vec![],
            output_proposals: vec![proposal(
                authority,
                object_id,
                1,
                None,
                serde_json::to_vec(&p).expect("proposal json"),
            )],
            fee: 0,
            nonce: Some(nonce),
            metadata: vec![],
            payload: vec![],
            deadline_unix_secs: None,
            chain_id: Some(10088),
            network_id: Some(10088),
            witness: TxWitness { signatures: vec![], threshold: None },
            max_fee: 0,
            priority_fee: 0,
            gas_limit: 0,
        };
        let propose = sign(propose, key);
        let v1 = propose.output_proposals[0].output_id;
        let mut v2 = p.clone();
        v2.votes_for = 500;
        let vote = governance_invoke(
            authority,
            key,
            object_id,
            2,
            Some(v1),
            nonce + 1,
            serde_json::to_vec(&GameOp::Vote {
                proposal_id: id.clone(),
                voter: proposer.clone(),
                stake: 500,
                approve: true,
            })
            .expect("vote op"),
            serde_json::to_vec(&v2).expect("proposal json"),
        );
        let v2_out = vote.output_proposals[0].output_id;
        let mut v3 = v2.clone();
        v3.status = ProposalStatus::Executed;
        let execute = governance_invoke(
            authority,
            key,
            object_id,
            3,
            Some(v2_out),
            nonce + 2,
            serde_json::to_vec(&GameOp::Execute {
                proposal_id: id.clone(),
            })
            .expect("execute op"),
            serde_json::to_vec(&v3).expect("proposal json"),
        );
        vec![propose, vote, execute]
    }

    fn mint_policy_tx(
        authority: &crate::crypto::Address,
        key: &ed25519_dalek::SigningKey,
        cap: u64,
        nonce: u64,
    ) -> ComputeTx {
        let object_id = crate::game::mint_policy_object_id();
        let policy = crate::game::MintPolicy { version: 1, per_mint_cap: cap };
        let tx = ComputeTx {
            tx_id: TxId(crate::crypto::Hash::zero()),
            domain_id: GAME_DOMAIN,
            command: Command::Mint,
            input_set: vec![],
            read_set: vec![],
            output_proposals: vec![proposal(
                authority,
                object_id,
                1,
                None,
                serde_json::to_vec(&policy).expect("policy json"),
            )],
            fee: 0,
            nonce: Some(nonce),
            metadata: vec![],
            payload: vec![],
            deadline_unix_secs: None,
            chain_id: Some(10088),
            network_id: Some(10088),
            witness: TxWitness { signatures: vec![], threshold: None },
            max_fee: 0,
            priority_fee: 0,
            gas_limit: 0,
        };
        sign(tx, key)
    }

    fn mint_config_tx(
        authority: &crate::crypto::Address,
        key: &ed25519_dalek::SigningKey,
        nonce: u64,
    ) -> ComputeTx {
        let object_id = crate::game::enhance_config_object_id();
        let cfg = crate::game::EnhanceConfig::default();
        let tx = ComputeTx {
            tx_id: TxId(crate::crypto::Hash::zero()),
            domain_id: GAME_DOMAIN,
            command: Command::Mint,
            input_set: vec![],
            read_set: vec![],
            output_proposals: vec![proposal(
                authority,
                object_id,
                1,
                None,
                serde_json::to_vec(&cfg).expect("config json"),
            )],
            fee: 0,
            nonce: Some(nonce),
            metadata: vec![],
            payload: vec![],
            deadline_unix_secs: None,
            chain_id: Some(10088),
            network_id: Some(10088),
            witness: TxWitness { signatures: vec![], threshold: None },
            max_fee: 0,
            priority_fee: 0,
            gas_limit: 0,
        };
        sign(tx, key)
    }

    #[test]
    fn mint_shc_proposal_executes_and_credits_balance() {
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let executor = StateExecutor::new(state_db.clone(), 10088);
        let (authority, key) = authority();
        let (target, target_key) = {
            use ed25519_dalek::SigningKey;
            let k = SigningKey::from_bytes(&[0x33; 32]);
            let pk = k.verifying_key().to_bytes();
            let h = crate::crypto::keccak256(&pk);
            (crate::crypto::Address::from_slice(&h[12..]).unwrap(), k)
        };

        // 铸币政策（上限）+ 治理铸币 [propose, vote, execute] → 目标地址入账。
        // 块时间 > 72h 窗口，tally = Passed。
        let policy = mint_policy_tx(&authority, &key, 1_000_000, 1);
        let trip = mint_shc_trip(&authority, &key, target, 1_000, 2);
        let basic = executor.new_basic_executor();
        let (receipts, _) = executor
            .execute_txs(&[policy, trip[0].clone(), trip[1].clone(), trip[2].clone()], 1, 1_700_000_000, &basic)
            .expect("execute mint trip");
        assert_eq!(receipts[0].status, ReceiptStatus::Success, "policy: {:?}", receipts[0].error);
        assert_eq!(receipts[1].status, ReceiptStatus::Success, "propose: {:?}", receipts[1].error);
        assert_eq!(receipts[2].status, ReceiptStatus::Success, "vote: {:?}", receipts[2].error);
        assert_eq!(receipts[3].status, ReceiptStatus::Success, "execute: {:?}", receipts[3].error);
        assert_eq!(state_db.get_balance(&target).as_u64(), 1_000, "vote-approved mint credited");
        // 生效产物记录 MintShcExecuted；账本 == 账户（vote/execute 的 gas 入国库）
        let ledger = executor.treasury_ledger();
        assert_eq!(
            ledger.executions(),
            &[crate::governance::ProposalOutcome::MintShcExecuted {
                to: format!("0x{}", hex::encode(target.as_bytes())),
                amount: 1_000,
            }]
        );
        let account = state_db.get_balance(&crate::governance::treasury_address()).as_u64();
        assert_eq!(ledger.balance(), account as u128, "ledger/account invariant");
        let _ = target_key;
    }

    #[test]
    fn mint_shc_over_cap_proposal_rejected() {
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let executor = StateExecutor::new(state_db.clone(), 10088);
        let (authority, key) = authority();
        let (target, _tk) = {
            use ed25519_dalek::SigningKey;
            let k = SigningKey::from_bytes(&[0x66; 32]);
            let pk = k.verifying_key().to_bytes();
            let h = crate::crypto::keccak256(&pk);
            (crate::crypto::Address::from_slice(&h[12..]).unwrap(), k)
        };
        // 治理策略：单笔上限 500；提案铸 1000 → execute 被拒（超上限）
        let policy = mint_policy_tx(&authority, &key, 500, 1);
        let trip = mint_shc_trip(&authority, &key, target, 1_000, 2);
        let basic = executor.new_basic_executor();
        let (receipts, _) = executor
            .execute_txs(&[policy, trip[0].clone(), trip[1].clone(), trip[2].clone()], 1, 1_700_000_000, &basic)
            .expect("execute economy");
        assert_eq!(receipts[0].status, ReceiptStatus::Success, "policy");
        assert_eq!(receipts[1].status, ReceiptStatus::Success, "propose");
        assert_eq!(receipts[2].status, ReceiptStatus::Success, "vote");
        assert_eq!(receipts[3].status, ReceiptStatus::Failed, "execute over cap must fail");
        assert!(
            receipts[3].error.as_deref().unwrap_or("").contains("exceeds per-mint cap"),
            "{:?}",
            receipts[3].error
        );
        assert_eq!(state_db.get_balance(&target).as_u64(), 0, "no credit on rejection");
    }

    #[test]
    fn enhance_debits_shc_cost_to_treasury() {
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let executor = StateExecutor::new(state_db.clone(), 10088);
        let (authority, auth_key) = authority();
        let (player_addr, player_key) = {
            use ed25519_dalek::SigningKey;
            let k = SigningKey::from_bytes(&[0x66; 32]);
            let pk = k.verifying_key().to_bytes();
            let h = crate::crypto::keccak256(&pk);
            (crate::crypto::Address::from_slice(&h[12..]).unwrap(), k)
        };

        // 配置对象 + 铸币政策（上限覆盖铸币量）+ 治理铸币给玩家 + 玩家自有装备
        let config = mint_config_tx(&authority, &auth_key, 1);
        let policy = mint_policy_tx(&authority, &auth_key, 2_000_000, 2);
        let trip = mint_shc_trip(&authority, &auth_key, player_addr, 1_000_000, 3);
        let vote_gas = crate::compute::estimate_tx_gas(&trip[1]);
        let execute_gas = crate::compute::estimate_tx_gas(&trip[2]);
        let equip_obj = ObjectId(crate::crypto::Hash::from_bytes(crate::crypto::keccak256(b"shc-eq")));
        let equip = ComputeTx {
            tx_id: TxId(crate::crypto::Hash::zero()),
            domain_id: GAME_DOMAIN,
            command: Command::Mint,
            input_set: vec![],
            read_set: vec![],
            output_proposals: vec![proposal_with_resources(
                &player_addr,
                equip_obj,
                1,
                None,
                b"equip".to_vec(),
                0,
            )],
            fee: 0,
            nonce: Some(6),
            metadata: vec![],
            payload: vec![],
            deadline_unix_secs: None,
            chain_id: Some(10088),
            network_id: Some(10088),
            witness: TxWitness { signatures: vec![], threshold: None },
            max_fee: 0,
            priority_fee: 0,
            gas_limit: 0,
        };
        let equip = sign(equip, &player_key);

        // 玩家强化（level 0 → cost_sh[0] = 10），玩家签名
        let seed = 7u64;
        let r = crate::game::roll_with_config(0, 0, seed, 0, &crate::game::EnhanceConfig::default());
        let op = GameOp::Enhance {
            object_id: format!("0x{}", hex::encode(equip_obj.0.as_bytes())),
            current_level: 0,
            current_pity: 0,
            star_stones: 0,
            seed,
            claimed_success: r.success,
            claimed_new_level: r.new_level,
            claimed_pity: r.pity,
            rules_version: 1,
        };
        let enhance = ComputeTx {
            tx_id: TxId(crate::crypto::Hash::zero()),
            domain_id: GAME_DOMAIN,
            command: Command::Invoke,
            input_set: vec![output_id_for_obj(&equip_obj, 1)],
            read_set: vec![],
            output_proposals: vec![proposal_with_resources(
                &player_addr,
                equip_obj,
                2,
                Some(output_id_for_obj(&equip_obj, 1)),
                b"equip2".to_vec(),
                0,
            )],
            fee: 0,
            nonce: Some(7),
            metadata: vec![],
            payload: serde_json::to_vec(&op).expect("enhance op"),
            deadline_unix_secs: None,
            chain_id: Some(10088),
            network_id: Some(10088),
            witness: TxWitness { signatures: vec![], threshold: None },
            max_fee: 1_000_000,
            priority_fee: 0,
            gas_limit: 100_000,
        };
        let enhance = sign(enhance, &player_key);
        let enhance_gas = crate::compute::estimate_tx_gas(&enhance);
        let enhance_fee = enhance_gas.min(enhance.max_fee); // base_fee=1，封顶 max_fee

        let basic = executor.new_basic_executor();
        let (receipts, _) = executor
            .execute_txs(
                &[
                    config,
                    policy,
                    trip[0].clone(),
                    trip[1].clone(),
                    trip[2].clone(),
                    equip,
                    enhance,
                ],
                1,
                1_700_000_000, // 块时间 > 72h 窗口 → 铸币提案 Passed
                &basic,
            )
            .expect("execute economy");
        assert_eq!(receipts[0].status, ReceiptStatus::Success, "config");
        assert_eq!(receipts[1].status, ReceiptStatus::Success, "policy");
        assert_eq!(receipts[2].status, ReceiptStatus::Success, "propose");
        assert_eq!(receipts[3].status, ReceiptStatus::Success, "vote");
        assert_eq!(receipts[4].status, ReceiptStatus::Success, "execute");
        assert_eq!(receipts[5].status, ReceiptStatus::Success, "equip");
        assert_eq!(receipts[6].status, ReceiptStatus::Success, "enhance: {:?}", receipts[6].error);

        // 玩家余额 = 1_000_000 - 10（强化成本） - gas 费（增强交易的 gas×base_fee=1）
        let player_bal = state_db.get_balance(&player_addr).as_u64();
        assert_eq!(player_bal, 1_000_000 - 10 - enhance_gas);
        // 国库：+10 成本 + gas 费（vote + execute + enhance；propose/policy/config 是 Mint 免收）
        let treasury = state_db.get_balance(&crate::governance::treasury_address()).as_u64();
        assert_eq!(treasury, 10 + vote_gas + execute_gas + enhance_gas);
        assert_eq!(executor.treasury_ledger().balance(), treasury as u128);
    }

    #[test]
    fn zk_enhance_executes_with_proof_and_debits_shc() {
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let executor = StateExecutor::new(state_db.clone(), 10088);
        let (authority, auth_key) = authority();
        let (player_addr, player_key) = {
            use ed25519_dalek::SigningKey;
            let k = SigningKey::from_bytes(&[0xbb; 32]);
            let pk = k.verifying_key().to_bytes();
            let h = crate::crypto::keccak256(&pk);
            (crate::crypto::Address::from_slice(&h[12..]).unwrap(), k)
        };

        let config = mint_config_tx(&authority, &auth_key, 1);
        let policy = mint_policy_tx(&authority, &auth_key, 6_000_000, 2);
        let trip = mint_shc_trip(&authority, &auth_key, player_addr, 5_000_000, 3);
        let equip_obj = ObjectId(crate::crypto::Hash::from_bytes(crate::crypto::keccak256(b"zk-eq")));
        let equip = ComputeTx {
            tx_id: TxId(crate::crypto::Hash::zero()),
            domain_id: GAME_DOMAIN,
            command: Command::Mint,
            input_set: vec![],
            read_set: vec![],
            output_proposals: vec![proposal_with_resources(
                &player_addr,
                equip_obj,
                1,
                None,
                b"zk-equip".to_vec(),
                0,
            )],
            fee: 0,
            nonce: Some(6),
            metadata: vec![],
            payload: vec![],
            deadline_unix_secs: None,
            chain_id: Some(10088),
            network_id: Some(10088),
            witness: TxWitness { signatures: vec![], threshold: None },
            max_fee: 0,
            priority_fee: 0,
            gas_limit: 0,
        };
        let equip = sign(equip, &player_key);

        // 客户端生成 ZK 证明（秘密 seed 不上链）；roll/结果由公开派生
        let seed = 987654321u64;
        let roll = zk::enhance::enhance_roll(seed).1;
        let proof = zk::enhance::prove_enhance(seed, roll, zk::enhance::ZK_ENHANCE_QUERIES);
        let proof_hex = hex::encode(zk::enhance::to_bytes(&proof));
        let cfg = crate::game::EnhanceConfig::default();
        let (success, new_level, new_pity) =
            crate::game::verify_zk_enhance(0, 0, 0, roll, &proof_hex, &cfg).expect("valid");
        let op = GameOp::ZkEnhance {
            object_id: format!("0x{}", hex::encode(equip_obj.0.as_bytes())),
            current_level: 0,
            current_pity: 0,
            star_stones: 0,
            roll_claim: roll,
            claimed_success: success,
            claimed_new_level: new_level,
            claimed_pity: new_pity,
            rules_version: 1,
            proof_hex,
        };
        let enhance = ComputeTx {
            tx_id: TxId(crate::crypto::Hash::zero()),
            domain_id: GAME_DOMAIN,
            command: Command::Invoke,
            input_set: vec![output_id_for_obj(&equip_obj, 1)],
            read_set: vec![],
            output_proposals: vec![proposal_with_resources(
                &player_addr,
                equip_obj,
                2,
                Some(output_id_for_obj(&equip_obj, 1)),
                b"zk-equip2".to_vec(),
                0,
            )],
            fee: 0,
            nonce: Some(7),
            metadata: vec![],
            payload: serde_json::to_vec(&op).expect("zk enhance op"),
            deadline_unix_secs: None,
            chain_id: Some(10088),
            network_id: Some(10088),
            witness: TxWitness { signatures: vec![], threshold: None },
            max_fee: 1_000_000,
            priority_fee: 0,
            gas_limit: 100_000,
        };
        let enhance = sign(enhance, &player_key);
        let enhance_gas = crate::compute::estimate_tx_gas(&enhance);
        let enhance_fee = enhance_gas.min(enhance.max_fee); // base_fee=1，封顶 max_fee

        let basic = executor.new_basic_executor();
        let (receipts, _) = executor
            .execute_txs(
                &[
                    config,
                    policy,
                    trip[0].clone(),
                    trip[1].clone(),
                    trip[2].clone(),
                    equip,
                    enhance,
                ],
                1,
                1_700_000_000,
                &basic,
            )
            .expect("execute economy");
        assert_eq!(receipts[6].status, ReceiptStatus::Success, "zk enhance: {:?}", receipts[6].error);
        // 玩家扣 cost(10) + gas；国库 +10 + gas
        assert_eq!(
            state_db.get_balance(&player_addr).as_u64(),
            5_000_000 - 10 - enhance_fee
        );
        let treasury = state_db.get_balance(&crate::governance::treasury_address()).as_u64();
        assert_eq!(treasury, 10 + enhance_fee + crate::compute::estimate_tx_gas(&trip[1])
            + crate::compute::estimate_tx_gas(&trip[2]));
    }

    #[test]
    fn transfer_coin_moves_shc_between_accounts() {
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let executor = StateExecutor::new(state_db.clone(), 10088);
        let (authority, auth_key) = authority();
        let (payer_addr, payer_key) = {
            use ed25519_dalek::SigningKey;
            let k = SigningKey::from_bytes(&[0x77; 32]);
            let pk = k.verifying_key().to_bytes();
            let h = crate::crypto::keccak256(&pk);
            (crate::crypto::Address::from_slice(&h[12..]).unwrap(), k)
        };
        let (receiver_addr, _rk) = {
            use ed25519_dalek::SigningKey;
            let k = SigningKey::from_bytes(&[0x88; 32]);
            let pk = k.verifying_key().to_bytes();
            let h = crate::crypto::keccak256(&pk);
            (crate::crypto::Address::from_slice(&h[12..]).unwrap(), k)
        };

        let policy = mint_policy_tx(&authority, &auth_key, 1_000_000, 1);
        let trip = mint_shc_trip(&authority, &auth_key, payer_addr, 100_000, 2);
        let vote_gas = crate::compute::estimate_tx_gas(&trip[1]);
        let execute_gas = crate::compute::estimate_tx_gas(&trip[2]);
        let op = GameOp::TransferCoin {
            to: format!("0x{}", hex::encode(receiver_addr.as_bytes())),
            amount: 3_000,
        };
        let transfer = ComputeTx {
            tx_id: TxId(crate::crypto::Hash::zero()),
            domain_id: GAME_DOMAIN,
            command: Command::Invoke,
            input_set: vec![],
            read_set: vec![],
            output_proposals: vec![],
            fee: 0,
            nonce: Some(5),
            metadata: vec![],
            payload: serde_json::to_vec(&op).expect("transfer op"),
            deadline_unix_secs: None,
            chain_id: Some(10088),
            network_id: Some(10088),
            witness: TxWitness { signatures: vec![], threshold: None },
            max_fee: 1_000_000,
            priority_fee: 0,
            gas_limit: 100_000,
        };
        let transfer = sign(transfer, &payer_key);
        let transfer_gas = crate::compute::estimate_tx_gas(&transfer);

        let basic = executor.new_basic_executor();
        let (receipts, _) = executor
            .execute_txs(
                &[
                    policy,
                    trip[0].clone(),
                    trip[1].clone(),
                    trip[2].clone(),
                    transfer,
                ],
                1,
                1_700_000_000, // 块时间 > 72h → 铸币提案 Passed
                &basic,
            )
            .expect("execute economy");
        assert_eq!(receipts[0].status, ReceiptStatus::Success, "policy");
        assert_eq!(receipts[1].status, ReceiptStatus::Success, "propose");
        assert_eq!(receipts[2].status, ReceiptStatus::Success, "vote");
        assert_eq!(receipts[3].status, ReceiptStatus::Success, "execute");
        assert_eq!(receipts[4].status, ReceiptStatus::Success, "transfer: {:?}", receipts[4].error);

        // 发送者 = 100_000 - 3_000（转账额） - gas；接收者 = 3_000；国库 = vote/execute/transfer gas
        assert_eq!(state_db.get_balance(&payer_addr).as_u64(), 100_000 - 3_000 - transfer_gas);
        assert_eq!(state_db.get_balance(&receiver_addr).as_u64(), 3_000);
        let treasury = state_db.get_balance(&crate::governance::treasury_address()).as_u64();
        assert_eq!(treasury, vote_gas + execute_gas + transfer_gas);
    }

    #[test]
    fn transfer_coin_insufficient_balance_rejected() {
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let executor = StateExecutor::new(state_db.clone(), 10088);
        let (authority, auth_key) = authority();
        let (payer_addr, payer_key) = {
            use ed25519_dalek::SigningKey;
            let k = SigningKey::from_bytes(&[0x99; 32]);
            let pk = k.verifying_key().to_bytes();
            let h = crate::crypto::keccak256(&pk);
            (crate::crypto::Address::from_slice(&h[12..]).unwrap(), k)
        };
        let (receiver_addr, _rk) = {
            use ed25519_dalek::SigningKey;
            let k = SigningKey::from_bytes(&[0xaa; 32]);
            let pk = k.verifying_key().to_bytes();
            let h = crate::crypto::keccak256(&pk);
            (crate::crypto::Address::from_slice(&h[12..]).unwrap(), k)
        };

        let policy = mint_policy_tx(&authority, &auth_key, 1_000_000, 1);
        let trip = mint_shc_trip(&authority, &auth_key, payer_addr, 100, 2); // 只够转 0
        let op = GameOp::TransferCoin {
            to: format!("0x{}", hex::encode(receiver_addr.as_bytes())),
            amount: 1_000,
        };
        let transfer = ComputeTx {
            tx_id: TxId(crate::crypto::Hash::zero()),
            domain_id: GAME_DOMAIN,
            command: Command::Invoke,
            input_set: vec![],
            read_set: vec![],
            output_proposals: vec![],
            fee: 0,
            nonce: Some(5),
            metadata: vec![],
            payload: serde_json::to_vec(&op).expect("transfer op"),
            deadline_unix_secs: None,
            chain_id: Some(10088),
            network_id: Some(10088),
            witness: TxWitness { signatures: vec![], threshold: None },
            max_fee: 1_000_000,
            priority_fee: 0,
            gas_limit: 100_000,
        };
        let transfer = sign(transfer, &payer_key);
        let basic = executor.new_basic_executor();
        let (receipts, _) = executor
            .execute_txs(
                &[policy, trip[0].clone(), trip[1].clone(), trip[2].clone(), transfer],
                1,
                1_700_000_000,
                &basic,
            )
            .expect("execute economy");
        assert_eq!(receipts[4].status, ReceiptStatus::Failed);
        assert!(
            receipts[4].error.as_deref().unwrap_or("").contains("insufficient shc balance"),
            "{:?}",
            receipts[4].error
        );
        assert_eq!(state_db.get_balance(&payer_addr).as_u64(), 100, "no state change on rejection");
        assert_eq!(state_db.get_balance(&receiver_addr).as_u64(), 0);
    }

    // ── 治理生效（FundActivity 扣国库）──────────────────────────────

    fn governance_invoke(
        authority: &crate::crypto::Address,
        key: &ed25519_dalek::SigningKey,
        object_id: ObjectId,
        version: u64,
        predecessor: Option<OutputId>,
        nonce: u64,
        payload: Vec<u8>,
        state: Vec<u8>,
    ) -> ComputeTx {
        let tx = ComputeTx {
            tx_id: TxId(crate::crypto::Hash::zero()),
            domain_id: GAME_DOMAIN,
            command: Command::Invoke,
            input_set: predecessor.iter().copied().collect(),
            read_set: vec![],
            output_proposals: vec![proposal(
                authority,
                object_id,
                version,
                predecessor,
                state,
            )],
            fee: 0,
            nonce: Some(nonce),
            metadata: vec![],
            payload,
            deadline_unix_secs: None,
            chain_id: Some(10088),
            network_id: Some(10088),
            witness: TxWitness { signatures: vec![], threshold: None },
            max_fee: 1_000_000_000,
            priority_fee: 0,
            gas_limit: 1_000_000,
        };
        sign(tx, key)
    }

    fn governance_mint_proposal(
        authority: &crate::crypto::Address,
        key: &ed25519_dalek::SigningKey,
        object_id: ObjectId,
        p: &Proposal,
    ) -> ComputeTx {
        let tx = ComputeTx {
            tx_id: TxId(crate::crypto::Hash::zero()),
            domain_id: GAME_DOMAIN,
            command: Command::Mint,
            input_set: vec![],
            read_set: vec![],
            output_proposals: vec![proposal(
                authority,
                object_id,
                1,
                None,
                serde_json::to_vec(p).expect("proposal json"),
            )],
            fee: 0,
            nonce: Some(1),
            metadata: vec![],
            payload: vec![],
            deadline_unix_secs: None,
            chain_id: Some(10088),
            network_id: Some(10088),
            witness: TxWitness { signatures: vec![], threshold: None },
            // Mint 策略禁止携带任何费用（fee/max_fee/priority_fee 全 0）
            max_fee: 0,
            priority_fee: 0,
            gas_limit: 0,
        };
        sign(tx, key)
    }

    /// 构造 [mint 提案, vote 500, execute FundActivity] 三条治理交易。
    fn governance_trip(
        authority: &crate::crypto::Address,
        key: &ed25519_dalek::SigningKey,
        id: &str,
        amount: u64,
    ) -> (Vec<ComputeTx>, ObjectId) {
        let object_id = ObjectId(crate::crypto::Hash::from_bytes(crate::crypto::keccak256(
            format!("shanhai/proposal/{id}").as_bytes(),
        )));
        let p = crate::governance::create_proposal(
            id,
            crate::governance::ProposalKind::FundActivity {
                amount,
                memo: "season prize".into(),
            },
            "0xaaa",
            1000,
            0,
        )
        .expect("proposal");
        let mint = governance_mint_proposal(authority, key, object_id, &p);
        let v1 = mint.output_proposals[0].output_id;

        let mut v2 = p.clone();
        v2.votes_for = 500;
        let vote = governance_invoke(
            authority,
            key,
            object_id,
            2,
            Some(v1),
            2,
            serde_json::to_vec(&GameOp::Vote {
                proposal_id: id.into(),
                voter: "0xbbb".into(),
                stake: 500,
                approve: true,
            })
            .expect("vote op"),
            serde_json::to_vec(&v2).expect("proposal json"),
        );
        let v2_out = vote.output_proposals[0].output_id;

        let mut v3 = v2.clone();
        v3.status = ProposalStatus::Executed;
        let execute = governance_invoke(
            authority,
            key,
            object_id,
            3,
            Some(v2_out),
            3,
            serde_json::to_vec(&GameOp::Execute {
                proposal_id: id.into(),
            })
            .expect("execute op"),
            serde_json::to_vec(&v3).expect("proposal json"),
        );
        (vec![mint, vote, execute], object_id)
    }

    #[test]
    fn fund_activity_executes_and_debits_treasury() {
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let executor = StateExecutor::new(state_db.clone(), 10088);
        let (authority, key) = authority();

        let (txs, _object_id) = governance_trip(&authority, &key, "fund-ok", 50);
        let basic = executor.new_basic_executor();
        let (receipts, _gas) = executor
            .execute_txs(&txs, 1, 1_700_000_000, &basic)
            .expect("execute governance trip");
        assert_eq!(receipts[0].status, ReceiptStatus::Success); // mint
        assert_eq!(receipts[1].status, ReceiptStatus::Success); // vote（产生费用进国库）
        assert_eq!(receipts[2].status, ReceiptStatus::Success); // execute（扣款 50）

        // 国库账户被扣 50：账本余额 == 账户余额（不变量）
        let account = state_db.get_balance(&crate::governance::treasury_address()).as_u64();
        let ledger = executor.treasury_ledger();
        assert_eq!(ledger.balance(), account as u128, "ledger/account invariant");
        assert!(
            ledger
                .events()
                .iter()
                .any(|e| matches!(e, crate::governance::LedgerEvent::Expense { amount: 50, .. })),
            "expense event recorded"
        );
        assert!(
            ledger
                .events()
                .iter()
                .any(|e| matches!(e, crate::governance::LedgerEvent::Income { .. })),
            "income events recorded"
        );
        // 生效产物被记录（FundActivity）
        assert_eq!(
            ledger.executions(),
            &[crate::governance::ProposalOutcome::ActivityFunded {
                amount: 50,
                destination: "activity".into(),
                memo: "season prize".into(),
            }]
        );
    }

    #[test]
    fn update_config_execute_records_outcome_without_treasury_change() {
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let executor = StateExecutor::new(state_db.clone(), 10088);
        let (authority, key) = authority();

        // 构造 UpdateConfig 提案的 [mint, vote, execute]
        let id = "cfg-ok";
        let object_id = ObjectId(crate::crypto::Hash::from_bytes(crate::crypto::keccak256(
            format!("shanhai/proposal/{id}").as_bytes(),
        )));
        let p = crate::governance::create_proposal(
            id,
            crate::governance::ProposalKind::UpdateConfig {
                config_object: "0xenhance".into(),
                params: serde_json::json!({ "success_permille": [900, 850] }),
            },
            "0xaaa",
            1000,
            0,
        )
        .expect("proposal");
        let mint = governance_mint_proposal(&authority, &key, object_id, &p);
        let v1 = mint.output_proposals[0].output_id;
        let mut v2 = p.clone();
        v2.votes_for = 500;
        let vote = governance_invoke(
            &authority,
            &key,
            object_id,
            2,
            Some(v1),
            2,
            serde_json::to_vec(&GameOp::Vote {
                proposal_id: id.into(),
                voter: "0xbbb".into(),
                stake: 500,
                approve: true,
            })
            .expect("vote op"),
            serde_json::to_vec(&v2).expect("proposal json"),
        );
        let v2_out = vote.output_proposals[0].output_id;
        let mut v3 = v2.clone();
        v3.status = ProposalStatus::Executed;
        let execute = governance_invoke(
            &authority,
            &key,
            object_id,
            3,
            Some(v2_out),
            3,
            serde_json::to_vec(&GameOp::Execute {
                proposal_id: id.into(),
            })
            .expect("execute op"),
            serde_json::to_vec(&v3).expect("proposal json"),
        );

        let basic = executor.new_basic_executor();
        let (receipts, _gas) = executor
            .execute_txs(&[mint, vote, execute], 1, 1_700_000_000, &basic)
            .expect("execute config trip");
        assert_eq!(receipts[0].status, ReceiptStatus::Success);
        assert_eq!(receipts[1].status, ReceiptStatus::Success);
        assert_eq!(receipts[2].status, ReceiptStatus::Success);

        // 无支出事件（UpdateConfig 不动国库），但生效产物被记录
        let ledger = executor.treasury_ledger();
        assert!(
            !ledger
                .events()
                .iter()
                .any(|e| matches!(e, crate::governance::LedgerEvent::Expense { .. })),
            "no expense for UpdateConfig"
        );
        assert_eq!(
            ledger.executions(),
            &[crate::governance::ProposalOutcome::ConfigUpdated {
                object: "0xenhance".into(),
                params: serde_json::json!({ "success_permille": [900, 850] }),
            }]
        );
    }

    #[test]
    fn fund_activity_insufficient_treasury_fails_without_state_change() {
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let executor = StateExecutor::new(state_db.clone(), 10088);
        let (authority, key) = authority();

        let (txs, _object_id) = governance_trip(&authority, &key, "fund-fail", 10_000_000_000u64);
        let basic = executor.new_basic_executor();
        let (receipts, _gas) = executor
            .execute_txs(&txs, 1, 1_700_000_000, &basic)
            .expect("execute governance trip");
        assert_eq!(receipts[0].status, ReceiptStatus::Success);
        assert_eq!(receipts[1].status, ReceiptStatus::Success);
        // execute 预检失败：余额不足 → Failed，且无支出事件
        assert_eq!(receipts[2].status, ReceiptStatus::Failed);
        assert!(
            receipts[2]
                .error
                .as_deref()
                .unwrap_or("")
                .contains("insufficient treasury"),
            "{:?}",
            receipts[2].error
        );
        let ledger = executor.treasury_ledger();
        let account = state_db.get_balance(&crate::governance::treasury_address()).as_u64();
        assert_eq!(ledger.balance(), account as u128, "ledger/account invariant");
        assert!(
            !ledger
                .events()
                .iter()
                .any(|e| matches!(e, crate::governance::LedgerEvent::Expense { .. })),
            "no expense on failed execute"
        );
    }
}
