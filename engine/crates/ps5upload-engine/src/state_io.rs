//! Turning a failed write under the engine's state folder (`/data` in the Docker images) into a
//! message a person can act on. A bare `Permission denied (os error 13)` gave a Synology user no
//! way to know which folder, which user, or what to change (#361).

use std::io;
use std::path::Path;

/// `(uid, gid)` of this process, where the OS tells us (Linux `/proc`); `None` elsewhere.
fn process_ids() -> Option<(u32, u32)> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
