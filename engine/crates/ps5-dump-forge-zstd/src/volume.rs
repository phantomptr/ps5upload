use crate::{
    FORMAT_VERSION, MAX_VOLUMES, Result, VOLUME_CRC_AT, VOLUME_HEADER_SIZE, VOLUME_LIMIT,
    VOLUME_MAGIC, VOLUME_PREFIX, VOLUME_SUFFIX, crc_zeroed, invalid, read, u32_at, u64_at,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeHeader {
    pub id: u32,
    pub count: u32,
    pub archive_id: [u8; 16],
    pub payload_bytes: u64,
}

impl VolumeHeader {
    fn validate(self) -> Result<()> {
        if self.count == 0
            || self.count > MAX_VOLUMES as u32
            || self.id >= self.count
            || self.payload_bytes == 0
            || self.payload_bytes > VOLUME_LIMIT - VOLUME_HEADER_SIZE as u64
        {
            return invalid("volume header values");
        }
        Ok(())
    }
}

pub fn encode_volume_header(header: VolumeHeader) -> Result<[u8; VOLUME_HEADER_SIZE]> {
    header.validate()?;
    let mut bytes = [0; VOLUME_HEADER_SIZE];
    bytes[..8].copy_from_slice(VOLUME_MAGIC);
    bytes[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes[12..16].copy_from_slice(&(VOLUME_HEADER_SIZE as u32).to_le_bytes());
    bytes[16..20].copy_from_slice(&header.id.to_le_bytes());
    bytes[20..24].copy_from_slice(&header.count.to_le_bytes());
    bytes[24..40].copy_from_slice(&header.archive_id);
    bytes[40..48].copy_from_slice(&header.payload_bytes.to_le_bytes());
    let crc = crc_zeroed::<4>(&bytes, VOLUME_CRC_AT, VOLUME_HEADER_SIZE)?;
    bytes[VOLUME_CRC_AT..VOLUME_CRC_AT + 4].copy_from_slice(&crc.to_le_bytes());
    Ok(bytes)
}

pub fn decode_volume_header(bytes: &[u8]) -> Result<VolumeHeader> {
    if bytes.len() < VOLUME_HEADER_SIZE
        || read::<8>(bytes, 0)? != *VOLUME_MAGIC
        || u32_at(bytes, 8)? != FORMAT_VERSION
        || u32_at(bytes, 12)? != VOLUME_HEADER_SIZE as u32
        || bytes[48..VOLUME_CRC_AT] != [0; 12]
        || u32_at(bytes, VOLUME_CRC_AT)?
            != crc_zeroed::<4>(bytes, VOLUME_CRC_AT, VOLUME_HEADER_SIZE)?
    {
        return invalid("volume header");
    }
    let header = VolumeHeader {
        id: u32_at(bytes, 16)?,
        count: u32_at(bytes, 20)?,
        archive_id: read(bytes, 24)?,
        payload_bytes: u64_at(bytes, 40)?,
    };
    header.validate()?;
    Ok(header)
}

/// Volume zero is `ampr_zstd-000.pak` (three digits at least).
#[cfg(feature = "std")]
pub fn volume_name(id: u32) -> String {
    format!("{VOLUME_PREFIX}{id:03}{VOLUME_SUFFIX}")
}

/// `volume_name` without allocating: written into `out` (24 bytes hold any
/// id), returned as a `&str` of it.
pub fn write_volume_name(id: u32, out: &mut [u8]) -> Result<&str> {
    let mut digits = [b'0'; 10];
    let (mut value, mut count) = (id, 0);
    while value > 0 || count < 3 {
        digits[count] = b'0' + (value % 10) as u8;
        (value, count) = (value / 10, count + 1);
    }
    let (prefix, suffix) = (VOLUME_PREFIX.as_bytes(), VOLUME_SUFFIX.as_bytes());
    let len = prefix.len() + count + suffix.len();
    let name = out
        .get_mut(..len)
        .ok_or(crate::Error::Invalid("volume name buffer"))?;
    name[..prefix.len()].copy_from_slice(prefix);
    for (i, digit) in digits[..count].iter().rev().enumerate() {
        name[prefix.len() + i] = *digit;
    }
    name[prefix.len() + count..].copy_from_slice(suffix);
    core::str::from_utf8(name).map_err(|_| crate::Error::Invalid("volume name"))
}
