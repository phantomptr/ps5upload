//! The zstd pack format: `ampr_zstd.index` and the volume headers, each with an
//! encoder and a validating decoder (so a decoder project can import them), plus
//! the pack writer (`writer`, and `measure`), a validated reader (`reader`, the packs as a
//! `SourceTree`) and `tree` (the packs `pack` would write, made on demand from the source).
//!
//! Without the default `std` feature the crate is `no_std` and keeps only the
//! convention (magics, layout, names, `Chunk`, the volume header): the runtime
//! (ampr-emu-ng) reads its packs with these.
#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "std")]
mod index;
#[cfg(feature = "std")]
pub mod reader;
#[cfg(feature = "std")]
pub mod tree;
mod volume;
#[cfg(feature = "std")]
pub mod writer;

#[cfg(feature = "std")]
pub use index::{FileRecord, Index, decode_index, encode_index};
#[cfg(feature = "std")]
pub use volume::volume_name;
pub use volume::{VolumeHeader, decode_volume_header, encode_volume_header, write_volume_name};

pub const INDEX_MAGIC: &[u8; 8] = b"AMPRZPK1";
pub const VOLUME_MAGIC: &[u8; 8] = b"AMPRZDT1";
/// The format version at byte 8 of the index and of each volume header.
pub const FORMAT_VERSION: u32 = 1;
/// The index file at the game root.
pub const INDEX_NAME: &str = "ampr_zstd.index";
/// Volume `id` is `VOLUME_PREFIX` + the id (three digits at least) + `VOLUME_SUFFIX`.
pub const VOLUME_PREFIX: &str = "ampr_zstd-";
pub const VOLUME_SUFFIX: &str = ".pak";
/// log2 of `CHUNK_SIZE`, stored at index header byte 16.
pub const CHUNK_SHIFT: u8 = 18;
pub const CHUNK_SIZE: u64 = 1 << CHUNK_SHIFT;
/// 4 GiB less 64 KiB (4,294,901,760 bytes), as the LZ4 packs: the largest
/// volume whose every offset fits the u32 chunk offset, and under FAT32's
/// 4 GiB file limit.
pub const VOLUME_LIMIT: u64 = (1 << 32) - (1 << 16);
pub const INDEX_HEADER_SIZE: usize = 128;
pub const VOLUME_HEADER_SIZE: usize = 64;
pub const MAX_VOLUMES: usize = 1 << 12;
/// The index's zstd level (header byte 17) is one of these.
pub const ZSTD_LEVELS: core::ops::RangeInclusive<u8> = 1..=22;
/// Index table record sizes: file, chunk, volume size.
pub const FILE_RECORD_SIZE: usize = 32;
pub const CHUNK_RECORD_SIZE: usize = 8;
pub const VOLUME_RECORD_SIZE: usize = 8;
/// Index header: CRC-32 of the payload (everything after the header) at 112,
/// CRC-32 of the header with bytes 112..120 zeroed at 116.
pub const INDEX_PAYLOAD_CRC_AT: usize = 112;
pub const INDEX_HEADER_CRC_AT: usize = 116;
/// Volume header: CRC-32 of the header with bytes 60..64 zeroed at 60.
pub const VOLUME_CRC_AT: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Invalid(what) => write!(f, "invalid zstd pack {what}"),
        }
    }
}

impl core::error::Error for Error {}
pub type Result<T> = core::result::Result<T, Error>;

fn invalid<T>(what: &'static str) -> Result<T> {
    Err(Error::Invalid(what))
}

fn read<const N: usize>(bytes: &[u8], at: usize) -> Result<[u8; N]> {
    bytes
        .get(at..at.checked_add(N).ok_or(Error::Invalid("offset"))?)
        .and_then(|s| s.try_into().ok())
        .ok_or(Error::Invalid("length"))
}

#[cfg(feature = "std")]
fn u16_at(bytes: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(read(bytes, at)?))
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(read(bytes, at)?))
}
fn u64_at(bytes: &[u8], at: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(read(bytes, at)?))
}

fn crc_zeroed<const N: usize>(bytes: &[u8], at: usize, len: usize) -> Result<u32> {
    let header = bytes.get(..len).ok_or(Error::Invalid("header length"))?;
    let end = at.checked_add(N).ok_or(Error::Invalid("CRC offset"))?;
    if end > len {
        return invalid("CRC offset");
    }
    let mut crc = crc32fast::Hasher::new();
    crc.update(&header[..at]);
    crc.update(&[0; N]);
    crc.update(&header[end..]);
    Ok(crc.finalize())
}

/// FNV-1a 64 over the exact UTF-8 relative path (case is preserved).
pub fn path_hash(path: &str) -> u64 {
    path_hash_bytes(path.as_bytes())
}

/// `path_hash` over the path's bytes.
pub fn path_hash_bytes(path: &[u8]) -> u64 {
    path.iter().fold(14695981039346656037, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(1099511628211)
    })
}

pub fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains(['\\', '\0'])
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Raw,
    Zstd,
}

/// Absolute location and stored length of one independently decoded chunk.
/// Packed as a u64: bits 0..31 offset, 32..43 volume id, 44..61
/// `stored_size - 1`, 62..63 codec (0 raw, 1 zstd).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    pub offset: u32,
    pub volume_id: u16,
    pub stored_size: u32,
    pub codec: Codec,
}

impl Chunk {
    pub fn pack(self) -> Result<u64> {
        if self.volume_id as usize >= MAX_VOLUMES
            || self.stored_size == 0
            || u64::from(self.stored_size) > CHUNK_SIZE
            || self.offset < VOLUME_HEADER_SIZE as u32
            || u64::from(self.offset) + u64::from(self.stored_size) > VOLUME_LIMIT
        {
            return invalid("chunk descriptor");
        }
        let codec = match self.codec {
            Codec::Raw => 0,
            Codec::Zstd => 1u64,
        };
        Ok(u64::from(self.offset)
            | (u64::from(self.volume_id) << 32)
            | (u64::from(self.stored_size - 1) << 44)
            | (codec << 62))
    }

    pub fn unpack(word: u64) -> Result<Self> {
        if word >> 62 > 1 {
            return invalid("chunk codec");
        }
        let chunk = Self {
            offset: word as u32,
            volume_id: ((word >> 32) & 0xfff) as u16,
            stored_size: (((word >> 44) & 0x3ffff) + 1) as u32,
            codec: if word >> 62 == 0 {
                Codec::Raw
            } else {
                Codec::Zstd
            },
        };
        chunk.pack()?;
        Ok(chunk)
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;

    #[test]
    fn chunk_record_layout() {
        let chunk = Chunk {
            offset: 0x1234_5678,
            volume_id: 0xabc,
            stored_size: CHUNK_SIZE as u32,
            codec: Codec::Zstd,
        };
        let word = chunk.pack().unwrap();
        assert_eq!(word & 0xffff_ffff, 0x1234_5678);
        assert_eq!((word >> 32) & 0xfff, 0xabc);
        assert_eq!((word >> 44) & 0x3ffff, CHUNK_SIZE - 1);
        assert_eq!(word >> 62, 1);
        assert_eq!(Chunk::unpack(word).unwrap(), chunk);
        let raw = Chunk {
            stored_size: 1,
            codec: Codec::Raw,
            ..chunk
        };
        assert_eq!(Chunk::unpack(raw.pack().unwrap()).unwrap(), raw);
    }

    #[test]
    fn chunk_record_rejects() {
        let base = Chunk {
            offset: VOLUME_HEADER_SIZE as u32,
            volume_id: 0,
            stored_size: 100,
            codec: Codec::Zstd,
        }
        .pack()
        .unwrap();
        for codec in [2u64, 3] {
            let word = (base & !(3 << 62)) | (codec << 62);
            assert!(Chunk::unpack(word).is_err());
        }
        let oversized = Chunk {
            offset: VOLUME_HEADER_SIZE as u32,
            volume_id: 0,
            stored_size: CHUNK_SIZE as u32 + 1,
            codec: Codec::Raw,
        };
        assert!(oversized.pack().is_err());
    }

    const FULL: u32 = CHUNK_SIZE as u32;

    /// A named way to break a valid value.
    type Case<T> = (&'static str, fn(&mut T));

    fn chunk(volume_id: u16, offset: u64, stored_size: u32, codec: Codec) -> Chunk {
        Chunk {
            offset: offset as u32,
            volume_id,
            stored_size,
            codec,
        }
    }

    /// Both index CRCs recomputed after the bytes were changed.
    fn fix_crcs(bytes: &mut [u8]) {
        let payload = crc32fast::hash(&bytes[INDEX_HEADER_SIZE..]);
        bytes[112..116].copy_from_slice(&payload.to_le_bytes());
        let header = crc_zeroed::<8>(bytes, 112, INDEX_HEADER_SIZE).unwrap();
        bytes[116..120].copy_from_slice(&header.to_le_bytes());
    }

    /// The layout of docs/zstd.md written without `validate` (or `Chunk::pack`'s
    /// checks): what a faulty writer would emit, for the decoder to refuse.
    fn raw(index: &Index) -> Vec<u8> {
        let paths: Vec<u8> = index.files.iter().flat_map(|f| f.path.bytes()).collect();
        let chunks_off = INDEX_HEADER_SIZE + 32 * index.files.len();
        let volumes_off = chunks_off + 8 * index.chunks.len();
        let strings_off = volumes_off + 8 * index.volume_sizes.len();
        let mut b = vec![0u8; strings_off];
        b[..8].copy_from_slice(b"AMPRZPK1");
        b[8..12].copy_from_slice(&1u32.to_le_bytes());
        b[12..16].copy_from_slice(&(INDEX_HEADER_SIZE as u32).to_le_bytes());
        b[16] = CHUNK_SHIFT;
        b[17] = index.zstd_level;
        b[20..24].copy_from_slice(&(index.volume_sizes.len() as u32).to_le_bytes());
        b[24..40].copy_from_slice(&index.archive_id);
        let total: u64 = index.files.iter().map(|f| f.logical_size).sum();
        for (at, value) in [
            (40, index.files.len() as u64),
            (48, index.chunks.len() as u64),
            (56, INDEX_HEADER_SIZE as u64),
            (64, chunks_off as u64),
            (72, volumes_off as u64),
            (80, strings_off as u64),
            (88, paths.len() as u64),
            (96, total),
        ] {
            b[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        let mut path_off = 0u32;
        for (i, file) in index.files.iter().enumerate() {
            let at = INDEX_HEADER_SIZE + 32 * i;
            b[at..at + 8].copy_from_slice(&path_hash(&file.path).to_le_bytes());
            b[at + 8..at + 16].copy_from_slice(&file.logical_size.to_le_bytes());
            b[at + 16..at + 20].copy_from_slice(&file.first_chunk.to_le_bytes());
            b[at + 20..at + 24].copy_from_slice(&path_off.to_le_bytes());
            b[at + 24..at + 26].copy_from_slice(&(file.path.len() as u16).to_le_bytes());
            b[at + 26..at + 28].copy_from_slice(&u16::from(file.store_only).to_le_bytes());
            path_off += file.path.len() as u32;
        }
        for (i, c) in index.chunks.iter().enumerate() {
            let codec = u64::from(c.codec == Codec::Zstd);
            let word = u64::from(c.offset)
                | (u64::from(c.volume_id) << 32)
                | (u64::from(c.stored_size - 1) << 44)
                | (codec << 62);
            let at = chunks_off + 8 * i;
            b[at..at + 8].copy_from_slice(&word.to_le_bytes());
        }
        for (i, size) in index.volume_sizes.iter().enumerate() {
            let at = volumes_off + 8 * i;
            b[at..at + 8].copy_from_slice(&size.to_le_bytes());
        }
        b.extend(paths);
        fix_crcs(&mut b);
        b
    }

    /// One file over three volumes; volume 0 ends exactly at `VOLUME_LIMIT`
    /// (16,383 full raw chunks, then a 196,544-byte zstd one).
    fn three_volumes() -> Index {
        let start = VOLUME_HEADER_SIZE as u64;
        let fill = (VOLUME_LIMIT - start) / CHUNK_SIZE;
        let tail = (VOLUME_LIMIT - start - fill * CHUNK_SIZE) as u32;
        let mut chunks: Vec<Chunk> = (0..fill)
            .map(|i| chunk(0, start + i * CHUNK_SIZE, FULL, Codec::Raw))
            .collect();
        chunks.push(chunk(0, VOLUME_LIMIT - u64::from(tail), tail, Codec::Zstd));
        chunks.push(chunk(1, start, FULL, Codec::Raw));
        chunks.push(chunk(2, start, 100, Codec::Zstd));
        Index {
            archive_id: [1; 16],
            zstd_level: 19,
            files: vec![FileRecord {
                path: "big.bin".into(),
                logical_size: (chunks.len() as u64 - 1) * CHUNK_SIZE + 1000,
                first_chunk: 0,
                store_only: false,
            }],
            chunks,
            volume_sizes: vec![VOLUME_LIMIT, start + CHUNK_SIZE, start + 100],
        }
    }

    /// Files of 10 bytes each, one raw chunk apiece, in (path_hash, path) order.
    fn ten_byte_files(paths: &[&str]) -> Index {
        let mut paths = paths.to_vec();
        paths.sort_by_key(|p| (path_hash(p), *p));
        let start = VOLUME_HEADER_SIZE as u64;
        Index {
            archive_id: [2; 16],
            zstd_level: 3,
            files: (0..paths.len())
                .map(|i| FileRecord {
                    path: paths[i].into(),
                    logical_size: 10,
                    first_chunk: i as u32,
                    store_only: true,
                })
                .collect(),
            chunks: (0..paths.len() as u64)
                .map(|i| chunk(0, start + 10 * i, 10, Codec::Raw))
                .collect(),
            volume_sizes: vec![start + 10 * paths.len() as u64],
        }
    }

    /// A store-only file (two raw chunks) and a zstd one; `swap` writes the
    /// records (and so their strings and chunks) in the wrong order.
    fn two_files(swap: bool) -> Index {
        let mut files = vec![
            FileRecord {
                path: "raw.bin".into(),
                logical_size: CHUNK_SIZE + 5,
                first_chunk: 0,
                store_only: true,
            },
            FileRecord {
                path: "zstd/packed.bin".into(),
                logical_size: 1000,
                first_chunk: 0,
                store_only: false,
            },
        ];
        files.sort_by_key(|f| (path_hash(&f.path), f.path.clone()));
        if swap {
            files.reverse();
        }
        let (mut chunks, mut cursor) = (Vec::new(), VOLUME_HEADER_SIZE as u64);
        for file in &mut files {
            file.first_chunk = chunks.len() as u32;
            let sizes: &[(u32, Codec)] = if file.store_only {
                &[(FULL, Codec::Raw), (5, Codec::Raw)]
            } else {
                &[(40, Codec::Zstd)]
            };
            for &(size, codec) in sizes {
                chunks.push(chunk(0, cursor, size, codec));
                cursor += u64::from(size);
            }
        }
        Index {
            archive_id: [3; 16],
            zstd_level: 3,
            files,
            chunks,
            volume_sizes: vec![cursor],
        }
    }

    /// `validate`, `encode_index` and `decode_index` all refuse `index`.
    fn refused(index: &Index, name: &str) {
        assert!(index.validate().is_err(), "validate: {name}");
        assert!(encode_index(index).is_err(), "encode: {name}");
        assert!(decode_index(&raw(index)).is_err(), "decode: {name}");
    }

    #[test]
    fn multi_volume_index() {
        let index = three_volumes();
        let bytes = encode_index(&index).unwrap();
        assert_eq!(raw(&index), bytes);
        assert_eq!(decode_index(&bytes).unwrap(), index);
        let cases: [Case<Index>; 5] = [
            ("volume 0 then 2", |ix| {
                let n = ix.chunks.len();
                ix.chunks[n - 2].volume_id = 2;
                ix.chunks[n - 1] = chunk(2, 64 + CHUNK_SIZE, 100, Codec::Zstd);
                ix.volume_sizes[2] = 64 + CHUNK_SIZE + 100;
            }),
            ("non-final volume size", |ix| ix.volume_sizes[1] += 1),
            ("empty leading volume", |ix| {
                ix.chunks.iter_mut().for_each(|c| c.volume_id += 1);
                ix.volume_sizes.insert(0, 64);
            }),
            ("chunk past the limit", |ix| {
                let n = ix.chunks.len();
                ix.chunks[n - 3].stored_size += 1;
                ix.volume_sizes[0] += 1;
            }),
            ("final volume size", |ix| ix.volume_sizes[2] += 1),
        ];
        for (name, break_it) in cases {
            let mut bad = index.clone();
            break_it(&mut bad);
            refused(&bad, name);
        }
        // Volume 0 may also end short of the limit.
        let mut short = index;
        let n = short.chunks.len();
        short.chunks[n - 3].stored_size -= 1;
        short.volume_sizes[0] -= 1;
        assert_eq!(decode_index(&encode_index(&short).unwrap()).unwrap(), short);
    }

    #[test]
    fn file_is_not_also_a_folder() {
        for paths in [["a", "a/b"], ["x/a", "x/a/b/c"]] {
            refused(&ten_byte_files(&paths), paths[1]);
        }
        let ok = ten_byte_files(&["a", "ab/c"]);
        assert_eq!(decode_index(&encode_index(&ok).unwrap()).unwrap(), ok);
    }

    #[test]
    fn decode_index_rejects() {
        let index = two_files(false);
        let good = encode_index(&index).unwrap();
        assert_eq!(raw(&index), good);
        assert_eq!(decode_index(&good).unwrap(), index);
        // Records at 128 and 160, so the first record's fields sit at 128 + n.
        let cases: &[Case<Vec<u8>>] = &[
            ("path hash", |b| b[128] ^= 1),
            ("flags bit 1", |b| b[128 + 26] |= 2),
            ("flags bit 15", |b| b[128 + 27] |= 0x80),
            ("record reserved", |b| b[128 + 28] = 1),
            ("logical total", |b| b[96] ^= 1),
            ("files_off", |b| b[56] += 8),
            ("chunks_off", |b| b[64] += 8),
            ("volumes_off", |b| b[72] += 8),
            ("strings_off", |b| b[80] += 8),
            ("strings_len + 1", |b| b[88] += 1),
            ("strings_len - 1", |b| b[88] -= 1),
            ("zstd level 0", |b| b[17] = 0),
            ("zstd level 23", |b| b[17] = 23),
            ("reserved 18", |b| b[18] = 1),
            ("reserved 19", |b| b[19] = 1),
            ("reserved 104", |b| b[104] = 1),
            ("reserved 111", |b| b[111] = 1),
            ("reserved 120", |b| b[120] = 1),
            ("reserved 127", |b| b[127] = 1),
            ("truncated", |b| {
                b.pop();
            }),
        ];
        for (name, break_it) in cases {
            let mut bytes = good.clone();
            break_it(&mut bytes);
            assert_ne!(bytes, good, "{name}");
            fix_crcs(&mut bytes);
            assert!(decode_index(&bytes).is_err(), "{name}");
        }
        assert!(decode_index(&raw(&two_files(true))).is_err(), "order");
        for len in [0, 100, INDEX_HEADER_SIZE] {
            assert!(decode_index(&good[..len]).is_err(), "{len} bytes");
        }
        // Header CRC: a header byte flipped, the CRCs left as they were.
        let mut bytes = good.clone();
        bytes[24] ^= 1;
        assert!(decode_index(&bytes).is_err(), "header CRC");
        // Payload CRC: a payload change valid on its own (the zstd file one
        // byte longer, the total to match, the header CRC renewed).
        let mut bytes = good.clone();
        let i = index.files.iter().position(|f| !f.store_only).unwrap();
        bytes[INDEX_HEADER_SIZE + 32 * i + 8] += 1;
        bytes[96] += 1;
        let header = crc_zeroed::<8>(&bytes, 112, INDEX_HEADER_SIZE).unwrap();
        bytes[116..120].copy_from_slice(&header.to_le_bytes());
        assert!(decode_index(&bytes).is_err(), "payload CRC");
        fix_crcs(&mut bytes);
        assert_eq!(decode_index(&bytes).unwrap().files[i].logical_size, 1001);
    }

    #[test]
    fn volume_header_round_trip_and_rejects() {
        let header = VolumeHeader {
            id: 2,
            count: 3,
            archive_id: [4; 16],
            payload_bytes: 1234,
        };
        let good = encode_volume_header(header).unwrap();
        assert_eq!(decode_volume_header(&good).unwrap(), header);
        let last = VolumeHeader {
            id: MAX_VOLUMES as u32 - 1,
            count: MAX_VOLUMES as u32,
            payload_bytes: VOLUME_LIMIT - VOLUME_HEADER_SIZE as u64,
            ..header
        };
        let bytes = encode_volume_header(last).unwrap();
        assert_eq!(decode_volume_header(&bytes).unwrap(), last);
        let cases: &[Case<[u8; VOLUME_HEADER_SIZE]>] = &[
            ("magic", |b| b[0] ^= 1),
            ("version", |b| b[8] = 2),
            ("header size", |b| b[12] = 65),
            ("id == count", |b| b[16] = 3),
            ("count 0", |b| b[16..24].fill(0)),
            ("count > MAX_VOLUMES", |b| {
                b[20..24].copy_from_slice(&(MAX_VOLUMES as u32 + 1).to_le_bytes())
            }),
            ("reserved 48", |b| b[48] = 1),
            ("reserved 59", |b| b[59] = 1),
            ("payload past the limit", |b| {
                b[40..48].copy_from_slice(&(VOLUME_LIMIT - 63).to_le_bytes())
            }),
        ];
        for (name, break_it) in cases {
            let mut bytes = good;
            break_it(&mut bytes);
            let crc = crc_zeroed::<4>(&bytes, 60, VOLUME_HEADER_SIZE).unwrap();
            bytes[60..64].copy_from_slice(&crc.to_le_bytes());
            assert!(decode_volume_header(&bytes).is_err(), "{name}");
        }
        let mut bytes = good;
        bytes[30] ^= 1;
        assert!(decode_volume_header(&bytes).is_err(), "CRC");
        assert!(decode_volume_header(&good[..VOLUME_HEADER_SIZE - 1]).is_err());
        for (id, count) in [(3, 3), (0, 0), (0, MAX_VOLUMES as u32 + 1)] {
            let bad = VolumeHeader {
                id,
                count,
                ..header
            };
            assert!(encode_volume_header(bad).is_err(), "{id} of {count}");
        }
    }
}
