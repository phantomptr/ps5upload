//! The lowest firmware a converted game can run on, from its executables' SDK stamps.
//!
//! A backport rewrites each module's SDK pair down (usually to the FW 4 pair) and leaves
//! `param.json` declaring the firmware the game was built for, so a package that copies the
//! declared value refuses to install on the very consoles the backport targets. Spider-Man 2
//! declares 10.20 while every module carries the FW 4 pair; converted with 5.10 declared
//! instead, it installs and plays on a FW 5.10 console.

use ps5upload_core::fakelibs::{is_known_sdk_pair, param_site_from_header, sdk_pair, sdk_pair_at};
use ps5upload_fpkg::source::SourceTree;

/// Headers are small; the param segment is placed from the first 64 KiB.
const HEADER: usize = 0x1_0000;
/// A module the header cannot place is read whole and scanned when it is no bigger than this.
/// Libraries are a few megabytes; an eboot this large is placed from its header.
const SCAN_LIMIT: u64 = 64 * 1024 * 1024;

fn is_executable(path: &str) -> bool {
    path == "eboot.bin"
        || (path.starts_with("sce_module/") && path.to_ascii_lowercase().ends_with(".prx"))
}

/// A module's `(ps4, ps5)` pair: placed from its header and read at the param site, or, when
/// the header does not place it (Spider-Man 2's libc.prx), found in the whole of a small
/// module by the param block's magic. `None` when neither finds it or it is not a pair Sony
/// ships.
fn module_pair(tree: &mut dyn SourceTree, path: &str) -> Option<(u32, u32)> {
    let head = tree.read_range(path, 0, HEADER).ok()?;
    let placed = param_site_from_header(&head).and_then(|site| {
        let chunk = tree.read_range(path, site as u64, 0x18).ok()?;
        sdk_pair_at(&chunk)
    });
    let pair = match placed {
        Some(pair) => pair,
        None => {
            let size = tree.files().iter().find(|f| f.path == path)?.size;
            if size > SCAN_LIMIT {
                return None;
            }
            sdk_pair(&tree.read(path).ok()?)?
        }
    };
    is_known_sdk_pair(pair).then_some(pair)
}

/// `(major, minor)` of a PS5 SDK word: the top byte is the firmware major in BCD, the next
/// byte the minor.
fn firmware_of(ps5: u32) -> Option<(u32, u32)> {
    let bcd = |b: u32| format!("{b:x}").parse::<u32>().ok();
    Some((bcd((ps5 >> 24) & 0xFF)?, bcd((ps5 >> 16) & 0xFF)?))
}

/// "10.20" or "5.1" as `(major, minor)`, the minor as two digits.
fn parts(v: &str) -> Option<(u32, u32)> {
    let (a, b) = v.trim().split_once('.')?;
    Some((a.parse().ok()?, format!("{b:0<2}").parse().ok()?))
}

/// The firmware the game needs, as "M.mm", when every executable (`eboot.bin` and each
/// `sce_module/*.prx`) carries a known SDK pair and the highest of them is below the firmware
/// `declared` in `param.json` (as the inspection reports it, e.g. "10.20"). `None` otherwise:
/// the declared value stands, so an unreadable module can never lower it.
pub(crate) fn min_firmware(tree: &mut dyn SourceTree, declared: Option<&str>) -> Option<String> {
    let declared = parts(declared?)?;
    let executables: Vec<String> = tree
        .files()
        .iter()
        .filter(|f| is_executable(&f.path))
        .map(|f| f.path.clone())
        .collect();
    if !executables.iter().any(|p| p == "eboot.bin") {
        return None;
    }
    let mut highest = 0u32;
    for path in &executables {
        let (_, ps5) = module_pair(tree, path)?;
        highest = highest.max(ps5);
    }
    let needed = firmware_of(highest)?;
    (needed < declared).then(|| format!("{}.{:02}", needed.0, needed.1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps5upload_fpkg::source::{SourceFile, SourceTree};

    /// A raw ELF with one PT_SCE_PROCPARAM segment whose param block carries `pair`.
    fn module(pair: Option<(u32, u32)>) -> Vec<u8> {
        let mut e = vec![0u8; 0x1000];
        e[0..4].copy_from_slice(b"\x7fELF");
        e[0x20..0x28].copy_from_slice(&0x40u64.to_le_bytes()); // e_phoff
        e[0x36..0x38].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        e[0x38..0x3A].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
        let ph = 0x40;
        e[ph..ph + 4].copy_from_slice(&0x6100_0001u32.to_le_bytes()); // PT_SCE_PROCPARAM
        e[ph + 8..ph + 16].copy_from_slice(&0x800u64.to_le_bytes()); // p_offset
        e[ph + 0x20..ph + 0x28].copy_from_slice(&0x40u64.to_le_bytes()); // p_filesz
        if let Some((ps4, ps5)) = pair {
            e[0x808..0x80C].copy_from_slice(&0x4942_524Fu32.to_le_bytes()); // "ORBI"
            e[0x810..0x814].copy_from_slice(&ps4.to_le_bytes());
            e[0x814..0x818].copy_from_slice(&ps5.to_le_bytes());
        }
        e
    }

    struct Tree(Vec<(String, Vec<u8>)>, Vec<SourceFile>);

    fn tree(files: Vec<(&str, Vec<u8>)>) -> Tree {
        let listed = files
            .iter()
            .map(|(p, d)| SourceFile {
                path: p.to_string(),
                size: d.len() as u64,
            })
            .collect();
        Tree(
            files.into_iter().map(|(p, d)| (p.to_string(), d)).collect(),
            listed,
        )
    }

    impl SourceTree for Tree {
        fn files(&self) -> &[SourceFile] {
            &self.1
        }
        fn read(&mut self, path: &str) -> ps5upload_fpkg::Result<Vec<u8>> {
            self.0
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, d)| d.clone())
                .ok_or_else(|| ps5upload_fpkg::Error::Format(format!("no {path}")))
        }
        fn describe(&self) -> String {
            "test".into()
        }
    }

    /// A module whose header does not place its param segment (Spider-Man 2's libc.prx):
    /// the block is found only by its magic, preceded by its 0x20 size word.
    fn scan_only_module(pair: (u32, u32)) -> Vec<u8> {
        let mut e = vec![0u8; 0x1000];
        e[0..4].copy_from_slice(b"\x7fELF");
        e[0x20..0x28].copy_from_slice(&0x40u64.to_le_bytes());
        e[0x36..0x38].copy_from_slice(&56u16.to_le_bytes());
        e[0x800..0x804].copy_from_slice(&0x20u32.to_le_bytes());
        e[0x808..0x80C].copy_from_slice(&0x4942_524Fu32.to_le_bytes());
        e[0x810..0x814].copy_from_slice(&pair.0.to_le_bytes());
        e[0x814..0x818].copy_from_slice(&pair.1.to_le_bytes());
        e
    }

    const FW4: (u32, u32) = (0x0904_0001, 0x0400_0031);
    const FW9: (u32, u32) = (0x1159_0001, 0x0900_0040);

    #[test]
    fn a_backported_title_needs_its_highest_module_pair() {
        let mut t = tree(vec![
            ("eboot.bin", module(Some(FW4))),
            ("sce_module/libc.prx", module(Some(FW4))),
        ]);
        assert_eq!(min_firmware(&mut t, Some("10.20")).as_deref(), Some("4.00"));
        let mut t = tree(vec![
            ("eboot.bin", module(Some(FW4))),
            ("sce_module/libc.prx", module(Some(FW9))),
        ]);
        assert_eq!(min_firmware(&mut t, Some("10.20")).as_deref(), Some("9.00"));
    }

    #[test]
    fn never_above_what_the_game_declares() {
        let mut t = tree(vec![("eboot.bin", module(Some(FW9)))]);
        assert_eq!(min_firmware(&mut t, Some("5.10")), None);
        assert_eq!(min_firmware(&mut t, None), None);
    }

    /// Measured on Spider-Man 2: its libc.prx cannot be placed from the header, and a module
    /// the header reader gives up on still counts when its param block can be found.
    #[test]
    fn a_module_found_only_by_scanning_still_counts() {
        let mut t = tree(vec![
            ("eboot.bin", module(Some(FW4))),
            ("sce_module/libc.prx", scan_only_module(FW4)),
        ]);
        assert_eq!(min_firmware(&mut t, Some("10.20")).as_deref(), Some("4.00"));
    }

    /// An unreadable pair keeps the declared value; it never lowers it.
    #[test]
    fn an_unreadable_module_keeps_the_declared_value() {
        let mut t = tree(vec![
            ("eboot.bin", module(Some(FW4))),
            ("sce_module/libc.prx", module(None)),
        ]);
        assert_eq!(min_firmware(&mut t, Some("10.20")), None);
        let mut t = tree(vec![("eboot.bin", vec![0x53, 0x43, 0x45, 0x00, 1, 2, 3])]);
        assert_eq!(min_firmware(&mut t, Some("10.20")), None);
        let mut t = tree(vec![("sce_module/libc.prx", module(Some(FW4)))]);
        assert_eq!(min_firmware(&mut t, Some("10.20")), None, "no eboot");
    }
}
