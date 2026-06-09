//! Merkle Patricia Trie implementation

use super::node::*;
use super::proof::TrieProof;
use crate::{Result, StorageError};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use rabbitcore::crypto::{keccak256, Hash};

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
        key: &NibbleSlice,
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
        key: &NibbleSlice,
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
        key: &NibbleSlice,
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
        key: &NibbleSlice,
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
        key: &NibbleSlice,
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
            None => Err(StorageError::NotFound(format!(
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

    /// Decode node from RLP
    fn decode_node(&self, data: &[u8]) -> Result<TrieNode> {
        // Simplified RLP decoding
        // In production, use full RLP decoder
        if data.is_empty() || data == [0x80] {
            return Ok(TrieNode::Empty);
        }

        // Placeholder - would implement full RLP decoding
        Err(StorageError::Serialization(
            "RLP decoding not fully implemented".into(),
        ))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trie_insert_get() {
        let db = Arc::new(MemTrieDB::new());
        let trie = MerklePatriciaTrie::new(db);

        // Insert key-value
        let key = b"test_key";
        let value = b"test_value".to_vec();

        let root = trie.insert(key, value.clone()).unwrap();
        assert!(!root.is_zero());

        // Get value
        let retrieved = trie.get(key).unwrap();
        assert_eq!(retrieved, Some(value));
    }

    #[test]
    fn test_trie_multiple_inserts() {
        let db = Arc::new(MemTrieDB::new());
        let trie = MerklePatriciaTrie::new(db);

        // Insert multiple keys
        trie.insert(b"key1", b"value1".to_vec()).unwrap();
        trie.insert(b"key2", b"value2".to_vec()).unwrap();
        trie.insert(b"key3", b"value3".to_vec()).unwrap();

        // Verify all values
        assert_eq!(trie.get(b"key1").unwrap(), Some(b"value1".to_vec()));
        assert_eq!(trie.get(b"key2").unwrap(), Some(b"value2".to_vec()));
        assert_eq!(trie.get(b"key3").unwrap(), Some(b"value3".to_vec()));
    }

    #[test]
    fn test_trie_update() {
        let db = Arc::new(MemTrieDB::new());
        let trie = MerklePatriciaTrie::new(db);

        // Insert
        trie.insert(b"key", b"value1".to_vec()).unwrap();

        // Update
        trie.insert(b"key", b"value2".to_vec()).unwrap();

        // Verify updated value
        assert_eq!(trie.get(b"key").unwrap(), Some(b"value2".to_vec()));
    }

    #[test]
    fn test_trie_remove() {
        let db = Arc::new(MemTrieDB::new());
        let trie = MerklePatriciaTrie::new(db);

        // Insert
        trie.insert(b"key", b"value".to_vec()).unwrap();

        // Remove
        let removed = trie.remove(b"key").unwrap();
        assert_eq!(removed, Some(b"value".to_vec()));

        // Verify removed
        assert_eq!(trie.get(b"key").unwrap(), None);
    }
}
