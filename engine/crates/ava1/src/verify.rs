//! Verification groups (SPEC.md §13). A file is hashed in 1 MiB groups; each group's
//! BLAKE3 chaining value is computed where the bytes are, and the file's root is merged
//! from them, so a resume never re-reads durable data to finish a hash.
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use blake3::hazmat::{merge_subtrees_non_root, merge_subtrees_root, HasherExt, Mode};

/// One verification group in bytes: 1 MiB (`crate::gen::GROUP_SHIFT` = 20).
pub const GROUP: u64 = 1 << crate::gen::GROUP_SHIFT;

/// The number of groups a file of `size` bytes covers: `ceil(size / GROUP)`
/// (0 for an empty file).
pub fn groups(size: u64) -> u64 {
    size.div_ceil(GROUP)
}

/// The chaining value of group `index` (non-root). Only a file of two or more groups
/// has group CVs; a smaller file's root is `blake3::hash` of its bytes.
pub fn group_cv(data: &[u8], index: u64) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.set_input_offset(index * GROUP);
    h.update(data);
    h.finalize_non_root()
}

fn left_len(n: usize) -> usize {
    let mut p = 1;
    while p * 2 < n {
        p *= 2;
    }
    p
}

fn merge(cvs: &[[u8; 32]]) -> [u8; 32] {
    if cvs.len() == 1 {
        return cvs[0];
    }
    let l = left_len(cvs.len());
    merge_subtrees_non_root(&merge(&cvs[..l]), &merge(&cvs[l..]), Mode::Hash)
}

/// The root of a file of `cvs.len()` >= 2 groups.
pub fn root_from_cvs(cvs: &[[u8; 32]]) -> [u8; 32] {
    assert!(cvs.len() >= 2, "a file of one group has no group CVs");
    let l = left_len(cvs.len());
    *merge_subtrees_root(&merge(&cvs[..l]), &merge(&cvs[l..]), Mode::Hash).as_bytes()
}

/// Collects a file's group hashes in any order; yields its root when complete.
pub struct FileHasher {
    size: u64,
    cvs: Vec<Option<[u8; 32]>>,
    single: Option<[u8; 32]>,
}

impl FileHasher {
    pub fn new(size: u64) -> Self {
        let n = groups(size);
        Self {
            size,
            cvs: if n >= 2 {
                vec![None; n as usize]
            } else {
                Vec::new()
            },
            single: (size == 0).then_some(*blake3::hash(&[]).as_bytes()),
        }
    }

    /// `data` is all of group `index` (shorter only for the last group).
    pub fn add_group(&mut self, index: u64, data: &[u8]) {
        if self.cvs.is_empty() {
            self.single = Some(*blake3::hash(data).as_bytes());
        } else {
            self.cvs[index as usize] = Some(group_cv(data, index));
        }
    }

    pub fn set_cv(&mut self, index: u64, cv: [u8; 32]) {
        if let Some(slot) = self.cvs.get_mut(index as usize) {
            *slot = Some(cv);
        }
    }

    pub fn cv(&self, index: u64) -> Option<[u8; 32]> {
        self.cvs.get(index as usize).copied().flatten()
    }

    pub fn missing(&self) -> Vec<u64> {
        if self.cvs.is_empty() {
            return if self.single.is_some() {
                Vec::new()
            } else {
                vec![0]
            };
        }
        (0..self.cvs.len() as u64)
            .filter(|i| self.cvs[*i as usize].is_none())
            .collect()
    }

    pub fn root(&self) -> Option<[u8; 32]> {
        if self.cvs.is_empty() {
            return self.single;
        }
        let all: Option<Vec<[u8; 32]>> = self.cvs.iter().copied().collect();
        all.map(|v| root_from_cvs(&v))
    }

    pub fn size(&self) -> u64 {
        self.size
    }
}

/// The most groups one outboard holds (4 TiB of file, 128 MiB of CVs). A larger declared size
/// is refused rather than allocated.
pub const MAX_OUTBOARD_GROUPS: u64 = 1 << 22;

/// Group CVs on disk: slot i at byte i*32; an all-zero slot is "not yet known".
///
/// Writes are crash-safe: `put` stages a 32-byte slot into a shadow file and `sync`
/// renames the shadow over the outboard, so a failed partial write or a crash between
/// syncs can never tear an existing file — the outboard holds either the previous
/// complete image or the new complete one. A crash that loses the rename costs only the
/// last batch's CVs, which §13.4 re-hashes at resume. `put` itself is one 32-byte write
/// (plus one full-image copy per batch), so feeding a group's CV is O(1).
///
/// One instance per path at a time: each rebuilds the whole image from the slots it read
/// at open, so a second instance's `sync` would drop the first's updates.
pub struct Outboard {
    path: PathBuf,
    f: File,
    shadow: Option<File>,
    slots: Vec<Option<[u8; 32]>>,
    /// A `put` failed part-way through a slot: the shadow no longer holds a complete
    /// image, so every later `put` and `sync` refuses rather than rename a torn file over
    /// the outboard. Reopening recovers — the outboard itself was never written.
    poisoned: bool,
}

impl Outboard {
    pub fn open(path: &Path, groups: u64) -> io::Result<Self> {
        // The group count comes from a size a peer declared: refuse an absurd one before
        // allocating `groups * 32` bytes for it.
        if groups > MAX_OUTBOARD_GROUPS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{groups} groups is more than an outboard holds"),
            ));
        }
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let need = groups * 32;
        // A tail that is not a whole slot (a crash mid-write, a manual truncation) is not
        // an error: the slots it covers are simply unknown, and the next `sync` rewrites
        // the file whole.
        let len = f.metadata()?.len().min(need) & !31;
        let mut raw = vec![0u8; need as usize];
        if len > 0 {
            read_exact_at(&f, &mut raw[..len as usize], 0)?;
        }
        let slots = raw
            .chunks(32)
            .map(|c| {
                let a: [u8; 32] = c.try_into().unwrap();
                (a != [0; 32]).then_some(a)
            })
            .collect();
        Ok(Self {
            path: path.to_owned(),
            f,
            shadow: None,
            slots,
            poisoned: false,
        })
    }

    pub fn get(&self, i: u64) -> Option<[u8; 32]> {
        self.slots.get(i as usize).copied().flatten()
    }

    pub fn put(&mut self, i: u64, cv: &[u8; 32]) -> io::Result<()> {
        if self.poisoned {
            return Err(io::Error::other(
                "the outboard's shadow was torn by a failed put",
            ));
        }
        self.slots[i as usize] = Some(*cv);
        match self.shadow.as_mut() {
            Some(shadow) => match write_all_at(shadow, cv, i * 32) {
                Ok(()) => Ok(()),
                Err(e) => {
                    self.poisoned = true;
                    Err(e)
                }
            },
            None => {
                let mut img = vec![0u8; self.slots.len() * 32];
                for (s, slot) in img.as_chunks_mut::<32>().0.iter_mut().zip(&self.slots) {
                    if let Some(cv) = slot {
                        s.copy_from_slice(cv);
                    }
                }
                let sf = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(tmp_path(&self.path))?;
                write_all_at(&sf, &img, 0)?;
                self.shadow = Some(sf);
                Ok(())
            }
        }
    }

    pub fn sync(&mut self) -> io::Result<()> {
        if self.poisoned {
            return Err(io::Error::other(
                "the outboard's shadow was torn by a failed put",
            ));
        }
        if let Some(shadow) = self.shadow.take() {
            shadow.sync_data()?;
            std::fs::rename(tmp_path(&self.path), &self.path)?;
            self.f = shadow;
        } else {
            self.f.sync_data()?;
        }
        Ok(())
    }
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

/// Read exactly `buf.len()` bytes at `off` (shared with the receiver).
pub(crate) fn read_exact_at(f: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::read_exact_at(f, buf, off)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut buf = buf;
        let mut read = 0u64;
        while !buf.is_empty() {
            let n = f.seek_read(buf, off + read)?;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            read += n as u64;
            buf = &mut buf[n..];
        }
        Ok(())
    }
}

/// Write all of `buf` at `off` (shared with the receiver).
pub(crate) fn write_all_at(f: &File, buf: &[u8], off: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::write_all_at(f, buf, off)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut buf = buf;
        let mut written = 0u64;
        while !buf.is_empty() {
            let n = f.seek_write(buf, off + written)?;
            written += n as u64;
            buf = &buf[n..];
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absurd_declared_size_is_refused_before_it_allocates() {
        let d = std::env::temp_dir().join(format!("p5a-ob-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for groups in [MAX_OUTBOARD_GROUPS + 1, u64::MAX / 32 + 1, u64::MAX] {
            let e = Outboard::open(&d.join("x.ob"), groups)
                .err()
                .expect("refused");
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        }
        assert!(Outboard::open(&d.join("y.ob"), 1000).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 31 + 7) as u8).collect()
    }

    fn cvs_of(d: &[u8]) -> Vec<[u8; 32]> {
        d.chunks(GROUP as usize)
            .enumerate()
            .map(|(i, g)| group_cv(g, i as u64))
            .collect()
    }

    #[test]
    fn roots_from_group_cvs_equal_plain_blake3() {
        let g = GROUP as usize;
        for n in [
            g + 1,
            2 * g,
            2 * g + 1023,
            3 * g + 5000,
            5 * g,
            6 * g,
            7 * g + 7,
            7 * g + 1,
            8 * g + 1024,
            9 * g - 1,
            12 * g + 1,
        ] {
            let d = data(n);
            assert_eq!(
                root_from_cvs(&cvs_of(&d)),
                *blake3::hash(&d).as_bytes(),
                "{n}"
            );
        }
    }

    #[test]
    fn the_file_hasher_handles_every_size_class() {
        let g = GROUP as usize;
        for n in [0, 1, 1024, 1025, g - 1, g, g + 1, 3 * g + 17] {
            let d = data(n);
            let mut h = FileHasher::new(n as u64);
            // feed groups out of order
            let mut idx: Vec<usize> = (0..d.chunks(g).count()).collect();
            idx.reverse();
            for i in idx {
                let s = i * g;
                assert!(h.root().is_none() || n == 0);
                h.add_group(i as u64, &d[s..(s + g).min(n)]);
            }
            assert_eq!(h.root(), Some(*blake3::hash(&d).as_bytes()), "{n}");
            assert!(h.missing().is_empty());
        }
    }

    #[test]
    fn cvs_restored_from_an_outboard_finish_the_root_without_rereading() {
        let g = GROUP as usize;
        let d = data(4 * g + 3);
        let dir = std::env::temp_dir().join(format!("ava1-ob-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("1.ob");
        let _ = std::fs::remove_file(&p);
        {
            let mut ob = Outboard::open(&p, 5).unwrap();
            for (i, cv) in cvs_of(&d).iter().enumerate().take(3) {
                ob.put(i as u64, cv).unwrap();
            }
            ob.sync().unwrap();
        }
        let ob = Outboard::open(&p, 5).unwrap();
        let mut h = FileHasher::new(d.len() as u64);
        for i in 0..5 {
            if let Some(cv) = ob.get(i) {
                h.set_cv(i, cv);
            }
        }
        assert_eq!(h.missing(), vec![3, 4]);
        h.add_group(3, &d[3 * g..4 * g]);
        h.add_group(4, &d[4 * g..]);
        assert_eq!(h.root(), Some(*blake3::hash(&d).as_bytes()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_outboard_tolerates_a_torn_tail() {
        // A crash mid-write can leave a length that is not a whole number of slots; the
        // whole slots are still true and the rest is simply unknown. This used to panic.
        let dir = std::env::temp_dir().join(format!("ava1-ob-torn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.ob");
        let _ = std::fs::remove_file(&p);
        let mut raw = vec![0u8; 32 + 8];
        raw[..32].copy_from_slice(&[7u8; 32]);
        std::fs::write(&p, &raw).unwrap();
        let ob = Outboard::open(&p, 3).expect("a torn tail is not an error");
        assert_eq!(ob.get(0), Some([7u8; 32]));
        assert_eq!(ob.get(1), None);
        assert_eq!(ob.get(2), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
