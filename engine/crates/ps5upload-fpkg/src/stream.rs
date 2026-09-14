//! Writing a package block by block, so a 100 GB game never has to be resident.
//!
//! The in-memory writers (`inner::write`, `outer_write::write`) stay as the oracle: they
//! are what gate G2 verified. This module walks the same plan and emits the same bytes —
//! a test asserts the two packages are identical, which is what keeps the streaming path
//! honest as the formats evolve.
//!
//! Resident state is bounded by the *file count* (the plan, the flat-path table, the
//! metadata region, the container) and by the *block count* (32 B per block for the image
//! digests) — never by the size of the game.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::cnt_write::{self, CntParams};
use crate::crypto::{crc32c, derive_ekpfs, derive_xts_keys, Hasher};
use crate::fih_write::{self, FihParams};
use crate::inner::{BlockSource, RangeRead};
use crate::naps;
use crate::outer_write::{self, layout};
use crate::plan::Plan;
use crate::si_write;
use crate::xts::{Xts, SIGNED_SECTOR_FLAG};
use crate::{format_err, Result, BLOCK};

/// What a streaming build needs beyond the plan.
pub struct StreamRequest<'a> {
    pub plan: &'a Plan,
    pub passcode: &'a str,
    pub seed: [u8; 16],
    pub time: (i64, u32),
    pub content_id: &'a str,
    pub content_version: u32,
    /// The container's payloads, read from the source before the pass — all small.
    pub param_json: Vec<u8>,
    pub icon_png: Vec<u8>,
    pub icon_dds: Vec<u8>,
}

/// How a build reports itself: phase names, and bytes written of the mount image.
pub struct Progress<'a> {
    pub phase: &'a mut dyn FnMut(&str),
    pub bytes: &'a mut dyn FnMut(u64, u64),
}

pub struct StreamedPackage {
    pub size: u64,
    pub outer_size: u64,
    pub cnt_offset: u64,
    pub game_digest: [u8; 32],
    pub content_id: String,
}

/// The digests the metric blob wants, accumulated while the image streams by: one per
/// content file (in afid order) and one for the whole image. Files are laid out in afid
/// order and never interleave, so one open hasher at a time is enough — the state of a
/// 300,000-file image stays constant.
struct FileDigester {
    files: Vec<[u8; 32]>,
    open: Option<(usize, Hasher)>,
    image: Hasher,
}

impl FileDigester {
    fn new(file_count: usize) -> Self {
        Self {
            files: vec![[0u8; 32]; file_count],
            open: None,
            image: Hasher::new(),
        }
    }

    /// Feed one block: `spans` are `(afid, from, to)` byte ranges within it, in image
    /// order — one per file the block touches.
    fn block(&mut self, block: &[u8], spans: &[(usize, usize, usize)]) {
        self.image.update(block);
        for &(afid, from, to) in spans {
            if self.open.as_ref().is_none_or(|(open, _)| *open != afid) {
                if let Some((previous, hasher)) = self.open.take() {
                    self.files[previous] = hasher.finish();
                }
                self.open = Some((afid, Hasher::new()));
            }
            if let Some((_, hasher)) = self.open.as_mut() {
                hasher.update(&block[from..to]);
            }
        }
    }

    fn finish(mut self) -> (Vec<[u8; 32]>, [u8; 32]) {
        if let Some((previous, hasher)) = self.open.take() {
            self.files[previous] = hasher.finish();
        }
        (self.files, self.image.finish())
    }
}

/// The files' byte ranges within one block, as `(afid, from, to)` offsets into the block,
/// in image order. A block can hold many files — a game's first block holds every small
/// file that was packed before the first large one — so the caller passes a buffer that is
/// reused between blocks rather than allocated per block.
fn block_spans(
    source: &BlockSource,
    plan: &Plan,
    index: u64,
    out: &mut Vec<(usize, usize, usize)>,
) {
    let lo = index * BLOCK;
    out.clear();
    for span in source.block_spans(index) {
        out.push((
            plan.files[span.file].afid as usize,
            (span.start.max(lo) - lo) as usize,
            (span.end.min(lo + BLOCK) - lo) as usize,
        ));
    }
}

fn cancelled() -> crate::Error {
    crate::Error::Format("the build was cancelled".to_string())
}

/// Write the whole package to `out`, which must be empty and seekable.
pub fn write_package(
    out: &mut File,
    request: &StreamRequest,
    read: RangeRead,
    progress: &mut Progress,
    cancel: &AtomicBool,
) -> Result<StreamedPackage> {
    let plan = request.plan;
    let inner_blocks = plan.ndblock;
    let lay = layout(inner_blocks)?;
    let inner_size = inner_blocks * BLOCK;
    let outer_size = lay.ndblock * BLOCK;
    let cnt_offset = BLOCK + outer_size;
    let mut source = BlockSource::new(plan, request.passcode, request.time)?;
    let naps = naps::build(
        inner_size,
        inner_blocks,
        &plan.afid_offsets(),
        plan.data_end,
        plan.meta_base,
    )?;

    let ekpfs = derive_ekpfs(request.content_id, request.passcode);
    let xts = Xts::new(&derive_xts_keys(&ekpfs, &request.seed));
    // `imagedigs` and `playgo-chunk.crc`, both indexed by file block: the header's block
    // first, then the outer image's. 32 B and 4 B per block — the only tables that scale
    // with the package, and they stay resident on purpose.
    let mut digests: Vec<[u8; 32]> = vec![[0u8; 32]; lay.ndblock as usize];
    let mut crcs: Vec<u32> = vec![0u32; 1 + lay.ndblock as usize];

    // ── the data blocks ──────────────────────────────────────────────────────────────
    (progress.phase)("writing the image");
    let files = plan.inner_files();
    let mut file_digests = FileDigester::new(files.len());
    let mut block = vec![0u8; BLOCK as usize];
    let mut spans: Vec<(usize, usize, usize)> = Vec::with_capacity(8);
    for index in 0..inner_blocks {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        block_spans(&source, plan, index, &mut spans);
        let plaintext = source.block(index, read)?;
        block.copy_from_slice(plaintext);

        file_digests.block(&block, &spans);

        digests[index as usize] = crate::crypto::sha3(&block);
        xts.encrypt(index, &mut block);
        crcs[1 + index as usize] = crc32c(&block);
        out.seek(SeekFrom::Start(BLOCK + index * BLOCK))?;
        out.write_all(&block)?;
        if index.is_multiple_of(512) {
            (progress.bytes)((index + 1) * BLOCK, cnt_offset);
        }
    }
    let (file_digests, image_digest) = file_digests.finish();

    // ── the outer metadata ───────────────────────────────────────────────────────────
    (progress.phase)("writing the layout");
    let mut game_digest = [0u8; 32];
    for (index, digest, mut plaintext) in outer_write::metadata_blocks(
        &lay,
        inner_blocks,
        &naps,
        &digests[..inner_blocks as usize],
        request.seed,
        request.time,
    )? {
        if index != lay.superblock_block {
            let sector = if index < lay.superblock_block {
                index
            } else {
                SIGNED_SECTOR_FLAG | index
            };
            xts.encrypt(sector, &mut plaintext);
        }
        digests[index as usize] = digest;
        if index == lay.superblock_block {
            game_digest = digest;
        }
        crcs[1 + index as usize] = crc32c(&plaintext);
        out.seek(SeekFrom::Start(BLOCK + index * BLOCK))?;
        out.write_all(&plaintext)?;
    }

    // ── the header, which needs the game digest the metadata pass just produced ──────
    (progress.phase)("writing the header");
    let fih = fih_write::write(&FihParams {
        outer_size,
        superblock_block: lay.superblock_block,
        game_digest,
        cnt_offset,
        naps: &naps,
        inner_size,
        meta_base_block: plan.meta_base / BLOCK,
        content_inodes: plan.content_inodes,
        content_version: request.content_version,
        app_file_count: plan.app_file_count,
        flt_count: u32::from(!plan.flt_apr.is_empty()) + 1,
    });
    crcs[0] = crc32c(&fih);
    out.seek(SeekFrom::Start(0))?;
    out.write_all(&fih)?;

    // ── the container ────────────────────────────────────────────────────────────────
    (progress.phase)("writing the container");
    let playgo_chunk = si_write::playgo_chunk_dat(request.content_id, BLOCK, outer_size)?;
    let ficm_files = plan.content_inodes + 3;
    let cnt = cnt_write::write(&CntParams {
        content_id: request.content_id,
        param_json: &request.param_json,
        icon_png: &request.icon_png,
        icon_dds: &request.icon_dds,
        playgo_chunk: &playgo_chunk,
        playgo_hash_table: &si_write::playgo_hash_table(ficm_files / 2),
        playgo_ficm: &si_write::playgo_ficm(ficm_files),
        imagedigs: &digests,
        game_digest,
        fih_block: &fih,
        outer_size,
        cnt_offset,
        seed: request.seed,
        passcode: request.passcode,
        content_type: 0x26,
        drm_type: 0,
        content_flags: 0x0602_0000,
        inner_size,
    })?;
    out.seek(SeekFrom::Start(cnt_offset))?;
    out.write_all(&cnt)?;
    // The mount image is the header, the outer image and the container; its CRC is the
    // container's chunks appended to the blocks already crc'd above.
    let mut crc_table: Vec<u8> = crcs.iter().flat_map(|c| c.to_le_bytes()).collect();
    crc_table.extend_from_slice(&si_write::chunk_crc(&cnt));

    // ── the install metadata ─────────────────────────────────────────────────────────
    (progress.phase)("writing the install metadata");
    let meta_18 = si_write::naps_meta_18(
        inner_size,
        &si_write::InnerDigests {
            image: image_digest,
            files: file_digests,
        },
        &source.metadata_region(),
        &files,
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
            crate::build::pfsimage_xml(
                request.content_id,
                plan,
                outer_size,
                inner_size,
                cnt.len() as u64,
            ),
        ),
        ("common/etc/playgo-chunk.dat".to_string(), playgo_chunk),
        (
            format!("config/{}/playgo-chunk.crc", request.content_id),
            crc_table,
        ),
    ];
    let si = si_write::zip(&members, request.time);
    out.seek(SeekFrom::Start(cnt_offset + cnt.len() as u64))?;
    out.write_all(&si)?;
    out.sync_all()?;

    let size = cnt_offset + cnt.len() as u64 + si.len() as u64;
    (progress.bytes)(cnt_offset, cnt_offset);
    Ok(StreamedPackage {
        size,
        outer_size,
        cnt_offset,
        game_digest,
        content_id: request.content_id.to_string(),
    })
}

/// The write is interruptible and the file is left as it is: the caller removes the
/// partial. Sized so a cancel during a long data pass is noticed within a block.
pub fn check_cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return format_err("the build was cancelled");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan;
    use crate::source::SourceFile;

    /// The metric blob's digests must be the image's: a per-file digest in afid order and
    /// one for the whole image, whether they come from the stream or from a built image.
    #[test]
    fn the_streamed_digests_are_the_image_digests() {
        let files: Vec<SourceFile> = [
            ("eboot.bin", 4000u64),
            ("data/one.bin", 600_000),
            ("data/two.bin", 5000),
            ("sce_sys/param.json", 197),
            ("sce_sys/icon0.png", 2048),
            ("sce_sys/icon0.dds", 4096),
            ("sce_sys/about/right.sprx", 4),
        ]
        .iter()
        .map(|(p, s)| SourceFile {
            path: (*p).to_string(),
            size: *s,
        })
        .collect();
        let plan = plan::build(&files).unwrap();
        let payloads: std::collections::HashMap<&str, Vec<u8>> = plan
            .files
            .iter()
            .map(|f| {
                let data: Vec<u8> = (0..f.size).map(|i| (i % 251) as u8).collect();
                (f.path.as_str(), data)
            })
            .collect();
        let time = (1_700_000_000i64, 0);
        let mut read_all =
            |path: &str| -> Result<Vec<u8>> { Ok(payloads.get(path).cloned().unwrap_or_default()) };
        let image =
            crate::inner::write(&plan, crate::crypto::DEFAULT_PASSCODE, &mut read_all, time)
                .unwrap()
                .image;
        let expected = crate::si_write::InnerDigests::of_image(&image, &plan.inner_files());

        let mut source = BlockSource::new(&plan, crate::crypto::DEFAULT_PASSCODE, time).unwrap();
        let mut digester = FileDigester::new(plan.inner_files().len());
        let mut spans = Vec::new();
        for index in 0..plan.ndblock {
            block_spans(&source, &plan, index, &mut spans);
            let block = source
                .block(index, &mut |path, offset, len| {
                    let data = payloads.get(path).cloned().unwrap_or_default();
                    let at = (offset as usize).min(data.len());
                    Ok(data[at..(at + len).min(data.len())].to_vec())
                })
                .unwrap()
                .to_vec();
            digester.block(&block, &spans);
        }
        let (files_streamed, image_streamed) = digester.finish();
        assert_eq!(image_streamed, expected.image, "whole-image digest");
        for (i, (a, b)) in files_streamed.iter().zip(&expected.files).enumerate() {
            assert_eq!(a, b, "file {i} digest ({})", plan.inner_files()[i].0);
        }

        // The metric blob itself, from both sets of inputs.
        let game = [7u8; 32];
        let list = plan.inner_files();
        let from_image = crate::si_write::naps_meta_18(
            plan.ndblock * BLOCK,
            &expected,
            &image[plan.meta_base as usize..],
            &list,
            plan.data_end,
            plan.meta_base,
            &game,
        )
        .unwrap();
        let from_stream = crate::si_write::naps_meta_18(
            plan.ndblock * BLOCK,
            &crate::si_write::InnerDigests {
                image: image_streamed,
                files: files_streamed,
            },
            &source.metadata_region(),
            &list,
            plan.data_end,
            plan.meta_base,
            &game,
        )
        .unwrap();
        assert_eq!(from_stream.len(), from_image.len());
        if from_stream != from_image {
            let at = from_stream
                .iter()
                .zip(&from_image)
                .position(|(a, b)| a != b)
                .unwrap();
            panic!("the metric blobs differ at {at}");
        }
    }
}
