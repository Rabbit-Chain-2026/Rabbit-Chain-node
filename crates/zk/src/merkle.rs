//! Merkle 承诺（自研，keccak-256 作为哈希）：用于 FRI/STARK 的承诺与打开。
//!
//! 结构：叶子哈希 = H(index || value)；内部节点 = H(left || right)。
//! 索引前缀避免长度扩展攻击下的碰撞（本协议为教学/自研版本，采用标准做法）。

use sha3::{Digest, Keccak256};

/// 哈希一个域元素（叶子：index 与值一起哈希，防位置交换）。
pub fn hash_leaf(index: usize, value: u64) -> [u8; 32] {
    let mut h = Keccak256::new();
    h.update((index as u64).to_be_bytes());
    h.update(value.to_be_bytes());
    h.finalize().into()
}

/// 哈希内部节点（子节点哈希拼接）。
fn hash_node(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut h = Keccak256::new();
    h.update(left);
    h.update(right);
    h.finalize().into()
}

/// Merkle 树：叶子数必须是 2 的幂；根承诺整个数组。
#[derive(Clone, Debug)]
pub struct MerkleTree {
    /// 层自底向上：[叶层, ..., 根]；每层长度减半。
    levels: Vec<Vec<[u8; 32]>>,
    /// 叶子数（2 的幂）。
    pub leaf_count: usize,
    /// 树根。
    pub root: [u8; 32],
}

impl MerkleTree {
    /// 从叶子值构建（leaf_count 必须为 2 的幂）。
    pub fn new(leaves: &[u64]) -> Self {
        let n = leaves.len();
        assert!(n.is_power_of_two(), "leaf count must be power of two");
        assert!(n >= 1);
        let mut cur: Vec<[u8; 32]> = leaves
            .iter()
            .enumerate()
            .map(|(i, &v)| hash_leaf(i, v))
            .collect();
        let mut levels = vec![cur.clone()];
        while cur.len() > 1 {
            let mut next = Vec::with_capacity(cur.len() / 2);
            for pair in cur.chunks(2) {
                next.push(hash_node(&pair[0], &pair[1]));
            }
            levels.push(next.clone());
            cur = next;
        }
        let root = levels.last().expect("levels")[0];
        Self {
            levels,
            leaf_count: n,
            root,
        }
    }

    /// 叶子 index 的认证路径：从底到根的所有兄弟节点哈希。
    pub fn auth_path(&self, index: usize) -> Vec<[u8; 32]> {
        let mut idx = index;
        let mut path = Vec::with_capacity(self.levels.len() - 1);
        for level in &self.levels[..self.levels.len() - 1] {
            path.push(level[idx ^ 1]);
            idx /= 2;
        }
        path
    }

    /// 校验认证路径是否通向根（验证方用，不持有整棵树）。
    pub fn verify(index: usize, value: u64, path: &[[u8; 32]], root: &[u8; 32]) -> bool {
        let mut h = hash_leaf(index, value);
        let mut idx = index;
        for (depth, sibling) in path.iter().enumerate() {
            h = if idx & 1 == 0 {
                hash_node(&h, sibling)
            } else {
                hash_node(sibling, &h)
            };
            idx /= 2;
            let _ = depth;
        }
        h == *root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_path_roundtrip() {
        let leaves: Vec<u64> = (0..8).map(|i| i * 1000 + 7).collect();
        let tree = MerkleTree::new(&leaves);
        for i in 0..8 {
            let path = tree.auth_path(i);
            assert!(MerkleTree::verify(i, leaves[i], &path, &tree.root), "i={i}");
            // 篡改值 → 校验失败
            assert!(!MerkleTree::verify(i, leaves[i] + 1, &path, &tree.root), "tamper i={i}");
        }
    }

    #[test]
    fn single_leaf_root_is_leaf_hash() {
        let tree = MerkleTree::new(&[42]);
        assert_eq!(tree.root, hash_leaf(0, 42));
    }

    #[test]
    fn different_leaves_different_roots() {
        let t1 = MerkleTree::new(&[1, 2, 3, 4]);
        let t2 = MerkleTree::new(&[1, 2, 3, 5]);
        assert_ne!(t1.root, t2.root);
    }
}
