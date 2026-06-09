//! Account manager trait and implementation

use super::{Account, AccountConfig, AccountError, AccountType, I256, U256};
use crate::account::utxo::{UtxoLock, UtxoOutput, UtxoReference};
use crate::crypto::{Address, Ed25519Signature, Hash};
use async_trait::async_trait;
use dashmap::DashMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Account manager trait
#[async_trait]
pub trait AccountManager: Send + Sync {
    /// Get account by address
    async fn get_account(&self, address: &Address) -> Result<Option<Account>, AccountError>;

    /// Create new account
    async fn create_account(
        &self,
        account_type: AccountType,
        config: AccountConfig,
    ) -> Result<Account, AccountError>;

    /// Update account balance
    async fn update_balance(
        &self,
        address: &Address,
        amount: I256,
        reason: BalanceChangeReason,
    ) -> Result<(), AccountError>;

    /// Spend UTXO
    async fn spend_utxo(
        &self,
        address: &Address,
        utxo_ref: &UtxoReference,
        signature: Ed25519Signature,
    ) -> Result<(), AccountError>;

    /// Create UTXO
    async fn create_utxo(
        &self,
        address: &Address,
        amount: U256,
        lock_rule: UtxoLock,
    ) -> Result<UtxoReference, AccountError>;

    /// Verify signature
    async fn verify_signature(
        &self,
        account: &Account,
        tx_hash: Hash,
        signature: Ed25519Signature,
    ) -> Result<bool, AccountError>;

    /// Get account nonce
    async fn get_nonce(&self, address: &Address) -> Result<u64, AccountError>;

    /// Increment account nonce
    async fn increment_nonce(&self, address: &Address) -> Result<(), AccountError>;

    /// Get storage value
    async fn get_storage(&self, address: &Address, key: Hash) -> Result<Hash, AccountError>;

    /// Set storage value
    async fn set_storage(
        &self,
        address: &Address,
        key: Hash,
        value: Hash,
    ) -> Result<(), AccountError>;

    /// Get account proof
    async fn get_proof(
        &self,
        address: &Address,
        keys: &[Hash],
    ) -> Result<AccountProof, AccountError>;

    /// Apply batch account changes
    async fn apply_changes(&self, changes: Vec<AccountChange>) -> Result<Hash, AccountError>;

    /// Get UTXOs for address
    async fn get_utxos(&self, address: &Address) -> Result<Vec<UtxoOutput>, AccountError>;
}

/// Balance change reason
#[derive(Clone, Debug)]
pub enum BalanceChangeReason {
    /// Block reward
    BlockReward,
    /// Uncle reward
    UncleReward,
    /// Operation fee
    TransactionFee,
    /// Transfer
    Transfer,
    /// Contract execution
    ContractExecution,
    /// Governance action
    Governance,
    /// Other
    Other(String),
}

/// Account change
#[derive(Clone, Debug)]
pub struct AccountChange {
    pub address: Address,
    pub balance_change: Option<I256>,
    pub nonce_change: Option<u64>,
    pub storage_changes: Vec<StorageChange>,
    pub code_change: Option<CodeChange>,
}

/// Storage change
#[derive(Clone, Debug)]
pub struct StorageChange {
    pub key: Hash,
    pub old_value: Hash,
    pub new_value: Hash,
}

/// Code change
#[derive(Clone, Debug)]
pub struct CodeChange {
    pub old_hash: Hash,
    pub new_code: Vec<u8>,
}

/// Account proof
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountProof {
    /// Account existence proof
    pub account_proof: Vec<Vec<u8>>,
    /// Storage proofs
    pub storage_proofs: Vec<Vec<Vec<u8>>>,
    /// State root
    pub state_root: Hash,
}

/// In-memory account manager (for testing and development)
pub struct InMemoryAccountManager {
    /// Account storage
    accounts: DashMap<Address, Account>,
    /// Storage trie (simplified)
    storage: DashMap<Address, DashMap<Hash, Hash>>,
    /// Contract code storage
    code: DashMap<Hash, Vec<u8>>,
    /// UTXO storage
    utxos: DashMap<Hash, UtxoOutput>,
    /// State root
    state_root: RwLock<Hash>,
}

impl InMemoryAccountManager {
    /// Create new in-memory account manager
    pub fn new() -> Self {
        Self {
            accounts: DashMap::new(),
            storage: DashMap::new(),
            code: DashMap::new(),
            utxos: DashMap::new(),
            state_root: RwLock::new(Hash::zero()),
        }
    }

    /// Create with genesis accounts
    pub fn with_genesis(genesis_accounts: Vec<Account>) -> Self {
        let manager = Self::new();

        for account in genesis_accounts {
            manager.accounts.insert(account.address, account);
        }

        manager
    }
}

impl Default for InMemoryAccountManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AccountManager for InMemoryAccountManager {
    async fn get_account(&self, address: &Address) -> Result<Option<Account>, AccountError> {
        Ok(self
            .accounts
            .get(address)
            .map(|entry| entry.value().clone()))
    }

    async fn create_account(
        &self,
        account_type: AccountType,
        config: AccountConfig,
    ) -> Result<Account, AccountError> {
        // Derive address from account type
        let address = match &account_type {
            AccountType::User { public_key } => Address::from_public_key(public_key),
            _ => {
                // For contract accounts, address would be computed differently
                Address::from_bytes([0u8; 20])
            }
        };

        let account = Account {
            address,
            account_type,
            config,
            state: super::AccountState::Active,
            created_at: current_timestamp(),
            updated_at: current_timestamp(),
            ..Default::default()
        };

        self.accounts.insert(address, account.clone());

        Ok(account)
    }

    async fn update_balance(
        &self,
        address: &Address,
        amount: I256,
        _reason: BalanceChangeReason,
    ) -> Result<(), AccountError> {
        let mut account = self
            .accounts
            .get_mut(address)
            .ok_or(AccountError::NotFound(*address))?;

        account.update_balance(amount)?;

        Ok(())
    }

    async fn spend_utxo(
        &self,
        _address: &Address,
        utxo_ref: &UtxoReference,
        _signature: Ed25519Signature,
    ) -> Result<(), AccountError> {
        // Mark UTXO as spent
        if let Some(mut utxo) = self.utxos.get_mut(&utxo_ref.tx_hash) {
            utxo.spend(utxo_ref.tx_hash);
        }

        Ok(())
    }

    async fn create_utxo(
        &self,
        _address: &Address,
        amount: U256,
        lock_rule: UtxoLock,
    ) -> Result<UtxoReference, AccountError> {
        let tx_hash = Hash::from_bytes([1u8; 32]); // Would be computed from operation payload

        let utxo = UtxoOutput::new(amount, lock_rule.clone());
        self.utxos.insert(tx_hash, utxo);

        Ok(UtxoReference {
            tx_hash,
            output_index: 0,
            amount,
            lock_rule,
            spent: false,
        })
    }

    async fn verify_signature(
        &self,
        account: &Account,
        tx_hash: Hash,
        signature: Ed25519Signature,
    ) -> Result<bool, AccountError> {
        account.verify_signature(tx_hash, signature)
    }

    async fn get_nonce(&self, address: &Address) -> Result<u64, AccountError> {
        let account = self
            .accounts
            .get(address)
            .ok_or(AccountError::NotFound(*address))?;

        Ok(account.nonce)
    }

    async fn increment_nonce(&self, address: &Address) -> Result<(), AccountError> {
        let mut account = self
            .accounts
            .get_mut(address)
            .ok_or(AccountError::NotFound(*address))?;

        account.increment_nonce();

        Ok(())
    }

    async fn get_storage(&self, address: &Address, key: Hash) -> Result<Hash, AccountError> {
        if let Some(storage_map) = self.storage.get(address) {
            if let Some(value) = storage_map.get(&key) {
                return Ok(*value);
            }
        }

        Ok(Hash::zero())
    }

    async fn set_storage(
        &self,
        address: &Address,
        key: Hash,
        value: Hash,
    ) -> Result<(), AccountError> {
        let storage_map = self.storage.entry(*address).or_default();
        storage_map.insert(key, value);

        Ok(())
    }

    async fn get_proof(
        &self,
        _address: &Address,
        _keys: &[Hash],
    ) -> Result<AccountProof, AccountError> {
        // Simplified - would generate Merkle proof in production
        Ok(AccountProof {
            account_proof: Vec::new(),
            storage_proofs: Vec::new(),
            state_root: *self.state_root.read(),
        })
    }

    async fn apply_changes(&self, changes: Vec<AccountChange>) -> Result<Hash, AccountError> {
        for change in changes {
            if let Some(balance_change) = change.balance_change {
                self.update_balance(
                    &change.address,
                    balance_change,
                    BalanceChangeReason::Other("batch".to_string()),
                )
                .await?;
            }

            if let Some(nonce_change) = change.nonce_change {
                let mut account = self
                    .accounts
                    .get_mut(&change.address)
                    .ok_or(AccountError::NotFound(change.address))?;
                account.nonce = nonce_change;
            }

            for storage_change in change.storage_changes {
                self.set_storage(
                    &change.address,
                    storage_change.key,
                    storage_change.new_value,
                )
                .await?;
            }

            if let Some(code_change) = change.code_change {
                self.code.insert(code_change.old_hash, code_change.new_code);
            }
        }

        // Update state root (simplified)
        *self.state_root.write() = Hash::from_bytes([1u8; 32]);

        Ok(*self.state_root.read())
    }

    async fn get_utxos(&self, _address: &Address) -> Result<Vec<UtxoOutput>, AccountError> {
        let utxos: Vec<UtxoOutput> = self
            .utxos
            .iter()
            .filter(|entry| !entry.value().spent)
            .map(|entry| entry.value().clone())
            .collect();

        Ok(utxos)
    }
}

fn current_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::Ed25519PrivateKey;

    #[tokio::test]
    async fn test_account_manager() {
        let manager = InMemoryAccountManager::new();

        // Create account
        let pk = Ed25519PrivateKey::random().public_key();
        let account_type = AccountType::User { public_key: pk };

        let account = manager
            .create_account(account_type, AccountConfig::default())
            .await
            .unwrap();

        // Get account
        let retrieved = manager
            .get_account(&account.address)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.address, account.address);

        // Update balance
        manager
            .update_balance(
                &account.address,
                I256::from(1000),
                BalanceChangeReason::Transfer,
            )
            .await
            .unwrap();

        let updated = manager
            .get_account(&account.address)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.balance.as_u64(), 1000);
    }

    #[tokio::test]
    async fn test_nonce_increment() {
        let manager = InMemoryAccountManager::new();

        let pk = Ed25519PrivateKey::random().public_key();
        let account_type = AccountType::User { public_key: pk };

        let account = manager
            .create_account(account_type, AccountConfig::default())
            .await
            .unwrap();

        // Initial nonce should be 0
        let nonce = manager.get_nonce(&account.address).await.unwrap();
        assert_eq!(nonce, 0);

        // Increment nonce
        manager.increment_nonce(&account.address).await.unwrap();

        let new_nonce = manager.get_nonce(&account.address).await.unwrap();
        assert_eq!(new_nonce, 1);
    }
}
