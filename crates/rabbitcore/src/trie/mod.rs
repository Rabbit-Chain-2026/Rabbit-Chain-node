//! Merkle Patricia Trie — core consensus types (in-memory).
//!
//! Moved from `rabbitstore` so consensus code in rabbitcore can compute the
//! unspent-output state root deterministically. Persistent/cached DB variants
//! live in rabbitstore on top of these types.

pub mod node;
pub mod proof;
#[allow(clippy::module_inception)]
pub mod trie;

pub use node::*;
pub use proof::*;
pub use trie::*;

/// Empty trie root (nil path); non-zero to avoid ambiguity with a real root.
pub use node::empty_trie_root;
