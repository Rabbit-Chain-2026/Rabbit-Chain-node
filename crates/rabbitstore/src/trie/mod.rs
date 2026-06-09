//! Merkle Patricia Trie Implementation
//!
//! RabbitChain uses a modified Merkle Patricia Trie,
//! optimized for our hybrid account model.

mod node;
mod proof;
#[allow(clippy::module_inception)]
mod trie;

pub use node::*;
pub use proof::*;
pub use trie::*;
