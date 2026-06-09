//! RLP Decoding utilities

use super::{Result, RlpDecode, RlpError};

/// Decode a single value
pub fn decode<T: RlpDecode>(bytes: &[u8]) -> Result<T> {
    T::rlp_decode(bytes)
}

/// Decode a list of values
pub fn decode_list<T: RlpDecode>(bytes: &[u8]) -> Result<Vec<T>> {
    T::rlp_decode(bytes)
}

/// Decode raw bytes
pub fn decode_bytes(bytes: &[u8]) -> Result<Vec<u8>> {
    Vec::<u8>::rlp_decode(bytes)
}

/// Decode a string
pub fn decode_string(bytes: &[u8]) -> Result<String> {
    String::rlp_decode(bytes)
}

/// Decode a u64
pub fn decode_u64(bytes: &[u8]) -> Result<u64> {
    u64::rlp_decode(bytes)
}

/// Decode U256
pub fn decode_u256(bytes: &[u8]) -> Result<[u8; 32]> {
    let data = Vec::<u8>::rlp_decode(bytes)?;

    if data.is_empty() || (data.len() == 1 && data[0] == 0) {
        return Ok([0u8; 32]);
    }

    if data.len() > 32 {
        return Err(RlpError::Overflow);
    }

    let mut result = [0u8; 32];
    result[32 - data.len()..].copy_from_slice(&data);

    Ok(result)
}

/// Get RLP item length
pub fn get_item_length(bytes: &[u8]) -> Result<usize> {
    if bytes.is_empty() {
        return Err(RlpError::DataTooShort);
    }

    let prefix = bytes[0];

    if prefix < 0x80 {
        Ok(1)
    } else if prefix <= 0xb7 {
        let len = (prefix - 0x80) as usize;
        Ok(1 + len)
    } else if prefix <= 0xbf {
        let len_of_len = (prefix - 0xb7) as usize;
        if bytes.len() < 1 + len_of_len {
            return Err(RlpError::DataTooShort);
        }

        let data_len = super::bytes_to_length(&bytes[1..1 + len_of_len]);
        Ok(1 + len_of_len + data_len)
    } else if prefix <= 0xf7 {
        let len = (prefix - 0xc0) as usize;
        Ok(1 + len)
    } else {
        let len_of_len = (prefix - 0xf7) as usize;
        if bytes.len() < 1 + len_of_len {
            return Err(RlpError::DataTooShort);
        }

        let data_len = super::bytes_to_length(&bytes[1..1 + len_of_len]);
        Ok(1 + len_of_len + data_len)
    }
}

/// Check if bytes encode a list
pub fn is_list(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes[0] >= 0xc0
}

/// Check if bytes encode a string
pub fn is_string(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes[0] < 0xc0
}

/// Get list length
pub fn get_list_length(bytes: &[u8]) -> Result<usize> {
    if bytes.is_empty() {
        return Err(RlpError::DataTooShort);
    }

    let prefix = bytes[0];

    if prefix < 0xc0 {
        return Err(RlpError::UnexpectedString);
    }

    if prefix <= 0xf7 {
        Ok((prefix - 0xc0) as usize)
    } else {
        let len_of_len = (prefix - 0xf7) as usize;
        if bytes.len() < 1 + len_of_len {
            return Err(RlpError::DataTooShort);
        }

        Ok(super::bytes_to_length(&bytes[1..1 + len_of_len]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_u256() {
        // all-zero
        let decoded = decode_u256(&[0x80]).unwrap();
        assert_eq!(decoded, [0u8; 32]);

        // One
        let decoded = decode_u256(&[0x01]).unwrap();
        let mut expected = [0u8; 32];
        expected[31] = 1;
        assert_eq!(decoded, expected);
    }

    #[test]
    fn test_is_list() {
        assert!(is_list(&[0xc0])); // Empty list
        assert!(is_list(&[
            0xc8, 0x83, b'c', b'a', b't', 0x83, b'd', b'o', b'g'
        ]));
        assert!(!is_list(&[0x83, b'd', b'o', b'g']));
    }

    #[test]
    fn test_get_list_length() {
        let list = &[0xc8, 0x83, b'c', b'a', b't', 0x83, b'd', b'o', b'g'];
        let len = get_list_length(list).unwrap();
        assert_eq!(len, 2); // Two items: "cat" and "dog"
    }
}
