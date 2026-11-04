//! Download a payload into the cache: stream to a private temporary file, check it, unpack a
//! release `.zip`, and only then move the ELF into place.
//!
//! Downloads of one payload run one at a time, and every temporary name is unique. Two at once
//! used to share one fixed temporary path: the second `create` truncated the first's open file,
//! both wrote into it, and the mix still started with `\x7FELF`, so nothing noticed and it could
//! go to a console at once.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use super::http::{fetch, FetchKind};
use super::{
    atomic_write, mtime_secs, replace_file, unique_suffix, Library, LibraryError,
    LocalInventoryEntry, ReleaseInfo, ResolvedEntry, TmpFile,
};

/// The most a payload asset may be. Covers the largest known homebrew with generous headroom;
/// anything above is a malformed asset or a supply-chain problem.
const PAYLOAD_DOWNLOAD_MAX_BYTES: u64 = 128 * 1024 * 1024;

/// Catalogue payloads whose releases publish a `sha256sum`-style list beside the ELF: a download
/// is refused unless its SHA-256 matches the list's line for the asset. Custom repos name theirs
/// when they are added.
const CHECKSUM_FILES: &[(&str, &str)] = &[("hush", "SHA256SUMS")];

/// The largest checksum list accepted (a few lines in practice).
const CHECKSUM_FILE_MAX_BYTES: u64 = 64 * 1024;

/// A temporary file older than this was left by a crash.
const STALE_TMP_AGE: Duration = Duration::from_secs(3600);

pub(super) fn checksum_file_for(id: &str) -> Option<&'static str> {
    CHECKSUM_FILES
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, name)| *name)
}

/// One payload's download state in this process: the turn lock, and the cancel request that
/// applies to everyone running or waiting. The flag is cleared when the last of them leaves.
#[derive(Default)]
struct Slot {
    turn: Mutex<()>,
    cancel: AtomicBool,
    pending: AtomicUsize,
}

fn slot_for(dir: &Path, id: &str) -> Arc<Slot> {
    static SLOTS: OnceLock<Mutex<HashMap<PathBuf, Arc<Slot>>>> = OnceLock::new();
    let map = SLOTS.get_or_init(Default::default);
    let mut map = map.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(dir.join(id)).or_default().clone()
}

/// Being in the pending count, for as long as a download call lasts.
struct Pending(Arc<Slot>);

impl Pending {
    fn enter(slot: Arc<Slot>) -> Self {
        slot.pending.fetch_add(1, Ordering::SeqCst);
        Self(slot)
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if self.0.pending.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.cancel.store(false, Ordering::SeqCst);
        }
    }
}

fn lock_turn(slot: &Slot) -> (MutexGuard<'_, ()>, bool) {
    if let Ok(g) = slot.turn.try_lock() {
        return (g, false);
    }
    (slot.turn.lock().unwrap_or_else(|e| e.into_inner()), true)
}

fn cancelled() -> LibraryError {
    LibraryError::new("download_cancelled", "The download was cancelled.")
}

fn io_err(what: impl std::fmt::Display) -> LibraryError {
    LibraryError::new("io_failed", what.to_string())
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The URL without its query and fragment.
fn url_path(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or("")
}

/// The asset's file name, the last segment of its download URL.
fn asset_file_name(asset_url: &str) -> Option<String> {
    let path = url_path(asset_url);
    let (_, after_scheme) = path.split_once("://")?;
    let (_, rest) = after_scheme.split_once('/')?;
    let name = rest.rsplit('/').next()?;
    (!name.is_empty()).then(|| name.to_string())
}

/// Another asset of the same release: the asset URL with its file name swapped
/// (`…/releases/download/v1.0.0/hush.elf` → `…/v1.0.0/SHA256SUMS`).
fn sibling_asset_url(asset_url: &str, name: &str) -> Option<String> {
    asset_file_name(asset_url)?;
    let path = url_path(asset_url);
    let (dir, _) = path.rsplit_once('/')?;
    Some(format!("{dir}/{name}"))
}

/// The lowercase hex SHA-256 a `sha256sum` list gives `file` (`<hex>  <name>`, or `<hex> *<name>`
/// in binary mode). None when the list has no well-formed line for it.
fn sha256_for(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let line = line.trim();
        let (hex, rest) = line.split_once(char::is_whitespace)?;
        let name = rest.trim_start().trim_start_matches('*');
        let name = name.rsplit('/').next().unwrap_or(name);
        (name == file && hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| hex.to_ascii_lowercase())
    })
}

/// SHA-256 of a file on disk: what is actually there, not what passed through the socket.
fn sha256_of_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

/// The file name a downloaded payload is cached under, which is also the name Store in Payload
/// Manager gives it: the asset hint when it is a plain name; with no hint (a custom repo), the
/// release asset's own file name (`<name>.elf`, or a `.zip`'s stem); else `<id>.elf`. A hint and
/// an asset name are outside text, so anything that could name a path (`/`, `..`, `:`, `%`) falls
/// back to the id instead of being joined. Without the asset name a custom repo's payload was
/// cached, and stored in Payload Manager, as `custom-<hex>.elf` (shown there as "Custom <hex>").
pub(super) fn cached_elf_name(id: &str, asset_name_hint: &str, asset_url: &str) -> String {
    let plain_base = |name: &str| -> Option<String> {
        let base = name
            .trim()
            .trim_end_matches(".elf")
            .trim_end_matches(".zip")
            .trim_matches(|c: char| !c.is_alphanumeric());
        let plain = !base.is_empty()
            && base
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            && !base.contains("..");
        plain.then(|| format!("{base}.elf"))
    };
    if !asset_name_hint.trim().is_empty() {
        return plain_base(asset_name_hint).unwrap_or_else(|| format!("{id}.elf"));
    }
    let asset_file = url_path(asset_url).rsplit('/').next().unwrap_or("");
    let lower = asset_file.to_ascii_lowercase();
    if lower.ends_with(".elf") || lower.ends_with(".zip") {
        if let Some(name) = plain_base(asset_file) {
            return name;
        }
    }
    format!("{id}.elf")
}

/// Remove every `.elf` in a payload's cache folder except `keep`: a copy cached under an earlier
/// name would be picked up just as well as the new one.
pub(super) fn remove_other_elfs(dir: &Path, keep: &str) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for f in read.flatten() {
        let p = f.path();
        if p.extension().and_then(|e| e.to_str()) == Some("elf")
            && f.file_name().to_str() != Some(keep)
        {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// Remove temporary files a crash left behind (older ones used fixed names).
fn sweep_stale_tmps(dir: &Path) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for f in read.flatten() {
        let p = f.path();
        if p.extension().and_then(|e| e.to_str()) != Some("tmp") {
            continue;
        }
        let old = f
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age >= STALE_TMP_AGE);
        if old {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// Extract the payload ELF from a downloaded release `.zip` into `dest`.
///
/// One whose name matches the hint wins over a generic `.elf`, and larger wins within a tier (the
/// runnable payload, not a tiny stub). `dest` is a fixed path and the entry's own name is never
/// used as one, so a malicious name can't escape the folder. The copy is capped, so a lying
/// central directory can't fill the disk.
pub(super) fn extract_payload_elf_from_zip(
    zip_path: &Path,
    dest: &Path,
    hint: &str,
) -> Result<(), String> {
    let f = std::fs::File::open(zip_path).map_err(|e| format!("open zip: {e}"))?;
    let mut zip = zip::ZipArchive::new(f).map_err(|e| format!("read zip: {e}"))?;
    let lower_hint = hint.to_ascii_lowercase();

    // rank 0 = name contains the hint, rank 1 = any other .elf.
    let mut best: Option<(u8, u64, usize)> = None;
    for i in 0..zip.len() {
        let e = zip.by_index(i).map_err(|e| format!("zip entry {i}: {e}"))?;
        if !e.is_file() {
            continue;
        }
        let name = e
            .name()
            .map_err(|e| format!("zip entry {i} name: {e}"))?
            .to_ascii_lowercase();
        if !name.ends_with(".elf") {
            continue;
        }
        let rank = if !lower_hint.is_empty() && name.contains(&lower_hint) {
            0
        } else {
            1
        };
        let size = e.size();
        let better = best.map(|(r, s, _)| rank < r || (rank == r && size > s));
        if better.unwrap_or(true) {
            best = Some((rank, size, i));
        }
    }
    let idx = best
        .map(|(_, _, i)| i)
        .ok_or_else(|| "zip release contains no .elf payload".to_string())?;

    let mut entry = zip.by_index(idx).map_err(|e| format!("zip entry: {e}"))?;
    let mut out =
        std::fs::File::create(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    let mut limited = (&mut entry).take(PAYLOAD_DOWNLOAD_MAX_BYTES + 1);
    let copied = std::io::copy(&mut limited, &mut out).map_err(|e| format!("extract: {e}"))?;
    if copied > PAYLOAD_DOWNLOAD_MAX_BYTES {
        drop(out);
        let _ = std::fs::remove_file(dest);
        return Err(format!(
            "zip ELF entry exceeds {PAYLOAD_DOWNLOAD_MAX_BYTES} byte cap"
        ));
    }
    out.sync_all()
        .map_err(|e| format!("fsync extracted elf: {e}"))?;
    Ok(())
}

/// The first four bytes of a file, or how many there were.
fn file_head(path: &Path) -> Result<Result<[u8; 4], usize>, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("reopen {}: {e}", path.display()))?;
    let mut head = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        let n = f
            .read(&mut head[got..])
            .map_err(|e| format!("read head: {e}"))?;
        if n == 0 {
            return Ok(Err(got));
        }
        got += n;
    }
    Ok(Ok(head))
}

impl Library {
    /// The SHA-256 the release's checksum list gives the asset.
    fn expected_sha256(
        &self,
        entry: &ResolvedEntry,
        asset_url: &str,
        sums_name: &str,
    ) -> Result<String, LibraryError> {
        let fail = |m: String| LibraryError::new("checksum_failed", m);
        let file = asset_file_name(asset_url)
            .ok_or_else(|| fail(format!("no file name in {asset_url}")))?;
        let url = sibling_asset_url(asset_url, sums_name)
            .ok_or_else(|| fail(format!("no {sums_name} url for {asset_url}")))?;
        let resp = fetch(self.http.as_ref(), entry, &url, None, FetchKind::Asset)?;
        if !(200..300).contains(&resp.status) {
            return Err(fail(format!(
                "fetch {sums_name}: HTTP {} (the release must publish it)",
                resp.status
            )));
        }
        // Never more than the cap comes into memory, whatever the server sends.
        let mut body = Vec::new();
        resp.body
            .take(CHECKSUM_FILE_MAX_BYTES + 1)
            .read_to_end(&mut body)
            .map_err(|e| fail(format!("read {sums_name}: {e}")))?;
        if body.len() as u64 > CHECKSUM_FILE_MAX_BYTES {
            return Err(fail(format!(
                "{sums_name} is larger than {CHECKSUM_FILE_MAX_BYTES} bytes"
            )));
        }
        sha256_for(&String::from_utf8_lossy(&body), &file)
            .ok_or_else(|| fail(format!("{sums_name} has no checksum for {file}")))
    }

    /// Stop this payload's download. `true` when one was running or waiting; it ends with
    /// `download_cancelled` and leaves nothing behind.
    pub fn cancel_download(&self, id: &str) -> bool {
        let slot = slot_for(&self.dir, id);
        if slot.pending.load(Ordering::SeqCst) == 0 {
            return false;
        }
        slot.cancel.store(true, Ordering::SeqCst);
        true
    }

    /// Download a release asset into the cache. Only a real ELF is ever cached, so a 404 page or
    /// a cut-off response can't be sent to a console later.
    pub fn download(
        &self,
        id: &str,
        asset_url: &str,
        version: &str,
    ) -> Result<LocalInventoryEntry, LibraryError> {
        let entry = self.resolve_entry(id)?;
        if asset_url.is_empty() {
            return Err(LibraryError::new("no_asset", "no asset url"));
        }
        // The first URL is on the repo's own host, https (a request can name any URL); every
        // redirect after it is checked by `fetch`.
        let pinned = asset_url.parse::<ureq::http::Uri>().ok().is_some_and(|u| {
            u.scheme_str() == Some("https")
                && u.host()
                    .is_some_and(|h| h.eq_ignore_ascii_case(&entry.repo_host))
        });
        if !pinned {
            return Err(LibraryError::new(
                "asset_url_refused",
                format!(
                    "asset url for {} must be https on {}",
                    entry.id, entry.repo_host
                ),
            ));
        }

        let slot = slot_for(&self.dir, &entry.id);
        let _pending = Pending::enter(slot.clone());
        let (_turn, waited) = lock_turn(&slot);
        if slot.cancel.load(Ordering::SeqCst) {
            return Err(cancelled());
        }
        // The one we waited for already fetched this very version: no second download.
        if waited {
            if let Some(local) = self.local_entry(&entry.id) {
                if local.version == version {
                    return Ok(local);
                }
            }
        }
        self.download_locked(&entry, asset_url, version, &slot.cancel)
    }

    fn download_locked(
        &self,
        entry: &ResolvedEntry,
        asset_url: &str,
        version: &str,
        cancel: &AtomicBool,
    ) -> Result<LocalInventoryEntry, LibraryError> {
        // A release with a checksum list: read the expected value first, so a missing or
        // malformed list fails before the download.
        let expected = if entry.checksum_file.is_empty() {
            None
        } else {
            Some(self.expected_sha256(entry, asset_url, &entry.checksum_file)?)
        };

        let resp = fetch(self.http.as_ref(), entry, asset_url, None, FetchKind::Asset)?;
        if !(200..300).contains(&resp.status) {
            return Err(LibraryError::new(
                "fetch_failed",
                format!("fetch {asset_url}: HTTP {}", resp.status),
            ));
        }
        let too_large = |n: u64| {
            LibraryError::new(
                "asset_too_large",
                format!("asset is too large ({n} bytes > {PAYLOAD_DOWNLOAD_MAX_BYTES} cap)"),
            )
        };
        if let Some(total) = resp
            .content_length
            .filter(|t| *t > PAYLOAD_DOWNLOAD_MAX_BYTES)
        {
            return Err(too_large(total));
        }

        let dir = self.dir.join(&entry.id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| io_err(format!("mkdir {}: {e}", dir.display())))?;
        sweep_stale_tmps(&dir);

        let final_name = cached_elf_name(&entry.id, &entry.asset_name_hint, asset_url);
        let final_path = dir.join(&final_name);
        let unique = unique_suffix();
        let dl_tmp = TmpFile(dir.join(format!("{final_name}.{unique}.download.tmp")));
        let elf_tmp = TmpFile(dir.join(format!("{final_name}.{unique}.elf.tmp")));

        // Stream to a file (capped). A payload is a raw ELF or a release .zip with the ELF
        // inside, told apart by its magic below.
        {
            let mut tmp = std::fs::File::create(&dl_tmp.0)
                .map_err(|e| io_err(format!("create {}: {e}", dl_tmp.0.display())))?;
            let mut body = resp.body;
            let mut buf = vec![0u8; 64 * 1024];
            let mut written: u64 = 0;
            loop {
                if cancel.load(Ordering::SeqCst) {
                    return Err(cancelled());
                }
                let n = body
                    .read(&mut buf)
                    .map_err(|e| LibraryError::new("fetch_failed", format!("read chunk: {e}")))?;
                if n == 0 {
                    break;
                }
                written = written.saturating_add(n as u64);
                if written > PAYLOAD_DOWNLOAD_MAX_BYTES {
                    return Err(too_large(written));
                }
                tmp.write_all(&buf[..n])
                    .map_err(|e| io_err(format!("write {}: {e}", dl_tmp.0.display())))?;
            }
            if let Some(total) = resp.content_length.filter(|t| *t != written) {
                return Err(LibraryError::new(
                    "fetch_failed",
                    format!("read chunk: the connection closed after {written} of {total} bytes"),
                ));
            }
            tmp.sync_all()
                .map_err(|e| io_err(format!("fsync {}: {e}", dl_tmp.0.display())))?;
        }

        if let Some(want) = expected {
            let got = sha256_of_file(&dl_tmp.0)
                .map_err(|e| io_err(format!("hash {}: {e}", dl_tmp.0.display())))?;
            if got != want {
                return Err(LibraryError::new(
                    "checksum_mismatch",
                    format!(
                        "downloaded {} does not match the release's checksum (got {got}, expected {want})",
                        entry.id
                    ),
                ));
            }
        }

        // What did we get? (A 404 HTML page, a truncated response, ...)
        let not_payload = |m: String| LibraryError::new("asset_not_payload", m);
        let head = file_head(&dl_tmp.0).map_err(io_err)?.map_err(|n| {
            not_payload(format!(
                "downloaded asset is too short to be a payload ({n} bytes total)"
            ))
        })?;
        if &head == b"\x7FELF" {
            std::fs::rename(&dl_tmp.0, &elf_tmp.0)
                .map_err(|e| io_err(format!("rename {}: {e}", dl_tmp.0.display())))?;
        } else if &head[..2] == b"PK" {
            extract_payload_elf_from_zip(&dl_tmp.0, &elf_tmp.0, &entry.asset_name_hint)
                .map_err(not_payload)?;
        } else {
            return Err(not_payload(format!(
                "downloaded asset is neither an ELF nor a zip (first bytes {head:02x?})"
            )));
        }

        // Whatever we ended up with must really be an ELF before it takes the .elf name.
        match file_head(&elf_tmp.0).map_err(io_err)? {
            Ok(m) if &m == b"\x7FELF" => {}
            Ok(m) => {
                return Err(not_payload(format!(
                    "extracted payload is not an ELF (first bytes {m:02x?})"
                )))
            }
            Err(n) => {
                return Err(not_payload(format!(
                    "extracted payload is not an ELF ({n} bytes)"
                )))
            }
        }
        if cancel.load(Ordering::SeqCst) {
            return Err(cancelled());
        }

        replace_file(&elf_tmp.0, &final_path).map_err(|e| {
            io_err(format!(
                "rename {} -> {}: {e}",
                elf_tmp.0.display(),
                final_path.display()
            ))
        })?;
        remove_other_elfs(&dir, &final_name);
        let _ = atomic_write(&dir.join("VERSION"), version.as_bytes());

        let meta = std::fs::metadata(&final_path).map_err(|e| io_err(format!("stat: {e}")))?;
        Ok(LocalInventoryEntry {
            payload_id: entry.id.clone(),
            version: version.to_string(),
            path: final_path.to_string_lossy().to_string(),
            size: meta.len(),
            mtime: mtime_secs(&meta),
        })
    }

    /// Download a payload's release by tag, or the latest when `tag` is `None`.
    pub fn download_release(
        &self,
        id: &str,
        tag: Option<&str>,
        force_refresh: bool,
    ) -> Result<LocalInventoryEntry, LibraryError> {
        let release: ReleaseInfo = match tag {
            None => self.release(id, force_refresh)?,
            Some(tag) => self
                .releases(id, force_refresh)?
                .into_iter()
                .find(|r| r.tag == tag)
                .ok_or_else(|| {
                    LibraryError::new("unknown_release", format!("{id} has no release {tag}"))
                })?,
        };
        if release.picked_asset_url.is_empty() {
            return Err(LibraryError::new(
                "no_asset",
                format!("release {} of {id} has no payload file", release.tag),
            ));
        }
        self.download(id, &release.picked_asset_url, &release.tag)
    }

    /// A payload's file ready to send: the cached one when it is the version asked for (or when
    /// no version was asked for), else downloaded now.
    pub fn ensure_cached(&self, id: &str, tag: Option<&str>) -> Result<PathBuf, LibraryError> {
        self.resolve_entry(id)?;
        if let Some(local) = self.local_entry(id) {
            if tag.is_none() || tag == Some(local.version.as_str()) {
                return Ok(PathBuf::from(local.path));
            }
        }
        Ok(PathBuf::from(self.download_release(id, tag, false)?.path))
    }
}

#[cfg(test)]
mod tests {
    use super::super::catalogue::CATALOGUE;
    use super::super::http::{HttpClient, HttpResponse};
    use super::super::test_support::{library, TempDir};
    use super::*;
    use std::sync::Barrier;

    const KSTUFF_ASSET: &str =
        "https://github.com/EchoStretch/kstuff-lite/releases/download/v1/kstuff.elf";
    const KSTUFF_ID: &str = "kstuff-echostretch";

    fn elf(fill: u8, len: usize) -> Vec<u8> {
        let mut v = b"\x7FELF".to_vec();
        v.resize(len, fill);
        v
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|f| f.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn a_new_download_leaves_only_its_own_elf() {
        let d = TempDir::new("elf-cache");
        for name in ["custom-0a.elf", "kstuff.elf", "VERSION", "notes.txt"] {
            std::fs::write(d.0.join(name), b"x").unwrap();
        }
        remove_other_elfs(&d.0, "kstuff.elf");
        assert_eq!(files_in(&d.0), ["VERSION", "kstuff.elf", "notes.txt"]);
    }

    #[test]
    fn a_custom_hint_cannot_name_a_path() {
        let n = |hint: &str| cached_elf_name("custom-0a", hint, "");
        assert_eq!(n("kstuff"), "kstuff.elf");
        assert_eq!(n("elfldr-ps5.elf"), "elfldr-ps5.elf");
        assert_eq!(n("HearBridge-"), "HearBridge.elf");
        assert_eq!(n(""), "custom-0a.elf");
        // Leading and trailing punctuation is trimmed; only interior path text falls back.
        assert_eq!(n("..\\x"), "x.elf");
        for hint in ["a/../../b", "C:evil", "sub/dir", "a..b", "a\\b"] {
            assert_eq!(n(hint), "custom-0a.elf", "{hint}");
        }
    }

    #[test]
    fn a_custom_repo_payload_keeps_its_release_asset_name() {
        let n = |url: &str| cached_elf_name("custom-3fa9c21b04de", "", url);
        let gh = "https://github.com/someone/thing/releases/download/v1.2";
        assert_eq!(n(&format!("{gh}/MyPayload_v1.2.elf")), "MyPayload_v1.2.elf");
        assert_eq!(n(&format!("{gh}/MyPayload-1.2.zip")), "MyPayload-1.2.elf");
        assert_eq!(n(&format!("{gh}/thing.elf?raw=1")), "thing.elf");
        assert_eq!(
            cached_elf_name("custom-0a", "kstuff", &format!("{gh}/kstuff_v1.elf")),
            "kstuff.elf"
        );
        for bad in [
            "My%20Payload.elf",
            "a..b.elf",
            "notes.txt",
            "",
            "https://x/",
        ] {
            assert_eq!(
                n(&format!("{gh}/{bad}")),
                "custom-3fa9c21b04de.elf",
                "{bad}"
            );
        }
    }

    #[test]
    fn hush_is_checked_against_its_sha256sums() {
        let hush = CATALOGUE.iter().find(|e| e.id == "hush").unwrap();
        assert_eq!(hush.asset_name_hint, "hush.elf");
        assert_eq!(hush.on_console_marker_path, Some("/data/hush/config.ini"));
        assert_eq!(checksum_file_for("hush"), Some("SHA256SUMS"));
        assert_eq!(checksum_file_for("nanodns"), None);
        for (id, _) in CHECKSUM_FILES {
            assert!(CATALOGUE.iter().any(|e| e.id == *id), "{id}");
        }
    }

    #[test]
    fn checksum_list_parsing() {
        let url = "https://github.com/disco-trooper/hush/releases/download/v1.0.0/hush.elf";
        assert_eq!(asset_file_name(url).as_deref(), Some("hush.elf"));
        assert_eq!(
            sibling_asset_url(url, "SHA256SUMS").as_deref(),
            Some("https://github.com/disco-trooper/hush/releases/download/v1.0.0/SHA256SUMS")
        );
        let hex = "5c98dd0881b64eb00e90d58d90056a66193ef8ee50e2d9906a1e298b5a34c352";
        assert_eq!(
            sha256_for(&format!("{hex}  hush.elf\n"), "hush.elf").as_deref(),
            Some(hex)
        );
        // Binary mode, a path, upper case, CRLF and other files around it.
        let list = format!(
            "{}  other.elf\r\n{} *dist/hush.elf\r\n",
            "a".repeat(64),
            hex.to_uppercase()
        );
        assert_eq!(sha256_for(&list, "hush.elf").as_deref(), Some(hex));
        assert_eq!(sha256_for(&list, "missing.elf"), None);
        assert_eq!(sha256_for("abc  hush.elf", "hush.elf"), None);
        assert_eq!(
            sha256_for(&format!("{}  hush.elf", "z".repeat(64)), "hush.elf"),
            None
        );
        assert_eq!(hex_lower(&[0x00, 0xab, 0x0f]), "00ab0f");
    }

    #[test]
    fn extract_payload_elf_from_zip_picks_inner_elf() {
        let d = TempDir::new("zip");
        let zip_path = d.0.join("a.zip");
        let dest = d.0.join("out.elf");
        let body = elf(0, 2048);
        {
            let f = std::fs::File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            // Decoys + the real payload (the real SMP zip's layout).
            zw.start_file("README.md", opts).unwrap();
            zw.write_all(b"# not a payload").unwrap();
            zw.start_file("config.ini.example", opts).unwrap();
            zw.write_all(b"key=value").unwrap();
            zw.start_file("shadowmountplus.elf", opts).unwrap();
            zw.write_all(&body).unwrap();
            zw.finish().unwrap();
        }
        extract_payload_elf_from_zip(&zip_path, &dest, "shadowmountplus").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
    }

    #[test]
    fn extract_payload_elf_from_zip_rejects_no_elf() {
        let d = TempDir::new("zip-none");
        let zip_path = d.0.join("a.zip");
        {
            let f = std::fs::File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default();
            zw.start_file("README.md", opts).unwrap();
            zw.write_all(b"no payload here").unwrap();
            zw.finish().unwrap();
        }
        assert!(extract_payload_elf_from_zip(&zip_path, &d.0.join("o"), "x").is_err());
    }

    #[test]
    fn a_download_is_cached_with_its_version_and_no_temporary_files() {
        let (_d, http, lib) = library("ok");
        let body = elf(7, 4096);
        http.ok(KSTUFF_ASSET, &body);
        let local = lib.download(KSTUFF_ID, KSTUFF_ASSET, "v1").unwrap();
        assert_eq!(local.version, "v1");
        assert_eq!(local.size, 4096);
        assert_eq!(std::fs::read(&local.path).unwrap(), body);
        let dir = lib.dir().join(KSTUFF_ID);
        assert_eq!(files_in(&dir), ["VERSION", "kstuff.elf"]);
        assert_eq!(lib.local_entry(KSTUFF_ID).unwrap().version, "v1");
    }

    #[test]
    fn the_asset_url_is_pinned_to_the_repo_host() {
        let (_d, http, lib) = library("pin");
        for url in [
            "https://evil.example/kstuff.elf",
            "http://github.com/x/kstuff.elf",
            "https://github.com.evil.example/kstuff.elf",
            "",
        ] {
            assert!(lib.download(KSTUFF_ID, url, "v1").is_err(), "{url}");
        }
        assert!(http.asked.lock().unwrap().is_empty());
    }

    #[test]
    fn a_page_or_a_short_body_never_gets_cached_and_leaves_nothing() {
        let (_d, http, lib) = library("junk");
        http.ok(KSTUFF_ASSET, b"<html>not found</html>");
        let e = lib.download(KSTUFF_ID, KSTUFF_ASSET, "v1").unwrap_err();
        assert_eq!(e.code, "asset_not_payload");
        http.ok(KSTUFF_ASSET, b"ab");
        let e = lib.download(KSTUFF_ID, KSTUFF_ASSET, "v1").unwrap_err();
        assert_eq!(e.code, "asset_not_payload");
        assert!(files_in(&lib.dir().join(KSTUFF_ID)).is_empty());
        assert!(lib.local_path(KSTUFF_ID).is_none());
    }

    #[test]
    fn a_failed_download_keeps_the_previous_version() {
        let (_d, http, lib) = library("keep");
        http.ok(KSTUFF_ASSET, &elf(1, 100));
        lib.download(KSTUFF_ID, KSTUFF_ASSET, "v1").unwrap();
        http.status(KSTUFF_ASSET, 500);
        assert!(lib.download(KSTUFF_ID, KSTUFF_ASSET, "v2").is_err());
        let local = lib.local_entry(KSTUFF_ID).unwrap();
        assert_eq!(local.version, "v1");
        assert_eq!(std::fs::read(&local.path).unwrap(), elf(1, 100));
    }

    #[test]
    fn a_body_cut_short_of_its_length_is_refused() {
        struct Short;
        impl HttpClient for Short {
            fn get(
                &self,
                _url: &str,
                _accept: Option<&str>,
                _kind: FetchKind,
            ) -> Result<HttpResponse, String> {
                Ok(HttpResponse {
                    status: 200,
                    location: None,
                    content_length: Some(9999),
                    body: Box::new(std::io::Cursor::new(elf(1, 100))),
                })
            }
        }
        let d = TempDir::new("short");
        let lib = Library::with_http(d.0.join("payloads"), Arc::new(Short));
        let e = lib.download(KSTUFF_ID, KSTUFF_ASSET, "v1").unwrap_err();
        assert!(e.message.contains("100 of 9999"), "{}", e.message);
        assert!(files_in(&lib.dir().join(KSTUFF_ID)).is_empty());
    }

    #[test]
    fn a_zip_release_caches_the_inner_elf() {
        let (d, http, lib) = library("zip-release");
        let zip_path = d.0.join("smp.zip");
        let inner = elf(9, 2048);
        {
            let f = std::fs::File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default();
            zw.start_file("readme.txt", opts).unwrap();
            zw.write_all(b"hello").unwrap();
            zw.start_file("shadowmountplus.elf", opts).unwrap();
            zw.write_all(&inner).unwrap();
            zw.finish().unwrap();
        }
        let row = CATALOGUE
            .iter()
            .find(|e| e.id == "shadowmountplus")
            .unwrap();
        let url = format!(
            "https://github.com/{}/{}/releases/download/v1/ShadowMountPlus_v1.zip",
            row.repo_owner, row.repo_name
        );
        http.ok(&url, &std::fs::read(&zip_path).unwrap());
        let local = lib.download(row.id, &url, "v1").unwrap();
        assert_eq!(std::fs::read(&local.path).unwrap(), inner);
        assert_eq!(
            files_in(&lib.dir().join(row.id)).len(),
            2,
            "the elf and VERSION, no zip"
        );
    }

    #[test]
    fn the_checksum_is_of_the_file_on_disk() {
        let (_d, http, lib) = library("sums");
        let body = elf(3, 512);
        let info = lib
            .add_custom_repo(&crate::payload_library::AddRepo {
                host: "github.com".into(),
                owner: "me".into(),
                name: "thing".into(),
                checksum_file: Some("SHA256SUMS".into()),
                ..Default::default()
            })
            .unwrap();
        let base = "https://github.com/me/thing/releases/download/v1";
        let url = format!("{base}/thing.elf");
        http.ok(&url, &body);

        let good = hex_lower(&Sha256::digest(&body));
        http.ok(
            &format!("{base}/SHA256SUMS"),
            format!("{good}  thing.elf\n").as_bytes(),
        );
        lib.download(&info.id, &url, "v1").unwrap();

        // A list that disagrees refuses the file and keeps what was cached.
        http.ok(
            &format!("{base}/SHA256SUMS"),
            format!("{}  thing.elf\n", "a".repeat(64)).as_bytes(),
        );
        let e = lib.download(&info.id, &url, "v2").unwrap_err();
        assert_eq!(e.code, "checksum_mismatch");
        assert_eq!(lib.local_entry(&info.id).unwrap().version, "v1");

        // A list with no line for the file, or no list at all, refuses it too.
        http.ok(&format!("{base}/SHA256SUMS"), b"nothing here\n");
        assert_eq!(
            lib.download(&info.id, &url, "v2").unwrap_err().code,
            "checksum_failed"
        );
        http.status(&format!("{base}/SHA256SUMS"), 404);
        assert_eq!(
            lib.download(&info.id, &url, "v2").unwrap_err().code,
            "checksum_failed"
        );
    }

    #[test]
    fn an_oversized_checksum_list_is_refused_without_reading_it_all() {
        let (_d, http, lib) = library("big-sums");
        let row = CATALOGUE.iter().find(|e| e.id == "hush").unwrap();
        let base = format!(
            "https://github.com/{}/{}/releases/download/v1",
            row.repo_owner, row.repo_name
        );
        http.ok(
            &format!("{base}/SHA256SUMS"),
            &vec![b'x'; CHECKSUM_FILE_MAX_BYTES as usize + 10],
        );
        let e = lib
            .download("hush", &format!("{base}/hush.elf"), "v1")
            .unwrap_err();
        assert!(e.message.contains("larger than"), "{}", e.message);
    }

    /// A server whose body blocks until released, so a test can hold a download open.
    struct Gated {
        started: Barrier,
        release: Barrier,
        served: AtomicUsize,
    }

    struct GatedBody {
        gate: Arc<Gated>,
        bytes: std::io::Cursor<Vec<u8>>,
        first: bool,
    }

    impl Read for GatedBody {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.first {
                self.first = false;
                self.gate.started.wait();
                self.gate.release.wait();
            }
            self.bytes.read(buf)
        }
    }

    struct GatedClient(Arc<Gated>, Vec<u8>);

    impl HttpClient for GatedClient {
        fn get(
            &self,
            _url: &str,
            _accept: Option<&str>,
            _kind: FetchKind,
        ) -> Result<HttpResponse, String> {
            self.0.served.fetch_add(1, Ordering::SeqCst);
            Ok(HttpResponse {
                status: 200,
                location: None,
                content_length: Some(self.1.len() as u64),
                body: Box::new(GatedBody {
                    gate: self.0.clone(),
                    bytes: std::io::Cursor::new(self.1.clone()),
                    first: true,
                }),
            })
        }
    }

    fn gated(tag: &str, body: Vec<u8>) -> (TempDir, Arc<Gated>, Library) {
        let d = TempDir::new(tag);
        let gate = Arc::new(Gated {
            started: Barrier::new(2),
            release: Barrier::new(2),
            served: AtomicUsize::new(0),
        });
        let lib = Library::with_http(
            d.0.join("payloads"),
            Arc::new(GatedClient(gate.clone(), body)),
        );
        (d, gate, lib)
    }

    #[test]
    fn two_downloads_of_one_payload_take_turns_and_fetch_once() {
        let body = elf(5, 256 * 1024);
        let (_d, gate, lib) = gated("race", body.clone());
        let (a, b) = (lib.clone(), lib.clone());
        let t1 = std::thread::spawn(move || a.download(KSTUFF_ID, KSTUFF_ASSET, "v1"));
        gate.started.wait();
        // The first holds the turn; the second has to wait for it.
        let t2 = std::thread::spawn(move || b.download(KSTUFF_ID, KSTUFF_ASSET, "v1"));
        while slot_for(lib.dir(), KSTUFF_ID)
            .pending
            .load(Ordering::SeqCst)
            < 2
        {
            std::thread::yield_now();
        }
        gate.release.wait();
        let first = t1.join().unwrap().unwrap();
        let second = t2.join().unwrap().unwrap();
        assert_eq!(first.path, second.path);
        assert_eq!(std::fs::read(&first.path).unwrap(), body);
        assert_eq!(
            gate.served.load(Ordering::SeqCst),
            1,
            "the second finds this version already cached"
        );
        assert_eq!(
            files_in(&lib.dir().join(KSTUFF_ID)),
            ["VERSION", "kstuff.elf"]
        );
    }

    #[test]
    fn cancel_stops_the_download_and_removes_its_temporary_file() {
        let (_d, gate, lib) = gated("cancel", elf(1, 4096));
        assert!(!lib.cancel_download(KSTUFF_ID), "nothing is running yet");
        let a = lib.clone();
        let t = std::thread::spawn(move || a.download(KSTUFF_ID, KSTUFF_ASSET, "v1"));
        gate.started.wait();
        // The download is in its first read, with its temporary file open.
        assert_eq!(files_in(&lib.dir().join(KSTUFF_ID)).len(), 1);
        assert!(lib.cancel_download(KSTUFF_ID));
        gate.release.wait();
        let err = t.join().unwrap().unwrap_err();
        assert_eq!(err.code, "download_cancelled");
        assert!(files_in(&lib.dir().join(KSTUFF_ID)).is_empty());
        assert!(lib.local_path(KSTUFF_ID).is_none());
        // The request is spent: the next download runs.
        assert!(!lib.cancel_download(KSTUFF_ID));
    }

    #[test]
    fn a_stale_temporary_file_is_swept_and_a_recent_one_left_alone() {
        let d = TempDir::new("sweep");
        let old = d.0.join("kstuff.download.tmp");
        std::fs::write(&old, b"x").unwrap();
        let f = std::fs::File::options().write(true).open(&old).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(2 * 3600))
            .unwrap();
        drop(f);
        let recent = d.0.join("kstuff.1.2.download.tmp");
        std::fs::write(&recent, b"x").unwrap();
        let elf_file = d.0.join("kstuff.elf");
        std::fs::write(&elf_file, b"x").unwrap();
        sweep_stale_tmps(&d.0);
        assert!(!old.exists());
        assert!(recent.exists());
        assert!(elf_file.exists());
    }

    #[test]
    fn a_release_is_found_by_tag_and_ensure_cached_keeps_the_right_version() {
        let (_d, http, lib) = library("ensure");
        let list = format!(
            r#"[{{"tag_name":"v2","assets":[{{"name":"kstuff.elf","browser_download_url":"https://github.com/EchoStretch/kstuff-lite/releases/download/v2/kstuff.elf","size":4}}]}},
                {{"tag_name":"v1","assets":[{{"name":"kstuff.elf","browser_download_url":"{KSTUFF_ASSET}","size":4}}]}}]"#
        );
        http.ok(
            "https://api.github.com/repos/EchoStretch/kstuff-lite/releases?per_page=30",
            list.as_bytes(),
        );
        http.ok(KSTUFF_ASSET, &elf(1, 100));
        http.ok(
            "https://github.com/EchoStretch/kstuff-lite/releases/download/v2/kstuff.elf",
            &elf(2, 200),
        );
        let p = lib.ensure_cached(KSTUFF_ID, Some("v1")).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), elf(1, 100));
        let asked = http.asked.lock().unwrap().len();
        // Cached: no version named, or the same one, is served from disk.
        assert_eq!(lib.ensure_cached(KSTUFF_ID, None).unwrap(), p);
        assert_eq!(lib.ensure_cached(KSTUFF_ID, Some("v1")).unwrap(), p);
        assert_eq!(http.asked.lock().unwrap().len(), asked);
        let p2 = lib.ensure_cached(KSTUFF_ID, Some("v2")).unwrap();
        assert_eq!(std::fs::read(&p2).unwrap(), elf(2, 200));
        let e = lib.ensure_cached(KSTUFF_ID, Some("v9")).unwrap_err();
        assert_eq!(e.code, "unknown_release");
        assert_eq!(
            lib.ensure_cached("nope", None).unwrap_err().code,
            "unknown_payload"
        );
    }
}
