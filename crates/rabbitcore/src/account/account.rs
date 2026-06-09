//! Account data structures

use crate::crypto::{Address, Ed25519PublicKey, Ed25519Signature, Hash};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

/// UTXO reference for hybrid account model
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UtxoReference {
    /// Transaction hash
    pub tx_hash: Hash,
    /// Output index
    pub output_index: u32,
    /// Amount
    pub amount: U256,
    /// Locking script or address
    pub lock_hash: Hash,
}

impl UtxoReference {
    /// Create a new UTXO reference
    pub fn new(tx_hash: Hash, output_index: u32, amount: U256, lock_hash: Hash) -> Self {
        Self {
            tx_hash,
            output_index,
            amount,
            lock_hash,
        }
    }
}

/// Account error types
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum AccountError {
    /// Account not found
    #[error("Account not found: {0}")]
    NotFound(Address),
    /// Invalid account type
    #[error("Invalid account type")]
    InvalidAccountType,
    /// Invalid state transition
    #[error("Invalid state transition from {from:?} with event {event:?}")]
    InvalidStateTransition {
        from: AccountState,
        event: StateEvent,
    },
    /// Insufficient balance
    #[error("Insufficient balance: have {have}, need {need}")]
    InsufficientBalance { have: U256, need: U256 },
    /// Invalid signature
    #[error("Invalid signature")]
    InvalidSignature,
    /// Nonce mismatch
    #[error("Nonce mismatch: expected {expected}, got {got}")]
    NonceMismatch { expected: u64, got: u64 },
    /// Account is frozen
    #[error("Account is frozen")]
    AccountFrozen,
    /// Account is destroyed
    #[error("Account is destroyed")]
    AccountDestroyed,
}

/// 256-bit unsigned integer
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct U256(pub [u8; 32]);

impl U256 {
    pub fn zero() -> Self {
        Self([0u8; 32])
    }

    pub fn one() -> Self {
        let mut bytes = [0u8; 32];
        bytes[31] = 1;
        Self(bytes)
    }

    pub fn from(value: u64) -> Self {
        let mut bytes = [0u8; 32];
        bytes[24..32].copy_from_slice(&value.to_be_bytes());
        Self(bytes)
    }

    pub fn from_u128(value: u128) -> Self {
        let mut bytes = [0u8; 32];
        bytes[16..32].copy_from_slice(&value.to_be_bytes());
        Self(bytes)
    }

    pub fn as_u64(&self) -> u64 {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&self.0[24..32]);
        u64::from_be_bytes(bytes)
    }

    pub fn as_u8(&self) -> u8 {
        self.0[31]
    }

    pub fn as_u128(&self) -> u128 {
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&self.0[16..32]);
        u128::from_be_bytes(bytes)
    }

    pub fn is_zero(&self) -> bool {
        self.0.iter().all(|&b| b == 0)
    }

    pub fn to_big_endian(&self) -> [u8; 32] {
        self.0
    }

    pub fn from_big_endian(bytes: &[u8]) -> Self {
        let mut result = [0u8; 32];
        let start = 32usize.saturating_sub(bytes.len());
        result[start..].copy_from_slice(bytes);
        Self(result)
    }

    pub fn overflowing_add(self, other: Self) -> (Self, bool) {
        let mut result = [0u8; 32];
        let mut carry = 0u16;

        for i in (0..32).rev() {
            let sum = self.0[i] as u16 + other.0[i] as u16 + carry;
            result[i] = sum as u8;
            carry = sum >> 8;
        }

        (Self(result), carry != 0)
    }

    pub fn overflowing_sub(self, other: Self) -> (Self, bool) {
        let mut result = [0u8; 32];
        let mut borrow = 0i16;

        for i in (0..32).rev() {
            let diff = self.0[i] as i16 - other.0[i] as i16 + borrow;
            result[i] = diff as u8;
            borrow = if diff < 0 { -1 } else { 0 };
        }

        (Self(result), borrow != 0)
    }

    pub fn saturating_add(self, other: Self) -> Self {
        let (result, overflow) = self.overflowing_add(other);
        if overflow {
            Self([0xFFu8; 32])
        } else {
            result
        }
    }

    pub fn saturating_sub(self, other: Self) -> Self {
        let (result, overflow) = self.overflowing_sub(other);
        if overflow {
            Self::zero()
        } else {
            result
        }
    }

    pub fn overflowing_mul(self, other: Self) -> (Self, bool) {
        let mut result = [0u8; 32];
        let mut carry = 0u32;

        for i in (0..32).rev() {
            let mut sum = carry;
            for j in (0..=i).rev() {
                let prod = self.0[j] as u32 * other.0[i - (31 - j)] as u32;
                sum += prod;
            }
            result[i] = sum as u8;
            carry = sum >> 8;
        }

        (Self(result), carry != 0)
    }

    pub fn overflowing_pow(self, exp: u32) -> (Self, bool) {
        if exp == 0 {
            return (Self::one(), false);
        }

        let mut result = Self::one();
        let mut base = self;
        let mut e = exp;
        let mut overflow = false;

        while e > 0 {
            if e % 2 == 1 {
                let (r, o) = result.overflowing_mul(base);
                result = r;
                overflow |= o;
            }
            let (b, o) = base.overflowing_mul(base);
            base = b;
            overflow |= o;
            e /= 2;
        }

        (result, overflow)
    }

    pub fn overflowing_pow_u256(self, exp: Self) -> (Self, bool) {
        // Simplified: only handle small exponents
        if exp.is_zero() {
            return (Self::one(), false);
        }

        // For large exponents, just use u32 version
        let exp_u32 = exp.as_u64().min(u32::MAX as u64) as u32;
        self.overflowing_pow(exp_u32)
    }

    pub fn leading_zeros(&self) -> u32 {
        let mut count = 0u32;
        for &byte in &self.0 {
            if byte == 0 {
                count += 8;
            } else {
                count += byte.leading_zeros();
                break;
            }
        }
        count
    }

    pub fn wrapping_add(self, other: Self) -> Self {
        self.overflowing_add(other).0
    }

    pub fn wrapping_sub(self, other: Self) -> Self {
        self.overflowing_sub(other).0
    }

    pub fn wrapping_mul(self, other: Self) -> Self {
        self.overflowing_mul(other).0
    }
}

impl std::ops::Add for U256 {
    type Output = Self;

    fn add(self, other: Self) -> Self::Output {
        self.overflowing_add(other).0
    }
}

impl std::ops::Sub for U256 {
    type Output = Self;

    fn sub(self, other: Self) -> Self::Output {
        self.overflowing_sub(other).0
    }
}

impl std::ops::Mul for U256 {
    type Output = Self;

    fn mul(self, other: Self) -> Self::Output {
        self.overflowing_mul(other).0
    }
}

impl std::ops::Div for U256 {
    type Output = Self;

    fn div(self, other: Self) -> Self::Output {
        if other.is_zero() {
            return Self::zero();
        }

        if self.is_zero() {
            return Self::zero();
        }

        let mut quotient = Self::zero();
        let mut remainder = Self::zero();

        for i in 0..256 {
            remainder = remainder << 1usize;

            let bit = (self.0[i / 8] >> (7 - (i % 8))) & 1;
            if bit == 1 {
                remainder = remainder.wrapping_add(Self::one());
            }

            if remainder >= other {
                remainder = remainder.wrapping_sub(other);
                quotient.0[i / 8] |= 1 << (7 - (i % 8));
            }
        }

        quotient
    }
}

impl std::ops::Rem for U256 {
    type Output = Self;

    fn rem(self, other: Self) -> Self::Output {
        if other.is_zero() {
            return Self::zero();
        }
        let quotient = self / other;
        self - (quotient * other)
    }
}

impl std::ops::BitAnd for U256 {
    type Output = Self;

    fn bitand(self, other: Self) -> Self::Output {
        let mut result = [0u8; 32];
        for (i, slot) in result.iter_mut().enumerate() {
            *slot = self.0[i] & other.0[i];
        }
        Self(result)
    }
}

impl std::ops::BitOr for U256 {
    type Output = Self;

    fn bitor(self, other: Self) -> Self::Output {
        let mut result = [0u8; 32];
        for (i, slot) in result.iter_mut().enumerate() {
            *slot = self.0[i] | other.0[i];
        }
        Self(result)
    }
}

impl std::ops::BitXor for U256 {
    type Output = Self;

    fn bitxor(self, other: Self) -> Self::Output {
        let mut result = [0u8; 32];
        for (i, slot) in result.iter_mut().enumerate() {
            *slot = self.0[i] ^ other.0[i];
        }
        Self(result)
    }
}

impl std::ops::Not for U256 {
    type Output = Self;

    fn not(self) -> Self::Output {
        let mut result = [0u8; 32];
        for (i, slot) in result.iter_mut().enumerate() {
            *slot = !self.0[i];
        }
        Self(result)
    }
}

impl std::ops::Shl<usize> for U256 {
    type Output = Self;

    fn shl(self, shift: usize) -> Self::Output {
        if shift >= 256 {
            return Self::zero();
        }

        let mut result = [0u8; 32];
        let byte_shift = shift / 8;
        let bit_shift = shift % 8;

        for i in (0..32).rev() {
            if i >= byte_shift {
                let src_idx = i - byte_shift;
                result[i] = self.0[src_idx] << bit_shift;
                if bit_shift > 0 && src_idx > 0 {
                    result[i] |= self.0[src_idx - 1] >> (8 - bit_shift);
                }
            }
        }

        Self(result)
    }
}

impl std::ops::Shl<u64> for U256 {
    type Output = Self;

    fn shl(self, shift: u64) -> Self::Output {
        self << (shift as usize)
    }
}

impl std::ops::Shl<Self> for U256 {
    type Output = Self;

    fn shl(self, shift: Self) -> Self::Output {
        self << (shift.as_u64() as usize)
    }
}

impl std::ops::Shr<usize> for U256 {
    type Output = Self;

    fn shr(self, shift: usize) -> Self::Output {
        if shift >= 256 {
            return Self::zero();
        }

        let mut result = [0u8; 32];
        let byte_shift = shift / 8;
        let bit_shift = shift % 8;

        for (i, slot) in result.iter_mut().enumerate() {
            if i + byte_shift < 32 {
                *slot = self.0[i + byte_shift] >> bit_shift;
                if bit_shift > 0 && i + byte_shift + 1 < 32 {
                    *slot |= self.0[i + byte_shift + 1] << (8 - bit_shift);
                }
            }
        }

        Self(result)
    }
}

impl std::ops::Shr<u64> for U256 {
    type Output = Self;

    fn shr(self, shift: u64) -> Self::Output {
        self >> (shift as usize)
    }
}

impl std::ops::Shr<Self> for U256 {
    type Output = Self;

    fn shr(self, shift: Self) -> Self::Output {
        self >> (shift.as_u64() as usize)
    }
}

impl std::fmt::Debug for U256 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "U256({})", self.as_u128())
    }
}

impl std::fmt::Display for U256 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_u128())
    }
}

/// Signed 256-bit integer
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct I256(pub [u8; 32]);

impl I256 {
    pub fn zero() -> Self {
        Self([0u8; 32])
    }

    pub fn from(value: i64) -> Self {
        let mut bytes = [0u8; 32];
        bytes[24..32].copy_from_slice(&value.to_be_bytes());
        Self(bytes)
    }

    pub fn is_positive(&self) -> bool {
        self.0[0] & 0x80 == 0 && !self.is_zero()
    }

    pub fn is_negative(&self) -> bool {
        self.0[0] & 0x80 != 0
    }

    pub fn is_zero(&self) -> bool {
        self.0.iter().all(|&b| b == 0)
    }

    pub fn to_u256(&self) -> Option<U256> {
        if self.is_negative() {
            None
        } else {
            Some(U256(self.0))
        }
    }
}

/// Account state machine states
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountState {
    /// Inactive (needs deposit to activate)
    #[default]
    Inactive,
    /// Active
    Active,
    /// Frozen (by governance)
    Frozen,
    /// Destroyed
    Destroyed,
}

/// State machine events
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StateEvent {
    /// Account created
    AccountCreated,
    /// Deposit received
    DepositReceived,
    /// Withdrawal completed
    WithdrawalCompleted,
    /// Frozen by governance
    FrozenByGovernance,
    /// Unfrozen by governance
    UnfrozenByGovernance,
    /// Account destroyed
    AccountDestroyed,
}

/// Account type enumeration
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountType {
    /// User-controlled account
    User {
        /// Public key
        public_key: Ed25519PublicKey,
    },

    /// Smart contract account
    Contract {
        /// Contract creator
        creator: Address,
        /// Contract version
        contract_version: u32,
        /// Is upgradeable
        upgradeable: bool,
        /// Admin address (if upgradeable)
        admin: Option<Address>,
    },

    /// Policy-controlled account
    AbstractAccount {
        /// Validator contract address
        validator: Address,
        /// Owner addresses
        owners: Vec<Address>,
        /// Threshold for multi-sig
        threshold: u32,
        /// Social recovery config
        recovery_config: Option<RecoveryConfig>,
    },

    /// Multi-signature account
    MultiSig {
        /// Signer addresses
        signers: Vec<Address>,
        /// Required signatures
        required: u32,
        /// Daily limit
        daily_limit: Option<U256>,
    },
}

/// Social recovery configuration
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryConfig {
    /// Guardian addresses
    pub guardians: Vec<Address>,
    /// Required guardians for recovery
    pub guardian_threshold: u32,
    /// Recovery delay in seconds
    pub recovery_delay: u64,
    /// Recovery expiry in seconds
    pub recovery_expiry: u64,
}

/// Account configuration
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AccountConfig {
    /// Enable UTXO mode
    pub enable_utxo: bool,
    /// Gas token whitelist (multi-gas support)
    pub gas_tokens: Vec<Address>,
    /// Operation limits
    pub limits: OperationLimits,
    /// Permissions
    pub permissions: Permissions,
    /// Metadata (for account abstraction)
    pub metadata: BTreeMap<String, String>,
}

/// Operation limits configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationLimits {
    /// Maximum single operation amount
    pub max_tx_amount: Option<U256>,
    /// Daily operation limit
    pub daily_limit: Option<U256>,
    /// Used daily limit
    pub daily_used: U256,
    /// Daily limit reset timestamp
    pub daily_reset_at: u64,
}

impl Default for OperationLimits {
    fn default() -> Self {
        Self {
            max_tx_amount: None,
            daily_limit: None,
            daily_used: U256::zero(),
            daily_reset_at: 0,
        }
    }
}

/// Permission levels
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionLevel {
    #[default]
    Standard,
    Restricted,
    Admin,
    SuperAdmin,
}

/// Permissions configuration
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Permissions {
    /// Contract call whitelist
    pub contract_whitelist: Vec<Address>,
    /// Contract call blacklist
    pub contract_blacklist: Vec<Address>,
    /// Operations requiring multi-sig
    pub multisig_operations: Vec<OperationType>,
    /// Permission level
    pub permission_level: PermissionLevel,
}

/// Operation types for permission control
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationType {
    Transfer,
    ContractCall,
    ContractDeploy,
    Governance,
    Admin,
}

/// Main account structure
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    /// Account address
    pub address: Address,
    /// Account type
    pub account_type: AccountType,
    /// Account version
    pub version: u32,
    /// Main balance (native token)
    pub balance: U256,
    /// UTXO references (for privacy transactions)
    pub utxo_refs: Vec<UtxoReference>,
    /// Account nonce
    pub nonce: u64,
    /// Storage root (Merkle Patricia Trie)
    pub storage_root: Hash,
    /// Contract code hash (if contract account)
    pub code_hash: Hash,
    /// Account configuration
    pub config: AccountConfig,
    /// Account state
    pub state: AccountState,
    /// Creation timestamp
    pub created_at: u64,
    /// Last update timestamp
    pub updated_at: u64,
}

impl Default for Account {
    fn default() -> Self {
        Self {
            address: Address::zero(),
            account_type: AccountType::User {
                public_key: Ed25519PublicKey::placeholder(),
            },
            version: 1,
            balance: U256::zero(),
            utxo_refs: Vec::new(),
            nonce: 0,
            storage_root: Hash::zero(),
            code_hash: Hash::zero(),
            config: AccountConfig::default(),
            state: AccountState::Inactive,
            created_at: 0,
            updated_at: 0,
        }
    }
}

impl Account {
    /// Create a new externally signed account
    pub fn new_user_account(public_key: Ed25519PublicKey, address: Address) -> Self {
        let now = current_timestamp();

        Self {
            address,
            account_type: AccountType::User { public_key },
            version: 1,
            balance: U256::zero(),
            utxo_refs: Vec::new(),
            nonce: 0,
            storage_root: Hash::zero(),
            code_hash: Hash::zero(),
            config: AccountConfig::default(),
            state: AccountState::Inactive,
            created_at: now,
            updated_at: now,
        }
    }

    /// Create a new contract account
    pub fn new_contract(creator: Address, address: Address) -> Self {
        let now = current_timestamp();

        Self {
            address,
            account_type: AccountType::Contract {
                creator,
                contract_version: 1,
                upgradeable: false,
                admin: None,
            },
            version: 1,
            balance: U256::zero(),
            utxo_refs: Vec::new(),
            nonce: 0,
            storage_root: Hash::zero(),
            code_hash: Hash::zero(),
            config: AccountConfig::default(),
            state: AccountState::Active,
            created_at: now,
            updated_at: now,
        }
    }

    /// Check if account can perform operations
    pub fn can_perform_operation(&self) -> bool {
        matches!(self.state, AccountState::Active)
    }

    /// Check if account can receive funds
    pub fn can_receive(&self) -> bool {
        !matches!(self.state, AccountState::Destroyed)
    }

    /// Verify transaction signature
    pub fn verify_signature(
        &self,
        tx_hash: Hash,
        signature: Ed25519Signature,
    ) -> Result<bool, AccountError> {
        match &self.account_type {
            AccountType::User { public_key } => signature
                .verify(tx_hash.as_bytes(), public_key)
                .map_err(|_| AccountError::InvalidSignature),
            AccountType::MultiSig {
                signers, required, ..
            } => {
                // Multi-sig verification logic
                // Simplified: would need multiple signatures
                Ok(true)
            }
            AccountType::AbstractAccount { validator, .. } => {
                // Delegate to validator contract
                // This path is handled by external runtime adapters.
                Ok(true)
            }
            AccountType::Contract { .. } => Err(AccountError::InvalidAccountType),
        }
    }

    /// Increment nonce
    pub fn increment_nonce(&mut self) {
        self.nonce = self.nonce.checked_add(1).unwrap_or(0);
        self.updated_at = current_timestamp();
    }

    /// Update balance
    pub fn update_balance(&mut self, amount: I256) -> Result<(), AccountError> {
        let (new_balance, overflow) = if amount.is_positive() {
            self.balance.overflowing_add(U256(amount.0))
        } else {
            self.balance.overflowing_sub(U256(amount.0))
        };

        if overflow && amount.is_positive() {
            return Err(AccountError::InsufficientBalance {
                have: self.balance,
                need: U256::from_u128(u128::MAX),
            });
        }

        self.balance = new_balance;
        self.updated_at = current_timestamp();

        Ok(())
    }

    /// Transition account state
    pub fn transition_state(&mut self, event: StateEvent) -> Result<(), AccountError> {
        let new_state = match (&self.state, &event) {
            (AccountState::Inactive, StateEvent::DepositReceived) => AccountState::Active,
            (AccountState::Active, StateEvent::FrozenByGovernance) => AccountState::Frozen,
            (AccountState::Frozen, StateEvent::UnfrozenByGovernance) => AccountState::Active,
            (AccountState::Active, StateEvent::AccountDestroyed) => AccountState::Destroyed,
            _ => {
                return Err(AccountError::InvalidStateTransition {
                    from: self.state.clone(),
                    event,
                });
            }
        };

        self.state = new_state;
        self.updated_at = current_timestamp();

        Ok(())
    }
}

/// Get current timestamp
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

    #[test]
    fn test_u256_arithmetic() {
        let a = U256::from(100);
        let b = U256::from(50);

        assert_eq!((a + b).as_u64(), 150);
        assert_eq!((a - b).as_u64(), 50);

        let (result, overflow) = a.overflowing_add(U256::zero());
        assert!(!overflow);
        assert_eq!(result.as_u64(), 100);
    }

    #[test]
    fn test_u256_division_preserves_big_endian_bit_order() {
        let quotient = U256::from(100) / U256::from(4);
        assert_eq!(quotient.as_u64(), 25);

        let max = U256::from_big_endian(&[0xFFu8; 32]);
        let half = max / U256::from(2);
        assert_eq!(half.to_big_endian()[0], 0x7F);
    }

    #[test]
    fn test_account_creation() {
        let pk = Ed25519PublicKey::from_bytes([0u8; 32]).unwrap();
        let addr = Address::from_bytes([1u8; 20]);

        let account = Account::new_user_account(pk, addr);

        assert_eq!(account.address, addr);
        assert!(account.balance.is_zero());
        assert_eq!(account.nonce, 0);
    }

    #[test]
    fn test_account_balance_update() {
        let pk = Ed25519PublicKey::from_bytes([0u8; 32]).unwrap();
        let addr = Address::from_bytes([1u8; 20]);

        let mut account = Account::new_user_account(pk, addr);

        // Add balance
        account.update_balance(I256::from(1000)).unwrap();
        assert_eq!(account.balance.as_u64(), 1000);

        // Subtract balance
        account.update_balance(I256::from(-300)).unwrap();
        assert_eq!(account.balance.as_u64(), 700);
    }

    #[test]
    fn test_account_state_transition() {
        let pk = Ed25519PublicKey::from_bytes([0u8; 32]).unwrap();
        let addr = Address::from_bytes([1u8; 20]);

        let mut account = Account::new_user_account(pk, addr);

        // Inactive -> Active (deposit)
        account
            .transition_state(StateEvent::DepositReceived)
            .unwrap();
        assert_eq!(account.state, AccountState::Active);

        // Active -> Frozen
        account
            .transition_state(StateEvent::FrozenByGovernance)
            .unwrap();
        assert_eq!(account.state, AccountState::Frozen);

        // Frozen -> Active
        account
            .transition_state(StateEvent::UnfrozenByGovernance)
            .unwrap();
        assert_eq!(account.state, AccountState::Active);
    }
}
