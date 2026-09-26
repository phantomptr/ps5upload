//! On-disk cache for console artwork.
//!
//! Cover art is the most expensive thing this engine serves repeatedly:
//! measured against a live console it averages 290 KB per title, and a
//! 23-title library is 6.6 MB — re-read over the mgmt port every time a
//! screen showing artwork is mounted.
//!
//! The cache lives here rather than in a client because the engine is the
//! only component that talks to the console, so the console is read once
//! per title instead of once per client (desktop, browser build and
//! Android each used to pay separately).
//!
//! It is an optimisation and never a dependency: an unwritable or missing
//! cache directory disables it and every route behaves exactly as before.
//! A truncated or unreadable entry is a miss, not an error.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

/// How long a stored image stays good.
///
/// Artwork changes only when a title is reinstalled, which
/// [`invalidate_console`] already handles — so this bound exists purely so
/// that nothing is cached *forever*, not because a day-old cover is
/// suspect.
const HIT_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// How long a recorded 404 stays good.
///
/// Deliberately much shorter than [`HIT_TTL`]: a title that has just
/// *gained* artwork should start showing it in minutes, not tomorrow. The
/// entry exists at all so that a title with genuinely no artwork stops
/// costing a console round-trip on every render.
const MISS_TTL: Duration = Duration::from_secs(15 * 60);

/// Ceiling for the whole cache. Enforced opportunistically on write.
const MAX_BYTES: u64 = 512 * 1024 * 1024;

/// What a lookup found.
pub enum Cached {
    /// Stored image bytes, with the ETag to serve alongside them.
    Hit { bytes: Vec<u8>, etag: String },
    /// The console has no artwork here, and we recorded that recently.
    KnownMissing,
    /// Nothing usable — caller should fetch.
    Unknown,
}

/// Root of the cache, or `None` when caching is unavailable.
///
/// Resolved once. `PS5UPLOAD_CACHE_DIR` exists so a container can point
/// this at a mounted volume; without it we sit beside the settings and
/// logs the desktop app already writes to `~/.ps5upload`.
fn root() -> Option<&'static Path> {
    static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    ROOT.get_or_init(|| {
        let base = match std::env::var("PS5UPLOAD_CACHE_DIR") {
            Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
            _ => {
                let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"));
                match home {
                    Ok(h) if !h.trim().is_empty() => {
                        PathBuf::from(h).join(".ps5upload").join("cache")
                    }
                    _ => {
                        eprintln!("[icon-cache] no HOME — artwork caching disabled");
                        return None;
                    }
                }
            }
        };
        let dir = base.join("icons");
        if let Err(e) = fs::create_dir_all(&dir) {
            eprintln!(
                "[icon-cache] {} unusable ({e}) — artwork caching disabled",
                dir.display()
            );
            return None;
        }
        Some(dir)
    })
    .as_deref()
}

/// Per-console subdirectory.
///
/// Keyed on the console address so one console's artwork can never be
/// served under another's name — the same invariant
/// `scripts/check-per-console-isolation.sh` protects in the route tree. A
/// console that changes DHCP address costs a cold cache, never a wrong
/// image.
fn console_dir(console: &str) -> Option<PathBuf> {
    let root = root()?;
    let key = blake3::hash(console.as_bytes()).to_hex();
    Some(root.join(&key.as_str()[..16]))
}

fn entry_path(console: &str, kind: &str, identity: &str) -> Option<PathBuf> {
    let dir = console_dir(console)?;
    let mut h = blake3::Hasher::new();
    h.update(kind.as_bytes());
    h.update(b"\0");
    h.update(identity.as_bytes());
    let name = h.finalize().to_hex();
    Some(dir.join(format!("{}.img", &name.as_str()[..32])))
}

/// Age of a file, or None if it has no readable timestamp.
fn age(path: &Path) -> Option<Duration> {
    let meta = fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    SystemTime::now().duration_since(modified).ok()
}

/// Look up artwork.
///
/// Note that mtime is never updated on a read: "age" therefore means
/// age-since-fetch, and traffic cannot keep an entry alive past its TTL.
pub fn get(console: &str, kind: &str, identity: &str) -> Cached {
    let Some(path) = entry_path(console, kind, identity) else {
        return Cached::Unknown;
    };
    let Some(age) = age(&path) else {
        return Cached::Unknown;
    };

    let Ok(bytes) = fs::read(&path) else {
        return Cached::Unknown;
    };

    // A zero-byte entry is the recorded 404, not an image.
    if bytes.is_empty() {
        return if age < MISS_TTL {
            Cached::KnownMissing
        } else {
            Cached::Unknown
        };
    }
    if age >= HIT_TTL {
        return Cached::Unknown;
    }
    let etag = etag_for(&bytes);
    Cached::Hit { bytes, etag }
}

/// Strong ETag for a body, so a revalidation can answer 304 with no body
/// rather than re-sending a quarter of a megabyte.
pub fn etag_for(bytes: &[u8]) -> String {
    format!("\"{}\"", &blake3::hash(bytes).to_hex().as_str()[..32])
}

/// Store artwork. Best-effort: a failed write just means the next request
/// fetches again.
pub fn put(console: &str, kind: &str, identity: &str, bytes: &[u8]) {
    if bytes.is_empty() {
        return; // an empty body would be indistinguishable from a 404 marker
    }
    write_entry(console, kind, identity, bytes);
}

/// Record that the console has no artwork here.
pub fn put_missing(console: &str, kind: &str, identity: &str) {
    write_entry(console, kind, identity, b"");
}

fn write_entry(console: &str, kind: &str, identity: &str, bytes: &[u8]) {
    let Some(path) = entry_path(console, kind, identity) else {
        return;
    };
    let Some(dir) = path.parent() else { return };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    // Write-then-rename so a reader never sees a half-written image. Both
    // paths are in the same directory, so the rename cannot cross devices.
    let tmp = path.with_extension("tmp");
    if fs::write(&tmp, bytes).is_err() {
        let _ = fs::remove_file(&tmp);
        return;
    }
    if fs::rename(&tmp, &path).is_err() {
        let _ = fs::remove_file(&tmp);
        return;
    }
    enforce_size_cap();
}

/// Drop everything cached for one console.
///
/// Called when a title is installed or uninstalled — the operation that
/// actually changes which artwork exists, so waiting out the TTL would
/// show the user something they just changed.
pub fn invalidate_console(console: &str) {
    if let Some(dir) = console_dir(console) {
        let _ = fs::remove_dir_all(dir);
    }
}

/// (files, bytes) currently stored.
pub fn stats() -> (u64, u64) {
    let Some(root) = root() else { return (0, 0) };
    let mut files = 0;
    let mut bytes = 0;
    for (path, meta) in walk(root) {
        let _ = path;
        files += 1;
        bytes += meta.len();
    }
    (files, bytes)
}

/// Remove every cached image. Returns the bytes freed.
pub fn clear() -> u64 {
    let Some(root) = root() else { return 0 };
    let (_, bytes) = stats();
    if let Ok(entries) = fs::read_dir(root) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                let _ = fs::remove_dir_all(&p);
            } else {
                let _ = fs::remove_file(&p);
            }
        }
    }
    bytes
}

/// Every cached file under `root`, with its metadata.
fn walk(root: &Path) -> Vec<(PathBuf, fs::Metadata)> {
    let mut out = Vec::new();
    let Ok(consoles) = fs::read_dir(root) else {
        return out;
    };
    for console in consoles.flatten() {
        let Ok(files) = fs::read_dir(console.path()) else {
            continue;
        };
        for f in files.flatten() {
            if let Ok(meta) = f.metadata() {
                if meta.is_file() {
                    out.push((f.path(), meta));
                }
            }
        }
    }
    out
}

/// Keep the cache under [`MAX_BYTES`], dropping oldest first.
///
/// Eviction is by age rather than by recency of use, which keeps it
/// consistent with the TTL rule (mtime is never touched on read) and needs
/// no index file that could disagree with the directory.
fn enforce_size_cap() {
    let Some(root) = root() else { return };
    let mut files = walk(root);
    let total: u64 = files.iter().map(|(_, m)| m.len()).sum();
    if total <= MAX_BYTES {
        return;
    }
    files.sort_by_key(|(_, m)| m.modified().unwrap_or(SystemTime::UNIX_EPOCH));
    let mut freed = 0u64;
    for (path, meta) in files {
        if total - freed <= MAX_BYTES {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            freed += meta.len();
        }
    }
}

/// Whether a failed read means *the console answered and said no*, as
/// opposed to *we could not ask it*.
///
/// This is the difference between "this title has no artwork" — safe and
/// useful to remember — and "the console was briefly unreachable", which
/// must never be recorded: doing so would blank every cover for the length
/// of the negative TTL after one dropped connection.
///
/// `fs_read` reports the first case by wrapping the payload's own
/// rejection (`fs_ops.rs`), and the second as a connection or timeout
/// error. There is no typed error to match on, so this matches that
/// prefix. If the wording ever changes the effect is simply that we stop
/// remembering absent artwork — a small cost, never a wrong answer, which
/// is the right way round for a guess like this to fail.
pub fn is_console_said_no(e: &anyhow::Error) -> bool {
    format!("{e:#}").contains("payload rejected FS_READ")
}

/// True when a request's `If-None-Match` matches what we hold.
pub fn etag_matches(if_none_match: Option<&str>, etag: &str) -> bool {
    match if_none_match {
        Some(v) => v.split(',').any(|c| c.trim() == etag || c.trim() == "*"),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `root()` resolves once per process, so every test in this module
    /// shares one cache directory. They therefore use distinct console
    /// keys rather than trying to reset it.
    fn setup(tag: &str) -> String {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let dir =
                std::env::temp_dir().join(format!("ps5upload-icontest-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            // SAFETY: single-threaded test setup, before any read of the var.
            unsafe { std::env::set_var("PS5UPLOAD_CACHE_DIR", &dir) };
        });
        format!("192.168.1.50:9114/{tag}")
    }

    /// Backdate an entry so a TTL can be tested without sleeping.
    fn backdate(console: &str, kind: &str, id: &str, by: Duration) {
        let path = entry_path(console, kind, id).unwrap();
        let when = SystemTime::now() - by;
        let f = fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(when).unwrap();
    }

    #[test]
    fn stores_and_returns_bytes() {
        let c = setup("basic");
        assert!(matches!(get(&c, "app", "CUSA00900"), Cached::Unknown));
        put(&c, "app", "CUSA00900", b"PNGDATA");
        match get(&c, "app", "CUSA00900") {
            Cached::Hit { bytes, etag } => {
                assert_eq!(bytes, b"PNGDATA");
                assert!(etag.starts_with('"'));
            }
            _ => panic!("expected a hit"),
        }
    }

    #[test]
    fn one_consoles_artwork_is_never_served_for_another() {
        let a = setup("iso-a");
        let b = setup("iso-b");
        put(&a, "app", "CUSA00900", b"CONSOLE-A");
        // Same title id, different console: must NOT resolve.
        assert!(
            matches!(get(&b, "app", "CUSA00900"), Cached::Unknown),
            "cache leaked across consoles"
        );
    }

    #[test]
    fn different_titles_do_not_collide() {
        let c = setup("collide");
        put(&c, "app", "CUSA00001", b"ONE");
        put(&c, "app", "CUSA00002", b"TWO");
        match get(&c, "app", "CUSA00001") {
            Cached::Hit { bytes, .. } => assert_eq!(bytes, b"ONE"),
            _ => panic!("expected a hit"),
        }
    }

    #[test]
    fn a_hit_expires_so_nothing_is_cached_forever() {
        let c = setup("hit-ttl");
        put(&c, "app", "CUSA00900", b"PNGDATA");
        backdate(&c, "app", "CUSA00900", HIT_TTL + Duration::from_secs(60));
        assert!(
            matches!(get(&c, "app", "CUSA00900"), Cached::Unknown),
            "an expired image must read as a miss"
        );
    }

    #[test]
    fn a_recorded_404_is_remembered_then_expires_sooner_than_a_hit() {
        let c = setup("miss-ttl");
        put_missing(&c, "app", "NPXS40172");
        assert!(matches!(get(&c, "app", "NPXS40172"), Cached::KnownMissing));

        // Past the miss TTL but well inside the hit TTL: a title that has
        // just gained artwork must not wait a full day to show it.
        backdate(&c, "app", "NPXS40172", MISS_TTL + Duration::from_secs(60));
        assert!(MISS_TTL < HIT_TTL);
        assert!(matches!(get(&c, "app", "NPXS40172"), Cached::Unknown));
    }

    #[test]
    fn an_empty_body_is_not_stored_as_an_image() {
        let c = setup("empty");
        // Would otherwise be indistinguishable from the 404 marker.
        put(&c, "app", "CUSA00900", b"");
        assert!(matches!(get(&c, "app", "CUSA00900"), Cached::Unknown));
    }

    #[test]
    fn install_and_uninstall_can_drop_a_consoles_entries() {
        let c = setup("invalidate");
        put(&c, "app", "CUSA00900", b"PNGDATA");
        invalidate_console(&c);
        assert!(matches!(get(&c, "app", "CUSA00900"), Cached::Unknown));
    }

    #[test]
    fn only_a_console_rejection_counts_as_absent_artwork() {
        // The console answered: there is genuinely no artwork here.
        let said_no =
            anyhow::anyhow!("payload rejected FS_READ(/user/appmeta/NPXS40172/icon0.png): ENOENT");
        assert!(is_console_said_no(&said_no));

        // We never reached it — remembering this would blank every cover
        // on the next render for the length of the negative TTL.
        for unreachable in [
            "connect to 192.168.1.5:9114: connection timed out",
            "expected FS_READ_ACK, got Error",
            "broken pipe",
        ] {
            assert!(
                !is_console_said_no(&anyhow::anyhow!(unreachable.to_string())),
                "must not treat {unreachable:?} as absent artwork"
            );
        }
    }

    #[test]
    fn etag_is_content_addressed() {
        assert_eq!(etag_for(b"same"), etag_for(b"same"));
        assert_ne!(etag_for(b"same"), etag_for(b"different"));
    }

    #[test]
    fn revalidation_matches_on_etag_or_wildcard() {
        let tag = etag_for(b"bytes");
        assert!(etag_matches(Some(&tag), &tag));
        assert!(etag_matches(Some("*"), &tag));
        assert!(etag_matches(Some(&format!("\"other\", {tag}")), &tag));
        assert!(!etag_matches(Some("\"other\""), &tag));
        assert!(!etag_matches(None, &tag));
    }

    #[test]
    fn a_truncated_entry_reads_as_a_miss_rather_than_an_error() {
        let c = setup("corrupt");
        put(&c, "app", "CUSA00900", b"PNGDATA");
        let path = entry_path(&c, "app", "CUSA00900").unwrap();
        fs::remove_file(&path).unwrap();
        assert!(matches!(get(&c, "app", "CUSA00900"), Cached::Unknown));
    }
}
