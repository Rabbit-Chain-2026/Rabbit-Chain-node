//! 游戏域（GAME_DOMAIN）结算负载与语义验证。
//!
//! 复用 `shanhai-core`（山海共享确定性规则，同一套 Rust 代码）：结算交易
//! （`Invoke` + payload）中的声明结果在此重算比对，任何节点可独立复现。
//!
//! 反作弊模型：客户端/服务器只**提议**结果，验证方用确定性规则重算——伪造的
//! 胜负/回合数/强化结果一律拒绝。真实玩法中，队伍数据最终应来自链上对象
//! （MVP 阶段内嵌在负载中，见 `shanhai-onchain-mmo/shanhai-server/src/settle.rs`）。

use serde::{Deserialize, Serialize};
use shanhai_core::battle::{Team, resolve};

/// 游戏结算负载（GAME_DOMAIN `Invoke` 交易 payload 的 JSON 内容）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum GameOp {
    /// 战斗结算：重算 `resolve(teams, seed)`，校验声明的胜负/回合数。
    BattleSettle {
        session_id: String,
        teams: BattleTeams,
        seed: u64,
        claimed_winner: u8,
        claimed_rounds: u32,
        rules_version: u64,
    },
    /// 强化结算：重算 `roll(...)`，校验声明的成功/等级/保底。
    Enhance {
        object_id: String,
        current_level: u8,
        current_pity: u8,
        star_stones: u16,
        seed: u64,
        claimed_success: bool,
        claimed_new_level: u8,
        claimed_pity: u8,
        rules_version: u64,
    },
    /// 治理：提交提案（押金门槛等规则在 `governance` 模块，状态落链上对象）。
    Propose {
        proposal_id: String,
        kind: crate::governance::ProposalKind,
        proposer: String,
        deposit: u64,
        created_at_unix: u64,
    },
    /// 治理：投票（按质押权重）。
    Vote {
        proposal_id: String,
        voter: String,
        stake: u64,
        approve: bool,
    },
    /// 治理：生效执行（消费 Passed 提案对象；FundActivity 由执行器在区块执行时确定性扣国库）。
    Execute {
        proposal_id: String,
    },
    /// 山海币转账：玩家向目标地址转移 SHC（扣发送者，入接收者；gas 由发送者承担）。
    TransferCoin {
        to: String,
        amount: u64,
    },
}

/// 战斗双方队伍（直接使用 shanhai-core 类型，序列化后跨进程传递）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BattleTeams {
    pub a: Team,
    pub b: Team,
}

#[derive(Debug, thiserror::Error)]
pub enum GameError {
    #[error("invalid game payload: {0}")]
    InvalidPayload(String),
    #[error("battle outcome mismatch: claimed winner={claimed}, computed={computed}")]
    BattleOutcomeMismatch { claimed: u8, computed: u8 },
    #[error("battle rounds mismatch: claimed={claimed}, computed={computed}")]
    BattleRoundsMismatch { claimed: u32, computed: u32 },
    #[error("enhance success mismatch: claimed={claimed}, computed={computed}")]
    EnhanceSuccessMismatch { claimed: bool, computed: bool },
    #[error("enhance level mismatch: claimed={claimed}, computed={computed}")]
    EnhanceLevelMismatch { claimed: u8, computed: u8 },
    #[error("enhance pity mismatch: claimed={claimed}, computed={computed}")]
    EnhancePityMismatch { claimed: u8, computed: u8 },
}

impl GameOp {
    /// 从交易 payload 解析游戏操作。
    pub fn parse(payload: &[u8]) -> Result<GameOp, GameError> {
        serde_json::from_slice(payload).map_err(|e| GameError::InvalidPayload(e.to_string()))
    }
}

pub use shanhai_core::enhancement::{EnhanceConfig, roll_with_config};

/// 强化规则配置对象逻辑 id（与 shanhai-server `config_object_id` 同源）。
pub fn enhance_config_object_id() -> crate::compute::ObjectId {
    crate::compute::ObjectId(crate::crypto::Hash::from_bytes(crate::crypto::keccak256(
        b"shanhai/config/enhance",
    )))
}

/// 山海币铸币政策（治理可调，防通胀）：单笔铸币上限。
/// 链上对象 `keccak("shanhai/config/mint")`；由权威铸造 v1，治理 `UpdateConfig` 可推进版本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintPolicy {
    /// 配置版本（UpdateConfig 推进；语义同 EnhanceConfig.version）。
    pub version: u64,
    /// 单笔铸币上限（SHC）。超过则 `mint denied: exceeds per-mint cap`。
    pub per_mint_cap: u64,
}

impl Default for MintPolicy {
    fn default() -> Self {
        Self {
            version: 1,
            per_mint_cap: 1_000_000_000,
        }
    }
}

/// 铸币政策对象 id（链上配置对象）。
pub fn mint_policy_object_id() -> crate::compute::ObjectId {
    crate::compute::ObjectId(crate::crypto::Hash::from_bytes(crate::crypto::keccak256(
        b"shanhai/config/mint",
    )))
}

/// 提取交易首个 ed25519 签名的派生地址（增强/转账的付款方判定）。
pub fn ed25519_signer_address(tx: &crate::compute::ComputeTx) -> Option<crate::crypto::Address> {
    for sig in &tx.witness.signatures {
        if sig.scheme == crate::compute::SignatureScheme::Ed25519 {
            if let Some(pk) = &sig.public_key {
                if pk.len() == 32 {
                    let mut key = [0u8; 32];
                    key.copy_from_slice(pk);
                    let hash = crate::crypto::keccak256(&key);
                    return Some(crate::crypto::Address::from_slice(&hash[12..]).ok()?);
                }
            }
        }
    }
    None
}

/// 重算验证游戏结算，返回权威结果（强化按默认配置 = 当前常量表）。
pub fn verify(op: &GameOp) -> Result<serde_json::Value, GameError> {
    let default_cfg = shanhai_core::enhancement::EnhanceConfig::default();
    verify_with_config(op, Some(&default_cfg))
}

/// 重算验证游戏结算（链上 Config 对象驱动）：强化按给定配置重算，
/// 未提供时回退默认常量表。战斗/治理与配置无关。
pub fn verify_with_config(
    op: &GameOp,
    config: Option<&shanhai_core::enhancement::EnhanceConfig>,
) -> Result<serde_json::Value, GameError> {
    match op {
        GameOp::BattleSettle {
            teams,
            seed,
            claimed_winner,
            claimed_rounds,
            ..
        } => {
            let report = resolve(&teams.a, &teams.b, *seed);
            if report.winner != *claimed_winner {
                return Err(GameError::BattleOutcomeMismatch {
                    claimed: *claimed_winner,
                    computed: report.winner,
                });
            }
            if report.total_rounds != *claimed_rounds {
                return Err(GameError::BattleRoundsMismatch {
                    claimed: *claimed_rounds,
                    computed: report.total_rounds,
                });
            }
            Ok(serde_json::json!({ "winner": report.winner, "total_rounds": report.total_rounds }))
        }
        GameOp::Enhance {
            current_level,
            current_pity,
            star_stones,
            seed,
            claimed_success,
            claimed_new_level,
            claimed_pity,
            ..
        } => {
            let default_cfg = shanhai_core::enhancement::EnhanceConfig::default();
            let cfg = config.unwrap_or(&default_cfg);
            let r = shanhai_core::enhancement::roll_with_config(
                *current_level,
                *current_pity,
                *seed,
                *star_stones,
                cfg,
            );
            if r.success != *claimed_success {
                return Err(GameError::EnhanceSuccessMismatch {
                    claimed: *claimed_success,
                    computed: r.success,
                });
            }
            if r.new_level != *claimed_new_level {
                return Err(GameError::EnhanceLevelMismatch {
                    claimed: *claimed_new_level,
                    computed: r.new_level,
                });
            }
            if r.pity != *claimed_pity {
                return Err(GameError::EnhancePityMismatch {
                    claimed: *claimed_pity,
                    computed: r.pity,
                });
            }
            Ok(serde_json::json!({ "success": r.success, "new_level": r.new_level, "pity": r.pity }))
        }
        GameOp::Propose {
            proposal_id,
            kind,
            proposer,
            deposit,
            created_at_unix,
        } => {
            // 治理提案：结构 + 押金/时间规则由 governance 纯函数校验
            if proposal_id.trim().is_empty() {
                return Err(GameError::InvalidPayload("proposal_id must not be empty".into()));
            }
            if proposer.trim().is_empty() {
                return Err(GameError::InvalidPayload("proposer must not be empty".into()));
            }
            let _ = crate::governance::create_proposal(
                proposal_id.clone(),
                kind.clone(),
                proposer.clone(),
                *deposit,
                *created_at_unix,
            )
            .map_err(|e| GameError::InvalidPayload(e.to_string()))?;
            Ok(serde_json::json!({ "proposal_id": proposal_id }))
        }
        GameOp::Vote {
            proposal_id,
            voter,
            stake,
            approve,
        } => {
            if proposal_id.trim().is_empty() || voter.trim().is_empty() {
                return Err(GameError::InvalidPayload(
                    "proposal_id/voter must not be empty".into(),
                ));
            }
            if *stake == 0 {
                return Err(GameError::InvalidPayload("vote stake must be positive".into()));
            }
            Ok(serde_json::json!({
                "proposal_id": proposal_id,
                "approve": approve,
                "stake": stake,
            }))
        }
        GameOp::TransferCoin { to, amount } => {
            if to.trim().is_empty() {
                return Err(GameError::InvalidPayload("transfer target must not be empty".into()));
            }
            if *amount == 0 {
                return Err(GameError::InvalidPayload("transfer amount must be positive".into()));
            }
            Ok(serde_json::json!({ "to": to, "amount": amount }))
        }
        GameOp::Execute { proposal_id } => {
            // 结构化校验：语义（tally=Passed、窗口到期、提案对象一致性）由
            // gate_game_tx 结合链上提案对象重算把关（见 compute_adapter）。
            if proposal_id.trim().is_empty() {
                return Err(GameError::InvalidPayload("proposal_id must not be empty".into()));
            }
            Ok(serde_json::json!({ "proposal_id": proposal_id }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shanhai_core::battle::{Element, Unit};

    fn unit(id: u32, class: u8, element: Element) -> Unit {
        Unit {
            id,
            class,
            element,
            atk: 100,
            def: 30,
            hp: 1000,
            max_hp: 1000,
            spd: 10,
            crit_permille: 50,
        }
    }

    #[test]
    fn battle_settlement_verifies_and_rejects_forgery() {
        let teams = BattleTeams {
            a: Team {
                units: vec![unit(1, 0, Element::Metal), unit(2, 2, Element::Wood)],
            },
            b: Team { units: vec![unit(3, 0, Element::Fire)] },
        };
        let seed = 42;
        let report = resolve(&teams.a, &teams.b, seed);
        let ok = GameOp::BattleSettle {
            session_id: "s1".into(),
            teams: teams.clone(),
            seed,
            claimed_winner: report.winner,
            claimed_rounds: report.total_rounds,
            rules_version: 1,
        };
        assert!(verify(&ok).is_ok());

        // 伪造胜负 → 拒绝
        let forged = GameOp::BattleSettle {
            session_id: "s1".into(),
            teams: teams.clone(),
            seed,
            claimed_winner: report.winner ^ 1,
            claimed_rounds: report.total_rounds,
            rules_version: 1,
        };
        assert!(matches!(
            verify(&forged),
            Err(GameError::BattleOutcomeMismatch { .. })
        ));

        // payload 序列化往返可解析
        let bytes = serde_json::to_vec(&ok).unwrap();
        let parsed = GameOp::parse(&bytes).unwrap();
        assert!(verify(&parsed).is_ok());
    }

    #[test]
    fn enhance_settlement_verifies_and_rejects_forgery() {
        let seed = 7;
        let r = shanhai_core::enhancement::roll(0, 0, seed, 0);
        let ok = GameOp::Enhance {
            object_id: "0x01".into(),
            current_level: 0,
            current_pity: 0,
            star_stones: 0,
            seed,
            claimed_success: r.success,
            claimed_new_level: r.new_level,
            claimed_pity: r.pity,
            rules_version: 1,
        };
        assert!(verify(&ok).is_ok());

        let forged = GameOp::Enhance {
            object_id: "0x01".into(),
            current_level: 0,
            current_pity: 0,
            star_stones: 0,
            seed,
            claimed_success: !r.success,
            claimed_new_level: r.new_level,
            claimed_pity: r.pity,
            rules_version: 1,
        };
        assert!(matches!(
            verify(&forged),
            Err(GameError::EnhanceSuccessMismatch { .. })
        ));
    }

    #[test]
    fn governance_ops_verify_structurally() {
        let propose = GameOp::Propose {
            proposal_id: "p1".into(),
            kind: crate::governance::ProposalKind::UpdateConfig {
                config_object: "0xenhance".into(),
                params: serde_json::json!({}),
            },
            proposer: "0xaaa".into(),
            deposit: 1000,
            created_at_unix: 0,
        };
        assert!(verify(&propose).is_ok());

        let vote = GameOp::Vote {
            proposal_id: "p1".into(),
            voter: "0xbbb".into(),
            stake: 500,
            approve: true,
        };
        assert!(verify(&vote).is_ok());

        let exec = GameOp::Execute {
            proposal_id: "p1".into(),
        };
        assert!(verify(&exec).is_ok());

        // 空 proposal_id 拒绝
        assert!(matches!(
            verify(&GameOp::Execute {
                proposal_id: "".into()
            }),
            Err(GameError::InvalidPayload(_))
        ));
        assert!(matches!(
            verify(&GameOp::Vote {
                proposal_id: "p1".into(),
                voter: "".into(),
                stake: 100,
                approve: false
            }),
            Err(GameError::InvalidPayload(_))
        ));
    }

    #[test]
    fn enhance_verifies_against_provided_config() {
        use crate::game::EnhanceConfig;
        let seed = 7;
        // 默认配置：+0 成功率 90%
        let default = EnhanceConfig::default();
        let r_default = roll_with_config(0, 0, seed, 0, &default);
        let ok_default = GameOp::Enhance {
            object_id: "0x01".into(),
            current_level: 0,
            current_pity: 0,
            star_stones: 0,
            seed,
            claimed_success: r_default.success,
            claimed_new_level: r_default.new_level,
            claimed_pity: r_default.pity,
            rules_version: 1,
        };
        assert!(verify_with_config(&ok_default, Some(&default)).is_ok());

        // 改规则：+0 成功率 90% → 0%（保证与默认配置结果相反）
        let mut v2 = default.clone();
        v2.version = 2;
        v2.success_permille[0] = 0;
        let r_v2 = roll_with_config(0, 0, seed, 0, &v2);
        let ok_v2 = GameOp::Enhance {
            object_id: "0x01".into(),
            current_level: 0,
            current_pity: 0,
            star_stones: 0,
            seed,
            claimed_success: r_v2.success,
            claimed_new_level: r_v2.new_level,
            claimed_pity: r_v2.pity,
            rules_version: 2,
        };
        assert!(verify_with_config(&ok_v2, Some(&v2)).is_ok());
        // 用新配置校验"按旧配置声称"的结果 → 结果不一致拒绝（v2 必失败，声称成功即拒）
        let stale_claim = GameOp::Enhance {
            object_id: "0x01".into(),
            current_level: 0,
            current_pity: 0,
            star_stones: 0,
            seed,
            claimed_success: !r_v2.success,
            claimed_new_level: r_v2.new_level,
            claimed_pity: r_v2.pity,
            rules_version: 2,
        };
        assert!(verify_with_config(&stale_claim, Some(&v2)).is_err());
    }
}
