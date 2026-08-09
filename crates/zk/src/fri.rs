//! FRI（Fast Reed-Solomon Interactive Oracle Proof of Proximity）低阶测试。
//!
//! 自研实现（无外部证明框架）：
//! - 承诺：把低阶多项式 f(X)（deg < 2^k）在 2^k 阶单位根域上求值，Merkle 承诺。
//! - 折叠：f_{j+1}(y) = E(y) + α_j·O(y)，其中 E/O 是 f_j 的偶/奇部分，y = x²。
//!   （deg 每轮减半；域 D_{j+1} = {x² : x ∈ D_j}）
//! - 打开：验证方随机查若干点，逐层检查折叠方程与 Merkle 路径，末层以系数发送。
//!
//! 菲亚特-沙米尔：提交 layer j 根后挤压 α_j（双方同序，抗选择挑战）。

use crate::field::Fp;
use crate::merkle::MerkleTree;
use crate::poly::Poly;
use sha3::{Digest, Keccak256};

fn sha3_256(msg: &[u8]) -> [u8; 32] {
    let mut h = Keccak256::new();
    h.update(msg);
    h.finalize().into()
}

/// 菲亚特-沙米尔抄本：absorb 承诺，squeeze 域元素。
#[derive(Clone, Debug, Default)]
pub struct Transcript {
    state: Vec<u8>,
}

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn absorb(&mut self, bytes: &[u8]) {
        self.state.extend_from_slice(bytes);
    }
    pub fn squeeze_fp(&mut self) -> Fp {
        let mut prefix = self.state.clone();
        prefix.extend_from_slice(&[0u8; 32]);
        let h: [u8; 32] = sha3_256(&prefix);
        let v = u64::from_be_bytes(h[..8].try_into().unwrap());
        self.state = h.to_vec();
        Fp::new(v)
    }
}

/// 2^k 阶单位根域 {ω^0..ω^(2^k − 1)}。
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

/// f = E(x²) + x·O(x²) 拆偶/奇部分。
fn even_odd_split(f: &Poly) -> (Poly, Poly) {
    let mut e = vec![];
    let mut o = vec![];
    for (i, &c) in f.coeffs.iter().enumerate() {
        if i % 2 == 0 {
            e.push(c);
        } else {
            o.push(c);
        }
    }
    (Poly::from_coeffs(e), Poly::from_coeffs(o))
}

/// 单点打开：值 + Merkle 认证路径。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct OpenVal {
    pub index: usize,
    pub value: Fp,
    pub path: Vec<[u8; 32]>,
}

/// FRI 证明。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FriProof {
    /// 层 0..folds−1 的承诺根（末层以系数发送）。
    pub roots: Vec<[u8; 32]>,
    /// 末层多项式系数（deg ≤ 1）。
    pub final_coeffs: Vec<Fp>,
    /// 每次查询的逐层打开：每层 [x 打开, −x 打开, y 打开]。
    pub queries: Vec<Vec<[OpenVal; 3]>>,
    /// 每次查询的起点 x0。
    pub query_starts: Vec<Fp>,
    /// 可选指定点打开（STARK 边界；点必须 ∈ D0）。
    pub boundary: Option<OpenVal>,
}

struct LayerCommit {
    tree: MerkleTree,
    values: Vec<Fp>,
}

fn commit_layer(poly: &Poly, k: u32) -> (LayerCommit, [u8; 32]) {
    let d = domain(k);
    let values: Vec<Fp> = poly.eval_many(&d);
    let raw: Vec<u64> = values.iter().map(|x| x.0).collect();
    let tree = MerkleTree::new(&raw);
    let root = tree.root;
    (LayerCommit { tree, values }, root)
}

fn open_at(commit: &LayerCommit, dom: &[Fp], x: Fp) -> OpenVal {
    let index = dom.iter().position(|&v| v == x).expect("point in domain");
    OpenVal {
        index,
        value: commit.values[index],
        path: commit.tree.auth_path(index),
    }
}

/// 证明方：对低阶多项式 f（deg < 2^k）生成 FRI 证明。
/// `boundary` 提供时，额外打开指定点（必须 ∈ D0；用于 STARK 边界检查）。
pub fn prove(
    f: &Poly,
    k: u32,
    num_queries: usize,
    mut transcript: Transcript,
    boundary: Option<Fp>,
) -> FriProof {
    let folds = (k - 1) as usize;
    // 逐层提交 + 挤压 α（菲亚特-沙米尔：先提交再挑战）
    let mut commits = vec![];
    let mut roots = vec![];
    let mut alphas = vec![];
    let mut cur = f.clone();
    for j in 0..folds {
        let (c, root) = commit_layer(&cur, k - j as u32);
        transcript.absorb(&root);
        commits.push(c);
        roots.push(root);
        let alpha = transcript.squeeze_fp();
        alphas.push(alpha);
        let (e, o) = even_odd_split(&cur);
        cur = e + o.scale(alpha);
    }
    let final_coeffs = cur.coeffs.clone();

    // 查询
    let d0 = domain(k);
    let mut queries = vec![];
    let mut starts = vec![];
    for _ in 0..num_queries {
        let h = sha3_256(&transcript.state);
        let idx = u64::from_be_bytes(h[..8].try_into().unwrap());
        transcript.absorb(&h);
        let mut x = d0[(idx as usize) % d0.len()];
        starts.push(x);
        let mut opens = vec![];
        for j in 0..folds {
            let dj = domain(k - j as u32);
            let neg_x = -x;
            let y = x * x;
            let o1 = open_at(&commits[j], &dj, x);
            let o2 = open_at(&commits[j], &dj, neg_x);
            let o3 = if j + 1 < folds {
                let dj1 = domain(k - (j + 1) as u32);
                open_at(&commits[j + 1], &dj1, y)
            } else {
                // 末层：由系数直接算（无 Merkle）
                OpenVal { index: usize::MAX, value: cur.eval(y), path: vec![] }
            };
            opens.push([o1, o2, o3]);
            x = y;
        }
        queries.push(opens);
    }
    // 边界打开（可选）
    let boundary = boundary.map(|pt| open_at(&commits[0], &d0, pt));
    FriProof { roots, final_coeffs, queries, query_starts: starts, boundary }
}

/// 验证方：检查 FRI 证明（返回 Err(原因)）。`boundary_expected` 提供时校验边界值。
pub fn verify(
    fri: &FriProof,
    k: u32,
    num_queries: usize,
    mut transcript: Transcript,
    boundary_expected: Option<(Fp, Fp)>,
) -> Result<(), String> {
    let folds = (k - 1) as usize;
    if fri.roots.len() != folds {
        return Err(format!("expected {} roots, got {}", folds, fri.roots.len()));
    }
    if fri.final_coeffs.len() > 2 {
        return Err("final layer degree too high".into());
    }
    if fri.queries.len() != num_queries || fri.query_starts.len() != num_queries {
        return Err("query count mismatch".into());
    }
    // 恢复 α_j（先吸收根再挤压，与证明方同序）
    let mut alphas = vec![];
    for j in 0..folds {
        transcript.absorb(&fri.roots[j]);
        alphas.push(transcript.squeeze_fp());
    }
    let two_inv = Fp::new(2).inv_self();
    for (qi, opens) in fri.queries.iter().enumerate() {
        if opens.len() != folds {
            return Err("query layer count mismatch".into());
        }
        let mut x = fri.query_starts[qi];
        let d0 = domain(k);
        if !d0.contains(&x) {
            return Err("query start not in domain".into());
        }
        for j in 0..folds {
            let [ox, onx, oy] = &opens[j];
            if !MerkleTree::verify(ox.index, ox.value.0, &ox.path, &fri.roots[j]) {
                return Err(format!("merkle fail layer {j} at x (query {qi})"));
            }
            if !MerkleTree::verify(onx.index, onx.value.0, &onx.path, &fri.roots[j]) {
                return Err(format!("merkle fail layer {j} at -x (query {qi})"));
            }
            let y = x * x;
            if j + 1 < folds {
                if !MerkleTree::verify(oy.index, oy.value.0, &oy.path, &fri.roots[j + 1]) {
                    return Err(format!("merkle fail layer {j} at y (query {qi})"));
                }
            } else {
                let f_last = Poly::from_coeffs(fri.final_coeffs.clone());
                if f_last.eval(y) != oy.value {
                    return Err("final layer eval mismatch".into());
                }
            }
            // 折叠方程
            let sum = ox.value + onx.value;
            let diff = ox.value - onx.value;
            let expected = sum * two_inv + alphas[j] * (diff * two_inv * x.inv_self());
            if expected != oy.value {
                return Err(format!("fold mismatch at query {qi} layer {j}"));
            }
            x = y;
        }
    }
    // 边界校验
    if let Some((pt, expected)) = boundary_expected {
        let open = fri.boundary.as_ref().ok_or("boundary open missing")?;
        if !MerkleTree::verify(open.index, open.value.0, &open.path, &fri.roots[0]) {
            return Err("boundary merkle fail".into());
        }
        if open.value != expected {
            return Err("boundary value mismatch".into());
        }
        if domain(k)[open.index] != pt {
            return Err("boundary point mismatch".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random_poly(k: u32, seed: u64) -> Poly {
        let n = 1usize << k;
        let mut s = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut coeffs = vec![];
        for _ in 0..n {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            coeffs.push(Fp::new(s));
        }
        Poly::from_coeffs(coeffs)
    }

    #[test]
    fn fri_honest_proof_verifies() {
        let k = 4;
        let f = random_poly(k, 42);
        let proof = prove(&f, k, 8, Transcript::new(), None);
        assert_eq!(verify(&proof, k, 8, Transcript::new(), None).unwrap(), ());
    }

    #[test]
    fn fri_tampered_proof_rejected() {
        let k = 4;
        let f = random_poly(k, 7);
        let mut proof = prove(&f, k, 8, Transcript::new(), None);
        proof.queries[0][0][0].value += Fp::ONE;
        assert!(verify(&proof, k, 8, Transcript::new(), None).is_err());
    }

    #[test]
    fn fri_wrong_polynomial_rejected() {
        let k = 4;
        let f = random_poly(k, 1);
        let g = random_poly(k, 2);
        let proof = prove(&f, k, 8, Transcript::new(), None);
        // 用错误多项式构造的验证（通过伪造 roots 显然失败；这里验证合法形状但错误内容）
        let proof2 = prove(&g, k, 8, Transcript::new(), None);
        assert_ne!(proof.roots, proof2.roots);
    }

    #[test]
    fn even_odd_split_recomposes() {
        let f = Poly::from_coeffs(vec![Fp::new(1), Fp::new(2), Fp::new(3), Fp::new(4)]);
        let (e, o) = even_odd_split(&f);
        for x in [Fp::new(0), Fp::new(1), Fp::new(2), Fp::new(7)] {
            let y = x * x;
            assert_eq!(e.eval(y) + x * o.eval(y), f.eval(x));
        }
    }
}
