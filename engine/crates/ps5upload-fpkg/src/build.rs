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
use crate::outer_write::{self, OuterImage};
use crate::plan::{self, Plan};
use crate::si_write;
use crate::source::{self, SourceFile};
use crate::verify::verify_package;
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

/// Build the package. `progress` receives short phase lines.
pub fn build(request: &BuildRequest, progress: &mut dyn FnMut(&str)) -> Result<BuildReport> {
    let mut tree = source::open(&request.source)?;
    let files: Vec<SourceFile> = tree.files().to_vec();
    if files.is_empty() {
        return format_err(format!("{} has no files", tree.describe()));
    }
    let readiness = source::readiness(tree.as_mut());
    let warnings: Vec<String> = readiness
        .warnings()
        .map(|c| format!("{}: {}", c.name, c.detail))
        .collect();
    let param_json = tree.read("sce_sys/param.json").unwrap_or_default();
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
    let time = request.time.unwrap_or_else(now);
    let seed = request.seed.unwrap_or_else(random_seed);

    progress(&format!("planning {}", tree.describe()));
    let plan = plan::build(&files)?;
    let sizes: std::collections::HashMap<&str, u64> =
        files.iter().map(|f| (f.path.as_str(), f.size)).collect();
    let mut read = |path: &str| -> Result<Vec<u8>> {
        match sizes.get(path) {
            Some(0) => Ok(Vec::new()),
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

    // The container's offset needs only the outer image's size, so the FIH can be built
    // before the container that embeds its digest.
    let cnt_offset = BLOCK + outer.image.len() as u64;
    let inner_size = plan.ndblock * BLOCK;
    let fih = fih_write::write(&FihParams {
        outer: &outer,
        cnt_offset,
        naps: &naps,
        inner_size,
        meta_base_block: plan.meta_base / BLOCK,
        content_inodes: plan.content_inodes,
        content_version,
        app_file_count: plan.app_file_count,
        flt_count: u32::from(!plan.flt_apr.is_empty()) + 1,
    });

    progress("writing the container");
    let mchunk0 = BLOCK;
    let mchunk1 = outer.image.len() as u64;
    let playgo_chunk = si_write::playgo_chunk_dat(&content_id, mchunk0, mchunk1)?;
    let ficm_files = plan.content_inodes + 3;
    let playgo_ficm = si_write::playgo_ficm(ficm_files);
    let playgo_hash = si_write::playgo_hash_table(ficm_files / 2);
    let icon_png = tree.read("sce_sys/icon0.png").unwrap_or_default();
    let icon_dds = tree.read("sce_sys/icon0.dds").unwrap_or_default();
    let (content_type, drm_type, content_flags) = (0x26u32, 0u32, 0x0602_0000u32);
    let cnt = cnt_write::write(&CntParams {
        content_id: &content_id,
        param_json: &param_json,
        icon_png: &icon_png,
        icon_dds: &icon_dds,
        playgo_chunk: &playgo_chunk,
        playgo_hash_table: &playgo_hash,
        playgo_ficm: &playgo_ficm,
        imagedigs: &outer.plaintext_digests,
        game_digest,
        fih_block: &fih,
        outer_size: outer.image.len() as u64,
        cnt_offset,
        seed,
        passcode: &request.passcode,
        content_type,
        drm_type,
        content_flags,
        inner_size,
    })?;

    progress("writing the install metadata");
    let mount_image_size = cnt_offset + cnt.len() as u64;
    let mut mount_image = Vec::with_capacity(mount_image_size as usize);
    mount_image.extend_from_slice(&fih);
    mount_image.extend_from_slice(&outer.image);
    mount_image.extend_from_slice(&cnt);
    let crc = si_write::chunk_crc(&mount_image);
    let meta_18 = si_write::naps_meta_18(
        inner_size,
        &inner.image,
        &inner_files(&plan),
        plan.data_end,
        plan.meta_base,
        &game_digest,
    )?;
    let meta_300 = si_write::naps_meta_300(inner_size);
    let members = vec![
        ("common/etc/naps_meta_18.dat".to_string(), meta_18),
        ("common/etc/naps_meta_300.dat".to_string(), meta_300.clone()),
        ("common/etc/naps_meta_301.dat".to_string(), meta_300.clone()),
        ("common/etc/naps_meta_302.dat".to_string(), meta_300.clone()),
        ("common/etc/naps_meta_308.dat".to_string(), meta_300),
        (
            "common/etc/pfsimage.xml".to_string(),
            pfsimage_xml(&content_id, &plan, &outer, inner_size, cnt.len() as u64),
        ),
        ("common/etc/playgo-chunk.dat".to_string(), playgo_chunk),
        (format!("config/{content_id}/playgo-chunk.crc"), crc),
    ];
    let si = si_write::zip(&members, time);

    progress("writing the package");
    let stem = request
        .file_name
        .clone()
        .unwrap_or_else(|| content_id.clone());
    std::fs::create_dir_all(&request.output_dir)?;
    let final_path = request.output_dir.join(format!("{stem}.pkg"));
    let partial = request.output_dir.join(format!("{stem}.pkg.partial"));
    {
        use std::io::Write;
        let mut out = std::fs::File::create(&partial)?;
        out.write_all(&fih)?;
        out.write_all(&outer.image)?;
        out.write_all(&cnt)?;
        out.write_all(&si)?;
        out.sync_all()?;
    }

    progress("verifying");
    let report = match verify_package(&partial, &request.passcode) {
        Ok(report) if report.ok() => report,
        Ok(report) => {
            std::fs::remove_file(&partial).ok();
            return format_err(format!("the built package failed verification:\n{report}"));
        }
        Err(e) => {
            std::fs::remove_file(&partial).ok();
            return Err(e);
        }
    };
    std::fs::rename(&partial, &final_path)?;
    let size = std::fs::metadata(&final_path)?.len();
    progress("done");
    Ok(BuildReport {
        path: final_path,
        size,
        content_id,
        verify: report,
        warnings,
    })
}

/// The inner files in afid order, as the metric blob wants them.
fn inner_files(plan: &Plan) -> Vec<(String, u64, u64)> {
    plan.afid_order
        .iter()
        .map(|&fi| {
            let f = &plan.files[fi];
            (f.path.clone(), f.logical_offset, f.size)
        })
        .collect()
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

/// The image descriptor the SI archive carries. The console does not read it; it is
/// emitted self-consistent for tools that do.
fn pfsimage_xml(
    content_id: &str,
    plan: &Plan,
    outer: &OuterImage,
    inner_size: u64,
    cnt_size: u64,
) -> Vec<u8> {
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    xml.push_str("<package-config version=\"1.0\">\n");
    xml.push_str("  <config>\n");
    xml.push_str("    <version-date>0x20240508</version-date>\n");
    xml.push_str("    <version-hash>0x090fbfc1</version-hash>\n");
    xml.push_str(&format!("    <content-id>{content_id}</content-id>\n"));
    xml.push_str("  </config>\n");
    xml.push_str("  <container>\n");
    xml.push_str(&format!("    <size>0x{cnt_size:x}</size>\n"));
    xml.push_str("  </container>\n");
    xml.push_str("  <mount-image>\n");
    xml.push_str(&format!(
        "    <filesize>0x{:x}</filesize>\n",
        outer.image.len()
    ));
    xml.push_str(&format!(
        "    <metadata offset=\"0x{:x}\" />\n",
        plan.meta_base
    ));
    xml.push_str(&format!("    <ndblock>0x{:x}</ndblock>\n", plan.ndblock));
    xml.push_str(&format!("    <inner-size>0x{inner_size:x}</inner-size>\n"));
    xml.push_str("  </mount-image>\n");
    xml.push_str("  <entries>\n");
    for f in &plan.files {
        xml.push_str(&format!(
            "    <entry path=\"{}\" size=\"0x{:x}\" />\n",
            f.path, f.size
        ));
    }
    xml.push_str("  </entries>\n");
    xml.push_str("</package-config>\n");
    xml.into_bytes()
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
