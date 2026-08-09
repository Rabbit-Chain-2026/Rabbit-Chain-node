//! 游戏治理模块（P1 起点）：链上提案/投票/国库 的纯函数核心。
//!
//! 与 `shanhai-core` 同一契约：只放纯函数 + 常量，零 IO、零 wall-clock，
//! 时间一律由调用方显式传入（链上由交易时间戳/区块时间驱动）。
//! 落地方式：提案/投票作为 GAME_DOMAIN `Invoke` 交易（`GameOp::Propose`/`GameOp::Vote`），
//! 状态存链上对象；本模块只负责规则（押金/投票期/权重计票/生效判定）。
//!
//! 规则（方案-山海原生应用.md §4）：
//! - 提案需押金（防垃圾提案）
//! - 投票期 72h；计票按山海币质押权重（经济权益 = 治理权，防女巫）
//! - 赞成权重 ≥ 50% 通过
//! - 国库 = 链上账本对象（交易税/市场税流入），FundActivity 从国库拨款

use serde::{Deserialize, Serialize};

/// 提案投票窗口（秒）
pub const VOTE_WINDOW_SECS: u64 = 72 * 3600;
/// 提案押金（山海币）
pub const DEPOSIT_SH: u64 = 1_000;
/// 通过阈值（千分位）：赞成质押权重 ≥ 50%
pub const PASS_PROMILLE: u64 = 500;

/// 国库账户地址：交易税/市场税流入，提案 `FundActivity` 拨款。
/// 固定推导（keccak("shanhai/treasury") 后 20 字节），所有节点一致。
pub fn treasury_address() -> crate::crypto::Address {
    let hash = crate::crypto::keccak256(b"shanhai/treasury");
    crate::crypto::Address::from_slice(&hash[12..]).expect("treasury address")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProposalKind {
    /// 修改规则参数（写回 Config 对象新版本）
    UpdateConfig {
        config_object: String,
        params: serde_json::Value,
    },
    /// 活动拨款（从国库）
    FundActivity { amount: u64, memo: String },
    /// 冻结异常对象（防作弊治理）
    Freeze { object_id: String, reason: String },
    /// 新增玩法交易类型（链上批准 + 链下部署双轨）
    AddGameOp { op_name: String, description: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Active,
    Passed,
    Rejected,
    Executed,
    Withdrawn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub proposal_id: String,
    pub kind: ProposalKind,
    /// 提案人地址（hex）
    pub proposer: String,
    /// 押金（山海币）
    pub deposit: u64,
    pub created_at_unix: u64,
    pub deadline_unix: u64,
    /// 赞成/反对的累计质押权重
    pub votes_for: u128,
    pub votes_against: u128,
    pub status: ProposalStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vote {
    pub proposal_id: String,
    /// 投票人地址（hex）
    pub voter: String,
    /// 本次投入的质押权重（山海币）
    pub stake: u128,
    pub approve: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GovernanceError {
    #[error("deposit too low: {got}, minimum {minimum}")]
    DepositTooLow { got: u64, minimum: u64 },
    #[error("proposal is not active (status={0:?})")]
    NotActive(ProposalStatus),
    #[error("proposal voting window expired")]
    Expired,
    #[error("vote stake must be positive")]
    ZeroStake,
    #[error("proposal is not passed (status={0:?}); only passed proposals execute")]
    NotPassed(ProposalStatus),
    #[error("insufficient treasury balance: {balance}, needed {needed}")]
    InsufficientTreasury { balance: u128, needed: u128 },
    #[error("proposal status must transition Active -> Passed/Rejected -> Executed")]
    InvalidStatusTransition(ProposalStatus),
}

/// 创建提案（纯函数）：校验押金，计算投票截止。
pub fn create_proposal(
    proposal_id: impl Into<String>,
    kind: ProposalKind,
    proposer: impl Into<String>,
    deposit: u64,
    created_at_unix: u64,
) -> Result<Proposal, GovernanceError> {
    if deposit < DEPOSIT_SH {
        return Err(GovernanceError::DepositTooLow {
            got: deposit,
            minimum: DEPOSIT_SH,
        });
    }
    Ok(Proposal {
        proposal_id: proposal_id.into(),
        kind,
        proposer: proposer.into(),
        deposit,
        created_at_unix,
        deadline_unix: created_at_unix.saturating_add(VOTE_WINDOW_SECS),
        votes_for: 0,
        votes_against: 0,
        status: ProposalStatus::Active,
    })
}

/// 投票（纯函数）：窗口内按质押权重累计；非 Active 或过期拒绝。
pub fn cast_vote(p: &mut Proposal, stake: u128, approve: bool, now_unix: u64) -> Result<(), GovernanceError> {
    if p.status != ProposalStatus::Active {
        return Err(GovernanceError::NotActive(p.status));
    }
    if now_unix > p.deadline_unix {
        return Err(GovernanceError::Expired);
    }
    if stake == 0 {
        return Err(GovernanceError::ZeroStake);
    }
    if approve {
        p.votes_for = p.votes_for.saturating_add(stake);
    } else {
        p.votes_against = p.votes_against.saturating_add(stake);
    }
    Ok(())
}

/// 计票（纯函数）：到期后按 赞成权重占比 ≥ PASS_PROMILLE 判定通过/拒绝。
pub fn tally(p: &mut Proposal, now_unix: u64) -> ProposalStatus {
    if p.status != ProposalStatus::Active {
        return p.status;
    }
    if now_unix <= p.deadline_unix {
        return ProposalStatus::Active;
    }
    let total = p.votes_for.saturating_add(p.votes_against);
    let threshold = total.saturating_mul(PASS_PROMILLE as u128);
    let status = if total > 0 && p.votes_for.saturating_mul(1000) >= threshold {
        ProposalStatus::Passed
    } else {
        ProposalStatus::Rejected
    };
    p.status = status;
    status
}

/// 国库账本事件（确定性、可审计）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LedgerEvent {
    /// 收入：区块执行收取的非 Mint 交易 gas 费（source 区分来源，如 `block_gas_fee`）。
    Income { source: String, amount: u64 },
    /// 支出：提案生效后的拨款/退款（destination 为目标账户/对象）。
    Expense { destination: String, amount: u64, memo: String },
}

/// 国库账本（纯函数）：余额 + 确定性事件流 + 治理生效记录。
/// 余额与链上国库账户（`treasury_address` 的 StateDb balance）保持一致；本结构为审计视图。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TreasuryLedger {
    balance: u128,
    events: Vec<LedgerEvent>,
    executions: Vec<ProposalOutcome>,
}

impl TreasuryLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn balance(&self) -> u128 {
        self.balance
    }

    pub fn events(&self) -> &[LedgerEvent] {
        &self.events
    }

    /// 已生效执行的提案产物（UpdateConfig/Freeze/AddGameOp/FundActivity）。
    pub fn executions(&self) -> &[ProposalOutcome] {
        &self.executions
    }

    pub fn record_execution(&mut self, outcome: ProposalOutcome) {
        self.executions.push(outcome);
    }

    pub fn record_income(&mut self, source: impl Into<String>, amount: u64) {
        self.balance = self.balance.saturating_add(amount as u128);
        self.events.push(LedgerEvent::Income {
            source: source.into(),
            amount,
        });
    }

    /// 支出：余额不足返回 Err（余额与事件流均不变）。
    pub fn record_expense(
        &mut self,
        destination: impl Into<String>,
        amount: u64,
        memo: impl Into<String>,
    ) -> Result<(), GovernanceError> {
        if (amount as u128) > self.balance {
            return Err(GovernanceError::InsufficientTreasury {
                balance: self.balance,
                needed: amount as u128,
            });
        }
        self.balance -= amount as u128;
        self.events.push(LedgerEvent::Expense {
            destination: destination.into(),
            amount,
            memo: memo.into(),
        });
        Ok(())
    }
}

/// 提案生效执行的产物（治理生效的可验证结果）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProposalOutcome {
    ConfigUpdated {
        object: String,
        params: serde_json::Value,
    },
    ActivityFunded {
        amount: u64,
        destination: String,
        memo: String,
    },
    ObjectFrozen {
        object_id: String,
        reason: String,
    },
    GameOpApproved {
        op_name: String,
        description: String,
    },
}

/// 提案生效执行（纯函数）：仅 `Passed` 可执行；`FundActivity` 从国库扣款。
/// 成功则把提案状态置为 `Executed` 并返回产物；失败（余额不足等）不改动任何状态。
pub fn execute_proposal(
    p: &mut Proposal,
    ledger: &mut TreasuryLedger,
) -> Result<ProposalOutcome, GovernanceError> {
    if p.status != ProposalStatus::Passed {
        return Err(GovernanceError::NotPassed(p.status));
    }
    let outcome = match &p.kind {
        ProposalKind::FundActivity { amount, memo } => {
            let destination = "activity".to_string();
            ledger
                .record_expense(destination.clone(), *amount, memo.clone())
                .map_err(|_| GovernanceError::InsufficientTreasury {
                    balance: ledger.balance(),
                    needed: *amount as u128,
                })?;
            ProposalOutcome::ActivityFunded {
                amount: *amount,
                destination,
                memo: memo.clone(),
            }
        }
        ProposalKind::UpdateConfig {
            config_object,
            params,
        } => ProposalOutcome::ConfigUpdated {
            object: config_object.clone(),
            params: params.clone(),
        },
        ProposalKind::Freeze { object_id, reason } => ProposalOutcome::ObjectFrozen {
            object_id: object_id.clone(),
            reason: reason.clone(),
        },
        ProposalKind::AddGameOp {
            op_name,
            description,
        } => ProposalOutcome::GameOpApproved {
            op_name: op_name.clone(),
            description: description.clone(),
        },
    };
    p.status = ProposalStatus::Executed;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update_config_proposal() -> ProposalKind {
        ProposalKind::UpdateConfig {
            config_object: "0xenhance".into(),
            params: serde_json::json!({ "success_permille": [900, 850] }),
        }
    }

    #[test]
    fn rejects_low_deposit() {
        let err = create_proposal("p1", update_config_proposal(), "0xaaa", 999, 1000);
        assert!(matches!(err, Err(GovernanceError::DepositTooLow { got: 999, minimum: 1000 })));
    }

    #[test]
    fn accepts_enough_deposit_and_sets_deadline() {
        let p = create_proposal("p1", update_config_proposal(), "0xaaa", 1000, 1000).unwrap();
        assert_eq!(p.status, ProposalStatus::Active);
        assert_eq!(p.deadline_unix, 1000 + VOTE_WINDOW_SECS);
    }

    #[test]
    fn stake_weighted_tally_passes_at_half() {
        let mut p = create_proposal("p1", update_config_proposal(), "0xaaa", 1000, 0).unwrap();
        cast_vote(&mut p, 500, true, 100).unwrap();
        cast_vote(&mut p, 500, false, 200).unwrap();
        // 未到期 → 仍 Active
        assert_eq!(tally(&mut p, VOTE_WINDOW_SECS - 1), ProposalStatus::Active);
        // 到期：赞成 500/1000 = 50% → 通过
        assert_eq!(tally(&mut p, VOTE_WINDOW_SECS + 1), ProposalStatus::Passed);
    }

    #[test]
    fn minority_fails() {
        let mut p = create_proposal("p2", update_config_proposal(), "0xbbb", 1000, 0).unwrap();
        cast_vote(&mut p, 400, true, 10).unwrap();
        cast_vote(&mut p, 600, false, 20).unwrap();
        assert_eq!(tally(&mut p, VOTE_WINDOW_SECS + 1), ProposalStatus::Rejected);
    }

    #[test]
    fn expired_or_non_active_vote_rejected() {
        let mut p = create_proposal("p3", update_config_proposal(), "0xccc", 1000, 0).unwrap();
        assert!(matches!(
            cast_vote(&mut p, 100, true, VOTE_WINDOW_SECS + 1),
            Err(GovernanceError::Expired)
        ));
        assert!(matches!(cast_vote(&mut p, 0, true, 10), Err(GovernanceError::ZeroStake)));
        // 已生效的提案不能再投票
        let mut q = create_proposal("p4", update_config_proposal(), "0xddd", 1000, 0).unwrap();
        q.status = ProposalStatus::Passed;
        assert!(matches!(
            cast_vote(&mut q, 100, true, 10),
            Err(GovernanceError::NotActive(ProposalStatus::Passed))
        ));
    }

    fn passed_proposal(id: &str) -> Proposal {
        let mut p = create_proposal(id, update_config_proposal(), "0xaaa", 1000, 0).unwrap();
        cast_vote(&mut p, 500, true, 100).unwrap();
        tally(&mut p, VOTE_WINDOW_SECS + 1);
        assert_eq!(p.status, ProposalStatus::Passed);
        p
    }

    #[test]
    fn treasury_ledger_income_expense_and_insufficient() {
        let mut l = TreasuryLedger::new();
        l.record_income("block_gas_fee", 100);
        l.record_income("block_gas_fee", 50);
        assert_eq!(l.balance(), 150);
        assert_eq!(l.events().len(), 2);

        l.record_expense("activity", 60, "funding").unwrap();
        assert_eq!(l.balance(), 90);
        assert_eq!(l.events().len(), 3);

        // 余额不足：Err 且余额/事件流不变
        let err = l.record_expense("activity", 91, "too-much").unwrap_err();
        assert_eq!(
            err,
            GovernanceError::InsufficientTreasury {
                balance: 90,
                needed: 91
            }
        );
        assert_eq!(l.balance(), 90);
        assert_eq!(l.events().len(), 3);
    }

    #[test]
    fn execute_requires_passed() {
        let mut ledger = TreasuryLedger::new();
        ledger.record_income("block_gas_fee", 10_000);
        let mut active = create_proposal("p1", update_config_proposal(), "0xaaa", 1000, 0).unwrap();
        assert!(matches!(
            execute_proposal(&mut active, &mut ledger),
            Err(GovernanceError::NotPassed(ProposalStatus::Active))
        ));
        assert_eq!(active.status, ProposalStatus::Active);
        // Rejected 同样不可执行
        let mut rejected = create_proposal("p2", update_config_proposal(), "0xbbb", 1000, 0).unwrap();
        rejected.status = ProposalStatus::Rejected;
        assert!(matches!(
            execute_proposal(&mut rejected, &mut ledger),
            Err(GovernanceError::NotPassed(ProposalStatus::Rejected))
        ));
    }

    #[test]
    fn execute_fund_activity_debits_treasury() {
        let mut ledger = TreasuryLedger::new();
        ledger.record_income("block_gas_fee", 5_000);
        let mut p = passed_proposal("fund-1");
        p.kind = ProposalKind::FundActivity {
            amount: 800,
            memo: "season prize pool".into(),
        };
        let outcome = execute_proposal(&mut p, &mut ledger).unwrap();
        assert_eq!(
            outcome,
            ProposalOutcome::ActivityFunded {
                amount: 800,
                destination: "activity".into(),
                memo: "season prize pool".into(),
            }
        );
        assert_eq!(p.status, ProposalStatus::Executed);
        assert_eq!(ledger.balance(), 4_200);
    }

    #[test]
    fn execute_fund_activity_insufficient_leaves_state_unchanged() {
        let mut ledger = TreasuryLedger::new();
        ledger.record_income("block_gas_fee", 100);
        let mut p = passed_proposal("fund-2");
        p.kind = ProposalKind::FundActivity {
            amount: 10_000,
            memo: "over budget".into(),
        };
        let err = execute_proposal(&mut p, &mut ledger).unwrap_err();
        assert_eq!(
            err,
            GovernanceError::InsufficientTreasury {
                balance: 100,
                needed: 10_000
            }
        );
        // 失败不改状态、不动账本
        assert_eq!(p.status, ProposalStatus::Passed);
        assert_eq!(ledger.balance(), 100);
        assert_eq!(ledger.events().len(), 1);
    }

    #[test]
    fn execute_update_config_freezes_and_approves_ops() {
        let mut ledger = TreasuryLedger::new();

        let mut cfg = passed_proposal("cfg");
        cfg.kind = ProposalKind::UpdateConfig {
            config_object: "0xenhance".into(),
            params: serde_json::json!({ "success_permille": [900, 850] }),
        };
        assert!(matches!(
            execute_proposal(&mut cfg, &mut ledger).unwrap(),
            ProposalOutcome::ConfigUpdated { object, .. } if object == "0xenhance"
        ));
        assert_eq!(cfg.status, ProposalStatus::Executed);

        let mut frz = passed_proposal("frz");
        frz.kind = ProposalKind::Freeze {
            object_id: "0xbad".into(),
            reason: "exploit".into(),
        };
        assert!(matches!(
            execute_proposal(&mut frz, &mut ledger).unwrap(),
            ProposalOutcome::ObjectFrozen { object_id, .. } if object_id == "0xbad"
        ));

        let mut op = passed_proposal("op");
        op.kind = ProposalKind::AddGameOp {
            op_name: "claim".into(),
            description: "activity claim".into(),
        };
        assert!(matches!(
            execute_proposal(&mut op, &mut ledger).unwrap(),
            ProposalOutcome::GameOpApproved { op_name, .. } if op_name == "claim"
        ));
        assert_eq!(op.status, ProposalStatus::Executed);
    }
}
