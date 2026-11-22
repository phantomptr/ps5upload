//! Turning a failed write under the engine's state folder (`/data` in the Docker images) into a
//! message a person can act on. A bare `Permission denied (os error 13)` gave a Synology user no
//! way to know which folder, which user, or what to change (#361).

use std::io;
use std::path::Path;

/// `(uid, gid)` of this process, where the OS tells us (Linux `/proc`); `None` elsewhere.
pub(crate) fn process_ids() -> Option<(u32, u32)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| -> Option<u32> {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    };
    Some((field("Uid:")?, field("Gid:")?))
}

/// `(uid, gid)` that own `path`, or its nearest existing parent.
#[cfg(unix)]
fn owner_of(path: &Path) -> Option<(u32, u32)> {
    use std::os::unix::fs::MetadataExt;
    path.ancestors()
        .find_map(|p| std::fs::metadata(p).ok())
        .map(|m| (m.uid(), m.gid()))
}

#[cfg(not(unix))]
fn owner_of(_path: &Path) -> Option<(u32, u32)> {
    None
}

/// [`describe`] as an `io::Error` of the same kind, for functions that return `io::Result`.
pub fn io_error(what: &str, path: &Path, err: io::Error) -> io::Error {
    io::Error::new(err.kind(), describe(what, path, &err))
}

/// The message for `err` hit while writing `path`. Anything but a permission or read-only-folder
/// failure keeps its own text, with the path added.
pub fn describe(what: &str, path: &Path, err: &io::Error) -> String {
    let denied = err.kind() == io::ErrorKind::PermissionDenied || err.raw_os_error() == Some(30); // EROFS
    if !denied {
        return format!("{what}: {} ({err})", path.display());
    }
    let dir = path.display();
    let me = process_ids();
    let owner = owner_of(path);
    let who = match me {
        Some((u, g)) => format!("the engine runs as UID:GID {u}:{g}"),
        None => "the engine's user cannot write there".to_string(),
    };
    let theirs = match owner {
        Some((u, g)) => format!(" but that folder is owned by {u}:{g}"),
        None => String::new(),
    };
    let fix = match me {
        Some((u, g)) => format!(
            "run `chown -R {u}:{g}` on the host folder mounted at /data, or set the container's \
             `user:` to the folder's owner (see engine/README.md, \"Synology and other NAS boxes\")"
        ),
        None => "give the engine's user write access to the folder, or point \
                 PS5UPLOAD_DATA_DIR at one it can write"
            .to_string(),
    };
    format!("{what}: cannot write {dir}: {who}{theirs}. To fix: {fix}. ({err})")
}

/// What to do about a folder the engine's user cannot use. In a container that is a `chown` on the
/// host folder mounted there (the images run as 65532:65532 unless `user:` says otherwise), the
/// step a ZimaOS user had to find by hand.
pub fn folder_fix(dir: &Path, ids: Option<(u32, u32)>, in_container: bool) -> String {
    match (ids, in_container) {
        (Some((u, g)), true) => format!(
            "the container runs as user {u}:{g}{}; on the host, run `chown -R {u}:{g}` on the \
             folder mounted at {}, or set the container's `user:` to that folder's owner",
            if (u, g) == (65532, 65532) {
                " (the image's default)"
            } else {
                ""
            },
            dir.display()
        ),
        (Some((u, g)), false) => format!(
            "give the account running ps5upload (UID:GID {u}:{g}) access to {}",
            dir.display()
        ),
        (None, _) => format!(
            "give the account running ps5upload access to {}",
            dir.display()
        ),
    }
}

/// The startup line for a folder the engine needs and cannot use. `need_write`: the engine writes
/// there (its data and staging folders); otherwise it only reads (a browse root, often `:ro`).
pub fn folder_message(
    what: &str,
    dir: &Path,
    need_write: bool,
    err: &io::Error,
    ids: Option<(u32, u32)>,
    in_container: bool,
) -> String {
    let verb = if need_write { "written to" } else { "read" };
    let denied = err.kind() == io::ErrorKind::PermissionDenied || err.raw_os_error() == Some(30); // EROFS
    if !denied {
        return format!("{what} {} cannot be {verb} ({err})", dir.display());
    }
    format!(
        "{what} {} cannot be {verb}. To fix: {}. ({err})",
        dir.display(),
        folder_fix(dir, ids, in_container)
    )
}

/// Writes and removes a small file in `dir` (made first when missing).
pub fn probe_write(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(format!(".write-check-{}", std::process::id()));
    std::fs::write(&probe, b"ok")?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// The folders the engine needs that it cannot use, worded for its log: the data folder and the
/// staging folder must take writes, the browse roots must be readable. A folder that does not
/// exist is left to the code that already reports it (an unmounted browse root, for one).
pub fn startup_folder_problems() -> Vec<String> {
    let ids = process_ids();
    let in_container = crate::pkg_install::in_container();
    let mut out = Vec::new();
    let mut write_dirs: Vec<(&str, std::path::PathBuf)> = Vec::new();
    if let Some(d) = crate::remote::store::data_dir() {
        write_dirs.push(("The engine's data folder", d));
    }
    if let Some(s) = std::env::var_os("PS5UPLOAD_STAGING_DIR").filter(|v| !v.is_empty()) {
        write_dirs.push(("The staging folder (PS5UPLOAD_STAGING_DIR)", s.into()));
    }
    for (what, dir) in write_dirs {
        if let Err(e) = probe_write(&dir) {
            out.push(folder_message(what, &dir, true, &e, ids, in_container));
        }
    }
    let roots = std::env::var("PS5UPLOAD_BROWSE_ROOTS").unwrap_or_default();
    for root in roots.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let dir = Path::new(root);
        if !dir.is_dir() {
            continue;
        }
        if let Err(e) = std::fs::read_dir(dir) {
            out.push(folder_message(
                "The browse folder (PS5UPLOAD_BROWSE_ROOTS)",
                dir,
                false,
                &e,
                ids,
                in_container,
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_denied_folder_in_a_container_names_the_user_and_the_chown() {
        let e = io::Error::from(io::ErrorKind::PermissionDenied);
        let m = folder_message(
            "The engine's data folder",
            Path::new("/data"),
            true,
            &e,
            Some((65532, 65532)),
            true,
        );
        assert!(m.contains("/data cannot be written to"), "{m}");
        assert!(m.contains("user 65532:65532"), "{m}");
        assert!(m.contains("chown -R 65532:65532"), "{m}");
        assert!(m.contains("`user:`"), "{m}");
    }

    #[test]
    fn a_denied_folder_outside_a_container_has_no_docker_advice() {
        let e = io::Error::from(io::ErrorKind::PermissionDenied);
        let m = folder_message(
            "x",
            Path::new("/media/games"),
            false,
            &e,
            Some((501, 20)),
            false,
        );
        assert!(m.contains("/media/games cannot be read"), "{m}");
        assert!(m.contains("501:20") && !m.contains("chown"), "{m}");
    }

    #[test]
    fn another_failure_keeps_its_own_text() {
        let e = io::Error::other("disk full");
        let m = folder_message("x", Path::new("/data"), true, &e, Some((1, 1)), true);
        assert!(m.contains("disk full") && !m.contains("To fix"), "{m}");
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_folder_fails_the_write_probe() {
        use std::os::unix::fs::PermissionsExt;
        if process_ids().map(|(u, _)| u) == Some(0) {
            return;
        }
        let dir = std::env::temp_dir().join(format!("ps5u-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(probe_write(&dir).is_ok());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let r = probe_write(&dir);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn a_permission_error_names_the_path_and_the_fix() {
        let e = io::Error::from(io::ErrorKind::PermissionDenied);
        let m = describe("saved connections", Path::new("/data/.ps5upload"), &e);
        assert!(m.contains("/data/.ps5upload"), "{m}");
        assert!(m.contains("To fix:"), "{m}");
        assert!(!m.starts_with("saved connections: Permission denied (os error 13)"));
    }

    #[test]
    fn other_errors_keep_their_text_and_gain_the_path() {
        let e = io::Error::from(io::ErrorKind::NotFound);
        let m = describe("x", Path::new("/a/b"), &e);
        assert!(m.contains("/a/b") && !m.contains("To fix"), "{m}");
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_dir_gives_the_descriptive_error() {
        use std::os::unix::fs::PermissionsExt;
        // root ignores mode bits, so this can only prove something as a normal user.
        if process_ids().map(|(u, _)| u) == Some(0) {
            return;
        }
        let dir = std::env::temp_dir().join(format!("ps5u-ro-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let err = crate::remote::store::Store::open(&dir.join("state")).err();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let msg = format!("{:#}", err.expect("a read-only parent must fail"));
        assert!(msg.contains("cannot write"), "{msg}");
        assert!(msg.contains("ps5u-ro-"), "{msg}");
        assert!(msg.contains("To fix:"), "{msg}");
        assert!(!msg.contains("os error 13") || msg.contains("cannot write"));
    }

    #[cfg(unix)]
    #[test]
    fn a_writable_dir_opens() {
        let dir = std::env::temp_dir().join(format!("ps5u-rw-{}", std::process::id()));
        let r = crate::remote::store::Store::open(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_ok());
    }
}
