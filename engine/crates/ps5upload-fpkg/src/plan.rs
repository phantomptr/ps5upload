//! The pure layout of a package: every offset fixed before a byte is read.
//!
//! Mirrors the measured template of real debug packages: the inner image is data-first,
//! files are packed in afid order, and the metadata region (superblock, inode table,
//! tables and directory blocks) sits on top of an aligned base.

use std::collections::BTreeMap;

use crate::flt;
use crate::source::SourceFile;
use crate::{format_err, Result, BLOCK};

/// Generated into the tree when the source lacks it.
pub const KEYSTONE: &str = "sce_sys/keystone";
pub const KEYSTONE_LEN: u64 = 96;
/// The image gives every file a boundary of its own at this granularity.
pub const FILE_ALIGN: u64 = 0x1_0000;

/// `metaBase` alignment: two 256 KiB ublocks, the granularity the NAPS u2c mapping
/// addresses. One further block is always left free so the block-info table has a home
/// after the data. The samples pad further (webbrowser's data ends at 0xA626 and its
/// metadata base is 0x400000); only the 256 KiB alignment is load-bearing here.
const META_ALIGN: u64 = 0x40000;

/// How far past the end of the file data a compressed image's metadata starts (before rounding
/// down to [`META_ALIGN`]). Measured on three references; see `build_with`.
const COMPRESSED_META_GAP: u64 = 0x40_0000;

/// `pfs-version.dat` is a system marker, not app payload — the app-payload count the
/// finalized-image header carries excludes it (measured: the sample's three uroot files
/// count as two).
const PFS_VERSION_DAT: &str = "pfs-version.dat";

/// Inner inode numbers 0..=3 are the super-root and its three tables.
pub const SUPER_ROOT_INODE: u32 = 0;
pub const INODE_FLT_INODE: u32 = 1;
pub const APR_FLT_INODE: u32 = 2;
pub const AFID_TABLE_INODE: u32 = 3;
pub const FIRST_DIR_INODE: u32 = 4;

/// Dirent kinds, as on disk.
pub const DIRENT_FILE: i8 = 2;
pub const DIRENT_DIR: i8 = 3;
pub const DIRENT_DOT: i8 = 4;
pub const DIRENT_DOTDOT: i8 = 5;

/// Inner inode mode and flag words, measured on the samples' outer template and read
/// from the reference for the inner tree.
pub const MODE_FILE: u16 = 0x816d;
pub const MODE_FILE_SCE_SYS: u16 = 0x8168;
pub const MODE_DIR_UROOT: u16 = 0x416d;
pub const MODE_DIR: u16 = 0x4168;
pub const FLAGS_DATA: u32 = 0x10;
pub const FLAGS_MODULE: u32 = 0x40;
pub const FLAGS_BLOB: u32 = 0x20;
pub const FLAGS_SCE_SYS: u32 = 0x0002_0000;
pub const FLAGS_TABLE: u32 = 0x0002_0010;

/// Where each inner-mount metadata structure lands, in absolute mount bytes, with its byte
/// length. Starts are block-aligned; a structure whose bytes outgrow one block runs on into the
/// next, which is what a real title's inode table, afid table and large directories do.
#[derive(Debug, Clone, Default)]
pub struct MetadataLayout {
    pub inode_table: (u64, u64),
    pub super_root: (u64, u64),
    pub flt: (u64, u64),
    pub flt_apr: (u64, u64),
    pub afid: (u64, u64),
    pub dirs: Vec<(u64, u64)>,
    /// Blocks the whole region occupies, the superblock block and the guard block included.
    pub blocks: u64,
}

/// The super-root's dirents: both flat-path tables, the afid table and `uroot`.
pub fn super_root_dirents() -> Vec<(String, u32, i8)> {
    vec![
        (
            "inode_flat_path_table".to_string(),
            INODE_FLT_INODE,
            DIRENT_FILE,
        ),
        (
            "apr_flat_path_table".to_string(),
            APR_FLT_INODE,
            DIRENT_FILE,
        ),
        (
            "afid_to_ino_table".to_string(),
            AFID_TABLE_INODE,
            DIRENT_FILE,
        ),
        ("uroot".to_string(), FIRST_DIR_INODE, DIRENT_DIR),
    ]
}

/// A file's placement in the inner image; `files` is in inode order after `build`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    pub path: String,
    pub size: u64,
    pub inode: u32,
    pub afid: u32,
    pub logical_offset: u64,
    /// Where the file sits in the image. Files are packed in the mount but each one starts at
    /// its own 64 KiB boundary on disk, so the two orderings differ once a file is not a whole
    /// number of those units — which is the usual case.
    pub on_disk_offset: u64,
    pub parent_inode: u32,
    pub dirent_offset: i32,
    pub sce_sys: bool,
    /// True for the keystone this build generated.
    pub generated: bool,
    /// An executable (SELF or ELF): `eboot.bin`, the `.prx`/`.sprx` modules. Set by
    /// [`Plan::mark_modules`], which reads the file's header.
    pub module: bool,
}

impl Plan {
    /// Mark the executables, which the image flags as modules. `is_module` answers for one
    /// path, normally from the file's first bytes (see [`is_module_header`]).
    ///
    /// Measured on Spider-Man 2: `eboot.bin` and every `.prx`/`.sprx`, `sce_sys/about/right.sprx`
    /// included, carry inode flags 0x50 (data + module) and every other file 0x30 (data + blob).
    /// We flagged everything 0x30, and the console refused to start the game:
    /// `sceSblACMgrGetFsSandboxType(.../eboot.bin) failed. 0x80020016` (FW 5.10 Phat).
    pub fn mark_modules(&mut self, mut is_module: impl FnMut(&str) -> bool) {
        for f in &mut self.files {
            f.module = !f.generated && f.size >= 4 && is_module(&f.path);
        }
    }
}

/// Whether a file's first bytes are an executable's: a PS5 or PS4 SELF (fake-signed or
/// genuine) or a plain ELF.
pub fn is_module_header(head: &[u8]) -> bool {
    use crate::source::magic;
    head.len() >= 4
        && [
            magic::SELF_PS5,
            magic::SELF_PS4,
            magic::SIGNED_SELF,
            magic::RAW_ELF,
        ]
        .iter()
        .any(|m| head[..4] == m[..])
}

impl Plan {
    /// The inner files in afid order — the order their payloads are laid out in, and the
    /// order the metric blob lists them.
    pub fn inner_files(&self) -> Vec<(String, u64, u64)> {
        self.afid_order
            .iter()
            .map(|&fi| {
                let f = &self.files[fi];
                (f.path.clone(), f.logical_offset, f.size)
            })
            .collect()
    }

    /// Every file the image holds as `(path, offset in the mount image, size)`: the inner image
    /// starts one block into the mount, after the finalized-image header.
    pub fn mount_files(&self) -> Vec<(String, u64, u64)> {
        self.files
            .iter()
            .map(|f| (f.path.clone(), crate::BLOCK + f.on_disk_offset, f.size))
            .collect()
    }

    /// Each file's logical offset by afid.
    pub fn afid_offsets(&self) -> Vec<u64> {
        self.afid_order
            .iter()
            .map(|&fi| self.files[fi].logical_offset)
            .collect()
    }

    /// `(logical, on_disk, size)` per file in afid order — everything the descriptor needs,
    /// since a file's two offsets no longer imply its length.
    pub fn placements(&self) -> Vec<(u64, u64, u64)> {
        self.afid_order
            .iter()
            .map(|&fi| {
                let f = &self.files[fi];
                (f.logical_offset, f.on_disk_offset, f.size)
            })
            .collect()
    }
}

impl PlannedFile {
    pub fn mode(&self) -> u16 {
        if self.sce_sys {
            MODE_FILE_SCE_SYS
        } else {
            MODE_FILE
        }
    }

    pub fn inode_flags(&self) -> u32 {
        let kind = if self.module {
            FLAGS_MODULE
        } else {
            FLAGS_BLOB
        };
        FLAGS_DATA | kind | if self.sce_sys { FLAGS_SCE_SYS } else { 0 }
    }
}

/// A directory of the inner tree, in pre-order with uroot first.
#[derive(Debug, Clone)]
pub struct PlannedDir {
    pub path: String,
    pub inode: u32,
    pub parent_inode: i32,
    pub dirent_offset: i32,
    pub nlink: u16,
    /// `(name, inode, kind)` in on-disk order: `.`, `..`, sub-directories, files.
    pub dirents: Vec<(String, u32, i8)>,
}

impl PlannedDir {
    /// `sce_sys` or a directory under it: the system directories, whose inodes carry the
    /// system mode and flags. Everything else, uroot included, is an ordinary directory.
    pub fn sce_sys(&self) -> bool {
        self.path == "sce_sys" || self.path.starts_with("sce_sys/")
    }
}

/// The whole layout. Pure: no file is read to build it.
#[derive(Debug)]
pub struct Plan {
    pub dirs: Vec<PlannedDir>,
    /// Inode order.
    pub files: Vec<PlannedFile>,
    /// Indices into `files`, in afid order.
    pub afid_order: Vec<usize>,
    pub flt_inode: Vec<(u64, u64)>,
    pub flt_apr: Vec<(u64, u64)>,
    pub afid_to_ino: Vec<i32>,
    pub first_file_inode: u32,
    /// End of the packed data region in the mount.
    pub data_end: u64,
    /// Metadata-region base, block-aligned (the inner superblock's offset).
    pub meta_base: u64,
    /// Inner mount size in blocks.
    pub ndblock: u64,
    pub metadata_blocks: u64,
    /// Where inside the mount each metadata structure lives. The writer places bytes by these
    /// offsets, so the planner and the writer cannot disagree about the region's shape.
    pub metadata: MetadataLayout,
    /// Directories below uroot plus every file: the count the header carries at `0x94`.
    pub content_inodes: u32,
    /// App-payload (non-`sce_sys`, non-marker) file count for the header's `0xF0`.
    pub app_file_count: u32,
}

struct DirNode {
    path: String,
    name: String,
    parent: Option<usize>,
    subdirs: Vec<usize>,
    files: Vec<usize>,
    inode: u32,
    dirent_offset: i32,
    nlink: u16,
}

fn ensure_dir(dirs: &mut Vec<DirNode>, index: &mut BTreeMap<String, usize>, path: &str) -> usize {
    if let Some(&i) = index.get(path) {
        return i;
    }
    let (parent_path, name) = match path.rsplit_once('/') {
        Some((p, n)) => (p.to_string(), n.to_string()),
        None => (String::new(), path.to_string()),
    };
    let parent = ensure_dir(dirs, index, &parent_path);
    let i = dirs.len();
    dirs.push(DirNode {
        path: path.to_string(),
        name,
        parent: Some(parent),
        subdirs: Vec::new(),
        files: Vec::new(),
        inode: 0,
        dirent_offset: -1,
        nlink: 1,
    });
    dirs[parent].subdirs.push(i);
    index.insert(path.to_string(), i);
    i
}

fn pre_order(dirs: &[DirNode], from: usize, out: &mut Vec<usize>) {
    out.push(from);
    for &d in &dirs[from].subdirs {
        pre_order(dirs, d, out);
    }
}

fn post_order(dirs: &[DirNode], from: usize, out: &mut Vec<usize>) {
    for &d in &dirs[from].subdirs {
        post_order(dirs, d, out);
    }
    out.push(from);
}

/// How many non-empty files a compressed image lets start in one 256 KiB ublock.
///
/// The layout descriptor finds a ublock's first block record from its group's base plus a
/// one-byte delta, so the records opening seven ublocks must number under 256. Every file opens
/// at least one record, and Sony's packages never come near the limit (at most 16 per group,
/// measured on Spider-Man 2, an EA title and the Web Browser), but a tree of thousands of tiny
/// files would pass it (Minecraft: 2043 records in one group). Sixteen starts per ublock keeps
/// a group at about 130.
pub const MAX_STARTS_PER_UBLOCK: u32 = 16;
const UBLOCK: u64 = 0x4_0000;

/// Plan the inner image and everything derived from it.
pub fn build(input: &[SourceFile]) -> Result<Plan> {
    build_with(input, false)
}

/// Like [`build`]; `spread` is for a compressed image. It moves the next file to a fresh
/// ublock once [`MAX_STARTS_PER_UBLOCK`] files have started in the current one. The gap
/// belongs to the file before it and is never read: an inode addresses its file by logical
/// offset and size.
pub fn build_with(input: &[SourceFile], spread: bool) -> Result<Plan> {
    let mut files: Vec<PlannedFile> = input
        .iter()
        .map(|f| PlannedFile {
            path: f.path.clone(),
            size: f.size,
            inode: 0,
            afid: 0,
            logical_offset: 0,
            on_disk_offset: 0,
            parent_inode: 0,
            dirent_offset: -1,
            sce_sys: f.path.starts_with("sce_sys/"),
            generated: false,
            module: false,
        })
        .collect();
    if !files.iter().any(|f| f.path == KEYSTONE) {
        files.push(PlannedFile {
            path: KEYSTONE.to_string(),
            size: KEYSTONE_LEN,
            inode: 0,
            afid: 0,
            logical_offset: 0,
            on_disk_offset: 0,
            parent_inode: 0,
            dirent_offset: -1,
            sce_sys: true,
            generated: true,
            module: false,
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    if files.is_empty() {
        return format_err("the source tree has no files");
    }

    // Directory tree.
    let mut dirs: Vec<DirNode> = vec![DirNode {
        path: String::new(),
        name: "uroot".to_string(),
        parent: None,
        subdirs: Vec::new(),
        files: Vec::new(),
        inode: 0,
        dirent_offset: -1,
        nlink: 1,
    }];
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    index.insert(String::new(), 0);
    for (fi, file) in files.iter().enumerate() {
        let parent_path = match file.path.rsplit_once('/') {
            Some((d, _)) => d.to_string(),
            None => String::new(),
        };
        let di = if parent_path.is_empty() {
            0
        } else {
            ensure_dir(&mut dirs, &mut index, &parent_path)
        };
        dirs[di].files.push(fi);
    }
    let dir_names: Vec<String> = dirs.iter().map(|d| d.name.clone()).collect();
    for d in dirs.iter_mut() {
        d.subdirs.sort_by(|a, b| dir_names[*a].cmp(&dir_names[*b]));
    }
    // Name-ordinal file order inside every directory.
    let names: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
    for d in dirs.iter_mut() {
        d.files.sort_by(|a, b| names[*a].cmp(&names[*b]));
    }

    // Inodes: directories pre-order, then files (directories post-order, files ordinal).
    let mut dirs_pre: Vec<usize> = Vec::new();
    pre_order(&dirs, 0, &mut dirs_pre);
    let mut dirs_post: Vec<usize> = Vec::new();
    post_order(&dirs, 0, &mut dirs_post);

    let mut next = FIRST_DIR_INODE;
    for &d in &dirs_pre {
        dirs[d].inode = next;
        next += 1;
    }
    let mut file_order: Vec<usize> = Vec::new();
    for &d in &dirs_post {
        for &fi in &dirs[d].files {
            files[fi].inode = next;
            next += 1;
            file_order.push(fi);
        }
    }
    let first_file_inode = FIRST_DIR_INODE + dirs.len() as u32;

    // afid order: the sce_sys subtree (pre-order) first, then every other directory's
    // files in pre-order.
    let sce_sys = dirs_pre
        .iter()
        .copied()
        .find(|&d| dirs[d].path == "sce_sys");
    let mut afid_order: Vec<usize> = Vec::new();
    if let Some(sd) = sce_sys {
        let mut subtree: Vec<usize> = Vec::new();
        collect_files_pre(&dirs, sd, &mut subtree);
        afid_order.extend(subtree);
    }
    for &d in &dirs_pre {
        if Some(d) == sce_sys || is_under(&dirs, d, sce_sys) {
            continue;
        }
        afid_order.extend(dirs[d].files.iter().copied());
    }
    // The keystone opens the layout whatever the tree order says. Sony's own packages carry it
    // at logical offset 0, and the reference engine's do too.
    if let Some(pos) = afid_order.iter().position(|&fi| files[fi].path == KEYSTONE) {
        let keystone = afid_order.remove(pos);
        afid_order.insert(0, keystone);
    }
    for (afid, &fi) in afid_order.iter().enumerate() {
        files[fi].afid = afid as u32;
    }

    // The mount packs the files in afid order; the image gives each one a 64 KiB boundary of
    // its own, so the two cursors diverge as soon as a file is not a whole number of units.
    let mut cursor = 0u64;
    let mut on_disk = 0u64;
    let mut starts = (0u64, 0u32); // (ublock, non-empty files started in it)
    let mut last_start: Option<u64> = None;
    for &fi in &afid_order {
        // A block record carries its start only to 16 bytes, so two files starting in one
        // 16-byte unit read back as one start: the console takes the second for a wrap into the
        // next 256 KiB window. Measured on a FW 5.10 Phat: nine such pairs in Minecraft gave
        // status 0x80010022 on each and an image nine windows too long, and the mount failed.
        // Spider-Man 2 has none. Moving the next file up by at most 15 bytes costs nothing.
        if spread {
            if let Some(prev) = last_start {
                if cursor / 16 == prev / 16 {
                    cursor = (prev / 16 + 1) * 16;
                }
            }
        }
        last_start = Some(cursor);
        if starts.0 != cursor / UBLOCK {
            starts = (cursor / UBLOCK, 0);
        }
        if files[fi].size > 0 {
            starts.1 += 1;
        }
        files[fi].logical_offset = cursor;
        files[fi].on_disk_offset = on_disk;
        files[fi].parent_inode = dirs[parent_of(&dirs, &files[fi].path)?].inode;
        cursor += files[fi].size;
        on_disk = (on_disk + files[fi].size).next_multiple_of(FILE_ALIGN);
        if spread
            && files[fi].size > 0
            && starts.1 >= MAX_STARTS_PER_UBLOCK
            && cursor / UBLOCK == starts.0
        {
            // The gap follows a file with bytes, so an empty file never owns one, and the
            // logical space stays inside the image's own extent, which sets the metadata base.
            let next = cursor.next_multiple_of(UBLOCK);
            if next <= on_disk {
                cursor = next;
            }
        }
    }
    let data_end = on_disk;

    // Directory entries with their byte offsets.
    let mut planned_dirs = Vec::with_capacity(dirs_pre.len());
    for &d in &dirs_pre {
        let inode = dirs[d].inode;
        let path = dirs[d].path.clone();
        let is_root = dirs[d].parent.is_none();
        let parent_inode = dirs[d].parent.map(|p| dirs[p].inode as i32).unwrap_or(-1);
        let dirent_offset = dirs[d].dirent_offset;
        let subdirs = dirs[d].subdirs.clone();
        let dir_files = dirs[d].files.clone();

        let mut dirents: Vec<(String, u32, i8)> = Vec::new();
        dirents.push((".".to_string(), inode, DIRENT_DOT));
        let up = if is_root { inode } else { parent_inode as u32 };
        dirents.push(("..".to_string(), up, DIRENT_DOTDOT));
        for &sd in &subdirs {
            dirents.push((dirs[sd].name.clone(), dirs[sd].inode, DIRENT_DIR));
        }
        for &fi in &dir_files {
            dirents.push((
                file_name(&files[fi].path).to_string(),
                files[fi].inode,
                DIRENT_FILE,
            ));
        }

        // Offsets, then push each child's offset back into its record.
        let mut offset = 0i32;
        let mut file_offsets: Vec<(usize, i32)> = Vec::new();
        let mut dir_offsets: Vec<(usize, i32)> = Vec::new();
        let mut di = 0usize;
        for (name, _, kind) in &dirents {
            match *kind {
                DIRENT_DIR => {
                    dir_offsets.push((subdirs[di], offset));
                    di += 1;
                }
                DIRENT_FILE => {
                    let ordinal = file_offsets.len();
                    file_offsets.push((dir_files[ordinal], offset));
                }
                _ => {}
            }
            offset += dirent_size(name);
        }
        for (fi, at) in &file_offsets {
            files[*fi].dirent_offset = *at;
        }
        for (sd, at) in &dir_offsets {
            dirs[*sd].dirent_offset = *at;
        }

        // The Unix rule PFS follows: `.` plus the parent's entry, plus one more for uroot,
        // whose parent is itself (the outer template's uroot shows nlink 3 with no
        // sub-directories).
        let nlink = 2 + subdirs.len() as u16 + u16::from(is_root);
        dirs[d].nlink = nlink;
        planned_dirs.push(PlannedDir {
            path,
            inode,
            parent_inode,
            dirent_offset,
            nlink,
            dirents,
        });
    }

    // Everything downstream consumes `files` in inode order, so reorder it here and remap
    // the afid indices onto the new positions.
    let mut position = vec![0usize; files.len()];
    for (new, &old) in file_order.iter().enumerate() {
        position[old] = new;
    }
    let files: Vec<PlannedFile> = file_order.iter().map(|&old| files[old].clone()).collect();
    let afid_order: Vec<usize> = afid_order.iter().map(|&old| position[old]).collect();

    // Flat-path tables and the afid table.
    let mut flt_inode: Vec<(u64, u64)> = Vec::new();
    for d in planned_dirs.iter().skip(1) {
        flt_inode.push((
            flt::hash_path(&d.path),
            flt::pack_inode_entry(d.inode, true, false, 0),
        ));
    }
    let mut flt_apr: Vec<(u64, u64)> = Vec::new();
    for f in &files {
        let apr = !f.sce_sys;
        flt_inode.push((
            flt::hash_path(&f.path),
            flt::pack_inode_entry(f.inode, false, !apr, f.afid),
        ));
        if apr {
            flt_apr.push((flt::hash_path(&f.path), flt::pack_apr_entry(f.size, f.afid)));
        }
    }
    let mut afid_to_ino: Vec<i32> = Vec::with_capacity(afid_order.len() + 3);
    afid_to_ino.push(first_file_inode as i32);
    for &fi in &afid_order {
        afid_to_ino.push(files[fi].inode as i32);
    }
    afid_to_ino.push(-1);
    afid_to_ino.push(-1);

    // Geometry. Every metadata structure is block-aligned at its start and runs on into as many
    // blocks as its bytes need: a real title's inode table, afid table and large directories all
    // outgrow a single 64 KiB block (Minecraft alone has 37k inodes). The region is one
    // superblock block, the inode table, the super-root dirents, both flat-path tables, the afid
    // table, one dirent stream per directory, then a guard block.
    // A compressed image has no on-disk layout: every reference places the metadata 4 MiB past
    // the end of the file data, rounded down to a ublock — Spider-Man 2 (0x3f68d373fe ->
    // 0x3f69100000), the Web Browser (0xa626 -> 0x400000) and LibProsperoPkg (0x5736f513 ->
    // 0x57740000) all fit. Placing it after the 64 KiB-per-file on-disk extent instead put
    // Minecraft's inner superblock 1.7 GB past its data.
    let meta_base = if spread {
        let logical_end = files
            .iter()
            .map(|f| f.logical_offset + f.size)
            .max()
            .unwrap_or(0);
        (logical_end + COMPRESSED_META_GAP) / META_ALIGN * META_ALIGN
    } else {
        (data_end + BLOCK).div_ceil(META_ALIGN) * META_ALIGN
    };
    let super_root = super_root_dirents();
    let inode_table_bytes =
        ((4 + planned_dirs.len() + files.len()) * crate::inner::INODE_LEN) as u64;
    let flt_bytes = flt::write(&flt_inode).len() as u64;
    let flt_apr_bytes = flt::write(&flt_apr).len() as u64;
    let afid_bytes = (afid_to_ino.len() * 4) as u64;
    let pad = |bytes: u64| bytes.div_ceil(BLOCK) * BLOCK;

    let mut at = meta_base + BLOCK;
    let mut layout = MetadataLayout {
        inode_table: (at, inode_table_bytes),
        ..Default::default()
    };
    at += pad(inode_table_bytes);
    let place = |at: &mut u64, bytes: u64| {
        let span = (*at, bytes);
        *at += pad(bytes);
        span
    };
    layout.super_root = place(
        &mut at,
        crate::inner::dirents_bytes(&super_root).len() as u64,
    );
    layout.flt = place(&mut at, flt_bytes);
    layout.flt_apr = place(&mut at, flt_apr_bytes);
    layout.afid = place(&mut at, afid_bytes);
    layout.dirs = planned_dirs
        .iter()
        .map(|d| {
            place(
                &mut at,
                crate::inner::dirents_bytes(&d.dirents).len() as u64,
            )
        })
        .collect();
    let metadata_blocks = (at - meta_base) / BLOCK + 1;
    let ndblock = meta_base / BLOCK + metadata_blocks;
    layout.blocks = metadata_blocks;

    let content_inodes = files.len() as u32 + dirs.len() as u32 - 1;
    let app_file_count = files
        .iter()
        .filter(|f| !f.sce_sys && file_name(&f.path) != PFS_VERSION_DAT)
        .count() as u32;

    Ok(Plan {
        dirs: planned_dirs,
        files,
        afid_order,
        flt_inode,
        flt_apr,
        afid_to_ino,
        first_file_inode,
        data_end,
        meta_base,
        ndblock,
        metadata_blocks,
        metadata: layout,
        content_inodes,
        app_file_count,
    })
}

fn is_under(dirs: &[DirNode], mut d: usize, ancestor: Option<usize>) -> bool {
    let Some(ancestor) = ancestor else {
        return false;
    };
    loop {
        if d == ancestor {
            return true;
        }
        match dirs[d].parent {
            Some(p) => d = p,
            None => return false,
        }
    }
}

fn collect_files_pre(dirs: &[DirNode], from: usize, out: &mut Vec<usize>) {
    out.extend(dirs[from].files.iter().copied());
    for &d in &dirs[from].subdirs {
        collect_files_pre(dirs, d, out);
    }
}

fn parent_of(dirs: &[DirNode], path: &str) -> Result<usize> {
    let parent = match path.rsplit_once('/') {
        Some((d, _)) => d,
        None => "",
    };
    dirs.iter()
        .position(|d| d.path == parent)
        .ok_or_else(|| crate::Error::Format(format!("no directory for {path}")))
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map(|(_, n)| n).unwrap_or(path)
}

/// A dirent's size: the 16-byte header, the name, padded to 8 bytes.
pub fn dirent_size(name: &str) -> i32 {
    let raw = name.len() as i32 + 17;
    (raw + 7) / 8 * 8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(paths: &[(&str, u64)]) -> Vec<SourceFile> {
        paths
            .iter()
            .map(|(p, s)| SourceFile {
                path: (*p).to_string(),
                size: *s,
            })
            .collect()
    }

    /// The real webbrowser sample's inner file sizes, recovered from its naps fidx offsets
    /// (96, 12752, 10, 14942, 14734 at offsets 0, 0x60, 0x3230, 0x323A, 0x6C98). With its
    /// order — keystone and one `sce_sys` file first, then three uroot files — the plan
    /// must land on its data-end offset and its counts.
    #[test]
    fn reproduces_the_websample_geometry() {
        let plan = build(&src(&[
            ("sce_sys/keystone", 96),
            ("sce_sys/about/right.sprx", 12752),
            ("00.dat", 10),
            ("m.dat", 14942),
            ("z.dat", 14734),
        ]))
        .unwrap();
        // The data region ends where the last file lands on disk, not where the mount packs it:
        // every file opens on a 64 KiB boundary of its own.
        assert_eq!(plan.data_end, 0x50000);
        assert_eq!(plan.dirs.len(), 3);
        assert_eq!(plan.metadata_blocks, 10);
        assert_eq!(plan.content_inodes, 7);
        assert_eq!(plan.app_file_count, 3);
        // afid order: sce_sys subtree first, then the uroot files, name-ordinal.
        let offsets: Vec<u64> = plan
            .afid_order
            .iter()
            .map(|&fi| plan.files[fi].logical_offset)
            .collect();
        assert_eq!(offsets, vec![0, 96, 12848, 12858, 27800]);
        // The same files on disk, each at its own boundary.
        let on_disk: Vec<u64> = plan
            .afid_order
            .iter()
            .map(|&fi| plan.files[fi].on_disk_offset)
            .collect();
        assert_eq!(on_disk, vec![0, 0x10000, 0x20000, 0x30000, 0x40000]);
    }

    /// A compressed image spreads a run of tiny files so no ublock opens more than the layout
    /// descriptor's one-byte deltas can index, starts every file in a 16-byte unit of its own,
    /// never lets an empty file own a ublock gap, and leaves the packed plan untouched.
    #[test]
    fn spreading_caps_file_starts_per_ublock() {
        let mut paths: Vec<(String, u64)> = (0..200).map(|i| (format!("d/{i:03}"), 4000)).collect();
        // An empty file after every 16th, where each gap opens in name order.
        for i in (15..200).step_by(16) {
            paths.push((format!("d/{i:03}e"), 0));
        }
        let owned: Vec<(&str, u64)> = paths.iter().map(|(p, s)| (p.as_str(), *s)).collect();
        let input = src(&owned);
        let spread = build_with(&input, true).unwrap();
        let mut per_ublock: BTreeMap<u64, u32> = BTreeMap::new();
        let order: Vec<&PlannedFile> = spread
            .afid_order
            .iter()
            .map(|&fi| &spread.files[fi])
            .collect();
        for (i, f) in order.iter().enumerate() {
            assert!(f.logical_offset <= f.on_disk_offset);
            if let Some(next) = order.get(i + 1) {
                assert!(
                    next.logical_offset >= f.logical_offset + f.size,
                    "{} overlaps",
                    f.path
                );
                // Every file starts in a 16-byte unit of its own, so an empty file owns at
                // most the step to the next unit and never a ublock gap.
                assert_ne!(
                    next.logical_offset / 16,
                    f.logical_offset / 16,
                    "{} and {} share a 16-byte unit",
                    f.path,
                    next.path
                );
                if f.size == 0 {
                    assert!(
                        next.logical_offset - f.logical_offset <= 16,
                        "an empty file owns a gap"
                    );
                }
            }
            if f.size > 0 {
                *per_ublock.entry(f.logical_offset / UBLOCK).or_default() += 1;
            }
        }
        assert!(per_ublock.values().all(|&n| n <= MAX_STARTS_PER_UBLOCK));
        assert!(per_ublock.len() > 1);
        let packed = build(&input).unwrap();
        let mut at = 0;
        for &fi in &packed.afid_order {
            assert_eq!(packed.files[fi].logical_offset, at);
            at += packed.files[fi].size;
        }
    }

    /// Executables carry the module flag (0x50) and everything else the blob flag (0x30), as on
    /// Spider-Man 2; the keystone we generate is never a module.
    #[test]
    fn executables_are_flagged_as_modules() {
        let mut plan = build(&src(&[
            ("eboot.bin", 64),
            ("sce_sys/about/right.sprx", 64),
            ("data/level.pak", 64),
        ]))
        .unwrap();
        let heads: BTreeMap<&str, [u8; 4]> = [
            ("eboot.bin", crate::source::magic::SELF_PS5),
            ("sce_sys/about/right.sprx", crate::source::magic::SELF_PS4),
            ("data/level.pak", *b"PAK1"),
        ]
        .into_iter()
        .collect();
        plan.mark_modules(|p| heads.get(p).is_some_and(|h| is_module_header(h)));
        let flags = |p: &str| {
            plan.files
                .iter()
                .find(|f| f.path == p)
                .unwrap()
                .inode_flags()
        };
        assert_eq!(flags("eboot.bin"), 0x50);
        assert_eq!(flags("sce_sys/about/right.sprx"), 0x0002_0050);
        assert_eq!(flags("data/level.pak"), 0x30);
        assert_eq!(flags(KEYSTONE), 0x0002_0030);
        assert!(!is_module_header(&[0x7F, b'E']));
    }

    /// `pfs-version.dat` is a marker, not app payload: the sample's three uroot files
    /// count as two at the header's `0xF0`.
    #[test]
    fn pfs_version_dat_is_not_app_payload() {
        let plan = build(&src(&[("eboot.bin", 10), ("pfs-version.dat", 10)])).unwrap();
        assert_eq!(plan.app_file_count, 1);
    }

    #[test]
    fn keystone_is_generated_when_absent() {
        let plan = build(&src(&[("eboot.bin", 100)])).unwrap();
        let keystone = plan.files.iter().find(|f| f.path == KEYSTONE).unwrap();
        assert!(keystone.generated);
        assert_eq!(keystone.size, KEYSTONE_LEN);
        assert_eq!(plan.files.len(), 2);
        assert_eq!(plan.afid_order.len(), 2);
        assert_eq!(plan.flt_apr.len(), 1); // eboot.bin only, keystone is sce_sys
    }

    #[test]
    fn inodes_follow_the_measured_order() {
        let plan = build(&src(&[
            ("eboot.bin", 10),
            ("sce_sys/param.json", 10),
            ("sce_sys/about/right.sprx", 10),
            ("data/a.bin", 10),
            ("data/b.bin", 10),
        ]))
        .unwrap();
        // 0..3 reserved, 4 uroot, then dirs pre-order (data, sce_sys, sce_sys/about).
        let dir_inodes: Vec<(String, u32)> = plan
            .dirs
            .iter()
            .map(|d| (d.path.clone(), d.inode))
            .collect();
        assert_eq!(
            dir_inodes,
            vec![
                (String::new(), 4),
                ("data".to_string(), 5),
                ("sce_sys".to_string(), 6),
                ("sce_sys/about".to_string(), 7),
            ]
        );
        // Files: directories post-order (deepest first, siblings ordinal), files ordinal
        // by name within each directory. A keystone is generated (the fixture has none).
        let file_inodes: Vec<(String, u32)> = plan
            .files
            .iter()
            .map(|f| (f.path.clone(), f.inode))
            .collect();
        assert_eq!(
            file_inodes,
            vec![
                ("data/a.bin".to_string(), 8),
                ("data/b.bin".to_string(), 9),
                ("sce_sys/about/right.sprx".to_string(), 10),
                ("sce_sys/keystone".to_string(), 11),
                ("sce_sys/param.json".to_string(), 12),
                ("eboot.bin".to_string(), 13),
            ]
        );
        assert_eq!(plan.first_file_inode, 8);
        // afids: the sce_sys subtree pre-order first (its own files, then its sub-directory),
        // then the other directories in pre-order. The leading value is the first file inode.
        assert_eq!(plan.afid_to_ino, vec![8, 11, 12, 10, 13, 8, 9, -1, -1]);
    }

    #[test]
    fn dirent_offsets_are_eight_byte_packed() {
        assert_eq!(dirent_size("."), 24);
        assert_eq!(dirent_size(".."), 24);
        assert_eq!(dirent_size("eboot.bin"), 32);
    }
}

#[cfg(test)]
mod afid_tests {
    use super::*;
    use crate::source::SourceFile;

    /// The streaming writer files its per-file digests by afid, so every file must have
    /// exactly one, and the afid order must cover them all.
    #[test]
    fn every_file_gets_a_distinct_afid() {
        let files: Vec<SourceFile> = [
            "eboot.bin",
            "data/one.bin",
            "sce_sys/param.json",
            "sce_sys/icon0.png",
            "sce_sys/about/right.sprx",
        ]
        .iter()
        .map(|p| SourceFile {
            path: (*p).to_string(),
            size: 10,
        })
        .collect();
        let plan = build(&files).unwrap();
        assert_eq!(
            plan.afid_order.len(),
            plan.files.len(),
            "afid order covers {} of {} files",
            plan.afid_order.len(),
            plan.files.len()
        );
        for (afid, &fi) in plan.afid_order.iter().enumerate() {
            assert_eq!(
                plan.files[fi].afid as usize, afid,
                "{}",
                plan.files[fi].path
            );
        }
        let mut seen: Vec<u32> = plan.files.iter().map(|f| f.afid).collect();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), count, "two files share an afid: {seen:?}");
    }
}
