//! Cylinder-group geometry: the same arithmetic as FreeBSD `newfs` (sbin/newfs/mkfs.c) for
//! `-O 2 -b 65536 -f 65536 -m 0 -S 4096 -i <density>`, so `dumpfs` of our image and of a
//! `newfs` of the same size agree on fpg/ipg/ncg.
//!
//! Portions derived from FreeBSD sbin/newfs/mkfs.c:
//!   Copyright (c) 2002 Networks Associates Technology, Inc. All rights reserved.
//!   Copyright (c) 1980, 1989, 1993 The Regents of the University of California.
//!   (BSD-3-Clause; see the FreeBSD source for the full notice.)

use ps5upload_fpkg::{Error, Result};

/// Block size = fragment size: one fragment per block, so every allocation is a whole block
/// and the fragment maps never hold partial blocks.
pub const BSIZE: u64 = 65536;
/// `fs_sblkno`/`fs_cblkno`/`fs_iblkno` for this geometry: superblock copy in block 2, the
/// cylinder group header in block 3, inodes from block 4 (`roundup(howmany(65536 + 8192,
/// 65536), 1)` and so on, exactly as newfs computes them).
pub const SBLKNO: u64 = 2;
pub const CBLKNO: u64 = 3;
pub const IBLKNO: u64 = 4;
/// 256-byte UFS2 inodes, 256 to a block.
pub const INOPB: u64 = BSIZE / 256;
/// Block pointers per indirect block.
pub const NINDIR: u64 = BSIZE / 8;
/// `fs_contigsumsize`: newfs takes `MIN(maxcontig, FS_MAXCONTIG)` with maxcontig =
/// MAXPHYS (1 MiB on FreeBSD 13+) / bsize = 16.
pub const CONTIGSUMSIZE: u64 = 16;
/// `sizeof(struct cg)`.
pub const CG_HDR: u64 = 168;
const CGSIZEFUDGE: u64 = 8;
const MINCYLGRPS: u64 = 4;
/// Bytes of `struct csum` per cylinder group in the summary area.
pub const CSUM_SIZE: u64 = 16;

/// One fixed layout: everything that follows from the size and the inode density.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geom {
    /// `fs_size`, in blocks (= fragments).
    pub size: u64,
    /// Bytes per inode the layout was built with (`-i`).
    pub density: u64,
    pub fpg: u64,
    pub ipg: u64,
    pub ncg: u64,
    /// First data block of every group (`fs_dblkno`).
    pub dblkno: u64,
    /// Blocks of the cylinder-group summary area at the start of group 0's data.
    pub csblocks: u64,
}

/// `CGSIZE(fs)` for UFS2 (no old rotational tables) with cluster maps.
pub fn cgsize(fpg: u64, ipg: u64) -> u64 {
    CG_HDR + ipg.div_ceil(8) + fpg.div_ceil(8) + 4 + CONTIGSUMSIZE * 4 + fpg.div_ceil(8)
}

fn ipg_for(fpg: u64, fpi: u64) -> u64 {
    fpg.div_ceil(fpi).next_multiple_of(INOPB)
}

impl Geom {
    /// newfs's choice of fpg/ipg for `size` blocks at `density` bytes per inode, then our own
    /// stricter last-group rule. Errors when the size cannot hold a single group.
    pub fn new(size: u64, density: u64) -> Result<Geom> {
        if size < 8 {
            return Err(Error::Format(format!(
                "UFS2 image of {size} blocks is too small"
            )));
        }
        let fpi = (density / BSIZE).max(1);
        // newfs would double the block size here; this writer has exactly one geometry.
        let maxinum = (1u64 << 32) - INOPB;
        if fpi < 1 + size / maxinum {
            return Err(Error::Format(format!(
                "UFS2 image of {size} blocks needs more than 2^32 inodes"
            )));
        }
        let minfpg = (fpi * INOPB).min(size);
        let mut ipg = INOPB;
        let mut fpg = (IBLKNO + ipg / INOPB).max(minfpg);
        ipg = ipg_for(fpg, fpi);
        fpg = (IBLKNO + ipg / INOPB).max(minfpg);
        ipg = ipg_for(fpg, fpi);
        if cgsize(fpg, ipg) >= BSIZE - CGSIZEFUDGE {
            // newfs lowers the density until the minimal group fits; with -i >= 64 KiB and
            // 64 KiB blocks the minimal group is always tiny, so this cannot happen.
            return Err(Error::Format(format!(
                "no UFS2 group fits density {density}"
            )));
        }
        // newfs grows the group one block at a time until its map fills a block, or until
        // there would be fewer than MINCYLGRPS groups (MAXBLKSPERCG = "infinity"). The map
        // only grows with the group, so both stops are found directly: `few` is the first
        // group size leaving fewer than MINCYLGRPS groups, `full` the first whose map
        // reaches the limit; whichever newfs meets first wins (`few` on a tie).
        let limit = BSIZE - CGSIZEFUDGE;
        let few = (size / MINCYLGRPS + 1).max(fpg);
        let (mut lo, mut hi) = (fpg, few);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if cgsize(mid, ipg_for(mid, fpi)) >= limit {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        fpg = if lo == few || cgsize(lo, ipg_for(lo, fpi)) == limit {
            lo
        } else {
            lo - 1
        };
        ipg = ipg_for(fpg, fpi);
        // newfs only demands that the last group hold its own metadata (sb, cg, inodes).
        let lastminfpg = IBLKNO + ipg / INOPB;
        loop {
            let rem = size % fpg;
            if size < lastminfpg {
                return Err(Error::Format(format!(
                    "UFS2 image of {size} blocks is too small"
                )));
            }
            if rem >= lastminfpg || rem == 0 {
                break;
            }
            fpg -= 1;
            ipg = ipg_for(fpg, fpi);
        }
        let ncg = size.div_ceil(fpg);
        // FreeBSD's validate_sblock bound, on the final, rounded geometry.
        if ncg * ipg > maxinum {
            return Err(Error::Format(format!(
                "UFS2 image of {size} blocks needs more than 2^32 inodes"
            )));
        }
        let dblkno = IBLKNO + ipg / INOPB;
        let csblocks = (ncg * CSUM_SIZE).div_ceil(BSIZE);
        Ok(Geom {
            size,
            density,
            fpg,
            ipg,
            ncg,
            dblkno,
            csblocks,
        })
    }

    /// Blocks in group `c` (`cg_ndblk`); only the last one can be short.
    pub fn cg_blocks(&self, c: u64) -> u64 {
        (self.size - c * self.fpg).min(self.fpg)
    }

    /// Our rule on top of newfs's: every group, the last included, holds all of its
    /// metadata *and* at least one data block, and group 0 also fits the summary area plus
    /// a data block (FreeBSD's `validate_sblock` wants the summary to end inside group 0).
    /// UFS2Tool v4.1 laid out a last group shorter than its own metadata and wrote past
    /// `fs_size`; `plan` grows the image until this holds.
    pub fn last_group_ok(&self) -> bool {
        let first = self.cg_blocks(0);
        let last = self.cg_blocks(self.ncg - 1);
        first > self.dblkno + self.csblocks && last > self.dblkno
    }

    /// Data blocks this writer can allocate: every group's blocks past `dblkno`, minus the
    /// summary area. ponytail: the two free blocks in front of each backup superblock
    /// (groups 1..) stay free instead of holding data; 128 KiB per group, ~0.001%.
    pub fn data_capacity(&self) -> u64 {
        self.size - self.ncg * self.dblkno - self.csblocks
    }

    pub fn inode_capacity(&self) -> u64 {
        self.ncg * self.ipg
    }

    /// Data blocks of group `c` this writer allocates from.
    pub fn cg_data(&self, c: u64) -> u64 {
        let extra = if c == 0 { self.csblocks } else { 0 };
        self.cg_blocks(c) - self.dblkno - extra
    }

    /// First allocatable block of group `c`.
    pub fn cg_data_start(&self, c: u64) -> u64 {
        let extra = if c == 0 { self.csblocks } else { 0 };
        c * self.fpg + self.dblkno + extra
    }

    /// Physical block of data index `g` (the g-th allocatable block), and how many
    /// allocatable blocks follow it contiguously in the same group (itself included).
    pub fn locate(&self, g: u64) -> (u64, u64) {
        let d0 = self.cg_data(0);
        if g < d0 {
            return (self.cg_data_start(0) + g, d0 - g);
        }
        let full = self.fpg - self.dblkno;
        let rest = g - d0;
        let c = 1 + rest / full;
        let off = rest % full;
        (self.cg_data_start(c) + off, self.cg_data(c) - off)
    }

    /// Data index range `[start, end)` that lives in group `c`.
    pub fn cg_data_range(&self, c: u64) -> (u64, u64) {
        let start = if c == 0 {
            0
        } else {
            self.cg_data(0) + (c - 1) * (self.fpg - self.dblkno)
        };
        (start, start + self.cg_data(c))
    }

    /// `fs_dsize`, as newfs computes it.
    pub fn dsize(&self) -> u64 {
        self.size - SBLKNO - self.ncg * (self.dblkno - SBLKNO) - self.csblocks
    }
}

/// `-i` as SMP recommends and the user's known-good `ffpkg.sh` computes it: the image size
/// over (files + dirs + 2048), rounded down to 4 KiB and clamped to 64..256 KiB. `bytes`
/// is the data the image holds (the tree's blocks plus the free spare), what the image
/// is before metadata; `entries` counts files and directories, the root included
/// (`find SRC | wc -l`).
pub fn density_for(bytes: u64, entries: u64) -> u64 {
    let d = bytes / entries.saturating_add(2048);
    (d / 4096 * 4096).clamp(64 * 1024, 256 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The UFS2Tool v4.1 bug class: for every small size, any layout we accept has a last
    /// group with its full metadata plus data, and fs_size sits inside the last group.
    #[test]
    fn last_group_never_shorter_than_its_metadata() {
        for density in [64 * 1024, 128 * 1024, 192 * 1024, 256 * 1024] {
            for size in 8..6000u64 {
                let Ok(g) = Geom::new(size, density) else {
                    continue;
                };
                assert!(g.size > (g.ncg - 1) * g.fpg && g.size <= g.ncg * g.fpg);
                let last = g.cg_blocks(g.ncg - 1);
                assert!(
                    last >= g.dblkno,
                    "size {size}: last group {last} < {}",
                    g.dblkno
                );
                if g.last_group_ok() {
                    assert!(last > g.dblkno);
                    assert!(g.cg_blocks(0) > g.dblkno + g.csblocks);
                    assert_eq!(
                        (0..g.ncg).map(|c| g.cg_data(c)).sum::<u64>(),
                        g.data_capacity()
                    );
                }
                assert!(cgsize(g.fpg, g.ipg) <= BSIZE);
            }
        }
    }

    #[test]
    fn locate_matches_group_ranges() {
        let g = Geom::new(40_000, 64 * 1024).unwrap();
        assert!(g.ncg > 1);
        let mut idx = 0;
        for c in 0..g.ncg {
            let (s, e) = g.cg_data_range(c);
            assert_eq!(s, idx);
            assert_eq!(g.locate(s), (g.cg_data_start(c), e - s));
            assert_eq!(g.locate(e - 1).1, 1);
            idx = e;
        }
        assert_eq!(idx, g.data_capacity());
    }

    /// FreeBSD refuses ncg * ipg > 2^32 - INOPB; the rounded geometry is what counts
    /// (an estimate from the density alone passes here).
    #[test]
    fn inode_limit_on_final_geometry() {
        let limit = (1u64 << 32) - INOPB;
        let mut refused = 0;
        for size in (limit - 200_000..limit).step_by(9_973) {
            match Geom::new(size, 64 * 1024) {
                Ok(g) => assert!(g.inode_capacity() <= limit, "size {size}"),
                Err(_) => refused += 1,
            }
        }
        assert!(refused > 0);
    }

    /// newfs's group-growing loop, one block at a time, as `Geom::new` had it: the binary
    /// search there must pick the same group size for every image size and density.
    #[test]
    fn group_size_matches_newfs_loop() {
        let slow = |size: u64, density: u64| -> u64 {
            let fpi = (density / BSIZE).max(1);
            let minfpg = (fpi * INOPB).min(size);
            let mut ipg = INOPB;
            let mut fpg = (IBLKNO + ipg / INOPB).max(minfpg);
            ipg = ipg_for(fpg, fpi);
            fpg = (IBLKNO + ipg / INOPB).max(minfpg);
            loop {
                ipg = ipg_for(fpg, fpi);
                if size / fpg < MINCYLGRPS {
                    break;
                }
                let cs = cgsize(fpg, ipg);
                if cs < BSIZE - CGSIZEFUDGE {
                    fpg += 1;
                    continue;
                }
                if cs > BSIZE - CGSIZEFUDGE {
                    fpg -= 1;
                    ipg = ipg_for(fpg, fpi);
                }
                break;
            }
            let lastminfpg = IBLKNO + ipg / INOPB;
            while !size.is_multiple_of(fpg) && size % fpg < lastminfpg {
                fpg -= 1;
            }
            fpg
        };
        let sizes = (8..3000u64)
            .chain((3000..2_000_000).step_by(7919))
            .chain([819_201, 870_401, 1_048_576, 2_458_626, 2_464_000]);
        for size in sizes {
            for density in [64 * 1024, 128 * 1024, 192 * 1024, 256 * 1024] {
                if let Ok(g) = Geom::new(size, density) {
                    assert_eq!(g.fpg, slow(size, density), "size {size}, density {density}");
                }
            }
        }
    }

    /// Large images: groups grow until the cg map fills a block, like newfs.
    #[test]
    fn big_image_fills_the_cg_block() {
        let g = Geom::new(100 * 1024 * 1024 * 1024 / BSIZE, 256 * 1024).unwrap();
        assert!(cgsize(g.fpg, g.ipg) <= BSIZE - CGSIZEFUDGE);
        assert!(cgsize(g.fpg + 1, ipg_for(g.fpg + 1, 4)) > BSIZE - CGSIZEFUDGE - 64);
        assert_eq!(g.csblocks, 1);
    }
}
