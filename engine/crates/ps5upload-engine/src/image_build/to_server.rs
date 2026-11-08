//! Convert straight to a server: an image build whose bytes go to a saved server (SMB, NFS,
//! FTP, SFTP, WebDAV) as they are written, so the image never exists on this computer.
//!
//! The same writers and the same plan as a build here or to the console: every plain writer
//! goes front to back and knows its size once planned, and with the time pinned (a PFS image's
//! inodes, a generated index) the same game gives the same bytes on every run. The image is
//! written as `<name>.partial` on the server through [`crate::server_out::Upload`], read back
//! there through its own reader (`RemoteFile::read_at`, a sample or every byte), and only then
//! renamed to its name, never over anything already there. A failure or a cancel removes the
//! partial file.
//!
//! Going on. A write that was cut off (the server went away, the engine was stopped) leaves the
//! partial file and, in the engine's data folder, a record of the plan it was written for and
//! the time pinned into it. On SMB, NFS and SFTP, which write at an offset, the next try with the
//! same plan writes on from a little before the partial file's end (the last bytes a pipelined
//! write sent may not all have landed) instead of from the start, and then reads every byte
//! back. A dropped connection during a build is tried again the same way, three times. FTP and
//! WebDAV write a file whole: they start over.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ps5upload_fpkg::ffpfsc_block::Level;
use ps5upload_fpkg::sample::VerifyMode;
use ps5upload_fpkg::source::SourceTree;

use super::{resume_key, verify, HashingTree, ImageBuilt, ImageFormat, Planned};
use crate::remote::RemoteError;
use crate::server_out::{said, ServerDest, Upload};

/// How far before the partial file's end a write goes on from: SMB sends several writes at
/// once, and the last of them may not all have landed.
const GO_BACK: u64 = if cfg!(test) { 1 << 20 } else { 64 << 20 };
/// Where a write goes on from is a multiple of this.
const ALIGN: u64 = 1 << 20;
/// Waits before trying a dropped connection again.
const RETRIES: [Duration; 3] = if cfg!(test) {
    [
        Duration::from_millis(10),
        Duration::from_millis(50),
        Duration::from_millis(200),
    ]
} else {
    [
        Duration::from_secs(2),
        Duration::from_secs(5),
        Duration::from_secs(10),
    ]
};

/// What a conversion to a server did.
pub(crate) struct ToServer {
    pub built: ImageBuilt,
    /// The byte the write went on from (0: from the start).
    pub resumed_from: u64,
}

/// What is kept between tries of one image to one place on one server: the time pinned into it,
/// the plan (resume key) its partial file was written for, and whether this engine made that
/// partial file at all. An empty key: not one to go on from.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Record {
    time: i64,
    #[serde(default)]
    key: String,
    /// This engine's upload made `<name>.partial` (set once `Upload::start` has created it). The
    /// time alone is written before any byte is ([`server_stamp`]), so a record without this says
    /// nothing about a partial file already on the server: that one is someone else's, and is
    /// never written over. A record from before the field reads as not ours.
    #[serde(default)]
    owned: bool,
}

fn record_file(data: &Path, id: &str, path: &str) -> PathBuf {
    let mut h = blake3::Hasher::new();
    h.update(id.as_bytes());
    h.update(b"\0");
    h.update(path.as_bytes());
    data.join("convert-to-server")
        .join(format!("{}.json", &h.finalize().to_hex()[..32]))
}

fn load_record(data: Option<&Path>, id: &str, path: &str) -> Option<Record> {
    let file = record_file(data?, id, path);
    let fresh = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < super::RESUME_KEPT);
    if !fresh {
        return None;
    }
    serde_json::from_slice(&std::fs::read(&file).ok()?).ok()
}

fn save_record(data: Option<&Path>, id: &str, path: &str, record: &Record) {
    let Some(data) = data else {
        return;
    };
    let file = record_file(data, id, path);
    let _ = std::fs::create_dir_all(file.parent().unwrap_or(data));
    if let Ok(bytes) = serde_json::to_vec(record) {
        let _ = std::fs::write(&file, bytes);
    }
}

/// The time an image written to `path` on the server `id` carries: the one an unfinished earlier
/// try used, so starting again gives the same bytes; else now, kept for the next try.
pub(crate) fn server_stamp(data: Option<&Path>, id: &str, path: &str) -> i64 {
    if let Some(r) = load_record(data, id, path) {
        return r.time;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    save_record(
        data,
        id,
        path,
        &Record {
            time: now,
            key: String::new(),
            owned: false,
        },
    );
    now
}

/// The image is on the server under its name, or nothing of it is: nothing to keep.
pub(crate) fn forget_server_stamp(data: Option<&Path>, id: &str, path: &str) {
    if let Some(data) = data {
        let _ = std::fs::remove_file(record_file(data, id, path));
    }
}

/// Where a write that left `held` bytes goes on from.
fn go_on_from(held: u64, size: u64) -> u64 {
    if held > size {
        return 0;
    }
    held.saturating_sub(GO_BACK) / ALIGN * ALIGN
}

/// Where the image goes on the server, and where its record is kept.
pub(crate) struct ServerImage<'a> {
    pub dest: &'a ServerDest,
    /// The image's name in `dest.dir`.
    pub name: String,
    /// The engine's data folder (the record); `None` keeps nothing.
    pub data: Option<PathBuf>,
}

impl ServerImage<'_> {
    pub(crate) fn path(&self) -> String {
        self.dest.path_of(&self.name)
    }

    fn partial(&self) -> String {
        format!("{}.partial", self.path())
    }

    /// The time and plan of this write, before it starts. Whether the partial file is ours stays
    /// as it was: only an upload that made it says so ([`Self::own`]).
    fn keep(&self, time: i64, key: &str) {
        let owned =
            load_record(self.data.as_deref(), &self.dest.id, &self.path()).is_some_and(|r| r.owned);
        save_record(
            self.data.as_deref(),
            &self.dest.id,
            &self.path(),
            &Record {
                time,
                key: key.to_string(),
                owned,
            },
        );
    }

    /// Our upload has made the partial file: a later try may go on from it, or write over it.
    fn own(&self, time: i64, key: &str) {
        save_record(
            self.data.as_deref(),
            &self.dest.id,
            &self.path(),
            &Record {
                time,
                key: key.to_string(),
                owned: true,
            },
        );
    }

    fn forget(&self) {
        forget_server_stamp(self.data.as_deref(), &self.dest.id, &self.path());
    }
}

/// Plans `source` as a `format` image, streams it to the server as `<name>.partial`, reads it
/// back there (`mode`, `verify_chunk` as for a local build) and renames it to its name. `time`
/// pins a PFS image's inodes and must be the time any generated index in `source` carries
/// ([`server_stamp`]); `extra` adds what else the bytes depend on to the plan (a zstd build's
/// level and emulator). `stage` reports `plan`, `write` and `verify`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stream_to_server(
    format: ImageFormat,
    time: i64,
    extra: &str,
    source: &mut dyn SourceTree,
    target: &ServerImage,
    mode: VerifyMode,
    verify_chunk: u64,
    cancel: &Arc<AtomicBool>,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<ToServer, String> {
    if source.files().is_empty() {
        return Err(format!("{} has no files in it", source.describe()));
    }
    let dest = target.dest;
    let started = std::time::Instant::now();
    stage("plan", 0, 0);
    let planned = Planned::plan(format, &*source, Some(time), cancel)?;
    let size = planned.image_size();
    let want_sizes: HashMap<String, u64> = source
        .files()
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    let want_empty: Vec<String> = source.empty_dirs().to_vec();
    let data: u64 = want_sizes.values().sum();
    let key = format!(
        "{}\0{extra}",
        resume_key(
            format,
            None,
            Level::default(),
            Some(time),
            &want_sizes,
            size
        )
    );
    let (path, partial) = (target.path(), target.partial());
    let mut fs = dest.fs().map_err(|e| said(&e))?;
    dest.make_dir(&*fs)
        .map_err(|e| format!("making {} on {}: {}", dest.dir, dest.label, said(&e)))?;
    if dest.exists(&*fs, &path).map_err(|e| said(&e))? {
        return Err(format!(
            "{path} already exists on {}: delete it there or give the image another name",
            dest.label
        ));
    }
    // A partial file is gone on from only when this engine wrote it for this very plan.
    let mut skip = match dest.file_size(&*fs, &partial).map_err(|e| said(&e))? {
        None => 0,
        Some(held) => {
            match load_record(target.data.as_deref(), &dest.id, &path).filter(|r| r.owned) {
                None => {
                    return Err(format!(
                        "{partial} already exists on {} and this app did not write it: delete it \
                         there or give the image another name",
                        dest.label
                    ))
                }
                Some(r) if r.key == key && fs.writes_from() => go_on_from(held, size),
                // Ours, for another plan or a server that writes whole: written over.
                Some(_) => 0,
            }
        }
    };
    dest.room_check(&*fs, size - skip)?;
    target.keep(time, &key);
    let first_skip = skip;
    crate::log_at!(
        Info,
        "convert",
        "to server: {} as {} ({size} bytes, {} files) -> {} {partial}{}",
        source.describe(),
        format.extension(),
        want_sizes.len(),
        dest.label,
        if skip > 0 {
            format!(", going on from {skip} bytes")
        } else {
            String::new()
        }
    );

    let mut hashing = HashingTree::new(source);
    // The source's hashes, once a write has read it all.
    let mut hashes: Option<HashMap<String, Vec<blake3::Hash>>> = None;
    let mut tries = 0usize;
    // Two rounds at most: one that went on from a partial file and failed its read-back starts
    // over once, as a local build does.
    let mut round = 0;
    let (summary, sent_at) = loop {
        // The write, tried again (on from what landed) when the connection drops.
        loop {
            let up = Upload::start(dest, Arc::clone(&fs), &partial, size, skip, cancel.clone());
            if up.is_ok() {
                // The partial file there is ours from now on (made, or gone on from).
                target.own(time, &key);
            }
            let written = up.map_err(|e| (said(&e), Some(e))).and_then(|mut up| {
                let n = planned
                    .write(&mut hashing, &mut up, skip, cancel, &mut |d, t| {
                        stage("write", d, t)
                    })
                    .map_err(|e| (e.to_string(), None))?;
                if n != size || up.position() != size {
                    return Err((
                        format!("the writer made {n} bytes of the planned {size}"),
                        None,
                    ));
                }
                up.finish().map_err(|e| (said(&e), Some(e)))
            });
            let (msg, remote) = match written {
                Ok(()) => break,
                Err(e) => e,
            };
            if cancel.load(Ordering::Relaxed) {
                return Err(cancelled(dest, &*fs, &partial, target));
            }
            // A dropped connection (the writer's own error says only that the upload stopped:
            // the server's reason is the one that counts).
            let transient = remote
                .as_ref()
                .is_some_and(crate::remote::pool::is_transient)
                || msg.contains("connection")
                || msg.contains("took nothing");
            if !transient || tries >= RETRIES.len() {
                return Err(failed(&msg, dest, &*fs, &partial, target, time));
            }
            crate::log_at!(
                Warn,
                "convert",
                "to server: {msg}; trying again in {} s",
                RETRIES[tries].as_secs()
            );
            std::thread::sleep(RETRIES[tries]);
            tries += 1;
            if cancel.load(Ordering::Relaxed) {
                return Err(cancelled(dest, &*fs, &partial, target));
            }
            dest.reconnect();
            fs = match dest.fs() {
                Ok(f) => f,
                Err(e) if tries < RETRIES.len() => {
                    crate::log_at!(Warn, "convert", "to server: signing in again: {e}");
                    continue;
                }
                Err(e) => return Err(format!("{msg}; signing in again failed: {}", said(&e))),
            };
            skip = match dest.file_size(&*fs, &partial) {
                Ok(Some(held)) if fs.writes_from() => go_on_from(held, size),
                _ => 0,
            };
        }
        let sent_at = started.elapsed();
        // Going on from what was there, every byte is read back: the plan cannot see what
        // happened to the bytes it did not write this time.
        let mode = if skip > 0 || first_skip > 0 {
            VerifyMode::Full
        } else {
            mode
        };
        if hashes.is_none() {
            match hashing.hashes_now(cancel) {
                Ok(h) => hashes = Some(h),
                Err(_) if cancel.load(Ordering::Relaxed) => {
                    return Err(cancelled(dest, &*fs, &partial, target))
                }
                Err(e) => return Err(failed(&e, dest, &*fs, &partial, target, time)),
            }
        }
        let checked = read_back(
            dest,
            &partial,
            size,
            format,
            hashes.as_ref().unwrap_or(&HashMap::new()),
            &want_sizes,
            &want_empty,
            mode,
            verify_chunk,
            cancel,
            stage,
        );
        match checked {
            Ok(s) => break (s, sent_at),
            Err(_) if cancel.load(Ordering::Relaxed) => {
                return Err(cancelled(dest, &*fs, &partial, target))
            }
            Err(e) if round == 0 && (first_skip > 0 || skip > 0) => {
                crate::log_at!(
                    Warn,
                    "convert",
                    "to server: going on from {first_skip} bytes failed its read-back ({e}); \
                     writing {partial} again from the start"
                );
                round += 1;
                skip = 0;
                tries = 0;
                // The bytes went on from a partial file that was not what the plan said: gone.
                let _ = dest.block_on(fs.remove_file(&partial));
                continue;
            }
            Err(e) => {
                let removed = dest.remove(&*fs, &partial);
                // Ours still, if it could not be removed, but not one to go on from.
                if removed.starts_with("removing") {
                    target.keep(time, "");
                } else {
                    target.forget();
                }
                return Err(format!("{e}; {removed}"));
            }
        }
    };

    // Into its name, never over anything: a name taken meanwhile keeps what took it.
    if let Err(e) = dest.block_on(fs.rename(&partial, &path)) {
        let removed = dest.remove(&*fs, &partial);
        target.forget();
        return Err(match e {
            RemoteError::Exists(_) => format!(
                "{path} appeared on {} while the image was written, and it was left as it is; \
                 the new image was not kept: {removed}",
                dest.label
            ),
            e => format!("renaming {partial} to {path}: {}; {removed}", said(&e)),
        });
    }
    target.forget();
    crate::engine_log::record(
        "info",
        format!(
            "image to server: format={} files={} data_bytes={data} image_bytes={size} resumed_from={first_skip} retries={tries} send_ms={} verify_ms={} verify={} dest={}",
            format.extension(),
            want_sizes.len(),
            sent_at.as_millis(),
            (started.elapsed() - sent_at).as_millis(),
            super::log_line(&summary),
            dest.url(&path),
        ),
    );
    let mut detail = format!(
        "{}, {} files, {data} data bytes, written to {}, {}",
        format.extension(),
        want_sizes.len(),
        dest.label,
        summary.describe()
    );
    if let Some(free) = planned.free_bytes() {
        detail.push_str(&format!(", {free} bytes free"));
    }
    if first_skip > 0 {
        detail.push_str(&format!(
            ", went on from {first_skip} bytes after an interruption"
        ));
    }
    Ok(ToServer {
        built: ImageBuilt {
            image_bytes: size,
            files: want_sizes.len() as u64,
            detail,
        },
        resumed_from: first_skip,
    })
}

/// The partial image read back on the server through its own reader and compared with the
/// source's hashes.
#[allow(clippy::too_many_arguments)]
fn read_back(
    dest: &ServerDest,
    partial: &str,
    size: u64,
    format: ImageFormat,
    hashes: &HashMap<String, Vec<blake3::Hash>>,
    want_sizes: &HashMap<String, u64>,
    want_empty: &[String],
    mode: VerifyMode,
    verify_chunk: u64,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<ps5upload_fpkg::sample::VerifySummary, String> {
    let files = dest.files().map_err(|e| said(&e))?;
    let (got, _) = files
        .stat(partial)
        .map_err(|e| format!("the image could not be read back on {}: {e}", dest.label))?;
    if got != size {
        return Err(format!(
            "the image is {got} bytes on {} where {size} were written",
            dest.label
        ));
    }
    let mut image =
        ps5upload_fpkg::remote_source::open_remote_as(files, partial, size, format.extension())
            .map_err(|e| format!("the image could not be read back on {}: {e}", dest.label))?;
    verify(
        &mut *image,
        hashes,
        want_sizes,
        want_empty,
        mode,
        verify_chunk,
        cancel,
        &mut |d, t| stage("verify", d, t),
    )
}

/// A cancel: the partial file goes (and its record, unless it could not be removed: the next
/// try may go on from it).
fn cancelled(
    dest: &ServerDest,
    fs: &dyn crate::remote::RemoteFs,
    partial: &str,
    target: &ServerImage,
) -> String {
    let removed = dest.remove(fs, partial);
    if !removed.starts_with("removing") {
        target.forget();
    }
    crate::log_at!(Info, "convert", "to server: cancelled ({removed})");
    "cancelled".into()
}

/// A failed write: the partial file goes. If it cannot (the server is gone), it and its record
/// stay, and the next try goes on from it.
fn failed(
    msg: &str,
    dest: &ServerDest,
    fs: &dyn crate::remote::RemoteFs,
    partial: &str,
    target: &ServerImage,
    _time: i64,
) -> String {
    let removed = dest.remove(fs, partial);
    if !removed.starts_with("removing") {
        target.forget();
        return format!("{msg}; {removed}");
    }
    format!(
        "{msg}; the partial image {partial} could not be removed, and the next try of this same \
         conversion goes on from it"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cut_off_write_goes_on_from_a_whole_mebibyte_before_its_end() {
        assert_eq!(go_on_from(0, 100 << 20), 0);
        assert_eq!(go_on_from((1 << 20) + 5, 100 << 20), 0);
        assert_eq!(go_on_from((10 << 20) + 5, 100 << 20), 9 << 20);
        // More than the plan: not this plan's file.
        assert_eq!(go_on_from(101 << 20, 100 << 20), 0);
    }

    #[test]
    fn the_time_is_kept_until_the_image_is_done() {
        let data = crate::remote::store::test_dir();
        let t1 = server_stamp(Some(&data), "c1", "/g/G.exfat");
        save_record(
            Some(&data),
            "c1",
            "/g/G.exfat",
            &Record {
                time: t1 - 100,
                key: "k".into(),
                owned: true,
            },
        );
        assert_eq!(server_stamp(Some(&data), "c1", "/g/G.exfat"), t1 - 100);
        // Another place has its own.
        assert_ne!(server_stamp(Some(&data), "c2", "/g/G.exfat"), t1 - 100);
        forget_server_stamp(Some(&data), "c1", "/g/G.exfat");
        assert!(load_record(Some(&data), "c1", "/g/G.exfat").is_none());
    }
}
