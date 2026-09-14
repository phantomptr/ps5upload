//! The build pipeline: a source folder to a verified `.pkg`.
//!
//! One sequential pass over the source bytes: sizes come from `stat`, the plan fixes every
//! offset before the first read, and each file is read once while its image is filled. The
//! output lands as `<name>.pkg.partial`, is verified by this crate's own reader, and is
//! only then renamed.

use std::path::{Path, PathBuf};

use crate::cnt_write::{self, CntParams};
use crate::crypto::sha3;
use crate::fih_write::{self, FihParams};
use crate::inner;
use crate::naps;
use crate::outer_write;
use crate::pfsimage;
use crate::plan::{self, Plan};
use crate::si_write;
use crate::source::{self, SourceFile};
use crate::stream;
use crate::verify;
use crate::{format_err, Result, BLOCK};

/// What to build and where.
#[derive(Debug, Clone)]
pub struct BuildRequest {
    pub source: PathBuf,
    pub output_dir: PathBuf,
    /// Overrides `param.json`'s content id when set.
    pub content_id: Option<String>,
    /// Output file stem; the content id by default.
    pub file_name: Option<String>,
    pub passcode: String,
    /// Build timestamp; the current time when absent.
    pub time: Option<(i64, u32)>,
    /// The outer PFS seed; random when absent.
    pub seed: Option<[u8; 16]>,
}

impl BuildRequest {
    pub fn new(source: impl Into<PathBuf>, output_dir: impl Into<PathBuf>) -> Self {
        Self {
            source: source.into(),
            output_dir: output_dir.into(),
            content_id: None,
            file_name: None,
            passcode: crate::crypto::DEFAULT_PASSCODE.to_string(),
            time: None,
            seed: None,
        }
    }
}

pub struct BuildReport {
    pub path: PathBuf,
    pub size: u64,
    pub content_id: String,
    /// Every verification check this crate knows, run on the finished package.
    pub verify: crate::verify::Report,
    /// Readiness findings that did not stop the build.
    pub warnings: Vec<String>,
}

/// Optional controls an asynchronous caller supplies: byte progress and cancellation.
#[derive(Default)]
pub struct BuildControl<'a> {
    /// Bytes written of the mount image, reported as the write proceeds.
    pub bytes: Option<&'a mut dyn FnMut(u64, u64)>,
    /// Set to abort: the partial file is removed and the error says so.
    pub cancel: Option<&'a std::sync::atomic::AtomicBool>,
}

/// Build the package. `progress` receives short phase lines.
pub fn build(request: &BuildRequest, progress: &mut dyn FnMut(&str)) -> Result<BuildReport> {
    build_controlled(request, progress, &mut BuildControl::default())
}

/// The build an asynchronous caller drives: same pipeline, with byte progress and a way
/// to stop it.
pub fn build_controlled(
    request: &BuildRequest,
    progress: &mut dyn FnMut(&str),
    control: &mut BuildControl,
) -> Result<BuildReport> {
    build_mode(request, progress, control, Mode::Streaming)
}

/// The writer that holds the whole image in memory. Gate G2 verified this one, and
/// `tests/scale.rs` still compares the streaming writer against it byte for byte.
pub fn build_in_memory(
    request: &BuildRequest,
    progress: &mut dyn FnMut(&str),
) -> Result<BuildReport> {
    build_mode(
        request,
        progress,
        &mut BuildControl::default(),
        Mode::InMemory,
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Streaming,
    InMemory,
}

fn build_mode(
    request: &BuildRequest,
    progress: &mut dyn FnMut(&str),
    control: &mut BuildControl,
    mode: Mode,
) -> Result<BuildReport> {
    let mut tree = source::open(&request.source)?;
    let mut files: Vec<SourceFile> = tree.files().to_vec();
    if files.is_empty() {
        return format_err(format!("{} has no files", tree.describe()));
    }
    let readiness = source::readiness(tree.as_mut());
    let warnings: Vec<String> = readiness
        .warnings()
        .map(|c| format!("{}: {}", c.name, c.detail))
        .collect();
    let param_json = tree.read("sce_sys/param.json").unwrap_or_default();
    // A "free" or "upgradable" DRM value makes the console show a lock and refuse to start
    // the title, so the package carries "standard". The user's file is untouched: the
    // rewritten bytes are served in its place, and the file list's size for it is adjusted
    // so the plan lays out what the package will actually carry.
    let param_json = source::drm_rewrite(&param_json).unwrap_or(param_json);
    // The console reads `titleId` out of the packaged copy at GetRawContentInfo; a source that
    // omits it makes the install fail before anything is transferred.
    let param_json = source::title_id_rewrite(&param_json).unwrap_or(param_json);
    if let Some(entry) = files.iter_mut().find(|f| f.path == "sce_sys/param.json") {
        entry.size = param_json.len() as u64;
    }
    let content_id = match &request.content_id {
        Some(id) => id.clone(),
        None => source::content_id(&param_json).ok_or_else(|| {
            crate::Error::Format(format!(
                "{} has no content id in sce_sys/param.json; pass one explicitly",
                tree.describe()
            ))
        })?,
    };
    if content_id.len() != 36 {
        return format_err(format!(
            "content id {content_id:?} is {} characters, not 36",
            content_id.len()
        ));
    }
    if !files.iter().any(|f| f.path == "eboot.bin") {
        return format_err("the source has no eboot.bin");
    }
    let content_version = source::content_version_word(&param_json).unwrap_or(0);
    // The container's payloads, read once: the streaming arm hands them to the writer,
    // and reading them here keeps the source free for the range reader below.
    let icon_png = tree.read("sce_sys/icon0.png").unwrap_or_default();
    let icon_dds = tree.read("sce_sys/icon0.dds").unwrap_or_default();
    let time = request.time.unwrap_or_else(now);
    let seed = request.seed.unwrap_or_else(random_seed);

    progress(&format!("planning {}", tree.describe()));
    let plan = plan::build(&files)?;
    // Refuse an over-large source here, before a single byte is read.
    if plan.ndblock > outer_write::max_inner_blocks() {
        return format_err(format!(
            "{} needs an inner image of {} blocks ({:.1} GiB); this writer covers {:.1} GiB \
             until the double-indirect outer slot is verified",
            tree.describe(),
            plan.ndblock,
            (plan.ndblock * BLOCK) as f64 / (1u64 << 30) as f64,
            (outer_write::max_inner_blocks() * BLOCK) as f64 / (1u64 << 30) as f64,
        ));
    }
    let sizes: std::collections::HashMap<&str, u64> =
        files.iter().map(|f| (f.path.as_str(), f.size)).collect();
    let cancelled = || {
        control
            .cancel
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
    };

    // Refuse before the first write rather than fill the disk and fail halfway: a game
    // package is as large as the game.
    let planned = estimate_size(&plan)?;
    if let Some(free) = free_bytes(&request.output_dir) {
        if let Some(message) = shortfall(free, planned + planned / 100) {
            return format_err(format!("{}: {message}", request.output_dir.display()));
        }
    }
    let stem = request
        .file_name
        .clone()
        .unwrap_or_else(|| content_id.clone());
    std::fs::create_dir_all(&request.output_dir)?;
    let final_path = request.output_dir.join(format!("{stem}.pkg"));
    let partial = request.output_dir.join(format!("{stem}.pkg.partial"));
    let cleanup = |e: crate::Error| -> crate::Error {
        std::fs::remove_file(&partial).ok();
        e
    };

    let written = match mode {
        Mode::Streaming => {
            let mut file = std::fs::File::create(&partial)?;
            let mut read_range = |path: &str, offset: u64, len: usize| -> Result<Vec<u8>> {
                if !sizes.contains_key(path) {
                    return format_err(format!(
                        "the plan asked for {path}, which is not in the source"
                    ));
                }
                if len == 0 {
                    return Ok(Vec::new());
                }
                if path == "sce_sys/param.json" {
                    let at = (offset as usize).min(param_json.len());
                    return Ok(param_json[at..(at + len).min(param_json.len())].to_vec());
                }
                tree.read_range(path, offset, len)
            };
            let mut bytes = |done: u64, total: u64| {
                if let Some(f) = control.bytes.as_deref_mut() {
                    f(done, total);
                }
            };
            let mut p = stream::Progress {
                phase: progress,
                bytes: &mut bytes,
            };
            let idle = std::sync::atomic::AtomicBool::new(false);
            let cancel = control.cancel.unwrap_or(&idle);
            let stream_request = stream::StreamRequest {
                plan: &plan,
                passcode: &request.passcode,
                seed,
                time,
                content_id: &content_id,
                content_version,
                // The range reader keeps a borrow of it for the file's bytes.
                param_json: param_json.clone(),
                icon_png,
                icon_dds,
            };
            match stream::write_package(&mut file, &stream_request, &mut read_range, &mut p, cancel)
            {
                Ok(package) => package.size,
                Err(e) => return Err(cleanup(e)),
            }
        }
        Mode::InMemory => {
            if cancelled() {
                return Err(cleanup(crate::Error::Format(
                    "the build was cancelled".to_string(),
                )));
            }
            let mut read = |path: &str| -> Result<Vec<u8>> {
                match sizes.get(path) {
                    Some(0) => Ok(Vec::new()),
                    Some(_) if path == "sce_sys/param.json" => Ok(param_json.clone()),
                    Some(_) => tree.read(path),
                    None => format_err(format!(
                        "the plan asked for {path}, which is not in the source"
                    )),
                }
            };
            progress("writing the inner image");
            let inner = inner::write(&plan, &request.passcode, &mut read, time)?;
            progress("writing the layout");
            let naps = naps::build(
                inner.image.len() as u64,
                plan.ndblock,
                &inner.afid_offsets,
                plan.data_end,
                plan.meta_base,
            )?;
            progress("writing the outer image");
            let outer = outer_write::write(
                &inner.image,
                &naps,
                seed,
                &content_id,
                &request.passcode,
                time,
            )?;
            let game_digest = outer.plaintext_digests[outer.superblock_block as usize];
            let cnt_offset = BLOCK + outer.image.len() as u64;
            let inner_size = plan.ndblock * BLOCK;
            let fih = fih_write::write(&FihParams {
                outer_size: outer.image.len() as u64,
                superblock_block: outer.superblock_block,
                game_digest,
                cnt_offset,
                naps: &naps,
                inner_size,
                meta_base: plan.meta_base,
                content_inodes: plan.content_inodes,
                content_version,
                app_file_count: plan.app_file_count,
                flt_count: u32::from(!plan.flt_apr.is_empty()) + 1,
            });

            progress("writing the container");
            let outer_size = outer.image.len() as u64;
            let playgo_chunk = si_write::playgo_chunk_dat(&content_id, cnt_offset)?;
            let ficm_files = plan.content_inodes + 3;
            let cnt = cnt_write::write(&CntParams {
                content_id: &content_id,
                param_json: &param_json,
                icon_png: &icon_png,
                icon_dds: &icon_dds,
                playgo_chunk: &playgo_chunk,
                playgo_hash_table: &si_write::playgo_hash_table(ficm_files / 2),
                playgo_ficm: &si_write::playgo_ficm(ficm_files),
                imagedigs: &outer.plaintext_digests,
                game_digest,
                fih_block: &fih,
                outer_size,
                cnt_offset,
                seed,
                passcode: &request.passcode,
                content_type: 0x26,
                drm_type: 0,
                content_flags: 0x0602_0000,
                inner_size,
            })?;

            progress("writing the install metadata");
            let mut mount_image =
                Vec::with_capacity((cnt_offset + cnt.bytes.len() as u64) as usize);
            mount_image.extend_from_slice(&fih);
            mount_image.extend_from_slice(&outer.image);
            mount_image.extend_from_slice(&cnt.bytes);
            let crc = si_write::chunk_crc(&mount_image);
            let inner_files = plan.inner_files();
            let meta_18 = si_write::naps_meta_18(
                inner_size,
                &si_write::InnerDigests::of_image(&inner.image, &inner_files),
                &inner.image[plan.meta_base as usize..],
                &inner_files,
                plan.data_end,
                plan.meta_base,
                &game_digest,
            )?;
            let meta_300 = si_write::naps_meta_300(inner_size);
            let outer_layout = outer_write::layout(plan.ndblock)?;
            let sb_at = outer.superblock_block as usize * BLOCK as usize;
            let manifest = pfsimage::build(&pfsimage::ManifestParams {
                facts: &cnt.facts,
                content_id: &content_id,
                content_type: 0x26,
                param_json: &param_json,
                content_version,
                cnt_offset,
                si_offset: cnt_offset + cnt.facts.container_size,
                outer_size,
                inner_size,
                seed,
                game_digest,
                icv: outer_write::superblock_icv(&outer.image[sb_at..sb_at + BLOCK as usize]),
                playgo_chunk_len: playgo_chunk.len() as u64,
                outer: &outer_layout,
                naps_len: naps.len() as u64,
                plan: &plan,
            });
            let members = vec![
                ("common/etc/naps_meta_18.dat".to_string(), meta_18),
                ("common/etc/naps_meta_300.dat".to_string(), meta_300.clone()),
                ("common/etc/naps_meta_301.dat".to_string(), meta_300.clone()),
                ("common/etc/naps_meta_302.dat".to_string(), meta_300.clone()),
                ("common/etc/naps_meta_308.dat".to_string(), meta_300),
                ("common/etc/pfsimage.xml".to_string(), manifest),
                ("common/etc/playgo-chunk.dat".to_string(), playgo_chunk),
                (format!("config/{content_id}/playgo-chunk.crc"), crc),
            ];
            let si = si_write::zip(&members, time);

            progress("writing the package");
            {
                use std::io::Write;
                let mut out = std::fs::File::create(&partial)?;
                out.write_all(&fih)?;
                out.write_all(&outer.image)?;
                out.write_all(&cnt.bytes)?;
                out.write_all(&si)?;
                out.sync_all()?;
            }
            cnt_offset + cnt.bytes.len() as u64 + si.len() as u64
        }
    };

    progress("verifying");
    // The streaming verifier reads the package block by block, so the self-check of a
    // 155 GB package does not need 155 GB of memory.
    let mut verify_bytes = |done: u64, total: u64| {
        if let Some(f) = control.bytes.as_deref_mut() {
            f(done, total);
        }
    };
    let report = match verify::verify_streaming(&partial, &request.passcode, &mut verify_bytes) {
        Ok(report) if report.ok() => report,
        Ok(report) => {
            return Err(cleanup(crate::Error::Format(format!(
                "the built package failed verification:\n{report}"
            ))))
        }
        Err(e) => return Err(cleanup(e)),
    };
    std::fs::rename(&partial, &final_path)?;
    let size = std::fs::metadata(&final_path)?.len();
    debug_assert_eq!(size, written);
    progress("done");
    Ok(BuildReport {
        path: final_path,
        size,
        content_id,
        verify: report,
        warnings,
    })
}

/// The package's size before it is written: the header block, the outer image the layout
/// fixes, and a couple of megabytes for the container and the install metadata.
pub fn estimate_size(plan: &Plan) -> Result<u64> {
    let outer = outer_write::layout(plan.ndblock)?.ndblock * BLOCK;
    Ok(BLOCK + outer + 2 * 1024 * 1024)
}

/// Bytes available to this process on the volume holding `path`. `None` where the platform
/// does not say — the build then proceeds and the caller is told nothing about space.
#[cfg(unix)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some(st.f_bavail as u64 * st.f_frsize as u64)
}

#[cfg(not(unix))]
pub fn free_bytes(_path: &Path) -> Option<u64> {
    None
}

/// The refusal message a shortfall deserves, or `None` when there is room.
fn shortfall(free: u64, needed: u64) -> Option<String> {
    (free < needed).then(|| {
        format!(
            "only {:.1} GiB free where the package is going, and it needs about {:.1} GiB",
            free as f64 / (1u64 << 30) as f64,
            needed as f64 / (1u64 << 30) as f64,
        )
    })
}

fn now() -> (i64, u32) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (nanos.as_secs() as i64, nanos.subsec_nanos())
}

fn random_seed() -> [u8; 16] {
    use std::io::Read;
    let mut seed = [0u8; 16];
    // `/dev/urandom` never reaches EOF, so read exactly one seed's worth.
    let filled = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut seed))
        .is_ok();
    if !filled {
        // Fall back to the clock; the seed is not a secret, it only diversifies the key.
        let (secs, nanos) = now();
        seed[..8].copy_from_slice(&secs.to_le_bytes());
        seed[8..].copy_from_slice(&nanos.to_le_bytes());
    }
    seed
}

/// A short summary line for logs.
pub fn summary(report: &BuildReport) -> String {
    format!(
        "{} ({:.1} MiB, {} checks, {})",
        report.path.display(),
        report.size as f64 / (1024.0 * 1024.0),
        report.verify.checks.len(),
        if report.verify.ok() {
            "verified"
        } else {
            "FAILED"
        }
    )
}

/// The end-to-end sanity the caller can rely on: the digest of the finished file.
pub fn package_digest(path: &Path) -> Result<[u8; 32]> {
    Ok(sha3(&std::fs::read(path)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seed must come from exactly one read: `fs::read` on `/dev/urandom` never
    /// reaches EOF, which used to grow the buffer until the process was killed.
    #[test]
    fn the_random_seed_is_sixteen_bytes_and_varies() {
        let a = random_seed();
        let b = random_seed();
        assert_eq!(a.len(), 16);
        assert_ne!(a, b, "two seeds from the system must differ");
    }

    #[test]
    fn the_clock_fallback_is_usable() {
        let (secs, nanos) = now();
        assert!(secs > 1_600_000_000);
        let _ = nanos;
    }
}

/// The title a `param.json` declares. Real PS5 titles keep it in
/// `localizedParameters` (`{defaultLanguage, "en-US": {titleName}}`), while a bare
/// `titleName` shows up in hand-made ones; the title id is the last resort.
fn title_of(json: &serde_json::Value) -> Option<String> {
    let from = |v: &serde_json::Value| {
        v.get("titleName")
            .and_then(|t| t.as_str())
            .map(str::to_string)
    };
    if let Some(localized) = json.get("localizedParameters").and_then(|v| v.as_object()) {
        let default = localized
            .get("defaultLanguage")
            .and_then(|v| v.as_str())
            .unwrap_or("en-US");
        if let Some(title) = localized.get(default).and_then(from) {
            return Some(title);
        }
        for (key, value) in localized {
            if key != "defaultLanguage" {
                if let Some(title) = from(value) {
                    return Some(title);
                }
            }
        }
    }
    from(json).or_else(|| {
        json.get("titleId")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    })
}

/// `requiredSystemSoftwareVersion` is a BCD hex word: `0x1160000000000000` is 11.60,
/// `0x0960…` is 9.60. Anything that does not parse is passed through as it came.
pub fn firmware_version(json: &serde_json::Value) -> Option<String> {
    let raw = json.get("requiredSystemSoftwareVersion")?.as_str()?;
    // A hand-made param.json may carry a plain version; only the hex word is re-encoded.
    let Ok(value) = u64::from_str_radix(raw.trim_start_matches("0x"), 16) else {
        return Some(raw.to_string());
    };
    let (major, minor) = ((value >> 56) & 0xFF, (value >> 48) & 0xFF);
    if major == 0 && minor == 0 {
        return Some(raw.to_string());
    }
    Some(format!("{major:02x}.{minor:02x}"))
}

/// What a caller learns before deciding to build: what the source is, whether it looks
/// like a launchable title, what the package will cost, and whether there is room.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Inspection {
    pub source: String,
    pub files: usize,
    pub bytes: u64,
    pub content_id: Option<String>,
    pub title: Option<String>,
    pub required_firmware: Option<String>,
    /// The package's estimated size (what the free-space check uses).
    pub planned_size: u64,
    /// Bytes free where the output would go; `None` where the platform does not say.
    pub output_free: Option<u64>,
    /// Every readiness finding, passes and warnings alike.
    pub checks: Vec<source::Check>,
}

impl Inspection {
    pub fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &source::Check> {
        self.checks.iter().filter(|c| !c.ok)
    }
}

/// Look at a source without building it: readiness, geometry, cost and room.
pub fn inspect(source_path: &Path, output_dir: &Path) -> Result<Inspection> {
    let mut tree = source::open(source_path)?;
    let files: Vec<SourceFile> = tree.files().to_vec();
    let checks = source::readiness(tree.as_mut()).checks;
    let param = tree.read("sce_sys/param.json").unwrap_or_default();
    let json: Option<serde_json::Value> = serde_json::from_slice(
        std::str::from_utf8(&param)
            .unwrap_or_default()
            .trim_start_matches('\u{feff}')
            .as_bytes(),
    )
    .ok();
    let title = json.as_ref().and_then(title_of);
    let required_firmware = json.as_ref().and_then(firmware_version);
    let planned_size = if files.is_empty() {
        0
    } else {
        estimate_size(&plan::build(&files)?)?
    };
    Ok(Inspection {
        source: tree.describe(),
        files: files.len(),
        bytes: files.iter().map(|f| f.size).sum(),
        content_id: source::content_id(&param),
        title,
        required_firmware,
        planned_size,
        output_free: free_bytes(output_dir),
        checks,
    })
}
