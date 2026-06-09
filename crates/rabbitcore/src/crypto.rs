//! Cryptographic primitives for RabbitChain
//!
//! This module provides:
//! - Hash functions (Keccak256, SHA256, Blake3)
//! - Public/Private key pairs (ed25519)
//! - Digital signatures (ed25519)
//! - Address derivation

use ed25519_dalek::{
    Signature as DalekEd25519Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey,
};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};
use std::fmt;
use thiserror::Error;

/// Crypto error types
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum CryptoError {
    /// Invalid signature
    #[error("Invalid signature")]
    InvalidSignature,
    /// Invalid private key
    #[error("Invalid private key")]
    InvalidPrivateKey,
    /// Invalid public key
    #[error("Invalid public key")]
    InvalidPublicKey,
    /// Signature verification failed
    #[error("Signature verification failed")]
    VerificationFailed,
    /// Key derivation failed
    #[error("Key derivation failed")]
    KeyDerivationFailed,
}

/// 256-bit hash
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Hash([u8; 32]);

impl Hash {
    /// Create a new hash from bytes
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Create a new hash from a slice
    pub fn from_slice(slice: &[u8]) -> Result<Self, CryptoError> {
        if slice.len() != 32 {
            return Err(CryptoError::InvalidPublicKey);
        }
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(slice);
        Ok(Self(bytes))
    }

    /// Get the hash as bytes
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Convert to hex string
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Create from hex string
    pub fn from_hex(s: &str) -> Result<Self, CryptoError> {
        let bytes = hex::decode(s.strip_prefix("0x").unwrap_or(s))
            .map_err(|_| CryptoError::InvalidPublicKey)?;
        Self::from_slice(&bytes)
    }

    /// All-zero hash
    pub fn zero() -> Self {
        Self::default()
    }

    /// Check if hash is zero
    pub fn is_zero(&self) -> bool {
        self.0.iter().all(|&b| b == 0)
    }

    /// Count leading zeros
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
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash({})", self.to_hex())
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", &self.to_hex()[..16])
    }
}

impl PartialOrd for Hash {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Hash {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

/// 160-bit address
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Address([u8; 20]);

impl Address {
    /// Create a new address from bytes
    pub fn from_bytes(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    /// Create a new address from a slice
    pub fn from_slice(slice: &[u8]) -> Result<Self, CryptoError> {
        if slice.len() != 20 {
            return Err(CryptoError::InvalidPublicKey);
        }
        let mut bytes = [0u8; 20];
        bytes.copy_from_slice(slice);
        Ok(Self(bytes))
    }

    /// Get the address as bytes
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Convert to hex string with checksum
    pub fn to_checksum_hex(&self) -> String {
        let hex = hex::encode(self.0);
        let hash = keccak256(hex.as_bytes());

        let mut result = String::with_capacity(42);
        result.push_str("0x");

        for (i, c) in hex.chars().enumerate() {
            if c.is_ascii_digit() {
                result.push(c);
            } else if ((hash[i / 2] >> (4 * (1 - i % 2))) & 0x8) != 0 {
                result.push(c.to_ascii_uppercase());
            } else {
                result.push(c.to_ascii_lowercase());
            }
        }

        result
    }

    /// Convert to simple hex string
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Create from hex string
    pub fn from_hex(s: &str) -> Result<Self, CryptoError> {
        let bytes = hex::decode(s.strip_prefix("0x").unwrap_or(s))
            .map_err(|_| CryptoError::InvalidPublicKey)?;
        Self::from_slice(&bytes)
    }

    /// Create address from public key
    pub fn from_public_key(pubkey: &Ed25519PublicKey) -> Self {
        let hash = keccak256(&pubkey.0);
        let mut address = [0u8; 20];
        address.copy_from_slice(&hash[12..]);
        Self(address)
    }

    /// All-zero address
    pub fn zero() -> Self {
        Self::default()
    }

    /// Check if address is zero
    pub fn is_zero(&self) -> bool {
        self.0.iter().all(|&b| b == 0)
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Address({})", self.to_checksum_hex())
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_checksum_hex())
    }
}

/// 256-bit public key (ed25519)
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ed25519PublicKey([u8; 32]);

impl Ed25519PublicKey {
    /// Create from bytes
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self, CryptoError> {
        Ok(Self(bytes))
    }

    pub(crate) fn placeholder() -> Self {
        Self([0u8; 32])
    }

    /// Get the public key as bytes
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Convert to hex string
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Debug for Ed25519PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ed25519PublicKey({})", &self.to_hex()[..16])
    }
}

/// 256-bit private key (ed25519 seed)
pub struct Ed25519PrivateKey {
    bytes: [u8; 32],
}

impl Clone for Ed25519PrivateKey {
    fn clone(&self) -> Self {
        Self { bytes: self.bytes }
    }
}

impl PartialEq for Ed25519PrivateKey {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl Eq for Ed25519PrivateKey {}

impl Ed25519PrivateKey {
    /// Generate a new random private key
    pub fn random() -> Self {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self { bytes }
    }

    /// Create from bytes
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self, CryptoError> {
        Ok(Self { bytes })
    }

    /// Get the private key as bytes
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Get the corresponding public key
    pub fn public_key(&self) -> Ed25519PublicKey {
        let signing_key = SigningKey::from_bytes(&self.bytes);
        Ed25519PublicKey(signing_key.verifying_key().to_bytes())
    }

    /// Sign a message
    pub fn sign(&self, message: &[u8]) -> Ed25519Signature {
        let signing_key = SigningKey::from_bytes(&self.bytes);
        let signature = signing_key.sign(message).to_bytes();
        Ed25519Signature { bytes: signature }
    }

    /// Convert to hex string
    pub fn to_hex(&self) -> String {
        hex::encode(self.bytes)
    }
}

impl fmt::Debug for Ed25519PrivateKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ed25519PrivateKey(***)")
    }
}

impl Drop for Ed25519PrivateKey {
    fn drop(&mut self) {
        self.bytes.iter_mut().for_each(|b| *b = 0);
    }
}

/// ed25519 signature
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Ed25519Signature {
    bytes: [u8; 64],
}

impl serde::Serialize for Ed25519Signature {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeTuple;
        let mut seq = serializer.serialize_tuple(64)?;
        for byte in &self.bytes {
            seq.serialize_element(byte)?;
        }
        seq.end()
    }
}

impl<'de> serde::Deserialize<'de> for Ed25519Signature {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct SignatureVisitor;

        impl<'de> serde::de::Visitor<'de> for SignatureVisitor {
            type Value = Ed25519Signature;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a 64-byte signature")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Ed25519Signature, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut bytes = [0u8; 64];
                for (i, slot) in bytes.iter_mut().enumerate() {
                    *slot = seq
                        .next_element()?
                        .ok_or_else(|| serde::de::Error::invalid_length(i, &self))?;
                }
                Ok(Ed25519Signature { bytes })
            }
        }

        deserializer.deserialize_tuple(64, SignatureVisitor)
    }
}

impl Ed25519Signature {
    /// Create a new signature
    pub fn new(r: [u8; 32], s: [u8; 32], _v: u8) -> Self {
        let mut bytes = [0u8; 64];
        bytes[..32].copy_from_slice(&r);
        bytes[32..64].copy_from_slice(&s);
        Self { bytes }
    }

    /// Create from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CryptoError> {
        if bytes.len() != 64 && bytes.len() != 65 {
            return Err(CryptoError::InvalidSignature);
        }
        let mut compact = [0u8; 64];
        compact.copy_from_slice(&bytes[..64]);
        Ok(Self { bytes: compact })
    }

    /// Get the signature as bytes
    pub fn as_bytes(&self) -> [u8; 64] {
        self.bytes
    }

    /// Get r component
    pub fn r(&self) -> [u8; 32] {
        self.bytes[..32]
            .try_into()
            .expect("signature prefix length")
    }

    /// Get s component
    pub fn s(&self) -> [u8; 32] {
        self.bytes[32..64]
            .try_into()
            .expect("signature suffix length")
    }

    /// Get v placeholder retained for legacy callers
    pub fn v(&self) -> u8 {
        0
    }

    /// Verify a signature
    pub fn verify(
        &self,
        message: &[u8],
        public_key: &Ed25519PublicKey,
    ) -> Result<bool, CryptoError> {
        let verifying_key =
            VerifyingKey::from_bytes(&public_key.0).map_err(|_| CryptoError::InvalidPublicKey)?;
        let signature = DalekEd25519Signature::from_slice(&self.bytes)
            .map_err(|_| CryptoError::InvalidSignature)?;
        verifying_key
            .verify(message, &signature)
            .map(|_| true)
            .map_err(|_| CryptoError::VerificationFailed)
    }

    /// Recovering ed25519 public keys from signatures is unsupported.
    pub fn recover(&self, message: &[u8]) -> Result<Ed25519PublicKey, CryptoError> {
        let _ = message;
        Err(CryptoError::KeyDerivationFailed)
    }

    /// Convert to hex string
    pub fn to_hex(&self) -> String {
        hex::encode(self.as_bytes())
    }
}

impl fmt::Debug for Ed25519Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ed25519Signature({})", &self.to_hex()[..16])
    }
}

// ============ Hash Functions ============

/// Compute Keccak256 hash
pub fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Compute SHA256 hash
pub fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    use sha2::Sha256;

    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Compute Blake3 hash
pub fn blake3_hash(data: &[u8]) -> [u8; 32] {
    let hash = blake3::hash(data);
    hash.into()
}

/// Compute RIPEMD160 hash
pub fn ripemd160(data: &[u8]) -> [u8; 20] {
    use ripemd::Ripemd160;
    use sha2::Digest;

    let mut hasher = Ripemd160::new();
    hasher.update(data);
    hasher.finalize().into()
}

// ============ Tests ============

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keccak256() {
        let data = b"hello";
        let hash = keccak256(data);

        // Known Keccak256 hash for "hello"
        let expected =
            hex::decode("1c8aff950685c2ed4bc3174f3472287b56d9517b9c948127319a09a7a36deac8")
                .unwrap();
        assert_eq!(&hash, expected.as_slice());
    }

    #[test]
    fn test_signature_roundtrip() {
        let private_key = Ed25519PrivateKey::random();
        let public_key = private_key.public_key();

        let message = b"test message";
        let signature = private_key.sign(message);

        // Verify signature
        let result = signature.verify(message, &public_key);
        assert!(result.is_ok());
        assert!(result.unwrap());

        assert!(signature.recover(message).is_err());
    }

    #[test]
    fn test_address_from_public_key() {
        let private_key = Ed25519PrivateKey::random();
        let public_key = private_key.public_key();
        let address = Address::from_public_key(&public_key);

        assert!(!address.is_zero());
    }

    #[test]
    fn test_hash_hex_roundtrip() {
        let hash = Hash::from_bytes([1u8; 32]);
        let hex = hash.to_hex();
        let recovered = Hash::from_hex(&hex).unwrap();

        assert_eq!(hash, recovered);
    }

    #[test]
    fn test_address_hex_roundtrip() {
        let address = Address::from_bytes([2u8; 20]);
        let hex = address.to_hex();
        let recovered = Address::from_hex(&hex).unwrap();

        assert_eq!(address, recovered);
    }
}
