//! Helpers shared by the fuzz targets.

pub mod mem_tree;
pub mod sparse;

use ps5upload_fpkg::source::SourceTree;

/// At most this many files are read back per input, at most `READ_CAP` bytes each: enough to
/// reach every data path (direct, indirect, FAT chain, compressed block) without letting one
/// input spend seconds copying.
pub const MAX_FILES: usize = 16;
pub const READ_CAP: usize = 256 * 1024;

/// Read the two ends of the first [`MAX_FILES`] files of an opened tree, the way every image
/// target exercises its reader once the metadata parsed.
pub fn read_back(tree: &mut dyn SourceTree) {
    let _ = tree.empty_dirs().len();
    let _ = tree.describe();
    let files: Vec<_> = tree.files().iter().take(MAX_FILES).cloned().collect();
    for f in files {
        let _ = tree.read_range(&f.path, 0, READ_CAP);
        let _ = tree.read_range(&f.path, f.size.saturating_sub(READ_CAP as u64), READ_CAP);
    }
}
