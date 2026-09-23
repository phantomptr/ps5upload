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
use crate::sdk_rules;
use crate::self_repair;
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
    /// The outer PFS seed. Only an `ImageMode::Native` build has one, and it is random when
    /// absent; a plaintext build's seed slot carries [`crate::PLAINTEXT_MARKER`] instead.
    pub seed: Option<[u8; 16]>,
    /// How the inner image's metadata region is stored. `PS5UPLOAD_FPKG_META_CODEC` (`stored` or
    /// `zlib`) overrides it, which is how a stored control package is built without a code change.
    pub metadata_codec: inner::MetaCodec,
    /// How the outer image's blocks are stored. `PS5UPLOAD_FPKG_IMAGE_MODE` (`native`) overrides
    /// it, which is how a native control package is built without a code change.
    pub image_mode: crate::ImageMode,
    /// Rewrites `requiredSystemSoftwareVersion` in the packaged `param.json`, which is what the
    /// console compares against its own firmware at install time. `PS5UPLOAD_FPKG_FW` (a BCD hex
    /// word) overrides the source's value; absent, the source's own value is carried through.
    pub firmware: Option<String>,
    /// PlayGo chunks (1 through 255). `PS5UPLOAD_FPKG_CHUNKS` overrides the default.
    pub playgo_chunks: u16,
    /// Compress the inner image with Kraken, as Sony's packages are (see
    /// [`crate::kraken_image`]). Off until a compressed package is confirmed on a console;
    /// `PS5UPLOAD_FPKG_KRAKEN=1` turns it on.
    pub kraken: bool,
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
            metadata_codec: codec_from_env(),
            image_mode: image_mode_from_env(),
            firmware: firmware_from_env(),
            playgo_chunks: chunks_from_env(),
            kraken: matches!(
                std::env::var("PS5UPLOAD_FPKG_KRAKEN").as_deref(),
                Ok("1") | Ok("true")
            ),
        }
    }
}

fn chunks_from_env() -> u16 {
    std::env::var("PS5UPLOAD_FPKG_CHUNKS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(crate::playgo::DEFAULT_CHUNKS)
}

fn firmware_from_env() -> Option<String> {
    match std::env::var("PS5UPLOAD_FPKG_FW") {
        Ok(v) if !v.trim().is_empty() => Some(v.trim().to_string()),
        _ => None,
    }
}

/// `stored` is the default: it declares `compType = 2` (Kraken), the value carried by both
/// packages whose descriptors we can read — including the only one seen to mount — while the
/// `zlib` shape's `compType = 1` is the code the console rejects at `ppfs_create_cmpc_for_naps()`
/// with `EOPNOTSUPP` before a mount can finish. `zlib` stays reachable for the A/B that
/// established this.
fn codec_from_env() -> inner::MetaCodec {
    match std::env::var("PS5UPLOAD_FPKG_META_CODEC").as_deref() {
        Ok("zlib") => inner::MetaCodec::Zlib,
        _ => inner::MetaCodec::Stored,
    }
}

fn image_mode_from_env() -> crate::ImageMode {
    match std::env::var("PS5UPLOAD_FPKG_IMAGE_MODE").as_deref() {
        Ok("native") => crate::ImageMode::Native,
        _ => crate::ImageMode::PlaintextNoAuth,
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
    // Leftovers of an earlier build stay out of this one (see `sdk_rules`).
    let before = files.len();
    files.retain(|f| match sdk_rules::excluded(&f.path) {
        Some(why) => {
            progress(&format!("leaving out {} ({why})", f.path));
            false
        }
        None => true,
    });
    if files.len() != before {
        progress(&format!(
            "left out {} generated or stale file(s)",
            before - files.len()
        ));
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
    // The packaged copy has to name the id the package is built under, both in `contentId`
    // (the console checks it against the transfer's own) and in `titleId` (which it reads at
    // GetRawContentInfo). A source that says something else is the ordinary case for a rename.
    let param_json = source::content_id_rewrite(&param_json, &content_id).unwrap_or(param_json);
    // Written into the install metadata, where the console compares it against its own
    // firmware and refuses the package when the console is older (0x80a3000d).
    let param_json = match request.firmware.as_deref() {
        Some(version) => source::firmware_rewrite(&param_json, version).unwrap_or(param_json),
        None => param_json,
    };
    // Executables some dumpers leave malformed are served repaired (see `self_repair`).
    let mut repairs: std::collections::HashMap<String, (self_repair::SelfRepair, u64)> =
        std::collections::HashMap::new();
    for f in files.iter_mut() {
        if f.size < 0x20 {
            continue;
        }
        let header = tree.read_range(&f.path, 0, 0x20)?;
        let path = f.path.clone();
        let mut read = |offset: u64, len: usize| tree.read_range(&path, offset, len);
        if let Some(repair) = self_repair::plan(&header, f.size, &mut read) {
            progress(&format!("repairing {}: {}", f.path, repair.describe()));
            repairs.insert(f.path.clone(), (repair, f.size));
            f.size = repair.new_size(f.size);
        }
    }
    // The declared size class, from everything but `param.json` itself.
    let unpacked: u64 = files
        .iter()
        .filter(|f| f.path != "sce_sys/param.json")
        .map(|f| f.size)
        .sum();
    let param_json = match sdk_rules::size_class_rewrite(&param_json, unpacked, files.len() as u64)
    {
        Ok(Some((rewritten, what))) => {
            progress(&format!("size class: {what}"));
            rewritten
        }
        Ok(None) => param_json,
        Err(e) => return format_err(e),
    };
    if let Some(entry) = files.iter_mut().find(|f| f.path == "sce_sys/param.json") {
        entry.size = param_json.len() as u64;
    }
    if !files.iter().any(|f| f.path == "eboot.bin") {
        return format_err("the source has no eboot.bin");
    }
    let content_version = source::content_version_word(&param_json).unwrap_or(0);
    // The container's payloads, read once: the streaming arm hands them to the writer,
    // and reading them here keeps the source free for the range reader below.
    let icon_png = tree.read("sce_sys/icon0.png").unwrap_or_default();
    let icon_dds = tree.read("sce_sys/icon0.dds").unwrap_or_default();
    // Only files still in the package: an excluded one must not reappear in the container.
    let extras = cnt_write::presentation_extras(&mut |path| {
        sizes_of(&files, path).and_then(|_| tree.read(path).ok())
    });
    // What the container now carries, the image leaves out (see `CONTAINER_ONLY`).
    let mut carried: std::collections::HashSet<&str> = cnt_write::PRESENTATION
        .iter()
        .filter(|(id, _, _)| extras.iter().any(|e| e.id == *id))
        .map(|(_, path, _)| *path)
        .collect();
    if !icon_png.is_empty() {
        carried.insert("sce_sys/icon0.png");
    }
    if !icon_dds.is_empty() {
        carried.insert("sce_sys/icon0.dds");
    }
    files.retain(|f| {
        !(cnt_write::CONTAINER_ONLY.contains(&f.path.as_str()) && carried.contains(f.path.as_str()))
    });
    let time = request.time.unwrap_or_else(now);
    // A plaintext package carries the marker where a native one carries its random seed, so the
    // slot and the mode can never disagree and `request.seed` only has meaning in the native mode.
    let seed = match request.image_mode {
        crate::ImageMode::PlaintextNoAuth => crate::PLAINTEXT_MARKER,
        crate::ImageMode::Native => request.seed.unwrap_or_else(random_seed),
    };

    progress(&format!("planning {}", tree.describe()));
    let plan = plan::build_with(&files, request.kraken)?;
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
    if final_path.exists() || partial.exists() {
        return format_err(format!(
            "output already exists: {} or {}; choose another output folder or name",
            final_path.display(),
            partial.display()
        ));
    }
    let cleanup = |e: crate::Error| -> crate::Error {
        std::fs::remove_file(&partial).ok();
        e
    };

    let written = match mode {
        Mode::Streaming => {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&partial)?;
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
                if let Some((repair, original)) = repairs.get(path) {
                    let mut read = |o: u64, l: usize| tree.read_range(path, o, l);
                    return repair.read(*original, offset, len, &mut read);
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
                image_mode: request.image_mode,
                time,
                content_id: &content_id,
                content_version,
                // The range reader keeps a borrow of it for the file's bytes.
                param_json: param_json.clone(),
                icon_png,
                icon_dds,
                extras,
                playgo_chunks: request.playgo_chunks,
                kraken_spool: request
                    .kraken
                    .then(|| PathBuf::from(format!("{}.kraken", partial.display()))),
                metadata_codec: request.metadata_codec,
            };
            let written =
                stream::write_package(&mut file, &stream_request, &mut read_range, &mut p, cancel);
            if let Some(spool) = &stream_request.kraken_spool {
                std::fs::remove_file(spool).ok();
            }
            match written {
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
                    Some(&size) => match repairs.get(path) {
                        Some((repair, original)) => {
                            let mut read = |o: u64, l: usize| tree.read_range(path, o, l);
                            repair.read(*original, 0, size as usize, &mut read)
                        }
                        None => tree.read(path),
                    },
                    None => format_err(format!(
                        "the plan asked for {path}, which is not in the source"
                    )),
                }
            };
            progress("writing the inner image");
            let inner = inner::write_with(
                &plan,
                &request.passcode,
                &mut read,
                crate::stream::image_time(request.image_mode, time),
                request.metadata_codec,
            )?;
            progress("writing the layout");
            let inner_blocks = inner.disk_blocks();
            let naps = naps::build_with_meta(
                inner.image.len() as u64,
                plan.ndblock,
                &inner.afid_files,
                plan.data_end,
                plan.meta_base,
                &inner.metadata.blocks,
                request.metadata_codec.compression_type(),
            )?;
            progress("writing the outer image");
            let outer = outer_write::write(
                &inner.image,
                &naps,
                seed,
                request.image_mode,
                &content_id,
                &request.passcode,
                crate::stream::image_time(request.image_mode, time),
            )?;
            let game_digest = outer.plaintext_digests[outer.superblock_block as usize];
            let cnt_offset = BLOCK + outer.image.len() as u64;
            // `0xA0` carries the mount's size; `0x90` the stored image's, which the outer PFS and
            // the container both follow.
            let inner_size = plan.ndblock * BLOCK;
            let fih = fih_write::write(&FihParams {
                outer_size: outer.image.len() as u64,
                superblock_block: outer.superblock_block,
                game_digest,
                cnt_offset,
                naps: &naps,
                inner_size,
                meta_base: plan.meta_base,
                inner_blocks: inner_blocks as u32,
                content_inodes: plan.content_inodes,
                content_version,
                app_file_count: plan.app_file_count,
                flt_count: u32::from(!plan.flt_apr.is_empty()) + 1,
            });

            progress("writing the container");
            let outer_size = outer.image.len() as u64;
            let playgo = crate::playgo::build(
                &content_id,
                &plan.mount_files(),
                cnt_offset,
                request.playgo_chunks,
            )?;
            let cnt = cnt_write::write(&CntParams {
                content_id: &content_id,
                param_json: &param_json,
                icon_png: &icon_png,
                icon_dds: &icon_dds,
                extras: &extras,
                playgo_chunk: &playgo.chunk_dat,
                playgo_hash_table: &playgo.hash_table,
                playgo_ficm: &playgo.ficm,
                imagedigs: &outer.plaintext_digests,
                game_digest,
                fih_block: &fih,
                outer_size,
                cnt_offset,
                seed,
                passcode: &request.passcode,
                content_type: cnt_write::content_class(&param_json).0,
                drm_type: 0,
                content_flags: cnt_write::content_class(&param_json).1,
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
                &si_write::InnerDigests::of_image(&inner.image, &inner.afid_files),
                // The metric blob describes the metadata region's logical bytes, not the container
                // the image stores there.
                &inner.metadata.plain,
                &inner_files,
                plan.data_end,
                plan.meta_base,
                &game_digest,
            )?;
            let meta_300 = si_write::naps_meta_300(inner_size);
            let outer_layout = outer_write::layout(inner_blocks, naps.len() as u64)?;
            let sb_at = outer.superblock_block as usize * BLOCK as usize;
            let manifest = pfsimage::build(&pfsimage::ManifestParams {
                facts: &cnt.facts,
                content_id: &content_id,
                content_type: cnt_write::content_class(&param_json).0,
                param_json: &param_json,
                content_version,
                cnt_offset,
                si_offset: cnt_offset + cnt.facts.container_size,
                outer_size,
                inner_size,
                seed,
                game_digest,
                icv: outer_write::superblock_icv(&outer.image[sb_at..sb_at + BLOCK as usize]),
                playgo: &playgo,
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
                (
                    "common/etc/playgo-chunk.dat".to_string(),
                    playgo.chunk_dat.clone(),
                ),
                (format!("config/{content_id}/playgo-chunk.crc"), crc),
            ];
            let si = si_write::zip(&members, time);

            progress("writing the package");
            {
                use std::io::Write;
                let mut out = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&partial)?;
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
    if final_path.exists() {
        return Err(cleanup(crate::Error::Format(format!(
            "output appeared during conversion: {}",
            final_path.display()
        ))));
    }
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
    // The descriptor's length is not known until it is built, so the guard allows for the
    // largest one a dinode can point at: an over-estimate only makes the free-space check
    // stricter, which is the safe direction for a build that must not run out of room.
    let outer = outer_write::layout(plan.ndblock, outer_write::DIRECT_SLOTS as u64 * BLOCK)?
        .ndblock
        * BLOCK;
    Ok(BLOCK + outer + 2 * 1024 * 1024)
}

/// Bytes available to this process on the volume holding `path`. `None` where the platform
/// does not say — the build then proceeds and the caller is told nothing about space.
#[cfg(unix)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    // A user may name an output folder that will be created by the build.
    // Check the nearest existing parent so the preflight still catches a full disk.
    let existing = path.ancestors().find(|ancestor| ancestor.exists())?;
    let c = std::ffi::CString::new(existing.as_os_str().as_bytes()).ok()?;
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

fn sizes_of(files: &[SourceFile], path: &str) -> Option<u64> {
    files.iter().find(|f| f.path == path).map(|f| f.size)
}

fn now() -> (i64, u32) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (nanos.as_secs() as i64, nanos.subsec_nanos())
}

/// The outer PFS seed, from the operating system's randomness.
///
/// This used to read `/dev/urandom`, which does not exist on Windows, so every build there
/// silently took the clock fallback below — two builds started in the same nanosecond window
/// would share a seed. The OS call works on every platform we ship, so the fallback is now
/// genuinely unreachable in practice and remains only so that a build never fails for want of
/// entropy: the seed diversifies the key, it is not itself a secret.
fn random_seed() -> [u8; 16] {
    let mut seed = [0u8; 16];
    if getrandom::fill(&mut seed).is_err() {
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
    /// The seed must come from the OS, not the clock. Reading `/dev/urandom` meant Windows
    /// always took the clock fallback, so two builds in the same nanosecond window shared a
    /// seed. Clock-derived seeds are recognisable: the first eight bytes are a small
    /// little-endian second count, so the high bytes are zero.
    #[test]
    fn the_seed_comes_from_the_os_not_the_clock() {
        let a = super::random_seed();
        let b = super::random_seed();
        assert_ne!(a, [0u8; 16], "an all-zero seed means nothing was written");
        assert_ne!(a, b, "two seeds must differ");
        // A seconds-since-epoch value leaves bytes 5..8 zero for the next few thousand
        // years; real randomness effectively never does across two draws.
        let clocklike = |s: &[u8; 16]| s[5] == 0 && s[6] == 0 && s[7] == 0;
        assert!(
            !(clocklike(&a) && clocklike(&b)),
            "both seeds look clock-derived: {a:02x?} {b:02x?}"
        );
    }

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
