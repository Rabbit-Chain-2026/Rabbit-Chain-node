//! Gas metering and EIP-1559 base fee adjustment for RabbitChain ComputeTx.
//!
//! Unit hierarchy:
//!   hopps (hp)   = 1                  (chain base unit)
//!   carrot (car) = 10⁹ hopps          (gas/fee unit, similar to Gwei)
//!   Rbit         = 10¹⁸ hopps         (main currency unit)

use crate::compute::{
    Command, ComputeTx, ObjectReadRef, OutputId, OutputProposal,
};

// ─── Constants ─────────────────────────────────────────────────────────

/// Minimum gas per transaction (signature verify + structural validation).
pub const TX_BASE_GAS: u64 = 21_000;

/// Per byte of calldata — zero byte.
pub const TX_CALLDATA_ZERO_GAS: u64 = 4;
/// Per byte of calldata — non-zero byte.
pub const TX_CALLDATA_NONZERO_GAS: u64 = 16;

/// Per compute command (Transfer/Invoke/Mint/Burn etc.).
pub const COMMAND_GAS: u64 = 5_000;

/// First (cold) access to a domain/account/object output.
pub const COLD_ACCESS_GAS: u64 = 2_100;
/// Repeated (warm) access within same execution.
pub const WARM_ACCESS_GAS: u64 = 100;

/// Arithmetic instruction (add/sub/mul).
pub const ARITH_GAS: u64 = 3;
/// Division/modulo.
pub const DIV_GAS: u64 = 10;
/// Comparison/branch.
pub const BRANCH_GAS: u64 = 2;

/// Keccak256 hash — per 32 bytes.
pub const KECCAK256_GAS_PER_32B: u64 = 30;
/// SHA3-256 — per 32 bytes.
pub const SHA3_256_GAS_PER_32B: u64 = 50;
/// BLAKE3 — per 32 bytes.
pub const BLAKE3_GAS_PER_32B: u64 = 10;

/// Ed25519 signature verification.
pub const ED25519_VERIFY_GAS: u64 = 3_000;
/// Ed25519 signature generation.
pub const ED25519_SIGN_GAS: u64 = 5_000;

/// Create a new output (UTXO).
pub const OUTPUT_CREATE_GAS: u64 = 25_000;
/// Consume / delete an existing output.
pub const OUTPUT_DELETE_GAS: u64 = 35_000;
/// Read an output from store (cold).
pub const OUTPUT_READ_COLD_GAS: u64 = 2_100;
/// Read an output from store (warm, same execution context).
pub const OUTPUT_READ_WARM_GAS: u64 = 100;

/// Write a latest_key entry.
pub const LATEST_KEY_WRITE_GAS: u64 = 5_000;
/// Read a latest_key entry.
pub const LATEST_KEY_READ_GAS: u64 = 800;
/// Write a checkpoint.
pub const CHECKPOINT_WRITE_GAS: u64 = 2_000;

/// Trie node read (from MPT).
pub const TRIE_NODE_READ_GAS: u64 = 700;
/// Trie node store (write + hash update).
pub const TRIE_NODE_STORE_GAS: u64 = 4_000;

// ─── Refunds ────────────────────────────────────────────────────────────

/// Refund for consuming old output and creating a new one (compression incentive).
pub const CONSUME_AND_CREATE_REFUND: u64 = 10_000;
/// Refund for deleting a checkpoint.
pub const CHECKPOINT_DELETE_REFUND: u64 = 1_500;

// ─── EIP-1559 ───────────────────────────────────────────────────────────

/// Denominator for base fee adjustment (EIP-1559: 1/8 = 12.5%).
pub const BASE_FEE_MAX_CHANGE_DENOMINATOR: u64 = 8;
/// Target gas utilization = block_gas_limit / 2.
pub const TARGET_GAS_FRACTION: u64 = 2;

/// Default block gas limit (30M gas — generous for UTXO Compute).
pub const DEFAULT_BLOCK_GAS_LIMIT: u64 = 30_000_000;

/// Initial base fee for genesis: 1 SHC (山海币) per gas.
///
/// SHC is the native account asset (`StateDb.Account.balance`); all EIP-1559
/// fee fields (`max_fee`/`priority_fee`/`base_fee_per_gas`) are denominated in
/// SHC, not the legacy hopps/Rbit unit.
pub const INITIAL_BASE_FEE: u64 = 1;

/// Maximum priority fee (SHC).
pub const MAX_PRIORITY_FEE: u64 = 1_000_000;

/// Maximum gas limit per transaction. Prevents a single tx from claiming the
/// entire block gas limit and starving other transactions.
pub const MAX_GAS_LIMIT_PER_TX: u64 = 5_000_000;

// ─── Conversion helpers ─────────────────────────────────────────────────

/// Convert carrots to hopps.
pub const fn carrots_to_hopps(carrots: u64) -> u64 {
    carrots * 1_000_000_000
}

/// Convert hopps to carrots (floor division).
pub const fn hopps_to_carrots(hopps: u64) -> u64 {
    hopps / 1_000_000_000
}

/// Convert Rbit to hopps.
pub const fn rbit_to_hopps(rbit: u64) -> u128 {
    (rbit as u128) * 1_000_000_000_000_000_000
}

// ─── Gas estimation ────────────────────────────────────────────────────

/// Estimate gas usage for a ComputeTx based on its structure.
///
/// This is a *static* estimation, used by `rabbit_estimateGas` RPC
/// and for initial validation. The actual execution may use less gas
/// (refunded) or fail (consuming all gas).
pub fn estimate_tx_gas(tx: &ComputeTx) -> u64 {
    let mut gas = TX_BASE_GAS;

    // Calldata bytes
    for byte in &tx.payload {
        if *byte == 0 {
            gas = gas.saturating_add(TX_CALLDATA_ZERO_GAS);
        } else {
            gas = gas.saturating_add(TX_CALLDATA_NONZERO_GAS);
        }
    }

    // Read set: first read is cold, repeated reads of the same output are warm.
    let mut seen: Vec<OutputId> = Vec::with_capacity(tx.read_set.len());
    for read in &tx.read_set {
        if seen.contains(&read.output_id) {
            gas = gas.saturating_add(WARM_ACCESS_GAS);
        } else {
            gas = gas.saturating_add(COLD_ACCESS_GAS);
            seen.push(read.output_id);
        }
    }

    // Input set: each input needs a cold read
    for _ in &tx.input_set {
        gas = gas.saturating_add(OUTPUT_READ_COLD_GAS);
    }

    // Output proposals: create new UTXOs
    for proposal in &tx.output_proposals {
        gas = gas.saturating_add(OUTPUT_CREATE_GAS);
        // State blob bytes cost calldata-like fees
        gas = gas.saturating_add((proposal.state.len() as u64).saturating_mul(TX_CALLDATA_NONZERO_GAS));
        // Lock script
        gas = gas.saturating_add((proposal.lock.code.len() as u64).saturating_mul(TX_CALLDATA_NONZERO_GAS));
        // Logic script (if present)
        if let Some(logic) = &proposal.logic {
            gas = gas.saturating_add((logic.code.len() as u64).saturating_mul(TX_CALLDATA_NONZERO_GAS));
        }
    }

    // Command base fee
    gas = gas.saturating_add(COMMAND_GAS);

    // Witness verification cost
    for sig in &tx.witness.signatures {
        gas = gas.saturating_add(ED25519_VERIFY_GAS);
    }

    // Metadata overhead
    for (key, value) in &tx.metadata {
        gas = gas.saturating_add((key.len() as u64).saturating_mul(TX_CALLDATA_NONZERO_GAS));
        gas = gas.saturating_add((value.len() as u64).saturating_mul(TX_CALLDATA_NONZERO_GAS));
    }

    gas
}

// ─── Base fee adjustment (EIP-1559) ────────────────────────────────────

/// Calculate the next block's base fee using EIP-1559 adjustment rule.
///
/// # Arguments
/// * `parent_base_fee`  - Base fee of the parent block (in hopps).
/// * `parent_gas_used`  - Gas used in the parent block.
/// * `parent_gas_limit` - Gas limit of the parent block.
///
/// # Returns
/// The new base fee for the current block (in hopps).
///
/// # Formula
/// ```text
/// target_gas = gas_limit / 2
/// if gas_used > target_gas:
///     base_fee += max(1, base_fee * (gas_used - target_gas) / (target_gas * 8))
/// else:
///     base_fee -= base_fee * (target_gas - gas_used) / (target_gas * 8)
/// ```
pub fn calculate_base_fee(
    parent_base_fee: u64,
    parent_gas_used: u64,
    parent_gas_limit: u64,
) -> u64 {
    let target_gas = parent_gas_limit / TARGET_GAS_FRACTION;

    if parent_gas_used == target_gas {
        return parent_base_fee;
    }

    let delta = parent_gas_used.abs_diff(target_gas);

    // change = base_fee * delta / (target_gas * 8)
    let numerator = (parent_base_fee as u128) * (delta as u128);
    let denominator = (target_gas as u128) * (BASE_FEE_MAX_CHANGE_DENOMINATOR as u128);
    let change = (numerator / denominator) as u64;

    if parent_gas_used > target_gas {
        // Fee increases — congestion
        parent_base_fee.saturating_add(change.max(1))
    } else {
        // Fee decreases — under-utilized
        parent_base_fee.saturating_sub(change)
    }
}

/// Validate that a ComputeTx's fee parameters are acceptable.
///
/// All fee amounts are in **hopps** (the chain's base unit):
/// - `max_fee`      — maximum total fee the sender will pay (hopps)
/// - `priority_fee` — tip for the miner, on top of the base fee (hopps)
/// - `base_fee`     — current EIP-1559 base fee (hopps)
///
/// Checks:
/// - `gas_limit` is within [estimated_gas, MAX_GAS_LIMIT_PER_TX]
/// - `priority_fee <= MAX_PRIORITY_FEE`
/// - `max_fee >= base_fee + priority_fee` (saturating arithmetic)
pub fn validate_tx_fee(
    tx: &ComputeTx,
    base_fee: u64,
) -> Result<(), FeeValidationError> {
    let estimated = estimate_tx_gas(tx);

    if tx.gas_limit < estimated {
        return Err(FeeValidationError::GasLimitTooLow {
            gas_limit: tx.gas_limit,
            estimated,
        });
    }

    if tx.gas_limit > MAX_GAS_LIMIT_PER_TX {
        return Err(FeeValidationError::GasLimitTooHigh {
            gas_limit: tx.gas_limit,
            max_allowed: MAX_GAS_LIMIT_PER_TX,
        });
    }

    if tx.priority_fee > MAX_PRIORITY_FEE {
        return Err(FeeValidationError::PriorityFeeTooHigh {
            priority_fee: tx.priority_fee,
            max_allowed: MAX_PRIORITY_FEE,
        });
    }

    // Saturating add so an absurd base_fee + priority_fee never wraps around.
    let min_total = base_fee.saturating_add(tx.priority_fee);
    if tx.max_fee < min_total {
        return Err(FeeValidationError::MaxFeeTooLow {
            max_fee: tx.max_fee,
            required: min_total,
            base_fee,
            priority_fee: tx.priority_fee,
        });
    }

    Ok(())
}

/// Calculate the effective tip for a transaction given the current base fee.
///
/// `effective_tip = min(priority_fee, max_fee - base_fee)` (all in hopps).
/// This ensures the miner never gets more than the sender authorized.
pub fn effective_tip(tx: &ComputeTx, base_fee: u64) -> u64 {
    std::cmp::min(tx.priority_fee, tx.max_fee.saturating_sub(base_fee))
}

/// Calculate the effective tip per gas unit for prioritization.
///
/// Uses the *estimated* gas (not the user-declared gas_limit) so that a user
/// cannot inflate their apparent priority by over-declaring gas_limit.
pub fn effective_tip_rate(tx: &ComputeTx, base_fee: u64) -> u64 {
    let tip = effective_tip(tx, base_fee);
    let gas = estimate_tx_gas(tx).max(1);
    tip / gas
}

// ─── Error types ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeeValidationError {
    GasLimitTooLow {
        gas_limit: u64,
        estimated: u64,
    },
    GasLimitTooHigh {
        gas_limit: u64,
        max_allowed: u64,
    },
    PriorityFeeTooHigh {
        priority_fee: u64,
        max_allowed: u64,
    },
    MaxFeeTooLow {
        max_fee: u64,
        required: u64,
        base_fee: u64,
        priority_fee: u64,
    },
}

impl std::fmt::Display for FeeValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GasLimitTooLow { gas_limit, estimated } => {
                write!(f, "gas limit {gas_limit} too low, estimated {estimated}")
            }
            Self::GasLimitTooHigh { gas_limit, max_allowed } => {
                write!(f, "gas limit {gas_limit} exceeds max {max_allowed}")
            }
            Self::PriorityFeeTooHigh { priority_fee, max_allowed } => {
                write!(f, "priority fee {priority_fee} exceeds max {max_allowed}")
            }
            Self::MaxFeeTooLow { max_fee, required, base_fee, priority_fee } => {
                write!(f, "max fee {max_fee} too low (need >= {required}: base_fee={base_fee} + priority_fee={priority_fee})")
            }
        }
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_fee_stays_same_at_target_utilization() {
        let base = calculate_base_fee(100, 15_000_000, 30_000_000);
        assert_eq!(base, 100);
    }

    #[test]
    fn base_fee_increases_when_over_target() {
        let base = calculate_base_fee(100, 20_000_000, 30_000_000);
        // target = 15M, delta = 5M
        // change = 100 * 5M / (15M * 8) = 500M / 120M = 4.16...
        assert!(base > 100);
    }

    #[test]
    fn base_fee_decreases_when_under_target() {
        let base = calculate_base_fee(100, 10_000_000, 30_000_000);
        // target = 15M, delta = 5M
        assert!(base < 100);
    }

    #[test]
    fn base_fee_does_not_underflow() {
        // With parent_base_fee=1 and utilization=0, change rounds to 0
        // so base_fee stays at 1 (floor of the EIP-1559 formula).
        let base = calculate_base_fee(1, 0, 30_000_000);
        assert_eq!(base, 1);
        // With a larger fee it can decrease meaningfully.
        let base2 = calculate_base_fee(100, 0, 30_000_000);
        assert!(base2 < 100);
    }

    #[test]
    fn effective_tip_never_exceeds_priority_fee() {
        let mut tx = ComputeTx {
            max_fee: 50,
            priority_fee: 10,
            gas_limit: 21_000,
            ..create_dummy()
        };
        assert_eq!(effective_tip(&tx, 30), 10); // 50-30 >= 10
        assert_eq!(effective_tip(&tx, 45), 5);  // 50-45 = 5 < 10
    }

    #[test]
    fn validate_rejects_insufficient_gas_limit() {
        let tx = ComputeTx {
            gas_limit: 100,
            max_fee: 1000,
            priority_fee: 10,
            ..create_dummy()
        };
        let result = validate_tx_fee(&tx, 50);
        assert!(result.is_err());
    }

    #[test]
    fn validate_rejects_max_fee_too_low() {
        let tx = ComputeTx {
            max_fee: 30,
            priority_fee: 20,
            gas_limit: 1_000_000,
            ..create_dummy()
        };
        let result = validate_tx_fee(&tx, 50);
        assert!(result.is_err()); // max_fee=30 < base_fee(50)+priority_fee(20)=70
    }

    #[test]
    fn validate_rejects_gas_limit_above_max() {
        let tx = ComputeTx {
            max_fee: u64::MAX,
            priority_fee: 0,
            gas_limit: MAX_GAS_LIMIT_PER_TX + 1,
            ..create_dummy()
        };
        let result = validate_tx_fee(&tx, 50);
        assert!(matches!(result, Err(FeeValidationError::GasLimitTooHigh { .. })));
    }

    #[test]
    fn validate_accepts_gas_limit_at_max() {
        let tx = ComputeTx {
            max_fee: u64::MAX,
            priority_fee: 0,
            gas_limit: MAX_GAS_LIMIT_PER_TX,
            ..create_dummy()
        };
        assert!(validate_tx_fee(&tx, 50).is_ok());
    }

    #[test]
    fn validate_no_overflow_on_absurd_fees() {
        // base_fee + priority_fee would overflow u64; must not panic.
        let tx = ComputeTx {
            max_fee: u64::MAX,
            priority_fee: MAX_PRIORITY_FEE, // within cap
            gas_limit: 1_000_000,
            ..create_dummy()
        };
        let result = validate_tx_fee(&tx, u64::MAX - 100);
        // min_total saturates to u64::MAX; max_fee == u64::MAX passes.
        assert!(result.is_ok());
    }

    #[test]
    fn effective_tip_rate_uses_estimated_gas_not_declared_limit() {
        let tx = ComputeTx {
            max_fee: 100_000,
            priority_fee: 50_000,
            gas_limit: 5_000_000, // inflated limit
            ..create_dummy()
        };
        // estimate_tx_gas for a bare Mint with no payload is TX_BASE_GAS + COMMAND_GAS
        let estimated = estimate_tx_gas(&tx);
        let expected_rate = 50_000 / estimated.max(1);
        assert_eq!(effective_tip_rate(&tx, 10_000), expected_rate);
    }

    #[test]
    fn estimate_tx_gas_basic() {
        let tx = ComputeTx {
            tx_id: crate::compute::TxId(crate::crypto::Hash::zero()),
            domain_id: crate::compute::DomainId(0),
            command: Command::Mint,
            input_set: vec![],
            read_set: vec![],
            output_proposals: vec![],
            fee: 0,
            nonce: Some(1),
            metadata: vec![],
            payload: vec![],
            deadline_unix_secs: None,
            chain_id: Some(10086),
            network_id: Some(10086),
            witness: crate::compute::TxWitness {
                signatures: vec![],
                threshold: Some(1),
            },
            max_fee: 0,
            priority_fee: 0,
            gas_limit: 1_000_000,
        };
        let gas = estimate_tx_gas(&tx);
        assert_eq!(gas, TX_BASE_GAS + COMMAND_GAS); // 21_000 + 5_000 = 26_000
    }

    /// Helper to create a dummy ComputeTx field for `..` update syntax.
    fn create_dummy() -> ComputeTx {
        ComputeTx {
            tx_id: crate::compute::TxId(crate::crypto::Hash::zero()),
            domain_id: crate::compute::DomainId(0),
            command: Command::Mint,
            input_set: vec![],
            read_set: vec![],
            output_proposals: vec![],
            fee: 0,
            nonce: Some(1),
            metadata: vec![],
            payload: vec![],
            deadline_unix_secs: None,
            chain_id: None,
            network_id: None,
            witness: crate::compute::TxWitness {
                signatures: vec![],
                threshold: None,
            },
            max_fee: 0,
            priority_fee: 0,
            gas_limit: 0,
        }
    }
}
