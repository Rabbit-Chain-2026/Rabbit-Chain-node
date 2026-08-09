//! M3：强化 roll 的 AIR 电路（自研 STARK）。
//!
//! 电路证明：存在秘密 64 位 seed s，使
//!   x = xorshift64³(s)（三道移位异或），且 x mod 1000 = r（公开）
//! 其中 r < 1000 与 r < threshold 由验证方在公开域检查。
//!
//! 迹布局（N = 8 行 × 64 列；列 j = 位 j，行 = 计算步骤）：
//!   row 0: seed 的位
//!   row 1: x1 = seed ⊕ (seed<<13) 的位
//!   row 2: x2 = x1 ⊕ (x1>>7) 的位
//!   row 3: x3 = x2 ⊕ (x2<<17) 的位（最终 x）
//!   row 4: r 的位（col 0..9，公开）+ col 10..63 为 0
//!   row 5: q = (x−r)/1000（col 0，秘密）
//!   row 6..7: 全 0
//!
//! 约束：
//!   1. 位性：col 0..63 在 row 0..3 上，col 0..9 在 row 4 上：b²−b = 0
//!   2. 三道 xorshift 步骤（分别在 row 0/1/2 上）：T_j(ωX) − [T_j ⊕ T_src] = 0
//!   3. 取模（row 5 上）：Σ2^j·T_j(ω⁻²X) − 1000·T_0(X) − Σ2^i·T_i(ω⁻¹X) = 0
//!   4. 零值（row 4 col 10..63；row 6..7 全部列）：T = 0
//!
//! 合并商 Q = Σ α_c·(C_c/Z_c)；组合迹 τ = Σ β_j·T_j。
//! 边界：τ 在 row 4（r 位全公开）打开。

use crate::field::Fp;
use crate::fri::{prove, verify, FriProof, Transcript};
use crate::poly::Poly;
use sha3::{Digest, Keccak256};

/// xorshift64 一步。
fn xorshift(x: u64) -> u64 {
    let mut x = x;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

/// 真实计算（供测试/边界对拍）：返回 (x3, roll)。
pub fn enhance_roll(seed: u64) -> (u64, u64) {
    let x = xorshift(xorshift(xorshift(seed)));
    (x, x % 1000)
}

fn sha3_256(msg: &[u8]) -> [u8; 32] {
    let mut h = Keccak256::new();
    h.update(msg);
    h.finalize().into()
}

fn domain(k: u32) -> Vec<Fp> {
    let w = Fp::root_of_unity(k);
    let mut pts = Vec::with_capacity(1 << k);
    let mut x = Fp::ONE;
    for _ in 0..(1usize << k) {
        pts.push(x);
        x *= w;
    }
    pts
}

/// 64 位字 → 位数组（LSB 在 index 0）。
fn bits_of(v: u64) -> [u8; 64] {
    let mut b = [0u8; 64];
    for i in 0..64 {
        b[i] = ((v >> i) & 1) as u8;
    }
    b
}

/// 增强 STARK 证明。
#[derive(Clone, Debug)]
pub struct EnhanceProof {
    pub tau_fri: FriProof,
    pub quotient_fri: FriProof,
    pub challenge: Fp,
    /// 约束检查打开：每列在 {χ, ωχ, ω⁻¹χ, ω⁻²χ} 的求值（列主序）。
    pub col_opens: Vec<[Fp; 4]>,
    pub c_at_chi: Vec<Fp>, // 每个约束 C_c(χ)
    pub q_at_chi: Fp,      // Q(χ)
}

pub const N_LOG2: u32 = 4; // 16 行迹
pub const FRI_K: u32 = 5; // FRI 域 32 点（覆盖商 deg < 32）（覆盖商 deg < 16）

/// 由承诺（τ/Q 根 + 系数）推导挑战 χ。
fn derive_challenge(tau_root: &[u8; 32], q_root: &[u8; 32], q_coeffs: &[Fp]) -> Fp {
    let mut buf = vec![];
    buf.extend_from_slice(tau_root);
    buf.extend_from_slice(q_root);
    for c in q_coeffs {
        buf.extend_from_slice(&c.0.to_be_bytes());
    }
    let h = sha3_256(&buf);
    Fp::new(u64::from_be_bytes(h[..8].try_into().unwrap()))
}

/// 单个 xorshift 子步：kind 0=<<13, 1=>>7, 2=<<17。
fn xorshift_sub(x: u64, kind: u8) -> u64 {
    match kind {
        0 => x ^ (x << 13),
        1 => x ^ (x >> 7),
        _ => x ^ (x << 17),
    }
}

/// 9 个 xorshift 子步（A/B/C × 3 轮）作用于 seed → x = xorshift³(seed)。
/// 每步的行号与类型：STEPS[i] = (word 行, kind)。
pub const STEPS: [(usize, u8); 9] = [
    (0, 0), (1, 1), (2, 2),
    (3, 0), (4, 1), (5, 2),
    (6, 0), (7, 1), (8, 2),
];

/// 迹行生成（验证用参考实现）。
/// row 0..8: 9 子步后的字位（row 0 = seed；row 9 = x = xorshift³(seed)）
/// row 10: r 的位（col 0..9，公开）+ col 10..63 = 0
/// row 14: q = (x−r)/1000（col 0，秘密）
/// 其余行 0
pub fn build_trace(seed: u64, roll: u64) -> Vec<Vec<Fp>> {
    let mut words = vec![seed];
    let mut cur = seed;
    for &(_, kind) in &STEPS {
        cur = xorshift_sub(cur, kind);
        words.push(cur);
    }
    let mut trace = vec![vec![Fp::ZERO; 64]; 16];
    for (row, word) in words.iter().enumerate() {
        let b = bits_of(*word);
        for j in 0..64 {
            trace[row][j] = Fp::new(b[j] as u64);
        }
    }
    let r = bits_of(roll);
    for i in 0..10 {
        trace[10][i] = Fp::new(r[i] as u64);
    }
    let x = *words.last().unwrap();
    let q = x.saturating_sub(roll) / 1000;
    trace[14][0] = Fp::new(q);
    trace
}

/// 约束 (rows, C(X) 构造)。返回 (rows, C(X))。
pub fn build_constraints(
    cols: &[Poly; 64],
    omega: Fp,
) -> Vec<(Vec<usize>, Poly)> {
    let zero = Poly::zero();
    let mut csts: Vec<(Vec<usize>, Poly)> = vec![];
    // 1) 位性：col 0..63 于 row 0..9；col 0..9 于 row 10
    let word_rows: Vec<usize> = (0..10).collect();
    for j in 0..64 {
        csts.push((word_rows.clone(), cols[j].mul(&cols[j]) - cols[j].clone()));
    }
    for j in 0..10 {
        csts.push((vec![10], cols[j].mul(&cols[j]) - cols[j].clone()));
    }
    // 2) 9 个 xorshift 子步（row 0..8）
    for &(row, kind) in &STEPS {
        for j in 0..64 {
            let src = match kind {
                0 => { if j >= 13 { cols[j - 13].clone() } else { zero.clone() } }
                1 => { if j + 7 < 64 { cols[j + 7].clone() } else { zero.clone() } }
                _ => { if j >= 17 { cols[j - 17].clone() } else { zero.clone() } }
            };
            let xor = cols[j].clone() + src.clone()
                - cols[j].clone().mul(&src).scale(Fp::new(2));
            let c = cols[j].scale_var(omega) - xor;
            csts.push((vec![row], c));
        }
    }
    // 3) 取模（row 14）：Σ2^j T_j(ω⁻⁵X) − 1000·T_0(X) − Σ2^i T_i(ω⁻⁴X)
    let omega_inv = omega.inv_self();
    let mut x_sum = Poly::zero();
    let mut pow2 = Fp::ONE;
    for j in 0..64 {
        x_sum = x_sum + cols[j].scale_var(omega_inv * omega_inv * omega_inv * omega_inv * omega_inv).scale(pow2);
        pow2 *= Fp::new(2);
    }
    let mut r_sum = Poly::zero();
    let mut pow2 = Fp::ONE;
    for i in 0..10 {
        r_sum = r_sum + cols[i].scale_var(omega_inv * omega_inv * omega_inv * omega_inv).scale(pow2);
        pow2 *= Fp::new(2);
    }
    let c_mod = x_sum - cols[0].clone().scale(Fp::new(1000)) - r_sum;
    csts.push((vec![14], c_mod));
    // 4) 零值：row 10 col 10..63；row 11..13 与 15 全部列；row 14 col 1..63（col 0 = q）
    for j in 10..64 {
        csts.push((vec![10], cols[j].clone()));
    }
    for row in [11usize, 12, 13, 15] {
        for j in 0..64 {
            csts.push((vec![row], cols[j].clone()));
        }
    }
    for j in 1..64 {
        csts.push((vec![14], cols[j].clone()));
    }
    csts
}

/// 行集合的消去多项式 Z(X) = ∏_{m∈rows}(X − ω^m)。
fn vanishing(rows: &[usize], omega: Fp) -> Poly {
    let mut z = Poly::from_coeffs(vec![Fp::ONE]);
    for &m in rows {
        let wm = omega.pow(m as u64);
        z = z.mul(&Poly::from_coeffs(vec![-wm, Fp::ONE]));
    }
    z
}

/// 证明：知道 seed 使 roll = xorshift64³(seed) mod 1000。
pub fn prove_enhance(seed: u64, roll: u64, num_queries: usize) -> EnhanceProof {
    assert_eq!(enhance_roll(seed).1, roll, "seed/roll 必须一致");
    let omega = Fp::root_of_unity(N_LOG2);
    let omega_inv = omega.inv_self();
    let trace = build_trace(seed, roll);
    // 列多项式
    let d = domain(N_LOG2);
    let mut cols: [Poly; 64] = std::array::from_fn(|_| Poly::zero());
    for j in 0..64 {
        let vals: Vec<Fp> = (0..16).map(|r| trace[r][j]).collect();
        cols[j] = Poly::interpolate(&d, &vals);
    }
    // 约束与商
    let csts = build_constraints(&cols, omega);
    let mut quotients = vec![];
    for (rows, c) in &csts {
        let z = vanishing(rows, omega);
        let (q, rem) = c.div_mod(&z);
        debug_assert!(
            rem.degree() == 0 && rem.coeffs[0].is_zero(),
            "constraint must divide; rem={rem:?}"
        );
        quotients.push(q);
    }
    // 随机组合系数（菲亚特-沙米尔，域分隔）
    let mut tr = Transcript::new();
    let betas: Vec<Fp> = (0..64).map(|_| tr.squeeze_fp()).collect();
    let alphas: Vec<Fp> = (0..csts.len()).map(|_| tr.squeeze_fp()).collect();
    let mut tau = Poly::zero();
    for (j, &b) in betas.iter().enumerate() {
        tau = tau + cols[j].scale(b);
    }
    let mut q_combined = Poly::zero();
    for (c, &a) in quotients.iter().zip(alphas.iter()) {
        q_combined = q_combined + c.scale(a);
    }
    // FRI（域 k = FRI_K）；FRI 内部抄本与 STARK 挑战抄本分离，双方同序
    let boundary_pt = omega.pow(10); // row 10（r 位公开行）
    let tau_fri = prove(&tau, FRI_K, num_queries, Transcript::new(), Some(boundary_pt));
    let quotient_fri = prove(&q_combined, FRI_K, num_queries, Transcript::new(), None);
    // 约束检查挑战与打开：{χ, ωχ, ω⁻⁴χ, ω⁻⁵χ}
    let chi = derive_challenge(&tau_fri.roots[0], &quotient_fri.roots[0], &quotient_fri.final_coeffs);
    let pts = [chi, omega * chi, omega_inv * omega_inv * omega_inv * omega_inv * chi,
               omega_inv * omega_inv * omega_inv * omega_inv * omega_inv * chi];
    let mut col_opens = vec![[Fp::ZERO; 4]; 64];
    for j in 0..64 {
        for (i, &p) in pts.iter().enumerate() {
            col_opens[j][i] = cols[j].eval(p);
        }
    }
    let mut c_at_chi = vec![];
    for (_rows, c) in &csts {
        c_at_chi.push(c.eval(chi));
    }
    EnhanceProof {
        tau_fri,
        quotient_fri,
        challenge: chi,
        col_opens,
        c_at_chi,
        q_at_chi: q_combined.eval(chi),
    }
}

/// 验证：roll、queries 公开。返回 Err(原因)。
pub fn verify_enhance(proof: &EnhanceProof, roll: u64, num_queries: usize) -> Result<(), String> {
    let omega = Fp::root_of_unity(N_LOG2);
    let boundary_pt = omega.pow(10);
    // 边界期望值：τ(ω^10) = Σ β_j·public_j；r 位公开，其余 0
    let mut tr = Transcript::new();
    let betas: Vec<Fp> = (0..64).map(|_| tr.squeeze_fp()).collect();
    let rbits = bits_of(roll);
    let mut expected = Fp::ZERO;
    for j in 0..10 {
        expected += betas[j] * Fp::new(rbits[j] as u64);
    }
    // 约束数 = 64(位性词 0..9) + 10(位性r) + 9×64(步骤) + 1(取模) + 54 + 63 + 4×64(零值)
    let num_csts = 64 + 10 + 9 * 64 + 1 + 54 + (64 - 1) + 4 * 64;
    let alphas: Vec<Fp> = (0..num_csts).map(|_| tr.squeeze_fp()).collect();
    verify(
        &proof.tau_fri,
        FRI_K,
        num_queries,
        Transcript::new(),
        Some((boundary_pt, expected)),
    )?;
    verify(&proof.quotient_fri, FRI_K, num_queries, Transcript::new(), None)?;
    // 约束检查：Σ α_c·C_c(χ)/Z_c(χ) == Q(χ)
    let chi = proof.challenge;
    let t0 = |idx: usize| proof.col_opens[idx][0]; // χ
    let t1 = |idx: usize| proof.col_opens[idx][1]; // ωχ
    let t2 = |idx: usize| proof.col_opens[idx][2]; // ω⁻⁴χ（r 行）
    let t3 = |idx: usize| proof.col_opens[idx][3]; // ω⁻⁵χ（x 行）
    let xor2 = |a: Fp, b: Fp| a + b - a * b * Fp::new(2);
    let mut rhs = Fp::ZERO;
    let mut ci = 0usize;
    let zc = |rows: &[usize]| -> Fp {
        let mut z = Fp::ONE;
        for &m in rows {
            z *= chi - omega.pow(m as u64);
        }
        z
    };
    // 位性词（rows 0..9）
    for j in 0..64 {
        let c = t0(j) * t0(j) - t0(j);
        rhs += alphas[ci] * c * zc(&[0,1,2,3,4,5,6,7,8,9]).inv_self();
        ci += 1;
    }
    // 位性 r（row 10）
    for j in 0..10 {
        let c = t0(j) * t0(j) - t0(j);
        rhs += alphas[ci] * c * zc(&[10]).inv_self();
        ci += 1;
    }
    // 9 个 xorshift 子步（row 0..8）
    for &(row, kind) in &STEPS {
        for j in 0..64 {
            let src = match kind {
                0 => if j >= 13 { t0(j - 13) } else { Fp::ZERO },
                1 => if j + 7 < 64 { t0(j + 7) } else { Fp::ZERO },
                _ => if j >= 17 { t0(j - 17) } else { Fp::ZERO },
            };
            let c = t1(j) - xor2(t0(j), src);
            rhs += alphas[ci] * c * zc(&[row]).inv_self();
            ci += 1;
        }
    }
    // 取模（row 14）：Σ2^j·x_j − 1000·q − Σ2^i·r_i，x 在 ω⁻⁵χ（row 9），r 在 ω⁻⁴χ（row 10）
    {
        let mut x_sum = Fp::ZERO;
        let mut p2 = Fp::ONE;
        for j in 0..64 {
            x_sum += t3(j) * p2;
            p2 *= Fp::new(2);
        }
        let mut r_sum = Fp::ZERO;
        let mut p2 = Fp::ONE;
        for i in 0..10 {
            r_sum += t2(i) * p2;
            p2 *= Fp::new(2);
        }
        let c = x_sum - t0(0) * Fp::new(1000) - r_sum;
        rhs += alphas[ci] * c * zc(&[14]).inv_self();
        ci += 1;
    }
    // 零值：row 10 col 10..63；row 11..13/15 全部列；row 14 col 1..63
    for j in 10..64 {
        let c = t0(j);
        rhs += alphas[ci] * c * zc(&[10]).inv_self();
        ci += 1;
    }
    for row in [11usize, 12, 13, 15] {
        for j in 0..64 {
            let c = t0(j);
            rhs += alphas[ci] * c * zc(&[row]).inv_self();
            ci += 1;
        }
    }
    for j in 1..64 {
        let c = t0(j);
        rhs += alphas[ci] * c * zc(&[14]).inv_self();
        ci += 1;
    }
    if rhs != proof.q_at_chi {
        return Err("constraint check failed".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xorshift_matches_reference() {
        for seed in [0u64, 1, 42, 0xDEADBEEF, 0xFFFF_FFFF_FFFF_FFFF] {
            let (x, r) = enhance_roll(seed);
            assert_eq!(r, x % 1000);
            assert!(r < 1000);
        }
    }

    #[test]
    fn trace_constraints_vanish_directly() {
        // 直接验证迹满足所有约束（不经过 STARK，排除电路 bug）
        let seed = 123456789012345u64;
        let roll = enhance_roll(seed).1;
        let trace = build_trace(seed, roll);
        let omega = Fp::root_of_unity(N_LOG2);
        let d = domain(N_LOG2);
        let mut cols: [Poly; 64] = std::array::from_fn(|_| Poly::zero());
        for j in 0..64 {
            let vals: Vec<Fp> = (0..16).map(|r| trace[r][j]).collect();
            cols[j] = Poly::interpolate(&d, &vals);
        }
        let csts = build_constraints(&cols, omega);
        for (rows, c) in &csts {
            for &m in rows {
                let c_at_row = c.eval(omega.pow(m as u64));
                assert_eq!(c_at_row, Fp::ZERO, "constraint at row {m}");
            }
        }
    }

    #[test]
    fn enhance_proof_roundtrip() {
        let seed = 987654321u64;
        let roll = enhance_roll(seed).1;
        let proof = prove_enhance(seed, roll, 8);
        assert_eq!(verify_enhance(&proof, roll, 8).unwrap(), ());
    }

    #[test]
    fn enhance_wrong_roll_rejected() {
        let seed = 555555u64;
        let roll = enhance_roll(seed).1;
        let proof = prove_enhance(seed, roll, 8);
        assert!(verify_enhance(&proof, (roll + 1) % 1000, 8).is_err());
    }
}
