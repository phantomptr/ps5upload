//! In-place editing of a Forge-built UFS2 image (`.ffpkg`): the read side. [`Volume::open`] reads
//! the superblock, checks it is the one geometry Forge's writer produces, and loads every cylinder
//! group with its bitmaps and summary, cross-checked against the totals.
//!
//! The cylinder group blocks (64 KiB each) and the summary area are held in memory whole, within
//! [`MEMORY_LIMIT`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

use ps5_dump_forge_patch::{Data, Op, Write};

use crate::ROOTINO;
use crate::bmap::{NDADDR, Seg, children, indirect_roots, segments, span, total_slots};
use crate::geom::{
    BSIZE, CBLKNO, CG_HDR, CONTIGSUMSIZE, CSUM_SIZE, Geom, IBLKNO, INOPB, NINDIR, SBLKNO,
};
use crate::ondisk::{
    CG_MAGIC, Csum, Dinode, FS_UFS2_MAGIC, IFDIR, IFREG, SBLOCK_UFS2, SBLOCKSIZE, superblock,
};
use crate::tree::{DIRBLKSIZ, DT_DIR, DT_REG, MAXDIRSIZE, MAXPATHLEN, bad_component, dirsiz};

/// The superblock, cylinder group blocks and the summary area, at most.
// ponytail: 128 MiB holds 2048 groups (~32 TiB at Forge's group sizes); past it, rebuild.
pub const MEMORY_LIMIT: u64 = 128 << 20;

/// `fs_flags` bits. Unclean / needs fsck: refused as not cleanly unmounted. Soft updates,
/// the SU journal, gjournal and metadata check hashes change what is on disk: another tool.
/// Every other bit (TRIM, ACLs, multilabel, ...) the console's kernel may set on an
/// `image_rw=` mount, so it passes.
const FS_UNCLEAN: u32 = 0x01;
const FS_NEEDSFSCK: u32 = 0x04;
const FS_FLAGS_UPDATED: u32 = 0x80;
const CHANGES_LAYOUT: u32 = 0x02 | 0x08 | 0x40 | 0x200; // DOSOFTDEP, SUJ, GJOURNAL, METACKHASH

/// Superblock fields (name, offset, bytes) that must equal what the writer puts there for
/// the image's geometry: block/fragment sizes, shifts and masks, disk-address conversion,
/// the group layout, the summary area and the file-size limits.
const GEOMETRY_FIELDS: [(&str, usize, usize); 35] = [
    ("fs_sblkno", 8, 4),
    ("fs_cblkno", 12, 4),
    ("fs_iblkno", 16, 4),
    ("fs_dblkno", 20, 4),
    ("fs_ncg", 44, 4),
    ("fs_bsize", 48, 4),
    ("fs_fsize", 52, 4),
    ("fs_frag", 56, 4),
    ("fs_minfree", 60, 4),
    ("fs_bmask", 72, 4),
    ("fs_fmask", 76, 4),
    ("fs_bshift", 80, 4),
    ("fs_fshift", 84, 4),
    ("fs_maxcontig", 88, 4),
    ("fs_maxbpg", 92, 4),
    ("fs_fragshift", 96, 4),
    ("fs_fsbtodb", 100, 4),
    ("fs_sbsize", 104, 4),
    ("fs_nindir", 116, 4),
    ("fs_inopb", 120, 4),
    ("fs_cssize", 156, 4),
    ("fs_cgsize", 160, 4),
    ("fs_ipg", 184, 4),
    ("fs_fpg", 188, 4),
    ("fs_maxbsize", 860, 4),
    ("fs_sblockloc", 1000, 8),
    ("fs_size", 1080, 8),
    ("fs_dsize", 1088, 8),
    ("fs_csaddr", 1096, 8),
    ("fs_contigsumsize", 1316, 4),
    ("fs_maxsymlinklen", 1320, 4),
    ("fs_old_inodefmt", 1324, 4),
    ("fs_maxfilesize", 1328, 8),
    ("fs_qbmask", 1336, 8),
    ("fs_qfmask", 1344, 8),
];

/// One cylinder group: its header block as on disk, and where its maps start in it.
#[derive(Debug, Clone)]
pub struct Group {
    /// The whole cg block (`fs_cgsize` = 64 KiB), as read.
    pub raw: Vec<u8>,
    /// `cg_cs`.
    pub cs: Csum,
    /// Offsets in `raw`: inode map, free-block map, cluster summary, cluster map.
    pub iusedoff: usize,
    pub freeoff: usize,
    pub clustersumoff: usize,
    pub clusteroff: usize,
    /// `cg_initediblk`: inodes known to be zeroed or written.
    pub initediblk: u64,
}

/// An opened UFS2 volume, its allocation metadata read into memory.
#[derive(Debug, Clone)]
pub struct Volume {
    pub(crate) geom: Geom,
    /// The primary superblock (`fs_sbsize` bytes at [`SBLOCK_UFS2`]), as read.
    pub sb: Vec<u8>,
    /// Every cylinder group, by index.
    pub groups: Vec<Group>,
    /// The summary area (`fs_cs`, one per group), as read.
    pub summary: Vec<Csum>,
    /// `fs_cstotal`.
    pub total: Csum,
    /// [`MEMORY_LIMIT`] (lowered by tests).
    limit: u64,
    /// What [`plan`]'s ops changed so far; reads see it.
    changes: Changes,
}

/// The planned change set. Group blocks, the summary and the totals are changed in place in
/// [`Volume`] (`groups`, `summary`, `total`); inodes and directory chunks are kept here as
/// modified copies, each written once. Blocks this plan frees are not reused by it.
#[derive(Debug, Clone, Default)]
struct Changes {
    /// Rewritten inodes (256 B) and directory chunks (512 B), by image byte offset; reads
    /// overlay them.
    meta: BTreeMap<u64, Vec<u8>>,
    /// Groups whose block (maps, `cg_cs`) changed.
    groups: BTreeSet<u64>,
    /// Folder listings already read, by directory inode, kept current as ops change them,
    /// so a plan lists each folder once.
    listings: BTreeMap<u64, Vec<Node>>,
    /// Bytes the above hold, and what a folder delete's stack holds: charged to the budget.
    bytes: u64,
    /// New file data, in free space (never overlapping anything the image or the plan uses).
    data: Vec<Write>,
    /// New indirect blocks, by block number: written into free space beside `data`; block
    /// maps read them here (a file this plan added may be replaced or deleted by a later op).
    fresh: BTreeMap<u64, Vec<u8>>,
    /// Each group's free-block map as it was before this plan freed a block in it, with every
    /// later free cleared: a block is taken only when free in both, so blocks this plan frees
    /// (its own new ones included) are never reused by it.
    pristine: BTreeMap<u64, Vec<u8>>,
    /// Blocks this plan freed (free now, not takeable).
    freed: u64,
    /// Blocks taken so far, and blocks the plan lacks room for (it is then refused).
    taken: u64,
    short: u64,
    /// New inodes the plan lacks (it is then refused).
    short_inodes: u64,
    /// Folders a Put or Mkdir needed but the plan lacked room for (already counted in
    /// `short`/`short_inodes`), so each is counted once.
    missing: BTreeSet<String>,
    /// Per folder, the room left in the growth block(s) the plan lacked (already counted in
    /// `short`), so later entries in that folder count no more until it is used up.
    pending: BTreeMap<u64, Pending>,
    /// Per folder, where [`Volume::find_slot`] starts looking (so adding n entries to one
    /// folder reads it about once, not n times).
    // ponytail: not charged to the budget; at most one per folder listed, which is.
    hints: BTreeMap<u64, Hint>,
    /// Every scan from the first chunk (tests compare the plans).
    #[cfg(test)]
    no_hints: bool,
    /// Folders listed so far (tests count them).
    #[cfg(test)]
    listed: std::cell::Cell<u64>,
}

/// Budget charged per change-set record beside its bytes (map node, vector header).
const RECORD: u64 = 64;

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn le64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn read_at(image: &mut File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    image.seek(SeekFrom::Start(offset))?;
    image.read_exact(buf)
}

fn other_tool(why: impl std::fmt::Display) -> Vec<String> {
    vec![format!(
        "written by another tool (UFS2 {why}): convert it first"
    )]
}

fn damaged(why: impl std::fmt::Display) -> Vec<String> {
    vec![format!("the image is damaged (UFS2 {why})")]
}

fn too_big(total: u64) -> Vec<String> {
    vec![format!(
        "its cylinder groups are too big to edit in place ({} MiB); rebuild it instead",
        total.div_ceil(1 << 20)
    )]
}

/// The primary superblock's geometry, if it is the one Forge's writer lays out for its
/// `fs_size`: every fixed field newfs sets for `-O 2 -b 65536 -f 65536 -m 0 -S 4096`, and
/// the fpg/ipg/ncg [`Geom::new`] computes at one of the densities the writer uses.
fn check_superblock(sb: &[u8], file_len: u64) -> Result<Geom, Vec<String>> {
    let f32 = |at: usize| le32(sb, at).unwrap_or(0);
    let f64 = |at: usize| le64(sb, at).unwrap_or(0);
    if f32(1372) != FS_UFS2_MAGIC {
        return Err(other_tool("magic"));
    }
    // FreeBSD reads `fs_old_flags` as the flags until FS_FLAGS_UPDATED moves them.
    let old = u32::from(sb.get(211).copied().unwrap_or(0));
    let flags = if old & FS_FLAGS_UPDATED != 0 {
        f32(1312)
    } else {
        old
    };
    if flags & CHANGES_LAYOUT != 0 || f32(1308) != 0 {
        return Err(other_tool(format!(
            "fs_flags {flags:#x}, fs_metackhash {:#x}",
            f32(1308)
        )));
    }
    if sb.get(209) == Some(&0) || flags & (FS_UNCLEAN | FS_NEEDSFSCK) != 0 {
        return Err(vec![
            "the image was not unmounted cleanly (UFS2 fs_clean): it can't be edited in \
             place; rebuild it instead"
                .into(),
        ]);
    }
    let size = f64(1080);
    let (fpg, ipg, ncg) = (u64::from(f32(188)), u64::from(f32(184)), u64::from(f32(44)));
    // fs_fpg / fs_ipg as the writer computes them at density 64..256 KiB (fpi 1..4).
    let geom = (1..=4)
        .filter_map(|fpi| Geom::new(size, fpi * BSIZE).ok())
        .find(|g| g.fpg == fpg && g.ipg == ipg && g.ncg == ncg && g.last_group_ok())
        .ok_or_else(|| other_tool(format!("geometry: {size} blocks, fpg {fpg}, ipg {ipg}")))?;
    // Every field the writer derives from that geometry, against the writer's own bytes.
    // PS5Upload: this vendored writer predates fs_volname (outside GEOMETRY_FIELDS anyway).
    let want = superblock(&geom, &Csum::default(), 0, SBLOCK_UFS2);
    for (name, at, len) in GEOMETRY_FIELDS {
        let (got, exp) = (sb.get(at..at + len), want.get(at..at + len));
        if got.is_none() || got != exp {
            return Err(other_tool(format!("{name} {:?}", got.unwrap_or_default())));
        }
    }
    match size.checked_mul(BSIZE) {
        Some(bytes) if bytes <= file_len => Ok(geom),
        _ => Err(damaged(format!(
            "the file is shorter than its {size} blocks"
        ))),
    }
}

/// Bit `i` of the map at `base` in `raw` (false past the end).
pub(crate) fn bit(raw: &[u8], base: usize, i: u64) -> bool {
    let at = base as u64 + i / 8;
    usize::try_from(at)
        .ok()
        .and_then(|at| raw.get(at))
        .is_some_and(|b| b & (1 << (i % 8)) != 0)
}

/// The cluster summary of a free map of `n` blocks: `sum[k]` counts the free runs of
/// length `k` (runs of [`CONTIGSUMSIZE`] or more in the last slot); slot 0 is unused.
pub(crate) fn cluster_sum(raw: &[u8], base: usize, n: u64) -> [u32; CONTIGSUMSIZE as usize + 1] {
    let mut sum = [0u32; CONTIGSUMSIZE as usize + 1];
    let mut run = 0u64;
    for i in 0..=n {
        if i < n && bit(raw, base, i) {
            run += 1;
        } else if run > 0 {
            let slot = run.min(CONTIGSUMSIZE) as usize;
            sum[slot] = sum[slot].saturating_add(1);
            run = 0;
        }
    }
    sum
}

/// Parses and checks cylinder group `c`: the header the writer lays out for this geometry,
/// `cg_cs` against the maps, the cluster map and summary against the free map, and no
/// metadata block marked free.
fn read_group(g: &Geom, c: u64, raw: Vec<u8>) -> Result<Group, Vec<String>> {
    let bad = |why: String| damaged(format!("cylinder group {c}: {why}"));
    let f32 = |at: usize| u64::from(le32(&raw, at).unwrap_or(0));
    let ndblk = g.cg_blocks(c);
    let iusedoff = CG_HDR;
    let freeoff = iusedoff + g.ipg.div_ceil(8);
    let clustersumoff = (freeoff + g.fpg.div_ceil(8)).next_multiple_of(4) - 4;
    let clusteroff = clustersumoff + (CONTIGSUMSIZE + 1) * 4;
    let header: [(&str, usize, u64); 10] = [
        ("cg_magic", 4, u64::from(CG_MAGIC)),
        ("cg_cgx", 12, c),
        ("cg_ndblk", 20, ndblk),
        ("cg_iusedoff", 92, iusedoff),
        ("cg_freeoff", 96, freeoff),
        ("cg_nextfreeoff", 100, clusteroff + g.fpg.div_ceil(8)),
        ("cg_clustersumoff", 104, clustersumoff),
        ("cg_clusteroff", 108, clusteroff),
        ("cg_nclusterblks", 112, ndblk),
        ("cg_niblk", 116, g.ipg),
    ];
    for (name, at, want) in header {
        if f32(at) != want {
            return Err(bad(format!("{name} {}, expected {want}", f32(at))));
        }
    }
    let cs = Csum {
        ndir: f32(24),
        nbfree: f32(28),
        nifree: f32(32),
    };
    if f32(36) != 0 {
        return Err(bad(format!("{} free fragments", f32(36))));
    }
    let initediblk = f32(120);
    if initediblk > g.ipg || initediblk % INOPB != 0 {
        return Err(bad(format!("cg_initediblk {initediblk}")));
    }
    let (iusedoff, freeoff) = (iusedoff as usize, freeoff as usize);
    let (clustersumoff, clusteroff) = (clustersumoff as usize, clusteroff as usize);
    let used = (0..g.ipg).filter(|&i| bit(&raw, iusedoff, i)).count() as u64;
    // An allocated inode past the watermark would be zeroed by the next initialisation.
    if let Some(i) = (initediblk..g.ipg).find(|&i| bit(&raw, iusedoff, i)) {
        return Err(bad(format!(
            "inode {} is allocated past cg_initediblk {initediblk}",
            c * g.ipg + i
        )));
    }
    if used + cs.nifree != g.ipg {
        return Err(bad(format!(
            "{} free inodes counted, {} mapped",
            cs.nifree,
            g.ipg - used
        )));
    }
    if (0..ROOTINO).any(|i| c == 0 && !bit(&raw, iusedoff, i)) {
        return Err(bad("reserved inodes 0 and 1 are free".into()));
    }
    // Blocks no allocation may take: group 0 from block 0 through the summary area, the
    // others their backup superblock, cg block and inodes.
    let (meta_lo, meta_hi) = match c {
        0 => (0, g.dblkno + g.csblocks),
        _ => (SBLKNO, g.dblkno),
    };
    if let Some(b) = (meta_lo..meta_hi).find(|&b| bit(&raw, freeoff, b)) {
        return Err(bad(format!("metadata block {b} is marked free")));
    }
    let free = (0..ndblk).filter(|&b| bit(&raw, freeoff, b)).count() as u64;
    if free != cs.nbfree {
        return Err(bad(format!(
            "{} free blocks counted, {free} mapped",
            cs.nbfree
        )));
    }
    if let Some(b) = (0..ndblk).find(|&b| bit(&raw, freeoff, b) != bit(&raw, clusteroff, b)) {
        return Err(bad(format!(
            "cluster map differs from the free map at block {b}"
        )));
    }
    let sum = cluster_sum(&raw, clusteroff, ndblk);
    for (k, want) in sum.iter().enumerate().skip(1) {
        let got = le32(&raw, clustersumoff + 4 * k).unwrap_or(0);
        if got != *want {
            return Err(bad(format!(
                "cluster summary of {k}-block runs {got}, mapped {want}"
            )));
        }
    }
    Ok(Group {
        raw,
        cs,
        iusedoff,
        freeoff,
        clustersumoff,
        clusteroff,
        initediblk,
    })
}

/// Every allocated inode is a file or a folder, and each group's `cs_ndir` counts its
/// folders. Reads the inode blocks holding allocated inodes, one 64 KiB block at a time
/// (within [`peak`]: the summary area is dropped first).
fn count_dirs(image: &mut File, g: &Geom, groups: &[Group]) -> io::Result<Result<(), Vec<String>>> {
    let mut block = vec![0u8; BSIZE as usize];
    for (c, grp) in (0u64..).zip(groups) {
        let mut ndir = 0u64;
        for k in 0..grp.initediblk / INOPB {
            let used: Vec<u64> = (k * INOPB..(k + 1) * INOPB)
                .filter(|&i| bit(&grp.raw, grp.iusedoff, i) && c * g.ipg + i >= ROOTINO)
                .collect();
            if used.is_empty() {
                continue;
            }
            read_at(image, (c * g.fpg + IBLKNO + k) * BSIZE, &mut block)?;
            for i in used {
                let at = ((i - k * INOPB) * DINODE) as usize;
                let mode = block
                    .get(at..at + 2)
                    .map_or(0, |s| u16::from_le_bytes([s[0], s[1]]));
                let ino = c * g.ipg + i;
                match mode & IFMT {
                    IFDIR => ndir += 1,
                    IFREG => {}
                    0 => return Ok(Err(damaged(format!("inode {ino}: allocated with mode 0")))),
                    t => {
                        return Ok(Err(other_tool(format!(
                            "inode {ino}: mode {t:#o}, not a file or folder"
                        ))));
                    }
                }
            }
        }
        if ndir != grp.cs.ndir {
            return Ok(Err(damaged(format!(
                "cylinder group {c}: {} directories counted, {ndir} allocated",
                grp.cs.ndir
            ))));
        }
    }
    Ok(Ok(()))
}

/// What an open volume keeps: the superblock, every group (its block and its `Group`) and
/// the decoded summary.
fn held(g: &Geom) -> u64 {
    let per_group = BSIZE + (size_of::<Group>() + size_of::<Csum>()) as u64;
    (SBLOCKSIZE as u64).saturating_add(g.ncg.saturating_mul(per_group))
}

/// The most [`Volume::open`] holds at once: [`held`] plus the raw summary area while it is
/// decoded.
fn peak(g: &Geom) -> u64 {
    held(g).saturating_add(g.csblocks.saturating_mul(BSIZE))
}

impl Volume {
    /// Reads and checks the primary superblock, every cylinder group and the summary area.
    /// `Ok(Err(findings))` when the image can't be edited in place: another tool's geometry,
    /// not cleanly unmounted, damaged counts, or metadata past [`MEMORY_LIMIT`].
    pub fn open(image: &mut File) -> io::Result<Result<Volume, Vec<String>>> {
        Self::open_within(image, MEMORY_LIMIT)
    }

    fn open_within(image: &mut File, limit: u64) -> io::Result<Result<Volume, Vec<String>>> {
        let file_len = image.metadata()?.len();
        if file_len < SBLOCK_UFS2 + SBLOCKSIZE as u64 {
            return Ok(Err(other_tool("superblock missing")));
        }
        let mut sb = vec![0u8; SBLOCKSIZE];
        read_at(image, SBLOCK_UFS2, &mut sb)?;
        let g = match check_superblock(&sb, file_len) {
            Ok(g) => g,
            Err(f) => return Ok(Err(f)),
        };
        let need = peak(&g);
        if need > limit {
            return Ok(Err(too_big(need)));
        }
        let mut groups = Vec::with_capacity(g.ncg as usize);
        for c in 0..g.ncg {
            let mut raw = vec![0u8; BSIZE as usize];
            read_at(image, (c * g.fpg + CBLKNO) * BSIZE, &mut raw)?;
            match read_group(&g, c, raw) {
                Ok(group) => groups.push(group),
                Err(f) => return Ok(Err(f)),
            }
        }
        let mut area = vec![0u8; (g.csblocks * BSIZE) as usize];
        read_at(image, g.dblkno * BSIZE, &mut area)?;
        let mut summary = Vec::with_capacity(g.ncg as usize);
        for (c, group) in groups.iter().enumerate() {
            let at = c * CSUM_SIZE as usize;
            let w = |k: usize| u64::from(le32(&area, at + 4 * k).unwrap_or(0));
            let cs = Csum {
                ndir: w(0),
                nbfree: w(1),
                nifree: w(2),
            };
            if cs != group.cs || w(3) != 0 {
                return Ok(Err(damaged(format!(
                    "cylinder group {c}: summary {cs:?}, cylinder group {:?}",
                    group.cs
                ))));
            }
            summary.push(cs);
        }
        let sum = summary.iter().fold(Csum::default(), |t, s| Csum {
            ndir: t.ndir + s.ndir,
            nbfree: t.nbfree + s.nbfree,
            nifree: t.nifree + s.nifree,
        });
        let f64 = |at: usize| le64(&sb, at).unwrap_or(0);
        let total = Csum {
            ndir: f64(1008),
            nbfree: f64(1016),
            nifree: f64(1024),
        };
        if total != sum || f64(1032) != 0 {
            return Ok(Err(damaged(format!(
                "fs_cstotal {total:?}, cylinder groups {sum:?}"
            ))));
        }
        drop(area);
        if let Err(f) = count_dirs(image, &g, &groups)? {
            return Ok(Err(f));
        }
        Ok(Ok(Volume {
            geom: g,
            sb,
            groups,
            summary,
            total,
            limit,
            changes: Changes::default(),
        }))
    }

    /// `fs_size`, in 64 KiB blocks.
    pub fn blocks(&self) -> u64 {
        self.geom.size
    }

    /// `fs_ncg`, `fs_fpg`, `fs_ipg`.
    pub fn cylinder_groups(&self) -> u64 {
        self.geom.ncg
    }
    pub fn blocks_per_group(&self) -> u64 {
        self.geom.fpg
    }
    pub fn inodes_per_group(&self) -> u64 {
        self.geom.ipg
    }

    /// Bytes of the budget the open volume holds: the superblock, the groups and the summary.
    pub fn held(&self) -> u64 {
        held(&self.geom)
    }

    /// What the budget leaves for later reads (inodes, directories) beside the change set.
    pub fn budget_left(&self) -> u64 {
        self.limit
            .saturating_sub(self.held())
            .saturating_sub(self.changes.bytes)
    }

    /// Free bytes: every free block (`fs_cstotal.cs_nbfree`) at 64 KiB.
    pub fn free_bytes(&self) -> u64 {
        self.total.nbfree * BSIZE
    }

    /// Free inodes (`fs_cstotal.cs_nifree`).
    pub fn free_inodes(&self) -> u64 {
        self.total.nifree
    }
}

/// Bytes of one `ufs2_dinode`.
const DINODE: u64 = 256;
/// `di_blocks` counts DEV_BSIZE (512-byte) units.
const SECTORS_PER_BLOCK: u64 = BSIZE / 512;
const IFMT: u16 = 0o170000;

/// What an inode holds: Forge writes files and folders only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
}

/// One inode as read (`struct ufs2_dinode`), the fields the editor uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inode {
    pub ino: u64,
    pub kind: Kind,
    /// `di_mode`, type bits included.
    pub mode: u16,
    pub nlink: u16,
    pub size: u64,
    /// `di_blocks`, in 512-byte units.
    pub blocks: u64,
    pub db: [u64; NDADDR as usize],
    pub ib: [u64; 3],
}

/// `len` logical blocks from `lbn` at physical blocks `block..`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub lbn: u64,
    pub block: u64,
    pub len: u64,
}

/// Every block an inode owns: its data as runs in logical order (holes left out), and its
/// indirect blocks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockMap {
    pub data: Vec<Run>,
    pub indirect: Vec<u64>,
}

impl BlockMap {
    /// Physical block of logical block `lbn`, `None` for a hole.
    pub fn physical(&self, lbn: u64) -> Option<u64> {
        let i = self.data.partition_point(|r| r.lbn + r.len <= lbn);
        let r = self.data.get(i)?;
        (r.lbn <= lbn).then(|| r.block + (lbn - r.lbn))
    }

    /// Data + indirect blocks.
    pub fn owned(&self) -> u64 {
        self.data.iter().map(|r| r.len).sum::<u64>() + self.indirect.len() as u64
    }
}

/// One directory entry, with what its inode says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    pub ino: u64,
    pub kind: Kind,
    pub size: u64,
    pub nlink: u16,
    /// The directory holding the entry, and the entry's byte offset and `d_reclen` in it.
    pub parent: u64,
    pub offset: u64,
    pub reclen: u16,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Dir
    }
}

fn bad_inode(ino: u64, why: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("the image is damaged (UFS2 inode {ino}: {why})"),
    )
}

fn too_big_io(bytes: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::OutOfMemory,
        format!(
            "its metadata is too big to edit in place ({} MiB); rebuild it instead",
            bytes.div_ceil(1 << 20)
        ),
    )
}

fn other_tool_io(ino: u64, why: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("written by another tool (UFS2 inode {ino}: {why}): convert it first"),
    )
}

impl Volume {
    /// Reads `buf` at image byte `offset` as the plan has changed it: the planned inodes and
    /// directory chunks laid over the image's bytes.
    fn read_planned(&self, image: &mut File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        read_at(image, offset, buf)?;
        let end = offset + buf.len() as u64;
        // Records are at most one chunk long: one starting a chunk before `offset` may reach in.
        let from = offset.saturating_sub(DIRBLKSIZ as u64);
        for (&at, bytes) in self.changes.meta.range(from..end) {
            let (lo, hi) = (at.max(offset), (at + bytes.len() as u64).min(end));
            if lo < hi {
                let dst = buf.get_mut((lo - offset) as usize..(hi - offset) as usize);
                let src = bytes.get((lo - at) as usize..(hi - at) as usize);
                if let (Some(d), Some(s)) = (dst, src) {
                    d.copy_from_slice(s);
                }
            }
        }
        Ok(())
    }

    /// Charges `bytes` more to the change set, refused past the budget.
    fn charge(&mut self, bytes: u64) -> io::Result<()> {
        if bytes > self.budget_left() {
            return Err(too_big_io(self.held() + self.changes.bytes + bytes));
        }
        self.changes.bytes += bytes;
        Ok(())
    }

    /// Plans `bytes` (an inode or a directory chunk) at image byte `at`, charged on first use.
    fn plan_meta(&mut self, at: u64, bytes: Vec<u8>) -> io::Result<()> {
        if !self.changes.meta.contains_key(&at) {
            self.charge(bytes.len() as u64 + RECORD)?;
        }
        self.changes.meta.insert(at, bytes);
        Ok(())
    }

    /// Byte offset of inode `ino` (in range: see [`Self::inode`]).
    pub fn inode_offset(&self, ino: u64) -> u64 {
        let g = &self.geom;
        (ino / g.ipg * g.fpg + IBLKNO) * BSIZE + ino % g.ipg * DINODE
    }

    /// Whether the inode map marks `ino` in use (false out of range).
    pub fn inode_used(&self, ino: u64) -> bool {
        let g = &self.geom;
        usize::try_from(ino / g.ipg)
            .ok()
            .and_then(|c| self.groups.get(c))
            .is_some_and(|grp| bit(&grp.raw, grp.iusedoff, ino % g.ipg))
    }

    /// Reads inode `ino`: it must be in range and allocated, a file or a folder without
    /// extended attributes, its size within what the format allows.
    pub fn inode(&self, image: &mut File, ino: u64) -> io::Result<Inode> {
        let g = &self.geom;
        if ino < ROOTINO || ino >= g.ipg.saturating_mul(g.ncg) {
            return Err(bad_inode(ino, "out of range"));
        }
        if !self.inode_used(ino) {
            return Err(bad_inode(ino, "referenced but free in the inode map"));
        }
        let mut b = [0u8; DINODE as usize];
        self.read_planned(image, self.inode_offset(ino), &mut b)?;
        parse_inode(ino, &b, le64(&self.sb, 1328).unwrap_or(0))
    }

    /// Why physical block `p` can't belong to an inode: past `fs_size`, group metadata, or
    /// free in its group's block map.
    fn block_problem(&self, p: u64) -> Option<&'static str> {
        let g = &self.geom;
        if p >= g.size {
            return Some("past fs_size");
        }
        let (c, rel) = (p / g.fpg, p % g.fpg);
        let (lo, hi) = match c {
            0 => (0, g.dblkno + g.csblocks),
            _ => (SBLKNO, g.dblkno),
        };
        if (lo..hi).contains(&rel) {
            return Some("group metadata");
        }
        let grp = usize::try_from(c).ok().and_then(|c| self.groups.get(c))?;
        bit(&grp.raw, grp.freeoff, rel).then_some("free in the block map")
    }

    /// Every block `ino` owns: pointers within `fs_size` on allocated non-metadata blocks,
    /// none past the size, none twice, `di_blocks` equal to the count, a directory without
    /// holes. The map is charged to the budget.
    pub fn block_map(&self, image: &mut File, ino: &Inode) -> io::Result<BlockMap> {
        let mut m = Mapper {
            vol: self,
            ino: ino.ino,
            n: ino.size.div_ceil(BSIZE),
            map: BlockMap::default(),
            left: self.budget_left(),
        };
        for (lbn, &p) in ino.db.iter().enumerate() {
            m.data(lbn as u64, p)?;
        }
        let mut first = NDADDR;
        for (i, &p) in ino.ib.iter().enumerate() {
            let level = i as u32 + 1;
            m.indirect(image, level, p, first)?;
            first += span(level);
        }
        // No block twice: the runs and indirect blocks sorted by block, none overlapping.
        let count = m.map.data.len() + m.map.indirect.len();
        m.charge((count * size_of::<(u64, u64)>()) as u64)?;
        let mut spans: Vec<(u64, u64)> = Vec::with_capacity(count);
        spans.extend(m.map.data.iter().map(|r| (r.block, r.len)));
        spans.extend(m.map.indirect.iter().map(|&b| (b, 1)));
        spans.sort_unstable();
        if let Some(w) = spans.windows(2).find(|w| w[0].0 + w[0].1 > w[1].0) {
            return Err(bad_inode(ino.ino, format!("block {} mapped twice", w[1].0)));
        }
        let map = m.map;
        let owned = map.owned();
        if owned.checked_mul(SECTORS_PER_BLOCK) != Some(ino.blocks) {
            return Err(bad_inode(
                ino.ino,
                format!("di_blocks {}, {owned} blocks mapped", ino.blocks),
            ));
        }
        let data = owned - map.indirect.len() as u64;
        if ino.kind == Kind::Dir && data != ino.size.div_ceil(BSIZE) {
            return Err(bad_inode(ino.ino, "directory with a hole"));
        }
        Ok(map)
    }

    /// Every entry of directory `dir` (whose parent is `parent`) but `.` and `..`, in disk
    /// order, each checked; a directory without both `.` and `..` is damaged. One block is
    /// read at a time. `f` gets each entry and the bytes the scan itself holds (the block
    /// map's capacity and the block), so it can charge its own beside them.
    fn scan_dir(
        &self,
        image: &mut File,
        dir: &Inode,
        parent: u64,
        f: &mut dyn FnMut(Dirent<'_>, u64) -> io::Result<()>,
    ) -> io::Result<()> {
        if dir.kind != Kind::Dir {
            return Err(bad_inode(dir.ino, "not a directory"));
        }
        let map = self.block_map(image, dir)?;
        let held =
            (map.data.capacity() * size_of::<Run>() + map.indirect.capacity() * 8) as u64 + BSIZE;
        if held > self.budget_left() {
            return Err(too_big_io(self.held() + held));
        }
        let mut block = vec![0u8; BSIZE as usize];
        let mut seen = 0u64;
        for r in &map.data {
            for k in 0..r.len {
                let base = (r.lbn + k) * BSIZE;
                let len = (dir.size - base).min(BSIZE) as usize;
                let buf = &mut block[..len];
                self.read_planned(image, (r.block + k) * BSIZE, buf)?;
                for (i, chunk) in buf.chunks(DIRBLKSIZ).enumerate() {
                    let at = base + (i * DIRBLKSIZ) as u64;
                    scan_chunk(dir.ino, parent, chunk, at, &mut seen, &mut |e| f(e, held))?;
                }
            }
        }
        if seen < 2 {
            return Err(bad_inode(dir.ino, "'.' or '..' missing"));
        }
        Ok(())
    }

    /// The node an entry names: its inode read, its `d_type` matching, its name UTF-8.
    fn node(&self, image: &mut File, dir: u64, e: &Dirent<'_>) -> io::Result<Node> {
        let i = self.inode(image, e.ino)?;
        let want = match i.kind {
            Kind::Dir => DT_DIR,
            Kind::File => DT_REG,
        };
        let at = e.offset;
        if e.dtype != want {
            return Err(bad_inode(
                dir,
                format!("directory entry at byte {at}: d_type {}", e.dtype),
            ));
        }
        let name = std::str::from_utf8(e.name)
            .map_err(|_| bad_inode(dir, format!("directory entry at byte {at}: name not UTF-8")))?;
        Ok(Node {
            name: name.to_string(),
            ino: e.ino,
            kind: i.kind,
            size: i.size,
            nlink: i.nlink,
            parent: dir,
            offset: e.offset,
            reclen: e.reclen,
        })
    }

    /// The root directory's inode (`UFS_ROOTINO`).
    pub fn root(&self, image: &mut File) -> io::Result<Inode> {
        let root = self.inode(image, ROOTINO)?;
        match root.kind {
            Kind::Dir => Ok(root),
            Kind::File => Err(bad_inode(ROOTINO, "the root is not a directory")),
        }
    }

    /// The directory at game path `dir` (`""` is the root) and its parent's inode number.
    fn dir_at(&self, image: &mut File, dir: &str) -> io::Result<(Inode, u64)> {
        if dir.is_empty() {
            return Ok((self.root(image)?, ROOTINO));
        }
        match self.lookup(image, dir)? {
            Some(n) if n.is_dir() => Ok((self.inode(image, n.ino)?, n.parent)),
            Some(_) => Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("{dir}: not a directory"),
            )),
            None => Err(io::Error::new(io::ErrorKind::NotFound, dir.to_string())),
        }
    }

    /// The entries of the directory at game path `dir` (`""` is the root), `.` and `..`
    /// left out, in disk order.
    pub fn list(&self, image: &mut File, dir: &str) -> io::Result<Vec<Node>> {
        let (inode, parent) = self.dir_at(image, dir)?;
        self.list_dir(image, &inode, parent)
    }

    /// The checked entries of directory `inode` (whose parent is `parent`): every record
    /// valid, `.` and `..` present, no name twice. Charged to the budget beside the scan's
    /// own block map and block: per entry slot (by capacity) its raw record, its `Node` and
    /// a sort slot, plus each name twice.
    fn list_dir(&self, image: &mut File, inode: &Inode, parent: u64) -> io::Result<Vec<Node>> {
        type Raw = (u64, u8, Vec<u8>, u64, u16);
        let per = (size_of::<Raw>() + size_of::<Node>() + size_of::<&str>()) as u64;
        #[cfg(test)]
        self.changes.listed.set(self.changes.listed.get() + 1);
        let mut raw: Vec<Raw> = Vec::new();
        let mut names = 0u64;
        let left = self.budget_left();
        self.scan_dir(image, inode, parent, &mut |e, scan| {
            let (len, cap) = (raw.len(), raw.capacity());
            let cap = cap + room(len, cap);
            names += 2 * e.name.len() as u64;
            let need = scan + cap as u64 * per + names;
            if need > left {
                return Err(too_big_io(self.held() + need));
            }
            raw.reserve_exact(cap - len);
            raw.push((e.ino, e.dtype, e.name.to_vec(), e.offset, e.reclen));
            Ok(())
        })?;
        let mut out = Vec::with_capacity(raw.len());
        for (ino, dtype, name, offset, reclen) in &raw {
            let e = Dirent {
                ino: *ino,
                dtype: *dtype,
                name,
                offset: *offset,
                reclen: *reclen,
            };
            out.push(self.node(image, inode.ino, &e)?);
        }
        let mut sorted: Vec<&str> = Vec::with_capacity(out.len());
        sorted.extend(out.iter().map(|n| n.name.as_str()));
        sorted.sort_unstable();
        if let Some(w) = sorted.windows(2).find(|w| w[0] == w[1]) {
            return Err(bad_inode(inode.ino, format!("{:?} listed twice", w[0])));
        }
        Ok(out)
    }

    /// The node at game path `path` (`/`-separated, compared byte for byte: UFS2 names are
    /// case-sensitive), or `None` when it does not exist (or a parent is a file). Each folder
    /// on the way is read whole through [`Self::list_dir`], so a damaged or ambiguous folder
    /// is an error, never a match.
    pub fn lookup(&self, image: &mut File, path: &str) -> io::Result<Option<Node>> {
        if path.split('/').any(str::is_empty) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("bad path {path:?}"),
            ));
        }
        let (mut dir, mut parent) = (self.root(image)?, ROOTINO);
        let mut parts = path.split('/').peekable();
        while let Some(part) = parts.next() {
            let found = self.list_dir(image, &dir, parent)?;
            let Some(n) = found.into_iter().find(|n| n.name == part) else {
                return Ok(None);
            };
            if parts.peek().is_none() {
                return Ok(Some(n));
            }
            if !n.is_dir() {
                return Ok(None);
            }
            parent = dir.ino;
            dir = self.inode(image, n.ino)?;
        }
        Ok(None)
    }
}

/// Byte offset of `fs_cstotal` (`cs_ndir`, `cs_nbfree`, `cs_nifree`, `cs_nffree`: i64 each)
/// and of `fs_clean` in a superblock.
const CSTOTAL_AT: u64 = 1008;
const CLEAN_AT: u64 = 209;
/// The most links an inode may have: FreeBSD 11's `LINK_MAX` (the console's kernel; later
/// releases allow 65500).
const LINK_MAX: u16 = 32767;

impl Volume {
    /// The group of block or inode index `n` with `per` per group, and `n`'s index in it.
    fn group_mut(&mut self, n: u64, per: u64) -> io::Result<(u64, u64, &mut Group)> {
        let (c, rel) = (n / per, n % per);
        let grp = usize::try_from(c)
            .ok()
            .and_then(|i| self.groups.get_mut(i))
            .ok_or_else(|| io::Error::other(format!("internal error: group {c} out of range")))?;
        Ok((c, rel, grp))
    }

    /// Marks block `p` of inode `ino` free: its group's free and cluster maps and every count. A block
    /// already free is damage (two inodes claimed it).
    fn free_block(&mut self, ino: u64, p: u64) -> io::Result<()> {
        let fpg = self.geom.fpg;
        self.touch_group(p / fpg)?;
        if !self.changes.pristine.contains_key(&(p / fpg)) {
            let len = fpg.div_ceil(8);
            self.charge(len + RECORD)?;
            let (c, _, grp) = self.group_mut(p, fpg)?;
            let map = grp
                .raw
                .get(grp.freeoff..grp.freeoff + len as usize)
                .ok_or_else(|| io::Error::other("internal error: free map out of range"))?
                .to_vec();
            self.changes.pristine.insert(c, map);
        }
        let (c, rel, grp) = self.group_mut(p, fpg)?;
        let byte = |base: usize| base + (rel / 8) as usize;
        let mask = 1u8 << (rel % 8);
        let (f, k) = (byte(grp.freeoff), byte(grp.clusteroff));
        match grp.raw.get(f) {
            Some(b) if b & mask == 0 => {}
            _ => {
                return Err(bad_inode(
                    ino,
                    format!("block {p} is free already (owned twice)"),
                ));
            }
        }
        for at in [f, k] {
            if let Some(b) = grp.raw.get_mut(at) {
                *b |= mask;
            }
        }
        grp.cs.nbfree += 1;
        self.summary_mut(c)?.nbfree += 1;
        self.total.nbfree += 1;
        self.changes.freed += 1;
        // A block this plan took after the copy and frees now is not takeable either: it
        // still holds a free-space write of this plan.
        if let Some(m) = self.changes.pristine.get_mut(&c)
            && let Some(b) = m.get_mut((rel / 8) as usize)
        {
            *b &= !mask;
        }
        Ok(())
    }

    /// Marks inode `ino` free in its group's map, with every count (`dir`: a folder's).
    fn free_inode_bit(&mut self, ino: u64, dir: bool) -> io::Result<()> {
        let ipg = self.geom.ipg;
        self.touch_group(ino / ipg)?;
        let (c, rel, grp) = self.group_mut(ino, ipg)?;
        let at = grp.iusedoff + (rel / 8) as usize;
        let mask = 1u8 << (rel % 8);
        match grp.raw.get_mut(at) {
            Some(b) if *b & mask != 0 => *b &= !mask,
            _ => return Err(bad_inode(ino, "freed twice")),
        }
        grp.cs.nifree += 1;
        grp.cs.ndir -= u64::from(dir).min(grp.cs.ndir);
        let s = self.summary_mut(c)?;
        s.nifree += 1;
        s.ndir -= u64::from(dir).min(s.ndir);
        self.total.nifree += 1;
        self.total.ndir -= u64::from(dir).min(self.total.ndir);
        Ok(())
    }

    /// Remembers that group `c`'s block changed (charged on first use).
    fn touch_group(&mut self, c: u64) -> io::Result<()> {
        if !self.changes.groups.contains(&c) {
            self.charge(RECORD)?;
            self.changes.groups.insert(c);
        }
        Ok(())
    }

    /// Whether block `rel` of group `c` may be taken: free now and never freed by this plan
    /// (the group's copy: its map before the first free, each later free cleared in it).
    fn takeable(&self, c: u64, rel: u64) -> bool {
        let Some(grp) = usize::try_from(c).ok().and_then(|i| self.groups.get(i)) else {
            return false;
        };
        bit(&grp.raw, grp.freeoff, rel)
            && self.changes.pristine.get(&c).is_none_or(|m| bit(m, 0, rel))
    }

    /// Free runs to take `n` blocks from, in slot order (`lbn` = slot): the first run of `n`
    /// takeable blocks when there is one, else the takeable runs in block order until they
    /// hold `n` (fewer when the image hasn't that many). The runs are charged before they
    /// grow and stay charged: the caller releases [`Self::map_bytes`].
    // ponytail: a linear scan of every group's map, twice at worst, per new file.
    fn free_runs(&mut self, n: u64) -> io::Result<BlockMap> {
        let g = self.geom;
        let mut map = BlockMap::default();
        for whole in [true, false] {
            let mut got = 0;
            for c in 0..g.ncg {
                let mut rel = 0;
                let end = g.cg_blocks(c).min(g.fpg);
                while rel < end && got < n {
                    if !self.takeable(c, rel) {
                        rel += 1;
                        continue;
                    }
                    let start = rel;
                    while rel < end && rel - start < n - got && self.takeable(c, rel) {
                        rel += 1;
                    }
                    let len = rel - start;
                    if !whole || len == n {
                        let more = room(map.data.len(), map.data.capacity());
                        self.charge((more * size_of::<Run>()) as u64)?;
                        map.data.reserve_exact(more);
                        map.data.push(Run {
                            lbn: got,
                            block: c * g.fpg + start,
                            len,
                        });
                        got += len;
                    }
                }
                if got == n {
                    return Ok(map);
                }
            }
            map.data.clear();
        }
        Ok(map)
    }

    /// Budget bytes a [`Self::free_runs`] map holds.
    fn map_bytes(map: &BlockMap) -> u64 {
        (map.data.capacity() * size_of::<Run>()) as u64
    }

    /// Marks block `p` taken: its group's free and cluster maps and every count.
    fn take_block(&mut self, p: u64) -> io::Result<()> {
        let fpg = self.geom.fpg;
        self.touch_group(p / fpg)?;
        let (c, rel, grp) = self.group_mut(p, fpg)?;
        let mask = 1u8 << (rel % 8);
        for base in [grp.freeoff, grp.clusteroff] {
            match grp.raw.get_mut(base + (rel / 8) as usize) {
                Some(b) if *b & mask != 0 => *b &= !mask,
                _ => return Err(io::Error::other(format!("internal error: block {p} taken"))),
            }
        }
        grp.cs.nbfree = less(grp.cs.nbfree)?;
        let s = self.summary_mut(c)?;
        s.nbfree = less(s.nbfree)?;
        self.total.nbfree = less(self.total.nbfree)?;
        Ok(())
    }

    /// Takes `n` blocks ([`Self::free_runs`]) as runs in slot order (`lbn` = slot), still
    /// charged ([`Self::map_bytes`]). Without room for them: counted in `short`, `None`.
    fn take_blocks(&mut self, n: u64) -> io::Result<Option<BlockMap>> {
        if n > self.total.nbfree.saturating_sub(self.changes.freed) {
            self.changes.short += n;
            return Ok(None);
        }
        let map = self.free_runs(n)?;
        let mut slot = 0;
        for r in &map.data {
            for k in 0..r.len {
                self.take_block(r.block + k)?;
            }
            slot += r.len;
        }
        if slot != n {
            return Err(io::Error::other(
                "internal error: free blocks short of the free count",
            ));
        }
        self.changes.taken += n;
        Ok(Some(map))
    }

    /// Takes a free inode (the first, never one this plan freed), its group's
    /// `cg_initediblk` raised over it when past (the inodes it then covers must be zero, as
    /// the writer leaves them: the kernel writes none past it). None free:
    /// counted in `short_inodes`, `None`.
    fn take_inode(&mut self, image: &mut File, dir: bool) -> io::Result<Option<u64>> {
        let g = self.geom;
        let mut found = None;
        for (c, grp) in (0..g.ncg).zip(&self.groups) {
            if grp.cs.nifree == 0 {
                continue;
            }
            let free = (0..g.ipg).find(|&rel| {
                let ino = c * g.ipg + rel;
                // By ruling, inodes this plan freed are not reused, the same as blocks.
                ino >= ROOTINO
                    && !bit(&grp.raw, grp.iusedoff, rel)
                    && !self.changes.meta.contains_key(&self.inode_offset(ino))
            });
            if let Some(rel) = free {
                found = Some((c, rel));
                break;
            }
        }
        let Some((c, rel)) = found else {
            self.changes.short_inodes += 1;
            return Ok(None);
        };
        let ino = c * g.ipg + rel;
        self.touch_group(c)?;
        let (_, _, grp) = self.group_mut(ino, g.ipg)?;
        let inited = grp.initediblk;
        if rel >= inited {
            let new = (rel + 1).next_multiple_of(INOPB).min(g.ipg);
            grp.initediblk = new;
            put(&mut grp.raw, 120, &(new as u32).to_le_bytes())?;
            let mut b = vec![0u8; ((new - inited) * DINODE) as usize];
            self.charge(b.len() as u64)?;
            self.read_planned(image, self.inode_offset(c * g.ipg + inited), &mut b)?;
            self.changes.bytes = self.changes.bytes.saturating_sub(b.len() as u64);
            if b.iter().any(|&x| x != 0) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("cylinder group {c}: inodes past cg_initediblk are not zero"),
                ));
            }
        }
        let (_, _, grp) = self.group_mut(ino, g.ipg)?;
        let at = grp.iusedoff + (rel / 8) as usize;
        match grp.raw.get_mut(at) {
            Some(b) => *b |= 1 << (rel % 8),
            None => return Err(io::Error::other("internal error: inode map out of range")),
        }
        grp.cs.nifree = less(grp.cs.nifree)?;
        grp.cs.ndir += u64::from(dir);
        let s = self.summary_mut(c)?;
        s.nifree = less(s.nifree)?;
        s.ndir += u64::from(dir);
        self.total.nifree = less(self.total.nifree)?;
        self.total.ndir += u64::from(dir);
        Ok(Some(ino))
    }

    /// New blocks for `len` bytes of `data`, laid out as the writer lays out a file (its slot
    /// sequence: the direct blocks, then each indirect block right before the blocks it
    /// maps) over the blocks taken, one run when a run that long is free. The data (the last
    /// block's tail zeroed) and the indirect blocks are planned into free space. No room:
    /// counted in `short`, `None`.
    fn put_data(&mut self, len: u64, data: &Data) -> io::Result<Option<Ptrs>> {
        let n = len.div_ceil(BSIZE);
        let total = total_slots(n).ok_or_else(|| io::Error::other("internal error: too big"))?;
        let Some(map) = self.take_blocks(total)? else {
            return Ok(None);
        };
        let at = |slot: u64| {
            map.physical(slot)
                .ok_or_else(|| io::Error::other("internal error: slot out of range"))
        };
        let segs = segments(n);
        // The segments and one indirect block's scratch; the map is charged already
        // (`take_blocks`), both released at the end.
        let held = (segs.capacity() * size_of::<Seg>()) as u64 + BSIZE;
        self.charge(held)?;
        for seg in segs {
            match seg {
                Seg::Data { slot, lbn, count } => {
                    let mut k = 0;
                    while k < count {
                        let p = at(slot + k)?;
                        // The rest of this run of the map, within the segment.
                        let i = map.data.partition_point(|r| r.lbn + r.len <= slot + k);
                        let run = map.data.get(i).map_or(1, |r| r.lbn + r.len - (slot + k));
                        let blocks = run.min(count - k);
                        let off = (lbn + k) * BSIZE;
                        let part = (blocks * BSIZE).min(len - off);
                        self.push_data(p * BSIZE, slice(data, off, part)?)?;
                        if part < blocks * BSIZE {
                            self.push_data(p * BSIZE + part, Data::Zero(blocks * BSIZE - part))?;
                        }
                        k += blocks;
                    }
                }
                Seg::Indirect { slot, level, k } => {
                    let mut block = vec![0u8; BSIZE as usize];
                    if level == 1 {
                        for (j, kid) in (slot + 1..slot + 1 + k).enumerate() {
                            put(&mut block, 8 * j, &at(kid)?.to_le_bytes())?;
                        }
                    } else {
                        for (j, kid) in children(slot, level, k).enumerate() {
                            put(&mut block, 8 * j, &at(kid)?.to_le_bytes())?;
                        }
                    }
                    self.charge(BSIZE + RECORD)?;
                    self.changes.fresh.insert(at(slot)?, block);
                }
            }
        }
        let mut ptrs = Ptrs {
            blocks: total * SECTORS_PER_BLOCK,
            ..Ptrs::default()
        };
        for (i, d) in ptrs.db.iter_mut().enumerate().take(n.min(NDADDR) as usize) {
            *d = at(i as u64)?;
        }
        for (r, root) in ptrs.ib.iter_mut().zip(indirect_roots(n)) {
            if let Some(slot) = root {
                *r = at(slot)?;
            }
        }
        let done = held + Self::map_bytes(&map);
        self.changes.bytes = self.changes.bytes.saturating_sub(done);
        Ok(Some(ptrs))
    }

    /// Plans `data` at image byte `at`, in free space.
    fn push_data(&mut self, at: u64, data: Data) -> io::Result<()> {
        let bytes = match &data {
            Data::Bytes(b) => b.len() as u64,
            _ => 0,
        };
        let more = room(self.changes.data.len(), self.changes.data.capacity());
        self.charge(bytes + (more * size_of::<Write>()) as u64)?;
        self.changes.data.reserve_exact(more);
        self.changes.data.push(Write {
            offset: at,
            data,
            live: false,
        });
        Ok(())
    }

    /// Budget bytes a cached listing holds.
    fn listing_bytes(list: &[Node], cap: usize) -> u64 {
        let names: usize = list.iter().map(|n| n.name.capacity()).sum();
        (cap * size_of::<Node>() + names) as u64 + RECORD
    }

    /// Lists directory `dir` (whose parent is `parent`) into the cache, once per plan.
    fn cache(&mut self, image: &mut File, dir: u64, parent: u64) -> io::Result<()> {
        if self.changes.listings.contains_key(&dir) {
            return Ok(());
        }
        let inode = self.inode(image, dir)?;
        let list = self.list_dir(image, &inode, parent)?;
        self.charge(Self::listing_bytes(&list, list.capacity()))?;
        self.changes.listings.insert(dir, list);
        Ok(())
    }

    /// The node at game path `path` as the plan has left it, resolved against the cached
    /// listings (each folder on the way listed once per plan); `None` when it does not exist
    /// or a parent is a file. `path` has no empty component.
    fn resolve(&mut self, image: &mut File, path: &str) -> io::Result<Option<Node>> {
        let (mut dir, mut parent) = (ROOTINO, ROOTINO);
        let mut parts = path.split('/').peekable();
        while let Some(part) = parts.next() {
            self.cache(image, dir, parent)?;
            let found = self
                .changes
                .listings
                .get(&dir)
                .and_then(|l| l.iter().find(|n| n.name == part));
            let Some(n) = found else {
                return Ok(None);
            };
            if parts.peek().is_none() {
                return Ok(Some(n.clone()));
            }
            if !n.is_dir() {
                return Ok(None);
            }
            (parent, dir) = (dir, n.ino);
        }
        Ok(None)
    }

    /// Image byte offset of directory `dir`'s chunk holding byte `offset`, and that chunk's
    /// offset in the directory.
    fn chunk_at(&self, image: &mut File, dir: u64, offset: u64) -> io::Result<(u64, u64)> {
        let inode = self.inode(image, dir)?;
        let map = self.block_map(image, &inode)?;
        let chunk = offset & !(DIRBLKSIZ as u64 - 1);
        let p = map
            .physical(chunk / BSIZE)
            .ok_or_else(|| bad_inode(dir, format!("no block at byte {chunk}")))?;
        Ok((p * BSIZE + chunk % BSIZE, chunk))
    }

    /// Removes `node`'s entry from its directory as the kernel does: merged into the entry
    /// before it in its chunk (`d_reclen`), or, first in its chunk, left as an unused entry
    /// (`d_ino` 0). The cached listing follows.
    fn remove_entry(&mut self, image: &mut File, node: &Node) -> io::Result<()> {
        let (at, chunk) = self.chunk_at(image, node.parent, node.offset)?;
        let mut b = vec![0u8; DIRBLKSIZ];
        self.read_planned(image, at, &mut b)?;
        let rel = (node.offset - chunk) as usize;
        let bad =
            |why: &str| bad_inode(node.parent, format!("entry at byte {}: {why}", node.offset));
        if le32(&b, rel) != Some(node.ino as u32) {
            return Err(bad("changed under the plan"));
        }
        let mut prev = None;
        if rel > 0 {
            let mut off = 0usize;
            loop {
                let r = b
                    .get(off + 4..off + 6)
                    .map_or(0, |s| u16::from_le_bytes([s[0], s[1]]));
                let next = off + usize::from(r);
                if next == rel {
                    break;
                }
                if r == 0 || next > rel {
                    return Err(bad("no entry before it"));
                }
                off = next;
            }
            let merged = (rel - off) as u16 + node.reclen;
            put(&mut b, off + 4, &merged.to_le_bytes())?;
            prev = Some((chunk + off as u64, merged));
        } else {
            b[0..4].fill(0);
        }
        self.plan_meta(at, b)?;
        self.room_at(node.parent, chunk);
        if let Some(list) = self.changes.listings.get_mut(&node.parent) {
            list.retain(|n| n.offset != node.offset);
            if let Some((off, merged)) = prev
                && let Some(n) = list.iter_mut().find(|n| n.offset == off)
            {
                n.reclen = merged;
            }
        }
        Ok(())
    }

    /// The first entry of directory `dir` with `need` bytes of room past its own record (an
    /// unused first entry of a chunk: its whole `d_reclen`), as the plan has left it. One
    /// block is read at a time, charged beside the directory's block map. The scan starts at
    /// the folder's [`Hint`] when no chunk before it has the room, and leaves a new one.
    fn find_slot(&mut self, image: &mut File, dir: u64, need: usize) -> io::Result<Option<Slot>> {
        let inode = self.inode(image, dir)?;
        let map = self.block_map(image, &inode)?;
        let held =
            (map.data.capacity() * size_of::<Run>() + map.indirect.capacity() * 8) as u64 + BSIZE;
        if held > self.budget_left() {
            return Err(too_big_io(self.held() + self.changes.bytes + held));
        }
        let mut hint = match self.changes.hints.get(&dir) {
            Some(&h) if h.below < need => h,
            _ => Hint::default(),
        };
        #[cfg(test)]
        if self.changes.no_hints {
            hint = Hint::default();
        }
        let start = hint.chunk;
        let mut block = vec![0u8; BSIZE as usize];
        for r in &map.data {
            for k in 0..r.len {
                let base = (r.lbn + k) * BSIZE;
                let len = (inode.size - base).min(BSIZE) as usize;
                if base + len as u64 <= start {
                    continue;
                }
                let buf = &mut block[..len];
                self.read_planned(image, (r.block + k) * BSIZE, buf)?;
                for (i, chunk) in buf.chunks(DIRBLKSIZ).enumerate() {
                    let at = base + (i * DIRBLKSIZ) as u64;
                    if at < start {
                        continue;
                    }
                    let mut off = 0usize;
                    let mut most = 0usize;
                    while off < chunk.len() {
                        let ino = le32(chunk, off).unwrap_or(0);
                        let reclen = chunk
                            .get(off + 4..off + 6)
                            .map_or(0, |s| usize::from(u16::from_le_bytes([s[0], s[1]])));
                        let used = match chunk.get(off + 7) {
                            _ if ino == 0 => 0,
                            Some(&n) => dirsiz(usize::from(n)),
                            None => usize::MAX,
                        };
                        if reclen == 0 || used > reclen || off + reclen > chunk.len() {
                            let at = at + off as u64;
                            return Err(bad_inode(dir, format!("directory entry at byte {at}")));
                        }
                        if reclen - used >= need {
                            hint.chunk = at;
                            self.changes.hints.insert(dir, hint);
                            return Ok(Some(Slot {
                                at: (r.block + k) * BSIZE + (i * DIRBLKSIZ) as u64,
                                chunk: at,
                                off,
                                used,
                            }));
                        }
                        most = most.max(reclen - used);
                        off += reclen;
                    }
                    hint.below = hint.below.max(most);
                }
            }
        }
        // A chunk the folder grows by starts here.
        hint.chunk = hint.chunk.max(inode.size);
        self.changes.hints.insert(dir, hint);
        Ok(None)
    }

    /// Directory `dir`'s chunk at `chunk` may have more room now: [`Self::find_slot`] looks
    /// there again.
    fn room_at(&mut self, dir: u64, chunk: u64) {
        if let Some(h) = self.changes.hints.get_mut(&dir) {
            h.chunk = h.chunk.min(chunk);
        }
    }

    /// The room `node`'s removal leaves in the entry before it in its chunk (that entry's
    /// slack plus `node`'s `d_reclen`, as [`Self::remove_entry`] merges them); 0 when `node`
    /// is first in its chunk (it then stays an unused entry of its own `d_reclen`).
    fn room_freed(&self, image: &mut File, node: &Node) -> io::Result<usize> {
        let (at, chunk) = self.chunk_at(image, node.parent, node.offset)?;
        let mut b = vec![0u8; DIRBLKSIZ];
        self.read_planned(image, at, &mut b)?;
        let rel = (node.offset - chunk) as usize;
        let mut off = 0usize;
        while off < rel {
            let reclen = b
                .get(off + 4..off + 6)
                .map_or(0, |s| usize::from(u16::from_le_bytes([s[0], s[1]])));
            if off + reclen == rel {
                let used = match b.get(off + 7) {
                    _ if le32(&b, off) == Some(0) => 0,
                    Some(&n) => dirsiz(usize::from(n)),
                    None => rel,
                };
                return Ok(reclen.saturating_sub(used) + usize::from(node.reclen));
            }
            if reclen == 0 || off + reclen > rel {
                let at = node.offset;
                return Err(bad_inode(
                    node.parent,
                    format!("entry at byte {at}: none before it"),
                ));
            }
            off += reclen;
        }
        Ok(0)
    }

    /// Writes an entry for `node`'s inode named `name` into `slot` of directory `dir` (the
    /// entry there shortened to what it uses, or an unused one taken whole) and adds it to
    /// the cached listing, in disk order.
    fn insert_entry(
        &mut self,
        image: &mut File,
        dir: u64,
        slot: Slot,
        node: &Node,
        name: &str,
    ) -> io::Result<()> {
        let mut b = vec![0u8; DIRBLKSIZ];
        self.read_planned(image, slot.at, &mut b)?;
        let reclen = b
            .get(slot.off + 4..slot.off + 6)
            .map_or(0, |s| usize::from(u16::from_le_bytes([s[0], s[1]])));
        if slot.used > 0 {
            put(&mut b, slot.off + 4, &(slot.used as u16).to_le_bytes())?;
        }
        let off = slot.off + slot.used;
        let entry = b
            .get_mut(off..slot.off + reclen)
            .ok_or_else(|| io::Error::other("internal error: slot out of range"))?;
        let len = entry.len() as u16;
        dirent(entry, node.ino, node.kind, name)?;
        self.plan_meta(slot.at, b)?;
        // The listing's next slot and the name, charged before it grows.
        let more = self.changes.listings.get(&dir).map_or(0, |l| {
            room(l.len(), l.capacity()) * size_of::<Node>() + name.len()
        });
        self.charge(more as u64)?;
        if let Some(list) = self.changes.listings.get_mut(&dir) {
            let kept = slot.chunk + slot.off as u64;
            if let Some(n) = list.iter_mut().find(|n| slot.used > 0 && n.offset == kept) {
                n.reclen = slot.used as u16;
            }
            let offset = slot.chunk + off as u64;
            let i = list.partition_point(|n| n.offset < offset);
            list.reserve_exact(room(list.len(), list.capacity()));
            list.insert(
                i,
                Node {
                    name: name.to_string(),
                    parent: dir,
                    offset,
                    reclen: len,
                    ..node.clone()
                },
            );
        }
        Ok(())
    }

    /// Renames `node`'s entry where it is (`name` fits its `d_reclen`); the cached listing
    /// follows.
    fn rename_entry(&mut self, image: &mut File, node: &Node, name: &str) -> io::Result<()> {
        let (at, chunk) = self.chunk_at(image, node.parent, node.offset)?;
        let mut b = vec![0u8; DIRBLKSIZ];
        self.read_planned(image, at, &mut b)?;
        let rel = (node.offset - chunk) as usize;
        if le32(&b, rel) != Some(node.ino as u32) {
            let at = node.offset;
            return Err(bad_inode(
                node.parent,
                format!("entry at byte {at}: changed"),
            ));
        }
        let entry = b
            .get_mut(rel..rel + usize::from(node.reclen))
            .ok_or_else(|| io::Error::other("internal error: entry out of range"))?;
        dirent(entry, node.ino, node.kind, name)?;
        self.plan_meta(at, b)?;
        // A shorter name leaves more room after it.
        self.room_at(node.parent, chunk);
        self.charge(name.len() as u64)?;
        if let Some(list) = self.changes.listings.get_mut(&node.parent)
            && let Some(n) = list.iter_mut().find(|n| n.offset == node.offset)
        {
            n.name = name.to_string();
        }
        Ok(())
    }

    /// Points folder `dir`'s `..` (offset 12 of its first chunk) from `old` to `new`.
    fn set_dotdot(&mut self, image: &mut File, dir: u64, old: u64, new: u64) -> io::Result<()> {
        let (at, _) = self.chunk_at(image, dir, 0)?;
        let mut b = vec![0u8; DIRBLKSIZ];
        self.read_planned(image, at, &mut b)?;
        if le32(&b, 12) != Some(old as u32) {
            return Err(bad_inode(dir, "'..' changed under the plan"));
        }
        put(&mut b, 12, &(new as u32).to_le_bytes())?;
        self.plan_meta(at, b)
    }

    /// Lowers directory `dir`'s link count by one (a folder in it went); cached nodes follow.
    fn unlink_dir(&mut self, image: &mut File, dir: u64) -> io::Result<()> {
        self.relink_dir(image, dir, false)
    }

    /// Raises (`up`) or lowers directory `dir`'s link count by one (a folder in it came or
    /// went); cached nodes follow. Past [`LINK_MAX`] or below 2 is damage: [`Self::rename`]
    /// refuses a move that would go past it first.
    fn relink_dir(&mut self, image: &mut File, dir: u64, up: bool) -> io::Result<()> {
        let inode = self.inode(image, dir)?;
        let nlink = match up {
            true => inode.nlink.checked_add(1).filter(|&n| n <= LINK_MAX),
            false => inode.nlink.checked_sub(1),
        }
        .filter(|&n| n >= 2)
        .ok_or_else(|| bad_inode(dir, format!("directory link count {}", inode.nlink)))?;
        let at = self.inode_offset(dir);
        let mut b = vec![0u8; DINODE as usize];
        self.read_planned(image, at, &mut b)?;
        b[2..4].copy_from_slice(&nlink.to_le_bytes());
        self.plan_meta(at, b)?;
        for list in self.changes.listings.values_mut() {
            for n in list.iter_mut().filter(|n| n.ino == dir) {
                n.nlink = nlink;
            }
        }
        Ok(())
    }

    /// Frees inode `inode` and every block it owns (holes have none), the inode zeroed as
    /// fsck expects of a free one. Its block map is charged while it is held.
    fn free_inode(&mut self, image: &mut File, inode: &Inode) -> io::Result<()> {
        if inode.kind == Kind::File && inode.nlink != 1 {
            // Its blocks would stay referenced by the other names.
            return Err(other_tool_io(
                inode.ino,
                format!("a file with {} links", inode.nlink),
            ));
        }
        let map = self.block_map(image, inode)?;
        let held = (map.data.capacity() * size_of::<Run>() + map.indirect.capacity() * 8) as u64;
        self.charge(held)?;
        for r in &map.data {
            for k in 0..r.len {
                self.free_block(inode.ino, r.block + k)?;
            }
        }
        for &p in &map.indirect {
            self.free_block(inode.ino, p)?;
        }
        self.changes.bytes = self.changes.bytes.saturating_sub(held);
        self.changes.hints.remove(&inode.ino);
        self.free_inode_bit(inode.ino, inode.kind == Kind::Dir)?;
        self.plan_meta(self.inode_offset(inode.ino), vec![0; DINODE as usize])
    }

    /// Deletes the file or folder at `path`: its entry removed, a folder's parent link
    /// dropped, then its inode and blocks freed, and for a folder everything under it (folders
    /// waiting to be listed kept on a stack, not recursed, charged to the budget; the entries
    /// inside freed folders are left as they are: their blocks are free).
    /// `Ok(Err(finding))` when the op does not apply.
    fn delete(&mut self, image: &mut File, path: &str) -> io::Result<Result<(), String>> {
        if path.split('/').any(str::is_empty) {
            return Ok(Err(format!("{path:?}: bad path")));
        }
        let Some(node) = self.resolve(image, path)? else {
            return Ok(Err(format!("{path}: not found")));
        };
        self.remove_entry(image, &node)?;
        if node.is_dir() {
            self.unlink_dir(image, node.parent)?;
        }
        const SLOT: u64 = size_of::<(u64, u64)>() as u64;
        let mut stack = vec![(node.ino, node.parent)];
        self.charge(SLOT * stack.capacity() as u64)?;
        let mut held = SLOT * stack.capacity() as u64;
        while let Some((ino, parent)) = stack.pop() {
            let inode = self.inode(image, ino)?;
            if inode.kind == Kind::Dir {
                // A folder this plan listed already: its cached listing, kept current and
                // charged since (what entries it lost stays charged until the plan ends).
                let (list, bytes) = match self.changes.listings.remove(&ino) {
                    Some(list) => {
                        let bytes = Self::listing_bytes(&list, list.capacity());
                        (list, bytes)
                    }
                    None => {
                        let list = self.list_dir(image, &inode, parent)?;
                        let bytes = Self::listing_bytes(&list, list.capacity());
                        self.charge(bytes)?;
                        (list, bytes)
                    }
                };
                for child in &list {
                    if child.is_dir() {
                        let (len, cap) = (stack.len(), stack.capacity());
                        let more = room(len, cap);
                        self.charge(SLOT * more as u64)?;
                        held += SLOT * more as u64;
                        stack.reserve_exact(more);
                        stack.push((child.ino, ino));
                    } else {
                        let c = self.inode(image, child.ino)?;
                        self.free_inode(image, &c)?;
                    }
                }
                self.changes.bytes = self.changes.bytes.saturating_sub(bytes);
            }
            self.free_inode(image, &inode)?;
        }
        self.changes.bytes = self.changes.bytes.saturating_sub(held);
        Ok(Ok(()))
    }

    /// Renames or moves `from` to `to` (compared byte for byte: UFS2 names are case-sensitive):
    /// in its own entry when the name fits its `d_reclen` and the folder stays, else a new
    /// entry in the target folder's first slot with room, then the old one removed (in its own
    /// folder, when only the room the old entry frees fits it: removed first, then placed); a
    /// folder moved between parents gets its `..` repointed and both parents' link counts
    /// follow. `Ok(Err(finding))` when the op does not apply; nothing is planned then.
    // ponytail: no room in the target folder's blocks is a finding (folder growth: 4c); times
    // are left as they are; only `to`'s own length is checked against MAXPATHLEN here (core
    // checks every resulting path, for all formats).
    fn rename(&mut self, image: &mut File, from: &str, to: &str) -> io::Result<Result<(), String>> {
        if let Some(p) = [from, to]
            .into_iter()
            .find(|p| p.split('/').any(str::is_empty))
        {
            return Ok(Err(format!("{p:?}: bad path")));
        }
        let Some(node) = self.resolve(image, from)? else {
            return Ok(Err(format!("{from}: not found")));
        };
        let (parent, name) = to.rsplit_once('/').unwrap_or(("", to));
        if to.len() > MAXPATHLEN {
            return Ok(Err(format!("{to}: path longer than {MAXPATHLEN} bytes")));
        }
        if let Some(why) = bad_component(name) {
            return Ok(Err(format!("{to}: {why}")));
        }
        if from == to {
            return Ok(Ok(()));
        }
        if node.is_dir() && to.strip_prefix(from).is_some_and(|r| r.starts_with('/')) {
            return Ok(Err(format!("{to}: a folder can't be moved into itself")));
        }
        if self.resolve(image, to)?.is_some() {
            return Ok(Err(format!("{to}: already exists")));
        }
        let (dir, up) = if parent.is_empty() {
            (ROOTINO, ROOTINO)
        } else {
            match self.resolve(image, parent)? {
                Some(n) if n.is_dir() => (n.ino, n.parent),
                _ => return Ok(Err(format!("{to}: folder {parent} not found"))),
            }
        };
        self.cache(image, dir, up)?;
        let moved = node.is_dir() && dir != node.parent;
        if moved && self.inode(image, dir)?.nlink >= LINK_MAX {
            return Ok(Err(format!("{to}: its folder holds too many folders")));
        }
        let need = dirsiz(name.len());
        if dir == node.parent && need <= usize::from(node.reclen) {
            self.rename_entry(image, &node, name)?;
            return Ok(Ok(()));
        }
        match self.find_slot(image, dir, need)? {
            Some(slot) => {
                self.insert_entry(image, dir, slot, &node, name)?;
                self.remove_entry(image, &node)?;
            }
            // In its own folder, the old entry merged into the one before it may make room.
            None if dir == node.parent && self.room_freed(image, &node)? >= need => {
                self.remove_entry(image, &node)?;
                let slot = self.find_slot(image, dir, need)?;
                let slot = slot.ok_or_else(|| io::Error::other("internal error: room lost"))?;
                self.insert_entry(image, dir, slot, &node, name)?;
            }
            None => match self.grow_dir(image, dir, need)? {
                Room::Slot(slot) => {
                    self.insert_entry(image, dir, slot, &node, name)?;
                    self.remove_entry(image, &node)?;
                }
                // The plan is refused: the move is left out.
                Room::Short => return Ok(Ok(())),
                Room::Full => return Ok(Err(format!("{to}: no room in its folder"))),
            },
        }
        if moved {
            self.set_dotdot(image, node.ino, node.parent, dir)?;
            self.relink_dir(image, node.parent, false)?;
            self.relink_dir(image, dir, true)?;
        }
        Ok(Ok(()))
    }

    /// Adds the file `path` (`len` bytes of `data`) into its folder (made with its parents when
    /// missing, [`Self::ensure_dir`]; grown when full), or replaces the
    /// file there: new blocks first ([`Self::put_data`]), then the inode (a new one as the
    /// writer makes it, or the old one rewritten: its name, inode number and times kept),
    /// then a replaced file's old blocks freed (old data is never overwritten). Without room
    /// for its blocks or inode the op is counted ([`Changes::short`]) and goes no further.
    /// `Ok(Err(finding))` when the op does not apply; nothing is planned then.
    fn put(
        &mut self,
        image: &mut File,
        path: &str,
        len: u64,
        data: &Data,
    ) -> io::Result<Result<(), String>> {
        if path.split('/').any(str::is_empty) {
            return Ok(Err(format!("{path:?}: bad path")));
        }
        if data.len() != len {
            return Ok(Err(format!(
                "{path}: {len} bytes, but its data holds {}",
                data.len()
            )));
        }
        if path.len() > MAXPATHLEN {
            return Ok(Err(format!("{path}: path longer than {MAXPATHLEN} bytes")));
        }
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        if let Some(why) = bad_component(name) {
            return Ok(Err(format!("{path}: {why}")));
        }
        let maxfilesize = le64(&self.sb, 1328).unwrap_or(0);
        if len > maxfilesize || total_slots(len.div_ceil(BSIZE)).is_none() {
            return Ok(Err(format!(
                "{path}: {len} bytes is more than a UFS2 file holds"
            )));
        }
        let (dir, up) = match self.ensure_dir(image, parent)? {
            Ok(Some(d)) => d,
            Ok(None) => {
                // Its folder was not made for want of room: the file counts all the same.
                self.count_new(image, false, total_slots(len.div_ceil(BSIZE)).unwrap_or(0))?;
                return Ok(Ok(()));
            }
            Err(f) => return Ok(Err(f)),
        };
        self.cache(image, dir, up)?;
        match self.resolve(image, path)? {
            Some(n) if n.is_dir() => Ok(Err(format!("{path} is a folder"))),
            Some(n) => self.replace(image, &n, len, data).map(Ok),
            None => self.add(image, path, dir, name, len, data),
        }
    }

    /// [`Self::put`] over the existing file `node`.
    fn replace(&mut self, image: &mut File, node: &Node, len: u64, data: &Data) -> io::Result<()> {
        let inode = self.inode(image, node.ino)?;
        if inode.nlink != 1 {
            // Its old blocks would stay referenced by the other names.
            return Err(other_tool_io(
                inode.ino,
                format!("a file with {} links", inode.nlink),
            ));
        }
        let old = self.block_map(image, &inode)?;
        let held = (old.data.capacity() * size_of::<Run>() + old.indirect.capacity() * 8) as u64;
        self.charge(held)?;
        let Some(p) = self.put_data(len, data)? else {
            self.changes.bytes = self.changes.bytes.saturating_sub(held);
            return Ok(());
        };
        let at = self.inode_offset(node.ino);
        let mut b = vec![0u8; DINODE as usize];
        self.read_planned(image, at, &mut b)?;
        put(&mut b, 16, &len.to_le_bytes())?;
        put(&mut b, 24, &p.blocks.to_le_bytes())?;
        for (i, d) in p.db.iter().enumerate() {
            put(&mut b, 112 + 8 * i, &d.to_le_bytes())?;
        }
        for (i, d) in p.ib.iter().enumerate() {
            put(&mut b, 208 + 8 * i, &d.to_le_bytes())?;
        }
        self.plan_meta(at, b)?;
        for r in &old.data {
            for k in 0..r.len {
                self.free_block(node.ino, r.block + k)?;
            }
        }
        for &q in &old.indirect {
            self.free_block(node.ino, q)?;
        }
        self.changes.bytes = self.changes.bytes.saturating_sub(held);
        if let Some(list) = self.changes.listings.get_mut(&node.parent)
            && let Some(n) = list.iter_mut().find(|n| n.ino == node.ino)
        {
            n.size = len;
        }
        Ok(())
    }

    /// [`Self::put`] of a new file `name` into folder `dir`: an inode as the writer makes it
    /// (mode 0100777, root:root, its fixed times), then its entry.
    fn add(
        &mut self,
        image: &mut File,
        path: &str,
        dir: u64,
        name: &str,
        len: u64,
        data: &Data,
    ) -> io::Result<Result<(), String>> {
        let slot = match self.slot_or_grow(image, dir, dirsiz(name.len()))? {
            Room::Slot(slot) => slot,
            Room::Short => {
                self.count_new(image, false, total_slots(len.div_ceil(BSIZE)).unwrap_or(0))?;
                return Ok(Ok(()));
            }
            Room::Full => return Ok(Err(format!("{path}: no room in its folder"))),
        };
        let Some(ino) = self.take_inode(image, false)? else {
            // Its blocks count all the same (the plan is refused).
            let n = total_slots(len.div_ceil(BSIZE)).unwrap_or(0);
            if let Some(map) = self.take_blocks(n)? {
                let bytes = Self::map_bytes(&map);
                self.changes.bytes = self.changes.bytes.saturating_sub(bytes);
            }
            return Ok(Ok(()));
        };
        let p = self.put_data(len, data)?;
        let size = if p.is_some() { len } else { 0 };
        let p = p.unwrap_or_default();
        let mut b = vec![0u8; DINODE as usize];
        Dinode {
            mode: IFREG | 0o777,
            nlink: 1,
            size,
            blocks: p.blocks,
            db: p.db,
            ib: p.ib,
            dirdepth: 0,
        }
        .write(&mut b);
        self.plan_meta(self.inode_offset(ino), b)?;
        let node = Node {
            name: String::new(),
            ino,
            kind: Kind::File,
            size,
            nlink: 1,
            parent: dir,
            offset: 0,
            reclen: 0,
        };
        self.insert_entry(image, dir, slot, &node, name)?;
        Ok(Ok(()))
    }

    /// The folder at game path `dir` (`""`: the root) as (inode, its parent), made with its
    /// parents when missing ([`Self::new_dir`]). `Ok(Ok(None))`: a folder on the way was
    /// not made for want of room (by this op or an earlier one); each folder below it is
    /// counted once. `Ok(Err(finding))`: a part of the path is a file, a bad name, or a
    /// folder that can't take another entry.
    fn ensure_dir(
        &mut self,
        image: &mut File,
        dir: &str,
    ) -> io::Result<Result<Option<(u64, u64)>, String>> {
        let (mut at, mut up) = (ROOTINO, ROOTINO);
        if dir.is_empty() {
            return Ok(Ok(Some((at, up))));
        }
        let mut prefix = String::new();
        for (i, part) in dir.split('/').enumerate() {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            match self.resolve(image, &prefix)? {
                Some(n) if n.is_dir() => (at, up) = (n.ino, n.parent),
                Some(_) => return Ok(Err(format!("{prefix} is a file"))),
                None => {
                    if let Some(why) = bad_component(part) {
                        return Ok(Err(format!("{prefix}: {why}")));
                    }
                    if !self.changes.missing.contains(&prefix) {
                        match self.new_dir(image, at, up, part, &prefix)? {
                            Ok(Some(ino)) => {
                                (at, up) = (ino, at);
                                continue;
                            }
                            Ok(None) => self.miss(&prefix)?,
                            Err(f) => return Ok(Err(f)),
                        }
                    }
                    for rest in dir.split('/').skip(i + 1) {
                        prefix.push('/');
                        prefix.push_str(rest);
                        if !self.changes.missing.contains(&prefix) {
                            self.miss(&prefix)?;
                            self.count_new(image, true, 1)?;
                        }
                    }
                    return Ok(Ok(None));
                }
            }
        }
        Ok(Ok(Some((at, up))))
    }

    /// Remembers that folder `path` was not made for want of room (charged).
    fn miss(&mut self, path: &str) -> io::Result<()> {
        self.charge(path.len() as u64 + RECORD)?;
        self.changes.missing.insert(path.to_string());
        Ok(())
    }

    /// Counts what a new file (`dir`: folder) of `blocks` blocks takes in a plan already
    /// short of room: its inode and blocks are taken (or counted missing), nothing written.
    fn count_new(&mut self, image: &mut File, dir: bool, blocks: u64) -> io::Result<()> {
        self.take_inode(image, dir)?;
        self.count_blocks(blocks)
    }

    /// Takes (or counts missing) `n` blocks in a plan already short of room; nothing written.
    fn count_blocks(&mut self, n: u64) -> io::Result<()> {
        if let Some(map) = self.take_blocks(n)? {
            let bytes = Self::map_bytes(&map);
            self.changes.bytes = self.changes.bytes.saturating_sub(bytes);
        }
        Ok(())
    }

    /// Makes folder `name` (game path `path`) in directory `dir` (whose parent is `up`) as
    /// the writer makes one: an inode (mode 040777, nlink 2, root:root, its fixed times,
    /// `di_dirdepth` one past its parent's) and one block whose first chunk holds `.` and
    /// `..`; then its entry, and `dir`'s nlink + 1. `Ok(Ok(None))`: no room (counted).
    fn new_dir(
        &mut self,
        image: &mut File,
        dir: u64,
        up: u64,
        name: &str,
        path: &str,
    ) -> io::Result<Result<Option<u64>, String>> {
        self.cache(image, dir, up)?;
        let parent = self.inode(image, dir)?;
        if parent.nlink >= LINK_MAX {
            return Ok(Err(format!("{path}: its folder holds too many folders")));
        }
        let slot = match self.slot_or_grow(image, dir, dirsiz(name.len()))? {
            Room::Slot(slot) => slot,
            Room::Short => {
                self.count_new(image, true, 1)?;
                return Ok(Ok(None));
            }
            Room::Full => return Ok(Err(format!("{path}: no room in its folder"))),
        };
        let Some(ino) = self.take_inode(image, true)? else {
            // The inode is counted already; its block counts all the same.
            self.count_blocks(1)?;
            return Ok(Ok(None));
        };
        let Some(map) = self.take_blocks(1)? else {
            return Ok(Ok(None));
        };
        let p = map
            .physical(0)
            .ok_or_else(|| io::Error::other("internal error: slot out of range"))?;
        let bytes = Self::map_bytes(&map);
        self.changes.bytes = self.changes.bytes.saturating_sub(bytes);
        let mut chunk = vec![0u8; DIRBLKSIZ];
        let (dot, dotdot) = chunk.split_at_mut(dirsiz(1));
        dirent(dot, ino, Kind::Dir, ".")?;
        dirent(dotdot, dir, Kind::Dir, "..")?;
        self.plan_meta(p * BSIZE, chunk)?;
        let mut pb = vec![0u8; DINODE as usize];
        self.read_planned(image, self.inode_offset(dir), &mut pb)?;
        let depth = le32(&pb, 240).unwrap_or(0).saturating_add(1);
        let mut db = [0u64; NDADDR as usize];
        db[0] = p;
        let mut b = vec![0u8; DINODE as usize];
        Dinode {
            mode: IFDIR | 0o777,
            nlink: 2,
            size: DIRBLKSIZ as u64,
            blocks: SECTORS_PER_BLOCK,
            db,
            ib: [0; 3],
            dirdepth: depth,
        }
        .write(&mut b);
        self.plan_meta(self.inode_offset(ino), b)?;
        let node = Node {
            name: String::new(),
            ino,
            kind: Kind::Dir,
            size: DIRBLKSIZ as u64,
            nlink: 2,
            parent: dir,
            offset: 0,
            reclen: 0,
        };
        self.insert_entry(image, dir, slot, &node, name)?;
        self.relink_dir(image, dir, true)?;
        Ok(Ok(Some(ino)))
    }

    /// [`Op::Mkdir`]: the folder `path` with its parents; an existing folder is a no-op.
    fn mkdir(&mut self, image: &mut File, path: &str) -> io::Result<Result<(), String>> {
        if path.split('/').any(str::is_empty) {
            return Ok(Err(format!("{path:?}: bad path")));
        }
        if path.len() > MAXPATHLEN {
            return Ok(Err(format!("{path}: path longer than {MAXPATHLEN} bytes")));
        }
        Ok(self.ensure_dir(image, path)?.map(|_| ()))
    }

    /// Room for an entry of `need` bytes in directory `dir`: an existing slot, else the
    /// folder grown by one chunk ([`Self::grow_dir`]).
    fn slot_or_grow(&mut self, image: &mut File, dir: u64, need: usize) -> io::Result<Room> {
        match self.find_slot(image, dir, need)? {
            Some(slot) => Ok(Room::Slot(slot)),
            None => self.grow_dir(image, dir, need),
        }
    }

    /// Appends one empty chunk (`d_ino` 0, `d_reclen` 512) to directory `dir`, as the kernel
    /// grows a folder: inside its last block while it has room, else in a new block (a new
    /// single indirect block first at the 13th). `Room::Short` when no block is free
    /// (counted), `Room::Full` past the single indirect block.
    // ponytail: a folder past 12 + 8192 blocks (512 MiB of entries) is not grown.
    fn grow_dir(&mut self, image: &mut File, dir: u64, need: usize) -> io::Result<Room> {
        let inode = self.inode(image, dir)?;
        let size = inode.size;
        let lbn = size / BSIZE;
        let at = self.inode_offset(dir);
        let mut b = vec![0u8; DINODE as usize];
        self.read_planned(image, at, &mut b)?;
        let chunk_at = if size % BSIZE != 0 {
            let map = self.block_map(image, &inode)?;
            let p = map
                .physical(lbn)
                .ok_or_else(|| bad_inode(dir, "directory block missing"))?;
            p * BSIZE + size % BSIZE
        } else if lbn >= NDADDR + NINDIR {
            return Ok(Room::Full);
        } else {
            // A block this folder lacked room for already holds the entry (counted once).
            if let Some(pending) = self.changes.pending.get_mut(&dir)
                && pending.fits(need)
            {
                return Ok(Room::Short);
            }
            // The indirect block is counted with a folder's first lacking block only.
            let new_ind = lbn == NDADDR && !self.changes.pending.contains_key(&dir);
            let Some(map) = self.take_blocks(1 + u64::from(new_ind))? else {
                if !self.changes.pending.contains_key(&dir) {
                    self.charge(RECORD)?;
                }
                let fresh = Pending {
                    chunks: BSIZE / DIRBLKSIZ as u64 - 1,
                    room: DIRBLKSIZ.saturating_sub(need),
                };
                self.changes.pending.insert(dir, fresh);
                return Ok(Room::Short);
            };
            let slot = |k: u64| {
                map.physical(k)
                    .ok_or_else(|| io::Error::other("internal error: slot out of range"))
            };
            let p = slot(u64::from(new_ind))?;
            if lbn < NDADDR {
                put(&mut b, 112 + 8 * lbn as usize, &p.to_le_bytes())?;
            } else if new_ind {
                let ind = slot(0)?;
                let mut block = vec![0u8; BSIZE as usize];
                put(&mut block, 0, &p.to_le_bytes())?;
                self.charge(BSIZE + RECORD)?;
                self.changes.fresh.insert(ind, block);
                put(&mut b, 208, &ind.to_le_bytes())?;
            } else {
                let ind = le64(&b, 208).filter(|&q| q != 0);
                let ind = ind.ok_or_else(|| bad_inode(dir, "indirect block missing"))?;
                let off = 8 * (lbn - NDADDR);
                match self.changes.fresh.get_mut(&ind) {
                    Some(block) => put(block, off as usize, &p.to_le_bytes())?,
                    None => self.plan_meta(ind * BSIZE + off, p.to_le_bytes().to_vec())?,
                }
            }
            let blocks = le64(&b, 24).unwrap_or(0) + map.owned() * SECTORS_PER_BLOCK;
            put(&mut b, 24, &blocks.to_le_bytes())?;
            let bytes = Self::map_bytes(&map);
            self.changes.bytes = self.changes.bytes.saturating_sub(bytes);
            // Only its first chunk is written (below): bytes past the folder's size are
            // never read, and each later chunk is written whole when the folder grows.
            p * BSIZE
        };
        let size = size + DIRBLKSIZ as u64;
        put(&mut b, 16, &size.to_le_bytes())?;
        self.plan_meta(at, b)?;
        let mut chunk = vec![0u8; DIRBLKSIZ];
        put(&mut chunk, 4, &(DIRBLKSIZ as u16).to_le_bytes())?;
        self.plan_meta(chunk_at, chunk)?;
        for list in self.changes.listings.values_mut() {
            for n in list.iter_mut().filter(|n| n.ino == dir) {
                n.size = size;
            }
        }
        Ok(Room::Slot(Slot {
            at: chunk_at,
            chunk: size - DIRBLKSIZ as u64,
            off: 0,
            used: 0,
        }))
    }

    fn summary_mut(&mut self, c: u64) -> io::Result<&mut Csum> {
        usize::try_from(c)
            .ok()
            .and_then(|i| self.summary.get_mut(i))
            .ok_or_else(|| io::Error::other(format!("internal error: summary {c} out of range")))
    }
}

/// Room left in a folder's growth block the plan lacked: whole chunks after the current
/// one, and bytes left in the current one (entries never straddle a chunk).
#[derive(Debug, Clone, Copy, Default)]
struct Pending {
    chunks: u64,
    room: usize,
}

impl Pending {
    /// Takes `need` bytes for an entry if they are left.
    fn fits(&mut self, need: usize) -> bool {
        if self.room >= need {
            self.room -= need;
        } else if self.chunks > 0 {
            self.chunks -= 1;
            self.room = DIRBLKSIZ.saturating_sub(need);
        } else {
            return false;
        }
        true
    }
}

/// Where [`Volume::find_slot`] starts in a folder: the entries in its chunks before byte
/// `chunk` have at most `below` bytes of room each (rooms there only shrink until
/// [`Volume::room_at`] moves `chunk` back), so a larger entry is looked for from `chunk` on.
#[derive(Debug, Clone, Copy, Default)]
struct Hint {
    chunk: u64,
    below: usize,
}

/// Where a new directory entry goes: a slot, none for want of a free block (counted), or
/// none because the folder can't grow.
enum Room {
    Slot(Slot),
    Short,
    Full,
}

/// Room for a new directory entry: in directory chunk `chunk` (image byte `at`), the entry
/// at byte `off` of the chunk keeps its first `used` bytes (0: it is unused, taken whole).
#[derive(Debug, Clone, Copy)]
struct Slot {
    at: u64,
    chunk: u64,
    off: usize,
    used: usize,
}

/// Fills `entry` (zeroed first, `d_reclen` = its length) as a directory entry naming `ino`.
fn dirent(entry: &mut [u8], ino: u64, kind: Kind, name: &str) -> io::Result<()> {
    let (reclen, namlen) = (u16::try_from(entry.len()), u8::try_from(name.len()));
    let (Ok(reclen), Ok(namlen), Ok(ino)) = (reclen, namlen, u32::try_from(ino)) else {
        return Err(io::Error::other(
            "internal error: directory entry out of range",
        ));
    };
    if dirsiz(name.len()) > entry.len() {
        return Err(io::Error::other(
            "internal error: directory entry too short",
        ));
    }
    entry.fill(0);
    let dtype = match kind {
        Kind::Dir => DT_DIR,
        Kind::File => DT_REG,
    };
    put(entry, 0, &ino.to_le_bytes())?;
    put(entry, 4, &reclen.to_le_bytes())?;
    put(entry, 6, &[dtype, namlen])?;
    put(entry, 8, name.as_bytes())
}

/// `v - 1`, a count that can't go below zero (checked at open).
fn less(v: u64) -> io::Result<u64> {
    v.checked_sub(1)
        .ok_or_else(|| io::Error::other("internal error: a free count below zero"))
}

/// `len` bytes of `data` from byte `off`.
fn slice(data: &Data, off: u64, len: u64) -> io::Result<Data> {
    Ok(match data {
        Data::Bytes(b) => Data::Bytes(
            usize::try_from(off)
                .ok()
                .zip(usize::try_from(off + len).ok())
                .and_then(|(lo, hi)| b.get(lo..hi))
                .ok_or_else(|| io::Error::other("internal error: data slice out of range"))?
                .to_vec(),
        ),
        Data::Zero(_) => Data::Zero(len),
        Data::Source { id, offset, .. } => Data::Source {
            id: *id,
            offset: offset + off,
            len,
        },
    })
}

/// A new file's block pointers, for its inode.
#[derive(Debug, Clone, Copy, Default)]
struct Ptrs {
    db: [u64; NDADDR as usize],
    ib: [u64; 3],
    /// `di_blocks`.
    blocks: u64,
}

/// Copies `v` into `b` at `at`, checked.
fn put(b: &mut [u8], at: usize, v: &[u8]) -> io::Result<()> {
    b.get_mut(at..at + v.len())
        .ok_or_else(|| io::Error::other(format!("internal error: write at {at} out of range")))?
        .copy_from_slice(v);
    Ok(())
}

fn live(offset: u64, bytes: Vec<u8>) -> Write {
    Write {
        offset,
        data: Data::Bytes(bytes),
        live: true,
    }
}

impl Volume {
    /// Every superblock copy the writer keeps in sync: the primary, then each group's.
    fn superblocks(&self) -> Vec<u64> {
        let g = &self.geom;
        let backups = (0..g.ncg).map(|c| (c * g.fpg + SBLKNO) * BSIZE);
        std::iter::once(SBLOCK_UFS2).chain(backups).collect()
    }

    /// The change set as live writes, between two writes of `fs_clean` in every superblock
    /// copy: the first clears it, the last sets it, after `fs_cstotal`. In between: directory
    /// chunks (entries removed before their inodes are freed), inodes, group blocks (`cg_cs`
    /// and the cluster summary renewed from the maps), the changed `fs_cs` entries.
    /// No change, no writes.
    fn writes(mut self) -> io::Result<Vec<Write>> {
        let c = &self.changes;
        if c.meta.is_empty() && c.groups.is_empty() {
            return Ok(Vec::new());
        }
        let copies = self.superblocks();
        let n = 3 * copies.len() + c.meta.len() + 2 * c.groups.len() + c.data.len() + c.fresh.len();
        self.charge((n * size_of::<Write>()) as u64 + 3 * copies.len() as u64 * RECORD)?;
        let mut out = Vec::with_capacity(n);
        // New data and indirect blocks first, into free space: nothing points at them yet.
        out.append(&mut self.changes.data);
        let fresh = std::mem::take(&mut self.changes.fresh);
        out.extend(fresh.into_iter().map(|(p, b)| Write {
            offset: p * BSIZE,
            data: Data::Bytes(b),
            live: false,
        }));
        out.extend(copies.iter().map(|&sb| live(sb + CLEAN_AT, vec![0])));
        let meta = std::mem::take(&mut self.changes.meta);
        let (dirs, inodes): (Vec<_>, Vec<_>) =
            meta.into_iter().partition(|(_, b)| b.len() == DIRBLKSIZ);
        out.extend(dirs.into_iter().map(|(at, b)| live(at, b)));
        out.extend(inodes.into_iter().map(|(at, b)| live(at, b)));
        let g = self.geom;
        let groups = std::mem::take(&mut self.changes.groups);
        for &c in &groups {
            let grp = usize::try_from(c)
                .ok()
                .and_then(|i| self.groups.get_mut(i))
                .ok_or_else(|| io::Error::other("internal error: group out of range"))?;
            let mut raw = std::mem::take(&mut grp.raw);
            for (at, v) in [(24, grp.cs.ndir), (28, grp.cs.nbfree), (32, grp.cs.nifree)] {
                put(&mut raw, at, &(v as u32).to_le_bytes())?;
            }
            let sum = cluster_sum(&raw, grp.clusteroff, g.cg_blocks(c));
            for (k, v) in sum.iter().enumerate().skip(1) {
                let at = grp.clustersumoff + 4 * k;
                put(&mut raw, at, &v.to_le_bytes())?;
            }
            out.push(live((c * g.fpg + CBLKNO) * BSIZE, raw));
        }
        for &c in &groups {
            let s = self.summary_mut(c)?;
            let mut b = vec![0u8; CSUM_SIZE as usize];
            for (i, v) in [s.ndir, s.nbfree, s.nifree].into_iter().enumerate() {
                b[4 * i..4 * i + 4].copy_from_slice(&(v as u32).to_le_bytes());
            }
            out.push(live(g.dblkno * BSIZE + c * CSUM_SIZE, b));
        }
        let t = self.total;
        let mut total = Vec::with_capacity(24);
        for v in [t.ndir, t.nbfree, t.nifree] {
            total.extend_from_slice(&v.to_le_bytes());
        }
        for &sb in &copies {
            out.push(live(sb + CSTOTAL_AT, total.clone()));
            out.push(live(sb + CLEAN_AT, vec![1]));
        }
        Ok(out)
    }
}

/// Plans the writes that apply `ops` to the image, in order (each op sees the ones before
/// it), reading it only. An op that does not apply (a missing path, a path through a file)
/// is a finding; so is an image Forge can't edit in place ([`Volume::open`]'s findings).
/// Blocks the plan frees are not reused by it; the image never grows.
pub fn plan(image: &mut File, ops: &[Op]) -> Result<Vec<Write>, Vec<String>> {
    plan_sized(image, ops).0
}

/// What a plan allocates in the image's free space and what it had: `needed` counts every
/// block it takes (file data, indirect blocks, new and grown directories), those it lacked
/// room for too; `free` the free blocks when the volume was opened. Bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Space {
    pub needed: u64,
    pub free: u64,
}

/// [`plan`], with the [`Space`] the plan counted (`None` when the volume could not be read or
/// was not planned).
pub fn plan_sized(
    image: &mut File,
    ops: &[Op],
) -> (Result<Vec<Write>, Vec<String>>, Option<Space>) {
    match Volume::open(image) {
        Ok(Ok(v)) => {
            let mut room = None;
            let planned = plan_volume(v, image, ops, &mut room);
            (planned, room)
        }
        Ok(Err(f)) => (Err(f), None),
        Err(e) => (Err(vec![e.to_string()]), None),
    }
}

fn plan_volume(
    mut v: Volume,
    image: &mut File,
    ops: &[Op],
    room: &mut Option<Space>,
) -> Result<Vec<Write>, Vec<String>> {
    let fail = |e: io::Error| vec![e.to_string()];
    let free = v.total.nbfree;
    let mut findings = Vec::new();
    for op in ops {
        // Once the plan is short of room, later ops may fail only because of it (a file left
        // out): their findings are dropped, the no-room finding stands for them.
        let short = v.changes.short > 0 || v.changes.short_inodes > 0;
        let done = match op {
            Op::Delete { path } => v.delete(image, path).map_err(fail)?,
            Op::Rename { from, to } => v.rename(image, from, to).map_err(fail)?,
            Op::Mkdir { path } => v.mkdir(image, path).map_err(fail)?,
            Op::Put { path, len, data } => v.put(image, path, *len, data).map_err(fail)?,
        };
        if let Err(f) = done
            && !short
        {
            findings.push(f);
        }
    }
    let c = &v.changes;
    *room = Some(Space {
        needed: (c.taken + c.short) * BSIZE,
        free: free * BSIZE,
    });
    if c.short > 0 {
        findings.push(format!(
            "the changes need {} MB, the image has {} MB free",
            ((c.taken + c.short) * BSIZE).div_ceil(1_000_000),
            free * BSIZE / 1_000_000
        ));
    }
    if c.short_inodes > 0 {
        findings.push(format!(
            "the image has no free inodes left for {} new files",
            c.short_inodes
        ));
    }
    if !findings.is_empty() {
        return Err(findings);
    }
    v.writes().map_err(fail)
}

/// One directory entry as on disk, `.` and `..` excepted.
struct Dirent<'a> {
    ino: u64,
    dtype: u8,
    name: &'a [u8],
    /// Byte offset of the entry in the directory.
    offset: u64,
    reclen: u16,
}

/// Checks the entries of one DIRBLKSIZ chunk at directory offset `base` and hands each in
/// use to `f`. `seen` counts the entries so far: the first two
/// of a directory must be `.` (`dir`) and `..` (`parent`) at offsets 0 and 12.
fn scan_chunk(
    dir: u64,
    parent: u64,
    chunk: &[u8],
    base: u64,
    seen: &mut u64,
    f: &mut dyn FnMut(Dirent<'_>) -> io::Result<()>,
) -> io::Result<()> {
    let mut off = 0usize;
    while off < chunk.len() {
        let at = base + off as u64;
        let bad = |why: &str| bad_inode(dir, format!("directory entry at byte {at}: {why}"));
        let ino = u64::from(le32(chunk, off).ok_or_else(|| bad("truncated"))?);
        let reclen = chunk
            .get(off + 4..off + 6)
            .map(|s| u16::from_le_bytes([s[0], s[1]]))
            .unwrap_or(0);
        let (dtype, namlen) = match chunk.get(off + 6..off + 8) {
            Some(s) => (s[0], usize::from(s[1])),
            None => return Err(bad("truncated")),
        };
        let r = usize::from(reclen);
        if r == 0 || r % 4 != 0 || r > chunk.len() - off || r < dirsiz(namlen) {
            return Err(bad(&format!("d_reclen {reclen} for a {namlen}-byte name")));
        }
        let name = &chunk[off + 8..off + 8 + namlen];
        let special = match *seen {
            0 => Some((&b"."[..], 0, dir)),
            1 => Some((&b".."[..], 12, parent)),
            _ => None,
        };
        if ino == 0 && special.is_some() {
            return Err(bad("'.' or '..' missing"));
        }
        if ino != 0 {
            if namlen == 0
                || name.iter().any(|&c| c == 0 || c == b'/')
                || chunk[off + 8 + namlen] != 0
            {
                return Err(bad("bad name"));
            }
            match special {
                Some((want, wat, wino)) => {
                    if name != want || at != wat || ino != wino || dtype != DT_DIR {
                        return Err(bad("'.' or '..' wrong"));
                    }
                }
                None if name == b"." || name == b".." => return Err(bad("extra '.' or '..'")),
                None => {
                    let e = Dirent {
                        ino,
                        dtype,
                        name,
                        offset: at,
                        reclen,
                    };
                    f(e)?;
                }
            }
            *seen += 1;
        }
        off += r;
    }
    Ok(())
}

/// Walks one inode's block pointers into a [`BlockMap`], charging what it keeps (and each
/// indirect block while it is read) to what the budget leaves.
struct Mapper<'a> {
    vol: &'a Volume,
    ino: u64,
    /// Logical blocks the inode's size covers.
    n: u64,
    map: BlockMap,
    left: u64,
}

/// Elements to reserve before a push onto a vector of `len` of `cap`: none while there is
/// room, else as many again (at least 4), so the capacity charged is the capacity held.
fn room(len: usize, cap: usize) -> usize {
    if len < cap { 0 } else { cap.max(4) }
}

impl Mapper<'_> {
    /// Charges the growth [`room`] reserves for a vector of `size`-byte elements.
    fn room(&mut self, len: usize, cap: usize, size: usize) -> io::Result<()> {
        self.charge((room(len, cap) * size) as u64)
    }

    fn charge(&mut self, bytes: u64) -> io::Result<()> {
        if bytes > self.left {
            let used = self.vol.budget_left() - self.left;
            return Err(too_big_io(self.vol.held() + used + bytes));
        }
        self.left -= bytes;
        Ok(())
    }

    /// A pointer to `p` covering logical blocks from `lbn`: within the size, a block a
    /// file may own.
    fn check(&self, lbn: u64, p: u64) -> io::Result<()> {
        if lbn >= self.n {
            return Err(bad_inode(
                self.ino,
                format!("block {p} mapped at logical block {lbn}, past its size"),
            ));
        }
        match self.vol.block_problem(p) {
            Some(why) => Err(bad_inode(
                self.ino,
                format!("block {p} at logical block {lbn} is {why}"),
            )),
            None => Ok(()),
        }
    }

    fn data(&mut self, lbn: u64, p: u64) -> io::Result<()> {
        if p == 0 {
            return Ok(());
        }
        self.check(lbn, p)?;
        if let Some(r) = self.map.data.last_mut()
            && r.lbn + r.len == lbn
            && r.block + r.len == p
        {
            r.len += 1;
            return Ok(());
        }
        let (len, cap) = (self.map.data.len(), self.map.data.capacity());
        self.room(len, cap, size_of::<Run>())?;
        self.map.data.reserve_exact(room(len, cap));
        self.map.data.push(Run {
            lbn,
            block: p,
            len: 1,
        });
        Ok(())
    }

    /// The indirect block `p` of `level` (1..=3), mapping logical blocks from `first`.
    fn indirect(&mut self, image: &mut File, level: u32, p: u64, first: u64) -> io::Result<()> {
        if p == 0 {
            return Ok(());
        }
        self.check(first, p)?;
        let (len, cap) = (self.map.indirect.len(), self.map.indirect.capacity());
        self.room(len, cap, 8)?;
        self.map.indirect.reserve_exact(room(len, cap));
        self.charge(BSIZE)?;
        self.map.indirect.push(p);
        let mut block = vec![0u8; BSIZE as usize];
        match self.vol.changes.fresh.get(&p) {
            Some(planned) => block.copy_from_slice(planned),
            // A live block may carry a pointer a folder's growth planned (`grow_dir`).
            None => self.vol.read_planned(image, p * BSIZE, &mut block)?,
        }
        let per = span(level - 1);
        for j in 0..NINDIR {
            let child = le64(&block, j as usize * 8).unwrap_or(0);
            let lbn = first + j * per;
            match level {
                1 => self.data(lbn, child)?,
                _ => self.indirect(image, level - 1, child, lbn)?,
            }
        }
        self.left += BSIZE;
        Ok(())
    }
}

/// Decodes and checks one on-disk inode; `maxfilesize` is `fs_maxfilesize`.
fn parse_inode(ino: u64, b: &[u8], maxfilesize: u64) -> io::Result<Inode> {
    let w16 = |at: usize| {
        b.get(at..at + 2)
            .map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
    };
    let w64 = |at: usize| le64(b, at).unwrap_or(0);
    let mode = w16(0);
    let kind = match mode & IFMT {
        IFDIR => Kind::Dir,
        IFREG => Kind::File,
        0 => return Err(bad_inode(ino, "allocated with mode 0")),
        t => {
            return Err(other_tool_io(
                ino,
                format!("mode {t:#o}, not a file or folder"),
            ));
        }
    };
    if le32(b, 92) != Some(0) || w64(96) != 0 || w64(104) != 0 {
        return Err(other_tool_io(ino, "extended attributes"));
    }
    let (nlink, size, blocks) = (w16(2), w64(16), w64(24));
    let mut db = [0u64; NDADDR as usize];
    for (i, p) in db.iter_mut().enumerate() {
        *p = w64(112 + 8 * i);
    }
    let ib = [w64(208), w64(216), w64(224)];
    let why = match kind {
        Kind::Dir if size == 0 || size % DIRBLKSIZ as u64 != 0 || size > MAXDIRSIZE => {
            Some(format!("directory size {size}"))
        }
        Kind::Dir if nlink < 2 => Some(format!("directory link count {nlink}")),
        Kind::File if size > maxfilesize => Some(format!("size {size}")),
        Kind::File if nlink == 0 => Some("link count 0".into()),
        _ => None,
    };
    if let Some(why) = why {
        return Err(bad_inode(ino, why));
    }
    Ok(Inode {
        ino,
        kind,
        mode,
        nlink,
        size,
        blocks,
        db,
        ib,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    use ps5upload_fpkg::source::FolderSource;

    /// The cylinder groups count against the budget: past it, the open is a finding.
    #[test]
    fn past_the_budget_is_refused() {
        let dir = crate::test_tmp::temp_path(format!("forge-ufs2-budget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/sce_sys")).unwrap();
        std::fs::write(dir.join("src/eboot.bin"), b"x").unwrap();
        std::fs::write(dir.join("src/sce_sys/param.json"), b"{}").unwrap();
        let mut tree = FolderSource::open(&dir.join("src")).unwrap();
        let cancel = AtomicBool::new(false);
        let layout = crate::plan(&tree, &crate::Options::default(), &cancel).unwrap();
        let path = dir.join("x.ffpkg");
        let mut out = File::create_new(&path).unwrap();
        crate::write(&mut tree, &layout, &mut out, &cancel, &mut |_, _| {}).unwrap();
        drop(out);
        let mut image = File::open(&path).unwrap();
        let v = Volume::open(&mut image).unwrap().unwrap();
        assert!(v.held() > 0 && v.budget_left() == MEMORY_LIMIT - v.held());
        // Retained: superblock + each group's block and records; peak adds the raw summary.
        let g = &v.geom;
        let records = (size_of::<Group>() + size_of::<Csum>()) as u64;
        assert_eq!(v.held(), 8192 + g.ncg * (BSIZE + records));
        assert_eq!(peak(g), v.held() + g.csblocks * BSIZE);
        let at = Volume::open_within(&mut image, peak(g)).unwrap();
        assert!(at.is_ok(), "{at:?}");
        // A listing reads a 64 KiB directory block on top of the block map: past the budget
        // it is refused; with the whole limit it reads.
        let tight = Volume::open_within(&mut image, v.held() + BSIZE)
            .unwrap()
            .unwrap();
        let e = tight.list(&mut image, "").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::OutOfMemory, "{e}");
        assert!(e.to_string().contains("too big to edit in place"), "{e}");
        assert_eq!(v.list(&mut image, "").unwrap().len(), 2);
        let f = Volume::open_within(&mut image, peak(g) - 1)
            .unwrap()
            .unwrap_err();
        assert!(
            f[0].contains("too big to edit in place") && f[0].contains("rebuild"),
            "{f:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
