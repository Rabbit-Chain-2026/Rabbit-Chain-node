//! 自研素数域 Fp（Goldilocks）：p = 2^64 − 2^32 + 1。
//!
//! 选择理由：p−1 = 2^32 · 3 · 5 · 17 · 257 · 65537，支持 2^32 阶 NTT（STARK/FRI 需要）；
//! 64 位域元素，u64/u128 即可手写全部运算，无需任何外部数学库。
//!
//! 归约技巧：2^64 ≡ 2^32 − 1 (mod p)，因此 128 位乘积可迭代拆高 64 位
//! 乘 (2^32−1) 累加，两三次迭代后落入 < 2^65，条件减 p 收尾。

use core::fmt;
use core::ops::{Add, AddAssign, Mul, MulAssign, Neg, Sub, SubAssign};

/// 模数 p = 2^64 − 2^32 + 1。
pub const MODULUS: u64 = 0xFFFF_FFFF_0000_0001;
/// (1 << 32) − 1，用于 2^64 ≡ (1<<32) − 1 的归约。
const TWO32_MINUS_1: u128 = (1u128 << 32) - 1;

/// 域元素（正规表示 0..p）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Fp(pub u64);

impl Fp {
    pub const ZERO: Self = Self(0);
    pub const ONE: Self = Self(1);
    /// 乘法群生成元（7 是 p 的本原根）。
    pub const GENERATOR: Self = Self(7);

    pub fn new(v: u64) -> Self {
        Self(v % MODULUS)
    }

    pub fn is_zero(&self) -> bool {
        self.0 == 0
    }

    /// a·b mod p：u128 乘积 → 迭代归约（≤3 轮 + 条件减）。
    pub fn mul(a: u64, b: u64) -> u64 {
        let t = (a as u128) * (b as u128);
        // t = hi·2^64 + lo；2^64 ≡ (1<<32)−1
        let lo = t as u64;
        let hi = (t >> 64) as u64;
        let s = lo as u128 + (hi as u128) * TWO32_MINUS_1; // < 2^97
        let lo2 = s as u64;
        let hi2 = (s >> 64) as u64; // < 2^33
        let s2 = lo2 as u128 + (hi2 as u128) * TWO32_MINUS_1; // < 2^66
        let lo3 = s2 as u64;
        let hi3 = (s2 >> 64) as u64; // 0..=3
        let r = lo3 as u128 + (hi3 as u128) * TWO32_MINUS_1; // < 2^65
        let mut r = r as u64;
        if r >= MODULUS {
            r -= MODULUS;
        }
        if r >= MODULUS {
            r -= MODULUS;
        }
        r
    }

    /// 逆元：a^(p−2)（费马小定理，平方-乘）。
    pub fn inv(a: u64) -> u64 {
        debug_assert!(a % MODULUS != 0, "inverse of zero");
        // p−2 = 0xFFFFFFFF00000000FFFFFFFE... 手写 64 位平方-乘
        let base = a % MODULUS;
        let mut acc = 1u64;
        // 指数 p−2 的二进制（从高位到低位遍历 64 位）
        let exp = MODULUS - 2;
        for i in (0..64).rev() {
            acc = Self::mul(acc, acc);
            if ((exp >> i) & 1) == 1 {
                acc = Self::mul(acc, base);
            }
        }
        acc
    }

    /// 本元素逆元（域方法形式）。
    pub fn inv_self(&self) -> Self {
        Self(Self::inv(self.0))
    }

    pub fn pow(&self, exp: u64) -> Self {
        let base = self.0;
        let mut acc = 1u64;
        for i in (0..64).rev() {
            acc = Self::mul(acc, acc);
            if ((exp >> i) & 1) == 1 {
                acc = Self::mul(acc, base);
            }
        }
        Self(acc)
    }

    /// 本原 2^k 阶单位根（k ≤ 32）：g^((p−1)/2^k)。
    pub fn root_of_unity(order_log2: u32) -> Self {
        debug_assert!(order_log2 <= 32);
        let group_order = MODULUS - 1; // = 2^32 · 3 · 5 · 17 · 257 · 65537
        let mut exp = group_order;
        exp >>= order_log2;
        Self::GENERATOR.pow(exp)
    }
}

impl Add for Fp {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        let (mut r, c) = self.0.overflowing_add(rhs.0);
        if c || r >= MODULUS {
            r = r.wrapping_sub(MODULUS);
        }
        Self(r)
    }
}

impl Sub for Fp {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        if self.0 >= rhs.0 {
            Self(self.0 - rhs.0)
        } else {
            Self(MODULUS - (rhs.0 - self.0))
        }
    }
}

impl Neg for Fp {
    type Output = Self;
    fn neg(self) -> Self {
        if self.0 == 0 {
            self
        } else {
            Self(MODULUS - self.0)
        }
    }
}

impl Mul for Fp {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        Self(Self::mul(self.0, rhs.0))
    }
}

impl AddAssign for Fp {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl SubAssign for Fp {
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

impl MulAssign for Fp {
    fn mul_assign(&mut self, rhs: Self) {
        *self = *self * rhs;
    }
}

impl fmt::Display for Fp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u64> for Fp {
    fn from(v: u64) -> Self {
        Self::new(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_sub_neg_roundtrip() {
        let a = Fp::new(1_000_000_007);
        let b = Fp::new(123_456_789);
        assert_eq!((a + b) - a, b);
        assert_eq!(a + (-a), Fp::ZERO);
        assert_eq!((-a) + a, Fp::ZERO);
    }

    #[test]
    fn mul_reduces_and_matches_u128() {
        // 与大数 u128 朴素乘法对拍
        let a = 0xFFFF_FFFF_0000_00FFu64;
        let b = 0xDEAD_BEEF_CAFE_1234u64;
        let expect = ((a as u128 * b as u128) % MODULUS as u128) as u64;
        assert_eq!(Fp::mul(a, b), expect);
        // 边界：接近 p 的值
        assert_eq!(Fp::mul(MODULUS - 1, MODULUS - 1), 1);
        assert_eq!(Fp::mul(MODULUS - 1, 2), MODULUS - 2);
        assert_eq!(Fp::mul(0, 12345), 0);
    }

    #[test]
    fn inv_is_reciprocal() {
        for a in [2u64, 3, 7, 1_000_000_007, MODULUS - 1, MODULUS / 2] {
            let f = Fp::new(a);
            assert_eq!(f * f.inv_self(), Fp::ONE, "a={a}");
        }
    }

    #[test]
    fn pow_matches_repeated_mul() {
        let g = Fp::GENERATOR;
        let g7 = g * g * g * g * g * g * g;
        assert_eq!(g.pow(7), g7);
        assert_eq!(g.pow(0), Fp::ONE);
        assert_eq!(g.pow(MODULUS - 1), Fp::ONE, "费马小定理");
    }

    #[test]
    fn roots_of_unity_have_expected_order() {
        // ω^(2^k) = 1 且 ω^(2^(k−1)) ≠ 1
        for k in [1u32, 2, 4, 8, 16, 20, 24] {
            let w = Fp::root_of_unity(k);
            assert_eq!(w.pow(1u64 << k), Fp::ONE, "k={k}");
            assert_ne!(w.pow(1u64 << (k - 1)), Fp::ONE, "k={k}");
        }
    }

    #[test]
    fn field_is_prime_for_small_roots() {
        // 生成元阶 = p−1（抽样验证 7 是原根：7^((p-1)/2) ≠ 1）
        let g = Fp::GENERATOR;
        let half = g.pow((MODULUS - 1) / 2);
        assert_eq!(half * half, Fp::ONE);
        assert_ne!(half, Fp::ONE);
    }
}
