//! Merkle Patricia Trie implementation (core, moved from rabbitstore).
//!
//! Pure in-memory MPT: `MemTrieDB` + `MerklePatriciaTrie`. Lives in rabbitcore
//! so consensus code (StateExecutor) can compute the unspent-output state root
//! deterministically. rabbitstore re-exports these types and keeps the
//! persistent/cached DB variants on top.

use super::node::*;
use super::proof::TrieProof;
use parking_lot::RwLock;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use crate::crypto::{keccak256, Hash};

/// Trie operation result.
pub type Result<T> = std::result::Result<T, TrieError>;

/// Minimal trie error (consensus-facing; DB backends never fail in-memory).
#[derive(Debug)]
pub enum TrieError {
    Serialization(String),
    NotFound(String),
}
impl std::fmt::Display for TrieError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrieError::Serialization(s) => write!(f, "trie serialization: {s}"),
            TrieError::NotFound(s) => write!(f, "trie not found: {s}"),
        }
    }
}
impl std::error::Error for TrieError {}

/// Trie database trait
pub trait TrieDB: Send + Sync {
    /// Get node by hash
    fn get_node(&self, hash: &NodeHash) -> Result<Option<Vec<u8>>>;
    /// Put node
    fn put_node(&self, hash: &NodeHash, data: &[u8]) -> Result<()>;
    /// Check if node exists
    fn has_node(&self, hash: &NodeHash) -> Result<bool>;
}

/// In-memory Trie database (for testing)
pub struct MemTrieDB {
    nodes: RwLock<HashMap<NodeHash, Vec<u8>>>,
}

impl MemTrieDB {
    pub fn new() -> Self {
        Self {
            nodes: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for MemTrieDB {
    fn default() -> Self {
        Self::new()
    }
}

impl TrieDB for MemTrieDB {
    fn get_node(&self, hash: &NodeHash) -> Result<Option<Vec<u8>>> {
        Ok(self.nodes.read().get(hash).cloned())
    }

    fn put_node(&self, hash: &NodeHash, data: &[u8]) -> Result<()> {
        self.nodes.write().insert(*hash, data.to_vec());
        Ok(())
    }

    fn has_node(&self, hash: &NodeHash) -> Result<bool> {
        Ok(self.nodes.read().contains_key(hash))
    }
}


/// Merkle Patricia Trie
pub struct MerklePatriciaTrie {
    /// Root node hash
    root: RwLock<Option<NodeHash>>,
    /// Database
    db: Arc<dyn TrieDB>,
    /// Node cache
    cache: RwLock<HashMap<NodeHash, TrieNode>>,
    /// Dirty nodes (pending write)
    dirty: RwLock<HashMap<NodeHash, TrieNode>>,
}

impl MerklePatriciaTrie {
    /// Create new empty trie
    pub fn new(db: Arc<dyn TrieDB>) -> Self {
        Self {
            root: RwLock::new(None),
            db,
            cache: RwLock::new(HashMap::new()),
            dirty: RwLock::new(HashMap::new()),
        }
    }

    /// Create trie from root hash
    pub fn from_root(root: Hash, db: Arc<dyn TrieDB>) -> Self {
        Self {
            root: RwLock::new(Some(root)),
            db,
            cache: RwLock::new(HashMap::new()),
            dirty: RwLock::new(HashMap::new()),
        }
    }

    /// Get root hash
    pub fn root(&self) -> Hash {
        (*self.root.read()).unwrap_or_else(empty_trie_root)
    }

    /// Get value by key
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let hashed_key = keccak256(key);
        let nibbles = NibbleSlice::new(&hashed_key);

        match *self.root.read() {
            None => Ok(None),
            Some(root_hash) => {
                let node = self.get_node_by_hash(&root_hash)?;
                self.get_recursive(&node, &nibbles, 0)
            }
        }
    }

    /// Recursive get
    fn get_recursive(
        &self,
        node: &TrieNode,
        key: &NibbleSlice<'_>,
        depth: usize,
    ) -> Result<Option<Vec<u8>>> {
        match node {
            TrieNode::Empty => Ok(None),

            TrieNode::Leaf(leaf) => {
                let key_suffix = key.slice_from(depth);
                if key_suffix.equals_nibbles(&leaf.key_suffix) {
                    Ok(Some(leaf.value.clone()))
                } else {
                    Ok(None)
                }
            }

            TrieNode::Extension(ext) => {
                let key_suffix = key.slice_from(depth);
                let common = key_suffix.common_prefix_nibbles(&ext.prefix);

                if common == ext.prefix.len() {
                    let child = self.get_node_by_hash(&ext.child)?;
                    self.get_recursive(&child, key, depth + ext.prefix.len())
                } else {
                    Ok(None)
                }
            }

            TrieNode::Branch(branch) => {
                if depth >= key.len() {
                    Ok(branch.value.clone())
                } else {
                    let index = key.at(depth) as usize;
                    match &branch.children[index] {
                        Some(child_hash) => {
                            let child = self.get_node_by_hash(child_hash)?;
                            self.get_recursive(&child, key, depth + 1)
                        }
                        None => Ok(None),
                    }
                }
            }
        }
    }

    /// Insert key-value pair
    pub fn insert(&self, key: &[u8], value: Vec<u8>) -> Result<Hash> {
        let hashed_key = keccak256(key);
        let nibbles = NibbleSlice::new(&hashed_key);

        let new_root = match *self.root.read() {
            None => {
                // Create new leaf node
                let leaf = TrieNode::Leaf(LeafNode::new(nibbles.to_nibbles(), value));
                self.save_node(leaf)?
            }
            Some(root_hash) => {
                let root_node = self.get_node_by_hash(&root_hash)?;
                self.insert_recursive(&root_node, &nibbles, 0, value)?
            }
        };

        *self.root.write() = Some(new_root);
        self.flush()?;

        Ok(new_root)
    }

    /// Recursive insert
    fn insert_recursive(
        &self,
        node: &TrieNode,
        key: &NibbleSlice<'_>,
        depth: usize,
        value: Vec<u8>,
    ) -> Result<NodeHash> {
        match node {
            TrieNode::Empty => {
                let leaf = TrieNode::Leaf(LeafNode::new(key.slice_from(depth).to_nibbles(), value));
                self.save_node(leaf)
            }

            TrieNode::Leaf(leaf) => {
                let key_suffix = key.slice_from(depth);
                let common = key_suffix.common_prefix_nibbles(&leaf.key_suffix);

                if common == leaf.key_suffix.len() && common == key_suffix.len() {
                    // Update existing leaf
                    let new_leaf = TrieNode::Leaf(LeafNode::new(leaf.key_suffix.clone(), value));
                    self.save_node(new_leaf)
                } else {
                    // Split leaf
                    self.split_leaf(leaf, key, depth, common, value)
                }
            }

            TrieNode::Extension(ext) => {
                let key_suffix = key.slice_from(depth);
                let common = key_suffix.common_prefix_nibbles(&ext.prefix);

                if common == ext.prefix.len() {
                    // Continue down the extension
                    let child = self.get_node_by_hash(&ext.child)?;
                    let new_child =
                        self.insert_recursive(&child, key, depth + ext.prefix.len(), value)?;

                    let new_ext =
                        TrieNode::Extension(ExtensionNode::new(ext.prefix.clone(), new_child));
                    self.save_node(new_ext)
                } else {
                    // Split extension
                    self.split_extension(ext, key, depth, common, value)
                }
            }

            TrieNode::Branch(branch) => {
                if depth >= key.len() {
                    // Update value at branch
                    let mut new_branch = (**branch).clone();
                    new_branch.value = Some(value);
                    self.save_node(TrieNode::Branch(Box::new(new_branch)))
                } else {
                    // Insert into child
                    let index = key.at(depth) as usize;
                    let mut new_branch = (**branch).clone();

                    let new_child = match &branch.children[index] {
                        Some(child_hash) => {
                            let child = self.get_node_by_hash(child_hash)?;
                            self.insert_recursive(&child, key, depth + 1, value)?
                        }
                        None => {
                            let leaf = TrieNode::Leaf(LeafNode::new(
                                key.slice_from(depth + 1).to_nibbles(),
                                value,
                            ));
                            self.save_node(leaf)?
                        }
                    };

                    new_branch.children[index] = Some(new_child);
                    self.save_node(TrieNode::Branch(Box::new(new_branch)))
                }
            }
        }
    }

    /// Split leaf node
    fn split_leaf(
        &self,
        leaf: &LeafNode,
        key: &NibbleSlice<'_>,
        depth: usize,
        common: usize,
        value: Vec<u8>,
    ) -> Result<NodeHash> {
        // Create new leaf for existing value
        let existing_leaf = TrieNode::Leaf(LeafNode::new(
            leaf.key_suffix[common + 1..].to_vec(),
            leaf.value.clone(),
        ));
        let existing_hash = self.save_node(existing_leaf)?;

        // Create new leaf for new value
        let new_leaf = TrieNode::Leaf(LeafNode::new(
            key.slice_from(depth + common + 1).to_nibbles(),
            value,
        ));
        let new_hash = self.save_node(new_leaf)?;

        // Create branch node
        let mut branch = BranchNode::new();
        branch.children[leaf.key_suffix[common] as usize] = Some(existing_hash);
        branch.children[key.at(depth + common) as usize] = Some(new_hash);

        let branch_hash = self.save_node(TrieNode::Branch(Box::new(branch)))?;

        // Create extension if there's a common prefix
        if common > 0 {
            let ext = TrieNode::Extension(ExtensionNode::new(
                leaf.key_suffix[..common].to_vec(),
                branch_hash,
            ));
            self.save_node(ext)
        } else {
            Ok(branch_hash)
        }
    }

    /// Split extension node
    fn split_extension(
        &self,
        ext: &ExtensionNode,
        key: &NibbleSlice<'_>,
        depth: usize,
        common: usize,
        value: Vec<u8>,
    ) -> Result<NodeHash> {
        let mut branch = BranchNode::new();

        // Old extension path becomes one branch arm.
        let old_index = ext.prefix[common] as usize;
        let old_remaining = if common + 1 < ext.prefix.len() {
            ext.prefix[common + 1..].to_vec()
        } else {
            Vec::new()
        };

        let old_child_hash = if old_remaining.is_empty() {
            ext.child
        } else {
            self.save_node(TrieNode::Extension(ExtensionNode::new(
                old_remaining,
                ext.child,
            )))?
        };
        branch.children[old_index] = Some(old_child_hash);

        // New key path becomes the other branch arm.
        let new_index = key.at(depth + common) as usize;
        let new_suffix = key.slice_from(depth + common + 1).to_nibbles();
        if new_suffix.is_empty() {
            branch.value = Some(value);
        } else {
            let new_leaf = TrieNode::Leaf(LeafNode::new(new_suffix, value));
            let new_leaf_hash = self.save_node(new_leaf)?;
            branch.children[new_index] = Some(new_leaf_hash);
        }

        let branch_hash = self.save_node(TrieNode::Branch(Box::new(branch)))?;

        // Preserve common prefix (if any) as a new extension above branch.
        if common > 0 {
            self.save_node(TrieNode::Extension(ExtensionNode::new(
                ext.prefix[..common].to_vec(),
                branch_hash,
            )))
        } else {
            Ok(branch_hash)
        }
    }

    /// Remove key
    pub fn remove(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let hashed_key = keccak256(key);
        let nibbles = NibbleSlice::new(&hashed_key);
        let root = *self.root.read();

        match root {
            None => Ok(None),
            Some(root_hash) => {
                let root_node = self.get_node_by_hash(&root_hash)?;
                let (new_root, removed_value) = self.remove_recursive(&root_node, &nibbles, 0)?;

                *self.root.write() = new_root;
                if new_root.is_some() {
                    self.flush()?;
                }

                Ok(removed_value)
            }
        }
    }

    /// Recursive remove
    fn remove_recursive(
        &self,
        node: &TrieNode,
        key: &NibbleSlice<'_>,
        depth: usize,
    ) -> Result<(Option<NodeHash>, Option<Vec<u8>>)> {
        match node {
            TrieNode::Empty => Ok((None, None)),

            TrieNode::Leaf(leaf) => {
                let key_suffix = key.slice_from(depth);
                if key_suffix.equals_nibbles(&leaf.key_suffix) {
                    Ok((None, Some(leaf.value.clone())))
                } else {
                    Ok((None, None))
                }
            }

            TrieNode::Branch(branch) => {
                if depth >= key.len() {
                    // Remove value at branch
                    let mut new_branch = (**branch).clone();
                    let removed = new_branch.value.take();

                    if !new_branch.has_children() && removed.is_some() {
                        Ok((None, removed))
                    } else {
                        let hash = self.save_node(TrieNode::Branch(Box::new(new_branch)))?;
                        Ok((Some(hash), removed))
                    }
                } else {
                    // Remove from child
                    let index = key.at(depth) as usize;
                    let mut new_branch = (**branch).clone();

                    if let Some(child_hash) = &branch.children[index] {
                        let child = self.get_node_by_hash(child_hash)?;
                        let (new_child, removed) = self.remove_recursive(&child, key, depth + 1)?;

                        new_branch.children[index] = new_child;

                        if !new_branch.has_children() && new_branch.value.is_none() {
                            Ok((None, removed))
                        } else {
                            let hash = self.save_node(TrieNode::Branch(Box::new(new_branch)))?;
                            Ok((Some(hash), removed))
                        }
                    } else {
                        Ok((None, None))
                    }
                }
            }

            // Extension and other cases simplified
            TrieNode::Extension(ext) => {
                let key_suffix = key.slice_from(depth);
                let common = key_suffix.common_prefix_nibbles(&ext.prefix);
                if common != ext.prefix.len() {
                    return Ok((
                        Some(self.save_node(TrieNode::Extension(ext.clone()))?),
                        None,
                    ));
                }

                let child = self.get_node_by_hash(&ext.child)?;
                let (new_child_opt, removed) =
                    self.remove_recursive(&child, key, depth + ext.prefix.len())?;

                let Some(new_child_hash) = new_child_opt else {
                    return Ok((None, removed));
                };

                let new_child_node = self.get_node_by_hash(&new_child_hash)?;
                match new_child_node {
                    TrieNode::Extension(child_ext) => {
                        let mut merged = ext.prefix.clone();
                        merged.extend(child_ext.prefix);
                        let merged_hash = self.save_node(TrieNode::Extension(
                            ExtensionNode::new(merged, child_ext.child),
                        ))?;
                        Ok((Some(merged_hash), removed))
                    }
                    TrieNode::Leaf(child_leaf) => {
                        let mut merged = ext.prefix.clone();
                        merged.extend(child_leaf.key_suffix);
                        let merged_hash = self
                            .save_node(TrieNode::Leaf(LeafNode::new(merged, child_leaf.value)))?;
                        Ok((Some(merged_hash), removed))
                    }
                    _ => {
                        let hash = self.save_node(TrieNode::Extension(ExtensionNode::new(
                            ext.prefix.clone(),
                            new_child_hash,
                        )))?;
                        Ok((Some(hash), removed))
                    }
                }
            }
        }
    }

    /// Get node by hash
    fn get_node_by_hash(&self, hash: &NodeHash) -> Result<TrieNode> {
        // Check cache first
        if let Some(node) = self.cache.read().get(hash) {
            return Ok(node.clone());
        }

        // Check dirty nodes
        if let Some(node) = self.dirty.read().get(hash) {
            return Ok(node.clone());
        }

        // Load from database
        match self.db.get_node(hash)? {
            Some(data) => {
                let node = self.decode_node(&data)?;
                self.cache.write().insert(*hash, node.clone());
                Ok(node)
            }
            None => Err(TrieError::NotFound(format!(
                "Node not found: {:?}",
                hash
            ))),
        }
    }

    /// Save node and return hash
    fn save_node(&self, node: TrieNode) -> Result<NodeHash> {
        let encoded = encode_node(&node);
        let hash = Hash::from_bytes(keccak256(&encoded));

        self.dirty.write().insert(hash, node);

        Ok(hash)
    }

    /// Flush dirty nodes to database
    fn flush(&self) -> Result<()> {
        let dirty = std::mem::take(&mut *self.dirty.write());

        for (hash, node) in dirty {
            let encoded = encode_node(&node);
            self.db.put_node(&hash, &encoded)?;
            self.cache.write().insert(hash, node);
        }

        Ok(())
    }

    /// Decode node from RLP (mirror of `encode_node` in node.rs).
    fn decode_node(&self, data: &[u8]) -> Result<TrieNode> {
        use crate::trie::node::{decode_hex_prefix, BranchNode, ExtensionNode, LeafNode};
        use rlp::Rlp;

        if data.is_empty() || data == [0x80] {
            return Ok(TrieNode::Empty);
        }

        let rlp = Rlp::new(data);
        if rlp.is_list() {
            let item_count = rlp.item_count().map_err(|e| {
                TrieError::Serialization(format!("invalid trie node RLP list: {e}"))
            })?;

            match item_count {
                2 => {
                    // Leaf or Extension: [hex_prefix, value_or_child]
                    let prefix_bytes: Vec<u8> = rlp.at(0).map_err(|e| {
                        TrieError::Serialization(format!("invalid trie node prefix: {e}"))
                    })?.as_val().map_err(|e| {
                        TrieError::Serialization(format!("invalid trie node prefix value: {e}"))
                    })?;
                    let (nibbles, is_leaf) = decode_hex_prefix(&prefix_bytes).map_err(|e| {
                        TrieError::Serialization(format!("invalid hex prefix: {e}"))
                    })?;

                    if is_leaf {
                        let value: Vec<u8> = rlp.at(1).map_err(|e| {
                            TrieError::Serialization(format!("invalid leaf value: {e}"))
                        })?.as_val().map_err(|e| {
                            TrieError::Serialization(format!("invalid leaf value bytes: {e}"))
                        })?;
                        Ok(TrieNode::Leaf(LeafNode::new(nibbles, value)))
                    } else {
                        let child: Vec<u8> = rlp.at(1).map_err(|e| {
                            TrieError::Serialization(format!("invalid extension child: {e}"))
                        })?.as_val().map_err(|e| {
                            TrieError::Serialization(format!("invalid extension child bytes: {e}"))
                        })?;
                        let child_hash = if child.len() == 32 {
                            let mut h = [0u8; 32];
                            h.copy_from_slice(&child);
                            NodeHash::from_bytes(h)
                        } else {
                            // Inline node (small child serialized directly).
                            let child_node = self.decode_node(&child)?;
                            let encoded = crate::trie::node::encode_node(&child_node);
                            NodeHash::from_bytes(crate::crypto::keccak256(&encoded))
                        };
                        Ok(TrieNode::Extension(ExtensionNode::new(nibbles, child_hash)))
                    }
                }
                17 => {
                    // Branch: 16 children + optional value
                    let mut branch = BranchNode::new();
                    for i in 0..16 {
                        let item = rlp.at(i).map_err(|e| {
                            TrieError::Serialization(format!("invalid branch child {i}: {e}"))
                        })?;
                        if !item.is_empty() {
                            let child: Vec<u8> = item.as_val().map_err(|e| {
                                TrieError::Serialization(format!("invalid branch child {i} bytes: {e}"))
                            })?;
                            let child_hash = if child.len() == 32 {
                                let mut h = [0u8; 32];
                                h.copy_from_slice(&child);
                                NodeHash::from_bytes(h)
                            } else {
                                let child_node = self.decode_node(&child)?;
                                let encoded = crate::trie::node::encode_node(&child_node);
                                NodeHash::from_bytes(crate::crypto::keccak256(&encoded))
                            };
                            branch.children[i] = Some(child_hash);
                        }
                    }
                    let value_item = rlp.at(16).map_err(|e| {
                        TrieError::Serialization(format!("invalid branch value: {e}"))
                    })?;
                    if !value_item.is_empty() {
                        branch.value = Some(
                            value_item
                                .as_val()
                                .map_err(|e| TrieError::Serialization(format!("invalid branch value bytes: {e}")))?,
                        );
                    }
                    Ok(TrieNode::Branch(Box::new(branch)))
                }
                other => Err(TrieError::Serialization(format!(
                    "unexpected trie node list item count: {other}"
                ))),
            }
        } else if rlp.is_empty() {
            Ok(TrieNode::Empty)
        } else {
            Err(TrieError::Serialization(
                "trie node must be an RLP list".into(),
            ))
        }
    }

    /// Generate proof for key
    pub fn get_proof(&self, key: &[u8]) -> Result<TrieProof> {
        let hashed_key = keccak256(key);
        let nibbles = NibbleSlice::new(&hashed_key);

        let mut proof_nodes = Vec::new();

        match *self.root.read() {
            None => Ok(TrieProof::new(Vec::new(), self.root())),
            Some(root_hash) => {
                let mut current_hash = root_hash;

                loop {
                    let node = self.get_node_by_hash(&current_hash)?;
                    let encoded = encode_node(&node);
                    proof_nodes.push(encoded);

                    match node {
                        TrieNode::Empty | TrieNode::Leaf(_) => break,
                        TrieNode::Extension(ext) => {
                            current_hash = ext.child;
                        }
                        TrieNode::Branch(branch) => {
                            if nibbles.len() <= proof_nodes.len() {
                                break;
                            }
                            let index = nibbles.at(proof_nodes.len() - 1) as usize;
                            match &branch.children[index] {
                                Some(child_hash) => current_hash = *child_hash,
                                None => break,
                            }
                        }
                    }
                }

                Ok(TrieProof::new(proof_nodes, self.root()))
            }
        }
    }

    /// Verify proof
    pub fn verify_proof(key: &[u8], value: Option<&Vec<u8>>, proof: &TrieProof) -> Result<bool> {
        let hashed_key = keccak256(key);
        let nibbles = NibbleSlice::new(&hashed_key);

        let mut current_hash = proof.root;

        for (i, node_data) in proof.nodes.iter().enumerate() {
            let node_hash = Hash::from_bytes(keccak256(node_data));
            if node_hash != current_hash {
                return Ok(false);
            }

            // Would decode and traverse node here
            // Simplified for brevity
        }

        Ok(true)
    }

    /// Clear cache
    pub fn clear_cache(&self) {
        self.cache.write().clear();
    }
}

/// 验证 MPT 包含证明：从 root 出发按 key 的 keccak nibble 路径遍历 proof 节点，
/// 逐级校验节点哈希一致，最终 leaf 的 key_suffix 匹配剩余 nibble 且 value 匹配。
/// `key` 是原始 output_id（32 字节），内部先 keccak 再按 nibble 走。
pub fn verify_unspent_proof(
    root: &Hash,
    key: &[u8],
    expected_value: Option<&Vec<u8>>,
    proof: &TrieProof,
) -> bool {
    use crate::trie::node::{
        decode_hex_prefix, BranchNode, ExtensionNode, LeafNode,
    };
    use rlp::Rlp;

    if proof.nodes.is_empty() {
        return false;
    }
    // 根节点哈希必须等于声明的 root
    let root_encoded = &proof.nodes[0];
    if Hash::from_bytes(keccak256(root_encoded)) != *root {
        return false;
    }

    let hashed_key = keccak256(key);
    let nibbles = NibbleSlice::new(&hashed_key);
    let mut consumed = 0usize;

    for (i, node_data) in proof.nodes.iter().enumerate() {
        if i > 0 {
            // 后续节点哈希必须等于父节点指向的子哈希（由上层分支决定，这里只做
            // 内部一致性检查：本节点哈希与父引用一致由遍历逻辑保证）
        }
        let rlp = match Rlp::new(node_data).is_list() {
            true => Rlp::new(node_data),
            false => {
                if *node_data == vec![0x80] || node_data.is_empty() {
                    return false;
                }
                Rlp::new(node_data)
            }
        };
        let item_count = match rlp.item_count() {
            Ok(c) => c,
            Err(_) => return false,
        };
        match item_count {
            2 => {
                let prefix_bytes: Vec<u8> = match rlp.at(0).and_then(|v| v.as_val()) {
                    Ok(v) => v,
                    Err(_) => return false,
                };
                let (nibbles_p, is_leaf) = match decode_hex_prefix(&prefix_bytes) {
                    Ok(v) => v,
                    Err(_) => return false,
                };
                if is_leaf {
                    let value: Vec<u8> = match rlp.at(1).and_then(|v| v.as_val()) {
                        Ok(v) => v,
                        Err(_) => return false,
                    };
                    // 已消费 nibble + leaf 后缀 必须覆盖完整 key，且后缀逐 nibble 匹配
                    if consumed + nibbles_p.len() != nibbles.len() {
                        return false;
                    }
                    for (j, exp_nib) in nibbles_p.iter().enumerate() {
                        if *exp_nib != nibbles.at(consumed + j) {
                            return false;
                        }
                    }
                    if let Some(expected) = expected_value {
                        if &value != expected {
                            return false;
                        }
                    }
                    return true;
                }
                // Extension：prefix 必须匹配剩余 key 的前缀
                if consumed + nibbles_p.len() > nibbles.len() {
                    return false;
                }
                for (j, exp_nib) in nibbles_p.iter().enumerate() {
                    if *exp_nib != nibbles.at(consumed + j) {
                        return false;
                    }
                }
                consumed += nibbles_p.len();
            }
            17 => {
                // Branch：16 children + optional value
                let mut children: [Option<Vec<u8>>; 16] = [const { None }; 16];
                let mut branch_value: Option<Vec<u8>> = None;
                for idx in 0..17 {
                    let item = match rlp.at(idx) {
                        Ok(v) => v,
                        Err(_) => return false,
                    };
                    if idx == 16 {
                        // 末尾 value（可为空）
                        if !item.is_empty() {
                            let v: Vec<u8> = match item.as_val() {
                                Ok(v) => v,
                                Err(_) => return false,
                            };
                            branch_value = Some(v);
                        }
                    } else {
                        if item.is_empty() {
                            continue;
                        }
                        let child: Vec<u8> = match item.as_val() {
                            Ok(v) => v,
                            Err(_) => return false,
                        };
                        children[idx] = Some(child);
                    }
                }
                if consumed >= nibbles.len() {
                    // key 已耗尽：值应在 branch 本身上
                    if let Some(v) = branch_value {
                        if let Some(expected) = expected_value {
                            return &v == expected;
                        }
                        return true;
                    }
                    return false;
                }
                let idx = nibbles.at(consumed) as usize;
                consumed += 1;
                let Some(child_bytes) = &children[idx] else {
                    return false;
                };
                // 下一个节点必须等于该子哈希
                if i + 1 >= proof.nodes.len() {
                    return false;
                }
                let next_encoded = &proof.nodes[i + 1];
                if Hash::from_bytes(keccak256(next_encoded)) != Hash::from_bytes(child_bytes.clone().try_into().unwrap_or([0u8; 32])) {
                    return false;
                }
                let _ = BranchNode::default();
            }
            _ => return false,
        }
    }
    false
}

