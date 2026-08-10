//! 塔挑战重模拟摘要电路（M2：ZK 主战场）。
//!
//! 场景：玩家（隐藏初始实力 `p_0`）闯 N 层塔。每层公开难度由**公开二次多项式**
//! `d(i) = A·i² + B·i + C` 给出（链上塔配置），成长率 `g`（千分位）公开。
//! 递推（毫秒制整数，零浮点）：
//! ```text
//!   p_{i+1} = p_i·(1000 + g) − d_i·1000
//! ```
//! 摘要 = 终层实力 `p_N`（公开声称）。证明"存在轨迹 p_0..p_N 满足递推与边界"。
//!
//! 链上验证 O(log N + 查询数)：Z(χ) 用 (χ^M − 1)/(χ − ω^(N−1)) 快速幂计算，
//! 无需重跑 N 层。N=127 层离链模拟（客户端），链上 O(1) 级验证——命中
//! "大计算离链证明"的 ZK 主战场定位。
//!
//! AIR：
//! - 转移约束（0 ≤ i < N）：`T(ω^i+1) − T(ω^i)·(1000+g) + d(ω^i)·1000 = 0`
//! - 边界：`T(ω^(N−1)) = p_N`（最后一行 = 摘要）
//! - 商多项式 `Q = C/Z`，`Z = (X^M − 1)/(X − ω^(N−1))`

use crate::field::Fp;
use crate::fri::{prove, verify, FriProof, Transcript};
use crate::poly::Poly;

/// 迹长（行数）对数：N_L2 层 + 末行摘要 → 2^N_L2 行（2^N_L2 必须 ≥ 层数+1）。
/// 默认 7 → 128 行（127 层）；证明体积受节点 gas 上限约束（估算 gas ≈ 4.7M < 5M）。
pub const TOWER_LOG2: u32 = 7;
pub const TOWER_ROWS: u32 = 1 << TOWER_LOG2; // 256
/// 层数 = 行数 − 1（末行是摘要）。
pub const TOWER_LAYERS: u64 = (TOWER_ROWS - 1) as u64;

/// 塔证明结构（与 square-chain DemoProof 同构）。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TowerProof {
    pub trace_fri: FriProof,
    pub quotient_fri: FriProof,
    /// 约束检查挑战 χ（由承诺根推导，菲亚特-沙米尔）。
    pub challenge: Fp,
    /// 约束检查打开：T(χ), T(ωχ), Q(χ)。
    pub opens: (Fp, Fp, Fp),
}

fn sha3_256(msg: &[u8]) -> [u8; 32] {
    use sha3::{Digest, Keccak256};
    let mut h = Keccak256::new();
    h.update(msg);
    h.finalize().into()
}

/// 公开难度多项式 d(X) = A·X² + B·X + C（系数公开，O(1) 求值）。
pub fn difficulty_poly(a: u64, b: u64, c: u64) -> Poly {
    Poly::from_coeffs(vec![Fp::new(c), Fp::new(b), Fp::new(a)])
}

/// 客户端参考模拟（离链）：从初始实力闯 `layers` 层，返回终层实力（毫秒）。
/// 难度按**域点**求值 d(ω^i)（与 AIR 约束一致：公开难度是域元素的多项式函数）。
pub fn simulate_tower(initial_power: u64, growth_permille: u64, a: u64, b: u64, c: u64, layers: u64) -> u64 {
    let mut p = Fp::new(initial_power.saturating_mul(1000));
    let g = Fp::new(1000 + growth_permille);
    let omega = Fp::root_of_unity(TOWER_LOG2);
    let d = difficulty_poly(a, b, c);
    for i in 0..layers {
        let di = d.eval(omega.pow(i));
        p = p * g - di * Fp::new(1000);
    }
    p.0
}

/// 由两个 FRI 的承诺根 + 末层系数推导约束挑战 χ。
fn derive_challenge(proof: &TowerProof) -> Fp {
    let mut buf = vec![];
    for r in &proof.trace_fri.roots {
        buf.extend_from_slice(r);
    }
    for r in &proof.quotient_fri.roots {
        buf.extend_from_slice(r);
    }
    for c in &proof.trace_fri.final_coeffs {
        buf.extend_from_slice(&c.0.to_be_bytes());
    }
    for c in &proof.quotient_fri.final_coeffs {
        buf.extend_from_slice(&c.0.to_be_bytes());
    }
    let h = sha3_256(&buf);
    Fp::new(u64::from_be_bytes(h[..8].try_into().unwrap()))
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

/// 证明：知道秘密初始实力 p_0，使 127 层塔递推的终层实力 = claimed_final。
/// `num_queries` 与节点共识一致；FRI 域 = 4× 迹域（低度膨胀，见 enhance 加固）。
pub fn prove_tower(
    initial_power: u64,
    growth_permille: u64,
    a: u64,
    b: u64,
    c: u64,
    num_queries: usize,
) -> TowerProof {
    let rows = TOWER_ROWS as usize;
    let d = difficulty_poly(a, b, c);
    let g = Fp::new(1000 + growth_permille);
    let omega = Fp::root_of_unity(TOWER_LOG2);
    // 1) trace：p_0（秘密）→ p_1 → ... → p_{rows-1}
    let mut trace = vec![Fp::new(initial_power.saturating_mul(1000))];
    for i in 0..rows - 1 {
        let prev = trace[i];
        let di = d.eval(omega.pow(i as u64));
        trace.push(prev * g - di * Fp::new(1000));
    }
    // 2) 插值 T(X) over D（256 点）
    let t = Poly::interpolate(&domain(TOWER_LOG2), &trace);
    // 3) 约束 C(X) = T(ωX) − T(X)·(1000+g) + d(X)·1000
    //    Z(X) = (X^rows − 1)/(X − ω^(rows−1))（除末行外全消）
    let shifted = t.scale_var(omega);
    let c_poly = (shifted - t.mul(&Poly::constant(g))) + d.mul(&Poly::constant(Fp::new(1000)));
    let z = {
        // X^rows − 1（rows 行全消），再除 (X − ω^(rows−1))（除末行外全消）
        let mut x_rows_coeffs = vec![Fp::ZERO; rows + 1];
        x_rows_coeffs[rows] = Fp::ONE;
        let x_rows = Poly::from_coeffs(x_rows_coeffs);
        let minus_one = Poly::constant(Fp::new(-1i64 as u64));
        let denom = Poly::from_coeffs(vec![-omega.pow(TOWER_LAYERS), Fp::ONE]);
        let (z, _) = (x_rows + minus_one).div_mod(&denom);
        z
    };
    let (q, rem) = c_poly.div_mod(&z);
    debug_assert!(
        rem.degree() == 0 && rem.coeffs[0].is_zero(),
        "honest tower trace must divide cleanly; rem={:?}",
        rem
    );
    // 4) FRI（4 倍膨胀：迹域 256 → FRI 域 1024）
    let fri_k = TOWER_LOG2 + 2;
    let boundary_pt = omega.pow(TOWER_LAYERS);
    let trace_fri = prove(&t, fri_k, num_queries, Transcript::new(), Some(boundary_pt));
    let quotient_fri = prove(&q, fri_k, num_queries, Transcript::new(), None);
    // 5) 约束检查挑战与打开
    let mut proof = TowerProof {
        trace_fri,
        quotient_fri,
        challenge: Fp::ZERO,
        opens: (Fp::ZERO, Fp::ZERO, Fp::ZERO),
    };
    let chi = derive_challenge(&proof);
    proof.challenge = chi;
    proof.opens = (t.eval(chi), t.eval(omega * chi), q.eval(chi));
    proof
}

/// 验证：growth/A/B/C、claimed_final、queries 公开。返回 Err(原因)。
pub fn verify_tower(
    proof: &TowerProof,
    growth_permille: u64,
    a: u64,
    b: u64,
    c: u64,
    claimed_final: u64,
    num_queries: usize,
) -> Result<(), String> {
    let rows = TOWER_ROWS;
    let omega = Fp::root_of_unity(TOWER_LOG2);
    let fri_k = TOWER_LOG2 + 2;
    let boundary_pt = omega.pow(TOWER_LAYERS);
    // 边界期望：T(ω^(N−1)) = claimed_final（毫秒）
    verify(
        &proof.trace_fri,
        fri_k,
        num_queries,
        Transcript::new(),
        Some((boundary_pt, Fp::new(claimed_final))),
    )?;
    // Q 低阶
    verify(&proof.quotient_fri, fri_k, num_queries, Transcript::new(), None)?;
    // 约束检查：Q(χ)·Z(χ) == T(ωχ) − T(χ)·(1000+g) + d(χ)·1000
    let chi = derive_challenge(proof);
    if chi != proof.challenge {
        return Err("tower challenge mismatch".into());
    }
    let (t_chi, t_wchi, q_chi) = proof.opens;
    // Z(χ) = (χ^rows − 1)/(χ − ω^(rows−1))（快速幂，O(log N)）
    let chi_pow = chi.pow(rows as u64);
    let z_chi = (chi_pow - Fp::ONE) * (chi - omega.pow(TOWER_LAYERS)).inv_self();
    let g = Fp::new(1000 + growth_permille);
    let d_chi = difficulty_poly(a, b, c).eval(chi);
    let rhs = (t_wchi - t_chi * g) + d_chi * Fp::new(1000);
    if q_chi * z_chi != rhs {
        return Err("tower constraint check failed".into());
    }
    Ok(())
}

/// bincode 序列化（与 enhance 一致：hex 编码后上链）。
pub fn to_bytes(proof: &TowerProof) -> Vec<u8> {
    bincode::serialize(proof).expect("tower proof bincode")
}

pub fn from_bytes(bytes: &[u8]) -> Result<TowerProof, String> {
    bincode::deserialize(bytes).map_err(|e| format!("tower proof decode: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tower_proof_roundtrip() {
        let initial = 1234u64;
        let g = 50u64; // 成长率 5%
        let (a, b, c) = (1u64, 100u64, 500u64);
        let final_power = simulate_tower(initial, g, a, b, c, TOWER_LAYERS);
        let proof = prove_tower(initial, g, a, b, c, 16);
        assert_eq!(
            verify_tower(&proof, g, a, b, c, final_power, 16).unwrap(),
            ()
        );
    }

    #[test]
    fn tower_wrong_final_rejected() {
        let initial = 777u64;
        let g = 40u64;
        let (a, b, c) = (2u64, 50u64, 300u64);
        let final_power = simulate_tower(initial, g, a, b, c, TOWER_LAYERS);
        let proof = prove_tower(initial, g, a, b, c, 16);
        assert!(verify_tower(&proof, g, a, b, c, final_power + 1000, 16).is_err());
    }

    #[test]
    fn tower_wrong_growth_rejected() {
        let initial = 555u64;
        let g = 30u64;
        let (a, b, c) = (1u64, 20u64, 100u64);
        let final_power = simulate_tower(initial, g, a, b, c, TOWER_LAYERS);
        let proof = prove_tower(initial, g, a, b, c, 16);
        assert!(verify_tower(&proof, g + 1, a, b, c, final_power, 16).is_err());
    }

    #[test]
    fn tower_sim_matches_explicit_reference() {
        // 小层数显式递推（Fp 算术，避免 u64 溢出）：p_{i+1} = p_i·(1000+g) − d_i·1000
        let (initial, g) = (100u64, 25u64);
        let (a, b, c) = (0u64, 0u64, 1000u64); // 恒定难度 1000
        let layers = 5;
        let final_power = simulate_tower(initial, g, a, b, c, layers);
        let mut p = Fp::new(initial * 1000);
        for _ in 0..layers {
            p = p * Fp::new(1000 + g) - Fp::new(1000 * 1000);
        }
        assert_eq!(final_power, p.0);
    }
}
