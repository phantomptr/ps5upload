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

/// `metaBase` alignment: two 256 KiB ublocks, the granularity the NAPS u2c mapping
/// addresses. One further block is always left free so the block-info table has a home
/// after the data. The samples pad further (webbrowser's data ends at 0xA626 and its
/// metadata base is 0x400000); only the 256 KiB alignment is load-bearing here.
const META_ALIGN: u64 = 0x40000;

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

/// A file's placement in the inner image; `files` is in inode order after `build`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    pub path: String,
    pub size: u64,
    pub inode: u32,
    pub afid: u32,
    pub logical_offset: u64,
    pub parent_inode: u32,
    pub dirent_offset: i32,
    pub sce_sys: bool,
    /// True for the keystone this build generated.
    pub generated: bool,
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

    /// Each file's logical offset by afid.
    pub fn afid_offsets(&self) -> Vec<u64> {
        self.afid_order
            .iter()
            .map(|&fi| self.files[fi].logical_offset)
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
        FLAGS_DATA | FLAGS_BLOB | if self.sce_sys { FLAGS_SCE_SYS } else { 0 }
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

/// Plan the inner image and everything derived from it.
pub fn build(input: &[SourceFile]) -> Result<Plan> {
    let mut files: Vec<PlannedFile> = input
        .iter()
        .map(|f| PlannedFile {
            path: f.path.clone(),
            size: f.size,
            inode: 0,
            afid: 0,
            logical_offset: 0,
            parent_inode: 0,
            dirent_offset: -1,
            sce_sys: f.path.starts_with("sce_sys/"),
            generated: false,
        })
        .collect();
    if !files.iter().any(|f| f.path == KEYSTONE) {
        files.push(PlannedFile {
            path: KEYSTONE.to_string(),
            size: KEYSTONE_LEN,
            inode: 0,
            afid: 0,
            logical_offset: 0,
            parent_inode: 0,
            dirent_offset: -1,
            sce_sys: true,
            generated: true,
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
    for (afid, &fi) in afid_order.iter().enumerate() {
        files[fi].afid = afid as u32;
    }

    // Logical offsets, packed in afid order.
    let mut cursor = 0u64;
    for &fi in &afid_order {
        files[fi].logical_offset = cursor;
        files[fi].parent_inode = dirs[parent_of(&dirs, &files[fi].path)?].inode;
        cursor += files[fi].size;
    }
    let data_end = cursor;

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

    // Geometry: two blocks (superblock, inode table) + four content blocks (super-root
    // dirents, both tables, afid table) + one dirent block per directory + one trailing.
    let metadata_blocks = dirs.len() as u64 + 7;
    let meta_base = (data_end + BLOCK).div_ceil(META_ALIGN) * META_ALIGN;
    let ndblock = meta_base / BLOCK + metadata_blocks;

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
        assert_eq!(plan.data_end, 0xa626);
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
