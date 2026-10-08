//! Write-once UFS2 image writer for the single ShadowMountPlus `.ffpkg` geometry.
//!
//! Frozen interface (ps5-dump-forge-core depends on it): `plan` validates names and sizes the image
//! without reading file data; `write` streams the files into `out` in offset order.
//!
//! The image is what `newfs -O 2 -b 65536 -f 65536 -m 0 -S 4096 -i <density>` would format
//! (no soft updates, no journal, no check hashes), filled the way a one-pass `makefs` would:
//! the raw filesystem starts at byte 0, every file is one contiguous run of 64 KiB blocks
//! with its indirect blocks in front of the blocks they map, and inodes are numbered
//! depth-first from the root (inode 2). Everything — the cylinder groups, the superblock
//! totals, every pointer — is computed by `plan`, so `write` never goes back.

mod bmap;
mod geom;
mod ondisk;
mod tree;

use std::io::{Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use ps5upload_fpkg::source::SourceTree;
use ps5upload_fpkg::{Error, Result};

use bmap::Seg;
use geom::{BSIZE, CBLKNO, Geom, IBLKNO, INOPB, SBLKNO};
use ondisk::{Csum, Dinode, IFDIR, IFREG};
use tree::{Kind, Node};

/// `UFS_ROOTINO`: inodes 0 and 1 are reserved.
pub(crate) const ROOTINO: u64 = 2;
/// File data is read and written in runs of at most this many bytes.
const CHUNK: u64 = 8 * 1024 * 1024;
/// Spare inodes kept for writable (`image_rw=`) mounts: the 2048 of SMP's `-i` formula,
/// guaranteed even where the 64 KiB density floor would eat into them (the image grows).
/// ponytail: ~2048 new files on a writable image; more needs a lower `-i` than SMP's.
const SPARE_INODES: u64 = 2048;
/// Free data blocks every image keeps for writable mounts: 64 MiB, like the floor of the
/// exFAT spare. ponytail: room for saves and small patches, not for installing DLC.
const SPARE_BLOCKS: u64 = 1024;

/// Knobs a caller may set. `Default` is the SMP-recommended layout.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Extra free space (bytes) beyond what the files need, for `image_rw=` mounts.
    pub free_bytes: u64,
}

/// Everything `write` needs, fixed before the first file byte is read.
#[derive(Debug, Clone)]
pub struct Layout {
    /// Final image size in bytes (a multiple of 64 KiB).
    pub image_size: u64,
    /// Cylinder groups (`fs_ncg`).
    pub cylinder_groups: u64,
    /// Blocks per full group (`fs_fpg`; one fragment per block).
    pub blocks_per_group: u64,
    /// Inodes per group (`fs_ipg`).
    pub inodes_per_group: u64,
    /// The `-i` the layout was built with.
    pub bytes_per_inode: u64,
    /// Blocks in the last group, and the metadata blocks every group starts with
    /// (`fs_dblkno`); the last group always holds more than its metadata.
    pub last_group_blocks: u64,
    pub metadata_blocks_per_group: u64,
    /// Free space and inodes left once the files are written.
    pub free_bytes: u64,
    pub free_inodes: u64,
    geom: Geom,
    nodes: Vec<Node>,
    files: u64,
    dirs: u64,
    file_bytes: u64,
    fs_id: u32,
    /// Hash of the tree's file list and empty dirs as planned; `write` refuses another.
    source: u64,
}

/// What was written.
#[derive(Debug, Clone)]
pub struct Report {
    pub image_size: u64,
    pub files: u64,
    /// Directories, the root included.
    pub dirs: u64,
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Format("UFS2 image cancelled".into()));
    }
    Ok(())
}

fn overflow() -> Error {
    Error::Format("UFS2 image size overflows".into())
}

/// FNV-1a over every planned path and size, so `write` can tell a renamed, reordered or
/// resized source from the one `plan` saw without keeping a second copy of the paths.
fn fingerprint(tree: &dyn SourceTree) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for f in tree.files() {
        eat(f.path.as_bytes());
        eat(&[0]);
        eat(&f.size.to_le_bytes());
    }
    eat(&[1]);
    for d in tree.empty_dirs() {
        eat(d.as_bytes());
        eat(&[0]);
    }
    h
}

/// Validate names and lay the image out. Fails with every offending path listed.
/// Checks `cancel` between steps.
pub fn plan(tree: &dyn SourceTree, opts: &Options, cancel: &AtomicBool) -> Result<Layout> {
    check_cancel(cancel)?;
    let mut nodes = tree::build(tree)?;
    check_cancel(cancel)?;

    let sizes: Vec<u64> = (0..nodes.len())
        .map(|i| match nodes[i].kind {
            Kind::Dir { .. } => tree::dir_size(&nodes, i),
            Kind::File { .. } => Ok(nodes[i].size),
        })
        .collect::<Result<_>>()?;
    let mut next = 0u64;
    let mut too_big = Vec::new();
    for (n, size) in nodes.iter_mut().zip(sizes) {
        n.size = size;
        n.start = next;
        let Some(slots) = bmap::total_slots(size.div_ceil(BSIZE)) else {
            too_big.push(n.name.to_string());
            continue;
        };
        n.slots = slots;
        next = next.checked_add(slots).ok_or_else(overflow)?;
    }
    if !too_big.is_empty() {
        return Err(Error::Format(format!(
            "file(s) larger than UFS2 can address with 64 KiB blocks: {}",
            too_big.join(", ")
        )));
    }
    let used_blocks = next;
    let objects = nodes.len() as u64;
    // The smallest image that holds the tree's blocks plus SPARE_BLOCKS free (and the
    // caller's extra), and its inodes plus SPARE_INODES. No percentage headroom: UFS2Tool's
    // `-D` (+10% + 10 MiB) left ~5 GiB free in a 50 GiB game. Metadata only ever adds to
    // the size, so the search starts at the data alone and grows by each shortfall.
    let data_blocks = used_blocks.checked_add(SPARE_BLOCKS).ok_or_else(overflow)?;
    let need_blocks = data_blocks
        .checked_add(opts.free_bytes.div_ceil(BSIZE))
        .ok_or_else(overflow)?;
    let need_inodes = ROOTINO + objects + SPARE_INODES;
    // -i from the tree and the spare, not the caller's extra free space (as ffpkg.sh).
    let density = geom::density_for(data_blocks.saturating_mul(BSIZE), objects);
    let geom = size_image(
        need_blocks.max(16),
        density,
        need_blocks,
        need_inodes,
        cancel,
    )?;
    let image_size = geom.size.checked_mul(BSIZE).ok_or_else(overflow)?;

    let dirs = nodes.iter().filter(|n| n.is_dir()).count() as u64;
    let file_bytes = nodes
        .iter()
        .filter(|n| !n.is_dir())
        .try_fold(0u64, |a, n| a.checked_add(n.size))
        .ok_or_else(overflow)?;
    // Deterministic volume id: a hash of the shape of the tree.
    let mut h: u32 = 0x811c_9dc5;
    for v in [image_size, objects, file_bytes, used_blocks] {
        for b in v.to_le_bytes() {
            h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
        }
    }
    Ok(Layout {
        image_size,
        cylinder_groups: geom.ncg,
        blocks_per_group: geom.fpg,
        inodes_per_group: geom.ipg,
        bytes_per_inode: geom.density,
        last_group_blocks: geom.cg_blocks(geom.ncg - 1),
        metadata_blocks_per_group: geom.dblkno,
        free_bytes: (geom.dsize() - used_blocks) * BSIZE,
        free_inodes: geom.inode_capacity() - ROOTINO - objects,
        geom,
        files: objects - dirs,
        dirs,
        file_bytes,
        fs_id: h,
        source: fingerprint(tree),
        nodes,
    })
}

/// The smallest newfs layout from `start` blocks up whose groups hold `need_blocks` data
/// blocks and `need_inodes` inodes and whose last group is sound. Capacity is not monotonic
/// in the size (inodes come in whole blocks per group, groups re-balance as the size
/// changes), so no step can be skipped: every size is tried, from `start`, which must be a
/// lower bound (metadata only takes from the data, so `need_blocks` is one).
/// ponytail: linear; a data-bound image tries about its metadata in sizes (~10k for 150
/// GiB), an inode-bound one up to its inode count (~1M for 1M empty files), each a
/// logarithmic `Geom::new`.
fn size_image(
    start: u64,
    density: u64,
    need_blocks: u64,
    need_inodes: u64,
    cancel: &AtomicBool,
) -> Result<Geom> {
    for size in start.. {
        if size % 4096 == 0 {
            check_cancel(cancel)?;
        }
        let g = Geom::new(size, density)?;
        if g.last_group_ok()
            && g.data_capacity() >= need_blocks
            && g.inode_capacity() >= need_inodes
        {
            return Ok(g);
        }
    }
    Err(overflow())
}

/// Write the image described by `layout` into `out` (empty: a fresh file the caller owns,
/// fsyncs and renames, or a `.ffpfsc` stream). `progress(done, total)` counts file-data bytes.
pub fn write<W: Write + Seek>(
    tree: &mut dyn SourceTree,
    layout: &Layout,
    out: &mut W,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Report> {
    check_cancel(cancel)?;
    // The plan indexes the tree's file list; a different list would write wrong data.
    if fingerprint(tree) != layout.source {
        return Err(Error::Format(
            "the source changed between planning and writing the UFS2 image".into(),
        ));
    }
    if out.seek(SeekFrom::End(0))? != 0 {
        // Unwritten regions are left as holes (or zero-filled by a stream) and must read
        // back as zeros.
        return Err(Error::Format("the UFS2 output is not empty".into()));
    }
    let mut w = Writer {
        out,
        pos: 0,
        layout,
        next_cg: 0,
        cs: Vec::new(),
        total: Csum::default(),
        cancel,
    };
    w.cs = group_summaries(layout);
    w.total = w.cs.iter().fold(Csum::default(), |t, u| Csum {
        ndir: t.ndir + u.cs.ndir,
        nbfree: t.nbfree + u.cs.nbfree,
        nifree: t.nifree + u.cs.nifree,
    });
    let total = layout.file_bytes;
    let mut done = 0u64;
    progress(0, total);

    let g = &layout.geom;
    for (i, node) in layout.nodes.iter().enumerate() {
        check_cancel(cancel)?;
        let (dir_bytes, path) = match node.kind {
            Kind::Dir { .. } => (Some(tree::dir_bytes(&layout.nodes, i)), String::new()),
            Kind::File { src } => (None, tree.files()[src].path.clone()),
        };
        for seg in bmap::segments(node.size.div_ceil(BSIZE)) {
            match seg {
                Seg::Indirect { slot, level, k } => {
                    let mut block = vec![0u8; BSIZE as usize];
                    for (j, child) in bmap::children(slot, level, k).enumerate() {
                        let p = g.locate(node.start + child).0;
                        block[j * 8..j * 8 + 8].copy_from_slice(&p.to_le_bytes());
                    }
                    w.data(g.locate(node.start + slot).0, &block)?;
                }
                Seg::Data { slot, lbn, count } => {
                    let mut b = 0;
                    while b < count {
                        check_cancel(cancel)?;
                        let (p, contiguous) = g.locate(node.start + slot + b);
                        let n = (count - b).min(contiguous).min(CHUNK / BSIZE);
                        let off = (lbn + b) * BSIZE;
                        let len = (n * BSIZE).min(node.size - off) as usize;
                        if let Some(bytes) = &dir_bytes {
                            w.data(p, &bytes[off as usize..off as usize + len])?;
                        } else {
                            let buf = tree.read_range(&path, off, len)?;
                            if buf.len() != len {
                                return Err(Error::Format(format!(
                                    "{path}: source file shrank while the UFS2 image was written"
                                )));
                            }
                            w.data(p, &buf)?;
                            done += len as u64;
                            progress(done, total);
                        }
                        b += n;
                    }
                }
            }
        }
    }
    w.meta_before(u64::MAX)?;
    if w.pos > layout.image_size {
        // The UFS2Tool v4.1 failure mode; the layout makes it impossible, so it's a bug.
        return Err(Error::Format(format!(
            "internal error: UFS2 writer reached byte {} of a {}-byte image",
            w.pos, layout.image_size
        )));
    }
    // One zero byte at the end extends the image: sparse on a fresh file, zero-filled by a
    // stream.
    if w.pos < layout.image_size {
        w.put(layout.image_size - 1, &[0])?;
    }
    w.out.flush()?;
    progress(total, total);
    check_cancel(cancel)?;
    Ok(Report {
        image_size: layout.image_size,
        files: layout.files,
        dirs: layout.dirs,
    })
}

/// Per group: the summary, how many inodes and allocatable blocks are in use.
struct GroupUse {
    cs: Csum,
    inodes: u64,
    blocks: u64,
}

fn group_summaries(layout: &Layout) -> Vec<GroupUse> {
    let g = &layout.geom;
    let inodes_used = ROOTINO + layout.nodes.len() as u64;
    let blocks_used: u64 = layout.nodes.iter().map(|n| n.slots).sum();
    let mut out: Vec<GroupUse> = (0..g.ncg)
        .map(|c| {
            let inodes = inodes_used.saturating_sub(c * g.ipg).min(g.ipg);
            let (start, end) = g.cg_data_range(c);
            let blocks = blocks_used.clamp(start, end) - start;
            let before_sb = if c > 0 { SBLKNO } else { 0 };
            GroupUse {
                cs: Csum {
                    ndir: 0,
                    nbfree: before_sb + g.cg_data(c) - blocks,
                    nifree: g.ipg - inodes,
                },
                inodes,
                blocks,
            }
        })
        .collect();
    for (i, n) in layout.nodes.iter().enumerate() {
        if n.is_dir() {
            out[((ROOTINO + i as u64) / g.ipg) as usize].cs.ndir += 1;
        }
    }
    out
}

/// Writes strictly forward through the image, emitting each cylinder group's metadata
/// (backup superblock, cg block, inodes; in group 0 also the primary superblock and the
/// summary area) just before the first data block that follows it.
struct Writer<'a, W: Write + Seek> {
    out: &'a mut W,
    pos: u64,
    layout: &'a Layout,
    next_cg: u64,
    cs: Vec<GroupUse>,
    /// `fs_cstotal`: the sum of `cs`.
    total: Csum,
    cancel: &'a AtomicBool,
}

impl<W: Write + Seek> Writer<'_, W> {
    fn put(&mut self, at: u64, bytes: &[u8]) -> Result<()> {
        if at < self.pos {
            return Err(Error::Format(format!(
                "internal error: UFS2 writer went back from {} to {at}",
                self.pos
            )));
        }
        if at > self.pos {
            self.out.seek(SeekFrom::Start(at))?;
        }
        self.out.write_all(bytes)?;
        self.pos = at + bytes.len() as u64;
        Ok(())
    }

    fn data(&mut self, block: u64, bytes: &[u8]) -> Result<()> {
        self.meta_before(block)?;
        self.put(block * BSIZE, bytes)
    }

    /// Writes the metadata of every group that starts at or before `block`.
    fn meta_before(&mut self, block: u64) -> Result<()> {
        let g = self.layout.geom;
        while self.next_cg < g.ncg && self.next_cg * g.fpg <= block {
            check_cancel(self.cancel)?;
            let c = self.next_cg;
            self.group_meta(c)?;
            self.next_cg += 1;
        }
        Ok(())
    }

    fn group_meta(&mut self, c: u64) -> Result<()> {
        let layout = self.layout;
        let g = layout.geom;
        let total = self.total;
        let base = c * g.fpg * BSIZE;
        if c == 0 {
            self.put(ondisk::SBLOCK_UFS2 - 20, &ondisk::fsrecovery(&g))?;
            let sb = ondisk::superblock(&g, &total, layout.fs_id, ondisk::SBLOCK_UFS2);
            self.put(ondisk::SBLOCK_UFS2, &sb)?;
        }
        let at = base + SBLKNO * BSIZE;
        self.put(at, &ondisk::superblock(&g, &total, layout.fs_id, at))?;
        let u = &self.cs[c as usize];
        let cg = ondisk::cylinder_group(&g, c, u.inodes, u.blocks, &u.cs);
        let used = u.inodes;
        self.put(base + CBLKNO * BSIZE, &cg)?;

        for blk in 0..used.div_ceil(INOPB) {
            let mut buf = vec![0u8; BSIZE as usize];
            for j in 0..INOPB.min(used - blk * INOPB) {
                let ino = c * g.ipg + blk * INOPB + j;
                if ino < ROOTINO {
                    continue;
                }
                let node = &layout.nodes[(ino - ROOTINO) as usize];
                let at = (j * 256) as usize;
                dinode(&g, node).write(&mut buf[at..at + 256]);
            }
            self.put(base + (IBLKNO + blk) * BSIZE, &buf)?;
        }
        if c == 0 {
            let mut sum = vec![0u8; (g.csblocks * BSIZE) as usize];
            for (i, u) in self.cs.iter().enumerate() {
                let at = i * 16;
                for (k, v) in [u.cs.ndir, u.cs.nbfree, u.cs.nifree, 0].iter().enumerate() {
                    sum[at + 4 * k..at + 4 * k + 4].copy_from_slice(&(*v as u32).to_le_bytes());
                }
            }
            self.put(g.dblkno * BSIZE, &sum)?;
        }
        Ok(())
    }
}

fn dinode(g: &Geom, node: &Node) -> Dinode {
    let nblocks = node.size.div_ceil(BSIZE);
    let mut db = [0u64; 12];
    for (i, p) in db.iter_mut().enumerate().take(nblocks.min(12) as usize) {
        *p = g.locate(node.start + i as u64).0;
    }
    let roots = bmap::indirect_roots(nblocks);
    let ib = roots.map(|r| r.map_or(0, |s| g.locate(node.start + s).0));
    let (mode, dirdepth) = match node.kind {
        Kind::Dir { depth, .. } => (IFDIR | 0o777, depth),
        Kind::File { .. } => (IFREG | 0o777, 0),
    };
    Dinode {
        mode,
        nlink: node.nlink(),
        size: node.size,
        blocks: node.slots * (BSIZE / 512),
        db,
        ib,
        dirdepth,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sizing search converges for every small need and yields a sound last group.
    #[test]
    fn sizing_converges() {
        let cancel = AtomicBool::new(false);
        for need in (0..20_000u64).step_by(7) {
            for objects in [1u64, 50, 5000] {
                let density = geom::density_for(need * BSIZE, objects);
                let inodes = ROOTINO + objects + SPARE_INODES;
                let g = size_image(need.max(16), density, need, inodes, &cancel).unwrap();
                assert!(g.last_group_ok(), "need {need}");
                assert!(g.data_capacity() >= need);
                assert!(g.inode_capacity() >= inodes);
            }
        }
    }

    /// Inode-bound layouts, where inode capacity holds still over many sizes: the search
    /// must not give up there, and must land on the first size that fits (a jump-then-
    /// backtrack search returned 797 128 blocks for the second, 6 GiB too many).
    #[test]
    fn inode_bound_sizing_is_exact() {
        let cancel = AtomicBool::new(false);
        for (need, inodes, want) in [
            (819_201u64, 870_401u64, None),
            (1221, 802_051, Some(696_325)),
        ] {
            let g = size_image(need, 65536, need, inodes, &cancel).unwrap();
            assert!(g.last_group_ok() && g.data_capacity() >= need);
            assert!(g.inode_capacity() >= inodes);
            if let Some(want) = want {
                assert_eq!(g.size, want);
            }
        }
    }
}
