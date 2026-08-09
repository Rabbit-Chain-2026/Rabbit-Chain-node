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
    /// ZK 强化：客户端证明"存在秘密 seed 使 xorshift64³(seed) mod 1000 = roll_claim"，
    /// 链上原生验证证明后按公开 roll_claim 派生强化结果（seed 永不上链）。
    ZkEnhance {
        object_id: String,
        current_level: u8,
        current_pity: u8,
        star_stones: u16,
        /// 声称的 roll（证明的公开输出）。
        roll_claim: u64,
        claimed_success: bool,
        claimed_new_level: u8,
        claimed_pity: u8,
        rules_version: u64,
        /// ZK 证明：hex 编码的 bincode（`zk::enhance::EnhanceProof`）。
        proof_hex: String,
    },
    /// 统一玩法动作：发起（Mint session 对象，输入承诺绑定）。
    ActionStart {
        session_id: String,
        action_type: ActionKind,
        inputs: ActionInput,
        rules_version: u64,
        creator: String,
        created_at_unix: u64,
    },
    /// 统一玩法动作：结算（Invoke 消费 session；随机揭晓 + 声称结果 + 掉落）。
    ActionSettle {
        session_id: String,
        /// 随机揭晓：seed = keccak(random_block_hash ‖ session_id)（先承诺后揭晓）。
        seed: u64,
        /// 随机源块哈希（必须是 session 提交之后的真实链块）。
        random_block_hash: String,
        claimed: serde_json::Value,
        drops: Vec<ActionDrop>,
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
        GameOp::ZkEnhance {
            current_level,
            current_pity,
            star_stones,
            roll_claim,
            claimed_success,
            claimed_new_level,
            claimed_pity,
            proof_hex,
            ..
        } => {
            let default_cfg = shanhai_core::enhancement::EnhanceConfig::default();
            let cfg = config.unwrap_or(&default_cfg);
            let (success, new_level, new_pity) = verify_zk_enhance(
                *current_level,
                *current_pity,
                *star_stones,
                *roll_claim,
                proof_hex,
                cfg,
            )?;
            if success != *claimed_success {
                return Err(GameError::EnhanceSuccessMismatch {
                    claimed: *claimed_success,
                    computed: success,
                });
            }
            if new_level != *claimed_new_level {
                return Err(GameError::EnhanceLevelMismatch {
                    claimed: *claimed_new_level,
                    computed: new_level,
                });
            }
            if new_pity != *claimed_pity {
                return Err(GameError::EnhancePityMismatch {
                    claimed: *claimed_pity,
                    computed: new_pity,
                });
            }
            Ok(serde_json::json!({ "success": success, "new_level": new_level, "pity": new_pity }))
        }
        GameOp::ActionStart {
            session_id,
            creator,
            action_type,
            inputs,
            rules_version,
            created_at_unix,
        } => {
            if session_id.trim().is_empty() {
                return Err(GameError::InvalidPayload("session_id must not be empty".into()));
            }
            if creator.trim().is_empty() {
                return Err(GameError::InvalidPayload("creator must not be empty".into()));
            }
            let _ = (action_type, inputs, rules_version, created_at_unix);
            Ok(serde_json::json!({
                "session_id": session_id,
                "action_type": action_type,
                "creator": creator,
            }))
        }
        GameOp::ActionSettle {
            session_id, seed, random_block_hash, claimed, drops, ..
        } => {
            if session_id.trim().is_empty() || random_block_hash.trim().is_empty() {
                return Err(GameError::InvalidPayload(
                    "settle requires session_id and random_block_hash".into(),
                ));
            }
            // 语义验证（session 生命周期/seed 块校验/结果与掉落重算）在
            // gate_game_tx 与执行器结合链上对象执行（见 compute_adapter/executor）。
            Ok(serde_json::json!({
                "session_id": session_id,
                "seed": seed,
                "claimed": claimed,
                "drops": drops,
            }))
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

// ── 统一玩法动作引擎（§6.8：动作 = 确定性状态转移）─────────────────

/// 统一动作类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// 打怪/打boss：队伍 vs 怪物，确定性战斗 + 掉落表。
    Battle,
    /// 强化：装备 + seed → 新等级。
    Enhance,
}

/// 队伍单位（玩家侧简化属性，确定性聚合）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamUnit {
    pub atk: u64,
    pub def: u64,
    pub hp: u64,
}

/// 统一动作输入（序列化进 session 对象与 settle 交易）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActionInput {
    Battle { monster_id: String, team: Vec<TeamUnit> },
    Enhance { object_id: String, current_level: u8, current_pity: u8, star_stones: u16 },
}

/// 掉落（settle 时由链 Mint 为唯一资产对象）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionDrop {
    pub item_id: String,
    pub count: u64,
}

/// 统一输出：结果 + 掉落。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionOutcome {
    pub result: serde_json::Value,
    pub drops: Vec<ActionDrop>,
}

/// 动作 session 对象 state（输入承诺，之后不可改）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionSession {
    pub session_id: String,
    pub action_type: ActionKind,
    pub inputs: ActionInput,
    pub rules_version: u64,
    pub creator: String,
    pub created_at_unix: u64,
    /// 是否已结算（session 一次性：结算后置 true）。
    pub settled: bool,
    /// 结算结果（settle 输出 v2 回填）。
    pub result: Option<serde_json::Value>,
}

/// 动作 session 对象逻辑 id。
pub fn action_session_object_id(session_id: &str) -> crate::compute::ObjectId {
    crate::compute::ObjectId(crate::crypto::Hash::from_bytes(crate::crypto::keccak256(
        format!("shanhai/action/{session_id}").as_bytes(),
    )))
}

/// seed = keccak(random_block_hash ‖ session_id) 低 64 位（先承诺后揭晓的随机源）。
pub fn derive_action_seed(block_hash: &crate::crypto::Hash, session_id: &str) -> u64 {
    let mut buf = Vec::with_capacity(32 + session_id.len());
    buf.extend_from_slice(block_hash.as_bytes());
    buf.extend_from_slice(session_id.as_bytes());
    let h = crate::crypto::keccak256(&buf);
    u64::from_be_bytes(h[..8].try_into().expect("hash"))
}

/// 怪物规格（确定性内嵌表：打怪/打boss 的统一掉落源）。
#[derive(Debug, Clone)]
pub struct MonsterSpec {
    pub hp: u64,
    pub atk: u64,
    pub def: u64,
    /// (物品, 掉率千分位, 数量)
    pub drops: Vec<(String, u16, u64)>,
}

/// 内嵌怪物表（演示；生产可改为链上配置对象）。
pub fn monster_spec(id: &str) -> Option<MonsterSpec> {
    match id {
        "slime" => Some(MonsterSpec { hp: 50, atk: 8, def: 2, drops: vec![("slime_core".into(), 500, 1)] }),
        "wolf" => Some(MonsterSpec { hp: 120, atk: 15, def: 5, drops: vec![("wolf_fang".into(), 400, 1), ("wolf_pelt".into(), 300, 1)] }),
        "boss_dragon" => Some(MonsterSpec { hp: 2000, atk: 60, def: 20, drops: vec![("dragon_scale".into(), 800, 2), ("dragon_heart".into(), 100, 1)] }),
        _ => None,
    }
}

/// 统一规则执行（确定性，零浮点）：动作 → 结果 + 掉落。
pub fn execute_action(
    kind: &ActionKind,
    input: &ActionInput,
    seed: u64,
    cfg: &shanhai_core::enhancement::EnhanceConfig,
) -> Result<ActionOutcome, GameError> {
    match (kind, input) {
        (ActionKind::Battle, ActionInput::Battle { monster_id, team }) => {
            let spec = monster_spec(monster_id).ok_or_else(|| {
                GameError::InvalidPayload(format!("unknown monster {monster_id}"))
            })?;
            // 队伍聚合（确定性）
            let mut p_atk = 0u64;
            let mut p_def = 0u64;
            let mut p_hp = 0u64;
            for u in team {
                p_atk = p_atk.saturating_add(u.atk);
                p_def = p_def.saturating_add(u.def);
                p_hp = p_hp.saturating_add(u.hp);
            }
            // 回合制：攻方伤害 = max(1, atk − def) + 0..2 抖动（seed 驱动）
            let mut rng = shanhai_core::battle::Prng::new(seed);
            let mut m_hp = spec.hp;
            let mut hp = p_hp;
            let mut rounds = 0u64;
            let mut player_win = false;
            const MAX_ROUNDS: u64 = 500;
            while hp > 0 && m_hp > 0 && rounds < MAX_ROUNDS {
                // 玩家回合
                let base = p_atk.saturating_sub(spec.def).max(1);
                let dmg = base.saturating_add(rng.below(3));
                m_hp = m_hp.saturating_sub(dmg);
                // 怪物回合
                if m_hp > 0 {
                    let base = spec.atk.saturating_sub(p_def).max(1);
                    let dmg = base.saturating_add(rng.below(3));
                    hp = hp.saturating_sub(dmg);
                }
                rounds += 1;
            }
            player_win = hp > 0;
            // 掉落：玩家胜利时按怪物掉落表确定性 roll
            let mut drops = vec![];
            if player_win {
                for (item, permille, count) in &spec.drops {
                    if rng.chance(*permille) {
                        drops.push(ActionDrop { item_id: item.clone(), count: *count });
                    }
                }
            }
            Ok(ActionOutcome {
                result: serde_json::json!({ "winner": if player_win { "player" } else { "monster" }, "rounds": rounds }),
                drops,
            })
        }
        (ActionKind::Enhance, ActionInput::Enhance { current_level, current_pity, star_stones, .. }) => {
            let r = shanhai_core::enhancement::roll_with_config(
                *current_level, *current_pity, seed, *star_stones, cfg,
            );
            Ok(ActionOutcome {
                result: serde_json::json!({ "success": r.success, "new_level": r.new_level, "new_pity": r.pity }),
                drops: vec![],
            })
        }
        _ => Err(GameError::InvalidPayload("action kind/input mismatch".into())),
    }
}

/// ZK 强化验证（纯函数，链上原生执行）：
/// 1) 验证证明：存在秘密 seed 使 xorshift64³(seed) mod 1000 = roll_claim（zk crate）
/// 2) 用公开 roll_claim 按配置派生结果（success/new_level/new_pity）——与
///    `roll_with_config` 的公开部分一致，seed 永不上链。
pub fn verify_zk_enhance(
    current_level: u8,
    current_pity: u8,
    star_stones: u16,
    roll_claim: u64,
    proof_hex: &str,
    cfg: &shanhai_core::enhancement::EnhanceConfig,
) -> Result<(bool, u8, u8), GameError> {
    if roll_claim >= 1000 {
        return Err(GameError::InvalidPayload("roll claim out of range".into()));
    }
    let raw = hex::decode(proof_hex.trim().strip_prefix("0x").unwrap_or(proof_hex.trim()))
        .map_err(|e| GameError::InvalidPayload(format!("proof hex invalid: {e}")))?;
    let proof = zk::enhance::from_bytes(&raw)
        .map_err(|e| GameError::InvalidPayload(e))?;
    zk::enhance::verify_enhance(&proof, roll_claim, zk::enhance::ZK_ENHANCE_QUERIES)
        .map_err(|e| GameError::InvalidPayload(format!("zk proof rejected: {e}")))?;
    let base = cfg.success_permille[current_level.min(11) as usize];
    let bonus = ((star_stones as u16) * cfg.star_stone_bonus).min(cfg.star_stone_cap);
    let threshold = (base + bonus).min(950);
    let pity_guarantee = current_pity >= cfg.pity;
    let success = pity_guarantee || roll_claim < threshold as u64;
    let (new_level, new_pity) = if success {
        ((current_level + 1).min(12), 0)
    } else {
        (current_level, current_pity + 1)
    };
    Ok((success, new_level, new_pity))
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

    #[test]
    fn zk_enhance_verifies_proof_and_rejects_forgery() {
        use zk::enhance::{enhance_roll, prove_enhance, ZK_ENHANCE_QUERIES};
        // 客户端用秘密 seed 生成证明（seed 不上链），公开 roll_claim
        let seed = 424242u64;
        let roll = enhance_roll(seed).1;
        let proof = prove_enhance(seed, roll, ZK_ENHANCE_QUERIES);
        let proof_hex = hex::encode(zk::enhance::to_bytes(&proof));
        let cfg = crate::game::EnhanceConfig::default();
        // 结果派生（与 roll_with_config 公开部分一致；同时覆盖 helper）
        let (success, new_level, new_pity) =
            crate::game::verify_zk_enhance(0, 0, 0, roll, &proof_hex, &cfg).expect("valid");
        let op = GameOp::ZkEnhance {
            object_id: "0xzk".into(),
            current_level: 0,
            current_pity: 0,
            star_stones: 0,
            roll_claim: roll,
            claimed_success: success,
            claimed_new_level: new_level,
            claimed_pity: new_pity,
            rules_version: 1,
            proof_hex: proof_hex.clone(),
        };
        assert!(verify(&op).is_ok(), "valid zk proof accepted");
        // 伪造：roll_claim 与证明不符 → 拒
        let mut forged_roll = op.clone();
        if let GameOp::ZkEnhance { roll_claim, .. } = &mut forged_roll {
            *roll_claim = (roll + 1) % 1000;
        }
        assert!(verify(&forged_roll).is_err(), "forged roll rejected");
        // 伪造：声称结果与派生不符 → 拒
        let mut forged_res = op.clone();
        if let GameOp::ZkEnhance { claimed_success, .. } = &mut forged_res {
            *claimed_success = !success;
        }
        assert!(verify(&forged_res).is_err(), "forged result rejected");
        // 伪造：证明内容被篡改 → 拒
        let mut forged_proof = op.clone();
        if let GameOp::ZkEnhance { proof_hex, .. } = &mut forged_proof {
            // 篡改最后一个字节 → 反序列化/验证失败
            let mut b = zk::enhance::to_bytes(&proof);
            let last = b.last_mut().unwrap();
            *last ^= 0xFF;
            *proof_hex = hex::encode(b);
        }
        assert!(verify(&forged_proof).is_err(), "tampered proof rejected");
    }

    #[test]
    fn unified_action_engine_is_deterministic_and_covers_gameplay() {
        use crate::game::{execute_action, ActionInput, ActionKind, ActionOutcome, MonsterSpec};
        use crate::game::EnhanceConfig;
        let cfg = EnhanceConfig::default();
        let team = vec![TeamUnit { atk: 30, def: 10, hp: 200 }, TeamUnit { atk: 20, def: 5, hp: 150 }];
        // 打怪（battle）：同一 seed 结果与掉落完全一致
        let a = execute_action(
            &ActionKind::Battle,
            &ActionInput::Battle { monster_id: "wolf".into(), team: team.clone() },
            42,
            &cfg,
        ).expect("battle action");
        let b = execute_action(
            &ActionKind::Battle,
            &ActionInput::Battle { monster_id: "wolf".into(), team: team.clone() },
            42,
            &cfg,
        ).expect("battle action");
        assert_eq!(a, b, "deterministic outcome");
        // 怪物存在且掉落表确定
        assert!(monster_spec("slime").is_some());
        assert!(monster_spec("boss_dragon").is_some());
        assert!(monster_spec("nope").is_none());
        // 不同 seed → 可能不同结果（战斗是 seed 驱动的）
        let c = execute_action(
            &ActionKind::Battle,
            &ActionInput::Battle { monster_id: "wolf".into(), team },
            43,
            &cfg,
        ).expect("battle action 2");
        let _ = c;
        // 强化（enhance）统一入口与 roll_with_config 一致
        let r = roll_with_config(0, 0, 7, 0, &cfg);
        let o = execute_action(
            &ActionKind::Enhance,
            &ActionInput::Enhance { object_id: "0x01".into(), current_level: 0, current_pity: 0, star_stones: 0 },
            7,
            &cfg,
        ).expect("enhance action");
        assert_eq!(o.result, serde_json::json!({ "success": r.success, "new_level": r.new_level, "new_pity": r.pity }));
        assert!(o.drops.is_empty(), "enhance has no drops");
        // 未知怪物 → 拒
        assert!(execute_action(
            &ActionKind::Battle,
            &ActionInput::Battle { monster_id: "nope".into(), team: vec![] },
            1, &cfg,
        ).is_err());
        // 类型/输入不匹配 → 拒
        assert!(execute_action(
            &ActionKind::Battle,
            &ActionInput::Enhance { object_id: "0x01".into(), current_level: 0, current_pity: 0, star_stones: 0 },
            1, &cfg,
        ).is_err());
    }

    #[test]
    fn action_seed_derives_from_block_and_session() {
        use crate::game::derive_action_seed;
        let h = crate::crypto::Hash::from_bytes([0xAB; 32]);
        let s1 = derive_action_seed(&h, "sess-1");
        let s2 = derive_action_seed(&h, "sess-2");
        assert_eq!(s1, derive_action_seed(&h, "sess-1"), "deterministic");
        assert_ne!(s1, s2, "session-scoped");
        let h2 = crate::crypto::Hash::from_bytes([0xCD; 32]);
        assert_ne!(s1, derive_action_seed(&h2, "sess-1"), "block-scoped");
    }
}
