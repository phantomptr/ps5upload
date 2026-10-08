//! Test helpers: a synthetic source tree, an image builder, and a small independent fsck.
//!
//! The checker reads the raw bytes with its own parser (offsets from FreeBSD fs.h /
//! dinode.h / dir.h), not the writer's code, so the two can't share a misreading.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use ps5_dump_forge_ufs2::{Layout, Options, Report};
use ps5upload_fpkg::Result;
use ps5upload_fpkg::source::{SourceFile, SourceTree};

/// Files whose bytes are computed from (file index, offset): no disk needed, and a block
/// written to the wrong place shows up as wrong bytes.
pub struct Synth {
    pub files: Vec<SourceFile>,
    pub empty: Vec<String>,
}

impl Synth {
    pub fn new(files: &[(&str, u64)]) -> Self {
        Synth {
            files: files
                .iter()
                .map(|(p, s)| SourceFile {
                    path: p.to_string(),
                    size: *s,
                })
                .collect(),
            empty: Vec::new(),
        }
    }
}

pub fn pattern(file: usize, offset: u64, len: usize) -> Vec<u8> {
    (0..len as u64)
        .map(|i| {
            let o = offset + i;
            (o.wrapping_mul(31) ^ (o >> 16).wrapping_mul(131) ^ (file as u64).wrapping_mul(7)) as u8
        })
        .collect()
}

impl SourceTree for Synth {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }
    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let size = self.files.iter().find(|f| f.path == path).unwrap().size;
        self.read_range(path, 0, size as usize)
    }
    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let i = self.files.iter().position(|f| f.path == path).unwrap();
        let size = self.files[i].size;
        let len = (len as u64).min(size.saturating_sub(offset)) as usize;
        Ok(pattern(i, offset, len))
    }
    fn empty_dirs(&self) -> &[String] {
        &self.empty
    }
    fn describe(&self) -> String {
        "synthetic".into()
    }
}

/// A scratch path under cargo's per-target temp dir.
pub fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ufs2");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// plan + write into `<scratch>/<name>.ffpkg`.
pub fn build(name: &str, tree: &mut dyn SourceTree, opts: &Options) -> (PathBuf, Layout, Report) {
    let cancel = AtomicBool::new(false);
    let layout = ps5_dump_forge_ufs2::plan(tree, opts, &cancel).expect("plan");
    let path = scratch(&format!("{name}.ffpkg"));
    let _ = std::fs::remove_file(&path);
    let mut out = File::create_new(&path).unwrap();
    let report = ps5_dump_forge_ufs2::write(tree, &layout, &mut out, &cancel, &mut |_, _| {})
        .expect("write");
    (path, layout, report)
}

/// Every file of `tree` reads back identically through the vendored UFS2 reader.
pub fn compare_with_reader(image: &Path, tree: &mut dyn SourceTree) {
    let mut img = ps5upload_fpkg::ufs2_source::Ufs2Source::open(image).expect("reader opens image");
    let mut want: Vec<(String, u64)> = tree
        .files()
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    want.sort();
    let got: Vec<(String, u64)> = img
        .files()
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    assert_eq!(got, want, "file list read back");
    for (path, size) in want {
        let mut off = 0;
        while off < size {
            let n = (size - off).min(8 << 20) as usize;
            let a = tree.read_range(&path, off, n).unwrap();
            let b = img.read_range(&path, off, n).unwrap();
            assert!(a == b, "{path}: bytes differ in [{off}, +{n})");
            off += n as u64;
        }
    }
}

/// The user's ffpkg.sh check, verbatim: int32 superblock fields at 65536 + offset, and
/// last = fs_size - (fs_ncg - 1) * fs_fpg must be >= fs_dblkno.
pub fn script_last_group_check(image: &Path) {
    let mut f = File::open(image).unwrap();
    let sb = |f: &mut File, off: u64| i32_at(&read_at(f, 65536 + off, 4), 0);
    let (size, ncg, fpg, dblkno) = (
        sb(&mut f, 1080),
        sb(&mut f, 44),
        sb(&mut f, 188),
        sb(&mut f, 20),
    );
    let last = size - (ncg - 1) * fpg;
    assert!(
        last >= dblkno,
        "{}: last group {last} < dblkno {dblkno}",
        image.display()
    );
}

fn u16_at(b: &[u8], at: usize) -> u64 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap()).into()
}
fn u32_at(b: &[u8], at: usize) -> u64 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap()).into()
}
fn i32_at(b: &[u8], at: usize) -> i64 {
    i32::from_le_bytes(b[at..at + 4].try_into().unwrap()).into()
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn read_at(f: &mut File, off: u64, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    f.seek(SeekFrom::Start(off)).unwrap();
    f.read_exact(&mut buf).unwrap();
    buf
}

/// What the checker found: problems, and the tree it walked.
#[derive(Debug, Default)]
pub struct Fsck {
    pub errors: Vec<String>,
    pub dirs: Vec<String>,
    pub files: Vec<(String, u64)>,
    pub ncg: u64,
    pub fpg: u64,
    pub dblkno: u64,
    pub size: u64,
}

struct Ino {
    mode: u64,
    nlink: u64,
    size: u64,
    blocks: u64,
    db: [u64; 12],
    ib: [u64; 3],
}

/// A small fsck: superblock and backups, cylinder groups and their maps, the summary area
/// and totals, every block referenced once, link counts, directory records, di_blocks.
pub fn fsck(image: &Path) -> Fsck {
    let mut f = File::open(image).unwrap();
    let len = f.metadata().unwrap().len();
    let mut r = Fsck::default();
    let err = |r: &mut Fsck, m: String| {
        if r.errors.len() < 50 {
            r.errors.push(m)
        }
    };
    let sb = read_at(&mut f, 65536, 8192);
    if u32_at(&sb, 1372) != 0x19540119 {
        err(&mut r, "bad superblock magic".into());
        return r;
    }
    let bsize = u32_at(&sb, 48);
    let fsize = u32_at(&sb, 52);
    let (sblkno, cblkno, iblkno, dblkno) = (
        u32_at(&sb, 8),
        u32_at(&sb, 12),
        u32_at(&sb, 16),
        u32_at(&sb, 20),
    );
    let ncg = u32_at(&sb, 44);
    let ipg = u32_at(&sb, 184);
    let fpg = u32_at(&sb, 188);
    let size = u64_at(&sb, 1080);
    let dsize = u64_at(&sb, 1088);
    let csaddr = u64_at(&sb, 1096);
    let cssize = u32_at(&sb, 156);
    let contig = u32_at(&sb, 1316);
    (r.ncg, r.fpg, r.dblkno, r.size) = (ncg, fpg, dblkno, size);
    let checks = [
        ("bsize", bsize, 65536),
        ("fsize", fsize, 65536),
        ("frag", u32_at(&sb, 56), 1),
        ("sblkno", sblkno, 2),
        ("cblkno", cblkno, 3),
        ("iblkno", iblkno, 4),
        ("dblkno", dblkno, 4 + ipg / 256),
        ("fsbtodb", u32_at(&sb, 100), 7),
        ("sbsize", u32_at(&sb, 104), 8192),
        ("nindir", u32_at(&sb, 116), 8192),
        ("inopb", u32_at(&sb, 120), 256),
        ("minfree", u32_at(&sb, 60), 0),
        ("clean", sb[209] as u64, 1),
        ("flags", u32_at(&sb, 1312), 0),
        ("sblockloc", u64_at(&sb, 1000), 65536),
        ("sblockactualloc", u64_at(&sb, 992), 65536),
        ("csaddr", csaddr, dblkno),
        ("cssize", cssize, (ncg * 16).div_ceil(65536) * 65536),
        ("cgsize", u32_at(&sb, 160), 65536),
        ("maxbsize", u32_at(&sb, 860), 65536),
        ("contigsumsize", contig, 16),
        ("maxsymlinklen", u32_at(&sb, 1320), 120),
        ("image length", len, size * 65536),
    ];
    for (name, got, want) in checks {
        if got != want {
            err(&mut r, format!("superblock {name} = {got}, want {want}"));
        }
    }
    if !(size > (ncg - 1) * fpg && size <= ncg * fpg) {
        err(
            &mut r,
            format!("fs_size {size} outside groups ({ncg} x {fpg})"),
        );
    }
    let csblocks = cssize / 65536;
    if dsize != size - sblkno - ncg * (dblkno - sblkno) - csblocks {
        err(&mut r, format!("fs_dsize {dsize} wrong"));
    }
    let cg_blocks = |c: u64| (size - c * fpg).min(fpg);
    // The UFS2Tool v4.1 bug: a last group shorter than its own metadata.
    if cg_blocks(ncg - 1) <= dblkno {
        err(
            &mut r,
            format!(
                "last group has {} blocks <= metadata {dblkno}",
                cg_blocks(ncg - 1)
            ),
        );
    }
    if csaddr + csblocks >= fpg.min(size) {
        err(&mut r, "summary area does not end inside group 0".into());
    }

    // Backup superblocks: identical apart from their own location.
    for c in 0..ncg {
        let at = (c * fpg + sblkno) * fsize;
        let b = read_at(&mut f, at, 8192);
        if u64_at(&b, 992) != at {
            err(
                &mut r,
                format!("cg {c}: backup sb actualloc {} != {at}", u64_at(&b, 992)),
            );
        }
        if b[..992] != sb[..992] || b[1000..] != sb[1000..] {
            err(
                &mut r,
                format!("cg {c}: backup superblock differs from primary"),
            );
        }
    }

    // Block ownership: 0 = free, else number of claims.
    let mut used = vec![0u8; size as usize];
    let claim = |used: &mut Vec<u8>, b: u64, who: &str, r: &mut Fsck| {
        if b >= size {
            err(r, format!("{who}: block {b} past fs_size {size}"));
            return;
        }
        used[b as usize] = used[b as usize].saturating_add(1);
        if used[b as usize] > 1 {
            err(r, format!("{who}: block {b} claimed twice"));
        }
    };
    for c in 0..ncg {
        let from = if c == 0 { 0 } else { c * fpg + sblkno };
        for b in from..c * fpg + dblkno {
            claim(&mut used, b, "metadata", &mut r);
        }
    }
    for b in csaddr..csaddr + csblocks {
        claim(&mut used, b, "summary", &mut r);
    }

    // Inodes.
    let mut inodes: BTreeMap<u64, Ino> = BTreeMap::new();
    let mut cgs = Vec::new();
    for c in 0..ncg {
        let cg = read_at(&mut f, (c * fpg + cblkno) * fsize, 65536);
        if u32_at(&cg, 4) != 0x090255 {
            err(&mut r, format!("cg {c}: bad magic"));
            continue;
        }
        let inited = u32_at(&cg, 120);
        let table = read_at(&mut f, (c * fpg + iblkno) * fsize, (ipg * 256) as usize);
        for i in 0..ipg {
            let ino = c * ipg + i;
            let d = &table[(i * 256) as usize..(i * 256 + 256) as usize];
            let mode = u16_at(d, 0);
            if mode == 0 {
                if d.iter().any(|&x| x != 0) && ino >= 2 {
                    err(&mut r, format!("inode {ino}: unallocated but not zero"));
                }
                continue;
            }
            if i >= inited {
                err(
                    &mut r,
                    format!("inode {ino}: allocated beyond cg_initediblk {inited}"),
                );
            }
            let mut db = [0; 12];
            for (k, p) in db.iter_mut().enumerate() {
                *p = u64_at(d, 112 + 8 * k);
            }
            let ib = [u64_at(d, 208), u64_at(d, 216), u64_at(d, 224)];
            inodes.insert(
                ino,
                Ino {
                    mode,
                    nlink: u16_at(d, 2),
                    size: u64_at(d, 16),
                    blocks: u64_at(d, 24),
                    db,
                    ib,
                },
            );
            if mode & 0o777 != 0o777 || u32_at(d, 4) != 0 || u32_at(d, 8) != 0 {
                err(
                    &mut r,
                    format!("inode {ino}: mode {mode:o} / owner not 0777 root"),
                );
            }
        }
        cgs.push(cg);
    }
    if !inodes.contains_key(&2) {
        err(&mut r, "no root inode".into());
        return r;
    }

    // Block maps of every inode, with di_blocks and size checks.
    let mut dir_data: HashMap<u64, Vec<u8>> = HashMap::new();
    for (&ino, n) in &inodes {
        let who = format!("inode {ino}");
        let nblocks = n.size.div_ceil(65536);
        let mut count = 0u64;
        let mut lbn = 0u64;
        let mut data: Vec<u64> = Vec::new();
        for &p in &n.db {
            if lbn < nblocks {
                if p == 0 {
                    err(&mut r, format!("{who}: hole at lbn {lbn}"));
                } else {
                    claim(&mut used, p, &who, &mut r);
                    data.push(p);
                    count += 1;
                }
            } else if p != 0 {
                err(&mut r, format!("{who}: pointer past size at lbn {lbn}"));
            }
            lbn += 1;
        }
        let mut blocks: Vec<(u64, bool)> = Vec::new();
        let mut problems: Vec<String> = Vec::new();
        for (level, &p) in n.ib.iter().enumerate() {
            walk_indirect(
                &mut f,
                p,
                level as u32 + 1,
                &mut lbn,
                nblocks,
                &mut |b, is_data| blocks.push((b, is_data)),
                &mut |m| problems.push(m),
            );
        }
        for m in problems {
            err(&mut r, format!("{who}: {m}"));
        }
        for (b, is_data) in blocks {
            claim(&mut used, b, &who, &mut r);
            count += 1;
            if is_data {
                data.push(b);
            }
        }
        if data.len() as u64 != nblocks {
            err(
                &mut r,
                format!("{who}: {} data blocks for size {}", data.len(), n.size),
            );
        }
        if n.blocks != count * 128 {
            err(
                &mut r,
                format!("{who}: di_blocks {} should be {}", n.blocks, count * 128),
            );
        }
        if n.mode & 0o170000 == 0o040000 {
            let mut bytes = Vec::new();
            for b in &data {
                bytes.extend(read_at(&mut f, b * 65536, 65536));
            }
            bytes.truncate(n.size as usize);
            dir_data.insert(ino, bytes);
        }
    }

    // Directories: records, ".", "..", types; walk from the root.
    let mut refs: HashMap<u64, u64> = HashMap::new();
    let mut parent_of: HashMap<u64, u64> = HashMap::from([(2, 2)]);
    let mut stack = vec![(2u64, String::new())];
    let mut seen = HashMap::new();
    while let Some((dir, path)) = stack.pop() {
        if seen.insert(dir, ()).is_some() {
            err(&mut r, format!("directory inode {dir} reached twice"));
            continue;
        }
        r.dirs.push(path.clone());
        let Some(bytes) = dir_data.get(&dir) else {
            err(&mut r, format!("{path:?}: not a directory"));
            continue;
        };
        if bytes.len() % 512 != 0 || bytes.is_empty() {
            err(
                &mut r,
                format!(
                    "{path:?}: directory size {} not a multiple of 512",
                    bytes.len()
                ),
            );
        }
        let mut off = 0usize;
        let mut k = 0;
        while off + 8 <= bytes.len() {
            let ino = u32_at(bytes, off);
            let reclen = u16_at(bytes, off + 4) as usize;
            let dtype = bytes[off + 6];
            let nlen = bytes[off + 7] as usize;
            let left = 512 - off % 512;
            if reclen == 0
                || reclen > left
                || !reclen.is_multiple_of(4)
                || reclen < (8 + nlen + 1 + 3) & !3
            {
                err(
                    &mut r,
                    format!("{path:?}: bad record at {off} (reclen {reclen})"),
                );
                break;
            }
            let name = &bytes[off + 8..off + 8 + nlen];
            if ino != 0 {
                if nlen == 0
                    || bytes[off + 8 + nlen] != 0
                    || name.contains(&b'/')
                    || name.contains(&0)
                {
                    err(&mut r, format!("{path:?}: bad name at {off}"));
                }
                let name = String::from_utf8_lossy(name).into_owned();
                *refs.entry(ino).or_default() += 1;
                match k {
                    0 if name != "." || ino != dir => {
                        err(&mut r, format!("{path:?}: first entry not '.'"))
                    }
                    1 if name != ".." || ino != parent_of[&dir] => {
                        err(&mut r, format!("{path:?}: second entry not '..' -> parent"))
                    }
                    0 | 1 => {}
                    _ => match inodes.get(&ino) {
                        None => err(&mut r, format!("{path:?}/{name}: unallocated inode {ino}")),
                        Some(n) => {
                            let is_dir = n.mode & 0o170000 == 0o040000;
                            if dtype != if is_dir { 4 } else { 8 } {
                                err(&mut r, format!("{path:?}/{name}: d_type {dtype} wrong"));
                            }
                            let child = if path.is_empty() {
                                name.clone()
                            } else {
                                format!("{path}/{name}")
                            };
                            if is_dir {
                                parent_of.insert(ino, dir);
                                stack.push((ino, child));
                            } else {
                                r.files.push((child, n.size));
                            }
                        }
                    },
                }
                k += 1;
            }
            off += reclen;
        }
    }
    for (&ino, n) in &inodes {
        let want = refs.get(&ino).copied().unwrap_or(0);
        if n.nlink != want {
            err(
                &mut r,
                format!("inode {ino}: nlink {} but {} references", n.nlink, want),
            );
        }
    }
    r.files.sort();
    r.dirs.sort();

    // Cylinder groups against the recomputed truth.
    let (mut t_ndir, mut t_nbfree, mut t_nifree) = (0u64, 0u64, 0u64);
    let csum = read_at(&mut f, csaddr * fsize, (ncg * 16) as usize);
    for (c, cg) in cgs.iter().enumerate() {
        let c = c as u64;
        let ndblk = cg_blocks(c);
        let iusedoff = 168;
        let freeoff = iusedoff + ipg.div_ceil(8);
        let nextfree = freeoff + fpg.div_ceil(8);
        let sumoff = nextfree.next_multiple_of(4) - 4;
        let clusteroff = sumoff + 17 * 4;
        let fields = [
            ("cgx", u32_at(cg, 12), c),
            ("ndblk", u32_at(cg, 20), ndblk),
            ("niblk", u32_at(cg, 116), ipg),
            ("iusedoff", u32_at(cg, 92), iusedoff),
            ("freeoff", u32_at(cg, 96), freeoff),
            ("clustersumoff", u32_at(cg, 104), sumoff),
            ("clusteroff", u32_at(cg, 108), clusteroff),
            ("nextfreeoff", u32_at(cg, 100), clusteroff + fpg.div_ceil(8)),
            ("nclusterblks", u32_at(cg, 112), ndblk),
            ("nffree", u32_at(cg, 36), 0),
        ];
        for (name, got, want) in fields {
            if got != want {
                err(&mut r, format!("cg {c}: {name} = {got}, want {want}"));
            }
        }
        let bit = |off: u64, i: u64| cg[(off + i / 8) as usize] >> (i % 8) & 1 == 1;
        let (mut ndir, mut nifree) = (0, 0);
        for i in 0..ipg {
            let ino = c * ipg + i;
            let alloc = ino < 2 || inodes.contains_key(&ino);
            if bit(iusedoff, i) != alloc {
                err(&mut r, format!("cg {c}: inode map wrong at {ino}"));
            }
            if !alloc {
                nifree += 1;
            } else if inodes
                .get(&ino)
                .is_some_and(|n| n.mode & 0o170000 == 0o040000)
            {
                ndir += 1;
            }
        }
        let mut nbfree = 0;
        let mut sums = [0u64; 17];
        let mut run = 0u64;
        for d in 0..fpg {
            let free = d < ndblk && used[(c * fpg + d) as usize] == 0;
            if bit(freeoff, d) != free || bit(clusteroff, d) != free {
                err(
                    &mut r,
                    format!("cg {c}: block map wrong at block {}", c * fpg + d),
                );
            }
            if free {
                nbfree += 1;
                run += 1;
            } else if run > 0 {
                sums[run.min(16) as usize] += 1;
                run = 0;
            }
        }
        if run > 0 {
            sums[run.min(16) as usize] += 1;
        }
        for (i, s) in sums.iter().enumerate().skip(1) {
            if u32_at(cg, (sumoff + 4 * i as u64) as usize) != *s {
                err(&mut r, format!("cg {c}: cluster sum [{i}] wrong"));
            }
        }
        let cs = [ndir, nbfree, nifree];
        let on_cg = [u32_at(cg, 24), u32_at(cg, 28), u32_at(cg, 32)];
        let at = (c * 16) as usize;
        let on_sum = [
            u32_at(&csum, at),
            u32_at(&csum, at + 4),
            u32_at(&csum, at + 8),
        ];
        if on_cg != cs || on_sum != cs || i32_at(&csum, at + 12) != 0 {
            err(
                &mut r,
                format!("cg {c}: summary {on_cg:?} / csum {on_sum:?}, want {cs:?}"),
            );
        }
        t_ndir += ndir;
        t_nbfree += nbfree;
        t_nifree += nifree;
    }
    let tot = [
        u64_at(&sb, 1008),
        u64_at(&sb, 1016),
        u64_at(&sb, 1024),
        u64_at(&sb, 1032),
        u64_at(&sb, 1040),
    ];
    if tot != [t_ndir, t_nbfree, t_nifree, 0, 0] {
        err(
            &mut r,
            format!(
                "cstotal {tot:?}, want {:?}",
                [t_ndir, t_nbfree, t_nifree, 0, 0]
            ),
        );
    }
    r
}

/// Walks an indirect tree of `level`, reporting each block (indirect or data) it owns.
fn walk_indirect(
    f: &mut File,
    p: u64,
    level: u32,
    lbn: &mut u64,
    nblocks: u64,
    on_block: &mut dyn FnMut(u64, bool),
    on_err: &mut dyn FnMut(String),
) {
    let span = 8192u64.pow(level);
    if *lbn >= nblocks {
        if p != 0 {
            on_err(format!("indirect pointer past size at lbn {}", *lbn));
        }
        *lbn += span;
        return;
    }
    if p == 0 {
        on_err(format!(
            "hole (missing level-{level} indirect) at lbn {}",
            *lbn
        ));
        *lbn += span;
        return;
    }
    on_block(p, false);
    let block = read_at(f, p * 65536, 65536);
    for i in 0..8192 {
        let q = u64_at(&block, i * 8);
        if level == 1 {
            if *lbn < nblocks {
                if q == 0 {
                    on_err(format!("hole at lbn {}", *lbn));
                } else {
                    on_block(q, true);
                }
            } else if q != 0 {
                on_err(format!("pointer past size at lbn {}", *lbn));
            }
            *lbn += 1;
        } else {
            walk_indirect(f, q, level - 1, lbn, nblocks, on_block, on_err);
        }
    }
}
