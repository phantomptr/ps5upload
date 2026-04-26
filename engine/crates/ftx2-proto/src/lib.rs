use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const FTX2_MAGIC: u32 = u32::from_le_bytes(*b"FTX2");
pub const FTX2_VERSION_1: u16 = 1;
pub const FRAME_HEADER_LEN: usize = 28;
pub const TX_META_LEN: usize = 24;
pub const SHARD_HEADER_LEN: usize = 64;
pub const SHARD_ACK_LEN: usize = 48;

// ─── ShardHeader.flags bit constants ─────────────────────────────────────────
/// Shard body carries multiple self-describing file records (pack shard).
/// When set, `ShardHeader.record_count` > 1 and the body layout is:
///   N × { u32 path_len, u32 data_len, path_len bytes, data_len bytes }
/// BLAKE3 digest still covers the entire body.
pub const SHARD_FLAG_PACKED: u32 = 1 << 0;
/// Per-packed-record prefix length: u32 path_len + u32 data_len.
pub const PACKED_RECORD_PREFIX_LEN: usize = 8;

// ─── Frame type ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u16)]
pub enum FrameType {
    Hello = 1,
    HelloAck = 2,
    Error = 3,
    BeginTx = 10,
    BeginTxAck = 11,
    QueryTx = 12,
    QueryTxAck = 13,
    CommitTx = 14,
    CommitTxAck = 15,
    AbortTx = 16,
    AbortTxAck = 17,
    TakeoverRequest = 18,
    TakeoverAck = 19,
    Status = 20,
    StatusAck = 21,
    Shutdown = 22,
    ShutdownAck = 23,
    StreamShard = 30,
    ShardAck = 31,
    Cleanup = 32,
    CleanupAck = 33,
    FsListVolumes = 34,
    FsListVolumesAck = 35,
    FsListDir = 36,
    FsListDirAck = 37,
    /// `FsHash` asks the payload to BLAKE3-hash a single file. Used for
    /// reconciling local-vs-remote content in "Safe" resume mode, where
    /// size equality isn't enough. Request body: `{"path":"/abs/..."}`;
    /// ACK body: `{"hash":"<hex>","size":N}`.
    FsHash = 38,
    FsHashAck = 39,
    /// Destructive FS ops. Payload enforces a writable-root allowlist
    /// (/data/**, /user/**, /mnt/ext*/**, /mnt/usb*/**) and rejects
    /// paths containing `..`. Success → empty ACK body; failure →
    /// FsError with a short diagnostic string.
    FsDelete = 40,
    FsDeleteAck = 41,
    FsMove = 42,
    FsMoveAck = 43,
    FsChmod = 44,
    FsChmodAck = 45,
    FsMkdir = 46,
    FsMkdirAck = 47,
    /// Bounded read of a file on the PS5. Used to pull small metadata
    /// blobs (param.json, icon0.png) out of a game folder so the Library
    /// can render covers + titles. Request body is JSON:
    /// `{"path":"/abs/...","offset":N,"limit":N}`. ACK body is the raw
    /// bytes (not JSON-wrapped — metadata is in the frame header). The
    /// payload caps `limit` at FS_READ_MAX_BYTES to keep the response
    /// bounded regardless of what the client asks for.
    FsRead = 48,
    FsReadAck = 49,
    /// Recursive copy. Both `from` and `to` must pass the writable-root
    /// allowlist. Behaves like `cp -r` — descends directories, preserves
    /// file modes, refuses if `to` already exists (payload responds with
    /// a descriptive error; UI is expected to pre-check or offer merge).
    /// Request body: `{"from":"/abs/...","to":"/abs/..."}`. Success →
    /// empty ACK body.
    FsCopy = 50,
    FsCopyAck = 51,
    /// Mount a disk image file on the PS5. Attaches the image to a
    /// memory-disk unit (/dev/md<N>) via MDIOCATTACH, then `nmount`s
    /// with the right fstype (`exfatfs` for .exfat, `ufs` for .ffpkg).
    /// Request body: `{"image_path":"/abs/...","mount_name":"foo"}`
    /// (mount_name optional — derived from basename without extension
    /// when absent). ACK body:
    ///   `{"mount_point":"/mnt/ps5upload/foo","dev_node":"/dev/md0","fstype":"exfatfs"}`
    /// Mounting .ffpfs / PFS is explicitly NOT supported — that format
    /// needs per-image crypto keys we don't have and never will.
    FsMount = 52,
    FsMountAck = 53,
    /// Reverse of FS_MOUNT: unmount a previously-mounted image and
    /// detach its MD unit. Request body: `{"mount_point":"/mnt/ps5upload/foo"}`.
    /// Success → empty ACK body. Best-effort: the payload forces the
    /// unmount if the bare unmount is busy, since an orphaned MD
    /// attachment leaks a kernel device until reboot.
    FsUnmount = 54,
    FsUnmountAck = 55,
    /// Register a title from a PS5 folder (game folder on /data or
    /// content inside a mounted `/mnt/ps5upload/<name>/`). The payload
    /// stages sce_sys into `/user/app/<title_id>/`, nullfs-binds the
    /// source at `/system_ex/app/<title_id>/`, and calls Sony's
    /// sceAppInstUtilAppInstallTitleDir so the title appears in XMB.
    /// Request body: `{"src_path":"/abs/..."}`.
    /// ACK body: `{"title_id":"...","title_name":"...","used_nullfs":true}`.
    AppRegister = 56,
    AppRegisterAck = 57,
    /// Reverse of AppRegister. Unmounts the nullfs at
    /// `/system_ex/app/<title_id>/`, removes tracking files, and
    /// (on firmware where Sony's API is exported) calls
    /// sceAppInstUtilAppUninstall to clear the XMB tile. Request
    /// body: `{"title_id":"..."}`.
    AppUnregister = 58,
    AppUnregisterAck = 59,
    /// Start an already-registered title via sceLncUtilLaunchApp.
    /// Title must exist in app.db (register first if needed).
    /// Request body: `{"title_id":"..."}`.
    AppLaunch = 60,
    AppLaunchAck = 61,
    /// List titles currently registered in Sony's app.db. Fallbacks:
    /// "sqlite_unavailable" if libSceSqlite can't be dlopened (rare;
    /// only on firmware where Sony moved the library).
    /// Request body: empty.
    /// ACK body: `{"apps":[{"title_id":"...","title_name":"...",
    ///             "src":"/mnt/ps5upload/foo","image_backed":true}, …]}`
    /// where `src` is our mount.lnk contents (empty if we did not
    /// install the title) and `image_backed` is true iff a
    /// `mount_img.lnk` lives alongside (meaning the source path
    /// lives inside a disk image we mounted — unmounting the image
    /// will break the title).
    AppListRegistered = 62,
    AppListRegisteredAck = 63,
    /// Hardware monitoring frames. Bodies are newline-separated
    /// "key=value" text (not JSON) so a simple parser on the client
    /// can handle all three.
    HwInfo = 64,
    HwInfoAck = 65,
    HwTemps = 66,
    HwTempsAck = 67,
    HwPower = 68,
    HwPowerAck = 69,
    /// Open the PS5's built-in web browser (NPXS20001) via
    /// sceSystemServiceLaunchApp. Fire-and-forget.
    AppLaunchBrowser = 70,
    AppLaunchBrowserAck = 71,
    /// Set the PS5 fan turbo threshold in °C. The payload opens
    /// `/dev/icc_fan` and sends `ioctl(0xC01C8F07, {0,0,0,0,0, T,
    /// 0,0,0,0})` — the canonical fan-threshold ioctl on PS5,
    /// hardware-validated on 9.x–11.x and expected-to-work on 12.x
    /// (the `/dev/icc_fan` device + ioctl code have been stable
    /// across every SDK-supported firmware so far). Request body is ASCII
    /// decimal digits of the threshold (e.g. `"65"`) for parser
    /// simplicity; the payload clamps to a safe range (45–80 °C)
    /// regardless of what the client sends. Persists until PS5
    /// reboot. ACK body empty on success.
    ///
    /// Temperature READING is *not* available via any safe
    /// userland path on our payload — see hw_info.c header for
    /// why `sceKernelGet*Temperature` requires running inside the
    /// Sony shellui process. Fan speed READ is a gap in the whole
    /// jailbreak ecosystem (no known ioctl returns RPM).
    HwSetFanThreshold = 72,
    HwSetFanThresholdAck = 73,
    /// Walk the kernel's `allproc` list and return a JSON blob of
    /// `{"ok":bool,"procs":[{"pid":N,"name":"..."}...]}`. Read-only —
    /// the payload never writes back to the kernel through this path.
    /// Uses the SDK's `kernel_copyout` primitive, so it fails cleanly
    /// (with `kernel_rw_unavailable`) if the payload was loaded into
    /// a context that doesn't supply kernel R/W.
    ProcList = 74,
    ProcListAck = 75,
    /// Long-running fs-op progress + cancel. Body: `{"op_id":<u64>}`
    /// where op_id is the trace_id the engine sent on the originating
    /// FS_COPY (or future FS_MOVE-via-copy) frame. The payload looks
    /// up the in-flight op in its g_fs_ops table and replies with
    /// either a status snapshot or a cancel-ack. Both run on a
    /// separate mgmt-port worker than the FS_COPY itself so the engine
    /// can interleave status polls with the FS_COPY's blocking wait
    /// for FS_COPY_ACK.
    FsOpStatus = 76,
    FsOpStatusAck = 77,
    FsOpCancel = 78,
    FsOpCancelAck = 79,
}

impl FrameType {
    pub fn try_from_u16(v: u16) -> Result<Self, DecodeError> {
        match v {
            1 => Ok(Self::Hello),
            2 => Ok(Self::HelloAck),
            3 => Ok(Self::Error),
            10 => Ok(Self::BeginTx),
            11 => Ok(Self::BeginTxAck),
            12 => Ok(Self::QueryTx),
            13 => Ok(Self::QueryTxAck),
            14 => Ok(Self::CommitTx),
            15 => Ok(Self::CommitTxAck),
            16 => Ok(Self::AbortTx),
            17 => Ok(Self::AbortTxAck),
            18 => Ok(Self::TakeoverRequest),
            19 => Ok(Self::TakeoverAck),
            20 => Ok(Self::Status),
            21 => Ok(Self::StatusAck),
            22 => Ok(Self::Shutdown),
            23 => Ok(Self::ShutdownAck),
            30 => Ok(Self::StreamShard),
            31 => Ok(Self::ShardAck),
            32 => Ok(Self::Cleanup),
            33 => Ok(Self::CleanupAck),
            34 => Ok(Self::FsListVolumes),
            35 => Ok(Self::FsListVolumesAck),
            36 => Ok(Self::FsListDir),
            37 => Ok(Self::FsListDirAck),
            38 => Ok(Self::FsHash),
            39 => Ok(Self::FsHashAck),
            40 => Ok(Self::FsDelete),
            41 => Ok(Self::FsDeleteAck),
            42 => Ok(Self::FsMove),
            43 => Ok(Self::FsMoveAck),
            44 => Ok(Self::FsChmod),
            45 => Ok(Self::FsChmodAck),
            46 => Ok(Self::FsMkdir),
            47 => Ok(Self::FsMkdirAck),
            48 => Ok(Self::FsRead),
            49 => Ok(Self::FsReadAck),
            50 => Ok(Self::FsCopy),
            51 => Ok(Self::FsCopyAck),
            52 => Ok(Self::FsMount),
            53 => Ok(Self::FsMountAck),
            54 => Ok(Self::FsUnmount),
            55 => Ok(Self::FsUnmountAck),
            56 => Ok(Self::AppRegister),
            57 => Ok(Self::AppRegisterAck),
            58 => Ok(Self::AppUnregister),
            59 => Ok(Self::AppUnregisterAck),
            60 => Ok(Self::AppLaunch),
            61 => Ok(Self::AppLaunchAck),
            62 => Ok(Self::AppListRegistered),
            63 => Ok(Self::AppListRegisteredAck),
            64 => Ok(Self::HwInfo),
            65 => Ok(Self::HwInfoAck),
            66 => Ok(Self::HwTemps),
            67 => Ok(Self::HwTempsAck),
            68 => Ok(Self::HwPower),
            69 => Ok(Self::HwPowerAck),
            70 => Ok(Self::AppLaunchBrowser),
            71 => Ok(Self::AppLaunchBrowserAck),
            72 => Ok(Self::HwSetFanThreshold),
            73 => Ok(Self::HwSetFanThresholdAck),
            74 => Ok(Self::ProcList),
            75 => Ok(Self::ProcListAck),
            76 => Ok(Self::FsOpStatus),
            77 => Ok(Self::FsOpStatusAck),
            78 => Ok(Self::FsOpCancel),
            79 => Ok(Self::FsOpCancelAck),
            _ => Err(DecodeError::UnknownFrameType(v)),
        }
    }
}

// ─── Frame header ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameHeader {
    pub magic: u32,
    pub version: u16,
    pub frame_type: u16,
    pub flags: u32,
    pub body_len: u64,
    pub trace_id: u64,
}

impl FrameHeader {
    pub fn new(frame_type: FrameType, flags: u32, body_len: u64, trace_id: u64) -> Self {
        Self {
            magic: FTX2_MAGIC,
            version: FTX2_VERSION_1,
            frame_type: frame_type as u16,
            flags,
            body_len,
            trace_id,
        }
    }

    pub fn encode(self) -> [u8; FRAME_HEADER_LEN] {
        let mut out = [0u8; FRAME_HEADER_LEN];
        out[0..4].copy_from_slice(&self.magic.to_le_bytes());
        out[4..6].copy_from_slice(&self.version.to_le_bytes());
        out[6..8].copy_from_slice(&self.frame_type.to_le_bytes());
        out[8..12].copy_from_slice(&self.flags.to_le_bytes());
        out[12..20].copy_from_slice(&self.body_len.to_le_bytes());
        out[20..28].copy_from_slice(&self.trace_id.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() != FRAME_HEADER_LEN {
            return Err(DecodeError::InvalidHeaderLength {
                expected: FRAME_HEADER_LEN,
                actual: bytes.len(),
            });
        }

        let header = Self {
            magic: u32::from_le_bytes(bytes[0..4].try_into().expect("slice length checked")),
            version: u16::from_le_bytes(bytes[4..6].try_into().expect("slice length checked")),
            frame_type: u16::from_le_bytes(bytes[6..8].try_into().expect("slice length checked")),
            flags: u32::from_le_bytes(bytes[8..12].try_into().expect("slice length checked")),
            body_len: u64::from_le_bytes(bytes[12..20].try_into().expect("slice length checked")),
            trace_id: u64::from_le_bytes(bytes[20..28].try_into().expect("slice length checked")),
        };

        if header.magic != FTX2_MAGIC {
            return Err(DecodeError::BadMagic(header.magic));
        }
        if header.version != FTX2_VERSION_1 {
            return Err(DecodeError::UnsupportedVersion(header.version));
        }

        Ok(header)
    }

    /// Decoded frame type, or an error if the u16 is not a known variant.
    pub fn frame_type(&self) -> Result<FrameType, DecodeError> {
        FrameType::try_from_u16(self.frame_type)
    }
}

// ─── TxMeta ──────────────────────────────────────────────────────────────────
//
// Binary prefix carried in the body of BEGIN_TX, QUERY_TX, and ABORT_TX frames.
// Layout (little-endian, 24 bytes total) matches the C payload's ftx2_tx_meta_t:
//
//   tx_id:  [u8; 16]
//   kind:   u32
//   flags:  u32 — see TX_FLAG_* below. Bits unused pre-2.1.

/// BeginTx only: resume an already-interrupted tx_id instead of allocating
/// a fresh entry. When set, the payload looks up the tx_id in its journal;
/// if found in `interrupted` state, it preserves the existing `shards_received`
/// counter and reports it back via BeginTxAck's `last_acked_shard` field so
/// the client can skip past shards the server already has. If the tx_id is
/// unknown or not in resumable state, the payload treats the request as a
/// fresh BeginTx (resume flag becomes a no-op).
pub const TX_FLAG_RESUME: u32 = 0x1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxMeta {
    pub tx_id: [u8; 16],
    pub kind: u32,
    pub flags: u32,
}

impl TxMeta {
    pub fn encode(self) -> [u8; TX_META_LEN] {
        let mut out = [0u8; TX_META_LEN];
        out[0..16].copy_from_slice(&self.tx_id);
        out[16..20].copy_from_slice(&self.kind.to_le_bytes());
        out[20..24].copy_from_slice(&self.flags.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < TX_META_LEN {
            return Err(DecodeError::InvalidTxMetaLength {
                expected: TX_META_LEN,
                actual: bytes.len(),
            });
        }
        Ok(Self {
            tx_id: bytes[0..16].try_into().expect("slice length checked"),
            kind: u32::from_le_bytes(bytes[16..20].try_into().expect("slice length checked")),
            flags: u32::from_le_bytes(bytes[20..24].try_into().expect("slice length checked")),
        })
    }

    /// Return the extra body bytes that follow the metadata prefix.
    pub fn extra(bytes: &[u8]) -> &[u8] {
        if bytes.len() > TX_META_LEN {
            &bytes[TX_META_LEN..]
        } else {
            &[]
        }
    }
}

// ─── Core identifiers ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxId(pub [u8; 16]);

impl TxId {
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobId(pub [u8; 16]);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestHash(pub [u8; 32]);

// ─── Semantic types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionKind {
    UploadTree,
    UploadFile,
    DownloadTree,
    DownloadFile,
    LocalCopy,
    LocalMove,
    ExtractArchive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApplyMode {
    DirectApply,
    SpooledApply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum AckState {
    Spooled = 0,
    Applied = 1,
    DuplicateIgnored = 2,
}

impl AckState {
    pub fn try_from_u8(v: u8) -> Result<Self, DecodeError> {
        match v {
            0 => Ok(Self::Spooled),
            1 => Ok(Self::Applied),
            2 => Ok(Self::DuplicateIgnored),
            _ => Err(DecodeError::UnknownAckState(v)),
        }
    }
}

// ─── ShardHeader ─────────────────────────────────────────────────────────────
//
// Binary prefix at the start of every STREAM_SHARD body (64 bytes, LE):
//
//   tx_id:        [u8; 16]   offset  0
//   shard_seq:    u64        offset 16
//   shard_digest: [u8; 32]   offset 24  (BLAKE3-256)
//   record_count: u32        offset 56
//   flags:        u32        offset 60

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardHeader {
    pub tx_id: [u8; 16],
    pub shard_seq: u64,
    pub shard_digest: [u8; 32],
    pub record_count: u32,
    pub flags: u32,
}

impl ShardHeader {
    pub fn encode(&self) -> [u8; SHARD_HEADER_LEN] {
        let mut out = [0u8; SHARD_HEADER_LEN];
        out[0..16].copy_from_slice(&self.tx_id);
        out[16..24].copy_from_slice(&self.shard_seq.to_le_bytes());
        out[24..56].copy_from_slice(&self.shard_digest);
        out[56..60].copy_from_slice(&self.record_count.to_le_bytes());
        out[60..64].copy_from_slice(&self.flags.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < SHARD_HEADER_LEN {
            return Err(DecodeError::InvalidShardHeaderLength {
                expected: SHARD_HEADER_LEN,
                actual: bytes.len(),
            });
        }
        Ok(Self {
            tx_id: bytes[0..16].try_into().expect("checked"),
            shard_seq: u64::from_le_bytes(bytes[16..24].try_into().expect("checked")),
            shard_digest: bytes[24..56].try_into().expect("checked"),
            record_count: u32::from_le_bytes(bytes[56..60].try_into().expect("checked")),
            flags: u32::from_le_bytes(bytes[60..64].try_into().expect("checked")),
        })
    }
}

// ─── ShardAck ────────────────────────────────────────────────────────────────
//
// Binary body of SHARD_ACK frames (48 bytes, LE):
//
//   tx_id:                [u8; 16]  offset  0
//   shard_seq:            u64       offset 16
//   ack_state:            u8        offset 24
//   _pad:                 [u8; 7]   offset 25
//   bytes_committed_total: u64      offset 32
//   files_committed_total: u64      offset 40

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardAck {
    pub tx_id: [u8; 16],
    pub shard_seq: u64,
    pub ack_state: AckState,
    pub bytes_committed_total: u64,
    pub files_committed_total: u64,
}

impl ShardAck {
    pub fn encode(&self) -> [u8; SHARD_ACK_LEN] {
        let mut out = [0u8; SHARD_ACK_LEN];
        out[0..16].copy_from_slice(&self.tx_id);
        out[16..24].copy_from_slice(&self.shard_seq.to_le_bytes());
        out[24] = self.ack_state as u8;
        // out[25..32] stays zero (pad)
        out[32..40].copy_from_slice(&self.bytes_committed_total.to_le_bytes());
        out[40..48].copy_from_slice(&self.files_committed_total.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < SHARD_ACK_LEN {
            return Err(DecodeError::InvalidShardAckLength {
                expected: SHARD_ACK_LEN,
                actual: bytes.len(),
            });
        }
        Ok(Self {
            tx_id: bytes[0..16].try_into().expect("checked"),
            shard_seq: u64::from_le_bytes(bytes[16..24].try_into().expect("checked")),
            ack_state: AckState::try_from_u8(bytes[24])?,
            bytes_committed_total: u64::from_le_bytes(bytes[32..40].try_into().expect("checked")),
            files_committed_total: u64::from_le_bytes(bytes[40..48].try_into().expect("checked")),
        })
    }
}

// ─── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("invalid header length: expected {expected}, got {actual}")]
    InvalidHeaderLength { expected: usize, actual: usize },
    #[error("invalid magic: {0:#010x}")]
    BadMagic(u32),
    #[error("unsupported version: {0}")]
    UnsupportedVersion(u16),
    #[error("unknown frame type: {0}")]
    UnknownFrameType(u16),
    #[error("unknown ack state: {0}")]
    UnknownAckState(u8),
    #[error("invalid tx_meta length: expected >= {expected}, got {actual}")]
    InvalidTxMetaLength { expected: usize, actual: usize },
    #[error("invalid shard header length: expected >= {expected}, got {actual}")]
    InvalidShardHeaderLength { expected: usize, actual: usize },
    #[error("invalid shard ack length: expected >= {expected}, got {actual}")]
    InvalidShardAckLength { expected: usize, actual: usize },
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_header_round_trip() {
        let header = FrameHeader::new(FrameType::Hello, 7, 1234, 55);
        let bytes = header.encode();
        let decoded = FrameHeader::decode(&bytes).expect("decode should succeed");
        assert_eq!(decoded.magic, FTX2_MAGIC);
        assert_eq!(decoded.version, FTX2_VERSION_1);
        assert_eq!(decoded.frame_type, FrameType::Hello as u16);
        assert_eq!(decoded.flags, 7);
        assert_eq!(decoded.body_len, 1234);
        assert_eq!(decoded.trace_id, 55);
    }

    #[test]
    fn frame_type_decoded_from_header() {
        let header = FrameHeader::new(FrameType::Status, 0, 0, 1);
        let bytes = header.encode();
        let decoded = FrameHeader::decode(&bytes).unwrap();
        assert_eq!(decoded.frame_type().unwrap(), FrameType::Status);
    }

    #[test]
    fn frame_type_all_variants_round_trip() {
        let variants = [
            FrameType::Hello,
            FrameType::HelloAck,
            FrameType::Error,
            FrameType::BeginTx,
            FrameType::BeginTxAck,
            FrameType::QueryTx,
            FrameType::QueryTxAck,
            FrameType::CommitTx,
            FrameType::CommitTxAck,
            FrameType::AbortTx,
            FrameType::AbortTxAck,
            FrameType::TakeoverRequest,
            FrameType::TakeoverAck,
            FrameType::Status,
            FrameType::StatusAck,
            FrameType::Shutdown,
            FrameType::ShutdownAck,
            FrameType::StreamShard,
            FrameType::ShardAck,
            FrameType::Cleanup,
            FrameType::CleanupAck,
            FrameType::FsListVolumes,
            FrameType::FsListVolumesAck,
            FrameType::FsListDir,
            FrameType::FsListDirAck,
            FrameType::HwSetFanThreshold,
            FrameType::HwSetFanThresholdAck,
        ];
        for ft in variants {
            assert_eq!(FrameType::try_from_u16(ft as u16).unwrap(), ft);
        }
    }

    #[test]
    fn unknown_frame_type_is_error() {
        assert!(matches!(
            FrameType::try_from_u16(0xFFFF),
            Err(DecodeError::UnknownFrameType(0xFFFF))
        ));
    }

    #[test]
    fn bad_magic_is_error() {
        let mut bytes = FrameHeader::new(FrameType::Hello, 0, 0, 0).encode();
        bytes[0] = 0xDE;
        assert!(matches!(
            FrameHeader::decode(&bytes),
            Err(DecodeError::BadMagic(_))
        ));
    }

    #[test]
    fn tx_meta_round_trip() {
        let meta = TxMeta {
            tx_id: [
                0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
                0x0f, 0x10,
            ],
            kind: 0x0000_0001,
            flags: 0x0000_0002,
        };
        let bytes = meta.encode();
        assert_eq!(bytes.len(), TX_META_LEN);
        let decoded = TxMeta::decode(&bytes).expect("decode should succeed");
        assert_eq!(decoded, meta);
    }

    #[test]
    fn tx_meta_extra_bytes() {
        let meta = TxMeta {
            tx_id: [0u8; 16],
            kind: 1,
            flags: 0,
        };
        let mut buf = meta.encode().to_vec();
        buf.extend_from_slice(b"{\"hello\":true}");
        let decoded_meta = TxMeta::decode(&buf).unwrap();
        assert_eq!(decoded_meta, meta);
        assert_eq!(TxMeta::extra(&buf), b"{\"hello\":true}");
    }

    #[test]
    fn tx_meta_short_body_is_error() {
        let short = [0u8; 10];
        assert!(matches!(
            TxMeta::decode(&short),
            Err(DecodeError::InvalidTxMetaLength { .. })
        ));
    }

    #[test]
    fn tx_id_hex() {
        let id = TxId([
            0xde, 0xad, 0xbe, 0xef, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99,
            0xaa, 0xbb,
        ]);
        assert_eq!(id.to_hex(), "deadbeef00112233445566778899aabb");
    }

    #[test]
    fn header_length_mismatch_is_error() {
        let short = [0u8; 10];
        assert!(matches!(
            FrameHeader::decode(&short),
            Err(DecodeError::InvalidHeaderLength { .. })
        ));
    }

    #[test]
    fn shard_header_round_trip() {
        let hdr = ShardHeader {
            tx_id: [0x01; 16],
            shard_seq: 42,
            shard_digest: [0xab; 32],
            record_count: 7,
            flags: 0,
        };
        let bytes = hdr.encode();
        assert_eq!(bytes.len(), SHARD_HEADER_LEN);
        let decoded = ShardHeader::decode(&bytes).unwrap();
        assert_eq!(decoded, hdr);
    }

    #[test]
    fn shard_header_field_offsets() {
        let hdr = ShardHeader {
            tx_id: [0xAA; 16],
            shard_seq: u64::from_le_bytes([1, 2, 3, 4, 5, 6, 7, 8]),
            shard_digest: [0xBB; 32],
            record_count: 0x0000_000F,
            flags: 0x0000_0001,
        };
        let bytes = hdr.encode();
        // tx_id at [0..16]
        assert!(bytes[0..16].iter().all(|&b| b == 0xAA));
        // shard_seq at [16..24]
        assert_eq!(&bytes[16..24], &[1, 2, 3, 4, 5, 6, 7, 8]);
        // shard_digest at [24..56]
        assert!(bytes[24..56].iter().all(|&b| b == 0xBB));
        // record_count at [56..60]
        assert_eq!(u32::from_le_bytes(bytes[56..60].try_into().unwrap()), 15);
        // flags at [60..64]
        assert_eq!(u32::from_le_bytes(bytes[60..64].try_into().unwrap()), 1);
    }

    #[test]
    fn shard_header_too_short_is_error() {
        let short = [0u8; 10];
        assert!(matches!(
            ShardHeader::decode(&short),
            Err(DecodeError::InvalidShardHeaderLength { .. })
        ));
    }

    #[test]
    fn shard_ack_round_trip() {
        let ack = ShardAck {
            tx_id: [0x02; 16],
            shard_seq: 99,
            ack_state: AckState::Spooled,
            bytes_committed_total: 1_048_576,
            files_committed_total: 42,
        };
        let bytes = ack.encode();
        assert_eq!(bytes.len(), SHARD_ACK_LEN);
        let decoded = ShardAck::decode(&bytes).unwrap();
        assert_eq!(decoded, ack);
    }

    #[test]
    fn shard_ack_all_states() {
        for state in [
            AckState::Spooled,
            AckState::Applied,
            AckState::DuplicateIgnored,
        ] {
            let ack = ShardAck {
                tx_id: [0u8; 16],
                shard_seq: 1,
                ack_state: state,
                bytes_committed_total: 0,
                files_committed_total: 0,
            };
            let decoded = ShardAck::decode(&ack.encode()).unwrap();
            assert_eq!(decoded.ack_state, state);
        }
    }

    #[test]
    fn shard_ack_too_short_is_error() {
        let short = [0u8; 10];
        assert!(matches!(
            ShardAck::decode(&short),
            Err(DecodeError::InvalidShardAckLength { .. })
        ));
    }

    #[test]
    fn shard_ack_unknown_state_is_error() {
        let mut bytes = ShardAck {
            tx_id: [0u8; 16],
            shard_seq: 0,
            ack_state: AckState::Spooled,
            bytes_committed_total: 0,
            files_committed_total: 0,
        }
        .encode();
        bytes[24] = 0xFF; // unknown ack_state
        assert!(matches!(
            ShardAck::decode(&bytes),
            Err(DecodeError::UnknownAckState(0xFF))
        ));
    }
}
