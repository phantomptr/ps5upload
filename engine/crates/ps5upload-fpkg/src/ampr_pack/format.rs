//! The binary records: the `AMPRPAK4` manifest, `AMPRDAT3` data-volume header, `AMPRCRC1`
//! decoded-chunk CRC sidecar and `AMPRCFG1` runtime settings. All little-endian.

use crate::{format_err, Result};

pub const INDEX_MAGIC: &[u8; 8] = b"AMPRPAK4";
pub const DATA_MAGIC: &[u8; 8] = b"AMPRDAT3";
pub const CRC_MAGIC: &[u8; 8] = b"AMPRCRC1";
pub const RUNTIME_MAGIC: &[u8; 8] = b"AMPRCFG1";
pub const INDEX_VERSION: u32 = 4;
pub const DATA_VERSION: u32 = 3;
pub const CRC_VERSION: u32 = 1;
pub const ENDIAN_MARKER: u32 = 0x0102_0304;
pub const INDEX_HEADER_SIZE: usize = 128;
pub const DATA_HEADER_SIZE: usize = 64;
pub const CRC_HEADER_SIZE: usize = 48;
pub const RUNTIME_SIZE: usize = 64;
pub const FILE_RECORD_SIZE: usize = 48;
pub const CHUNK_RECORD_SIZE: usize = 12;
pub const PACK_RECORD_SIZE: usize = 32;

pub const FILE_PACKED: u32 = 1 << 0;
pub const FILE_STORE_ONLY: u32 = 1 << 1;
pub const FILE_STREAMING: u32 = 1 << 2;
pub const FILE_HOT: u32 = 1 << 3;
pub const FILE_RANDOM_ACCESS: u32 = 1 << 4;
const FILE_KNOWN: u32 =
    FILE_PACKED | FILE_STORE_ONLY | FILE_STREAMING | FILE_HOT | FILE_RANDOM_ACCESS;

pub const CODEC_RAW: u8 = 0;
pub const CODEC_LZ4: u8 = 1;
pub const CHUNK_SHARED: u8 = 1 << 0;
pub const CHUNK_STREAMING: u8 = 1 << 1;
pub const CHUNK_PAGE_CONTAINED: u8 = 1 << 2;
pub const CHUNK_PAGE_ALIGNED: u8 = 1 << 3;
const CHUNK_KNOWN: u8 = CHUNK_SHARED | CHUNK_STREAMING | CHUNK_PAGE_CONTAINED | CHUNK_PAGE_ALIGNED;

pub const PACK_STRIPED: u32 = 1 << 0;
pub const PACK_IO_PAGE_LAYOUT: u32 = 1 << 1;
const PACK_KNOWN: u32 = PACK_STRIPED | PACK_IO_PAGE_LAYOUT;

pub const MIN_BLOCK_SHIFT: u8 = 14;
pub const MAX_BLOCK_SHIFT: u8 = 20;
pub const MIN_IO_PAGE: u64 = 1 << 12;
pub const MAX_IO_PAGE: u64 = 1 << 20;
/// Every chunk starts on a multiple of this.
pub const CHUNK_ALIGNMENT: u64 = 64;
const OFFSET_MASK: u64 = (1 << 48) - 1;
const STORED_BITS: u32 = 20;
const STORED_MASK: u32 = (1 << STORED_BITS) - 1;
const CODEC_SHIFT: u32 = 20;
const FLAGS_SHIFT: u32 = 22;
const DESCRIPTOR_KNOWN: u32 = STORED_MASK | (0x3 << CODEC_SHIFT) | (0xFF << FLAGS_SHIFT);

pub fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

pub fn align_up(value: u64, alignment: u64) -> u64 {
    debug_assert!(alignment.is_power_of_two());
    (value + alignment - 1) & !(alignment - 1)
}

pub fn align_down(value: u64, alignment: u64) -> u64 {
    debug_assert!(alignment.is_power_of_two());
    value & !(alignment - 1)
}

/// 64-bit FNV-1a with the standard offset basis (0 stored as 1). The pack manifest uses the
/// standard basis; `ampr_emu.index` does not (see [`crate::ampr_index`]).
pub fn fnv1a64(data: &[u8]) -> u64 {
    let h = data.iter().fold(0xCBF2_9CE4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01B3)
    });
    h.max(1)
}

/// `\` as `/` and ASCII `A`–`Z` lowered: the case folding AMPR lookups use.
pub fn ascii_fold(path: &str) -> Vec<u8> {
    path.bytes()
        .map(|b| match b {
            b'\\' => b'/',
            b'A'..=b'Z' => b + 0x20,
            _ => b,
        })
        .collect()
}

/// The hash a file record carries: FNV-1a of the folded `/app0/...` path.
pub fn path_hash(app0_path: &str) -> u64 {
    fnv1a64(&ascii_fold(app0_path))
}

/// A path as the manifest stores it: absolute under `/app0`, `.` and empty parts dropped.
pub fn canonical(path: &str) -> Result<String> {
    let path = path.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return format_err(format!("path escapes the root: {path}"));
                }
            }
            c if c.contains('\0') => return format_err("NUL in a path"),
            c => parts.push(c),
        }
    }
    let joined = format!("/{}", parts.join("/"));
    if joined.eq_ignore_ascii_case("/app0") {
        return Ok("/app0".into());
    }
    if !joined.to_ascii_lowercase().starts_with("/app0/") {
        return format_err(format!("{joined} is outside /app0"));
    }
    Ok(joined)
}

/// A file's entry in the manifest; record `i` is AMPRIDX3 file id `i + 1`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileRecord {
    pub path_hash: u64,
    pub logical_size: u64,
    pub mtime: i64,
    pub first_chunk: u32,
    pub chunk_count: u32,
    pub path_offset: u32,
    pub path_length: u32,
    pub flags: u32,
    pub block_shift: u8,
    pub packing_class: u8,
    pub reserved: u16,
}

impl FileRecord {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.path_hash.to_le_bytes());
        out.extend_from_slice(&self.logical_size.to_le_bytes());
        out.extend_from_slice(&self.mtime.to_le_bytes());
        out.extend_from_slice(&self.first_chunk.to_le_bytes());
        out.extend_from_slice(&self.chunk_count.to_le_bytes());
        out.extend_from_slice(&self.path_offset.to_le_bytes());
        out.extend_from_slice(&self.path_length.to_le_bytes());
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.push(self.block_shift);
        out.push(self.packing_class);
        out.extend_from_slice(&self.reserved.to_le_bytes());
    }

    fn decode(d: &[u8]) -> Self {
        Self {
            path_hash: le64(d, 0),
            logical_size: le64(d, 8),
            mtime: le64(d, 16) as i64,
            first_chunk: le32(d, 24),
            chunk_count: le32(d, 28),
            path_offset: le32(d, 32),
            path_length: le32(d, 36),
            flags: le32(d, 40),
            block_shift: d[44],
            packing_class: d[45],
            reserved: u16::from_le_bytes([d[46], d[47]]),
        }
    }

    pub fn packed(&self) -> bool {
        self.flags & FILE_PACKED != 0
    }
}

/// One stored block: where it is, how big it is stored, and how. Its decoded size follows from
/// the owning file's block size, so the 12-byte record does not carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkRecord {
    pub offset: u64,
    pub stored_size: u32,
    pub pack_id: u16,
    pub codec: u8,
    pub flags: u8,
}

impl ChunkRecord {
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        if self.offset > OFFSET_MASK {
            return format_err("a chunk offset exceeds 48 bits");
        }
        if self.stored_size == 0 || self.stored_size > 1 << MAX_BLOCK_SHIFT {
            return format_err("a chunk's stored size is outside 1 byte to 1 MiB");
        }
        if self.codec > 3 || self.flags & !CHUNK_KNOWN != 0 {
            return format_err("a chunk has an unknown codec or flag");
        }
        let location = self.offset | (u64::from(self.pack_id) << 48);
        let descriptor = (self.stored_size - 1)
            | (u32::from(self.codec) << CODEC_SHIFT)
            | (u32::from(self.flags) << FLAGS_SHIFT);
        out.extend_from_slice(&location.to_le_bytes());
        out.extend_from_slice(&descriptor.to_le_bytes());
        Ok(())
    }

    fn decode(d: &[u8]) -> Result<Self> {
        let location = le64(d, 0);
        let descriptor = le32(d, 8);
        if descriptor & !DESCRIPTOR_KNOWN != 0 {
            return format_err("a chunk descriptor has reserved bits set");
        }
        Ok(Self {
            offset: location & OFFSET_MASK,
            stored_size: (descriptor & STORED_MASK) + 1,
            pack_id: (location >> 48) as u16,
            codec: ((descriptor >> CODEC_SHIFT) & 0x3) as u8,
            flags: ((descriptor >> FLAGS_SHIFT) & 0xFF) as u8,
        })
    }
}

/// One data volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackRecord {
    pub payload_bytes: u64,
    pub file_size: u64,
    pub name_offset: u32,
    pub name_length: u32,
    pub flags: u32,
    pub io_page_size: u32,
}

impl PackRecord {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.payload_bytes.to_le_bytes());
        out.extend_from_slice(&self.file_size.to_le_bytes());
        out.extend_from_slice(&self.name_offset.to_le_bytes());
        out.extend_from_slice(&self.name_length.to_le_bytes());
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.io_page_size.to_le_bytes());
    }

    fn decode(d: &[u8]) -> Self {
        Self {
            payload_bytes: le64(d, 0),
            file_size: le64(d, 8),
            name_offset: le32(d, 16),
            name_length: le32(d, 20),
            flags: le32(d, 24),
            io_page_size: le32(d, 28),
        }
    }
}

/// NUL-terminated UTF-8 strings, each stored once.
#[derive(Default)]
pub struct StringTable {
    data: Vec<u8>,
    seen: std::collections::HashMap<String, (u32, u32)>,
}

impl StringTable {
    pub fn add(&mut self, value: &str) -> Result<(u32, u32)> {
        if let Some(&at) = self.seen.get(value) {
            return Ok(at);
        }
        if value.contains('\0') {
            return format_err("NUL in a manifest string");
        }
        let offset = self.data.len();
        if offset + value.len() + 1 > u32::MAX as usize {
            return format_err("the manifest string table exceeds 4 GiB");
        }
        self.data.extend_from_slice(value.as_bytes());
        self.data.push(0);
        let at = (offset as u32, value.len() as u32);
        self.seen.insert(value.to_string(), at);
        Ok(at)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.data
    }
}

/// A parsed, validated manifest.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub build_id: [u8; 16],
    pub files: Vec<FileRecord>,
    pub chunks: Vec<ChunkRecord>,
    pub packs: Vec<PackRecord>,
    pub strings: Vec<u8>,
}

/// The manifest bytes for these records.
pub fn manifest_bytes(
    build_id: &[u8; 16],
    files: &[FileRecord],
    chunks: &[ChunkRecord],
    packs: &[PackRecord],
    strings: &[u8],
) -> Result<Vec<u8>> {
    if files.len() > 0xFFFF_FFFE || chunks.len() > u32::MAX as usize || packs.len() > 0xFFFF {
        return format_err("the pack manifest exceeds its format's limits");
    }
    let mut payload = Vec::with_capacity(
        files.len() * FILE_RECORD_SIZE
            + chunks.len() * CHUNK_RECORD_SIZE
            + packs.len() * PACK_RECORD_SIZE
            + strings.len(),
    );
    files.iter().for_each(|f| f.encode(&mut payload));
    for c in chunks {
        c.encode(&mut payload)?;
    }
    packs.iter().for_each(|p| p.encode(&mut payload));
    payload.extend_from_slice(strings);
    let files_offset = INDEX_HEADER_SIZE as u64;
    let chunks_offset = files_offset + (files.len() * FILE_RECORD_SIZE) as u64;
    let packs_offset = chunks_offset + (chunks.len() * CHUNK_RECORD_SIZE) as u64;
    let strings_offset = packs_offset + (packs.len() * PACK_RECORD_SIZE) as u64;
    let mut h = Vec::with_capacity(INDEX_HEADER_SIZE);
    h.extend_from_slice(INDEX_MAGIC);
    h.extend_from_slice(&INDEX_VERSION.to_le_bytes());
    h.extend_from_slice(&(INDEX_HEADER_SIZE as u32).to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes()); // flags
    h.extend_from_slice(&ENDIAN_MARKER.to_le_bytes());
    h.extend_from_slice(build_id);
    h.extend_from_slice(&(files.len() as u64).to_le_bytes());
    h.extend_from_slice(&(chunks.len() as u64).to_le_bytes());
    h.extend_from_slice(&(packs.len() as u32).to_le_bytes());
    h.extend_from_slice(&(FILE_RECORD_SIZE as u32).to_le_bytes());
    h.extend_from_slice(&(CHUNK_RECORD_SIZE as u32).to_le_bytes());
    h.extend_from_slice(&(PACK_RECORD_SIZE as u32).to_le_bytes());
    h.extend_from_slice(&files_offset.to_le_bytes());
    h.extend_from_slice(&chunks_offset.to_le_bytes());
    h.extend_from_slice(&packs_offset.to_le_bytes());
    h.extend_from_slice(&strings_offset.to_le_bytes());
    h.extend_from_slice(&(strings.len() as u64).to_le_bytes());
    h.extend_from_slice(&crc32(&payload).to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes()); // header CRC, at 116
    h.extend_from_slice(&0u64.to_le_bytes()); // reserved
    debug_assert_eq!(h.len(), INDEX_HEADER_SIZE);
    let header_crc = crc32(&h);
    h[116..120].copy_from_slice(&header_crc.to_le_bytes());
    h.extend_from_slice(&payload);
    Ok(h)
}

impl Manifest {
    /// Parse and validate a manifest, checking everything `ampr_emu` checks before it serves a
    /// byte: CRCs, section layout, every file's chunk range and sizes, page-safe placement.
    pub fn parse(d: &[u8]) -> Result<Self> {
        if d.len() < INDEX_HEADER_SIZE {
            return format_err("the pack manifest is truncated");
        }
        if &d[..8] != INDEX_MAGIC || le32(d, 8) != INDEX_VERSION {
            return format_err("not an AMPRPAK4 manifest");
        }
        if le32(d, 12) as usize != INDEX_HEADER_SIZE
            || le32(d, 16) != 0
            || le32(d, 20) != ENDIAN_MARKER
            || le64(d, 120) != 0
        {
            return format_err("invalid AMPRPAK4 header");
        }
        if le32(d, 60) as usize != FILE_RECORD_SIZE
            || le32(d, 64) as usize != CHUNK_RECORD_SIZE
            || le32(d, 68) as usize != PACK_RECORD_SIZE
        {
            return format_err("unsupported AMPRPAK4 record sizes");
        }
        let mut header = d[..INDEX_HEADER_SIZE].to_vec();
        header[116..120].fill(0);
        if crc32(&header) != le32(d, 116) {
            return format_err("pack manifest header CRC mismatch");
        }
        if crc32(&d[INDEX_HEADER_SIZE..]) != le32(d, 112) {
            return format_err("pack manifest payload CRC mismatch");
        }
        let build_id: [u8; 16] = d[24..40].try_into().unwrap();
        let file_count = le64(d, 40) as usize;
        let chunk_count = le64(d, 48) as usize;
        let pack_count = le32(d, 56) as usize;
        let at = |n: usize, size: usize| n.checked_mul(size);
        let files_offset = INDEX_HEADER_SIZE;
        let chunks_offset = at(file_count, FILE_RECORD_SIZE)
            .and_then(|n| n.checked_add(files_offset))
            .ok_or_else(|| crate::Error::Format("file count overflows".into()))?;
        let packs_offset = at(chunk_count, CHUNK_RECORD_SIZE)
            .and_then(|n| n.checked_add(chunks_offset))
            .ok_or_else(|| crate::Error::Format("chunk count overflows".into()))?;
        let strings_offset = packs_offset + pack_count * PACK_RECORD_SIZE;
        if le64(d, 72) as usize != files_offset
            || le64(d, 80) as usize != chunks_offset
            || le64(d, 88) as usize != packs_offset
            || le64(d, 96) as usize != strings_offset
            || strings_offset.checked_add(le64(d, 104) as usize) != Some(d.len())
        {
            return format_err("invalid pack manifest section layout");
        }
        let files = (0..file_count)
            .map(|i| FileRecord::decode(&d[files_offset + i * FILE_RECORD_SIZE..]))
            .collect();
        let chunks = (0..chunk_count)
            .map(|i| ChunkRecord::decode(&d[chunks_offset + i * CHUNK_RECORD_SIZE..]))
            .collect::<Result<Vec<_>>>()?;
        let packs = (0..pack_count)
            .map(|i| PackRecord::decode(&d[packs_offset + i * PACK_RECORD_SIZE..]))
            .collect();
        let m = Self {
            build_id,
            files,
            chunks,
            packs,
            strings: d[strings_offset..].to_vec(),
        };
        m.validate()?;
        Ok(m)
    }

    pub fn string_at(&self, offset: u32, length: u32) -> Result<&str> {
        let (o, l) = (offset as usize, length as usize);
        if o + l >= self.strings.len() || self.strings[o + l] != 0 {
            return format_err("a manifest string is out of bounds or unterminated");
        }
        std::str::from_utf8(&self.strings[o..o + l])
            .map_err(|_| crate::Error::Format("a manifest string is not UTF-8".into()))
    }

    /// The `/app0/...` path of record `i` (file id `i + 1`).
    pub fn path(&self, i: usize) -> Result<&str> {
        let f = &self.files[i];
        self.string_at(f.path_offset, f.path_length)
    }

    pub fn pack_name(&self, pack_id: usize) -> Result<&str> {
        let p = &self.packs[pack_id];
        self.string_at(p.name_offset, p.name_length)
    }

    /// Decoded size of chunk `local` of file `f`.
    pub fn raw_size(f: &FileRecord, local: u32) -> u64 {
        let block = 1u64 << f.block_shift;
        (f.logical_size - u64::from(local) * block).min(block)
    }

    fn validate(&self) -> Result<()> {
        for (id, p) in self.packs.iter().enumerate() {
            let name = self.string_at(p.name_offset, p.name_length)?;
            if !safe_relative(name) {
                return format_err(format!("unsafe pack name {id}: {name:?}"));
            }
            let page = u64::from(p.io_page_size);
            if p.flags & !PACK_KNOWN != 0
                || !(MIN_IO_PAGE..=MAX_IO_PAGE).contains(&page)
                || !page.is_power_of_two()
                || p.flags & PACK_IO_PAGE_LAYOUT == 0
            {
                return format_err(format!("pack {id} has invalid flags or I/O page size"));
            }
            if p.file_size < DATA_HEADER_SIZE as u64 || p.payload_bytes > p.file_size {
                return format_err(format!("pack {id} has an invalid size"));
            }
            let payload_at = p.file_size - p.payload_bytes;
            if payload_at < DATA_HEADER_SIZE as u64
                || payload_at % page != 0
                || p.file_size % page != 0
            {
                return format_err(format!("pack {id} has an invalid payload offset"));
            }
        }
        for (i, f) in self.files.iter().enumerate() {
            let id = i + 1;
            let path = self.string_at(f.path_offset, f.path_length)?;
            if canonical(path).ok().as_deref() != Some(path) || path == "/app0" {
                return format_err(format!("file id {id} has a non-canonical path"));
            }
            if f.reserved != 0 || f.flags & !FILE_KNOWN != 0 {
                return format_err(format!("file id {id} has unknown flags"));
            }
            if path_hash(path) != f.path_hash {
                return format_err(format!("file id {id}'s path hash does not match"));
            }
            if !f.packed() {
                if f.first_chunk != 0
                    || f.chunk_count != 0
                    || f.block_shift != 0
                    || f.packing_class != 0
                    || f.flags != 0
                {
                    return format_err(format!("loose file id {id} references chunks"));
                }
                continue;
            }
            if !(MIN_BLOCK_SHIFT..=MAX_BLOCK_SHIFT).contains(&f.block_shift) {
                return format_err(format!("file id {id} has an invalid block size"));
            }
            let streaming = f.flags & FILE_STREAMING != 0;
            if streaming && f.flags & FILE_RANDOM_ACCESS != 0 {
                return format_err(format!("file id {id} is both streaming and random"));
            }
            let end = f.first_chunk as usize + f.chunk_count as usize;
            if end > self.chunks.len() {
                return format_err(format!("file id {id}'s chunk range is invalid"));
            }
            let block = 1u64 << f.block_shift;
            if f.logical_size.div_ceil(block) != u64::from(f.chunk_count) {
                return format_err(format!("file id {id}'s chunk count does not cover it"));
            }
            for local in 0..f.chunk_count {
                let c = &self.chunks[f.first_chunk as usize + local as usize];
                let raw = Self::raw_size(f, local);
                let stored = u64::from(c.stored_size);
                if stored > block {
                    return format_err(format!("file id {id} has an oversized chunk"));
                }
                match c.codec {
                    CODEC_RAW if stored != raw => {
                        return format_err(format!("file id {id} has a raw size mismatch"))
                    }
                    CODEC_RAW | CODEC_LZ4 => {}
                    _ => return format_err(format!("file id {id} has an unknown codec")),
                }
                if f.flags & FILE_STORE_ONLY != 0 && c.codec != CODEC_RAW {
                    return format_err(format!("store-only file id {id} has an LZ4 chunk"));
                }
                if streaming != (c.flags & CHUNK_STREAMING != 0) {
                    return format_err(format!("file id {id} has a streaming flag mismatch"));
                }
                let Some(p) = self.packs.get(usize::from(c.pack_id)) else {
                    return format_err(format!("file id {id} names a missing pack"));
                };
                let payload_at = p.file_size - p.payload_bytes;
                if c.offset < payload_at
                    || c.offset + stored > p.file_size
                    || !c.offset.is_multiple_of(CHUNK_ALIGNMENT)
                {
                    return format_err(format!("file id {id} has a misplaced chunk"));
                }
                let page = u64::from(p.io_page_size);
                let contained = c.flags & CHUNK_PAGE_CONTAINED != 0;
                let aligned = c.flags & CHUNK_PAGE_ALIGNED != 0;
                if contained
                    && (stored > page
                        || align_down(c.offset, page) != align_down(c.offset + stored - 1, page))
                {
                    return format_err(format!("file id {id} has a chunk crossing a page"));
                }
                if aligned && !c.offset.is_multiple_of(page) {
                    return format_err(format!("file id {id} has a misaligned chunk"));
                }
                if stored <= page && aligned && !contained {
                    return format_err(format!(
                        "file id {id} has a small page-aligned chunk not marked contained"
                    ));
                }
                if !streaming && !(contained || aligned) {
                    return format_err(format!("file id {id} has a chunk that is not page-safe"));
                }
                if align_down(c.offset, page) < payload_at
                    || align_up(c.offset + stored, page) > p.file_size
                {
                    return format_err(format!("file id {id} reads outside its pack"));
                }
            }
        }
        Ok(())
    }
}

/// A relative name with no empty, `.` or `..` part and no backslash.
pub fn safe_relative(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('\\')
        && !name.starts_with('/')
        && name.split('/').all(|c| !matches!(c, "" | "." | ".."))
}

/// A data volume's 64-byte header.
pub fn data_header(
    pack_id: u32,
    build_id: &[u8; 16],
    payload_offset: u64,
    payload_bytes: u64,
    flags: u32,
) -> Vec<u8> {
    let mut h = Vec::with_capacity(DATA_HEADER_SIZE);
    h.extend_from_slice(DATA_MAGIC);
    h.extend_from_slice(&DATA_VERSION.to_le_bytes());
    h.extend_from_slice(&(DATA_HEADER_SIZE as u32).to_le_bytes());
    h.extend_from_slice(&pack_id.to_le_bytes());
    h.extend_from_slice(&flags.to_le_bytes());
    h.extend_from_slice(build_id);
    h.extend_from_slice(&payload_offset.to_le_bytes());
    h.extend_from_slice(&payload_bytes.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes()); // CRC, at 56
    h.extend_from_slice(&0u32.to_le_bytes());
    let crc = crc32(&h);
    h[56..60].copy_from_slice(&crc.to_le_bytes());
    h
}

/// A data volume header's `(pack id, build id, payload offset, payload bytes, flags)`.
pub fn parse_data_header(h: &[u8]) -> Result<(u32, [u8; 16], u64, u64, u32)> {
    if h.len() < DATA_HEADER_SIZE
        || &h[..8] != DATA_MAGIC
        || le32(h, 8) != DATA_VERSION
        || le32(h, 12) as usize != DATA_HEADER_SIZE
        || le32(h, 60) != 0
        || le32(h, 20) & !PACK_KNOWN != 0
    {
        return format_err("not an AMPRDAT3 volume header");
    }
    let mut copy = h[..DATA_HEADER_SIZE].to_vec();
    copy[56..60].fill(0);
    if crc32(&copy) != le32(h, 56) {
        return format_err("pack volume header CRC mismatch");
    }
    Ok((
        le32(h, 16),
        h[24..40].try_into().unwrap(),
        le64(h, 40),
        le64(h, 48),
        le32(h, 20),
    ))
}

/// The offline decoded-chunk CRC sidecar (`<manifest>.crc`).
pub fn crc_sidecar(build_id: &[u8; 16], crcs: &[u32]) -> Vec<u8> {
    let payload: Vec<u8> = crcs.iter().flat_map(|c| c.to_le_bytes()).collect();
    let mut h = Vec::with_capacity(CRC_HEADER_SIZE + payload.len());
    h.extend_from_slice(CRC_MAGIC);
    h.extend_from_slice(&CRC_VERSION.to_le_bytes());
    h.extend_from_slice(&(CRC_HEADER_SIZE as u32).to_le_bytes());
    h.extend_from_slice(build_id);
    h.extend_from_slice(&(crcs.len() as u64).to_le_bytes());
    h.extend_from_slice(&crc32(&payload).to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes()); // header CRC, at 44
    let crc = crc32(&h);
    h[44..48].copy_from_slice(&crc.to_le_bytes());
    h.extend_from_slice(&payload);
    h
}

pub fn parse_crc_sidecar(d: &[u8], build_id: &[u8; 16], chunks: usize) -> Result<Vec<u32>> {
    if d.len() < CRC_HEADER_SIZE
        || &d[..8] != CRC_MAGIC
        || le32(d, 8) != CRC_VERSION
        || le32(d, 12) as usize != CRC_HEADER_SIZE
    {
        return format_err("not an AMPRCRC1 sidecar");
    }
    if &d[16..32] != build_id || le64(d, 32) as usize != chunks {
        return format_err("the CRC sidecar belongs to another build");
    }
    let mut h = d[..CRC_HEADER_SIZE].to_vec();
    h[44..48].fill(0);
    let payload = &d[CRC_HEADER_SIZE..];
    if crc32(&h) != le32(d, 44) || payload.len() != chunks * 4 || crc32(payload) != le32(d, 40) {
        return format_err("CRC sidecar checksum mismatch");
    }
    Ok(payload
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| le32(c, 0))
        .collect())
}

/// Per-title runtime settings (`[runtime]` in a profile): written beside the manifest as
/// `<manifest>.runtime`, read once by the loader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeSettings {
    pub decoded_cache_bytes: u64,
    pub physical_cache_bytes: u64,
    pub workers: u32,
    pub latency_reserve_workers: u32,
}

impl RuntimeSettings {
    pub fn validate(&self) -> Result<()> {
        if !(1..=16).contains(&self.workers) || self.latency_reserve_workers >= self.workers {
            return format_err("runtime workers must be 1 to 16 and the reserve smaller");
        }
        if !self.decoded_cache_bytes.is_multiple_of(16384)
            || !self.physical_cache_bytes.is_multiple_of(16384)
        {
            return format_err("runtime cache sizes must be multiples of 16 KiB");
        }
        Ok(())
    }

    pub fn encode(&self, build_id: &[u8; 16]) -> Result<Vec<u8>> {
        self.validate()?;
        let mut d = Vec::with_capacity(RUNTIME_SIZE);
        d.extend_from_slice(RUNTIME_MAGIC);
        d.extend_from_slice(&1u32.to_le_bytes());
        d.extend_from_slice(&(RUNTIME_SIZE as u32).to_le_bytes());
        d.extend_from_slice(build_id);
        d.extend_from_slice(&self.decoded_cache_bytes.to_le_bytes());
        d.extend_from_slice(&self.physical_cache_bytes.to_le_bytes());
        d.extend_from_slice(&self.workers.to_le_bytes());
        d.extend_from_slice(&self.latency_reserve_workers.to_le_bytes());
        d.extend_from_slice(&0u32.to_le_bytes()); // CRC, at 56
        d.extend_from_slice(&0u32.to_le_bytes());
        let crc = crc32(&d);
        d[56..60].copy_from_slice(&crc.to_le_bytes());
        Ok(d)
    }

    pub fn parse(d: &[u8], build_id: &[u8; 16]) -> Result<Self> {
        if d.len() != RUNTIME_SIZE
            || &d[..8] != RUNTIME_MAGIC
            || le32(d, 8) != 1
            || le32(d, 12) as usize != RUNTIME_SIZE
            || &d[16..32] != build_id
            || le32(d, 60) != 0
        {
            return format_err("invalid AMPRCFG1 runtime settings");
        }
        let mut copy = d.to_vec();
        copy[56..60].fill(0);
        if crc32(&copy) != le32(d, 56) {
            return format_err("runtime settings CRC mismatch");
        }
        let s = Self {
            decoded_cache_bytes: le64(d, 32),
            physical_cache_bytes: le64(d, 40),
            workers: le32(d, 48),
            latency_reserve_workers: le32(d, 52),
        };
        s.validate()?;
        Ok(s)
    }
}

fn le32(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(d[at..at + 4].try_into().unwrap())
}

fn le64(d: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(d[at..at + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The compact chunk descriptor: 48-bit offset and 16-bit pack id in the first word;
    /// stored size − 1 in 20 bits, codec in 2 and flags in 8 in the second.
    #[test]
    fn a_chunk_record_packs_its_bits() {
        let c = ChunkRecord {
            offset: 0x0000_1234_5678_9AC0,
            stored_size: 1 << 20,
            pack_id: 0xBEEF,
            codec: CODEC_LZ4,
            flags: CHUNK_SHARED | CHUNK_PAGE_ALIGNED,
        };
        let mut b = Vec::new();
        c.encode(&mut b).unwrap();
        assert_eq!(b.len(), CHUNK_RECORD_SIZE);
        assert_eq!(le64(&b, 0), 0xBEEF_1234_5678_9AC0);
        assert_eq!(le32(&b, 8), 0xF_FFFF | (1 << 20) | (0b1001 << 22));
        assert_eq!(ChunkRecord::decode(&b).unwrap(), c);

        // Out of the domain: refused, never truncated.
        let mut big = c;
        big.offset = 1 << 48;
        assert!(big.encode(&mut Vec::new()).is_err());
        let mut empty = c;
        empty.stored_size = 0;
        assert!(empty.encode(&mut Vec::new()).is_err());
        // A reserved descriptor bit is an error on read.
        b[11] |= 0x40;
        assert!(ChunkRecord::decode(&b).is_err());
    }

    #[test]
    fn hashes_and_paths_follow_the_format() {
        // Standard FNV-1a 64 test vectors.
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(
            path_hash("/APP0/Data\\X.bin"),
            path_hash("/app0/data/x.bin")
        );
        assert_eq!(canonical("/app0/./a//b/../c").unwrap(), "/app0/a/c");
        assert_eq!(canonical("app0/x").unwrap(), "/app0/x");
        assert!(canonical("/app1/x").is_err());
        assert!(canonical("/app0/../../x").is_err());
    }

    #[test]
    fn headers_and_sidecars_round_trip_and_catch_damage() {
        let id = [7u8; 16];
        let h = data_header(3, &id, 0x10000, 0x20000, PACK_IO_PAGE_LAYOUT);
        assert_eq!(h.len(), DATA_HEADER_SIZE);
        assert_eq!(
            parse_data_header(&h).unwrap(),
            (3, id, 0x10000, 0x20000, PACK_IO_PAGE_LAYOUT)
        );
        let mut bad = h.clone();
        bad[40] ^= 1;
        assert!(parse_data_header(&bad).is_err());

        let crc = crc_sidecar(&id, &[1, 2, 0xFFFF_FFFF]);
        assert_eq!(crc.len(), CRC_HEADER_SIZE + 12);
        assert_eq!(
            parse_crc_sidecar(&crc, &id, 3).unwrap(),
            vec![1, 2, 0xFFFF_FFFF]
        );
        assert!(parse_crc_sidecar(&crc, &[8; 16], 3).is_err());
        assert!(parse_crc_sidecar(&crc, &id, 2).is_err());

        let rt = RuntimeSettings {
            decoded_cache_bytes: 128 << 20,
            physical_cache_bytes: 32 << 20,
            workers: 4,
            latency_reserve_workers: 1,
        };
        let d = rt.encode(&id).unwrap();
        assert_eq!(d.len(), RUNTIME_SIZE);
        assert_eq!(RuntimeSettings::parse(&d, &id).unwrap(), rt);
        let mut wrong = rt;
        wrong.latency_reserve_workers = 4;
        assert!(wrong.encode(&id).is_err());
        wrong = rt;
        wrong.decoded_cache_bytes = 1000;
        assert!(wrong.encode(&id).is_err());
    }
}
