//! One move on the console (`/api/ps5/fs/move`). Blocking: call from `spawn_blocking`.
//!
//! A move is a rename when it can be one, because that is instant and never fails half way:
//! the payload refuses a cross-mount rename (it panics this kernel), and only then does the
//! console copy and delete. A destination that exists is never touched unless the caller asked
//! (`overwrite`, which the client only sends after the user pressed Replace).

use anyhow::{anyhow, Result};

/// What this module needs from the console. The real one is [`Console`]; the tests use a fake.
pub(crate) trait MoveOps {
    /// `rename(2)` on the console. `replace` false: an existing destination is refused.
    fn rename(&self, from: &str, to: &str, replace: bool) -> Result<()>;
    /// The kind `fs.stat` reports (`file`, `dir`, ...). `None`: the path is not there.
    fn kind(&self, path: &str) -> Result<Option<String>>;
    /// Whether the folder `dir` holds an entry spelled exactly `name`.
    fn has_exact_name(&self, dir: &str, name: &str) -> Result<bool>;
    /// The console-side move: copy to `to`, then delete `from` once the copy is known whole.
    /// With `overwrite`, colliding files are replaced and everything else in `to` stays.
    fn copy_then_delete(&self, from: &str, to: &str, overwrite: bool) -> Result<()>;
}

/// The refusal for a destination that is already there. The client keys on the token.
pub(crate) fn exists_refusal(to: &str) -> anyhow::Error {
    anyhow!("fs_move_exists: {to} already exists")
}

/// Whether an error is the "destination is already there" refusal of a move, from either path
/// (the rename's own word, or the copy the cross-mount fallback runs).
pub(crate) fn is_exists_refusal(message: &str) -> bool {
    message.contains("fs_move_exists") || message.contains("fs_copy_dest_exists")
}

/// `Foo` to `foo` in one folder. On a case-insensitive drive (exFAT) the destination "exists"
/// because it is the source under its new spelling.
fn is_case_only_rename(from: &str, to: &str) -> bool {
    from != to && from.to_lowercase() == to.to_lowercase()
}

fn split_parent(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some(("", name)) => ("/", name),
        Some((dir, name)) => (dir, name),
        None => ("", path),
    }
}

/// Moves `from` to `to`.
///
/// Without `overwrite` an existing destination is refused with `fs_move_exists`. The one
/// exception is a case-only rename on a case-insensitive drive, where nothing else has the name.
///
/// With `overwrite`, what the API promises is a merge: colliding files are replaced, and
/// anything else in the destination stays. A rename does that for a file over a file and for a
/// folder over an empty folder. It cannot put a folder onto a folder with files in it, so that
/// goes through the same console-side move as a cross-mount one. A file and a folder cannot
/// replace each other: refused, the same as the cross-mount path does.
pub(crate) fn run(ops: &impl MoveOps, from: &str, to: &str, overwrite: bool) -> Result<()> {
    let Err(e) = ops.rename(from, to, overwrite) else {
        return Ok(());
    };
    let msg = format!("{e:#}");
    if msg.contains("cross_mount") || msg.contains("EXDEV") {
        return ops.copy_then_delete(from, to, overwrite);
    }
    if msg.contains("fs_move_exists") {
        if is_case_only_rename(from, to) {
            let (dir, name) = split_parent(to);
            // A case-sensitive drive can hold both spellings: that one is a real collision.
            if !ops.has_exact_name(dir, name)? {
                return ops.rename(from, to, true);
            }
        }
        return Err(exists_refusal(to));
    }
    // The payload says only `fs_move_failed` for ENOTEMPTY, EISDIR and ENOTDIR.
    if overwrite && msg.contains("fs_move_failed") {
        let (src, dst) = (ops.kind(from)?, ops.kind(to)?);
        match (src.as_deref(), dst.as_deref()) {
            (Some("dir"), Some("dir")) => return ops.copy_then_delete(from, to, true),
            (Some(a), Some(b)) if a != b => return Err(exists_refusal(to)),
            _ => {}
        }
    }
    Err(e)
}

/// The console behind [`run`].
pub(crate) struct Console<'a> {
    pub addr: &'a str,
    pub op_id: u64,
    pub rename_timeout: std::time::Duration,
}

impl MoveOps for Console<'_> {
    fn rename(&self, from: &str, to: &str, replace: bool) -> Result<()> {
        ps5upload_core::fs_ops::fs_move_with_timeout(
            self.addr,
            from,
            to,
            replace,
            Some(self.rename_timeout),
        )
    }

    fn kind(&self, path: &str) -> Result<Option<String>> {
        match ps5upload_core::fs_ops::fs_stat(self.addr, path) {
            Ok(st) => Ok(Some(st.kind)),
            Err(e) if ps5upload_core::fs_ops::is_not_found(&format!("{e:#}")) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn has_exact_name(&self, dir: &str, name: &str) -> Result<bool> {
        use ps5upload_core::fs_ops::{list_dir, ListDirOptions};
        let mut offset = 0u64;
        loop {
            let page = list_dir(
                self.addr,
                dir,
                ListDirOptions {
                    offset,
                    ..ListDirOptions::default()
                },
            )?;
            if page.entries.iter().any(|e| e.name == name) {
                return Ok(true);
            }
            if !page.truncated || page.entries.is_empty() {
                return Ok(false);
            }
            offset += page.entries.len() as u64;
        }
    }

    fn copy_then_delete(&self, from: &str, to: &str, overwrite: bool) -> Result<()> {
        crate::require_console_ready(self.addr)?;
        ps5upload_ava1::copy::console_copy(self.addr, from, to, self.op_id, true, overwrite)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// A drive in memory, with the payload's refusals: path -> kind, plus which folders hold
    /// something. `insensitive` makes it answer like exFAT.
    #[derive(Default)]
    struct Fake {
        paths: RefCell<BTreeMap<String, String>>,
        full: Vec<&'static str>,
        insensitive: bool,
        cross: bool,
        copied: RefCell<Vec<(String, String, bool)>>,
    }

    impl Fake {
        fn with(paths: &[(&str, &str)]) -> Self {
            Self {
                paths: RefCell::new(
                    paths
                        .iter()
                        .map(|(p, k)| (p.to_string(), k.to_string()))
                        .collect(),
                ),
                ..Self::default()
            }
        }
        fn has(&self, p: &str) -> bool {
            self.paths
                .borrow()
                .keys()
                .any(|k| k == p || (self.insensitive && k.eq_ignore_ascii_case(p)))
        }
        fn kind_of(&self, p: &str) -> Option<String> {
            self.paths.borrow().get(p).cloned()
        }
    }

    impl MoveOps for Fake {
        fn rename(&self, from: &str, to: &str, replace: bool) -> Result<()> {
            if self.cross {
                return Err(anyhow!("payload rejected FS_MOVE: fs_move_cross_mount"));
            }
            if !replace && self.has(to) {
                return Err(anyhow!("payload rejected FS_MOVE: fs_move_exists"));
            }
            let (src, dst) = (self.kind_of(from), self.kind_of(to));
            let blocked = match (src.as_deref(), dst.as_deref()) {
                (Some("dir"), Some("dir")) => self.full.contains(&to),
                (Some(a), Some(b)) => a != b,
                (None, _) => true,
                _ => false,
            };
            if blocked {
                return Err(anyhow!("payload rejected FS_MOVE: fs_move_failed"));
            }
            let mut paths = self.paths.borrow_mut();
            let kind = paths.remove(from).unwrap();
            paths.insert(to.to_string(), kind);
            Ok(())
        }
        fn kind(&self, path: &str) -> Result<Option<String>> {
            Ok(self.kind_of(path))
        }
        fn has_exact_name(&self, dir: &str, name: &str) -> Result<bool> {
            Ok(self.paths.borrow().contains_key(&format!("{dir}/{name}")))
        }
        fn copy_then_delete(&self, from: &str, to: &str, overwrite: bool) -> Result<()> {
            self.copied
                .borrow_mut()
                .push((from.to_string(), to.to_string(), overwrite));
            Ok(())
        }
    }

    #[test]
    fn a_move_onto_an_existing_file_is_refused_and_nothing_changes() {
        let f = Fake::with(&[("/d/a.pkg", "file"), ("/e/a.pkg", "file")]);
        let e = run(&f, "/d/a.pkg", "/e/a.pkg", false).unwrap_err();
        assert_eq!(e.to_string(), "fs_move_exists: /e/a.pkg already exists");
        assert!(is_exists_refusal(&e.to_string()));
        assert!(f.has("/d/a.pkg") && f.has("/e/a.pkg"));
    }

    #[test]
    fn a_move_with_nothing_in_the_way_is_a_plain_rename() {
        let f = Fake::with(&[("/d/a.pkg", "file")]);
        run(&f, "/d/a.pkg", "/e/a.pkg", false).unwrap();
        assert!(!f.has("/d/a.pkg") && f.has("/e/a.pkg"));
        assert!(f.copied.borrow().is_empty());
    }

    #[test]
    fn a_replace_the_user_confirmed_replaces_the_file_by_rename() {
        let f = Fake::with(&[("/d/a.pkg", "file"), ("/e/a.pkg", "file")]);
        run(&f, "/d/a.pkg", "/e/a.pkg", true).unwrap();
        assert!(!f.has("/d/a.pkg") && f.has("/e/a.pkg"));
        assert!(f.copied.borrow().is_empty());
    }

    #[test]
    fn a_confirmed_folder_merge_on_one_drive_goes_through_the_console_move() {
        let mut f = Fake::with(&[("/d/g", "dir"), ("/e/g", "dir")]);
        f.full.push("/e/g");
        run(&f, "/d/g", "/e/g", true).unwrap();
        assert_eq!(
            *f.copied.borrow(),
            [("/d/g".to_string(), "/e/g".to_string(), true)]
        );
    }

    #[test]
    fn a_folder_over_an_empty_folder_stays_a_rename() {
        let f = Fake::with(&[("/d/g", "dir"), ("/e/g", "dir")]);
        run(&f, "/d/g", "/e/g", true).unwrap();
        assert!(f.copied.borrow().is_empty());
        assert!(!f.has("/d/g"));
    }

    #[test]
    fn a_file_and_a_folder_never_replace_each_other() {
        for (src, dst) in [("file", "dir"), ("dir", "file")] {
            let f = Fake::with(&[("/d/x", src), ("/e/x", dst)]);
            let e = run(&f, "/d/x", "/e/x", true).unwrap_err();
            assert!(is_exists_refusal(&e.to_string()), "{e}");
            assert!(f.copied.borrow().is_empty());
            assert!(f.has("/d/x") && f.has("/e/x"));
        }
    }

    #[test]
    fn a_rename_that_fails_for_another_reason_keeps_its_error() {
        let f = Fake::with(&[("/e/x", "dir")]);
        let e = run(&f, "/d/missing", "/e/x", true).unwrap_err();
        assert!(e.to_string().contains("fs_move_failed"), "{e}");
    }

    #[test]
    fn across_drives_the_callers_choice_reaches_the_console_move() {
        for overwrite in [false, true] {
            let mut f = Fake::with(&[("/d/a", "file")]);
            f.cross = true;
            run(&f, "/d/a", "/mnt/usb0/a", overwrite).unwrap();
            assert_eq!(
                *f.copied.borrow(),
                [("/d/a".to_string(), "/mnt/usb0/a".to_string(), overwrite)]
            );
        }
    }

    #[test]
    fn a_case_only_rename_works_on_a_case_insensitive_drive() {
        let mut f = Fake::with(&[("/mnt/usb0/Foo", "file")]);
        f.insensitive = true;
        run(&f, "/mnt/usb0/Foo", "/mnt/usb0/foo", false).unwrap();
        assert!(f.paths.borrow().contains_key("/mnt/usb0/foo"));
        assert!(!f.paths.borrow().contains_key("/mnt/usb0/Foo"));
    }

    #[test]
    fn a_case_only_rename_onto_a_real_other_file_is_refused() {
        let f = Fake::with(&[("/data/Foo", "file"), ("/data/foo", "file")]);
        let e = run(&f, "/data/Foo", "/data/foo", false).unwrap_err();
        assert!(is_exists_refusal(&e.to_string()), "{e}");
        assert!(f.has("/data/Foo") && f.has("/data/foo"));
    }

    #[test]
    fn a_move_onto_itself_without_overwrite_is_not_a_case_only_rename() {
        let f = Fake::with(&[("/data/Foo", "file")]);
        let e = run(&f, "/data/Foo", "/data/Foo", false).unwrap_err();
        assert!(is_exists_refusal(&e.to_string()), "{e}");
    }
}
