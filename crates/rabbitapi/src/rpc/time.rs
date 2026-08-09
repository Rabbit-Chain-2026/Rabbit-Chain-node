//! 虚拟时钟（testkit 测试框架）：anvil 风格的时间跳跃。
//!
//! 隔离原则：
//! - `VirtualClock` 是**可注入的时间源**：offset 默认 0（= 真实墙钟），
//!   仅当 `rabbit_increaseTime`（`rabbit_increaseTime` 仅在 `testkit` feature 下编译）被调用时推进。
//! - `rabbit_increaseTime` 仅在 **`testkit` feature 构建 + `enable_time_travel` 配置开启**时存在，
//!   生产构建（默认 features）不含该方法，真实链实现不受影响。
//! - 用途：让 72h 治理投票窗口可在 e2e 中即时越过（anvil `evm_increaseTime` 的对应物）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 进程内虚拟时钟：offset 叠加在真实墙钟上。
pub struct VirtualClock {
    offset_secs: AtomicU64,
}

impl VirtualClock {
    pub fn new() -> Self {
        Self {
            offset_secs: AtomicU64::new(0),
        }
    }

    pub fn real_now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// 当前虚拟时间 = 真实时间 + offset（默认即真实时间）。
    pub fn now(&self) -> u64 {
        VirtualClock::real_now().saturating_add(self.offset_secs.load(Ordering::SeqCst))
    }

    /// 推进虚拟时间，返回推进后的虚拟时间（anvil `evm_increaseTime` 语义）。
    pub fn increase(&self, secs: u64) -> u64 {
        self.offset_secs.fetch_add(secs, Ordering::SeqCst);
        self.now()
    }

    pub fn offset_secs(&self) -> u64 {
        self.offset_secs.load(Ordering::SeqCst)
    }
}

impl Default for VirtualClock {
    fn default() -> Self {
        Self::new()
    }
}
