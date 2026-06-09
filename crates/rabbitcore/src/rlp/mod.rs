//! RLP (Recursive Length Prefix) Encoding/Decoding
//! 
//! Complete implementation for deterministic recursive length-prefix serialization

mod encode;
mod decode;
mod stream;

pub use encode::*;
pub use decode::*;
pub use stream::*;

use thiserror::Error;

/// RLP error types
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum RlpError {
    #[error("Invalid RLP prefix")]
    InvalidPrefix,
    #[error("Invalid length")]
    InvalidLength,
    #[error("Data too short")]
    DataTooShort,
    #[error("Overflow")]
    Overflow,
    #[error("Unexpected list")]
    UnexpectedList,
    #[error("Unexpected string")]
    UnexpectedString,
    #[error("Invalid internal string length")]
    InvalidInternalStringLength,
}

pub type Result<T> = std::result::Result<T, RlpError>;

/// RLP encode trait
pub trait RlpEncode {
    fn rlp_encode(&self) -> Vec<u8>;
}

/// RLP decode trait
pub trait RlpDecode: Sized {
    fn rlp_decode(bytes: &[u8]) -> Result<Self>;
}

// ============ Basic Type Implementations ============

impl RlpEncode for u8 {
    fn rlp_encode(&self) -> Vec<u8> {
        if *self == 0 {
            vec![0x80]
        } else if *self < 0x80 {
            vec![*self]
        } else {
            vec![0x81, *self]
        }
    }
}

impl RlpDecode for u8 {
    fn rlp_decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            return Err(RlpError::DataTooShort);
        }
        
        if bytes[0] < 0x80 {
            Ok(bytes[0])
        } else if bytes[0] == 0x81 {
            if bytes.len() < 2 {
                return Err(RlpError::DataTooShort);
            }
            Ok(bytes[1])
        } else {
            Err(RlpError::InvalidPrefix)
        }
    }
}

impl RlpEncode for u64 {
    fn rlp_encode(&self) -> Vec<u8> {
        if *self == 0 {
            return vec![0x80];
        }
        
        let mut bytes = Vec::new();
        let mut n = *self;
        
        while n > 0 {
            bytes.push((n & 0xFF) as u8);
            n >>= 8;
        }
        
        bytes.reverse();
        
        if bytes.len() == 1 && bytes[0] < 0x80 {
            bytes
        } else {
            let mut result = Vec::with_capacity(bytes.len() + 1);
            result.push(0x80 + bytes.len() as u8);
            result.extend_from_slice(&bytes);
            result
        }
    }
}

impl RlpDecode for u64 {
    fn rlp_decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            return Err(RlpError::DataTooShort);
        }
        
        if bytes[0] < 0x80 {
            Ok(bytes[0] as u64)
        } else if bytes[0] >= 0x80 && bytes[0] <= 0xb7 {
            let len = (bytes[0] - 0x80) as usize;
            if bytes.len() < 1 + len {
                return Err(RlpError::DataTooShort);
            }
            
            if len > 8 {
                return Err(RlpError::Overflow);
            }
            
            let mut value = 0u64;
            for &byte in &bytes[1..1 + len] {
                value = (value << 8) | byte as u64;
            }
            
            Ok(value)
        } else {
            Err(RlpError::InvalidPrefix)
        }
    }
}

impl RlpEncode for usize {
    fn rlp_encode(&self) -> Vec<u8> {
        (*self as u64).rlp_encode()
    }
}

impl RlpDecode for usize {
    fn rlp_decode(bytes: &[u8]) -> Result<Self> {
        u64::rlp_decode(bytes).map(|v| v as usize)
    }
}

impl RlpEncode for &[u8] {
    fn rlp_encode(&self) -> Vec<u8> {
        if self.is_empty() {
            return vec![0x80];
        }
        
        if self.len() == 1 && self[0] < 0x80 {
            return self.to_vec();
        }
        
        let mut result = Vec::with_capacity(self.len() + 4);
        
        if self.len() <= 55 {
            result.push(0x80 + self.len() as u8);
        } else {
            let len_bytes = length_to_bytes(self.len());
            result.push(0xb7 + len_bytes.len() as u8);
            result.extend_from_slice(&len_bytes);
        }
        
        result.extend_from_slice(self);
        result
    }
}

impl RlpEncode for Vec<u8> {
    fn rlp_encode(&self) -> Vec<u8> {
        self.as_slice().rlp_encode()
    }
}

impl RlpDecode for Vec<u8> {
    fn rlp_decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            return Err(RlpError::DataTooShort);
        }
        
        if bytes[0] < 0x80 {
            Ok(vec![bytes[0]])
        } else if bytes[0] >= 0x80 && bytes[0] <= 0xb7 {
            let len = (bytes[0] - 0x80) as usize;
            if bytes.len() < 1 + len {
                return Err(RlpError::DataTooShort);
            }
            Ok(bytes[1..1 + len].to_vec())
        } else if bytes[0] > 0xb7 && bytes[0] <= 0xbf {
            let len_of_len = (bytes[0] - 0xb7) as usize;
            if bytes.len() < 1 + len_of_len {
                return Err(RlpError::DataTooShort);
            }
            
            let data_len = bytes_to_length(&bytes[1..1 + len_of_len]);
            
            if bytes.len() < 1 + len_of_len + data_len {
                return Err(RlpError::DataTooShort);
            }
            
            Ok(bytes[1 + len_of_len..1 + len_of_len + data_len].to_vec())
        } else {
            Err(RlpError::InvalidPrefix)
        }
    }
}

impl RlpEncode for String {
    fn rlp_encode(&self) -> Vec<u8> {
        self.as_bytes().rlp_encode()
    }
}

impl RlpDecode for String {
    fn rlp_decode(bytes: &[u8]) -> Result<Self> {
        let vec = Vec::<u8>::rlp_decode(bytes)?;
        String::from_utf8(vec).map_err(|_| RlpError::InvalidPrefix)
    }
}

impl<T: RlpEncode> RlpEncode for Option<T> {
    fn rlp_encode(&self) -> Vec<u8> {
        match self {
            Some(v) => v.rlp_encode(),
            None => vec![0xc0],  // Empty list
        }
    }
}

impl<T: RlpDecode> RlpDecode for Option<T> {
    fn rlp_decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            return Ok(None);
        }
        
        if bytes[0] == 0xc0 {
            Ok(None)
        } else {
            T::rlp_decode(bytes).map(Some)
        }
    }
}

impl<T: RlpEncode> RlpEncode for Vec<T> {
    fn rlp_encode(&self) -> Vec<u8> {
        let mut stream = RlpStream::new();
        stream.begin_list(self.len());
        for item in self {
            stream.append(item);
        }
        stream.out()
    }
}

impl<T: RlpDecode> RlpDecode for Vec<T> {
    fn rlp_decode(bytes: &[u8]) -> Result<Self> {
        let mut stream = RlpStream::new();
        stream.append(bytes);
        
        let mut decoder = stream.into_decoder();
        let mut result = Vec::new();
        
        while let Some(item) = decoder.decode_next()? {
            result.push(item);
        }
        
        Ok(result)
    }
}

// ============ Helper Functions ============

fn length_to_bytes(len: usize) -> Vec<u8> {
    let mut n = len as u64;
    let mut bytes = Vec::new();
    
    while n > 0 {
        bytes.push((n & 0xFF) as u8);
        n >>= 8;
    }
    
    bytes.reverse();
    
    // Remove leading zeros
    while bytes.len() > 1 && bytes[0] == 0 {
        bytes.remove(0);
    }
    
    bytes
}

fn bytes_to_length(bytes: &[u8]) -> usize {
    let mut value = 0usize;
    for &byte in bytes {
        value = (value << 8) | byte as usize;
    }
    value
}

// ============ Tests ============

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_encode_decode_u8() {
        let values = vec![0, 1, 127, 128, 255];
        
        for value in values {
            let encoded = value.rlp_encode();
            let decoded = u8::rlp_decode(&encoded).unwrap();
            assert_eq!(value, decoded);
        }
    }
    
    #[test]
    fn test_encode_decode_u64() {
        let values = vec![
            0,
            1,
            127,
            128,
            255,
            256,
            1000,
            u64::MAX,
        ];
        
        for value in values {
            let encoded = value.rlp_encode();
            let decoded = u64::rlp_decode(&encoded).unwrap();
            assert_eq!(value, decoded);
        }
    }
    
    #[test]
    fn test_encode_decode_bytes() {
        let values = vec![
            vec![],
            vec![0],
            vec![127],
            vec![128],
            vec![1, 2, 3, 4, 5],
            vec![0u8; 100],
        ];
        
        for value in values {
            let encoded = value.rlp_encode();
            let decoded = Vec::<u8>::rlp_decode(&encoded).unwrap();
            assert_eq!(value, decoded);
        }
    }
    
    #[test]
    fn test_encode_decode_string() {
        let values = vec![
            "".to_string(),
            "hello".to_string(),
            "Hello, World!".to_string(),
        ];
        
        for value in values {
            let encoded = value.rlp_encode();
            let decoded = String::rlp_decode(&encoded).unwrap();
            assert_eq!(value, decoded);
        }
    }
    
    #[test]
    fn test_encode_list() {
        let list = vec![1u64, 2, 3, 4, 5];
        let encoded = list.rlp_encode();
        
        // Should be a list
        assert!(encoded[0] >= 0xc0);
        
        let decoded = Vec::<u64>::rlp_decode(&encoded).unwrap();
        assert_eq!(list, decoded);
    }
    
    #[test]
    fn test_known_vectors() {
        // Test vectors from the canonical RLP specification
        
        // "" -> 0x80
        let encoded = "".rlp_encode();
        assert_eq!(encoded, vec![0x80]);
        
        // "dog" -> 0x83 + "dog"
        let encoded = "dog".rlp_encode();
        assert_eq!(encoded, vec![0x83, b'd', b'o', b'g']);
        
        // ["cat", "dog"] -> 0xc8 + 0x83 + "cat" + 0x83 + "dog"
        let list = vec!["cat".to_string(), "dog".to_string()];
        let encoded = list.rlp_encode();
        assert_eq!(encoded[0], 0xc8);
    }
}
