//! RLP Encoding utilities

use super::RlpEncode;

/// Encode a single value
pub fn encode<T: RlpEncode>(value: &T) -> Vec<u8> {
    value.rlp_encode()
}

/// Encode a list of values
pub fn encode_list<T: RlpEncode>(items: &[T]) -> Vec<u8> {
    let mut stream = super::stream::RlpStream::new();
    stream.begin_list(items.len());
    for item in items {
        stream.append(item);
    }
    stream.out()
}

/// Encode raw bytes
pub fn encode_bytes(bytes: &[u8]) -> Vec<u8> {
    bytes.rlp_encode()
}

/// Encode a string
pub fn encode_string(s: &str) -> Vec<u8> {
    s.rlp_encode()
}

/// Encode a u64
pub fn encode_u64(value: u64) -> Vec<u8> {
    value.rlp_encode()
}

/// Encode an address (20 bytes)
pub fn encode_address(address: &[u8]) -> Vec<u8> {
    address.rlp_encode()
}

/// Encode a hash (32 bytes)
pub fn encode_hash(hash: &[u8]) -> Vec<u8> {
    hash.rlp_encode()
}

/// Encode U256
pub fn encode_u256(value: &[u8; 32]) -> Vec<u8> {
    // Remove leading zeros
    let mut start = 0;
    while start < 31 && value[start] == 0 {
        start += 1;
    }

    if start == 31 && value[31] == 0 {
        return vec![0x80]; // all-zero
    }

    if 32 - start == 1 && value[31] < 0x80 {
        return vec![value[31]];
    }

    let mut result = Vec::with_capacity(1 + (32 - start));
    result.push(0x80 + (32 - start) as u8);
    result.extend_from_slice(&value[start..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_u256() {
        // all-zero
        let zero = [0u8; 32];
        let encoded = encode_u256(&zero);
        assert_eq!(encoded, vec![0x80]);

        // One
        let mut one = [0u8; 32];
        one[31] = 1;
        let encoded = encode_u256(&one);
        assert_eq!(encoded, vec![0x01]);

        // 256
        let mut val = [0u8; 32];
        val[30] = 1;
        let encoded = encode_u256(&val);
        assert_eq!(encoded, vec![0x82, 0x01, 0x00]);
    }
}
