//! Error types for UTXO Compute v1.1.

use crate::crypto::Hash;
use thiserror::Error;

/// Result alias for compute module.
pub type ComputeResult<T> = Result<T, ComputeError>;

/// Unified error type for object-centric execution.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ComputeError {
    /// Object is not found in the object store.
    #[error("Object output not found: {0}")]
    ObjectNotFound(Hash),

    /// Input/output domain mismatch.
    #[error("Domain mismatch: expected {expected}, got {actual}")]
    DomainMismatch { expected: u32, actual: u32 },

    /// Execution domain is not registered in registry.
    #[error("Domain is not registered: {0}")]
    DomainNotRegistered(u32),

    /// Domain is registered but currently not accepting public transactions.
    #[error("Domain is not public: {0}")]
    DomainNotPublic(u32),

    /// Input object kind does not satisfy command requirements.
    #[error("Invalid object kind for command")]
    InvalidObjectKind,

    /// Read-set validation failed.
    #[error("Read set validation failed")]
    ReadSetValidationFailed,

    /// Referenced read-set version does not match expectation.
    #[error("Read version mismatch: expected {expected}, got {actual}")]
    ReadVersionMismatch { expected: u64, actual: u64 },

    /// Ownership check failed.
    #[error("Ownership check failed")]
    OwnershipCheckFailed,

    /// Authorization check failed.
    #[error("Authorization denied")]
    AuthorizationDenied,

    /// Signature cannot be recovered/decoded for authorization.
    #[error("Invalid signature for authorization")]
    InvalidSignature,

    /// Operation id does not match canonical signed body hash.
    #[error("Operation id does not match signed payload")]
    TxIdMismatch,

    /// Signature is valid but does not match owner requirement.
    #[error("Signature does not match owner")]
    SignatureOwnerMismatch,

    /// Signature scheme is not supported in current context.
    #[error("Unsupported signature scheme")]
    UnsupportedSignatureScheme,

    /// Resource accounting policy check failed.
    #[error("Resource policy violation")]
    ResourcePolicyViolation,

    /// Duplicate output ID insertion.
    #[error("Duplicate output id")]
    DuplicateOutputId,

    /// Output proposal references invalid predecessor.
    #[error("Invalid output predecessor")]
    InvalidPredecessor,

    /// Output proposal has invalid version progression.
    #[error("Invalid version progression")]
    InvalidVersionProgression,

    /// General invalid operation condition.
    #[error("Invalid operation: {0}")]
    InvalidOperation(String),
}
