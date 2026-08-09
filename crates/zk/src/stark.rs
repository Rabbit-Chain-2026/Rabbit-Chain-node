//! 最小 STARK（自研）：用 FRI 证明"秘密 witness 满足一组 AIR 约束"。
//!
//! 演示电路（M2）：证明知道秘密 a，使得平方链 t[0]=a, t[i+1]=t[i]²+c
//! 在位置 N−1 到达公开值 b。约束：
//!   - 转移：对 i ∈ 0..N−1，t[i+1] − t[i]² − c = 0
//!   - 边界：t[N−1] = b（公开）
//! a 由 b/c/N 不可逆推（域上 2^(N−1) 次根），故为零知识演示。
//!
//! 协议：trace 插值 → T(X)（deg<N）→ C(X)=T(ωX)−T(X)²−c（在 D∖{ω^(N−1)} 上为 0）
//! → 除转移消去多项式 Z=(X^N−1)/(X−ω^(N−1)) 得 Q → FRI(T) + FRI(Q) + 边界打开。

use crate::field::Fp;
use crate::fri::{prove, verify, FriProof, Transcript};
use crate::poly::Poly;

/// 平方链演示：证明结构。
#[derive(Clone, Debug)]
pub struct DemoProof {
    pub trace_fri: FriProof,
    pub quotient_fri: FriProof,
    /// 约束检查挑战点 χ（由承诺根推导，菲亚特-沙米尔风格）。
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

/// 由两个 FRI 证明的承诺根 + 末层系数推导约束挑战点（双方可重算）。
fn derive_challenge(proof: &DemoProof) -> Fp {
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
    let v = u64::from_be_bytes(h[..8].try_into().unwrap());
    Fp::new(v)
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

/// 证明：知道秘密 a，t[N−1] = b（c、b、N 公开）。
pub fn prove_square_chain(
    secret_a: u64,
    c: u64,
    n_log2: u32,
    num_queries: usize,
) -> DemoProof {
    let n = 1usize << n_log2;
    // 1) trace
    let mut trace = vec![Fp::new(secret_a)];
    for i in 1..n {
        let prev = trace[i - 1];
        trace.push(prev * prev + Fp::new(c));
    }
    let _b = trace[n - 1];
    // 2) 插值 T(X) over D
    let d = domain(n_log2);
    let t = Poly::interpolate(&d, &trace);
    // 3) 约束 C(X) = T(ωX) − T(X)² − c；Z = (X^N − 1)/(X − ω^(N−1))
    let w = Fp::root_of_unity(n_log2);
    let shifted = t.scale_var(w);
    let t_sq = t.mul(&t);
    let c_poly = (shifted - t_sq) - Poly::constant(Fp::new(c));
    // Z = ∏_{i=0}^{N−2} (X − ω^i)
    let mut z = Poly::from_coeffs(vec![Fp::ONE]);
    for i in 0..n - 1 {
        let wi = Fp::root_of_unity(n_log2).pow(i as u64);
        z = z.mul(&Poly::from_coeffs(vec![-wi, Fp::ONE]));
    }
    let (q, rem) = c_poly.div_mod(&z);
    debug_assert!(
        rem.degree() == 0 && rem.coeffs[0].is_zero(),
        "honest trace must divide cleanly; rem={:?}",
        rem
    );
    // 4) FRI(T) + FRI(Q) + 边界（T 在 ω^(N−1) = b）
    let boundary_pt = Fp::root_of_unity(n_log2).pow((n - 1) as u64);
    let trace_fri = prove(&t, n_log2, num_queries, Transcript::new(), Some(boundary_pt));
    let quotient_fri = prove(&q, n_log2, num_queries, Transcript::new(), None);
    // 5) 约束检查：挑战 χ（由承诺推导），打开 T(χ)/T(ωχ)/Q(χ)，
    //    验证方重算 Z(χ) 并检查 Q(χ)·Z(χ) == T(ωχ) − T(χ)² − c。
    let mut proof = DemoProof {
        trace_fri,
        quotient_fri,
        challenge: Fp::ZERO,
        opens: (Fp::ZERO, Fp::ZERO, Fp::ZERO),
    };
    let chi = derive_challenge(&proof);
    proof.challenge = chi;
    proof.opens = (t.eval(chi), t.eval(w * chi), q.eval(chi));
    proof
}

/// 验证：c、b、N、queries 公开。
pub fn verify_square_chain(
    proof: &DemoProof,
    c: u64,
    b: u64,
    n_log2: u32,
    num_queries: usize,
) -> Result<(), String> {
    let boundary_pt = Fp::root_of_unity(n_log2).pow(((1usize << n_log2) - 1) as u64);
    // T 边界 = b
    verify(
        &proof.trace_fri,
        n_log2,
        num_queries,
        Transcript::new(),
        Some((boundary_pt, Fp::new(b))),
    )?;
    // Q 低阶
    verify(&proof.quotient_fri, n_log2, num_queries, Transcript::new(), None)?;
    // 约束检查：Q(χ)·Z(χ) == T(ωχ) − T(χ)² − c
    let chi = derive_challenge(proof);
    if chi != proof.challenge {
        return Err("challenge mismatch".into());
    }
    let (t_chi, t_wchi, q_chi) = proof.opens;
    // Z(χ) = ∏_{i=0}^{N−2} (χ − ω^i)
    let n = 1usize << n_log2;
    let w = Fp::root_of_unity(n_log2);
    let mut z_chi = Fp::ONE;
    for i in 0..n - 1 {
        z_chi *= chi - w.pow(i as u64);
    }
    let rhs = (t_wchi - t_chi * t_chi) - Fp::new(c);
    if q_chi * z_chi != rhs {
        return Err("constraint check failed".into());
    }
    Ok(())
}

/// 证明公开输出（trace 末值）。
pub fn public_b(secret_a: u64, c: u64, n_log2: u32) -> u64 {
    let mut t = Fp::new(secret_a);
    for _ in 1..(1usize << n_log2) {
        t = t * t + Fp::new(c);
    }
    t.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_chain_proof_roundtrip() {
        let a = 1234567u64;
        let c = 99u64;
        let k = 4;
        let n_queries = 8;
        let b = public_b(a, c, k);
        let proof = prove_square_chain(a, c, k, n_queries);
        assert_eq!(verify_square_chain(&proof, c, b, k, n_queries).unwrap(), ());
    }

    #[test]
    fn square_chain_wrong_b_rejected() {
        let a = 777u64;
        let c = 5u64;
        let k = 4;
        let b = public_b(a, c, k);
        let proof = prove_square_chain(a, c, k, 8);
        assert!(verify_square_chain(&proof, c, b + 1, k, 8).is_err());
    }

    #[test]
    fn square_chain_wrong_c_rejected() {
        let a = 31337u64;
        let c = 42u64;
        let k = 4;
        let b = public_b(a, c, k);
        let proof = prove_square_chain(a, c, k, 8);
        assert!(verify_square_chain(&proof, c + 1, b, k, 8).is_err());
    }

    #[test]
    fn larger_domain_roundtrip() {
        let a = 987654321u64;
        let c = 7u64;
        let k = 5;
        let b = public_b(a, c, k);
        let proof = prove_square_chain(a, c, k, 8);
        assert_eq!(verify_square_chain(&proof, c, b, k, 8).unwrap(), ());
    }
}
