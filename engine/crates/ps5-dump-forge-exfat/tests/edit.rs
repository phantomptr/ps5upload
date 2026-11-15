//! The in-place editor's volume reader against Forge-built images.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use ps5_dump_forge_exfat::edit::{Volume, plan};
use ps5_dump_forge_exfat::{Layout, Options};
use ps5_dump_forge_patch::{Data, Op, Syncs};
use ps5upload_fpkg::exfat::ExFatSource;
use ps5upload_fpkg::source::{FolderSource, SourceTree};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "ps5-dump-forge-exfat-edit-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn bytes(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31) ^ seed)
        .collect()
}

const LONG: &str = "a-file-name-longer-than-fifteen-units.bin";

/// A small game tree in `<tmp>/src`, built into `<tmp>/game.exfat`.
fn build(tmp: &TempDir, opts: &Options) -> (PathBuf, Layout) {
    build_with(tmp, opts, &[])
}

/// [`build`] with `extra` empty files in the tree.
fn build_with(tmp: &TempDir, opts: &Options, extra: &[String]) -> (PathBuf, Layout) {
    let src = tmp.0.join("src");
    for rel in extra {
        let p = src.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"").unwrap();
    }
    let files: [(&str, usize); 5] = [
        ("eboot.bin", 100_000),
        ("sce_sys/param.json", 300),
        ("data/big.bin", 200_000),
        ("data/sub/x.txt", 0),
        ("data/Ünïcode.txt", 5),
    ];
    for (i, (rel, len)) in files.iter().enumerate() {
        let p = src.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes(i as u8, *len)).unwrap();
    }
    std::fs::write(src.join(LONG), b"long").unwrap();
    std::fs::create_dir_all(src.join("empty")).unwrap();
    let image = tmp.0.join("game.exfat");
    let mut tree = FolderSource::open(&src).unwrap();
    let cancel = AtomicBool::new(false);
    let layout = ps5_dump_forge_exfat::plan(&tree, opts, &cancel).unwrap();
    let mut out = File::create(&image).unwrap();
    ps5_dump_forge_exfat::write(&mut tree, &layout, &mut out, &cancel, &mut |_, _| {}).unwrap();
    (image, layout)
}

fn open(image: &Path) -> (File, Volume) {
    let mut f = File::open(image).unwrap();
    let v = Volume::open(&mut f).unwrap();
    (f, v)
}

fn read_byte(f: &mut File, at: u64) -> u8 {
    use std::io::{Read, Seek, SeekFrom};
    let mut b = [0u8];
    f.seek(SeekFrom::Start(at)).unwrap();
    f.read_exact(&mut b).unwrap();
    b[0]
}

#[test]
fn corrupt_boot_checksum_is_an_error() {
    use std::io::{Seek, SeekFrom, Write};
    let tmp = TempDir::new("checksum");
    let (image, _) = build(&tmp, &Options::default());
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(&image)
        .unwrap();
    f.seek(SeekFrom::Start(100)).unwrap(); // VolumeSerialNumber, under the checksum
    f.write_all(&[0x5A]).unwrap();
    drop(f);
    let err = Volume::open(&mut File::open(&image).unwrap()).unwrap_err();
    assert!(err.to_string().contains("boot checksum"), "{err}");
}

/// Byte `at` of test source `id`: `bytes(id, n)` is its first `n` bytes.
fn src_byte(id: u32, at: u64) -> u8 {
    (at as u8).wrapping_mul(31) ^ id as u8
}

/// Plans `ops`, applies and commits them; the image's free bytes before and after.
fn edit(image: &Path, ops: &[Op]) -> (u64, u64) {
    let before = open(image).1.free_bytes();
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(image)
        .unwrap();
    let writes = plan(&mut f, ops).unwrap();
    // Free-space writes (new directory clusters) come first, then the live ones, bracketed
    // by the boot sector: VolumeDirty set first, cleared last.
    let free = writes.iter().take_while(|w| !w.live).count();
    let writes = &writes[free..];
    assert!(writes.iter().all(|w| w.live));
    let flags = |w: &ps5_dump_forge_patch::Write| match &w.data {
        Data::Bytes(b) if w.offset == 0 && b.len() == 512 => b[106] & 0x2,
        _ => panic!("not a boot sector write: {w:?}"),
    };
    assert_eq!(flags(&writes[0]), 0x2);
    assert_eq!(flags(writes.last().unwrap()), 0);
    let cancel = AtomicBool::new(false);
    let mut source = |id: u32, at: u64, buf: &mut [u8]| -> std::io::Result<()> {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = src_byte(id, at + i as u64);
        }
        Ok(())
    };
    let writes = plan(&mut f, ops).unwrap();
    ps5_dump_forge_patch::apply(&mut f, image, &writes, &mut source, &cancel, Syncs::STD)
        .unwrap()
        .commit(Syncs::STD)
        .unwrap();
    drop(f);
    (before, open(image).1.free_bytes())
}

/// Every file of the image with its bytes, read back by ps5upload's reader.
fn read_back(image: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut src = ExFatSource::open(image).unwrap();
    let paths: Vec<String> = src.files().iter().map(|f| f.path.clone()).collect();
    paths
        .into_iter()
        .map(|p| {
            let b = src.read(&p).unwrap();
            (p, b)
        })
        .collect()
}

fn write_at(image: &Path, at: u64, data: &[u8]) {
    use std::io::{Seek, SeekFrom, Write};
    let mut f = std::fs::OpenOptions::new().write(true).open(image).unwrap();
    f.seek(SeekFrom::Start(at)).unwrap();
    f.write_all(data).unwrap();
}

/// Two free clusters, not adjacent, from the end of the heap.
fn two_free(v: &Volume) -> (u32, u32) {
    let last = v.cluster_count + 1;
    let (a, b) = (last, last - 2);
    assert!(!v.in_use(a) && !v.in_use(b));
    (a, b)
}

#[test]
fn fat_chains_fragmented_short_long_and_cyclic() {
    use ps5_dump_forge_exfat::edit::Stream;
    let tmp = TempDir::new("chains");
    let (image, _) = build(&tmp, &Options::default());
    let (_, mut v) = open(&image);
    let cs = v.cluster_size();
    let (a, b) = two_free(&v);
    write_at(&image, v.cluster_offset(a), &[0xAA; 65536]);
    write_at(&image, v.cluster_offset(b), &[0xBB; 65536]);
    v.fat[a as usize] = b;
    v.fat[b as usize] = u32::MAX;
    let s = |clusters: u64| Stream {
        first_cluster: a,
        len: clusters * cs - 10,
        no_fat_chain: false,
    };
    assert_eq!(v.chain(s(2)).unwrap(), [a, b]);
    // The list is reserved at its exact bound, never regrown past it.
    assert_eq!(v.chain(s(2)).unwrap().capacity(), 2);
    let data = v
        .read_stream(&mut File::open(&image).unwrap(), s(2))
        .unwrap();
    assert!(data[..65536].iter().all(|&x| x == 0xAA) && data[65536..].iter().all(|&x| x == 0xBB));
    assert!(v.chain(s(1)).is_err(), "chain longer than the stream");
    assert!(v.chain(s(3)).is_err(), "chain ends early");
    v.fat[b as usize] = a;
    assert!(v.chain(s(3)).is_err(), "loop a -> b -> a");
    v.fat[a as usize] = a;
    assert!(v.chain(s(2)).is_err(), "FAT[a] = a");
    v.fat[a as usize] = 0;
    assert!(v.chain(s(2)).is_err(), "free entry before the end");

    // On disk: the root directory's chain pointing at itself.
    let root_slot = v.fat_offset + u64::from(v.root_cluster) * 4;
    write_at(&image, root_slot, &v.root_cluster.to_le_bytes());
    let err = Volume::open(&mut File::open(&image).unwrap()).unwrap_err();
    assert!(err.to_string().contains("loops"), "{err}");
}

#[test]
fn dirty_image_is_refused_by_plan() {
    let tmp = TempDir::new("dirty");
    let (image, _) = build(&tmp, &Options::default());
    write_at(&image, 106, &[0x02]); // VolumeFlags.VolumeDirty, outside the boot checksum
    let mut f = File::open(&image).unwrap();
    assert!(Volume::open(&mut f).unwrap().dirty);
    let findings = plan(&mut f, &[]).unwrap_err();
    assert_eq!(findings.len(), 1);
    assert!(
        findings[0].contains("VolumeDirty") && findings[0].contains("rebuild"),
        "{findings:?}"
    );
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

#[test]
fn huge_allocation_tables_are_refused() {
    // 1 TiB of 4 KiB clusters: a 1 GiB FAT. The file holds only the boot region.
    let cc: u32 = 1 << 28;
    let fat_sectors = ((u64::from(cc) + 2) * 4).div_ceil(512) as u32;
    let heap = 128 + fat_sectors;
    let mut r = vec![0u8; 12 * 512];
    r[3..11].copy_from_slice(b"EXFAT   ");
    r[72..80].copy_from_slice(&(u64::from(heap) + u64::from(cc) * 8).to_le_bytes());
    put32(&mut r, 80, 128);
    put32(&mut r, 84, fat_sectors);
    put32(&mut r, 88, heap);
    put32(&mut r, 92, cc);
    put32(&mut r, 96, 2);
    (r[108], r[109], r[110]) = (9, 3, 1);
    r[510..512].copy_from_slice(&[0x55, 0xAA]);
    let sum = r[..11 * 512]
        .iter()
        .enumerate()
        .filter(|(i, _)| !matches!(i, 106 | 107 | 112))
        .fold(0u32, |s, (_, &b)| {
            s.rotate_right(1).wrapping_add(u32::from(b))
        });
    for at in (11 * 512..12 * 512).step_by(4) {
        put32(&mut r, at, sum);
    }
    let tmp = TempDir::new("huge");
    let p = tmp.0.join("huge.exfat");
    std::fs::write(&p, &r).unwrap();
    let err = Volume::open(&mut File::open(&p).unwrap()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::OutOfMemory);
    assert!(
        err.to_string()
            .contains("too big to edit in place (1057 MiB); rebuild it instead"),
        "{err}"
    );
    let findings = plan(&mut File::open(&p).unwrap(), &[]).unwrap_err();
    assert!(
        findings[0].contains("allocation tables are too big"),
        "{findings:?}"
    );
}

/// The byte just past the root directory's last entry set.
fn root_end(image: &Path) -> u64 {
    let (mut f, v) = open(image);
    v.list(&mut f, "")
        .unwrap()
        .iter()
        .map(|n| *n.entries.last().unwrap())
        .max()
        .unwrap()
        + 32
}

/// Fills in the SetChecksum of an entry set.
fn seal(set: &mut [u8]) {
    let sum = set
        .iter()
        .enumerate()
        .filter(|(i, _)| !matches!(i, 2 | 3))
        .fold(0u16, |s, (_, &b)| {
            s.rotate_right(1).wrapping_add(u16::from(b))
        });
    set[2..4].copy_from_slice(&sum.to_le_bytes());
}

/// Marks cluster `c` in use on disk, its FAT entry `next`.
fn use_cluster(image: &Path, v: &Volume, c: u32, next: u32) {
    write_at(image, v.fat_offset + u64::from(c) * 4, &next.to_le_bytes());
    let i = u64::from(c - 2);
    let at = v.cluster_offset(v.bitmap_stream.first_cluster) + i / 8;
    let b = read_byte(&mut File::open(image).unwrap(), at) | 1 << (i % 8);
    write_at(image, at, &[b]);
}

/// Deletes `name` from the root with every check: free bytes grew by `clusters`, each FAT
/// entry of `freed` is 0, the other files are unchanged.
fn delete_and_check(image: &Path, name: &str, clusters: u64, freed: &[u32]) {
    let mut want = read_back(image);
    want.remove(name);
    let (before, after) = edit(image, &[Op::Delete { path: name.into() }]);
    let (mut f, v) = open(image);
    assert_eq!(after - before, clusters * v.cluster_size());
    assert_eq!(v.lookup(&mut f, name).unwrap(), None);
    for &c in freed {
        assert_eq!(v.fat[c as usize], 0, "FAT[{c}]");
        assert!(!v.in_use(c), "{c}");
    }
    assert_eq!(read_back(image), want);
}

#[test]
fn delete_a_fragmented_fat_chained_file() {
    let tmp = TempDir::new("delfrag");
    let (image, _) = build(&tmp, &Options::default());
    let v = open(&image).1;
    let (a, b) = two_free(&v);
    write_at(&image, v.cluster_offset(a), &[0xAA; 65536]);
    write_at(&image, v.cluster_offset(b), &[0xBB; 100]);
    use_cluster(&image, &v, a, b);
    use_cluster(&image, &v, b, u32::MAX);
    let len = 65536u64 + 100;
    let mut set = vec![0u8; 3 * 32];
    (set[0], set[1], set[4]) = (0x85, 2, 0x20);
    (set[32], set[33], set[35]) = (0xC0, 0x01, 4); // FAT-chained (no NoFatChain)
    set[40..48].copy_from_slice(&len.to_le_bytes());
    put32(&mut set, 52, a);
    set[56..64].copy_from_slice(&len.to_le_bytes());
    set[64] = 0xC1;
    for (i, u) in "frag".encode_utf16().enumerate() {
        set[66 + 2 * i..68 + 2 * i].copy_from_slice(&u.to_le_bytes());
    }
    seal(&mut set);
    write_at(&image, root_end(&image), &set);
    let (mut f, v) = open(&image);
    let n = v.lookup(&mut f, "frag").unwrap().unwrap();
    assert_eq!(v.chain(n.stream).unwrap(), [a, b]);
    assert_eq!(read_back(&image)["frag"].len() as u64, len);
    delete_and_check(&image, "frag", 2, &[a, b]);
}

fn ren(from: &str, to: &str) -> Op {
    Op::Rename {
        from: from.into(),
        to: to.into(),
    }
}

/// `want` with `from` (a file, or every file under the folder) renamed to `to`.
fn moved(mut want: BTreeMap<String, Vec<u8>>, from: &str, to: &str) -> BTreeMap<String, Vec<u8>> {
    let keys: Vec<String> = want
        .keys()
        .filter(|p| *p == from || p.starts_with(&format!("{from}/")))
        .cloned()
        .collect();
    assert!(!keys.is_empty(), "{from}");
    for k in keys {
        let b = want.remove(&k).unwrap();
        want.insert(format!("{to}{}", &k[from.len()..]), b);
    }
    want
}

fn put(path: &str, data: Data) -> Op {
    Op::Put {
        path: path.into(),
        len: data.len(),
        data,
    }
}

fn source(id: u32, offset: u64, len: u64) -> Data {
    Data::Source { id, offset, len }
}

fn mkdir(path: &str) -> Op {
    Op::Mkdir { path: path.into() }
}

#[test]
fn no_room_is_one_finding() {
    let tmp = TempDir::new("putfull");
    let (image, _) = build(&tmp, &Options::default());
    let (_, v) = open(&image);
    let (cs, free) = (v.cluster_size(), v.free_clusters());
    let mut f = File::open(&image).unwrap();
    let mb = |clusters: u64| (clusters * cs).div_ceil(1_000_000);
    let have = free * cs / 1_000_000;
    // Data and new folder clusters are counted together.
    // 16 folders (a cluster each, about 1 MB) and a file in the last one.
    let deep = "d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s";
    let ops = [
        put("a.bin", Data::Zero(free * cs)),
        mkdir(deep),
        put(&format!("{deep}/b.bin"), Data::Zero(1)),
    ];
    let findings = plan(&mut f, &ops).unwrap_err();
    let msg = |n| {
        format!(
            "the changes need {} MB, the image has {have} MB free",
            mb(n)
        )
    };
    assert_ne!(mb(free + 17), mb(free + 2));
    assert_eq!(findings, [msg(free + 17)]);
    // What a plan frees is not reused: a delete does not make room for a replace.
    let ops = [
        Op::Delete {
            path: "data/big.bin".into(),
        },
        put("eboot.bin", Data::Zero((free + 1) * cs)),
    ];
    assert_eq!(plan(&mut f, &ops).unwrap_err(), [msg(free + 1)]);
    // Exactly the free space fits.
    assert!(plan(&mut f, &[put("eboot.bin", Data::Zero(free * cs))]).is_ok());
}

/// Every op in one plan. With `FORGE_EXFAT_KEEP=<dir>` the edited image is kept there as
/// `every-op.exfat`, its expected tree beside it in `every-op/` (for `scripts/check-exfat.sh`).
#[test]
fn every_op_in_one_plan() {
    let tmp = TempDir::new("everyop");
    let (image, _) = build(&tmp, &Options::default());
    let mut want = read_back(&image);
    let cs = open(&image).1.cluster_size();
    let ops = [
        Op::Delete { path: LONG.into() },
        Op::Delete {
            path: "data/sub".into(),
        },
        ren("data/Ünïcode.txt", "empty/Ünïcode-moved.txt"),
        ren("empty", "Empty-Renamed"),
        put("eboot.bin", source(30, 0, 3 * cs + 7)),
        put("data/big.bin", Data::Bytes(vec![0x42; 1000])),
        mkdir("new/a/b"),
        put("new/a/b/c.bin", source(31, 0, 2 * cs)),
        put("new/zero.bin", Data::Zero(0)),
        put("top.bin", source(32, 5, 12_345)),
    ];
    edit(&image, &ops);
    want.remove(LONG);
    want.remove("data/sub/x.txt");
    let want = moved(want, "data/Ünïcode.txt", "Empty-Renamed/Ünïcode-moved.txt");
    let mut want = want;
    want.insert("eboot.bin".into(), bytes(30, 3 * cs as usize + 7));
    want.insert("data/big.bin".into(), vec![0x42; 1000]);
    want.insert("new/a/b/c.bin".into(), bytes(31, 2 * cs as usize));
    want.insert("new/zero.bin".into(), Vec::new());
    want.insert("top.bin".into(), bytes(32, 5 + 12_345)[5..].to_vec());
    assert_eq!(read_back(&image), want);
    let (_, v) = open(&image);
    assert!(!v.dirty);
    let Some(keep) = std::env::var_os("FORGE_EXFAT_KEEP") else {
        return;
    };
    let keep = PathBuf::from(keep);
    let tree = keep.join("every-op");
    let _ = std::fs::remove_dir_all(&tree);
    for (p, b) in &want {
        let at = tree.join(p);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, b).unwrap();
    }
    std::fs::create_dir_all(tree.join("data")).unwrap();
    std::fs::copy(&image, keep.join("every-op.exfat")).unwrap();
}
