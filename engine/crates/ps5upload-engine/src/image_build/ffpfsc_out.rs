//! A `.ffpfsc` straight to a console or a server.
//!
//! A local `.ffpfsc` writes its blocks first and then goes back for the header, the block table
//! and the PFS around them, which only its compression decides. AVA1 and a server upload are
//! written front to back, and AVA1 needs the file's size before its first byte. So the image
//! takes two passes over the source: a measure pass compresses it and keeps each block's stored
//! length (8 bytes per 64 KiB block: 13 MB for a 100 GB image), which gives the size, the header
//! and the table; then the write pass compresses it again into the destination, everything in
//! order, the same bytes a local build makes. A block's stored form follows from its bytes and
//! the level alone, so the lengths come out the same; one that does not (the source changed in
//! between) stops the write with an error, never a wrong table. The price is compressing twice.
//!
//! Going on works as for a plain image: a write pass again from the start, or (on a server that
//! writes at an offset) from what it holds, where the blocks wholly inside it are not compressed
//! again.

use std::io::{Seek, Write};
use std::sync::atomic::AtomicBool;

use ps5upload_fpkg::ffpfsc_block::Level;
use ps5upload_fpkg::source::SourceTree;

use super::Planned;

/// A `.ffpfsc` written straight to a console or a server: the image's name inside it (which
/// tells ShadowMount+ its filesystem) and how hard it is compressed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FfpfscOut<'a> {
    pub inner: &'a str,
    pub level: Level,
}

/// What the measure pass found.
pub(crate) struct MeasuredFfpfsc {
    measured: ps5_dump_forge_pfs::Measured,
    opts: ps5_dump_forge_pfs::WrapOptions,
    raw: u64,
}

impl MeasuredFfpfsc {
    /// The measure pass: `planned` written from `tree` and compressed, nothing kept but the
    /// lengths. `time` is what the PFS carries (the inner image's own, when it has one).
    /// Reports `compress`.
    pub(crate) fn measure(
        out: FfpfscOut,
        planned: &Planned,
        tree: &mut dyn SourceTree,
        time: Option<i64>,
        cancel: &AtomicBool,
        stage: &mut dyn FnMut(&str, u64, u64),
    ) -> Result<Self, String> {
        let raw = planned.image_size();
        let mut opts = ps5_dump_forge_pfs::WrapOptions::at(out.level);
        opts.time = time;
        stage("compress", 0, raw);
        let (_, measured) = ps5_dump_forge_pfs::measure(out.inner, raw, &opts, cancel, |s| {
            planned.write(tree, s, 0, cancel, &mut |d, t| stage("compress", d, t))
        })
        .map_err(|e| e.to_string())?;
        Ok(Self {
            measured,
            opts,
            raw,
        })
    }

    /// The whole `.ffpfsc`'s size.
    pub(crate) fn image_size(&self) -> u64 {
        self.measured.image_size()
    }

    /// The write pass into `out`, which drops (and only counts) the first `held` bytes when the
    /// destination holds them already. Reports `write`. Answers the bytes `out` was given.
    pub(crate) fn write<W: Write + Seek + Send>(
        &self,
        planned: &Planned,
        tree: &mut dyn SourceTree,
        out: &mut W,
        held: u64,
        cancel: &AtomicBool,
        stage: &mut dyn FnMut(&str, u64, u64),
    ) -> Result<u64, String> {
        ps5_dump_forge_pfs::wrap_measured(
            &self.measured,
            out,
            held,
            &self.opts,
            cancel,
            |s, from| planned.write(tree, s, from, cancel, &mut |d, t| stage("write", d, t)),
        )
        .map(|(_, r)| r.image_size)
        .map_err(|e| e.to_string())
    }

    /// For the job's detail: what the compression came to.
    pub(crate) fn describe(&self) -> String {
        let r = &self.measured.report;
        format!(
            "in a .ffpfsc ({}, {} of {} bytes, {} of {} blocks compressed, written in two passes)",
            self.opts.encoder(),
            self.measured.stored_size(),
            self.raw,
            r.compressed_blocks,
            r.blocks
        )
    }

    /// The figures the plan's key adds for a `.ffpfsc`.
    pub(crate) fn key(&self) -> String {
        format!(
            "\0ffpfsc\0{}\0{}",
            self.measured.image_size(),
            self.measured.stored_size()
        )
    }
}
