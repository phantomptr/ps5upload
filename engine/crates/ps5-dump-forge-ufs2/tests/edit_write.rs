//! The in-place editor's write side: plans applied through the undo journal, read back by
//! the vendored reader, the volume reopened (every count cross-checked) and the test fsck.

mod common;

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use common::{Synth, build, fsck};
use ps5_dump_forge_patch::{Data, Op, ReadSource, Syncs, Write};
use ps5_dump_forge_ufs2::Options;
use ps5_dump_forge_ufs2::edit::{Volume, plan};
use ps5upload_fpkg::source::SourceTree;

const B: u64 = 65536;

/// The files of the test game: nested folders, an empty file, one needing an indirect
/// block, and a folder of 1700 entries over two directory blocks.
fn files() -> Vec<(String, u64)> {
    let mut files: Vec<(String, u64)> = [
        ("eboot.bin", 3 * B + 17),
        ("sce_sys/param.json", 812),
        ("data/a/b/c.bin", 5),
        ("data/a/d.bin", 2 * B),
        ("data/empty.bin", 0),
        ("data/big.bin", 40 * B + 3),
        ("data/edge.bin", 12 * B + 1),
    ]
    .iter()
    .map(|(p, s)| (p.to_string(), *s))
    .collect();
    files.extend((0..1700).map(|i| (format!("many/file-{i:05}-padding-padding-more"), i % 3)));
    files
}

fn synth() -> Synth {
    synth_of(&files())
}

fn synth_of(files: &[(String, u64)]) -> Synth {
    let refs: Vec<(&str, u64)> = files.iter().map(|(p, s)| (p.as_str(), *s)).collect();
    let mut src = Synth::new(&refs);
    src.empty = vec!["data/void".into(), "z/y".into()];
    src
}

fn image(name: &str) -> PathBuf {
    build(name, &mut synth(), &Options::default()).0
}

fn open(path: &Path) -> Volume {
    Volume::open(&mut File::open(path).unwrap())
        .unwrap()
        .unwrap()
}

fn rw(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap()
}

/// `fs_clean`'s byte in each superblock copy: the primary, then one per group.
fn clean_bytes(v: &Volume) -> Vec<u64> {
    let g = v.blocks_per_group();
    let backups = (0..v.cylinder_groups()).map(|c| (c * g + 2) * B + 209);
    std::iter::once(B + 209).chain(backups).collect()
}

/// Plans `ops`, checks the fs_clean bracket, applies the writes through the journal and
/// commits.
fn edit(img: &Path, ops: &[Op]) {
    edit_from(img, ops, &mut |_, _, _| unreachable!())
}

/// [`edit`] with `source` for `Data::Source` bytes. Free-space writes (new data) come first,
/// overlapping nothing else in the plan.
fn edit_from(img: &Path, ops: &[Op], source: ReadSource) {
    let mut f = rw(img);
    let all = plan(&mut f, ops).unwrap();
    let k = all.iter().take_while(|w| !w.live).count();
    let span = |w: &Write| (w.offset, w.offset + w.data.len());
    for (i, w) in all[..k].iter().enumerate() {
        let (lo, hi) = span(w);
        let hit = all
            .iter()
            .enumerate()
            .find(|&(j, o)| j != i && span(o).0 < hi && lo < span(o).1);
        assert!(hit.is_none(), "free-space write {i} overlaps {hit:?}");
    }
    let writes = &all[k..];
    let clean = clean_bytes(&open(img));
    let byte = |w: &Write| match &w.data {
        Data::Bytes(b) if b.len() == 1 => Some((w.offset, b[0])),
        _ => None,
    };
    assert!(
        writes.iter().all(|w| w.live),
        "a free-space write after the first live one"
    );
    let n = clean.len();
    let first: Vec<_> = writes[..n].iter().map(byte).collect();
    assert_eq!(
        first,
        clean.iter().map(|&at| Some((at, 0))).collect::<Vec<_>>()
    );
    // The last write of each copy sets it again, after its fs_cstotal.
    let last: Vec<_> = writes[writes.len() - 2 * n..]
        .iter()
        .skip(1)
        .step_by(2)
        .map(byte)
        .collect();
    assert_eq!(
        last,
        clean.iter().map(|&at| Some((at, 1))).collect::<Vec<_>>()
    );
    let cancel = AtomicBool::new(false);
    ps5_dump_forge_patch::apply(&mut f, img, &all, source, &cancel, Syncs::STD)
        .unwrap()
        .commit(Syncs::STD)
        .unwrap();
}

fn delete(paths: &[&str]) -> Vec<Op> {
    paths
        .iter()
        .map(|p| Op::Delete {
            path: p.to_string(),
        })
        .collect()
}

/// The image reads back as the source `all` without the files under `gone`, byte for byte,
/// and the empty folders `empty`; the test fsck finds nothing.
fn reads_back_of(img: &Path, all: &[(String, u64)], gone: &[&str], empty: &[&str]) {
    let under = |p: &str| {
        gone.iter()
            .any(|g| p == *g || p.starts_with(&format!("{g}/")))
    };
    let mut want = synth_of(all);
    let mut reader = ps5upload_fpkg::ufs2_source::Ufs2Source::open(img).unwrap();
    let mut keep: Vec<(String, u64)> = all.iter().filter(|&(p, _)| !under(p)).cloned().collect();
    keep.sort();
    let got: Vec<(String, u64)> = reader
        .files()
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    assert_eq!(got, keep);
    for (p, size) in &keep {
        let n = *size as usize;
        assert_eq!(
            reader.read_range(p, 0, n).unwrap(),
            want.read_range(p, 0, n).unwrap(),
            "{p}"
        );
    }
    let mut dirs: Vec<String> = reader.empty_dirs().to_vec();
    dirs.sort();
    assert_eq!(dirs, empty);
    let r = fsck(img);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
}

/// Deleting `gone` applied, then rolled back, leaves `img` byte-identical.
fn rolls_back(img: &Path, gone: &[&str]) {
    use std::io::Read;
    let img = img.to_path_buf();
    let copy = img.with_extension("orig");
    std::fs::copy(&img, &copy).unwrap();
    let mut f = rw(&img);
    let writes = plan(&mut f, &delete(gone)).unwrap();
    let cancel = AtomicBool::new(false);
    let mut source = |_: u32, _: u64, _: &mut [u8]| -> std::io::Result<()> { unreachable!() };
    let applied =
        ps5_dump_forge_patch::apply(&mut f, &img, &writes, &mut source, &cancel, Syncs::STD)
            .unwrap();
    assert!(
        open(&img)
            .lookup(&mut File::open(&img).unwrap(), gone[0])
            .unwrap()
            .is_none()
    );
    applied.rollback(Syncs::STD).unwrap();
    let (mut a, mut b) = (File::open(&img).unwrap(), File::open(&copy).unwrap());
    let (mut x, mut y) = (vec![0u8; 8 << 20], vec![0u8; 8 << 20]);
    loop {
        let n = a.read(&mut x).unwrap();
        b.read_exact(&mut y[..n]).unwrap();
        assert!(x[..n] == y[..n], "image differs after rollback");
        if n == 0 {
            break;
        }
    }
    std::fs::remove_file(&copy).unwrap();
}

/// The test game plus a 100 MiB file and a folder of 5000 one-block files: with no extra
/// free space the writer lays it out in 4 groups of ~2200 blocks and ~2300 inodes, so its
/// blocks and inodes reach every group.
fn bulk_files() -> Vec<(String, u64)> {
    let mut f = files();
    f.push(("bulk/big.bin".into(), 100 << 20));
    f.extend((0..5000).map(|i| (format!("bulk/n/f-{i:05}"), 1)));
    f
}

/// Per group, the (folders, blocks, inodes) everything at and under `paths` holds.
fn held_per_group(v: &Volume, f: &mut File, paths: &[&str]) -> Vec<[u64; 3]> {
    let mut out = vec![[0u64; 3]; v.cylinder_groups() as usize];
    let mut stack: Vec<_> = paths
        .iter()
        .map(|p| (p.to_string(), v.lookup(f, p).unwrap().unwrap()))
        .collect();
    while let Some((p, n)) = stack.pop() {
        let inode = v.inode(f, n.ino).unwrap();
        let map = v.block_map(f, &inode).unwrap();
        let blocks = map.data.iter().flat_map(|r| r.block..r.block + r.len);
        for b in blocks.chain(map.indirect.iter().copied()) {
            out[(b / v.blocks_per_group()) as usize][1] += 1;
        }
        let c = (n.ino / v.inodes_per_group()) as usize;
        out[c][2] += 1;
        if n.is_dir() {
            out[c][0] += 1;
            stack.extend(
                v.list(f, &p)
                    .unwrap()
                    .into_iter()
                    .map(|c| (format!("{p}/{}", c.name), c)),
            );
        }
    }
    out
}

/// `fs_cstotal` and `fs_clean` of the superblock copy at byte `sb`.
fn sb_counts(f: &mut File, sb: u64) -> (Vec<u8>, u8) {
    use std::io::{Read, Seek, SeekFrom};
    let mut b = vec![0u8; 32];
    f.seek(SeekFrom::Start(sb + 1008)).unwrap();
    f.read_exact(&mut b).unwrap();
    let mut clean = [0u8];
    f.seek(SeekFrom::Start(sb + 209)).unwrap();
    f.read_exact(&mut clean).unwrap();
    (b, clean[0])
}

/// Deletes reaching every cylinder group: each group's `cg_cs` (and so its `fs_cs` entry,
/// which open checks against it) gains exactly what the deleted inodes held there, every
/// backup superblock carries the primary's `fs_cstotal` and is clean, the rest reads back;
/// a rollback first restores every byte. `FORGE_UFS2_KEEP=<dir>` keeps the image.
#[test]
fn delete_across_cylinder_groups() {
    let all = bulk_files();
    let mut src = synth_of(&all);
    src.empty = vec!["data/void".into(), "z/y".into()];
    let img = build("edit-delete-groups", &mut src, &Options::default()).0;
    let gone = ["bulk/n", "many", "data/edge.bin"];
    rolls_back(&img, &gone);
    let before = open(&img);
    let ncg = before.cylinder_groups() as usize;
    assert!(ncg >= 4, "{ncg} groups");
    let held = held_per_group(&before, &mut File::open(&img).unwrap(), &gone);
    for k in 1..3 {
        let groups = (1..ncg).filter(|&c| held[c][k] > 0).count();
        assert!(
            groups >= 2,
            "field {k} reaches {groups} groups past group 0: {held:?}"
        );
    }
    edit(&img, &delete(&gone));
    reads_back_of(&img, &all, &gone, &["data/void", "z/y"]);
    let after = open(&img);
    for (c, h) in held.iter().enumerate() {
        let (b, a) = (before.groups[c].cs, after.groups[c].cs);
        assert_eq!(
            [b.ndir - h[0], b.nbfree + h[1], b.nifree + h[2]],
            [a.ndir, a.nbfree, a.nifree],
            "group {c}"
        );
        assert_eq!(after.summary[c], a, "fs_cs[{c}]");
    }
    let mut f = File::open(&img).unwrap();
    let primary = sb_counts(&mut f, B);
    assert_eq!(primary.1, 1);
    let t = after.total;
    let want: Vec<u8> = [t.ndir, t.nbfree, t.nifree, 0]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    assert_eq!(primary.0, want);
    for c in 0..ncg as u64 {
        let sb = (c * after.blocks_per_group() + 2) * B;
        assert_eq!(sb_counts(&mut f, sb), primary, "backup superblock {c}");
    }
    if let Some(keep) = std::env::var_os("FORGE_UFS2_KEEP") {
        std::fs::copy(&img, Path::new(&keep).join("edit-delete-groups.ffpkg")).unwrap();
    }
    let _ = std::fs::remove_file(&img);
}

fn rename(pairs: &[(&str, &str)]) -> Vec<Op> {
    pairs
        .iter()
        .map(|(from, to)| Op::Rename {
            from: from.to_string(),
            to: to.to_string(),
        })
        .collect()
}

/// Where `p` ends up after `renames`, in order (a folder's carries what it holds).
fn moved(p: &str, renames: &[(&str, &str)]) -> String {
    let mut p = p.to_string();
    for (from, to) in renames {
        if p == *from {
            p = to.to_string();
        } else if let Some(rest) = p.strip_prefix(&format!("{from}/")) {
            p = format!("{to}/{rest}");
        }
    }
    p
}

/// The image reads back as the source with `renames` applied, byte for byte, and the empty
/// folders `empty`; the test fsck finds nothing.
fn reads_back_renamed(img: &Path, renames: &[(&str, &str)], empty: &[&str]) {
    let all = files();
    let mut want = synth();
    let mut reader = ps5upload_fpkg::ufs2_source::Ufs2Source::open(img).unwrap();
    let mut keep: Vec<(String, u64)> = all.iter().map(|(p, s)| (moved(p, renames), *s)).collect();
    keep.sort();
    let mut got: Vec<(String, u64)> = reader
        .files()
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    got.sort();
    assert_eq!(got, keep);
    for (p, size) in &all {
        let n = *size as usize;
        assert_eq!(
            reader.read_range(&moved(p, renames), 0, n).unwrap(),
            want.read_range(p, 0, n).unwrap(),
            "{p}"
        );
    }
    let mut dirs: Vec<String> = reader.empty_dirs().to_vec();
    dirs.sort();
    assert_eq!(dirs, empty);
    let r = fsck(img);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
}

/// Renames in place (shorter, same length, another case), into a new entry (longer), files
/// and folders moved between folders (`..` and link counts following), and an op on what an
/// earlier one moved, in one plan: everything reads back under its new path, the free counts
/// stay. With `FORGE_UFS2_KEEP=<dir>` the edited image is copied there.
#[test]
fn rename_and_move() {
    let img = image("edit-rename");
    let before = open(&img);
    let mut f = File::open(&img).unwrap();
    let nlink = |v: &Volume, f: &mut File, p: &str| match p {
        "" => v.root(f).unwrap().nlink,
        _ => v.lookup(f, p).unwrap().unwrap().nlink,
    };
    let (root, data, z) = (
        nlink(&before, &mut f, ""),
        nlink(&before, &mut f, "data"),
        nlink(&before, &mut f, "z"),
    );
    let first = before.list(&mut f, "many").unwrap()[0].clone();
    assert_eq!(first.name, "file-00000-padding-padding-more");
    let renames = [
        ("many/file-00000-padding-padding-more", "many/f0"),
        ("data/big.bin", "data/BIG.bin"),
        (
            "data/edge.bin",
            "data/edge-with-a-name-long-enough-for-a-new-entry.bin",
        ),
        ("data/a", "z/a"),
        ("data/empty.bin", "empty.bin"),
        ("data/void", "void"),
        ("z/a/b", "b"),
        ("z/y", "z/y2"),
        ("eboot.bin", "eboot.bin"),
    ];
    edit(&img, &rename(&renames));
    reads_back_renamed(&img, &renames, &["void", "z/y2"]);
    let after = open(&img);
    assert_eq!(after.free_bytes(), before.free_bytes());
    assert_eq!(after.free_inodes(), before.free_inodes());
    assert_eq!(after.total.ndir, before.total.ndir);
    // The root gained `void` and `b`, `data` lost `a` and `void`, `z` gained `a`, `z/a` lost `b`.
    let mut f = File::open(&img).unwrap();
    assert_eq!(nlink(&after, &mut f, ""), root + 2);
    assert_eq!(nlink(&after, &mut f, "data"), data - 2);
    assert_eq!(nlink(&after, &mut f, "z"), z + 1);
    assert_eq!(nlink(&after, &mut f, "z/a"), 2);
    // Each moved folder's `..` names its new parent (listing checks it).
    for dir in ["z/a", "b", "void", "z/y2"] {
        after.list(&mut f, dir).unwrap();
    }
    // The shorter name stayed in its entry.
    let f0 = after.list(&mut f, "many").unwrap()[0].clone();
    assert_eq!(
        (f0.name.as_str(), f0.offset, f0.reclen),
        ("f0", first.offset, first.reclen)
    );
    if let Some(keep) = std::env::var_os("FORGE_UFS2_KEEP") {
        std::fs::copy(&img, Path::new(&keep).join("edit-rename.ffpkg")).unwrap();
    }
}

fn put(path: &str, data: Data) -> Op {
    Op::Put {
        path: path.into(),
        len: data.len(),
        data,
    }
}

/// Source `id`'s bytes, for `Data::Source`.
fn from(id: u32, offset: u64, len: u64) -> Data {
    Data::Source { id, offset, len }
}

fn source_bytes(id: u32, offset: u64, len: u64) -> Vec<u8> {
    common::pattern(1000 + id as usize, offset, len as usize)
}

/// Applies `ops` with sources read from [`source_bytes`].
fn edit_put(img: &Path, ops: &[Op]) {
    let mut source = |id: u32, off: u64, buf: &mut [u8]| -> std::io::Result<()> {
        buf.copy_from_slice(&source_bytes(id, off, buf.len() as u64));
        Ok(())
    };
    edit_from(img, ops, &mut source)
}

/// The image reads back as the source without `gone`, the files of `put` holding their bytes
/// (added or replaced), and the empty folders `empty`; the test fsck finds nothing.
fn reads_back_put(img: &Path, gone: &[&str], put: &[(&str, Vec<u8>)], empty: &[&str]) {
    let under = |p: &str| {
        gone.iter()
            .any(|g| p == *g || p.starts_with(&format!("{g}/")))
    };
    let mut want = synth();
    let mut reader = ps5upload_fpkg::ufs2_source::Ufs2Source::open(img).unwrap();
    let mut keep: Vec<(String, u64)> = files()
        .into_iter()
        .filter(|(p, _)| !under(p) && !put.iter().any(|(q, _)| q == p))
        .collect();
    let mut all = keep.clone();
    all.extend(put.iter().map(|(p, b)| (p.to_string(), b.len() as u64)));
    all.sort();
    let got: Vec<(String, u64)> = reader
        .files()
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    assert_eq!(got, all);
    keep.sort();
    for (p, size) in &keep {
        let n = *size as usize;
        assert_eq!(
            reader.read_range(p, 0, n).unwrap(),
            want.read_range(p, 0, n).unwrap(),
            "{p}"
        );
    }
    for (p, bytes) in put {
        assert!(
            reader.read_range(p, 0, bytes.len()).unwrap() == *bytes,
            "{p}"
        );
    }
    let mut dirs: Vec<String> = reader.empty_dirs().to_vec();
    dirs.sort();
    assert_eq!(dirs, empty);
    let r = fsck(img);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
}

/// The blocks a file of `n` data blocks owns, laid out as the writer lays it out.
fn slots(n: u64) -> u64 {
    n + u64::from(n > 12)
}

/// New files (one needing an indirect block, an empty one, one of zeros) and replaced files
/// (bigger, smaller) in one plan: everything reads back, a replaced file keeps its inode and
/// times, a new file is one run with its indirect block where the writer puts it, and the
/// counts follow. With `FORGE_UFS2_KEEP=<dir>` the image is kept as `edit-put.ffpkg`.
#[test]
fn put_new_and_replaced() {
    let img = image("edit-put");
    let before = open(&img);
    let mut f = rw(&img);
    let ino = |v: &Volume, f: &mut File, p: &str| v.lookup(f, p).unwrap().unwrap().ino;
    let times = |v: &Volume, f: &mut File, p: &str| {
        let at = v.inode_offset(ino(v, f, p)) + 32;
        let mut b = [0u8; 32];
        std::io::Seek::seek(f, std::io::SeekFrom::Start(at)).unwrap();
        std::io::Read::read_exact(f, &mut b).unwrap();
        b
    };
    let (eboot, edge) = (
        ino(&before, &mut f, "eboot.bin"),
        ino(&before, &mut f, "data/edge.bin"),
    );
    let eboot_times = times(&before, &mut f, "eboot.bin");
    let small = b"replaced, smaller".to_vec();
    let ops = [
        put("data/new.bin", from(1, 7, 3 * B + 5)),
        put("big-new.bin", from(2, 0, 30 * B + 9)),
        put("data/edge.bin", Data::Bytes(small.clone())),
        put("eboot.bin", from(3, 0, 20 * B)),
        put("data/a/empty-new", Data::Zero(0)),
        put("data/zeros.bin", Data::Zero(2 * B + 3)),
    ];
    edit_put(&img, &ops);
    let put_back = [
        ("data/new.bin", source_bytes(1, 7, 3 * B + 5)),
        ("big-new.bin", source_bytes(2, 0, 30 * B + 9)),
        ("data/edge.bin", small),
        ("eboot.bin", source_bytes(3, 0, 20 * B)),
        ("data/a/empty-new", Vec::new()),
        ("data/zeros.bin", vec![0; (2 * B + 3) as usize]),
    ];
    reads_back_put(&img, &[], &put_back, &["data/void", "z/y"]);
    let after = open(&img);
    let mut f = rw(&img);
    assert_eq!(ino(&after, &mut f, "eboot.bin"), eboot);
    assert_eq!(ino(&after, &mut f, "data/edge.bin"), edge);
    assert_eq!(times(&after, &mut f, "eboot.bin"), eboot_times);
    // New: 4 + 32 + 0 + 3 blocks; replaced: eboot 4 -> 21, edge 14 -> 1.
    let new = 4 + slots(31) + 3 + slots(20) + 1;
    let old = slots(4) + slots(13);
    assert_eq!(after.free_bytes(), before.free_bytes() + old * B - new * B);
    assert_eq!(after.free_inodes(), before.free_inodes() - 4);
    let node = after.lookup(&mut f, "big-new.bin").unwrap().unwrap();
    let inode = after.inode(&mut f, node.ino).unwrap();
    let map = after.block_map(&mut f, &inode).unwrap();
    let p = map.data[0].block;
    assert_eq!(map.indirect, [p + 12]);
    let runs: Vec<_> = map.data.iter().map(|r| (r.lbn, r.block, r.len)).collect();
    assert_eq!(runs, [(0, p, 12), (12, p + 13, 19)]);
    assert_eq!(inode.mode, 0o100777);
    if let Some(keep) = std::env::var_os("FORGE_UFS2_KEEP") {
        std::fs::copy(&img, Path::new(&keep).join("edit-put.ffpkg")).unwrap();
    }
}

/// Short of room: one finding, the need counting every block the plan takes or lacks, the
/// free space as the image has it (blocks this plan frees are not reused); findings before
/// the shortfall stand, later ones are dropped.
#[test]
fn no_room_is_one_finding() {
    let img = image("edit-put-no-room");
    let mut f = rw(&img);
    let free = open(&img).free_bytes() / B;
    let huge = free * B;
    let ops = [
        Op::Delete {
            path: "missing".into(),
        },
        put("a.bin", Data::Zero(B)),
        Op::Delete {
            path: "data/big.bin".into(),
        },
        put("huge.bin", Data::Zero(huge)),
        Op::Delete {
            path: "missing-too".into(),
        },
        put("nope/x", Data::Zero(1)),
        put("b.bin", Data::Zero(1)),
    ];
    // `nope/x` makes its folder (a block) and the file (a block): both counted.
    let need = (1 + (free + 1) + 2 + 1) * B;
    assert_eq!(
        plan(&mut f, &ops).unwrap_err(),
        [
            "missing: not found".to_string(),
            format!(
                "the changes need {} MB, the image has {} MB free",
                need.div_ceil(1_000_000),
                free * B / 1_000_000
            ),
        ]
    );
    // Exactly the free blocks fits.
    let ops = [put("huge.bin", Data::Zero(huge - B))];
    assert!(plan(&mut f, &ops).is_ok());
}

/// Raw inode bytes `at..at + len` of the node at `path`.
fn inode_bytes(v: &Volume, f: &mut File, path: &str, at: u64, len: usize) -> Vec<u8> {
    let ino = v.lookup(f, path).unwrap().unwrap().ino;
    let mut b = vec![0u8; len];
    std::io::Seek::seek(f, std::io::SeekFrom::Start(v.inode_offset(ino) + at)).unwrap();
    std::io::Read::read_exact(f, &mut b).unwrap();
    b
}

/// Mkdir (nested, into an empty folder, of an existing folder: a no-op) and Puts making
/// their missing folders: each new folder is the writer's (mode 040777, nlink 2, one block,
/// `di_dirdepth` one past its parent's), parents gain a link; findings for a file on the
/// way and a bad name.
#[test]
fn mkdir_and_missing_parents() {
    let img = image("edit-mkdir");
    let before = open(&img);
    let mut f = rw(&img);
    let bad = [
        Op::Mkdir {
            path: "eboot.bin".into(),
        },
        Op::Mkdir {
            path: "data/a/b/c.bin/x".into(),
        },
        Op::Mkdir {
            path: "q/..".into(),
        },
        Op::Mkdir {
            path: "a//b".into(),
        },
        put("data/empty.bin/x/y", Data::Zero(1)),
    ];
    assert_eq!(
        plan(&mut f, &bad).unwrap_err(),
        [
            "eboot.bin is a file",
            "data/a/b/c.bin is a file",
            "q/..: '.' or '..' component",
            "\"a//b\": bad path",
            "data/empty.bin is a file",
        ]
    );
    let root_links = before.lookup(&mut f, "data").unwrap().unwrap().nlink;
    let a_links = before.lookup(&mut f, "data/a").unwrap().unwrap().nlink;
    let mkdir = |p: &str| Op::Mkdir { path: p.into() };
    let ops = [
        mkdir("data"),
        mkdir("n1/n2/n3"),
        mkdir("n1/n2"),
        put("p/q/r.bin", from(1, 0, 3 * B)),
        put("n1/n2/n3/f.bin", Data::Zero(5)),
        mkdir("data/a/new"),
        mkdir("z/y/w"),
    ];
    edit_put(&img, &ops);
    let put_back = [
        ("p/q/r.bin", source_bytes(1, 0, 3 * B)),
        ("n1/n2/n3/f.bin", vec![0; 5]),
    ];
    reads_back_put(&img, &[], &put_back, &["data/a/new", "data/void", "z/y/w"]);
    let after = open(&img);
    let mut f = rw(&img);
    // 7 folders of one block, r.bin 3 blocks, f.bin 1.
    assert_eq!(after.free_bytes(), before.free_bytes() - 11 * B);
    assert_eq!(after.free_inodes(), before.free_inodes() - 9);
    let links = |p: &str| after.lookup(&mut rw(&img), p).unwrap().unwrap().nlink;
    assert_eq!(links("data"), root_links);
    assert_eq!(links("data/a"), a_links + 1);
    assert_eq!(links("z/y"), 3);
    assert_eq!((links("n1"), links("n1/n2"), links("n1/n2/n3")), (3, 3, 2));
    let n3 = after.lookup(&mut f, "n1/n2/n3").unwrap().unwrap();
    assert_eq!(n3.size, 512);
    let inode = after.inode(&mut f, n3.ino).unwrap();
    assert_eq!(inode.mode, 0o040777);
    assert_eq!(after.block_map(&mut f, &inode).unwrap().owned(), 1);
    let depth = |p: &str| {
        let b = inode_bytes(&after, &mut rw(&img), p, 240, 4);
        u32::from_le_bytes(b.try_into().unwrap())
    };
    assert_eq!(
        (depth("n1"), depth("n1/n2/n3"), depth("data/a/new")),
        (1, 3, 3)
    );
    assert_eq!(depth("data/a"), 2);
    let root = after.list(&mut f, "").unwrap();
    assert!(root.iter().any(|n| n.name == "n1" && n.is_dir()));
}

/// A block the plan took and then freed is never taken again: repeated Puts of one path, and
/// a Delete, Put, Delete, Put, each give free-space writes that overlap nothing (else the
/// journal refuses the job), and the counts follow.
#[test]
fn a_block_taken_and_freed_is_not_taken_again() {
    let img = image("edit-retake");
    let before = open(&img);
    // A Put takes its blocks, then frees the old ones: the fourth Put of a file, or the
    // fifth of a new one, finds free the blocks of the third.
    let mut ops: Vec<Op> = (1..=4)
        .map(|i| put("data/edge.bin", from(i, 0, 5 * B)))
        .collect();
    ops.extend((11..=15).map(|i| put("f.bin", from(i, 0, 5 * B))));
    edit_put(&img, &ops);
    reads_back_put(
        &img,
        &[],
        &[
            ("data/edge.bin", source_bytes(4, 0, 5 * B)),
            ("f.bin", source_bytes(15, 0, 5 * B)),
        ],
        &["data/void", "z/y"],
    );
    assert_eq!(
        open(&img).free_bytes(),
        before.free_bytes() + slots(13) * B - 10 * B
    );

    let img = image("edit-retake-delete");
    let before = open(&img);
    let ops = [
        Op::Delete {
            path: "data/big.bin".into(),
        },
        put("a.bin", from(4, 0, 20 * B)),
        Op::Delete {
            path: "a.bin".into(),
        },
        put("b.bin", from(5, 0, 15 * B)),
    ];
    edit_put(&img, &ops);
    reads_back_put(
        &img,
        &["data/big.bin"],
        &[("b.bin", source_bytes(5, 0, 15 * B))],
        &["data/void", "z/y"],
    );
    let big = slots(41);
    assert_eq!(
        open(&img).free_bytes(),
        before.free_bytes() + big * B - slots(15) * B
    );
}
