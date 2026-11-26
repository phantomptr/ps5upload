//! Patch packages (`\x7FLIH`): a delta of a whole package, read as that package.
//!
//! Ported from PS5 Dump Forge (`crates/ps5-dump-forge-fpkg/src/lih.rs`), which measured the
//! layout on one debug patch (PPSA23566 01.006); there is no public description of it.
//!
//! The file starts with a 64 KiB header, little-endian: the file's own length at `0x18`, then
//! three `{offset, size}` pairs: the patched package's `\x7FFIH` header block (`0x20`), its
//! `\x7FCNT` container (`0x30`) and a stored zip of PlayGo and relocation tables (`0x40`, not
//! read here). Between the header block and the container sit the outer blocks the update
//! changed, 64 KiB each, in image order with the unchanged ones simply missing.
//!
//! The FIH and the container describe the whole patched package: the container's `imagedigs`
//! lists a digest for every outer block. So each stored block is placed by matching its SHA3
//! against that list, and every other block is the base game's, which this file does not hold.
//!
//! [`View`] is the package the header describes, as bytes: the FIH block at 0, the outer image
//! at the FIH's `pfs_offset`, the container at its `cnt_offset`. A read of a block the patch
//! does not carry fails with an error [`in_base`] recognises, so the package reader can leave
//! out what the patch does not hold instead of failing.
//!
//! Placing by digest means an open reads and hashes the whole patch once (a 472 MB patch took
//! Forge 0.2 s; a 30 GB one takes minutes from a slow disk). Decoding the zip's relocation
//! tables would place the blocks without reading them; nobody has decoded them yet.

use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use crate::crypto::sha3;
use crate::{be32, cnt, fih, le64, prealloc, Error, PkgFile, ReadSeek, Result, BLOCK};

/// The first four bytes of a patch package.
pub const MAGIC: &[u8; 4] = b"\x7FLIH";

const CNT_HEADER: usize = 0x1000;
const CNT_ENTRY_LEN: usize = 0x20;
/// `imagedigs` bytes read per pass while blocks are placed.
const DIGEST_CHUNK: u64 = 1 << 20;
/// Stored blocks read and hashed at once (16 MiB).
const HASH_BATCH: u64 = 256;

fn bad(msg: impl Into<String>) -> Error {
    Error::Format(msg.into())
}

/// `head` starts with a patch package's magic.
pub fn is_lih(head: &[u8]) -> bool {
    head.starts_with(MAGIC)
}

/// The file at `path` (here, on a saved server or on a console) starts with a patch package's
/// magic; `false` when it cannot be read. Four bytes: it never reads the patch itself.
pub fn is_patch(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    let read = if crate::remote_source::is_remote(path) {
        crate::remote_source::open_file(path)
            .ok()
            .is_some_and(|(mut r, _)| r.read_exact(&mut magic).is_ok())
    } else {
        std::fs::File::open(path).is_ok_and(|mut f| f.read_exact(&mut magic).is_ok())
    };
    read && is_lih(&magic)
}

/// The error a read of bytes only the base game holds carries, so it is told from damage by
/// its type and not by its words (a package's own file names end up in error text).
#[derive(Debug)]
struct InBase(String);

impl std::fmt::Display for InBase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is not in this patch package: it is the base game's, and a patch holds only \
             what its update changed",
            self.0
        )
    }
}

impl std::error::Error for InBase {}

/// The error for a read of `what`, which only the base game holds.
pub(crate) fn in_base_error(what: impl Into<String>) -> Error {
    Error::Io(io::Error::other(InBase(what.into())))
}

/// `e` is a read of bytes a patch package does not carry.
pub(crate) fn in_base(e: &Error) -> bool {
    matches!(e, Error::Io(io) if io.get_ref().is_some_and(|inner| inner.is::<InBase>()))
}

/// Which outer blocks of the patched package the patch holds.
#[derive(Clone)]
pub(crate) struct Carried {
    /// Per outer block: the stored block that holds it, plus one; 0 when the patch has none.
    slots: Arc<Vec<u32>>,
    /// How many outer blocks the patch holds (a block of zeros it stores once may stand for
    /// many).
    pub count: u64,
}

impl Carried {
    pub fn has(&self, index: u64) -> bool {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.slots.get(i))
            .is_some_and(|slot| *slot != 0)
    }

    /// Outer blocks of the patched package.
    pub fn outer_blocks(&self) -> u64 {
        self.slots.len() as u64
    }
}

/// The patched package, as bytes.
pub(crate) struct View {
    file: Box<dyn ReadSeek>,
    /// The FIH block's offset in the patch.
    fih_at: u64,
    pfs_offset: u64,
    pfs_size: u64,
    cnt_offset: u64,
    /// The container's offset and size in the patch.
    cnt_at: u64,
    cnt_size: u64,
    /// The first stored outer block's offset in the patch.
    data_at: u64,
    slots: Arc<Vec<u32>>,
    len: u64,
    pos: u64,
}

impl View {
    /// Reads the patch's header, hashes every block it stores and places each by `imagedigs`.
    pub fn open(file: Box<dyn ReadSeek>, len: u64) -> Result<(Self, Carried)> {
        let mut pkg = PkgFile::from_reader(file, len);
        if len < BLOCK {
            return Err(bad(
                "the patch package is incomplete (it is cut short inside its header); copy or \
                 download it again",
            ));
        }
        let head = pkg.read_at(0, 0x40)?;
        if !is_lih(&head) {
            return Err(bad("not a patch package (no \\x7FLIH header)"));
        }
        if le64(&head, 0x18) != len {
            return Err(bad(format!(
                "the patch package is incomplete: its header says {} bytes and the file has \
                 {len}; copy or download it again",
                le64(&head, 0x18)
            )));
        }
        let pair = |at: usize, what: &str| -> Result<(u64, u64)> {
            let (off, size) = (le64(&head, at), le64(&head, at + 8));
            if off < BLOCK || off.checked_add(size).is_none_or(|end| end > len) {
                return Err(bad(format!(
                    "the patch package's {what} ({size} bytes at {off:#x}) is outside the file; \
                     the package is damaged"
                )));
            }
            Ok((off, size))
        };
        let (fih_at, fih_len) = pair(0x20, "package header")?;
        let (cnt_at, cnt_size) = pair(0x30, "container")?;
        if fih_len != BLOCK || !fih_at.is_multiple_of(BLOCK) || !cnt_at.is_multiple_of(BLOCK) {
            return Err(bad(
                "the patch package's regions are not whole blocks, a layout this reader does \
                 not know",
            ));
        }
        // `fih_at + BLOCK <= len`: the pair above checked it.
        let data_at = fih_at + BLOCK;
        let stored_blocks = cnt_at.saturating_sub(data_at) / BLOCK;
        if stored_blocks == 0 {
            return Err(bad("the patch package holds no blocks of its package"));
        }
        if stored_blocks >= u64::from(u32::MAX) {
            return Err(bad(
                "the patch package holds more blocks than this reader places",
            ));
        }
        // What `cnt::read` accepts: the container holds the digest list sized below.
        if cnt_size > cnt::MAX_BYTES {
            return Err(bad(format!(
                "the patch package's container claims {cnt_size} bytes (at most {} accepted)",
                cnt::MAX_BYTES
            )));
        }

        let fih_head = pkg.read_at(fih_at, fih::HEADER_LEN)?;
        if !fih_head.starts_with(b"\x7FFIH") {
            return Err(bad(
                "the patch package carries no \\x7FFIH package header, a layout this reader \
                 does not know",
            ));
        }
        let parsed = fih::parse(&fih_head)?;
        let (pfs_offset, pfs_size, cnt_offset) =
            (parsed.pfs_offset, parsed.pfs_size, parsed.cnt_offset);
        let outer_blocks = pfs_size / BLOCK;
        let (Some(image_end), Some(cnt_end)) = (
            pfs_offset.checked_add(pfs_size),
            cnt_offset.checked_add(cnt_size),
        ) else {
            return Err(bad(
                "the patched package's regions overflow; the package is damaged",
            ));
        };
        let apart = image_end <= cnt_offset || cnt_end <= pfs_offset;
        if pfs_offset < BLOCK || cnt_offset < BLOCK || pfs_size == 0 || !apart {
            return Err(bad(format!(
                "the patched package's layout (image {pfs_size:#x} at {pfs_offset:#x}, container \
                 at {cnt_offset:#x}) is not one this reader knows"
            )));
        }

        let (digests_at, digests_size) = image_digests(&mut pkg, cnt_at, cnt_size)?;
        if outer_blocks.checked_mul(32) != Some(digests_size) {
            return Err(bad(format!(
                "the patch package's block digest table holds {digests_size} bytes for \
                 {outer_blocks} blocks; the package is damaged"
            )));
        }

        // Each stored block's digest, and whether `imagedigs` named it. Read a batch at a time,
        // so what is held follows the bytes really read, never a count from the header.
        let mut stored: HashMap<[u8; 32], (u32, bool)> = HashMap::new();
        let mut slot = 0u64;
        while slot < stored_blocks {
            let n = (stored_blocks - slot).min(HASH_BATCH);
            let raw = pkg.read_at(data_at + slot * BLOCK, (n * BLOCK) as usize)?;
            for (k, digest) in hash_blocks(&raw).into_iter().enumerate() {
                // Identical stored blocks share one digest; the first serves them all.
                stored
                    .entry(digest)
                    .or_insert(((slot + k as u64) as u32, false));
            }
            slot += n;
        }

        // One pass over `imagedigs` places them. A digest it lists more than once (a block of
        // zeros) serves every one of its blocks.
        let mut slots: Vec<u32> = Vec::with_capacity(prealloc(outer_blocks as usize));
        let mut index = 0u64;
        while index < outer_blocks {
            let n = ((outer_blocks - index) * 32).min(DIGEST_CHUNK);
            let chunk = pkg.read_at(digests_at + index * 32, n as usize)?;
            for d in chunk.as_chunks::<32>().0 {
                let mut want = *d;
                want.reverse();
                slots.push(match stored.get_mut(&want) {
                    Some((slot, named)) => {
                        *named = true;
                        *slot + 1
                    }
                    None => 0,
                });
            }
            index += n / 32;
        }
        let strays = stored.values().filter(|(_, named)| !named).count();
        if strays > 0 {
            return Err(bad(format!(
                "{strays} of the patch package's {} distinct blocks match no block of the \
                 package it patches; the package is damaged, or encrypted in a way this reader \
                 does not know",
                stored.len()
            )));
        }
        let slots = Arc::new(slots);
        let carried = Carried {
            count: slots.iter().filter(|s| **s != 0).count() as u64,
            slots: Arc::clone(&slots),
        };
        Ok((
            Self {
                file: pkg.into_reader(),
                fih_at,
                pfs_offset,
                pfs_size,
                cnt_offset,
                cnt_at,
                cnt_size,
                data_at,
                slots,
                len: image_end.max(cnt_end),
                pos: 0,
            },
            carried,
        ))
    }

    /// The patched package's length: what the view reads as.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Where in the patch the view's byte `pos` is, and how many bytes from there are
    /// contiguous. Every sum here is of values `open` checked.
    fn locate(&self, pos: u64) -> io::Result<(u64, u64)> {
        if pos < BLOCK {
            return Ok((self.fih_at + pos, BLOCK - pos));
        }
        if pos >= self.pfs_offset && pos - self.pfs_offset < self.pfs_size {
            let rel = pos - self.pfs_offset;
            let (index, within) = (rel / BLOCK, rel % BLOCK);
            let slot = usize::try_from(index)
                .ok()
                .and_then(|i| self.slots.get(i))
                .copied()
                .unwrap_or(0);
            if slot == 0 {
                return Err(io::Error::other(InBase(format!("outer block {index}"))));
            }
            let at = self.data_at + u64::from(slot - 1) * BLOCK;
            return Ok((at + within, BLOCK - within));
        }
        if pos >= self.cnt_offset && pos - self.cnt_offset < self.cnt_size {
            let rel = pos - self.cnt_offset;
            return Ok((self.cnt_at + rel, self.cnt_size - rel));
        }
        Err(io::Error::other(format!(
            "offset {pos:#x} is outside the patched package; the package is damaged"
        )))
    }
}

impl Read for View {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let (at, run) = self.locate(self.pos)?;
        let n = (buf.len() as u64).min(run) as usize;
        self.file.seek(SeekFrom::Start(at))?;
        self.file.read_exact(&mut buf[..n])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for View {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let pos = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        self.pos = pos.ok_or_else(|| io::Error::other("seek out of range"))?;
        Ok(self.pos)
    }
}

/// SHA3 of each whole 64 KiB block of `raw`, in order, on every core: each thread takes one
/// contiguous share.
fn hash_blocks(raw: &[u8]) -> Vec<[u8; 32]> {
    let blocks: Vec<&[u8]> = raw
        .as_chunks::<{ BLOCK as usize }>()
        .0
        .iter()
        .map(<[u8; BLOCK as usize]>::as_slice)
        .collect();
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    if cores < 2 || blocks.len() < 2 {
        return blocks.into_iter().map(sha3).collect();
    }
    let per = blocks.len().div_ceil(cores);
    std::thread::scope(|s| {
        let handles: Vec<_> = blocks
            .chunks(per)
            .map(|share| s.spawn(move || share.iter().map(|b| sha3(b)).collect::<Vec<_>>()))
            .collect();
        // A worker's panic is re-raised here, as if the hash had panicked on this thread.
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    })
}

/// The container's `imagedigs` entry: its offset in the patch and its size. The container is
/// `cnt_size` bytes at `cnt_at`, both inside the file.
fn image_digests(pkg: &mut PkgFile, cnt_at: u64, cnt_size: u64) -> Result<(u64, u64)> {
    let truncated = || bad("the patch package's container is cut short; the package is damaged");
    if cnt_size < CNT_HEADER as u64 {
        return Err(truncated());
    }
    let head = pkg.read_at(cnt_at, CNT_HEADER)?;
    if !head.starts_with(b"\x7FCNT") {
        return Err(bad(
            "the patch package's container has no \\x7FCNT magic; the package is damaged",
        ));
    }
    let count = be32(&head, 0x10) as usize;
    let table = u64::from(be32(&head, 0x18));
    if count > cnt::MAX_ENTRIES {
        return Err(bad(format!(
            "the patch package's container claims {count} entries (at most {} accepted)",
            cnt::MAX_ENTRIES
        )));
    }
    // Both factors are bounded (4096 × 32), so the table is at most 128 KiB.
    let table_len = count * CNT_ENTRY_LEN;
    if table
        .checked_add(table_len as u64)
        .is_none_or(|end| end > cnt_size)
    {
        return Err(truncated());
    }
    let rows = pkg.read_at(cnt_at + table, table_len)?;
    for row in rows.as_chunks::<CNT_ENTRY_LEN>().0 {
        if be32(row, 0) == cnt::ids::IMAGE_DIGESTS {
            let (off, size) = (u64::from(be32(row, 16)), u64::from(be32(row, 20)));
            if off + size > cnt_size {
                return Err(bad(
                    "the patch package's block digest table runs past its container; the \
                     package is damaged",
                ));
            }
            return Ok((cnt_at + off, size));
        }
    }
    Err(bad(
        "the patch package's container has no block digest table, a layout this reader does \
         not know",
    ))
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// `pkg` (a whole package: FIH block, outer image, container) as a patch that carries
    /// every outer block but `omit`: a 64 KiB header, the FIH block, the carried blocks in
    /// order, the container. From Forge's `tests/patch.rs`.
    pub(crate) fn patch_of(pkg: &[u8], omit: &[usize]) -> Vec<u8> {
        let b = BLOCK as usize;
        let (pfs_offset, pfs_size, cnt_offset) = (
            le64(pkg, 0x10) as usize,
            le64(pkg, 0x18) as usize,
            le64(pkg, 0x58) as usize,
        );
        // The container runs to the image or to the package's end, whichever comes first.
        let cnt_end = if cnt_offset < pfs_offset {
            pfs_offset
        } else {
            pkg.len()
        };
        let mut out = vec![0u8; b];
        out.extend_from_slice(&pkg[..b]);
        for i in (0..pfs_size / b).filter(|i| !omit.contains(i)) {
            out.extend_from_slice(&pkg[pfs_offset + i * b..pfs_offset + (i + 1) * b]);
        }
        let cnt_at = out.len();
        out.extend_from_slice(&pkg[cnt_offset..cnt_end]);
        let len = out.len() as u64;
        out[..4].copy_from_slice(MAGIC);
        for (at, v) in [
            (0x18, len),
            (0x20, BLOCK),
            (0x28, BLOCK),
            (0x30, cnt_at as u64),
            (0x38, (cnt_end - cnt_offset) as u64),
        ] {
            out[at..at + 8].copy_from_slice(&v.to_le_bytes());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::patch_of;
    use super::*;
    use crate::cnt::test_support::minimal_cnt;

    const B: usize = BLOCK as usize;

    /// Four distinct outer blocks and one of zeros twice (blocks 4 and 5).
    fn blocks() -> Vec<Vec<u8>> {
        let mut out: Vec<Vec<u8>> = (0..4u8)
            .map(|i| (0..B).map(|k| (k as u8).wrapping_mul(i + 3) ^ i).collect())
            .collect();
        out.push(vec![0u8; B]);
        out.push(vec![0u8; B]);
        out
    }

    /// A whole package of `blocks`: the FIH block, the image, a container with their digests.
    fn package(blocks: &[Vec<u8>]) -> Vec<u8> {
        let digests: Vec<[u8; 32]> = blocks.iter().map(|b| sha3(b)).collect();
        let mut pkg = vec![0u8; B];
        pkg[..4].copy_from_slice(b"\x7FFIH");
        let cnt_offset = B + blocks.len() * B;
        pkg[0x10..0x18].copy_from_slice(&BLOCK.to_le_bytes());
        pkg[0x18..0x20].copy_from_slice(&((blocks.len() * B) as u64).to_le_bytes());
        pkg[0x58..0x60].copy_from_slice(&(cnt_offset as u64).to_le_bytes());
        for b in blocks {
            pkg.extend_from_slice(b);
        }
        pkg.extend_from_slice(&minimal_cnt(
            "UP0000-PPSA01234_00-TESTGAME00000000",
            &digests,
        ));
        pkg
    }

    fn open(patch: Vec<u8>) -> Result<(View, Carried)> {
        let len = patch.len() as u64;
        View::open(Box::new(io::Cursor::new(patch)), len)
    }

    fn refused(patch: Vec<u8>) -> String {
        match open(patch) {
            Ok(_) => panic!("must not open"),
            Err(e) => e.to_string(),
        }
    }

    fn read_at(view: &mut View, at: u64, len: usize) -> io::Result<Vec<u8>> {
        view.seek(SeekFrom::Start(at))?;
        let mut buf = vec![0u8; len];
        view.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// The view is the patched package where the patch holds it: the header block, every
    /// carried block at its place in the image, the container. A block left out is the base
    /// game's, and reading it says so.
    #[test]
    fn a_patch_reads_as_the_package_it_patches() {
        let blocks = blocks();
        let pkg = package(&blocks);
        let (mut view, carried) = open(patch_of(&pkg, &[1, 3])).unwrap();
        assert_eq!(view.len(), pkg.len() as u64);
        assert_eq!((carried.outer_blocks(), carried.count), (6, 4));
        assert!(carried.has(0) && !carried.has(1) && carried.has(2) && !carried.has(3));
        assert!(!carried.has(6), "past the image");

        assert_eq!(read_at(&mut view, 0, 4).unwrap(), b"\x7FFIH");
        for i in [0usize, 2, 4, 5] {
            let at = (B + i * B) as u64;
            assert!(read_at(&mut view, at, B).unwrap() == blocks[i], "block {i}");
            // From inside a block, too.
            assert!(read_at(&mut view, at + 100, 50).unwrap() == blocks[i][100..150]);
        }
        let cnt_offset = B + blocks.len() * B;
        let cnt = read_at(&mut view, cnt_offset as u64, 0x3000).unwrap();
        assert!(cnt == pkg[cnt_offset..]);

        for i in [1usize, 3] {
            let e = Error::Io(read_at(&mut view, (B + i * B) as u64, 16).unwrap_err());
            assert!(in_base(&e), "{e}");
            assert!(e.to_string().contains("not in this patch package"), "{e}");
        }
        // A read that starts in a carried block and runs into a missing one fails the same way.
        let e = Error::Io(read_at(&mut view, B as u64, 2 * B).unwrap_err());
        assert!(in_base(&e), "{e}");
    }

    /// A block of zeros stored once serves every outer block with that digest.
    #[test]
    fn one_stored_block_serves_every_block_with_its_digest() {
        let blocks = blocks();
        let pkg = package(&blocks);
        // Block 5 (zeros) is not stored; block 4, the same bytes, is.
        let patch = patch_of(&pkg, &[5]);
        let (mut view, carried) = open(patch).unwrap();
        assert!(carried.has(4) && carried.has(5));
        assert_eq!(carried.count, 6);
        assert!(read_at(&mut view, (B + 5 * B) as u64, B).unwrap() == blocks[5]);
    }

    /// A header that lies about the file, its regions or its blocks is refused in words; no
    /// offset from it is followed first.
    #[test]
    fn a_damaged_patch_is_refused() {
        let pkg = package(&blocks());
        let good = patch_of(&pkg, &[1]);
        let with = |at: usize, v: u64| {
            let mut p = good.clone();
            p[at..at + 8].copy_from_slice(&v.to_le_bytes());
            p
        };

        assert!(refused(good[..B - 1].to_vec()).contains("cut short"));
        // Cut after the header: the length it records no longer matches.
        assert!(refused(good[..good.len() - 1].to_vec()).contains("incomplete"));
        assert!(refused(with(0x20, u64::MAX)).contains("package header"));
        assert!(refused(with(0x20, 0)).contains("package header"));
        assert!(refused(with(0x38, u64::MAX)).contains("container"));
        assert!(refused(with(0x30, good.len() as u64 + 1)).contains("container"));
        assert!(refused(with(0x28, BLOCK * 2)).contains("whole blocks"));
        assert!(refused(with(0x30, 2 * BLOCK + 1)).contains("whole blocks"));
        // The container right after the header block leaves no room for a block.
        assert!(refused(with(0x30, 2 * BLOCK)).contains("holds no blocks"));

        // A stored block that is no block of the patched package.
        let mut stray = good.clone();
        stray[2 * B + 7] ^= 0xFF;
        assert!(refused(stray).contains("match no block"));

        // The carried FIH: not one, or one whose image and container overlap.
        let mut no_fih = good.clone();
        no_fih[B] = 0;
        assert!(refused(no_fih).contains("\\x7FFIH"));
        let mut overlap = good.clone();
        overlap[B + 0x58..B + 0x60].copy_from_slice(&(2 * BLOCK).to_le_bytes());
        assert!(refused(overlap).contains("not one this reader knows"));
        let mut huge = good.clone();
        huge[B + 0x18..B + 0x20].copy_from_slice(&(u64::MAX - BLOCK).to_le_bytes());
        assert!(refused(huge).contains("not one this reader knows"));

        // The container: no magic, an entry count past the bound, no digest table, a table of
        // the wrong size.
        let cnt_at = good.len() - 0x3000;
        let mut no_cnt = good.clone();
        no_cnt[cnt_at] = 0;
        assert!(refused(no_cnt).contains("\\x7FCNT"));
        let mut many = good.clone();
        many[cnt_at + 0x10..cnt_at + 0x14].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(refused(many).contains("entries"));
        let mut no_digests = good.clone();
        no_digests[cnt_at + 0x2000..cnt_at + 0x2004].copy_from_slice(&0x0001u32.to_be_bytes());
        assert!(refused(no_digests).contains("no block digest table"));
        let mut short = good.clone();
        short[cnt_at + 0x2014..cnt_at + 0x2018].copy_from_slice(&64u32.to_be_bytes());
        assert!(refused(short).contains("block digest table holds"));
        let mut past = good.clone();
        past[cnt_at + 0x2010..cnt_at + 0x2014].copy_from_slice(&0x2FF0u32.to_be_bytes());
        assert!(refused(past).contains("runs past its container"));
    }

    #[test]
    fn the_magic_names_a_patch() {
        assert!(is_lih(b"\x7FLIH\0\0"));
        assert!(!is_lih(b"\x7FFIH") && !is_lih(b"\x7FL"));
        let dir = crate::test_tmp::temp_path(format!("fpkg-lih-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (patch, whole) = (dir.join("patch.pkg"), dir.join("whole.pkg"));
        std::fs::write(&patch, b"\x7FLIH and the rest").unwrap();
        std::fs::write(&whole, b"\x7FFIH and the rest").unwrap();
        assert!(is_patch(&patch) && !is_patch(&whole) && !is_patch(&dir.join("absent.pkg")));
        std::fs::remove_file(&patch).ok();
        std::fs::remove_file(&whole).ok();
        std::fs::remove_dir(&dir).ok();
    }
}
