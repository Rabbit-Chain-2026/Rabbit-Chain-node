//! 资产抽象：双代币拆分 + 多游戏可扩展。
//!
//! 代币账本在共识层按 `TokenId` 分离：
//! - `NATIVE_TOKEN`（0）：原生代币——gas/优先费（税）、出块奖励、原生国库。
//! - `SHC_TOKEN`（1）：山海币（游戏代币）——`TransferCoin`、强化成本、
//!   `MintShc` 治理铸币、`FundActivity` 支出。
//! - 其他游戏/应用注册自己的 `TokenId` 即可无缝扩展（账本通用，RPC 通用）。
//!
//! 代币注册元数据（可选）：链上治理配置对象（`TokenRegistry`），便于发现与治理更新。

use serde::{Deserialize, Serialize};

/// 代币 id。`0` = 原生代币；`1+` = 游戏/应用代币。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TokenId(pub u64);

/// 原生代币（gas / 出块奖励 / 税）。
pub const NATIVE_TOKEN: TokenId = TokenId(0);

/// 山海币（游戏代币，GAME_DOMAIN 经济）。
pub const SHC_TOKEN: TokenId = TokenId(1);

impl TokenId {
    pub const fn new(id: u64) -> Self {
        Self(id)
    }
}

/// 代币注册元数据（链上 TokenRegistry 对象条目；治理可更新）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenInfo {
    pub token_id: u64,
    /// 符号（如 "SHC"）
    pub symbol: String,
    /// 名称（如 "山海币"）
    pub name: String,
    /// 精度（小数位）
    pub decimals: u8,
    /// 所属域（GAME_DOMAIN 或未来游戏的 DomainId）
    pub domain: u32,
    /// 是否可治理铸造（MintShc 类机制）
    pub governable: bool,
}

/// TokenRegistry 配置对象（链上对象 state；Default = 原生 + SHC）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRegistry {
    pub version: u64,
    pub tokens: Vec<TokenInfo>,
}

impl Default for TokenRegistry {
    fn default() -> Self {
        Self {
            version: 1,
            tokens: vec![
                TokenInfo {
                    token_id: NATIVE_TOKEN.0,
                    symbol: "RBT".into(),
                    name: "RabbitChain Native".into(),
                    decimals: 18,
                    domain: 0,
                    governable: false,
                },
                TokenInfo {
                    token_id: SHC_TOKEN.0,
                    symbol: "SHC".into(),
                    name: "山海币".into(),
                    decimals: 0,
                    domain: 0x6A7A7A00,
                    governable: true,
                },
            ],
        }
    }
}

/// TokenRegistry 配置对象逻辑 id（治理 `UpdateConfig` 可更新：注册新代币）。
pub fn token_registry_object_id() -> crate::compute::ObjectId {
    crate::compute::ObjectId(crate::crypto::Hash::from_bytes(crate::crypto::keccak256(
        b"shanhai/config/tokens",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_ids_are_distinct() {
        assert_ne!(NATIVE_TOKEN, SHC_TOKEN);
        assert_eq!(NATIVE_TOKEN, TokenId::new(0));
        assert_eq!(SHC_TOKEN, TokenId::new(1));
    }

    #[test]
    fn registry_default_has_native_and_shc() {
        let reg = TokenRegistry::default();
        assert!(reg.tokens.iter().any(|t| t.token_id == NATIVE_TOKEN.0));
        assert!(reg.tokens.iter().any(|t| t.token_id == SHC_TOKEN.0 && t.symbol == "SHC"));
    }
}
