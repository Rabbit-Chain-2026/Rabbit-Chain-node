//! Authorization/resource policy traits and default policies.

use super::{
    error::{ComputeError, ComputeResult},
    object::{AssetId, ObjectOutput, Ownership, ResourceMap, ResourceValue, Script},
    tx::{Command, ComputeTx, SignatureScheme, TxSignature},
};
use crate::crypto::{Address, Hash};
use ed25519_dalek::{Signature as Ed25519Signature, Verifier as _};
use std::{collections::HashMap, sync::Arc};

/// Authorization policy checks ownership/capability validity.
pub trait AuthorizationPolicy: Send + Sync {
    /// Validates operation authorization against resolved inputs/reads.
    fn authorize(
        &self,
        tx: &ComputeTx,
        inputs: &[ObjectOutput],
        reads: &[ObjectOutput],
    ) -> ComputeResult<()>;
}

/// Resource policy validates resource accounting constraints.
pub trait ResourcePolicy: Send + Sync {
    /// Checks resource constraints for one operation context.
    fn check_resources(
        &self,
        tx: &ComputeTx,
        inputs: &[ObjectOutput],
        reads: &[ObjectOutput],
    ) -> ComputeResult<()>;
}

/// Default authorization policy for scaffolding stage.
#[derive(Debug, Default, Clone)]
pub struct DefaultAuthorizationPolicy;

impl AuthorizationPolicy for DefaultAuthorizationPolicy {
    fn authorize(
        &self,
        tx: &ComputeTx,
        inputs: &[ObjectOutput],
        _reads: &[ObjectOutput],
    ) -> ComputeResult<()> {
        if !tx.has_consistent_tx_id() {
            return Err(ComputeError::TxIdMismatch);
        }

        let required = tx.witness.threshold.unwrap_or(1) as usize;
        if required == 0 {
            return Err(ComputeError::AuthorizationDenied);
        }

        if tx.witness.signatures.len() < required {
            return Err(ComputeError::AuthorizationDenied);
        }

        // Minimal command-level restrictions for scaffolding stage.
        if matches!(tx.command, Command::Mint | Command::Burn) {
            for input in inputs {
                if !matches!(input.owner, Ownership::Program(_) | Ownership::Shared) {
                    return Err(ComputeError::AuthorizationDenied);
                }
            }
        }

        // Minimal owner presence check for transfer/invoke.
        if matches!(tx.command, Command::Transfer | Command::Invoke) {
            for input in inputs {
                if let Ownership::Address(addr) = input.owner {
                    if addr.is_zero() {
                        return Err(ComputeError::OwnershipCheckFailed);
                    }
                }
            }

            for input in inputs {
                match input.owner {
                    Ownership::Address(owner_addr) => {
                        if tx.witness.signatures.is_empty() {
                            return Err(ComputeError::AuthorizationDenied);
                        }
                        match ed25519_signature_matches_address_owner(
                            tx,
                            owner_addr,
                            &tx.witness.signatures,
                        ) {
                            Ok(true) => {}
                            Ok(false) => return Err(ComputeError::SignatureOwnerMismatch),
                            Err(err) => return Err(err),
                        }
                    }
                    Ownership::Ed25519(owner_pubkey) => {
                        if tx.witness.signatures.is_empty() {
                            return Err(ComputeError::AuthorizationDenied);
                        }
                        match ed25519_signature_matches_owner(
                            tx,
                            &owner_pubkey,
                            &tx.witness.signatures,
                        ) {
                            Ok(true) => {}
                            Ok(false) => return Err(ComputeError::SignatureOwnerMismatch),
                            Err(err) => return Err(err),
                        }
                    }
                    _ => {}
                }
            }
        }

        // P0 lock-script execution entry: interpret per-input lock script with tx witness/payload.
        for input in inputs {
            evaluate_lock_script(&input.lock, tx)?;
        }

        Ok(())
    }
}

fn ed25519_signature_matches_address_owner(
    tx: &ComputeTx,
    owner: Address,
    signatures: &[TxSignature],
) -> ComputeResult<bool> {
    let message = signing_message_for_tx(tx);

    for sig in signatures {
        if sig.scheme != SignatureScheme::Ed25519 {
            continue;
        }
        let Some(pk_bytes) = &sig.public_key else {
            return Err(ComputeError::InvalidSignature);
        };
        if pk_bytes.len() != 32 {
            return Err(ComputeError::InvalidSignature);
        }
        let derived = address_from_ed25519_public_key(pk_bytes)?;
        if derived != owner {
            continue;
        }
        let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(
            pk_bytes
                .as_slice()
                .try_into()
                .map_err(|_| ComputeError::InvalidSignature)?,
        )
        .map_err(|_| ComputeError::InvalidSignature)?;
        let sig_obj =
            Ed25519Signature::from_slice(&sig.bytes).map_err(|_| ComputeError::InvalidSignature)?;
        if verifying_key.verify(&message, &sig_obj).is_ok() {
            return Ok(true);
        }
    }

    Ok(false)
}

fn ed25519_signature_matches_owner(
    tx: &ComputeTx,
    owner_pubkey: &[u8; 32],
    signatures: &[TxSignature],
) -> ComputeResult<bool> {
    let message = signing_message_for_tx(tx);
    let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(owner_pubkey)
        .map_err(|_| ComputeError::OwnershipCheckFailed)?;

    for sig in signatures {
        if sig.scheme != SignatureScheme::Ed25519 {
            continue;
        }
        let Some(pk_bytes) = &sig.public_key else {
            return Err(ComputeError::InvalidSignature);
        };
        if pk_bytes.len() != 32 {
            return Err(ComputeError::InvalidSignature);
        }
        if pk_bytes.as_slice() != owner_pubkey {
            continue;
        }
        let sig_obj =
            Ed25519Signature::from_slice(&sig.bytes).map_err(|_| ComputeError::InvalidSignature)?;
        if verifying_key.verify(&message, &sig_obj).is_ok() {
            return Ok(true);
        }
    }

    Ok(false)
}

fn signing_message_for_tx(tx: &ComputeTx) -> Vec<u8> {
    tx.signing_preimage()
}

fn evaluate_lock_script(lock: &Script, tx: &ComputeTx) -> ComputeResult<()> {
    if lock.code.is_empty() {
        return Ok(());
    }

    if !matches!(lock.vm, 0 | 1) {
        return Err(ComputeError::InvalidOperation(format!(
            "unsupported lock vm: {}",
            lock.vm
        )));
    }

    let expr = std::str::from_utf8(&lock.code)
        .map_err(|_| ComputeError::InvalidOperation("lock script must be utf8".to_string()))?;

    match expr {
        "ALLOW" => Ok(()),
        "REQUIRE_SIG" => {
            if tx.witness.signatures.is_empty() {
                Err(ComputeError::AuthorizationDenied)
            } else {
                Ok(())
            }
        }
        "REQUIRE_ED25519" => {
            if tx
                .witness
                .signatures
                .iter()
                .any(|sig| sig.scheme == SignatureScheme::Ed25519)
            {
                Ok(())
            } else {
                Err(ComputeError::AuthorizationDenied)
            }
        }
        _ if expr.starts_with("PAYLOAD_EQ:") => {
            let expected_hex = expr.trim_start_matches("PAYLOAD_EQ:");
            let expected = hex::decode(expected_hex).map_err(|e| {
                ComputeError::InvalidOperation(format!("invalid PAYLOAD_EQ hex: {e}"))
            })?;
            if expected == tx.payload {
                Ok(())
            } else {
                Err(ComputeError::AuthorizationDenied)
            }
        }
        _ => Err(ComputeError::InvalidOperation(format!(
            "unsupported lock expression: {expr}"
        ))),
    }
}

fn address_from_ed25519_public_key(public_key: &[u8]) -> ComputeResult<Address> {
    if public_key.len() != 32 {
        return Err(ComputeError::InvalidSignature);
    }
    let hash = crate::crypto::keccak256(public_key);
    Address::from_slice(&hash[12..]).map_err(|_| ComputeError::InvalidSignature)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceKind {
    Amount,
    Data,
    Ref,
    RefBatch,
}

#[derive(Debug, Default, Clone, Copy)]
struct ResourceCounter {
    input_amount: u128,
    output_amount: u128,
    input_non_amount: u64,
    output_non_amount: u64,
    kind: Option<ResourceKind>,
}

fn kind_of_value(value: &ResourceValue) -> ResourceKind {
    match value {
        ResourceValue::Amount(_) => ResourceKind::Amount,
        ResourceValue::Data(_) => ResourceKind::Data,
        ResourceValue::Ref(_) => ResourceKind::Ref,
        ResourceValue::RefBatch(_) => ResourceKind::RefBatch,
    }
}

fn validate_resource_map(resources: &ResourceMap) -> ComputeResult<()> {
    for window in resources.windows(2) {
        if window[0].0 >= window[1].0 {
            return Err(ComputeError::ResourcePolicyViolation);
        }
    }
    Ok(())
}

fn update_resource_counter(
    counters: &mut HashMap<AssetId, ResourceCounter>,
    asset_id: AssetId,
    value: &ResourceValue,
    input_side: bool,
) -> ComputeResult<()> {
    let entry = counters.entry(asset_id).or_default();
    let kind = kind_of_value(value);

    if let Some(existing) = entry.kind {
        if existing != kind {
            return Err(ComputeError::ResourcePolicyViolation);
        }
    } else {
        entry.kind = Some(kind);
    }

    match value {
        ResourceValue::Amount(v) => {
            if input_side {
                entry.input_amount = entry
                    .input_amount
                    .checked_add(*v)
                    .ok_or(ComputeError::ResourcePolicyViolation)?;
            } else {
                entry.output_amount = entry
                    .output_amount
                    .checked_add(*v)
                    .ok_or(ComputeError::ResourcePolicyViolation)?;
            }
        }
        ResourceValue::Data(_) | ResourceValue::Ref(_) | ResourceValue::RefBatch(_) => {
            if input_side {
                entry.input_non_amount = entry
                    .input_non_amount
                    .checked_add(1)
                    .ok_or(ComputeError::ResourcePolicyViolation)?;
            } else {
                entry.output_non_amount = entry
                    .output_non_amount
                    .checked_add(1)
                    .ok_or(ComputeError::ResourcePolicyViolation)?;
            }
        }
    }

    Ok(())
}

fn aggregate_resources(
    tx: &ComputeTx,
    inputs: &[ObjectOutput],
) -> ComputeResult<HashMap<AssetId, ResourceCounter>> {
    let mut counters = HashMap::new();

    for input in inputs {
        validate_resource_map(&input.resources)?;
        for (asset_id, value) in &input.resources {
            update_resource_counter(&mut counters, *asset_id, value, true)?;
        }
    }

    for proposal in &tx.output_proposals {
        validate_resource_map(&proposal.resources)?;
        for (asset_id, value) in &proposal.resources {
            update_resource_counter(&mut counters, *asset_id, value, false)?;
        }
    }

    Ok(counters)
}

fn native_asset_id() -> Hash {
    Hash::zero()
}

/// Resource policy with basic conservation + type rules.
#[derive(Debug, Default, Clone)]
pub struct NoopResourcePolicy;

impl ResourcePolicy for NoopResourcePolicy {
    fn check_resources(
        &self,
        tx: &ComputeTx,
        inputs: &[ObjectOutput],
        _reads: &[ObjectOutput],
    ) -> ComputeResult<()> {
        let counters = aggregate_resources(tx, inputs)?;

        if !matches!(tx.command, Command::Mint) {
            for counter in counters.values() {
                if counter.output_amount > counter.input_amount {
                    return Err(ComputeError::ResourcePolicyViolation);
                }
                if counter.output_non_amount > counter.input_non_amount {
                    return Err(ComputeError::ResourcePolicyViolation);
                }
            }
        }

        let native = counters
            .get(&native_asset_id())
            .copied()
            .unwrap_or_default();
        if matches!(tx.command, Command::Mint) {
            if tx.fee != 0 {
                return Err(ComputeError::ResourcePolicyViolation);
            }
        } else {
            let required_native = native
                .output_amount
                .checked_add(tx.fee as u128)
                .ok_or(ComputeError::ResourcePolicyViolation)?;
            if native.input_amount < required_native {
                return Err(ComputeError::ResourcePolicyViolation);
            }
        }

        Ok(())
    }
}

impl<A: AuthorizationPolicy + ?Sized> AuthorizationPolicy for Arc<A> {
    fn authorize(
        &self,
        tx: &ComputeTx,
        inputs: &[ObjectOutput],
        reads: &[ObjectOutput],
    ) -> ComputeResult<()> {
        self.as_ref().authorize(tx, inputs, reads)
    }
}

impl<R: ResourcePolicy + ?Sized> ResourcePolicy for Arc<R> {
    fn check_resources(
        &self,
        tx: &ComputeTx,
        inputs: &[ObjectOutput],
        reads: &[ObjectOutput],
    ) -> ComputeResult<()> {
        self.as_ref().check_resources(tx, inputs, reads)
    }
}
