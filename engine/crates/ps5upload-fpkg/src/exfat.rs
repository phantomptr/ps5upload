//! A read-only exFAT reader: enough to walk a mount image and read the files in it.
//!
//! Written from the published exFAT specification. Nothing here writes, and every offset
//! is validated against the file length before it is used, because a mount image is
//! untrusted input.
//!
//! Measured on the real game mounts (tests/mounts.rs): macOS writes these volumes with
//! `NoFatChain` set on **every** entry, files and directories alike, and leaves the FAT
//! zeroed apart from its reserved entries. Contiguity is therefore the authoritative
//! description, and a FAT entry outside the maintained range — zero included — ends the
//! chain; the stream length then decides whether the read was complete.

use std::path::Path;

use crate::source::{is_junk, SourceFile, SourceTree};
use crate::{format_err, PkgFile, Result};

const OEM: &[u8; 8] = b"EXFAT   ";
/// FAT values at or above this mark the end of a chain (EOC, bad, reserved).
const FAT_EOC: u32 = 0xFFFF_FFF7;
/// A directory stream larger than this is refused; real ones are a handful of clusters.
const MAX_DIR_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 1_000_000;
const MAX_DEPTH: u32 = 32;

/// The Microsoft basic-data GUID a GPT partition table gives an exFAT volume.
const BASIC_DATA_GUID: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3C,
];

/// Where the volume lives and how it is shaped, for the readiness report.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub volume_offset: u64,
    pub sector_size: u64,
    pub cluster_size: u64,
    pub cluster_count: u32,
}

/// A file of the volume, with what is needed to read it back.
#[derive(Debug, Clone)]
pub struct ExFatFile {
    pub path: String,
    pub size: u64,
    first_cluster: u32,
    no_fat_chain: bool,
}

pub struct ExFat {
    file: PkgFile,
    geom: Geometry,
    fat_offset: u64,
    heap_offset: u64,
    root_cluster: u32,
}

impl ExFat {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = PkgFile::open(path)?;
        let volume_offset = find_volume(&mut file)?;
        let boot = file.read_at(volume_offset, 512)?;
        if &boot[3..11] != OEM {
            return format_err(format!(
                "{} +{volume_offset:#x} is not an exFAT volume",
                path.display()
            ));
        }
        let sector_shift = boot[108];
        let cluster_shift = boot[109];
        if !(9..=12).contains(&sector_shift) {
            return format_err(format!("exFAT sector shift {sector_shift} is out of range"));
        }
        if cluster_shift > 25 {
            return format_err(format!(
                "exFAT cluster shift {cluster_shift} is out of range"
            ));
        }
        let sector_size = 1u64 << sector_shift;
        let cluster_size = sector_size << cluster_shift;
        let le32 = |at: usize| u32::from_le_bytes(boot[at..at + 4].try_into().unwrap());
        let fat_offset = volume_offset + u64::from(le32(80)) * sector_size;
        let heap_offset = volume_offset + u64::from(le32(88)) * sector_size;
        let cluster_count = le32(92);
        let root_cluster = le32(96);
        if cluster_count < 2 {
            return format_err(format!("exFAT declares {cluster_count} clusters"));
        }
        let end = heap_offset.saturating_add(u64::from(cluster_count) * cluster_size);
        if end > file.len() {
            return format_err(format!(
                "exFAT heap ends at {end:#x}, past the image's {:#x}",
                file.len()
            ));
        }
        if fat_offset >= file.len() {
            return format_err(format!("exFAT FAT at {fat_offset:#x} is past the image"));
        }
        let this = Self {
            file,
            geom: Geometry {
                volume_offset,
                sector_size,
                cluster_size,
                cluster_count,
            },
            fat_offset,
            heap_offset,
            root_cluster,
        };
        this.check_cluster(root_cluster)?;
        Ok(this)
    }

    pub fn geometry(&self) -> Geometry {
        self.geom
    }

    /// Every file of the volume, `/`-separated, junk skipped, sorted by path.
    pub fn walk(&mut self) -> Result<Vec<ExFatFile>> {
        let mut files = Vec::new();
        let root = self.root_cluster;
        let bytes = self.chain_stream(root)?;
        self.walk_dir(&bytes, "", 0, &mut files)?;
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(files)
    }

    /// Up to `len` bytes at `offset` of a walked file.
    pub fn read_file(&mut self, f: &ExFatFile, offset: u64, len: usize) -> Result<Vec<u8>> {
        if offset >= f.size {
            return Ok(Vec::new());
        }
        let want = len.min((f.size - offset) as usize);
        self.read_stream(f.first_cluster, f.no_fat_chain, &f.path, offset, want)
    }

    fn walk_dir(
        &mut self,
        bytes: &[u8],
        prefix: &str,
        depth: u32,
        out: &mut Vec<ExFatFile>,
    ) -> Result<()> {
        if depth > MAX_DEPTH {
            return format_err(format!("{prefix} nests deeper than {MAX_DEPTH} levels"));
        }
        for e in parse_dir(bytes)? {
            let path = format!("{prefix}{}", e.name);
            if e.is_dir {
                if out.len() > MAX_ENTRIES {
                    return format_err("the volume holds more than a million files");
                }
                let bytes = self.read_dir_stream(&path, e.first_cluster, e.size)?;
                self.walk_dir(&bytes, &format!("{path}/"), depth + 1, out)?;
            } else {
                out.push(ExFatFile {
                    path,
                    size: e.size,
                    first_cluster: e.first_cluster,
                    no_fat_chain: e.no_fat_chain,
                });
            }
        }
        Ok(())
    }

    /// A directory's bytes: its own stream length, but at least one cluster.
    fn read_dir_stream(&mut self, path: &str, first: u32, size: u64) -> Result<Vec<u8>> {
        let bytes = size.max(self.geom.cluster_size);
        if bytes > MAX_DIR_BYTES {
            return format_err(format!("directory {path} is {bytes} bytes"));
        }
        self.read_stream(first, true, path, 0, bytes as usize)
    }

    /// A stream of unknown length, followed by its FAT chain.
    fn chain_stream(&mut self, first: u32) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut cluster = first;
        loop {
            out.extend_from_slice(&self.read_cluster(cluster)?);
            if out.len() as u64 >= MAX_DIR_BYTES {
                return format_err("the directory stream is over 64 MiB");
            }
            match self.next_cluster(cluster)? {
                Some(next) => cluster = next,
                None => return Ok(out),
            }
        }
    }

    /// `len` bytes at `offset` of a stream, contiguous or FAT-chained.
    fn read_stream(
        &mut self,
        first: u32,
        no_fat_chain: bool,
        path: &str,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>> {
        let cluster_size = self.geom.cluster_size;
        let skip_clusters = offset / cluster_size;
        let mut skip = (offset % cluster_size) as usize;
        let mut cluster = first;
        if no_fat_chain {
            let index = u32::try_from(skip_clusters)
                .map_err(|_| crate::Error::Format(format!("{path} is too large")))?;
            cluster = first
                .checked_add(index)
                .ok_or_else(|| crate::Error::Format(format!("{path} runs off the volume")))?;
            self.check_cluster(cluster)?;
        } else {
            for _ in 0..skip_clusters {
                cluster = self.next_cluster(cluster)?.ok_or_else(|| early_end(path))?;
            }
        }
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let at = self.cluster_offset(cluster)? + skip as u64;
            let want = (len - out.len()).min(cluster_size as usize - skip);
            out.extend_from_slice(&self.file.read_at(at, want)?);
            skip = 0;
            if out.len() < len {
                cluster = self.next_cluster(cluster)?.ok_or_else(|| early_end(path))?;
            }
        }
        Ok(out)
    }

    fn check_cluster(&self, cluster: u32) -> Result<()> {
        if cluster < 2 || cluster >= self.geom.cluster_count + 2 {
            return format_err(format!(
                "cluster {cluster} is outside the volume's {} clusters",
                self.geom.cluster_count
            ));
        }
        Ok(())
    }

    fn cluster_offset(&self, cluster: u32) -> Result<u64> {
        self.check_cluster(cluster)?;
        Ok(self.heap_offset + (u64::from(cluster) - 2) * self.geom.cluster_size)
    }

    fn read_cluster(&mut self, cluster: u32) -> Result<Vec<u8>> {
        let at = self.cluster_offset(cluster)?;
        self.file.read_at(at, self.geom.cluster_size as usize)
    }

    /// The next cluster of a chain, or `None` at its end. A zero or reserved entry ends
    /// the chain too: macOS images keep the FAT for contiguous streams unmaintained.
    fn next_cluster(&mut self, cluster: u32) -> Result<Option<u32>> {
        let at = self.fat_offset + 4 * u64::from(cluster);
        let raw = self.file.read_at(at, 4)?;
        let next = u32::from_le_bytes(raw.try_into().unwrap());
        if (2..self.geom.cluster_count + 2).contains(&next) && next < FAT_EOC {
            Ok(Some(next))
        } else {
            Ok(None)
        }
    }
}

fn early_end(path: &str) -> crate::Error {
    crate::Error::Format(format!("{path} ends before its data length"))
}

/// An exFAT mount image as a source tree.
pub struct ExFatSource {
    path: std::path::PathBuf,
    volume: ExFat,
    files: Vec<SourceFile>,
    inner: Vec<ExFatFile>,
}

impl ExFatSource {
    pub fn open(path: &Path) -> Result<Self> {
        let mut volume = ExFat::open(path)?;
        let inner = volume.walk()?;
        let files = inner
            .iter()
            .map(|f| SourceFile {
                path: f.path.clone(),
                size: f.size,
            })
            .collect();
        Ok(Self {
            path: path.to_path_buf(),
            volume,
            files,
            inner,
        })
    }

    fn at(&self, path: &str) -> Result<usize> {
        self.inner
            .binary_search_by(|f| f.path.as_str().cmp(path))
            .map_err(|_| crate::Error::Format(format!("{path} is not in this exFAT volume")))
    }
}

impl SourceTree for ExFatSource {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let i = self.at(path)?;
        let file = self.inner[i].clone();
        self.volume.read_file(&file, 0, usize::MAX)
    }

    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let i = self.at(path)?;
        let file = self.inner[i].clone();
        self.volume.read_file(&file, offset, len)
    }

    fn describe(&self) -> String {
        let g = self.volume.geometry();
        format!(
            "exfat {} ({} KiB clusters, {} clusters)",
            self.path.display(),
            g.cluster_size / 1024,
            g.cluster_count
        )
    }
}

/// A parsed directory entry, before it is turned into a file or a directory.
struct Parsed {
    name: String,
    size: u64,
    first_cluster: u32,
    no_fat_chain: bool,
    is_dir: bool,
}

/// The 32-byte records of a directory stream, primary/stream/name groups assembled.
fn parse_dir(bytes: &[u8]) -> Result<Vec<Parsed>> {
    let mut out = Vec::new();
    let mut pending: Option<Pending> = None;
    for chunk in bytes.as_chunks::<32>().0 {
        let ty = chunk[0];
        if ty == 0x00 {
            break;
        }
        if ty == 0xFF || ty & 0x80 == 0 {
            continue; // unused, or deleted (the InUse bit is cleared)
        }
        match ty {
            0x85 => {
                let attr = u16::from_le_bytes(chunk[4..6].try_into().unwrap());
                pending = Some(Pending {
                    is_dir: attr & 0x10 != 0,
                    name_len: 0,
                    name: Vec::new(),
                    first_cluster: 0,
                    no_fat_chain: false,
                    size: 0,
                });
            }
            0xC0 => {
                let Some(p) = pending.as_mut() else { continue };
                let valid = u64::from_le_bytes(chunk[8..16].try_into().unwrap());
                let data = u64::from_le_bytes(chunk[24..32].try_into().unwrap());
                p.name_len = chunk[3] as usize;
                p.no_fat_chain = chunk[1] & 0x02 != 0;
                p.first_cluster = u32::from_le_bytes(chunk[20..24].try_into().unwrap());
                // Bytes between the valid length and the allocated length are undefined.
                p.size = if valid > 0 && valid < data {
                    valid
                } else {
                    data
                };
            }
            0xC1 => {
                let Some(p) = pending.as_mut() else { continue };
                for unit in chunk[2..32].as_chunks::<2>().0 {
                    if p.name.len() >= p.name_len {
                        break;
                    }
                    p.name.push(u16::from_le_bytes(*unit));
                }
                if p.name.len() >= p.name_len {
                    let p = pending.take().unwrap();
                    let name = String::from_utf16_lossy(&p.name);
                    if !name.is_empty() && name != "." && name != ".." && !is_junk(&name) {
                        out.push(Parsed {
                            name,
                            size: p.size,
                            first_cluster: p.first_cluster,
                            no_fat_chain: p.no_fat_chain,
                            is_dir: p.is_dir,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

struct Pending {
    is_dir: bool,
    name_len: usize,
    name: Vec<u16>,
    first_cluster: u32,
    no_fat_chain: bool,
    size: u64,
}

/// Where the exFAT volume starts inside `file`: the image itself, an MBR partition, or a
/// GPT partition of the basic-data type.
fn find_volume(file: &mut PkgFile) -> Result<u64> {
    if file.len() < 512 {
        return format_err("the image is smaller than one sector");
    }
    let head = file.read_at(0, 512)?;
    if &head[3..11] == OEM {
        return Ok(0);
    }
    if head[510..512] == [0x55, 0xAA] {
        for i in 0..4 {
            let e = &head[0x1BE + 16 * i..0x1BE + 16 * (i + 1)];
            if e[4] == 0x07 {
                let lba = u64::from(u32::from_le_bytes(e[8..12].try_into().unwrap()));
                if lba > 0 {
                    return Ok(lba * 512);
                }
            }
        }
    }
    if file.len() >= 1024 {
        let gpt = file.read_at(512, 512)?;
        if &gpt[0..8] == b"EFI PART" {
            let entries_lba = u64::from_le_bytes(gpt[72..80].try_into().unwrap());
            let count = u32::from_le_bytes(gpt[80..84].try_into().unwrap());
            let size = u32::from_le_bytes(gpt[84..88].try_into().unwrap()) as usize;
            if (128..=4096).contains(&size) {
                for i in 0..count.min(128) {
                    let at = entries_lba
                        .checked_mul(512)
                        .and_then(|b| b.checked_add(u64::from(i) * size as u64));
                    let Some(at) = at else { break };
                    let Ok(entry) = file.read_at(at, size) else {
                        break;
                    };
                    if entry[0..16] != BASIC_DATA_GUID {
                        continue;
                    }
                    let first = u64::from_le_bytes(entry[32..40].try_into().unwrap());
                    if first > 0 {
                        return Ok(first * 512);
                    }
                }
            }
        }
    }
    format_err("the image holds no exFAT volume (raw, MBR or GPT)")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 32-byte directory entry of the given type.
    fn record(ty: u8) -> [u8; 32] {
        let mut e = [0u8; 32];
        e[0] = ty;
        e
    }

    fn file_entry(attr: u16) -> [u8; 32] {
        let mut e = record(0x85);
        e[4..6].copy_from_slice(&attr.to_le_bytes());
        e
    }

    fn stream_entry(name_len: usize, flags: u8, first: u32, size: u64) -> [u8; 32] {
        let mut e = record(0xC0);
        e[1] = flags;
        e[3] = name_len as u8;
        e[20..24].copy_from_slice(&first.to_le_bytes());
        e[24..32].copy_from_slice(&size.to_le_bytes());
        e
    }

    fn name_entry(name: &str) -> [u8; 32] {
        let mut e = record(0xC1);
        for (i, unit) in name.encode_utf16().take(15).enumerate() {
            e[2 + 2 * i..4 + 2 * i].copy_from_slice(&unit.to_le_bytes());
        }
        e
    }

    fn file_bytes(name: &str, first: u32, size: u64) -> Vec<[u8; 32]> {
        vec![
            file_entry(0x20),
            stream_entry(name.chars().count(), 0x03, first, size),
            name_entry(name),
        ]
    }

    fn flat(records: Vec<[u8; 32]>) -> Vec<u8> {
        records.into_iter().flatten().collect()
    }

    #[test]
    fn a_file_group_parses_with_its_name_size_and_cluster() {
        let mut records = file_bytes("eboot.bin", 5, 1000);
        records.extend(file_bytes("sce_sys", 9, 65536));
        // The directory's attribute word marks it as one.
        records[3][4..6].copy_from_slice(&0x10u16.to_le_bytes());
        let entries = parse_dir(&flat(records)).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "eboot.bin");
        assert_eq!(entries[0].size, 1000);
        assert_eq!(entries[0].first_cluster, 5);
        assert!(entries[0].no_fat_chain);
        assert!(!entries[0].is_dir);
        assert_eq!(entries[1].name, "sce_sys");
        assert!(entries[1].is_dir);
    }

    #[test]
    fn deleted_unused_and_junk_entries_are_skipped() {
        let mut bytes = Vec::new();
        // A deleted group: the InUse bit is cleared on every record of it.
        bytes.extend_from_slice(&{
            let mut e = file_entry(0x20);
            e[0] = 0x05;
            e
        });
        bytes.extend_from_slice(&{
            let mut e = stream_entry(4, 0x03, 5, 10);
            e[0] = 0x40;
            e
        });
        bytes.extend_from_slice(&{
            let mut e = name_entry("gone");
            e[0] = 0x41;
            e
        });
        bytes.extend_from_slice(&[0xFF; 32]);
        bytes.extend_from_slice(&flat(file_bytes("keep", 6, 20)));
        bytes.extend_from_slice(&flat(file_bytes(".DS_Store", 7, 30)));

        let entries = parse_dir(&bytes).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "keep");
    }

    #[test]
    fn a_name_longer_than_one_fragment_assembles() {
        let name = "abcdefghijklmnopqrstuvwxyz0123";
        let mut bytes = file_bytes(&name[..15], 5, 1);
        bytes.push(name_entry(&name[15..]));
        bytes[1] = stream_entry(name.len(), 0x03, 5, 1);
        let entries = parse_dir(&flat(bytes)).unwrap();
        assert_eq!(entries[0].name, name);
    }

    #[test]
    fn a_name_length_past_the_fragments_never_completes() {
        let mut bytes = file_bytes("short", 5, 1);
        bytes[1] = stream_entry(200, 0x03, 5, 1);
        // Without a complete name the entry is dropped, not half-named.
        assert!(parse_dir(&flat(bytes)).unwrap().is_empty());
    }

    #[test]
    fn the_valid_data_length_wins_when_it_is_shorter() {
        let mut bytes = file_bytes("a", 5, 65536);
        bytes[1][8..16].copy_from_slice(&100u64.to_le_bytes());
        assert_eq!(parse_dir(&flat(bytes)).unwrap()[0].size, 100);
    }

    #[test]
    fn a_short_image_is_refused() {
        let dir = std::env::temp_dir().join(format!("fpkg-exfat-short-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("short.exfat");
        std::fs::write(&img, [0u8; 100]).unwrap();
        let Err(err) = ExFat::open(&img) else {
            panic!("{} must not open", img.display());
        };
        let err = err.to_string();
        assert!(err.contains("one sector"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_image_with_no_exfat_volume_is_refused() {
        let dir = std::env::temp_dir().join(format!("fpkg-exfat-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("blank.exfat");
        std::fs::write(&img, [0u8; 4096]).unwrap();
        let Err(err) = ExFat::open(&img) else {
            panic!("{} must not open", img.display());
        };
        let err = err.to_string();
        assert!(err.contains("no exFAT volume"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_bogus_cluster_shift_is_refused() {
        let dir = std::env::temp_dir().join(format!("fpkg-exfat-shift-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("shift.exfat");
        let mut boot = vec![0u8; 8192];
        boot[3..11].copy_from_slice(OEM);
        boot[108] = 9;
        boot[109] = 40; // 1 TB clusters
        boot[92..96].copy_from_slice(&16u32.to_le_bytes());
        boot[96..100].copy_from_slice(&4u32.to_le_bytes());
        std::fs::write(&img, &boot).unwrap();
        let Err(err) = ExFat::open(&img) else {
            panic!("{} must not open", img.display());
        };
        let err = err.to_string();
        assert!(err.contains("cluster shift"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_heap_past_the_image_is_refused() {
        let dir = std::env::temp_dir().join(format!("fpkg-exfat-heap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("heap.exfat");
        let mut boot = vec![0u8; 8192];
        boot[3..11].copy_from_slice(OEM);
        boot[108] = 9;
        boot[109] = 7;
        boot[88..92].copy_from_slice(&8u32.to_le_bytes());
        boot[92..96].copy_from_slice(&1_000_000u32.to_le_bytes());
        boot[96..100].copy_from_slice(&4u32.to_le_bytes());
        std::fs::write(&img, &boot).unwrap();
        let Err(err) = ExFat::open(&img) else {
            panic!("{} must not open", img.display());
        };
        let err = err.to_string();
        assert!(err.contains("past the image"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
