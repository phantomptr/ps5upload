use crate::{
    CHUNK_RECORD_SIZE as CHUNK_BYTES, CHUNK_SHIFT, CHUNK_SIZE, Chunk, Codec,
    FILE_RECORD_SIZE as FILE_BYTES, FORMAT_VERSION, INDEX_HEADER_CRC_AT, INDEX_HEADER_SIZE,
    INDEX_MAGIC, INDEX_PAYLOAD_CRC_AT, MAX_VOLUMES, Result, VOLUME_HEADER_SIZE, VOLUME_LIMIT,
    VOLUME_RECORD_SIZE as VOLUME_BYTES, ZSTD_LEVELS, crc_zeroed, invalid, path_hash, read, u16_at,
    u32_at, u64_at, valid_path,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub path: String,
    pub logical_size: u64,
    pub first_chunk: u32,
    pub store_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    pub archive_id: [u8; 16],
    pub zstd_level: u8,
    pub files: Vec<FileRecord>,
    pub chunks: Vec<Chunk>,
    /// Size of each complete volume, including its 64-byte header.
    pub volume_sizes: Vec<u64>,
}

impl Index {
    pub fn validate(&self) -> Result<()> {
        if !ZSTD_LEVELS.contains(&self.zstd_level)
            || self.volume_sizes.len() > MAX_VOLUMES
            || self.chunks.len() > u32::MAX as usize
        {
            return invalid("index limits");
        }
        let mut next = 0u64;
        let mut previous: Option<(u64, &str)> = None;
        for file in &self.files {
            if !valid_path(&file.path)
                || file.path.len() > u16::MAX as usize
                || u64::from(file.first_chunk) != next
            {
                return invalid("file record");
            }
            let key = (path_hash(&file.path), file.path.as_str());
            if previous.is_some_and(|prior| prior >= key) {
                return invalid("file order");
            }
            previous = Some(key);
            let count = file.logical_size.div_ceil(CHUNK_SIZE);
            next = next
                .checked_add(count)
                .ok_or(crate::Error::Invalid("chunk count"))?;
            if next > self.chunks.len() as u64 {
                return invalid("file chunk range");
            }
            for number in 0..count {
                let chunk = self.chunks[(next - count + number) as usize];
                chunk.pack()?;
                let logical = (file.logical_size - number * CHUNK_SIZE).min(CHUNK_SIZE);
                if (chunk.codec == Codec::Raw && u64::from(chunk.stored_size) != logical)
                    || (chunk.codec == Codec::Zstd && u64::from(chunk.stored_size) >= logical)
                    || (file.store_only && chunk.codec != Codec::Raw)
                {
                    return invalid("chunk size or file flags");
                }
            }
        }
        if next != self.chunks.len() as u64 {
            return invalid("unused chunks");
        }
        // No file is also a folder of another (`a` beside `a/b`).
        let paths: std::collections::HashSet<&str> =
            self.files.iter().map(|file| file.path.as_str()).collect();
        for file in &self.files {
            let mut parents = file.path.match_indices('/').map(|(at, _)| &file.path[..at]);
            if parents.any(|parent| paths.contains(parent)) {
                return invalid("file is also a folder");
            }
        }
        if self.chunks.is_empty() {
            if !self.volume_sizes.is_empty() {
                return invalid("unused volumes");
            }
            return Ok(());
        }
        let mut volume = 0usize;
        let mut cursor = VOLUME_HEADER_SIZE as u64;
        for chunk in &self.chunks {
            let id = chunk.volume_id as usize;
            if id != volume {
                // The volume left holds at least one chunk: none is empty.
                if id != volume + 1
                    || cursor == VOLUME_HEADER_SIZE as u64
                    || self.volume_sizes.get(volume) != Some(&cursor)
                {
                    return invalid("volume order or size");
                }
                volume = id;
                cursor = VOLUME_HEADER_SIZE as u64;
            }
            if u64::from(chunk.offset) != cursor {
                return invalid("chunk offset");
            }
            cursor += u64::from(chunk.stored_size);
            if cursor > VOLUME_LIMIT {
                return invalid("volume limit");
            }
        }
        if volume + 1 != self.volume_sizes.len() || self.volume_sizes[volume] != cursor {
            return invalid("final volume size");
        }
        Ok(())
    }
}

fn checked_end(start: usize, count: usize, size: usize) -> Result<usize> {
    start
        .checked_add(
            count
                .checked_mul(size)
                .ok_or(crate::Error::Invalid("table length"))?,
        )
        .ok_or(crate::Error::Invalid("table length"))
}

fn total_logical(files: &[FileRecord]) -> Result<u64> {
    files.iter().try_fold(0u64, |sum, file| {
        sum.checked_add(file.logical_size)
            .ok_or(crate::Error::Invalid("logical size"))
    })
}

pub fn encode_index(index: &Index) -> Result<Vec<u8>> {
    index.validate()?;
    let files_off = INDEX_HEADER_SIZE;
    let chunks_off = checked_end(files_off, index.files.len(), FILE_BYTES)?;
    let volumes_off = checked_end(chunks_off, index.chunks.len(), CHUNK_BYTES)?;
    let strings_off = checked_end(volumes_off, index.volume_sizes.len(), VOLUME_BYTES)?;
    let strings_len = index.files.iter().try_fold(0usize, |sum, file| {
        sum.checked_add(file.path.len())
            .ok_or(crate::Error::Invalid("strings length"))
    })?;
    if strings_len > u32::MAX as usize {
        return invalid("strings length");
    }
    let end = checked_end(strings_off, strings_len, 1)?;
    let mut bytes = vec![0u8; end];
    bytes[..8].copy_from_slice(INDEX_MAGIC);
    bytes[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes[12..16].copy_from_slice(&(INDEX_HEADER_SIZE as u32).to_le_bytes());
    bytes[16] = CHUNK_SHIFT;
    bytes[17] = index.zstd_level;
    bytes[20..24].copy_from_slice(&(index.volume_sizes.len() as u32).to_le_bytes());
    bytes[24..40].copy_from_slice(&index.archive_id);
    bytes[40..48].copy_from_slice(&(index.files.len() as u64).to_le_bytes());
    bytes[48..56].copy_from_slice(&(index.chunks.len() as u64).to_le_bytes());
    for (at, value) in [
        (56, files_off),
        (64, chunks_off),
        (72, volumes_off),
        (80, strings_off),
        (88, strings_len),
    ] {
        bytes[at..at + 8].copy_from_slice(&(value as u64).to_le_bytes());
    }
    bytes[96..104].copy_from_slice(&total_logical(&index.files)?.to_le_bytes());
    let mut path_off = 0usize;
    for (i, file) in index.files.iter().enumerate() {
        let at = files_off + i * FILE_BYTES;
        bytes[at..at + 8].copy_from_slice(&path_hash(&file.path).to_le_bytes());
        bytes[at + 8..at + 16].copy_from_slice(&file.logical_size.to_le_bytes());
        bytes[at + 16..at + 20].copy_from_slice(&file.first_chunk.to_le_bytes());
        bytes[at + 20..at + 24].copy_from_slice(&(path_off as u32).to_le_bytes());
        bytes[at + 24..at + 26].copy_from_slice(&(file.path.len() as u16).to_le_bytes());
        bytes[at + 26..at + 28].copy_from_slice(&(file.store_only as u16).to_le_bytes());
        bytes[strings_off + path_off..strings_off + path_off + file.path.len()]
            .copy_from_slice(file.path.as_bytes());
        path_off += file.path.len();
    }
    for (i, chunk) in index.chunks.iter().enumerate() {
        let at = chunks_off + i * CHUNK_BYTES;
        bytes[at..at + 8].copy_from_slice(&chunk.pack()?.to_le_bytes());
    }
    for (i, size) in index.volume_sizes.iter().enumerate() {
        let at = volumes_off + i * VOLUME_BYTES;
        bytes[at..at + 8].copy_from_slice(&size.to_le_bytes());
    }
    let payload_crc = crc32fast::hash(&bytes[INDEX_HEADER_SIZE..]);
    bytes[INDEX_PAYLOAD_CRC_AT..INDEX_PAYLOAD_CRC_AT + 4]
        .copy_from_slice(&payload_crc.to_le_bytes());
    let header_crc = crc_zeroed::<8>(&bytes, INDEX_PAYLOAD_CRC_AT, INDEX_HEADER_SIZE)?;
    bytes[INDEX_HEADER_CRC_AT..INDEX_HEADER_CRC_AT + 4].copy_from_slice(&header_crc.to_le_bytes());
    Ok(bytes)
}

pub fn decode_index(bytes: &[u8]) -> Result<Index> {
    if bytes.len() < INDEX_HEADER_SIZE
        || read::<8>(bytes, 0)? != *INDEX_MAGIC
        || u32_at(bytes, 8)? != FORMAT_VERSION
        || u32_at(bytes, 12)? != INDEX_HEADER_SIZE as u32
        || bytes[16] != CHUNK_SHIFT
        || bytes[18..20] != [0; 2]
        || bytes[104..112] != [0; 8]
        || bytes[120..128] != [0; 8]
        || u32_at(bytes, INDEX_PAYLOAD_CRC_AT)? != crc32fast::hash(&bytes[INDEX_HEADER_SIZE..])
        || u32_at(bytes, INDEX_HEADER_CRC_AT)?
            != crc_zeroed::<8>(bytes, INDEX_PAYLOAD_CRC_AT, INDEX_HEADER_SIZE)?
    {
        return invalid("index header or CRC");
    }
    let count =
        usize::try_from(u64_at(bytes, 40)?).map_err(|_| crate::Error::Invalid("file count"))?;
    let chunks =
        usize::try_from(u64_at(bytes, 48)?).map_err(|_| crate::Error::Invalid("chunk count"))?;
    let volumes = u32_at(bytes, 20)? as usize;
    if chunks > u32::MAX as usize || volumes > MAX_VOLUMES {
        return invalid("index counts");
    }
    let files_off = INDEX_HEADER_SIZE;
    let chunks_off = checked_end(files_off, count, FILE_BYTES)?;
    let volumes_off = checked_end(chunks_off, chunks, CHUNK_BYTES)?;
    let strings_off = checked_end(volumes_off, volumes, VOLUME_BYTES)?;
    let strings_len =
        usize::try_from(u64_at(bytes, 88)?).map_err(|_| crate::Error::Invalid("strings length"))?;
    if u64_at(bytes, 56)? != files_off as u64
        || u64_at(bytes, 64)? != chunks_off as u64
        || u64_at(bytes, 72)? != volumes_off as u64
        || u64_at(bytes, 80)? != strings_off as u64
        || checked_end(strings_off, strings_len, 1)? != bytes.len()
    {
        return invalid("index table offsets");
    }
    let mut files = Vec::with_capacity(count);
    let mut next_path = 0usize;
    for i in 0..count {
        let at = files_off + i * FILE_BYTES;
        let path_off = u32_at(bytes, at + 20)? as usize;
        let path_len = u16_at(bytes, at + 24)? as usize;
        let flags = u16_at(bytes, at + 26)?;
        if flags & !1 != 0 || u32_at(bytes, at + 28)? != 0 {
            return invalid("file flags");
        }
        if path_off != next_path {
            return invalid("string order");
        }
        let path_end = path_off
            .checked_add(path_len)
            .ok_or(crate::Error::Invalid("path length"))?;
        if path_end > strings_len {
            return invalid("path range");
        }
        let path = bytes
            .get(strings_off + path_off..strings_off + path_end)
            .ok_or(crate::Error::Invalid("path range"))?;
        let path = std::str::from_utf8(path).map_err(|_| crate::Error::Invalid("path UTF-8"))?;
        if u64_at(bytes, at)? != path_hash(path) {
            return invalid("path hash");
        }
        files.push(FileRecord {
            path: path.to_owned(),
            logical_size: u64_at(bytes, at + 8)?,
            first_chunk: u32_at(bytes, at + 16)?,
            store_only: flags == 1,
        });
        next_path = path_end;
    }
    if next_path != strings_len {
        return invalid("unused strings");
    }
    let mut chunk_records = Vec::with_capacity(chunks);
    for i in 0..chunks {
        chunk_records.push(Chunk::unpack(u64_at(bytes, chunks_off + i * CHUNK_BYTES)?)?);
    }
    let mut volume_sizes = Vec::with_capacity(volumes);
    for i in 0..volumes {
        volume_sizes.push(u64_at(bytes, volumes_off + i * VOLUME_BYTES)?);
    }
    let index = Index {
        archive_id: read(bytes, 24)?,
        zstd_level: bytes[17],
        files,
        chunks: chunk_records,
        volume_sizes,
    };
    if total_logical(&index.files)? != u64_at(bytes, 96)? {
        return invalid("logical total");
    }
    index.validate()?;
    Ok(index)
}
