//! 自研零知识证明（zk-STARK / FRI）核心库。
//!
//! 不依赖任何外部证明框架（无 sp1/arkworks 等）：有限域、NTT、多项式、
//! Merkle 承诺与 FRI 协议全部在本 crate 内手写实现。
//!
//! 模块：
//! - `field`：Goldilocks 素数域 Fp (p = 2^64 − 2^32 + 1)
//! - `poly`：多项式（NTT 乘法、求值、插值）
//! - `merkle`：Merkle 承诺（keccak）
//! - `fri`：FRI 低阶测试与打开（进行中）
//! - `stark`：AIR 证明/验证协议（进行中）

pub mod field;
pub mod fri;
pub mod merkle;
pub mod poly;
pub mod enhance;
pub mod stark;
