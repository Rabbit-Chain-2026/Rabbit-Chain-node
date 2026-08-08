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
use std::sync::Arc;
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
}

/// 默认域注册：main + 游戏域（jzz）。
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
        name: "jzz".to_string(),
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
        }
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
        let (computed, _) = self.execute_txs(&body.transactions, base_fee, &executor)?;

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
    pub fn execute_txs(
        &self,
        txs: &[ComputeTx],
        base_fee: u64,
        executor: &BasicTxExecutor<Arc<dyn ObjectStore>, DefaultAuthorizationPolicy, NoopResourcePolicy, Arc<dyn DomainRegistry>>,
    ) -> Result<(Vec<Receipt>, u64)> {
        let mut receipts = Vec::with_capacity(txs.len());
        let mut total_gas = 0u64;
        for tx in txs {
            match executor.execute(tx) {
                Ok(_) => {
                    let gas_used = crate::compute::estimate_tx_gas(tx);
                    total_gas = total_gas.saturating_add(gas_used);
                    // BlockTime 费用收集：非 Mint 交易按 gas_used × base_fee（上限 max_fee）入国库。
                    if tx.command != Command::Mint {
                        let fee = gas_used.saturating_mul(base_fee).min(tx.max_fee);
                        if fee > 0 {
                            self.credit_treasury(fee);
                        }
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

    /// 费用入国库账户（确定性，跨节点一致）。
    fn credit_treasury(&self, fee: u64) {
        let mut account = self.state_db.get_account(&self.treasury).unwrap_or_default();
        account.balance = account.balance.saturating_add(U256::from(fee));
        self.state_db.insert_account(self.treasury, account);
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
            priority_fee: 0,
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
}
