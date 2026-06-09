//! Account management module
//!
//! RabbitChain uses an account model combining:
//! - Balance-based accounts
//! - UTXO data structures for parallel execution
//! - Multi-party account controls

#[allow(clippy::module_inception)]
mod account;
mod manager;
mod utxo;

pub use account::*;
pub use manager::*;
pub use utxo::*;
