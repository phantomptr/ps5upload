pub mod cleanup;
pub mod connection;
pub mod download;
pub mod excludes;
pub mod fs_ops;
pub mod game_meta;
pub mod hw;
pub mod log;
pub mod payload_loader;
pub mod transfer;
pub mod volumes;

/// Compute the BLAKE3-256 digest of a shard's raw bytes.
/// The result goes into `ShardHeader.shard_digest` and is verified by the payload.
pub fn hash_shard(data: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(data);
    *hasher.finalize().as_bytes()
}

use ftx2_proto::{ApplyMode, ManifestHash, TransactionKind, TxId};
use serde::{Deserialize, Serialize};

/// One file entry in a multi-file manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestFile {
    /// Destination path on PS5 storage (absolute).
    pub path: String,
    /// Uncompressed byte size of this file.
    pub size: u64,
    /// First shard sequence number (1-based, inclusive) for this file.
    pub shard_start: u64,
    /// Number of shards that carry this file's data.
    pub shard_count: u64,
}

/// Manifest embedded in a BEGIN_TX body (after the TxMeta prefix).
///
/// Single-file transfer: `files` is empty; `dest_root` is the destination
/// file path; all shards concatenated in order produce the file.
///
/// Multi-file transfer: `files` lists each file with its destination path
/// and size; shard K carries the content for `files[K-1]` (one shard per
/// file, split at `SHARD_SIZE` boundaries within each file — future work).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Destination file path (single-file) or root directory (multi-file).
    pub dest_root: String,
    pub file_count: u64,
    pub total_bytes: u64,
    pub total_shards: u64,
    /// Non-empty for multi-file transactions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionPlan {
    pub tx_id: TxId,
    pub kind: TransactionKind,
    pub destination_root: String,
    pub manifest_hash: ManifestHash,
    pub file_count: u64,
    pub total_bytes: u64,
    pub preferred_apply_mode: ApplyMode,
}

impl TransactionPlan {
    pub fn summary(&self) -> String {
        format!(
            "tx kind={:?} dest={} files={} bytes={} mode={:?}",
            self.kind,
            self.destination_root,
            self.file_count,
            self.total_bytes,
            self.preferred_apply_mode
        )
    }
}
