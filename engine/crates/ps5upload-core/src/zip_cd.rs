//! Zero-seek ZIP central-directory parser.
//!
//! The `zip` 2.x crate keeps per-entry metadata in memory after
//! `ZipArchive::new`, but its public API forces a seek-per-entry to read
//! it back: `by_index_raw(i)` calls `find_content`, which seeks to the
//! local file header just to return a wrapper over data we already have
//! in `Shared.files[i]`. That `pub(crate)` field is invisible to us, so
//! the only zero-seek view the crate exposes is `file_names()` —
//! sufficient for inspect but not for plan, which needs per-entry
//! `uncompressed_size`.
//!
//! In practice this was the bottleneck: planning a 6.7 GB / 1,548-entry
//! zip from exFAT external HDD took 17 s (1,548 sequential disk seeks).
//! A 70 GB game dump with 80–100k files routinely blows the Tauri
//! client's 60 s deadline with the misleading "engine request failed:
//! error sending request" symptom. The pattern matches what the
//! destructive trio hit pre-2.18.4.
//!
//! This module reads the End of Central Directory record (plus a ZIP64
//! locator + ZIP64 EOCD where present), then slurps the central
//! directory into memory in one read and walks the CDFH records
//! linearly. For a 1,548-entry zip the central directory is ~150 KB
//! and parses in ~1 ms once cached. The only file IO is:
//! 1. one stat
//! 2. one read of the last 64 KB to find the EOCD signature
//! 3. one read of the ZIP64 EOCD record (~56 bytes) if the archive is ZIP64
//! 4. one bulk read of the central directory itself
//!
//! Compatible with PKWARE APPNOTE.TXT 6.3.10 §4.3.12, §4.3.16, §4.5.3.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};

/// One central-directory entry, central-dir-only fields. No local-header
/// data (we never seek there).
#[derive(Debug, Clone)]
pub struct CentralDirEntry {
    /// Entry name as stored. Cleaned to valid UTF-8 (invalid bytes
    /// replaced with U+FFFD via `from_utf8_lossy`). Trailing '/'
    /// preserved so `is_dir` can match the `zip` crate's behaviour.
    pub name: String,
    /// Uncompressed size in bytes. ZIP64 extra-field decoded where
    /// applicable.
    pub uncompressed_size: u64,
    /// True if the name ends with '/'. Matches `ZipFile::is_dir`.
    pub is_dir: bool,
}

/// End of Central Directory record signature.
const EOCD_SIG: u32 = 0x0605_4b50;
/// ZIP64 EOCD locator signature.
const ZIP64_EOCD_LOCATOR_SIG: u32 = 0x0706_4b50;
/// ZIP64 End of Central Directory record signature.
const ZIP64_EOCD_SIG: u32 = 0x0606_4b50;
/// Central directory file header signature.
const CDFH_SIG: u32 = 0x0201_4b50;

/// Max EOCD comment length is 64 KiB per spec; the EOCD record itself is
/// 22 bytes. Scan that window backward from end of file to find the sig.
const EOCD_SCAN_WINDOW: u64 = 65_557; // 64 KiB + 22 + small slack

/// CDFH fixed-portion length (sig + 42 bytes of fields).
const CDFH_FIXED_LEN: usize = 46;

/// Sentinel value placed in 32-bit CDFH fields when the real value is in
/// the ZIP64 extra field.
const ZIP64_SENTINEL_U32: u32 = 0xFFFF_FFFF;
const ZIP64_SENTINEL_U16: u16 = 0xFFFF;

/// ZIP64 extra-field header id.
const ZIP64_EXTRA_ID: u16 = 0x0001;

/// Parse the central directory of `zip_path` and return every entry's
/// name + uncompressed size + is_dir flag, with zero per-entry seeks.
///
/// Errors propagate file-IO problems and structural-corruption issues
/// (missing EOCD, CDFH signature mismatch, truncated record). Unknown
/// compression methods do **not** error here — the parser only reads
/// metadata, so an unsupported-method archive parses fine and the clear
/// error surfaces later at transfer time (matches the post-2.14 split).
pub fn read_central_directory(zip_path: &Path) -> Result<Vec<CentralDirEntry>> {
    read_central_directory_with_progress(zip_path, |_| {})
}

/// `read_central_directory` variant that calls `on_progress(entries_seen)`
/// every 1,000 entries so the caller can emit heartbeats / progress
/// events while the central directory is walked. The walk itself is
/// in-memory after one bulk read, so this is purely so a streaming HTTP
/// handler can prove forward progress to a client watchdog.
pub fn read_central_directory_with_progress(
    zip_path: &Path,
    mut on_progress: impl FnMut(u64),
) -> Result<Vec<CentralDirEntry>> {
    let mut file = std::fs::File::open(zip_path)
        .with_context(|| format!("open zip {}", zip_path.display()))?;
    let file_len = file
        .metadata()
        .with_context(|| format!("stat zip {}", zip_path.display()))?
        .len();
    if file_len < 22 {
        bail!("zip too short ({file_len} bytes): no room for EOCD record");
    }

    let (cd_offset, cd_size, total_entries) = locate_central_directory(&mut file, file_len)?;
    if cd_size == 0 {
        return Ok(Vec::new());
    }
    if cd_offset
        .checked_add(cd_size)
        .is_none_or(|end| end > file_len)
    {
        bail!(
            "central directory ({cd_size} bytes @ {cd_offset}) extends past end of file \
             ({file_len} bytes)"
        );
    }

    let mut cd_buf = vec![0u8; cd_size as usize];
    file.seek(SeekFrom::Start(cd_offset))
        .context("seek to central directory")?;
    file.read_exact(&mut cd_buf)
        .context("read central directory")?;

    let mut entries: Vec<CentralDirEntry> = Vec::with_capacity(total_entries as usize);
    let mut cursor = 0usize;
    // The CDFH count from EOCD is advisory in spec — some real-world
    // zippers (notably old InfoZip with bit-3 streaming) under-report it.
    // Walk by signature until we run out of bytes or hit a non-CDFH sig.
    while cursor + CDFH_FIXED_LEN <= cd_buf.len() {
        let sig = u32_le(&cd_buf, cursor);
        if sig != CDFH_SIG {
            // Trailing data after the last CDFH (digital sig record etc)
            // — stop cleanly rather than bailing, mirroring the `zip`
            // crate's tolerance.
            break;
        }
        let raw_uncompressed = u32_le(&cd_buf, cursor + 24);
        let name_len = u16_le(&cd_buf, cursor + 28) as usize;
        let extra_len = u16_le(&cd_buf, cursor + 30) as usize;
        let comment_len = u16_le(&cd_buf, cursor + 32) as usize;
        let record_len = CDFH_FIXED_LEN + name_len + extra_len + comment_len;
        if cursor + record_len > cd_buf.len() {
            bail!("CDFH at offset {cursor} runs past end of central directory");
        }
        let name_bytes = &cd_buf[cursor + CDFH_FIXED_LEN..cursor + CDFH_FIXED_LEN + name_len];
        let extra_bytes = &cd_buf
            [cursor + CDFH_FIXED_LEN + name_len..cursor + CDFH_FIXED_LEN + name_len + extra_len];

        // The CDFH stores the (potentially-truncated) compressed and
        // uncompressed sizes in 32-bit fields. For >4 GiB entries the
        // value 0xFFFFFFFF is a sentinel meaning "look in the ZIP64
        // extended-info extra field". Game dumps frequently include
        // multi-GB files (eboot bins, asset archives), so handling
        // this is mandatory, not optional.
        let uncompressed_size = if raw_uncompressed == ZIP64_SENTINEL_U32 {
            zip64_uncompressed_from_extra(extra_bytes, &cd_buf, cursor).ok_or_else(|| {
                anyhow!(
                    "CDFH at offset {cursor}: uncompressed_size sentinel but no \
                     ZIP64 extended-info extra field present"
                )
            })?
        } else {
            u64::from(raw_uncompressed)
        };

        let name = String::from_utf8_lossy(name_bytes).into_owned();
        let is_dir = name.ends_with('/');
        entries.push(CentralDirEntry {
            name,
            uncompressed_size,
            is_dir,
        });

        cursor += record_len;
        if entries.len().is_multiple_of(1000) {
            on_progress(entries.len() as u64);
        }
    }

    // Final tick so a small archive still emits at least one progress
    // event — without it, watchdogs that fire on "no event for N seconds"
    // never see the final result-burst on a sub-1000-entry zip.
    on_progress(entries.len() as u64);
    Ok(entries)
}

/// Locate the central directory: scan for EOCD, then promote to ZIP64
/// EOCD if any sentinel value is set. Returns `(cd_offset, cd_size,
/// total_entries)`.
fn locate_central_directory(file: &mut std::fs::File, file_len: u64) -> Result<(u64, u64, u64)> {
    let scan_len = EOCD_SCAN_WINDOW.min(file_len) as usize;
    let scan_start = file_len - scan_len as u64;
    let mut tail = vec![0u8; scan_len];
    file.seek(SeekFrom::Start(scan_start))
        .context("seek to EOCD scan window")?;
    file.read_exact(&mut tail)
        .context("read EOCD scan window")?;

    // Scan backward for the EOCD signature. The record is 22 bytes
    // followed by an optional comment, so the sig sits at scan_len -
    // 22 - comment_len. Iterate i from high to low.
    let mut eocd_off_in_tail: Option<usize> = None;
    if tail.len() >= 22 {
        for i in (0..=tail.len() - 22).rev() {
            if u32_le(&tail, i) == EOCD_SIG {
                let comment_len = u16_le(&tail, i + 20) as usize;
                // Sanity: comment must fit in the scan window after the
                // EOCD record. If it doesn't, this is a false-positive
                // signature inside a comment / archive data; keep scanning.
                if i + 22 + comment_len <= tail.len() {
                    eocd_off_in_tail = Some(i);
                    break;
                }
            }
        }
    }
    let eocd_off_in_tail = eocd_off_in_tail
        .ok_or_else(|| anyhow!("no EOCD signature found in last {scan_len} bytes — not a zip?"))?;
    let eocd_abs = scan_start + eocd_off_in_tail as u64;

    let total_entries_u16 = u16_le(&tail, eocd_off_in_tail + 10);
    let cd_size_u32 = u32_le(&tail, eocd_off_in_tail + 12);
    let cd_offset_u32 = u32_le(&tail, eocd_off_in_tail + 16);

    let needs_zip64 = total_entries_u16 == ZIP64_SENTINEL_U16
        || cd_size_u32 == ZIP64_SENTINEL_U32
        || cd_offset_u32 == ZIP64_SENTINEL_U32;

    if !needs_zip64 {
        return Ok((
            u64::from(cd_offset_u32),
            u64::from(cd_size_u32),
            u64::from(total_entries_u16),
        ));
    }

    // ZIP64 EOCD locator sits immediately before the EOCD. It's a
    // fixed 20-byte record carrying the absolute offset of the ZIP64
    // EOCD record.
    if eocd_abs < 20 {
        bail!("ZIP64 sentinel set but no room for ZIP64 EOCD locator before EOCD");
    }
    let locator_abs = eocd_abs - 20;
    // Did the 64 KiB scan window already cover the locator? Usually yes.
    let locator_bytes: [u8; 20] = if eocd_off_in_tail >= 20 {
        tail[eocd_off_in_tail - 20..eocd_off_in_tail]
            .try_into()
            .expect("slice is 20 bytes")
    } else {
        let mut buf = [0u8; 20];
        file.seek(SeekFrom::Start(locator_abs))
            .context("seek to ZIP64 EOCD locator")?;
        file.read_exact(&mut buf)
            .context("read ZIP64 EOCD locator")?;
        buf
    };
    if u32_le(&locator_bytes, 0) != ZIP64_EOCD_LOCATOR_SIG {
        bail!(
            "expected ZIP64 EOCD locator signature at offset {locator_abs}, found {:#010x}",
            u32_le(&locator_bytes, 0)
        );
    }
    let zip64_eocd_off = u64_le(&locator_bytes, 8);
    if zip64_eocd_off + 56 > file_len {
        bail!("ZIP64 EOCD offset {zip64_eocd_off} runs past end of file");
    }

    let mut zip64_eocd = [0u8; 56];
    file.seek(SeekFrom::Start(zip64_eocd_off))
        .context("seek to ZIP64 EOCD")?;
    file.read_exact(&mut zip64_eocd)
        .context("read ZIP64 EOCD")?;
    if u32_le(&zip64_eocd, 0) != ZIP64_EOCD_SIG {
        bail!(
            "expected ZIP64 EOCD signature at offset {zip64_eocd_off}, found {:#010x}",
            u32_le(&zip64_eocd, 0)
        );
    }
    let total_entries = u64_le(&zip64_eocd, 32);
    let cd_size = u64_le(&zip64_eocd, 40);
    let cd_offset = u64_le(&zip64_eocd, 48);
    Ok((cd_offset, cd_size, total_entries))
}

/// Walk the extra-field blob looking for the ZIP64 extended-info
/// record (header id 0x0001). When the CDFH sentinel for
/// uncompressed_size is set, the real 64-bit value lives at the
/// start of the ZIP64 extra's payload (spec §4.5.3 ordering rule).
///
/// `cd_buf` and `record_off` are passed only for the error-message
/// offset; the caller could compute it but threading the context here
/// is cheaper than reconstructing it.
fn zip64_uncompressed_from_extra(extra: &[u8], _cd_buf: &[u8], _record_off: usize) -> Option<u64> {
    let mut cursor = 0;
    while cursor + 4 <= extra.len() {
        let id = u16_le(extra, cursor);
        let size = u16_le(extra, cursor + 2) as usize;
        let payload_start = cursor + 4;
        let payload_end = payload_start + size;
        if payload_end > extra.len() {
            return None;
        }
        if id == ZIP64_EXTRA_ID && size >= 8 {
            // §4.5.3: the ZIP64 extra holds 8-byte uncompressed_size,
            // 8-byte compressed_size, 8-byte local-header offset,
            // 4-byte disk start, in that order, BUT only the fields
            // whose CDFH-side value was sentinel are present. We need
            // the first 8 bytes when the CDFH's 32-bit
            // uncompressed_size was 0xFFFFFFFF — which is the
            // condition the caller already checked.
            return Some(u64_le(extra, payload_start));
        }
        cursor = payload_end;
    }
    None
}

#[inline]
fn u16_le(buf: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([buf[at], buf[at + 1]])
}

#[inline]
fn u32_le(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

#[inline]
fn u64_le(buf: &[u8], at: usize) -> u64 {
    u64::from_le_bytes([
        buf[at],
        buf[at + 1],
        buf[at + 2],
        buf[at + 3],
        buf[at + 4],
        buf[at + 5],
        buf[at + 6],
        buf[at + 7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Build a unique temp-file path for one test. We deliberately avoid
    /// adding a `tempfile` dev-dep since nothing else in the workspace
    /// uses one — a per-test PID+counter+nanosec name in `std::env::temp_dir()`
    /// is enough, paired with `TmpGuard` for cleanup on Drop.
    fn tmp_zip_path() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "ps5upload-zipcd-test-{}-{}-{}.zip",
            std::process::id(),
            n,
            nanos
        ))
    }

    struct TmpGuard(PathBuf);
    impl Drop for TmpGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Build a small zip via the `zip` crate, then verify our parser
    /// agrees on names + uncompressed sizes. Uses a temp file because
    /// the zip crate writes via a `Seek + Write` and we need to round-trip
    /// the EOCD locator search.
    #[test]
    fn parses_small_zip_matches_zip_crate() {
        use std::io::{Cursor, Write};
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zw.start_file("hello.txt", opts).unwrap();
            zw.write_all(b"hello world").unwrap();
            zw.start_file("sub/dir/file.bin", opts).unwrap();
            zw.write_all(&[0x42; 1024]).unwrap();
            zw.add_directory("emptydir/", opts).unwrap();
            zw.finish().unwrap();
        }
        let tmp = tmp_zip_path();
        std::fs::write(&tmp, &buf).unwrap();
        let _g = TmpGuard(tmp.clone());
        let entries = read_central_directory(&tmp).unwrap();
        // Three CDFH records: two files + one directory.
        assert_eq!(entries.len(), 3);
        let by_name: std::collections::HashMap<_, _> =
            entries.iter().map(|e| (e.name.as_str(), e)).collect();
        assert_eq!(by_name["hello.txt"].uncompressed_size, 11);
        assert!(!by_name["hello.txt"].is_dir);
        assert_eq!(by_name["sub/dir/file.bin"].uncompressed_size, 1024);
        assert_eq!(by_name["emptydir/"].uncompressed_size, 0);
        assert!(by_name["emptydir/"].is_dir);
    }

    /// Synthetic test that the EOCD scan handles a comment field. A 5
    /// KiB comment forces a non-trivial scan-window position.
    #[test]
    fn parses_zip_with_eocd_comment() {
        use std::io::{Cursor, Write};
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(Cursor::new(&mut buf));
            zw.set_raw_comment(vec![b'X'; 5000].into_boxed_slice());
            let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zw.start_file("only.txt", opts).unwrap();
            zw.write_all(b"data").unwrap();
            zw.finish().unwrap();
        }
        let tmp = tmp_zip_path();
        std::fs::write(&tmp, &buf).unwrap();
        let _g = TmpGuard(tmp.clone());
        let entries = read_central_directory(&tmp).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "only.txt");
        assert_eq!(entries[0].uncompressed_size, 4);
    }

    /// Progress callback fires at the final entry count even for small
    /// inputs, so watchdog clients always see at least one event.
    #[test]
    fn progress_callback_fires_final_tick() {
        use std::io::{Cursor, Write};
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for i in 0..5 {
                zw.start_file(format!("f{i}.txt"), opts).unwrap();
                zw.write_all(b"x").unwrap();
            }
            zw.finish().unwrap();
        }
        let tmp = tmp_zip_path();
        std::fs::write(&tmp, &buf).unwrap();
        let _g = TmpGuard(tmp.clone());
        let mut seen: Vec<u64> = Vec::new();
        let entries = read_central_directory_with_progress(&tmp, |n| seen.push(n)).unwrap();
        assert_eq!(entries.len(), 5);
        assert_eq!(seen.last().copied(), Some(5));
    }
}
