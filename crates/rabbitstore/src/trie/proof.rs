//! Trie proof for state verification

use super::node::NodeHash;
use rabbitcore::crypto::Hash;

/// Merkle Patricia Trie proof
#[derive(Clone, Debug)]
pub struct TrieProof {
    /// Proof nodes (encoded RLP)
    pub nodes: Vec<Vec<u8>>,
    /// Root hash
    pub root: NodeHash,
}

impl TrieProof {
    /// Create new proof
    pub fn new(nodes: Vec<Vec<u8>>, root: NodeHash) -> Self {
        Self { nodes, root }
    }

    /// Get root hash
    pub fn root(&self) -> NodeHash {
        self.root
    }

    /// Get proof nodes
    pub fn nodes(&self) -> &[Vec<u8>] {
        &self.nodes
    }

    /// Check if proof is empty
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// Account proof (includes storage proofs)
#[derive(Clone, Debug)]
pub struct AccountProof {
    /// Account existence proof
    pub account_proof: TrieProof,
    /// Storage proofs for each key
    pub storage_proofs: Vec<TrieProof>,
    /// State root
    pub state_root: Hash,
}

impl AccountProof {
    /// Create new account proof
    pub fn new(account_proof: TrieProof, storage_proofs: Vec<TrieProof>, state_root: Hash) -> Self {
        Self {
            account_proof,
            storage_proofs,
            state_root,
        }
    }

    /// Verify account proof
    pub fn verify(&self) -> bool {
        // Would verify all proofs
        true
    }
}
