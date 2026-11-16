//! Forward-only exFAT image writer (512 B sectors, 64 KiB clusters) for ShadowMountPlus `.exfat` images.
//!
//! Frozen interface (ps5-dump-forge-core depends on it): `plan` validates names and sizes the image
//! without reading file data; `write` streams the files into `out` in offset order.
//!
//! Ported from MkPFS (PSBrew, GPL-3.0) `exfat_writer.py` and `_exfat_upcase.py`, checked
//! against the Microsoft exFAT specification. Differences from the port: names are
//! validated and hashed through the real up-case table (MkPFS upcases ASCII only), the
//! serial is derived from the manifest instead of fixed, the image is sized like the
//! user's known-good `exfat.sh` (free spare for `image_rw=` mounts) instead of tight, macOS
//! junk is left out by path, and file data comes from a [`SourceTree`] instead of a folder.
//!
//! Because every size is known up front, the whole image is laid out before the first
//! byte is written, and nothing is ever written behind the cursor — so the same stream
//! can later go straight into a `.ffpfsc` container. The image, by offset:
//!
//! | Sectors / clusters | What |
//! |---|---|
//! | sectors 0..12 | main boot region (boot sector, 8 extended boot sectors, OEM, reserved, checksum) |
//! | sectors 12..24 | backup boot region, identical |
//! | sector 128.. | the FAT, padded to a cluster |
//! | cluster 2.. | allocation bitmap, up-case table, root directory |
//! | then | the tree in pre-order: a directory, then everything under it |
//! | last | free clusters: the spare of the default size, plus `Options::free_bytes` |
//!
//! Every allocation is one contiguous run, so each stream carries `NoFatChain`, and the FAT
//! still holds an explicit chain for every run so a driver that ignores the flag reads the
//! same bytes. The root directory, bitmap and up-case table have no flag to set and rely
//! on their chains.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use ps5upload_fpkg::source::SourceTree;
use ps5upload_fpkg::{Error, Result};

pub mod edit;
mod names;
mod upcase_table;

const SECTOR: u64 = 512;
const SECTOR_SHIFT: u8 = 9;
/// Sectors per cluster, as a shift: 2^7 × 512 B = 64 KiB, the size SMP's fast path wants.
const CLUSTER_SHIFT: u8 = 7;
const CLUSTER: u64 = SECTOR << CLUSTER_SHIFT;
const SECTORS_PER_CLUSTER: u64 = 1 << CLUSTER_SHIFT;
/// The FAT starts one cluster in, as newfs_exfat and MkPFS place it; it keeps the heap aligned.
const FAT_OFFSET: u64 = SECTORS_PER_CLUSTER;
const BOOT_REGION: usize = 12 * SECTOR as usize;
/// The first heap cluster; clusters 0 and 1 exist only as FAT slots.
pub(crate) const FIRST_CLUSTER: u64 = 2;
/// ClusterCount may not exceed 2^32 − 11.
pub(crate) const MAX_CLUSTERS: u64 = (1 << 32) - 11;
/// A directory's DataLength may not exceed 256 MiB.
pub(crate) const MAX_DIR_BYTES: u64 = 256 << 20;
pub(crate) const ENTRY: usize = 32;
/// The OEM Parameters record (spec §3.3) a maker's mark goes in: GUID
/// {182B1321-1B2D-441D-BECA-28B704837CA0}, ours, in the on-disk order (first three fields
/// little-endian), then 32 bytes of ASCII.
pub const MAKER_GUID: [u8; 16] = [
    0x21, 0x13, 0x2B, 0x18, 0x2D, 0x1B, 0x1D, 0x44, 0xBE, 0xCA, 0x28, 0xB7, 0x04, 0x83, 0x7C, 0xA0,
];
const MAKER_LEN: usize = 32;
/// Sector 9 of the boot region: OEM Parameters.
const OEM_SECTOR: usize = 9;
const NAME_UNITS_PER_ENTRY: usize = 15;
/// Read size per `read_range`: bounded memory however large the file.
const CHUNK: u64 = 8 << 20;
/// Every timestamp is 2024-01-01 00:00:00, so the image depends on the tree alone.
pub(crate) const TIMESTAMP: u32 = ((2024 - 1980) << 25) | (1 << 21) | (1 << 16);
const FAT_MEDIA: u32 = 0xFFFF_FFF8;
pub(crate) const FAT_EOC: u32 = 0xFFFF_FFFF;
pub(crate) const ATTR_DIRECTORY: u16 = 0x10;
pub(crate) const ATTR_ARCHIVE: u16 = 0x20;
pub(crate) const ALLOCATION_POSSIBLE: u8 = 0x01;
pub(crate) const NO_FAT_CHAIN: u8 = 0x02;

/// Knobs a caller may set. `Default` is the SMP-recommended layout.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Extra free space (bytes) beyond what the files need, for `image_rw=` mounts.
    pub free_bytes: u64,
    /// Volume label (ps5-dump-forge-core passes the title ID), at most 11 UTF-16 units; a longer
    /// one is rejected by `plan`. `None` writes an empty label entry, as mkfs.exfat does.
    pub label: Option<String>,
    /// Maker's mark: printable ASCII, at most 32 bytes, written to the first OEM Parameters
    /// record under [`MAKER_GUID`]; `plan` rejects anything else. `None` leaves sector 9 zero.
    pub maker: Option<String>,
}

/// Everything `write` needs, fixed before the first file byte is read.
#[derive(Debug, Clone)]
pub struct Layout {
    /// Final image size in bytes (a multiple of 64 KiB).
    pub image_size: u64,
    /// File-data bytes `write` copies: the `total` its progress reports against.
    pub data_bytes: u64,
    /// Source paths left out as macOS junk (`.DS_Store`, `._*`, `.fseventsd`,
    /// `.Spotlight-V100`, `.Trashes`), for the caller's log.
    pub skipped: Vec<String>,
    geo: Geometry,
    /// Node 0 is the root directory.
    nodes: Vec<Node>,
    /// Nodes that own clusters, in heap order (root first, then pre-order).
    order: Vec<usize>,
    label: Vec<u16>,
    maker: Option<String>,
}

/// The boot-sector numbers, all in the units the boot sector stores.
#[derive(Debug, Clone, Copy)]
struct Geometry {
    fat_sectors: u32,
    heap_sector: u32,
    cluster_count: u32,
    /// Allocated clusters, all at the front of the heap.
    used_clusters: u32,
    bitmap_clusters: u32,
    upcase_cluster: u32,
    root_cluster: u32,
    serial: u32,
}

impl Layout {
    /// Bytes of the clusters the image leaves free.
    pub fn free_bytes(&self) -> u64 {
        u64::from(self.geo.cluster_count - self.geo.used_clusters) * CLUSTER
    }
}

/// A file or directory of the image.
#[derive(Debug, Clone)]
struct Node {
    name: String,
    /// `/`-separated source path; empty for the root.
    path: String,
    dir: bool,
    /// File length; directories use `clusters`.
    size: u64,
    children: Vec<usize>,
    /// 0 when nothing is allocated (an empty file).
    first_cluster: u32,
    clusters: u32,
}

impl Node {
    fn new(name: &str, path: &str, dir: bool, size: u64) -> Self {
        Self {
            name: name.to_string(),
            path: path.to_string(),
            dir,
            size,
            children: Vec::new(),
            first_cluster: 0,
            clusters: 0,
        }
    }

    /// The bytes its stream entry declares: a directory is its whole allocation.
    fn data_length(&self) -> u64 {
        if self.dir {
            u64::from(self.clusters) * CLUSTER
        } else {
            self.size
        }
    }
}

/// What was written.
#[derive(Debug, Clone)]
pub struct Report {
    pub image_size: u64,
    pub files: u64,
    /// Directories below the root.
    pub dirs: u64,
}

/// Validate names and lay the image out. Fails with every offending path listed.
/// Checks `cancel` between steps.
pub fn plan(tree: &dyn SourceTree, opts: &Options, cancel: &AtomicBool) -> Result<Layout> {
    check_cancel(cancel)?;
    let mut problems = Vec::new();
    let label = opts.label.as_deref().unwrap_or("");
    if let Some(why) = names::label_problem(label) {
        problems.push(format!("volume label {label:?}: {why}"));
    }
    if let Some(m) = &opts.maker
        && (m.len() > MAKER_LEN || !m.bytes().all(|b| b.is_ascii_graphic() || b == b' '))
    {
        problems.push(format!(
            "maker's mark {m:?}: printable ASCII, at most {MAKER_LEN} bytes"
        ));
    }
    let mut nodes = vec![Node::new("", "", true, 0)];
    let mut index = HashMap::new();
    let mut skipped = Vec::new();
    let paths = tree.files().iter().map(|f| (f.path.as_str(), Some(f.size)));
    let dirs = tree.empty_dirs().iter().map(|d| (d.as_str(), None));
    for (i, (path, size)) in paths.chain(dirs).enumerate() {
        if i % 4096 == 0 {
            check_cancel(cancel)?;
        }
        // Junk is left out, but the directory that held it stays (it may be empty now).
        if let Some(cut) = junk_at(path) {
            skipped.push(path.to_string());
            let parent = path[..cut].trim_end_matches('/');
            if !parent.is_empty() {
                add_path(&mut nodes, &mut index, parent, None, &mut problems);
            }
            continue;
        }
        add_path(&mut nodes, &mut index, path, size, &mut problems);
    }
    check_cancel(cancel)?;
    for d in 0..nodes.len() {
        if d % 4096 == 0 {
            check_cancel(cancel)?;
        }
        if nodes[d].dir {
            sort_and_check_collisions(&mut nodes, d, &mut problems);
        }
    }
    if !problems.is_empty() {
        problems.sort();
        problems.dedup();
        return Err(err(format!(
            "{} name problem(s) keep this tree out of an exFAT image:\n  {}",
            problems.len(),
            problems.join("\n  ")
        )));
    }
    check_cancel(cancel)?;

    let order = heap_order(&nodes);
    let mut content = 1u64; // the up-case table: 5836 bytes, one cluster
    let mut data_bytes = 0u64;
    let mut data_clusters = 0u64;
    for &n in &order {
        let clusters = if nodes[n].dir {
            let bytes = dir_entries(&nodes, n) * ENTRY as u64;
            if bytes > MAX_DIR_BYTES {
                return Err(err(format!(
                    "directory /{} needs {bytes} bytes of entries, over exFAT's 256 MiB",
                    nodes[n].path
                )));
            }
            bytes.div_ceil(CLUSTER)
        } else {
            data_bytes = add(data_bytes, nodes[n].size)?;
            data_clusters = add(data_clusters, nodes[n].size.div_ceil(CLUSTER))?;
            nodes[n].size.div_ceil(CLUSTER)
        };
        nodes[n].clusters = u32::try_from(clusters).map_err(|_| too_big())?;
        content = add(content, clusters)?;
    }

    let files = nodes.iter().filter(|n| !n.dir).count() as u64;
    let dirs = nodes.len() as u64 - files; // the root included, as the script counts
    let alloc = data_clusters.checked_mul(CLUSTER).ok_or_else(too_big)?;
    // The free room asked for is kept free whatever the folders take (the size alone budgets
    // 256 B per entry, a folder takes a cluster): sized for content + room clusters.
    let target = add(
        default_size(files, dirs, data_bytes, alloc)?,
        opts.free_bytes
            .checked_next_multiple_of(CLUSTER)
            .ok_or_else(too_big)?,
    )?;
    let Volume {
        bitmap,
        clusters: total,
        fat_sectors,
        heap_sector,
        image: image_size,
    } = size_volume(add(content, opts.free_bytes.div_ceil(CLUSTER))?, target)?;
    let used = add(bitmap, content)?;
    let mut next = FIRST_CLUSTER + bitmap + 1; // after the bitmap and the up-case table
    for &n in &order {
        nodes[n].first_cluster = u32::try_from(next).map_err(|_| too_big())?;
        next += u64::from(nodes[n].clusters);
    }
    debug_assert_eq!(next, FIRST_CLUSTER + used);

    let u32_of = |v: u64| u32::try_from(v).map_err(|_| too_big());
    let geo = Geometry {
        fat_sectors: u32_of(fat_sectors)?,
        heap_sector: u32_of(heap_sector)?,
        cluster_count: u32_of(total)?,
        used_clusters: u32_of(used)?,
        bitmap_clusters: u32_of(bitmap)?,
        upcase_cluster: u32_of(FIRST_CLUSTER + bitmap)?,
        root_cluster: nodes[0].first_cluster,
        serial: serial(tree, opts, image_size),
    };
    check_cancel(cancel)?;
    Ok(Layout {
        image_size,
        data_bytes,
        geo,
        nodes,
        order,
        label: label.encode_utf16().collect(),
        maker: opts.maker.clone(),
        skipped,
    })
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
    // Unwritten regions must read back as zeros, and the image starts at byte 0.
    if out.seek(SeekFrom::End(0))? != 0 {
        return Err(err("the exFAT output is not empty"));
    }
    check_cancel(cancel)?;
    check_unchanged(tree, layout)?;
    let geo = layout.geo;
    let mut sink = Sink {
        w: BufWriter::with_capacity(1 << 20, &mut *out),
        pos: 0,
        unchecked: 0,
        cancel,
    };

    // The backup boot region is a byte copy; the checksum covers either.
    let boot = boot_region(layout);
    sink.write(&boot)?;
    sink.write(&boot)?;
    sink.zeros_to(FAT_OFFSET * SECTOR)?;
    write_fat(&mut sink, layout)?;
    sink.zeros_to(u64::from(geo.heap_sector) * SECTOR)?;
    check_cancel(cancel)?;

    write_bitmap(&mut sink, &geo)?;
    sink.expect(cluster_offset(&geo, geo.upcase_cluster))?;
    let table = names::table_bytes();
    sink.write(&table)?;
    sink.zeros_to(cluster_offset(&geo, geo.upcase_cluster + 1))?;

    let mut done = 0u64;
    let mut report = Report {
        image_size: layout.image_size,
        files: 0,
        dirs: 0,
    };
    for &n in &layout.order {
        check_cancel(cancel)?;
        let node = &layout.nodes[n];
        sink.expect(cluster_offset(&geo, node.first_cluster))?;
        if node.dir {
            write_dir(&mut sink, layout, n)?;
        } else {
            copy_file(
                tree,
                node,
                &mut sink,
                cancel,
                &mut done,
                layout.data_bytes,
                progress,
            )?;
        }
        sink.zeros_to(cluster_offset(&geo, node.first_cluster + node.clusters))?;
    }
    for node in &layout.nodes[1..] {
        if node.dir {
            report.dirs += 1;
        } else {
            report.files += 1;
        }
    }
    sink.expect(cluster_offset(
        &geo,
        FIRST_CLUSTER as u32 + geo.used_clusters,
    ))?;
    progress(done, layout.data_bytes);
    // A cancel raised by the last progress call still wins over finishing.
    check_cancel(cancel)?;
    sink.w.flush()?;
    let end = sink.pos;
    drop(sink);
    // The free tail: one zero byte at the end extends the image (sparse on a fresh file,
    // zero-filled by a stream).
    if end > layout.image_size {
        return Err(err(format!(
            "internal error: exFAT writer reached byte {end} of a {}-byte image",
            layout.image_size
        )));
    }
    if end < layout.image_size {
        out.seek(SeekFrom::Start(layout.image_size - 1))?;
        out.write_all(&[0])?;
        out.flush()?;
    }
    Ok(report)
}

/// Adds `path` (a file when `size` is set, else a directory) and its parent directories,
/// recording every name that cannot go into the image.
fn add_path(
    nodes: &mut Vec<Node>,
    index: &mut HashMap<String, usize>,
    path: &str,
    size: Option<u64>,
    problems: &mut Vec<String>,
) {
    let parts: Vec<&str> = path.split('/').collect();
    let mut parent = 0;
    let mut end = 0;
    for (i, part) in parts.iter().enumerate() {
        end += part.len() + usize::from(i > 0);
        let here = &path[..end];
        let dir = i + 1 < parts.len() || size.is_none();
        if let Some(&n) = index.get(here) {
            if nodes[n].dir && dir {
                parent = n;
                continue;
            }
            let what = if nodes[n].dir == dir {
                "listed twice"
            } else {
                "both a file and a directory"
            };
            problems.push(format!("{}: {what}", here.escape_debug()));
            return;
        }
        if let Some(why) = names::problem(part) {
            problems.push(format!("{}: {why}", here.escape_debug()));
        }
        let n = nodes.len();
        nodes.push(Node::new(
            part,
            here,
            dir,
            size.filter(|_| !dir).unwrap_or(0),
        ));
        nodes[parent].children.push(n);
        index.insert(here.to_string(), n);
        parent = n;
    }
}

/// Sorts a directory's children by name (deterministic output) and reports names that
/// are one name to the case-insensitive volume.
fn sort_and_check_collisions(nodes: &mut [Node], dir: usize, problems: &mut Vec<String>) {
    let mut kids = std::mem::take(&mut nodes[dir].children);
    kids.sort_by(|&a, &b| nodes[a].name.cmp(&nodes[b].name));
    let mut seen: HashMap<Vec<u16>, usize> = HashMap::new();
    for &k in &kids {
        match seen.entry(names::upcase(&nodes[k].name)) {
            Entry::Occupied(first) => problems.push(format!(
                "{}: same name as {} on the case-insensitive volume",
                nodes[k].path.escape_debug(),
                nodes[*first.get()].path.escape_debug()
            )),
            Entry::Vacant(slot) => {
                slot.insert(k);
            }
        }
    }
    nodes[dir].children = kids;
}

/// The nodes that own clusters, root first, then each directory followed by its contents.
fn heap_order(nodes: &[Node]) -> Vec<usize> {
    let mut order = Vec::new();
    let mut stack = vec![0];
    while let Some(n) = stack.pop() {
        order.push(n);
        for &c in nodes[n].children.iter().rev() {
            if nodes[c].dir || nodes[c].size > 0 {
                stack.push(c);
            }
        }
    }
    order
}

/// 32-byte entries a directory holds, plus one left unused: a full last cluster has no
/// end-of-directory record, and the vendored reader then runs on into the next cluster
/// of the root (which it reads by contiguity). One spare slot costs a cluster at most.
fn dir_entries(nodes: &[Node], dir: usize) -> u64 {
    let root = if dir == 0 { 3 } else { 0 }; // label, bitmap, up-case
    let sets: u64 = nodes[dir]
        .children
        .iter()
        .map(|&c| {
            2 + nodes[c]
                .name
                .encode_utf16()
                .count()
                .div_ceil(NAME_UNITS_PER_ENTRY) as u64
        })
        .sum();
    root + sets + 1
}

/// The image size the user's known-good `exfat.sh` (after upstream `mkexfat_macos.sh`)
/// gives a tree: data rounded to clusters + FAT + bitmap + 256 B per entry + 32 MiB of
/// fixed metadata, plus 0.5 % spare clamped to 64..512 MiB, never under the raw bytes
/// plus 64 MiB, rounded up to 1 MiB. The spare is the free space `image_rw=` mounts
/// write into, so images are deliberately not tight. `dirs` counts the root, as `find -type d`.
fn default_size(files: u64, dirs: u64, bytes: u64, alloc: u64) -> Result<u64> {
    const MIB: u64 = 1 << 20;
    let clusters = alloc / CLUSTER;
    let entries = add(files, dirs)?.checked_mul(256).ok_or_else(too_big)?;
    let mut total = add(alloc, clusters * 4)?;
    total = add(total, clusters / 8 + 1)?;
    total = add(total, entries)?;
    total = add(total, 32 * MIB)?;
    total = add(total, (total / 200).clamp(64 * MIB, 512 * MIB))?;
    total = total.max(add(bytes, 64 * MIB)?);
    total.checked_next_multiple_of(MIB).ok_or_else(too_big)
}

/// Where the regions fall, in boot-sector units.
struct Volume {
    bitmap: u64,
    clusters: u64,
    fat_sectors: u64,
    heap_sector: u64,
    image: u64,
}

/// Lays out a volume of exactly `image` bytes (a multiple of 64 KiB) holding `content`
/// clusters, or the smallest one that holds them if that is larger. The FAT is sized for
/// every cluster the image could hold, which may leave it a little longer than the
/// cluster count needs: FatLength is a lower bound in the spec, and mkfs.exfat does the same.
fn size_volume(content: u64, image: u64) -> Result<Volume> {
    let fat_sectors = |clusters: u64| {
        ((clusters + 2) * 4)
            .div_ceil(SECTOR)
            .next_multiple_of(SECTORS_PER_CLUSTER)
    };
    // The bitmap covers every cluster, its own included.
    let bitmap_for = |clusters: u64| clusters.div_ceil(8).div_ceil(CLUSTER);
    let mut image = image.next_multiple_of(CLUSTER);
    loop {
        if image / CLUSTER > MAX_CLUSTERS + 64 {
            return Err(too_big());
        }
        let fat = fat_sectors(image / CLUSTER);
        let heap_sector = FAT_OFFSET + fat;
        let heap = heap_sector * SECTOR;
        if image > heap {
            let clusters = (image - heap) / CLUSTER;
            let bitmap = bitmap_for(clusters);
            if clusters <= MAX_CLUSTERS && clusters >= add(bitmap, content)? {
                return Ok(Volume {
                    bitmap,
                    clusters,
                    fat_sectors: fat,
                    heap_sector,
                    image,
                });
            }
        }
        // Too small for the content: grow until it fits (only for a caller's tiny target).
        image = add(image, CLUSTER)?;
    }
}

/// VolumeSerialNumber from the manifest (FNV-1a), so the same tree gives the same image.
fn serial(tree: &dyn SourceTree, opts: &Options, image_size: u64) -> u32 {
    let mut h: u32 = 0x811C_9DC5;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
        }
    };
    for f in tree.files() {
        eat(f.path.as_bytes());
        eat(&[0]);
        eat(&f.size.to_le_bytes());
    }
    for d in tree.empty_dirs() {
        eat(d.as_bytes());
        eat(&[1]);
    }
    eat(opts.label.as_deref().unwrap_or("").as_bytes());
    eat(&image_size.to_le_bytes());
    h
}

/// Byte offset of the first macOS-junk component of `path`, if it has one. The upstream
/// scanners filter these already; this keeps them out of the image whatever the source.
fn junk_at(path: &str) -> Option<usize> {
    let mut at = 0;
    for part in path.split('/') {
        let lower = part.to_ascii_lowercase();
        if part.starts_with("._")
            || matches!(
                lower.as_str(),
                ".ds_store" | ".fseventsd" | ".spotlight-v100" | ".trashes"
            )
        {
            return Some(at);
        }
        at += part.len() + 1;
    }
    None
}

/// `write` gets the tree again; it must be the one `plan` measured.
fn check_unchanged(tree: &dyn SourceTree, layout: &Layout) -> Result<()> {
    let sizes: HashMap<&str, u64> = tree
        .files()
        .iter()
        .filter(|f| junk_at(&f.path).is_none())
        .map(|f| (f.path.as_str(), f.size))
        .collect();
    let planned = layout.nodes.iter().filter(|n| !n.dir);
    let mut count = 0usize;
    for node in planned {
        count += 1;
        if sizes.get(node.path.as_str()) != Some(&node.size) {
            return Err(err(format!(
                "{} changed since the exFAT image was planned",
                node.path
            )));
        }
    }
    if count != sizes.len() {
        return Err(err(
            "the source's file list changed since the exFAT image was planned",
        ));
    }
    Ok(())
}

fn boot_region(layout: &Layout) -> Vec<u8> {
    let g = &layout.geo;
    let mut r = vec![0u8; BOOT_REGION];
    let s = &mut r[..SECTOR as usize];
    s[0..3].copy_from_slice(&[0xEB, 0x76, 0x90]);
    s[3..11].copy_from_slice(b"EXFAT   ");
    // BootCode without boot-strapping instructions is all 0xF4 (halt), spec §3.1.19.
    s[120..510].fill(0xF4);
    // 11..64 MustBeZero; 64 PartitionOffset 0: a raw image, the volume at byte 0.
    put64(s, 72, layout.image_size / SECTOR);
    put32(s, 80, FAT_OFFSET as u32);
    put32(s, 84, g.fat_sectors);
    put32(s, 88, g.heap_sector);
    put32(s, 92, g.cluster_count);
    put32(s, 96, g.root_cluster);
    put32(s, 100, g.serial);
    put16(s, 104, 0x0100); // FileSystemRevision 1.00
    put16(s, 106, 0); // VolumeFlags: clean, first FAT active
    s[108] = SECTOR_SHIFT;
    s[109] = CLUSTER_SHIFT;
    s[110] = 1; // NumberOfFats
    s[111] = 0x80; // DriveSelect
    s[112] = (u64::from(g.used_clusters) * 100 / u64::from(g.cluster_count)) as u8;
    put16(s, 510, 0xAA55);
    for i in 1..=8 {
        let at = i * SECTOR as usize;
        put32(
            &mut r[at..at + SECTOR as usize],
            SECTOR as usize - 4,
            0xAA55_0000,
        );
    }
    // Sector 9 (OEM parameters): the maker's record, the other nine null (zero GUID); sector
    // 10 (reserved) stays zero.
    if let Some(m) = &layout.maker {
        let at = OEM_SECTOR * SECTOR as usize;
        r[at..at + 16].copy_from_slice(&MAKER_GUID);
        r[at + 16..at + 16 + m.len()].copy_from_slice(m.as_bytes());
    }
    let sum = boot_checksum(&r[..11 * SECTOR as usize]);
    for at in (11 * SECTOR as usize..BOOT_REGION).step_by(4) {
        put32(&mut r, at, sum);
    }
    r
}

/// BootChecksum (spec §3.4): sectors 0..11, skipping VolumeFlags and PercentInUse, which
/// change without the region being rewritten.
pub(crate) fn boot_checksum(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .enumerate()
        .filter(|(i, _)| !matches!(i, 106 | 107 | 112))
        .fold(0u32, |sum, (_, &b)| {
            sum.rotate_right(1).wrapping_add(u32::from(b))
        })
}

/// The FAT: media and reserved entries, a chain per run, zero for free clusters, padded
/// to its declared length.
fn write_fat<W: Write>(sink: &mut Sink<W>, layout: &Layout) -> Result<()> {
    let g = &layout.geo;
    let start = sink.pos;
    sink.write(&FAT_MEDIA.to_le_bytes())?;
    sink.write(&FAT_EOC.to_le_bytes())?;
    let mut runs = vec![
        (FIRST_CLUSTER as u32, g.bitmap_clusters),
        (g.upcase_cluster, 1),
    ];
    runs.extend(
        layout
            .order
            .iter()
            .map(|&n| (layout.nodes[n].first_cluster, layout.nodes[n].clusters)),
    );
    for (first, count) in runs {
        for c in first..first + count {
            let next = if c + 1 == first + count {
                FAT_EOC
            } else {
                c + 1
            };
            sink.write(&next.to_le_bytes())?;
        }
    }
    sink.zeros_to(start + u64::from(g.fat_sectors) * SECTOR)
}

/// The allocation bitmap: the used clusters are all at the front of the heap.
fn write_bitmap<W: Write>(sink: &mut Sink<W>, g: &Geometry) -> Result<()> {
    sink.expect(cluster_offset(g, FIRST_CLUSTER as u32))?;
    static ONES: [u8; 64 * 1024] = [0xFF; 64 * 1024];
    let used = u64::from(g.used_clusters);
    let mut full = used / 8;
    while full > 0 {
        let n = full.min(ONES.len() as u64);
        sink.write(&ONES[..n as usize])?;
        full -= n;
    }
    if used % 8 != 0 {
        sink.write(&[(1u8 << (used % 8)) - 1])?;
    }
    sink.zeros_to(cluster_offset(g, FIRST_CLUSTER as u32 + g.bitmap_clusters))
}

/// A directory's entry sets, one at a time; the caller zero-fills the rest of its
/// clusters (end of directory).
fn write_dir<W: Write>(sink: &mut Sink<W>, layout: &Layout, dir: usize) -> Result<()> {
    let g = &layout.geo;
    let node = &layout.nodes[dir];
    if dir == 0 {
        let mut label = [0u8; ENTRY];
        label[0] = 0x83;
        label[1] = layout.label.len() as u8;
        for (i, &u) in layout.label.iter().enumerate() {
            put16(&mut label, 2 + 2 * i, u);
        }
        sink.write(&label)?;

        let mut bitmap = [0u8; ENTRY];
        bitmap[0] = 0x81; // BitmapFlags 0: the first (only) FAT's bitmap
        put32(&mut bitmap, 20, FIRST_CLUSTER as u32);
        put64(&mut bitmap, 24, u64::from(g.cluster_count).div_ceil(8));
        sink.write(&bitmap)?;

        let table = names::table_bytes();
        let mut upcase = [0u8; ENTRY];
        upcase[0] = 0x82;
        put32(&mut upcase, 4, names::table_checksum(&table));
        put32(&mut upcase, 20, g.upcase_cluster);
        put64(&mut upcase, 24, table.len() as u64);
        sink.write(&upcase)?;
    }
    for &c in &node.children {
        sink.write(&entry_set(&layout.nodes[c]))?;
    }
    Ok(())
}

/// File + Stream Extension + File Name entries for one node, SetChecksum filled in.
fn entry_set(node: &Node) -> Vec<u8> {
    let units: Vec<u16> = node.name.encode_utf16().collect();
    let name_entries = units.len().div_ceil(NAME_UNITS_PER_ENTRY);
    let mut set = vec![0u8; ENTRY * (2 + name_entries)];

    set[0] = 0x85;
    set[1] = (1 + name_entries) as u8; // SecondaryCount
    put16(
        &mut set,
        4,
        if node.dir {
            ATTR_DIRECTORY
        } else {
            ATTR_ARCHIVE
        },
    );
    put32(&mut set, 8, TIMESTAMP); // created
    put32(&mut set, 12, TIMESTAMP); // modified
    put32(&mut set, 16, TIMESTAMP); // accessed

    let s = &mut set[ENTRY..2 * ENTRY];
    s[0] = 0xC0;
    // An empty file owns no cluster: FirstCluster 0, and a FAT chain is the only mode
    // that means anything without one.
    s[1] = if node.first_cluster == 0 {
        ALLOCATION_POSSIBLE
    } else {
        ALLOCATION_POSSIBLE | NO_FAT_CHAIN
    };
    s[3] = units.len() as u8;
    put16(s, 4, names::name_hash(&names::upcase(&node.name)));
    put64(s, 8, node.data_length()); // ValidDataLength: every byte is written
    put32(s, 20, node.first_cluster);
    put64(s, 24, node.data_length());

    for (i, chunk) in units.chunks(NAME_UNITS_PER_ENTRY).enumerate() {
        let e = &mut set[(2 + i) * ENTRY..(3 + i) * ENTRY];
        e[0] = 0xC1;
        for (j, &u) in chunk.iter().enumerate() {
            put16(e, 2 + 2 * j, u);
        }
    }
    let sum = set_checksum(&set);
    put16(&mut set, 2, sum);
    set
}

/// SetChecksum (spec §6.3.3): every byte of the set but the checksum field itself.
pub(crate) fn set_checksum(set: &[u8]) -> u16 {
    set.iter()
        .enumerate()
        .filter(|(i, _)| !matches!(i, 2 | 3))
        .fold(0u16, |sum, (_, &b)| {
            sum.rotate_right(1).wrapping_add(u16::from(b))
        })
}

/// Streams one file in `CHUNK` reads, checking `cancel` before each.
fn copy_file<W: Write>(
    tree: &mut dyn SourceTree,
    node: &Node,
    sink: &mut Sink<W>,
    cancel: &AtomicBool,
    done: &mut u64,
    total: u64,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    let mut at = 0u64;
    while at < node.size {
        check_cancel(cancel)?;
        let want = (node.size - at).min(CHUNK);
        let buf = tree.read_range(&node.path, at, want as usize)?;
        // Sources may return short reads; none may return nothing or too much.
        if buf.is_empty() || buf.len() as u64 > want {
            return Err(err(format!(
                "{}: read {} bytes at {at} of its planned {} (did it change?)",
                node.path,
                buf.len(),
                node.size
            )));
        }
        sink.write(&buf)?;
        at += buf.len() as u64;
        *done += buf.len() as u64;
        progress(*done, total);
    }
    Ok(())
}

/// The output with its position, so every region can assert it starts where the plan
/// put it — the guarantee that the writer never goes back. It also checks `cancel` every
/// `CHUNK` bytes, so long metadata runs (FAT, bitmap, zero fill) stay cancellable.
struct Sink<'a, W: Write> {
    w: BufWriter<&'a mut W>,
    pos: u64,
    unchecked: u64,
    cancel: &'a AtomicBool,
}

impl<W: Write> Sink<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.w.write_all(bytes)?;
        self.pos += bytes.len() as u64;
        self.unchecked += bytes.len() as u64;
        if self.unchecked >= CHUNK {
            self.unchecked = 0;
            check_cancel(self.cancel)?;
        }
        Ok(())
    }

    fn zeros_to(&mut self, end: u64) -> Result<()> {
        if end < self.pos {
            return Err(err(format!(
                "exFAT writer overran: at {} but the next region starts at {end}",
                self.pos
            )));
        }
        static ZEROS: [u8; 64 * 1024] = [0; 64 * 1024];
        while self.pos < end {
            let n = (end - self.pos).min(ZEROS.len() as u64) as usize;
            self.write(&ZEROS[..n])?;
        }
        Ok(())
    }

    fn expect(&self, at: u64) -> Result<()> {
        if self.pos != at {
            return Err(err(format!(
                "exFAT writer is at {} but the plan says {at}",
                self.pos
            )));
        }
        Ok(())
    }
}

fn cluster_offset(g: &Geometry, cluster: u32) -> u64 {
    u64::from(g.heap_sector) * SECTOR + (u64::from(cluster) - FIRST_CLUSTER) * CLUSTER
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(err("exFAT image cancelled"));
    }
    Ok(())
}

fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).ok_or_else(too_big)
}

fn too_big() -> Error {
    err("the tree is too large for an exFAT volume with 64 KiB clusters")
}

/// The maker's mark of an image this crate wrote, from its main boot region: `None` for
/// any other exFAT (not 512-byte sectors at byte 0, a bad boot checksum, which the spec
/// says voids the OEM parameters, or no record under [`MAKER_GUID`]).
pub fn read_maker<R: Read + Seek>(r: &mut R) -> std::io::Result<Option<String>> {
    let mut b = vec![0u8; BOOT_REGION];
    r.seek(SeekFrom::Start(0))?;
    r.read_exact(&mut b)?;
    let checksum = 11 * SECTOR as usize;
    let sum = boot_checksum(&b[..checksum]).to_le_bytes();
    if &b[3..11] != b"EXFAT   "
        || b[108] != SECTOR_SHIFT
        || !b[checksum..].chunks(4).all(|c| c == sum)
    {
        return Ok(None);
    }
    let oem = &b[OEM_SECTOR * SECTOR as usize..][..10 * (16 + MAKER_LEN)];
    let Some(rec) = oem.chunks(16 + MAKER_LEN).find(|r| r[..16] == MAKER_GUID) else {
        return Ok(None);
    };
    let text = &rec[16..];
    let end = text.iter().position(|&c| c == 0).unwrap_or(MAKER_LEN);
    Ok(std::str::from_utf8(&text[..end]).ok().map(str::to_string))
}

fn err(msg: impl Into<String>) -> Error {
    Error::Format(msg.into())
}

fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
