//! RLP Stream for building encoded data

use super::encode::RlpEncode;

/// RLP stream builder
pub struct RlpStream {
    buffer: Vec<u8>,
    list_stack: Vec<ListInfo>,
}

struct ListInfo {
    start_pos: usize,
    list_len: usize, // Number of items in the list
}

impl RlpStream {
    /// Create new stream
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(256),
            list_stack: Vec::new(),
        }
    }

    /// Create stream with capacity
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buffer: Vec::with_capacity(capacity),
            list_stack: Vec::new(),
        }
    }

    /// Begin a list
    pub fn begin_list(&mut self, len: usize) -> &mut Self {
        self.list_stack.push(ListInfo {
            start_pos: self.buffer.len(),
            list_len: len,
        });

        // Reserve space for length prefix
        self.buffer.push(0); // Placeholder

        self
    }

    /// Append an item
    pub fn append<T: RlpEncode>(&mut self, item: &T) -> &mut Self {
        let encoded = item.rlp_encode();
        self.buffer.extend_from_slice(&encoded);

        // Update list length if in a list
        if let Some(info) = self.list_stack.last_mut() {
            info.list_len += 1;
        }

        self
    }

    /// Append raw bytes
    pub fn append_raw(&mut self, bytes: &[u8]) -> &mut Self {
        self.buffer.extend_from_slice(bytes);
        self
    }

    /// Append a list of items
    pub fn append_list<T: RlpEncode, I: IntoIterator<Item = T>>(&mut self, items: I) -> &mut Self {
        self.begin_list(0); // Will be calculated
        for item in items {
            self.append(&item);
        }
        self.finalize_list();
        self
    }

    /// Finalize current list
    fn finalize_list(&mut self) {
        if let Some(info) = self.list_stack.pop() {
            let list_content_len = self.buffer.len() - info.start_pos - 1;

            // Update the prefix
            if list_content_len <= 55 {
                self.buffer[info.start_pos] = 0xc0 + list_content_len as u8;
            } else {
                // Need to shift and insert length bytes
                let len_bytes = length_to_bytes(list_content_len);
                let prefix_byte = 0xf7 + len_bytes.len() as u8;

                // Remove placeholder
                self.buffer.remove(info.start_pos);

                // Insert prefix and length
                let mut new_bytes = Vec::with_capacity(1 + len_bytes.len());
                new_bytes.push(prefix_byte);
                new_bytes.extend_from_slice(&len_bytes);

                for (i, byte) in new_bytes.into_iter().enumerate() {
                    self.buffer.insert(info.start_pos + i, byte);
                }
            }
        }
    }

    /// Get encoded output
    pub fn out(self) -> Vec<u8> {
        // Finalize any remaining lists
        let mut stream = self;
        while !stream.list_stack.is_empty() {
            stream.finalize_list();
        }

        stream.buffer
    }

    /// Get current length
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Convert into decoder
    pub fn into_decoder(self) -> RlpDecoder {
        RlpDecoder::new(self.buffer)
    }
}

impl Default for RlpStream {
    fn default() -> Self {
        Self::new()
    }
}

/// RLP decoder
pub struct RlpDecoder {
    data: Vec<u8>,
    position: usize,
}

impl RlpDecoder {
    /// Create new decoder
    pub fn new(data: Vec<u8>) -> Self {
        Self { data, position: 0 }
    }

    /// Check if more data available
    pub fn has_more(&self) -> bool {
        self.position < self.data.len()
    }

    /// Decode next item
    pub fn decode_next<T: super::RlpDecode>(&mut self) -> super::Result<Option<T>> {
        if self.position >= self.data.len() {
            return Ok(None);
        }

        let remaining = &self.data[self.position..];
        let decoded = T::rlp_decode(remaining)?;

        // Calculate how many bytes were consumed
        let consumed = calculate_rlp_length(remaining)?;
        self.position += consumed;

        Ok(Some(decoded))
    }

    /// Decode all items
    pub fn decode_all<T: super::RlpDecode>(&mut self) -> super::Result<Vec<T>> {
        let mut items = Vec::new();

        while let Some(item) = self.decode_next()? {
            items.push(item);
        }

        Ok(items)
    }
}

fn calculate_rlp_length(data: &[u8]) -> super::Result<usize> {
    if data.is_empty() {
        return Err(super::RlpError::DataTooShort);
    }

    let prefix = data[0];

    if prefix < 0x80 {
        // Single byte
        Ok(1)
    } else if prefix <= 0xb7 {
        // Short string
        let len = (prefix - 0x80) as usize;
        Ok(1 + len)
    } else if prefix <= 0xbf {
        // Long string
        let len_of_len = (prefix - 0xb7) as usize;
        if data.len() < 1 + len_of_len {
            return Err(super::RlpError::DataTooShort);
        }

        let data_len = super::bytes_to_length(&data[1..1 + len_of_len]);
        Ok(1 + len_of_len + data_len)
    } else if prefix <= 0xf7 {
        // Short list
        let len = (prefix - 0xc0) as usize;
        Ok(1 + len)
    } else {
        // Long list
        let len_of_len = (prefix - 0xf7) as usize;
        if data.len() < 1 + len_of_len {
            return Err(super::RlpError::DataTooShort);
        }

        let data_len = super::bytes_to_length(&data[1..1 + len_of_len]);
        Ok(1 + len_of_len + data_len)
    }
}

fn length_to_bytes(len: usize) -> Vec<u8> {
    let mut n = len as u64;
    let mut bytes = Vec::new();

    while n > 0 {
        bytes.push((n & 0xFF) as u8);
        n >>= 8;
    }

    bytes.reverse();
    while bytes.len() > 1 && bytes[0] == 0 {
        bytes.remove(0);
    }

    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_basic() {
        let mut stream = RlpStream::new();
        stream.append(&1u64);
        stream.append(&2u64);
        stream.append(&3u64);

        let encoded = stream.out();
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_stream_list() {
        let mut stream = RlpStream::new();
        stream.begin_list(3);
        stream.append(&1u64);
        stream.append(&2u64);
        stream.append(&3u64);

        let encoded = stream.out();

        // First byte should indicate a list
        assert!(encoded[0] >= 0xc0);
    }

    #[test]
    fn test_decoder() {
        let mut stream = RlpStream::new();
        stream.append(&1u64);
        stream.append(&2u64);
        stream.append(&3u64);

        let encoded = stream.out();
        let mut decoder = RlpDecoder::new(encoded);

        let items: Vec<u64> = decoder.decode_all().unwrap();
        assert_eq!(items, vec![1, 2, 3]);
    }
}
