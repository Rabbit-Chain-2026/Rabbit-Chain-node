//! 游戏治理模块（P1 起点）：链上提案/投票/国库 的纯函数核心。
//!
//! 与 `shanhai-core` 同一契约：只放纯函数 + 常量，零 IO、零 wall-clock，
//! 时间一律由调用方显式传入（链上由交易时间戳/区块时间驱动）。
//! 落地方式：提案/投票作为 GAME_DOMAIN `Invoke` 交易（`GameOp::Propose`/`GameOp::Vote`），
//! 状态存链上对象；本模块只负责规则（押金/投票期/权重计票/生效判定）。
//!
//! 规则（方案-山海原生应用.md §4）：
//! - 提案需押金（防垃圾提案）
//! - 投票期 72h；计票按 JZ 质押权重（经济权益 = 治理权，防女巫）
//! - 赞成权重 ≥ 50% 通过
//! - 国库 = 链上账本对象（交易税/市场税流入），FundActivity 从国库拨款

use serde::{Deserialize, Serialize};

/// 提案投票窗口（秒）
pub const VOTE_WINDOW_SECS: u64 = 72 * 3600;
/// 提案押金（JZ）
pub const DEPOSIT_JZ: u64 = 1_000;
/// 通过阈值（千分位）：赞成质押权重 ≥ 50%
pub const PASS_PROMILLE: u64 = 500;

/// 国库账户地址：交易税/市场税流入，提案 `FundActivity` 拨款。
/// 固定推导（keccak("jzz/treasury") 后 20 字节），所有节点一致。
pub fn treasury_address() -> crate::crypto::Address {
    let hash = crate::crypto::keccak256(b"jzz/treasury");
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
    /// 押金（JZ）
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
    /// 本次投入的质押权重（JZ）
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
}

/// 创建提案（纯函数）：校验押金，计算投票截止。
pub fn create_proposal(
    proposal_id: impl Into<String>,
    kind: ProposalKind,
    proposer: impl Into<String>,
    deposit: u64,
    created_at_unix: u64,
) -> Result<Proposal, GovernanceError> {
    if deposit < DEPOSIT_JZ {
        return Err(GovernanceError::DepositTooLow {
            got: deposit,
            minimum: DEPOSIT_JZ,
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
}
