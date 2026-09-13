//! The SI segment: a STORED ZIP appended after the embedded CNT.

use crate::crypto::crc32c;
use crate::{format_err, le16, le32, PkgFile, Result, BLOCK};

const EOCD_SIG: u32 = 0x0605_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;
const EOCD_LEN: usize = 22;

pub struct Member {
    pub name: String,
    /// Absolute offset of the member's bytes in the package.
    pub offset: u64,
    pub size: u64,
}

pub struct Si {
    /// Absolute offset where the ZIP (and the CRC-excluded tail) begins.
    pub zip_start: u64,
    pub members: Vec<Member>,
}

pub fn read(file: &mut PkgFile) -> Result<Option<Si>> {
    let total = file.len();
    if total < EOCD_LEN as u64 {
        return Ok(None);
    }
    let tail_len = total.min(EOCD_LEN as u64 + 0xFFFF);
    let tail_start = total - tail_len;
    let tail = file.read_at(tail_start, tail_len as usize)?;
    let Some(rel) = (0..=tail.len() - EOCD_LEN)
        .rev()
        .find(|&i| le32(&tail, i) == EOCD_SIG)
    else {
        return Ok(None);
    };
    let entries = le16(&tail, rel + 10) as usize;
    let cd_size = le32(&tail, rel + 12) as u64;
    let cd_off = le32(&tail, rel + 16) as u64;
    let eocd = tail_start + rel as u64;
    if cd_size == 0 || cd_size + cd_off > eocd {
        return Ok(None);
    }
    let cd_abs = eocd - cd_size;
    let zip_start = cd_abs - cd_off;
    let cd = file.read_at(cd_abs, cd_size as usize)?;

    let mut members = Vec::with_capacity(entries);
    let mut pos = 0usize;
    for _ in 0..entries {
        if pos + 46 > cd.len() || le32(&cd, pos) != CENTRAL_SIG {
            return format_err("SI central directory is malformed");
        }
        let method = le16(&cd, pos + 10);
        let size = le32(&cd, pos + 24) as u64;
        let name_len = le16(&cd, pos + 28) as usize;
        let extra = le16(&cd, pos + 30) as usize;
        let comment = le16(&cd, pos + 32) as usize;
        let local = zip_start + le32(&cd, pos + 42) as u64;
        if pos + 46 + name_len > cd.len() {
            return format_err("SI central directory name out of range");
        }
        let name = String::from_utf8_lossy(&cd[pos + 46..pos + 46 + name_len]).into_owned();
        pos += 46 + name_len + extra + comment;
        if method != 0 {
            return format_err(format!("SI member {name} is not STORED"));
        }
        let lh = file.read_at(local, 30)?;
        if le32(&lh, 0) != LOCAL_SIG {
            return format_err(format!("SI member {name} has no local header"));
        }
        let offset = local + 30 + le16(&lh, 26) as u64 + le16(&lh, 28) as u64;
        members.push(Member { name, offset, size });
    }
    Ok(Some(Si { zip_start, members }))
}

/// CRC-32C of every 64 KiB block before the SI ZIP, little-endian.
pub fn chunk_crc_table(file: &mut PkgFile, zip_start: u64) -> Result<Vec<u8>> {
    let blocks = zip_start.div_ceil(BLOCK);
    let mut out = Vec::with_capacity(blocks as usize * 4);
    for i in 0..blocks {
        let start = i * BLOCK;
        let len = (zip_start - start).min(BLOCK) as usize;
        out.extend_from_slice(&crc32c(&file.read_at(start, len)?).to_le_bytes());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> Option<PkgFile> {
        let dir = std::env::var("PS5UPLOAD_SAMPLE_PKGS")
            .unwrap_or_else(|_| "/Volumes/Storage/PS5/pkgs".into());
        let p = std::path::Path::new(&dir).join(name);
        match PkgFile::open(&p) {
            Ok(f) => Some(f),
            Err(_) => {
                eprintln!("skip: {} not present", p.display());
                None
            }
        }
    }

    #[test]
    fn dlc_sample_si_members_and_crc() {
        let Some(mut f) = sample("EP7579-PPSA17599_00-EXP33DLC10000PS5.pkg") else {
            return;
        };
        let si = read(&mut f).unwrap().expect("debug package has an SI ZIP");
        assert_eq!(si.zip_start, 0x130000);
        let names: Vec<&str> = si.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "common/etc/naps_meta_18.dat",
                "common/etc/naps_meta_300.dat",
                "common/etc/naps_meta_301.dat",
                "common/etc/naps_meta_302.dat",
                "common/etc/naps_meta_308.dat",
                "common/etc/pfsimage.xml",
                "common/etc/playgo-chunk.dat",
                "config/EP7579-PPSA17599_00-EXP33DLC10000PS5/playgo-chunk.crc",
            ]
        );
        let crc = si.members.last().unwrap();
        assert_eq!((crc.offset, crc.size), (0x1324d8, 76));
        let stored = f.read_at(crc.offset, crc.size as usize).unwrap();
        assert_eq!(chunk_crc_table(&mut f, si.zip_start).unwrap(), stored);
    }
}
