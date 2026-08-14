//! State management module

pub mod executor;

use crate::account::{Account, AccountChange, AccountError, I256, U256};
use crate::block::BlockHeader;
use crate::crypto::{Address, Hash};
use parking_lot::RwLock;
use std::sync::Arc;

/// State errors
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("Account not found: {0}")]
    AccountNotFound(Address),
    #[error("Database error: {0}")]
    DatabaseError(String),
    #[error("Trie error: {0}")]
    TrieError(String),
}

/// State database
pub struct StateDb {
    /// Current state root
    state_root: RwLock<Hash>,
    /// Account cache
    accounts: RwLock<std::collections::HashMap<Address, Account>>,
    /// Token balance ledger (双代币拆分：NATIVE / SHC / 未来游戏代币)
    token_balances: RwLock<std::collections::HashMap<(Address, crate::assets::TokenId), U256>>,
    /// Storage cache
    storage: RwLock<std::collections::HashMap<Address, std::collections::HashMap<Hash, Hash>>>,
    /// Code cache
    codes: RwLock<std::collections::HashMap<Hash, Vec<u8>>>,
}

impl StateDb {
    pub fn new(state_root: Hash) -> Self {
        Self {
            state_root: RwLock::new(state_root),
            accounts: RwLock::new(std::collections::HashMap::new()),
            token_balances: RwLock::new(std::collections::HashMap::new()),
            storage: RwLock::new(std::collections::HashMap::new()),
            codes: RwLock::new(std::collections::HashMap::new()),
        }
    }

    pub fn state_root(&self) -> Hash {
        *self.state_root.read()
    }

    pub fn get_account(&self, address: &Address) -> Option<Account> {
        self.accounts.read().get(address).cloned()
    }

    pub fn insert_account(&self, address: Address, account: Account) {
        self.accounts.write().insert(address, account);
    }

    pub fn get_balance(&self, address: &Address) -> U256 {
        self.accounts
            .read()
            .get(address)
            .map(|a| a.balance)
            .unwrap_or_default()
    }

    /// 代币余额查询（NATIVE 走 account.balance，其他走代币账本）。
    pub fn get_token_balance(&self, address: &Address, token: crate::assets::TokenId) -> U256 {
        if token == crate::assets::NATIVE_TOKEN {
            return self.get_balance(address);
        }
        self.token_balances
            .read()
            .get(&(*address, token))
            .copied()
            .unwrap_or_default()
    }

    /// 代币余额写入（NATIVE 写 account.balance）。
    pub fn set_token_balance(&self, address: Address, token: crate::assets::TokenId, amount: U256) {
        if token == crate::assets::NATIVE_TOKEN {
            return self.set_balance(address, amount);
        }
        self.token_balances
            .write()
            .insert((address, token), amount);
    }

    pub fn get_nonce(&self, address: &Address) -> u64 {
        self.accounts
            .read()
            .get(address)
            .map(|a| a.nonce)
            .unwrap_or(0)
    }

    pub fn get_code(&self, address: &Address) -> Option<Vec<u8>> {
        let accounts = self.accounts.read();
        if let Some(account) = accounts.get(address) {
            let codes = self.codes.read();
            return codes.get(&account.code_hash).cloned();
        }
        None
    }

    pub fn set_code(&self, address: Address, code: Vec<u8>) {
        let code_hash = Hash::from_bytes(crate::crypto::keccak256(&code));

        let mut accounts = self.accounts.write();
        if let Some(account) = accounts.get_mut(&address) {
            account.code_hash = code_hash;
        }

        self.codes.write().insert(code_hash, code);
    }

    pub fn get_storage(&self, address: &Address, key: &Hash) -> Hash {
        let storage = self.storage.read();
        storage
            .get(address)
            .and_then(|map| map.get(key).copied())
            .unwrap_or_default()
    }

    pub fn set_storage(&self, address: Address, key: Hash, value: Hash) {
        let mut storage = self.storage.write();
        storage.entry(address).or_default().insert(key, value);
    }

    pub fn set_balance(&self, address: Address, balance: U256) {
        let mut accounts = self.accounts.write();
        if let Some(account) = accounts.get_mut(&address) {
            account.balance = balance;
        }
    }

    pub fn set_nonce(&self, address: Address, nonce: u64) {
        let mut accounts = self.accounts.write();
        if let Some(account) = accounts.get_mut(&address) {
            account.nonce = nonce;
        }
    }

    pub fn apply_changes(&self, changes: &[AccountChange]) -> Result<Hash, StateError> {
        for change in changes {
            if let Some(balance_change) = change.balance_change {
                let mut accounts = self.accounts.write();
                if let Some(account) = accounts.get_mut(&change.address) {
                    account.balance = account.balance.saturating_add(U256::from_big_endian(
                        &change.balance_change.unwrap_or(I256::zero()).0,
                    ));
                }
            }

            for storage_change in &change.storage_changes {
                self.set_storage(change.address, storage_change.key, storage_change.new_value);
            }
        }

        // Update state root (simplified)
        let new_root = self.compute_state_root();
        *self.state_root.write() = new_root;

        Ok(new_root)
    }

    fn compute_state_root(&self) -> Hash {
        // Simplified state root computation
        // In production, this would use Merkle Patricia Trie
        let accounts = self.accounts.read();
        let tokens = self.token_balances.read();
        let mut data = Vec::new();

        for (address, account) in accounts.iter() {
            data.extend_from_slice(address.as_bytes());
            data.extend_from_slice(&account.balance.to_big_endian());
        }
        // 代币账本纳入状态根（共识校验：跨节点余额一致）
        for ((address, token), balance) in tokens.iter() {
            data.extend_from_slice(address.as_bytes());
            data.extend_from_slice(&token.0.to_be_bytes());
            data.extend_from_slice(&balance.to_big_endian());
        }

        Hash::from_bytes(crate::crypto::keccak256(&data))
    }
}

/// State transition
pub struct StateTransition {
    pub from_root: Hash,
    pub to_root: Hash,
    pub changes: Vec<AccountChange>,
    pub block: BlockHeader,
}

impl StateTransition {
    pub fn apply(self, state: &StateDb) -> Result<(), StateError> {
        state.apply_changes(&self.changes)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::I256;

    #[test]
    fn test_state_db() {
        let state = StateDb::new(Hash::zero());

        let address = Address::from_bytes([1u8; 20]);
        let account = Account {
            address,
            balance: U256::from(1000),
            ..Default::default()
        };

        state.insert_account(address, account);

        assert_eq!(state.get_balance(&address).as_u64(), 1000);
        assert_eq!(state.get_nonce(&address), 0);
    }
}

/// 计算 unspent output 集的 MPT 状态根（共识：跨节点一致）。
///
/// 与 rabbitstore 的 `compute_unspent_mpt_root` 同算法（同一 MPT 核心，
/// 排序 output_id 后逐个插入 MemTrieDB），保证 header state_root 的
/// unspent 部分与 `rabbit_getProof` 返回的根一致。空集 → zero hash。
pub fn compute_unspent_mpt_root(outputs: &[crate::compute::ObjectOutput]) -> Hash {
    use std::sync::Arc;
    let mut live: Vec<&crate::compute::ObjectOutput> =
        outputs.iter().filter(|o| !o.spent).collect();
    if live.is_empty() {
        return Hash::zero();
    }
    live.sort_by(|a, b| a.output_id.0.as_bytes().cmp(b.output_id.0.as_bytes()));

    let db = Arc::new(crate::trie::MemTrieDB::new());
    let trie = Arc::new(crate::trie::MerklePatriciaTrie::new(db));
    for out in live {
        let mut clean = (*out).clone();
        clean.spent = false;
        let value = match bincode::serialize(&clean) {
            Ok(v) => v,
            Err(_) => return Hash::zero(),
        };
        let _ = trie.insert(out.output_id.0.as_bytes(), value);
    }
    let root = trie.root();
    if root == crate::trie::empty_trie_root() {
        return Hash::zero();
    }
    root
}
