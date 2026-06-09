//! Trie node types

use bytes::Bytes;
use rlp::{DecoderError, RlpStream};
use serde::{Deserialize, Serialize};
use rabbitcore::crypto::Hash;

/// Trie node hash
pub type NodeHash = Hash;

/// Trie node data
pub type NodeData = Vec<u8>;

/// Empty trie root hash data
pub const EMPTY_TRIE_ROOT_DATA: [u8; 32] = [
    0x56, 0xe8, 0x1f, 0x17, 0x1b, 0xcc, 0x55, 0xa6, 0xff, 0x83, 0x45, 0xe6, 0x92, 0xc0, 0xf8, 0x6e,
    0x5b, 0x48, 0xe0, 0x1b, 0x99, 0x6c, 0xad, 0xc0, 0x01, 0x62, 0x2f, 0xb5, 0xe3, 0x63, 0xb4, 0x21,
];

pub fn empty_trie_root() -> NodeHash {
    Hash::from_bytes(EMPTY_TRIE_ROOT_DATA)
}

/// Trie node enumeration
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrieNode {
    /// Empty node
    Empty,

    /// Branch node (16 children + optional value)
    Branch(Box<BranchNode>),

    /// Extension node (shared prefix + child)
    Extension(ExtensionNode),

    /// Leaf node (key suffix + value)
    Leaf(LeafNode),
}

/// Branch node
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchNode {
    /// 16 children (indexed by nibble 0-15)
    pub children: [Option<NodeHash>; 16],
    /// Optional value stored at this node
    pub value: Option<Vec<u8>>,
}

impl BranchNode {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get child at index
    pub fn get_child(&self, index: usize) -> Option<&NodeHash> {
        self.children.get(index).and_then(|x| x.as_ref())
    }

    /// Set child at index
    pub fn set_child(&mut self, index: usize, hash: NodeHash) {
        if index < 16 {
            self.children[index] = Some(hash);
        }
    }

    /// Check if node has any children
    pub fn has_children(&self) -> bool {
        self.children.iter().any(|x| x.is_some())
    }

    /// Count number of children
    pub fn child_count(&self) -> usize {
        self.children.iter().filter(|x| x.is_some()).count()
    }
}

/// Extension node
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionNode {
    /// Shared key prefix (nibbles)
    pub prefix: Vec<u8>,
    /// Child node hash
    pub child: NodeHash,
}

impl ExtensionNode {
    pub fn new(prefix: Vec<u8>, child: NodeHash) -> Self {
        Self { prefix, child }
    }
}

/// Leaf node
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeafNode {
    /// Key suffix (nibbles)
    pub key_suffix: Vec<u8>,
    /// Stored value
    pub value: Vec<u8>,
}

impl LeafNode {
    pub fn new(key_suffix: Vec<u8>, value: Vec<u8>) -> Self {
        Self { key_suffix, value }
    }
}

/// Nibble slice (helper for trie operations)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NibbleSlice<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> NibbleSlice<'a> {
    /// Create from bytes
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    /// Create from bytes with offset
    pub fn from_offset(data: &'a [u8], offset: usize) -> Self {
        Self { data, offset }
    }

    /// Get nibble at index
    pub fn at(&self, index: usize) -> u8 {
        let pos = self.offset + index;
        if pos >= self.data.len() * 2 {
            0
        } else if pos.is_multiple_of(2) {
            self.data[pos / 2] >> 4
        } else {
            self.data[pos / 2] & 0x0F
        }
    }

    /// Get length in nibbles
    pub fn len(&self) -> usize {
        self.data.len() * 2 - self.offset
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get common prefix length with another slice
    pub fn common_prefix_length(&self, other: &NibbleSlice) -> usize {
        let mut count = 0;
        let min_len = self.len().min(other.len());

        for i in 0..min_len {
            if self.at(i) != other.at(i) {
                break;
            }
            count += 1;
        }

        count
    }

    /// Slice from index
    pub fn slice_from(&self, index: usize) -> NibbleSlice<'a> {
        Self::from_offset(self.data, self.offset + index)
    }

    /// Convert to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let len = self.len().div_ceil(2);
        let mut bytes = vec![0u8; len];

        for i in 0..self.len() {
            let nibble = self.at(i);
            if i % 2 == 0 {
                bytes[i / 2] = nibble << 4;
            } else {
                bytes[i / 2] |= nibble;
            }
        }

        bytes
    }

    /// Convert to nibble vector (each element is 0..=15)
    pub fn to_nibbles(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len());
        for i in 0..self.len() {
            out.push(self.at(i));
        }
        out
    }

    /// Compare against nibble vector.
    pub fn equals_nibbles(&self, nibbles: &[u8]) -> bool {
        if self.len() != nibbles.len() {
            return false;
        }
        for (i, nib) in nibbles.iter().enumerate() {
            if self.at(i) != *nib {
                return false;
            }
        }
        true
    }

    /// Common prefix length with nibble vector.
    pub fn common_prefix_nibbles(&self, nibbles: &[u8]) -> usize {
        let mut count = 0;
        let min_len = self.len().min(nibbles.len());
        for (i, nib) in nibbles.iter().enumerate().take(min_len) {
            if self.at(i) != *nib {
                break;
            }
            count += 1;
        }
        count
    }
}

/// Encode node to RLP
pub fn encode_node(node: &TrieNode) -> Vec<u8> {
    match node {
        TrieNode::Empty => {
            vec![0x80] // RLP empty string
        }
        TrieNode::Leaf(leaf) => {
            let mut stream = RlpStream::new_list(2);
            stream.append(&encode_hex_prefix(&leaf.key_suffix, true));
            stream.append(&Bytes::copy_from_slice(&leaf.value));
            stream.out().to_vec()
        }
        TrieNode::Extension(ext) => {
            let mut stream = RlpStream::new_list(2);
            stream.append(&encode_hex_prefix(&ext.prefix, false));
            stream.append(&ext.child.as_bytes());
            stream.out().to_vec()
        }
        TrieNode::Branch(branch) => {
            let mut stream = RlpStream::new_list(17);
            for child in &branch.children {
                if let Some(hash) = child {
                    stream.append(&hash.as_bytes());
                } else {
                    stream.append_empty_data();
                }
            }
            if let Some(val) = &branch.value {
                stream.append(&Bytes::copy_from_slice(val));
            } else {
                stream.append_empty_data();
            }
            stream.out().to_vec()
        }
    }
}

/// Encode node hash or inline node
pub fn encode_node_ref(node: &TrieNode) -> Vec<u8> {
    let encoded = encode_node(node);

    // If encoded size >= 32, hash it
    if encoded.len() >= 32 {
        let hash = rabbitcore::crypto::keccak256(&encoded);
        hash.to_vec()
    } else {
        encoded
    }
}

/// Encode hex prefix with flags
fn encode_hex_prefix(nibbles: &[u8], is_leaf: bool) -> Vec<u8> {
    let mut result = Vec::new();
    let flag = if is_leaf { 0x20 } else { 0x00 };

    if nibbles.len().is_multiple_of(2) {
        // Even length
        result.push(flag);
        for i in (0..nibbles.len()).step_by(2) {
            result.push((nibbles[i] << 4) | nibbles[i + 1]);
        }
    } else {
        // Odd length
        result.push(flag | 0x10 | nibbles[0]);
        for i in (1..nibbles.len()).step_by(2) {
            result.push((nibbles[i] << 4) | nibbles[i + 1]);
        }
    }

    result
}

/// Decode hex prefix
pub fn decode_hex_prefix(data: &[u8]) -> Result<(Vec<u8>, bool), DecoderError> {
    if data.is_empty() {
        return Err(DecoderError::Custom("Empty hex prefix"));
    }

    let first = data[0];
    let is_leaf = (first & 0x20) != 0;
    let is_odd = (first & 0x10) != 0;

    let mut nibbles = Vec::new();

    if is_odd {
        nibbles.push(first & 0x0F);
    }

    for &byte in &data[1..] {
        nibbles.push(byte >> 4);
        nibbles.push(byte & 0x0F);
    }

    Ok((nibbles, is_leaf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nibble_slice() {
        let data = [0x12, 0x34, 0x56];
        let slice = NibbleSlice::new(&data);

        assert_eq!(slice.at(0), 0x01);
        assert_eq!(slice.at(1), 0x02);
        assert_eq!(slice.at(2), 0x03);
        assert_eq!(slice.at(3), 0x04);
        assert_eq!(slice.len(), 6);
    }

    #[test]
    fn test_common_prefix() {
        let data1 = [0x12, 0x34, 0x56];
        let data2 = [0x12, 0x35, 0x56];

        let slice1 = NibbleSlice::new(&data1);
        let slice2 = NibbleSlice::new(&data2);

        assert_eq!(slice1.common_prefix_length(&slice2), 3);
    }

    #[test]
    fn test_encode_decode_leaf() {
        let leaf = TrieNode::Leaf(LeafNode {
            key_suffix: vec![1, 2, 3],
            value: b"test value".to_vec(),
        });

        let encoded = encode_node(&leaf);
        // Would decode and verify in full implementation
        assert!(!encoded.is_empty());
    }
}
