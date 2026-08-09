//! 多项式运算（自研，基于 NTT）：乘法、点值求值、插值。
//!
//! 系数为 `Fp`（Goldilocks 域）。NTT 支持 ≤ 2^32 长度；迭代 Cooley-Tukey。

use crate::field::Fp;

/// 多项式（系数升序：poly[i] 是 x^i 的系数）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Poly {
    pub coeffs: Vec<Fp>,
}

impl Poly {
    /// 常数多项式 0。
    pub fn zero() -> Self {
        Self { coeffs: vec![] }
    }

    /// 常数多项式 c。
    pub fn constant(c: Fp) -> Self {
        Self { coeffs: vec![c] }
    }

    /// 从系数构建并去掉高位零。
    pub fn from_coeffs(coeffs: Vec<Fp>) -> Self {
        let mut c = coeffs;
        while c.len() > 1 && c.last() == Some(&Fp::ZERO) {
            c.pop();
        }
        if c.is_empty() {
            c.push(Fp::ZERO);
        }
        Self { coeffs: c }
    }

    pub fn degree(&self) -> usize {
        self.coeffs.len().saturating_sub(1)
    }

    /// x 处求值（霍纳）。
    pub fn eval(&self, x: Fp) -> Fp {
        let mut acc = Fp::ZERO;
        for &c in self.coeffs.iter().rev() {
            acc = acc * x + c;
        }
        acc
    }

    /// 在多个点求值（朴素 O(n·m)，点数小足够）。
    pub fn eval_many(&self, xs: &[Fp]) -> Vec<Fp> {
        xs.iter().map(|&x| self.eval(x)).collect()
    }

    /// 多项式乘法（NTT），返回规范结果（无高位零已由 from_coeffs 处理）。
    pub fn mul(&self, rhs: &Self) -> Self {
        let (a, b) = (&self.coeffs, &rhs.coeffs);
        let out_len = a.len() + b.len() - 1;
        if out_len <= 1 {
            return Self::constant(a.first().copied().unwrap_or(Fp::ZERO)
                * b.first().copied().unwrap_or(Fp::ZERO));
        }
        // 选最小的 2 的幂 ≥ out_len
        let n = out_len.next_power_of_two();
        let log2 = n.trailing_zeros();
        let mut fa = a.clone();
        let mut fb = b.clone();
        fa.resize(n, Fp::ZERO);
        fb.resize(n, Fp::ZERO);
        ntt(&mut fa, log2, false);
        ntt(&mut fb, log2, false);
        for i in 0..n {
            fa[i] *= fb[i];
        }
        ntt(&mut fa, log2, true);
        fa.truncate(out_len);
        Self::from_coeffs(fa)
    }

    /// 由 n 个点 (x_i, y_i) 插值（拉格朗日，O(n²)）；n ≤ 128 足够。
    pub fn interpolate(xs: &[Fp], ys: &[Fp]) -> Self {
        assert_eq!(xs.len(), ys.len());
        let n = xs.len();
        // 预计算主多项式 L(x) = ∏(x − x_i)
        let mut master = Poly::from_coeffs(vec![Fp::ONE]);
        for &x in xs {
            master = master.mul(&Poly::from_coeffs(vec![-x, Fp::ONE]));
        }
        let mut result = Self::zero();
        for i in 0..n {
            // 第 i 个基多项式 L_i(x) = master(x) / (x − x_i)
            let basis = master.divide_by_linear(xs[i]);
            // 归一化：L_i(x_i) 应为 1
            let li_xi = basis.eval(xs[i]);
            let li = basis.scale(ys[i] * li_xi.inv_self());
            result = result + li;
        }
        result
    }

    /// 系数缩放：g(X) = Σ (c_i·s^i)·X^i（即 f(s·X)）。
    pub fn scale_var(&self, s: Fp) -> Self {
        let mut pow = Fp::ONE;
        let mut c = Vec::with_capacity(self.coeffs.len());
        for &v in &self.coeffs {
            c.push(v * pow);
            pow *= s;
        }
        Self::from_coeffs(c)
    }

    /// 多项式长除法：返回 (商, 余)。商/余规范（高位零已去）。
    pub fn div_mod(&self, divisor: &Self) -> (Self, Self) {
        let mut rem = self.coeffs.clone();
        let n = self.degree();
        let m = divisor.degree();
        if n < m || m == 0 && divisor.coeffs[0].is_zero() {
            return (Self::zero(), self.clone());
        }
        if m == 0 {
            // 除以常数
            let inv = divisor.coeffs[0].inv_self();
            return (self.scale(inv), Self::zero());
        }
        let lc = divisor.coeffs[m];
        let lc_inv = lc.inv_self();
        let mut q = vec![Fp::ZERO; n - m + 1];
        for i in (m..=n).rev() {
            if rem[i].is_zero() {
                continue;
            }
            let coeff = rem[i] * lc_inv;
            let qi = i - m;
            q[qi] = coeff;
            for j in 0..=m {
                rem[qi + j] -= coeff * divisor.coeffs[j];
            }
        }
        rem.truncate(m);
        (Self::from_coeffs(q), Self::from_coeffs(rem))
    }
}

impl Poly {
    /// 除以一次因子 (x − r)：返回商（余数应为 0 的理想情况）。
    /// 用综合除法（系数升序）。
    fn divide_by_linear(&self, r: Fp) -> Self {
        let n = self.coeffs.len();
        if n == 0 {
            return Self::zero();
        }
        let mut q = vec![Fp::ZERO; n - 1];
        let mut carry = Fp::ZERO;
        for i in (0..n - 1).rev() {
            q[i] = self.coeffs[i + 1] + carry;
            carry = q[i] * r;
        }
        Self::from_coeffs(q)
    }

    /// 逐系数乘标量。
    pub(crate) fn scale(&self, s: Fp) -> Self {
        Self::from_coeffs(self.coeffs.iter().map(|&c| c * s).collect())
    }
}

impl core::ops::Add for Poly {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        let n = self.coeffs.len().max(rhs.coeffs.len());
        let mut c = vec![Fp::ZERO; n];
        for (i, &v) in self.coeffs.iter().enumerate() {
            c[i] += v;
        }
        for (i, &v) in rhs.coeffs.iter().enumerate() {
            c[i] += v;
        }
        Self::from_coeffs(c)
    }
}

impl core::ops::Sub for Poly {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        self + (-rhs)
    }
}

impl core::ops::Neg for Poly {
    type Output = Self;
    fn neg(self) -> Self {
        Self::from_coeffs(self.coeffs.iter().map(|&c| -c).collect())
    }
}

/// 迭代 NTT。`inv=true` 时做逆变换（用逆根 + 归 n）。
/// 输入/输出长度必须是 2^log2 且 log2 ≤ 32。
pub fn ntt(a: &mut [Fp], log2: u32, inv: bool) {
    let n = a.len();
    assert_eq!(n, 1 << log2);
    // 位反转置换
    for i in 0..n {
        let j = bit_reverse(i as u32, log2) as usize;
        if i < j {
            a.swap(i, j);
        }
    }
    let mut len = 2usize;
    while len <= n {
        let half = len / 2;
        // 该层旋转因子：len 阶本原单位根
        let w_len = Fp::root_of_unity((len as u32).trailing_zeros());
        let w_len = if inv { w_len.inv_self() } else { w_len };
        for start in (0..n).step_by(len) {
            let mut w = Fp::ONE;
            for k in 0..half {
                let even = a[start + k];
                let odd = a[start + k + half] * w;
                a[start + k] = even + odd;
                a[start + k + half] = even - odd;
                w *= w_len;
            }
        }
        len *= 2;
    }
    if inv {
        let inv_n = Fp::new(n as u64).inv_self();
        for v in a.iter_mut() {
            *v *= inv_n;
        }
    }
}

/// 位反转（log2 位）。
fn bit_reverse(mut x: u32, log2: u32) -> u32 {
    let mut r = 0u32;
    for _ in 0..log2 {
        r = (r << 1) | (x & 1);
        x >>= 1;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(x: u64) -> Fp {
        Fp::new(x)
    }

    /// 测试用：负值 → p − |x|。
    fn fp_i(x: i64) -> Fp {
        if x >= 0 {
            Fp::new(x as u64)
        } else {
            Fp::new(crate::field::MODULUS - x.unsigned_abs() as u64)
        }
    }

    #[test]
    fn ntt_roundtrip() {
        for n in [1usize, 2, 4, 8, 16, 64] {
            let log2 = n.trailing_zeros();
            let mut a: Vec<Fp> = (0..n).map(|i| fp((i as u64 * 7 + 3) % 100_000)).collect();
            let orig = a.clone();
            ntt(&mut a, log2, false);
            ntt(&mut a, log2, true);
            assert_eq!(a, orig, "ntt roundtrip n={n}");
        }
    }

    #[test]
    fn ntt_convolves_like_naive() {
        let a = vec![fp(1), fp(2), fp(3)];
        let b = vec![fp(4), fp(5)];
        let pa = Poly::from_coeffs(a.clone());
        let pb = Poly::from_coeffs(b.clone());
        let got = pa.mul(&pb).coeffs;
        // 朴素卷积
        let mut expect = vec![Fp::ZERO; a.len() + b.len() - 1];
        for (i, &x) in a.iter().enumerate() {
            for (j, &y) in b.iter().enumerate() {
                expect[i + j] += x * y;
            }
        }
        assert_eq!(got, expect);
    }

    #[test]
    fn eval_matches_horner_reference() {
        // p(x) = 3x^2 + 2x + 1
        let p = Poly::from_coeffs(vec![fp(1), fp(2), fp(3)]);
        for x in [0u64, 1, 2, 7, 12345] {
            let xf = fp(x);
            let expect = fp(3 * x * x + 2 * x + 1);
            assert_eq!(p.eval(xf), expect, "x={x}");
        }
    }

    #[test]
    fn interpolate_recovers_polynomial() {
        let p = Poly::from_coeffs(vec![fp(3), fp(1), fp(4)]); // 4x^2 + x + 3
        let xs: Vec<Fp> = (0..4).map(|i| fp(i + 1)).collect();
        let ys = p.eval_many(&xs);
        let recovered = Poly::interpolate(&xs, &ys);
        // 在更多点上比对
        let check: Vec<Fp> = (0..7).map(|i| fp(i * 3 + 2)).collect();
        assert_eq!(recovered.eval_many(&check), p.eval_many(&check));
    }

    #[test]
    fn multiply_and_divide_linear() {
        // (x−2)(x−3) = x^2 −5x +6
        let m = Poly::from_coeffs(vec![fp_i(6), fp_i(-5), fp_i(1)]);
        let q = m.divide_by_linear(fp(2));
        // 商应为 (x−3) = [-3, 1]
        assert_eq!(q, Poly::from_coeffs(vec![fp_i(-3), fp_i(1)]));
    }
}
