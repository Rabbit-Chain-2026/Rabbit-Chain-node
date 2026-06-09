//! Compressed archive segments for finalized history outside the hot consensus state.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use rabbitcore::block::Block;
use rabbitcore::compute::{primitives::TxId, ObjectOutput};
use rabbitcore::crypto::Hash;

use crate::{Result, StorageError};

const ARCHIVE_SEGMENT_MAGIC: [u8; 4] = *b"ZAS1";
const ARCHIVE_SEGMENT_VERSION: u16 = 1;

/// A block archived with fields that `Block` serialization intentionally skips.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArchivedBlock {
    /// Canonical height.
    pub height: u64,
    /// Canonical block hash.
    pub hash: Hash,
    /// Serialized block body/header.
    pub block: Block,
}

impl ArchivedBlock {
    /// Builds an archive block wrapper from a canonical block.
    pub fn from_block(block: Block) -> Self {
        Self {
            height: block.header.number.as_u64(),
            hash: block.header.hash,
            block,
        }
    }
}

/// A persisted compute tx result archived outside hot KV storage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedComputeTxResult {
    /// Compute tx id.
    pub tx_id: TxId,
    /// JSON result returned by RPC.
    pub result_json: String,
}

/// Finalized block and compute history segment.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArchiveSegment {
    /// Segment codec version.
    pub version: u16,
    /// Inclusive first block height.
    pub from_height: u64,
    /// Inclusive last block height.
    pub to_height: u64,
    /// Creation unix timestamp.
    pub created_at_unix: u64,
    /// Finalized canonical blocks in this range.
    pub blocks: Vec<ArchivedBlock>,
    /// Historical compute outputs, usually spent/finalized outputs.
    pub compute_outputs: Vec<ObjectOutput>,
    /// Historical compute tx results.
    pub compute_tx_results: Vec<ArchivedComputeTxResult>,
}

impl ArchiveSegment {
    /// Creates a new archive segment with the current codec version.
    pub fn new(from_height: u64, to_height: u64, created_at_unix: u64) -> Self {
        Self {
            version: ARCHIVE_SEGMENT_VERSION,
            from_height,
            to_height,
            created_at_unix,
            blocks: Vec::new(),
            compute_outputs: Vec::new(),
            compute_tx_results: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ArchiveSegmentFile {
    magic: [u8; 4],
    segment: ArchiveSegment,
}

/// Writes archive segments as atomic ZSTD-compressed files.
pub struct ArchiveSegmentWriter {
    dir: PathBuf,
    compression_level: i32,
}

impl ArchiveSegmentWriter {
    /// Creates a writer for archive segment files.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            compression_level: 3,
        }
    }

    /// Overrides the ZSTD compression level.
    pub fn with_compression_level(mut self, compression_level: i32) -> Self {
        self.compression_level = compression_level;
        self
    }

    /// Writes a segment and returns the final path.
    pub fn write_segment(&self, segment: &ArchiveSegment) -> Result<PathBuf> {
        validate_segment(segment)?;
        fs::create_dir_all(&self.dir)?;

        let final_path = self.segment_path(segment.from_height, segment.to_height);
        let tmp_path = final_path.with_extension("zst.tmp");
        let file = ArchiveSegmentFile {
            magic: ARCHIVE_SEGMENT_MAGIC,
            segment: segment.clone(),
        };
        let encoded = bincode::serialize(&file)
            .map_err(|e| StorageError::Serialization(format!("encode archive segment: {e}")))?;
        let compressed = zstd::stream::encode_all(Cursor::new(encoded), self.compression_level)
            .map_err(|e| StorageError::Serialization(format!("compress archive segment: {e}")))?;

        fs::write(&tmp_path, compressed)?;
        fs::rename(&tmp_path, &final_path)?;
        Ok(final_path)
    }

    fn segment_path(&self, from_height: u64, to_height: u64) -> PathBuf {
        self.dir
            .join(format!("archive-{from_height:020}-{to_height:020}.zst"))
    }
}

/// Reads and validates an archive segment file.
pub fn read_archive_segment(path: impl AsRef<Path>) -> Result<ArchiveSegment> {
    let compressed = fs::read(path)?;
    let encoded = zstd::stream::decode_all(Cursor::new(compressed))
        .map_err(|e| StorageError::Serialization(format!("decompress archive segment: {e}")))?;
    let file = bincode::deserialize::<ArchiveSegmentFile>(&encoded)
        .map_err(|e| StorageError::Serialization(format!("decode archive segment: {e}")))?;
    if file.magic != ARCHIVE_SEGMENT_MAGIC {
        return Err(StorageError::Serialization(
            "invalid archive segment magic".to_string(),
        ));
    }
    validate_segment(&file.segment)?;
    Ok(file.segment)
}

fn validate_segment(segment: &ArchiveSegment) -> Result<()> {
    if segment.version != ARCHIVE_SEGMENT_VERSION {
        return Err(StorageError::Serialization(format!(
            "unsupported archive segment version: {}",
            segment.version
        )));
    }
    if segment.from_height > segment.to_height {
        return Err(StorageError::Serialization(format!(
            "invalid archive height range: {}..{}",
            segment.from_height, segment.to_height
        )));
    }
    for block in &segment.blocks {
        if block.height < segment.from_height || block.height > segment.to_height {
            return Err(StorageError::Serialization(format!(
                "archived block height {} outside segment range {}..{}",
                block.height, segment.from_height, segment.to_height
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;
    use rabbitcore::block::create_genesis_block;
    use rabbitcore::compute::{
        object::{ObjectKind, Ownership, Script},
        primitives::{DomainId, ObjectId, OutputId, Version},
    };

    use super::*;

    #[test]
    fn archive_segment_roundtrip() {
        let dir = tempdir().expect("create temp dir");
        let writer = ArchiveSegmentWriter::new(dir.path());
        let block = create_genesis_block();
        let mut segment = ArchiveSegment::new(0, 10, 123);
        segment
            .blocks
            .push(ArchivedBlock::from_block(block.clone()));
        segment.compute_outputs.push(sample_output());
        segment.compute_tx_results.push(ArchivedComputeTxResult {
            tx_id: TxId(Hash::from_bytes([3; 32])),
            result_json: r#"{"ok":true}"#.to_string(),
        });

        let path = writer.write_segment(&segment).expect("write segment");
        assert!(path.exists());
        let decoded = read_archive_segment(&path).expect("read segment");

        assert_eq!(decoded.version, 1);
        assert_eq!(decoded.from_height, 0);
        assert_eq!(decoded.to_height, 10);
        assert_eq!(decoded.blocks.len(), 1);
        assert_eq!(decoded.blocks[0].hash, block.header.hash);
        assert_eq!(decoded.compute_outputs, segment.compute_outputs);
        assert_eq!(decoded.compute_tx_results, segment.compute_tx_results);
    }

    #[test]
    fn archive_segment_rejects_invalid_height_range() {
        let dir = tempdir().expect("create temp dir");
        let writer = ArchiveSegmentWriter::new(dir.path());
        let segment = ArchiveSegment::new(10, 9, 123);

        let err = writer.write_segment(&segment).expect_err("invalid range");
        assert!(err.to_string().contains("invalid archive height range"));
    }

    fn sample_output() -> ObjectOutput {
        ObjectOutput {
            output_id: OutputId(Hash::from_bytes([1; 32])),
            object_id: ObjectId(Hash::from_bytes([2; 32])),
            version: Version(1),
            domain_id: DomainId(0),
            kind: ObjectKind::State,
            owner: Ownership::Shared,
            predecessor: None,
            state: vec![1, 2, 3],
            state_root: None,
            resources: vec![],
            lock: Script::default(),
            logic: None,
            created_at: 0,
            ttl: None,
            rent_reserve: None,
            flags: 0,
            extensions: vec![],
            spent: true,
        }
    }
}
