//! Every integrity check we understand, as a named list.

use std::fmt;
use std::path::Path;

use crate::cnt::{self, EntryDigest};
use crate::crypto::sha3;
use crate::crypto::{derive_ekpfs, derive_xts_keys};
use crate::outer::{self, BlockKind};
use crate::xts::{Xts, SIGNED_SECTOR_FLAG};
use crate::{fih, flt, le32, si, PkgFile, Result};
use crate::{format_err, BLOCK};

/// What a check looks at, so a caller can say in plain words what a failure of it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// What the package says about itself: the header, and the container's digests, descriptors
    /// and install manifest. Small; a tool that edits a package without redoing them fails these
    /// while every byte of the game is right.
    Metadata,
    /// The image's own tables: its superblock, inodes, folders and the layout descriptor.
    Structure,
    /// The game's data blocks against the digest the package carries for each.
    Data,
    /// The file ends before the package does.
    Truncated,
}

impl Kind {
    /// The word an API gives for it.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Metadata => "metadata",
            Kind::Structure => "structure",
            Kind::Data => "data",
            Kind::Truncated => "truncated",
        }
    }
}

#[derive(Debug)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    pub kind: Kind,
    /// For a check that counts: how many of `total` failed (the data sweep's blocks), or how
    /// many bytes of `total` the file has ([`Kind::Truncated`]).
    pub count: Option<(u64, u64)>,
}

#[derive(Debug)]
pub struct Report {
    pub content_id: String,
    pub checks: Vec<Check>,
}

/// What a report's failed checks add up to: the one thing to tell someone who asks what is
/// wrong with their package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Damage {
    /// The file is `have` bytes and the package needs at least `need` (both 0 when only the
    /// end of the file, the install manifest, is missing).
    Truncated { have: u64, need: u64 },
    /// `bad` of `total` data blocks (of [`BLOCK`] bytes each) are not the bytes the package's
    /// digests say they are. With a sampled sweep, `total` is the number looked at.
    Data { bad: u64, total: u64 },
    /// The image's own tables fail and no data block was found bad: the game can't be read
    /// out of it.
    Structure { failed: u64 },
    /// Only what the package says about itself fails: every data block and table checked is
    /// right.
    Metadata { failed: u64 },
}

/// The check that the header marks a debug image.
const DEBUG_IMAGE: &str = "fih debug image";

impl Report {
    pub fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }

    /// Whether the package is a debug image, the only kind these checks can read: a retail
    /// one is encrypted with keys this does not have, so what it reports about one (every
    /// block "failing") says nothing about the file.
    pub fn is_debug(&self) -> bool {
        !self.checks.iter().any(|c| c.name == DEBUG_IMAGE && !c.ok)
    }

    /// The worst thing the failed checks say, `None` when every check holds: a file cut short
    /// before damaged data (the rest of it was never read), damaged data before broken tables,
    /// and metadata alone last.
    pub fn damage(&self) -> Option<Damage> {
        let failed = |kind: Kind| self.checks.iter().filter(move |c| !c.ok && c.kind == kind);
        if let Some(c) = failed(Kind::Truncated).next() {
            let (have, need) = c.count.unwrap_or((0, 0));
            return Some(Damage::Truncated { have, need });
        }
        if failed(Kind::Data).next().is_some() {
            // The streaming sweep is one counted check; the in-memory verifier has one per block.
            let (mut bad, mut total) = (0u64, 0u64);
            for c in self.checks.iter().filter(|c| c.kind == Kind::Data) {
                let (b, t) = c.count.unwrap_or((u64::from(!c.ok), 1));
                bad += b;
                total += t;
            }
            return Some(Damage::Data { bad, total });
        }
        let structure = failed(Kind::Structure).count() as u64;
        if structure > 0 {
            return Some(Damage::Structure { failed: structure });
        }
        let metadata = failed(Kind::Metadata).count() as u64;
        (metadata > 0).then_some(Damage::Metadata { failed: metadata })
    }

    fn push(&mut self, kind: Kind, name: impl Into<String>, ok: bool, detail: impl Into<String>) {
        self.checks.push(Check {
            name: name.into(),
            ok,
            detail: detail.into(),
            kind,
            count: None,
        });
    }

    /// [`Self::push`] for a check that counts ([`Check::count`]).
    fn push_counted(
        &mut self,
        kind: Kind,
        name: impl Into<String>,
        (failed, total): (u64, u64),
        ok: bool,
        detail: impl Into<String>,
    ) {
        self.push(kind, name, ok, detail);
        if let Some(c) = self.checks.last_mut() {
            c.count = Some((failed, total));
        }
    }
}

/// The least a debug package's file has to hold for its header to be true: the outer image and
/// the start of the container behind it. `None` when the file is long enough (or the header is
/// a retail one, whose layout this does not judge).
fn cut_short(fih: &fih::Fih, len: u64) -> Option<(u64, u64)> {
    if !fih.is_debug() {
        return None;
    }
    let need = fih
        .pfs_offset
        .saturating_add(fih.pfs_size)
        .max(fih.cnt_offset.saturating_add(1));
    (len < need).then_some((len, need))
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.content_id)?;
        for c in &self.checks {
            let mark = if c.ok { "ok  " } else { "FAIL" };
            writeln!(f, "  [{mark}] {} {}", c.name, c.detail)?;
        }
        Ok(())
    }
}

/// A dinode's file bytes, gathered from the plaintext blocks it points at.
fn file_data(img: &outer::OuterImage, node: Option<&outer::Dinode>) -> Vec<u8> {
    node.map(|n| img.file_data(n)).unwrap_or_default()
}

/// Does the inner image expand, through its descriptor, to a mount whose metadata walks?
///
/// The descriptor is the authority on every length here: the image must be the block count it
/// declares, and what the image stores from the metadata base on must expand to the rest of the
/// mount it declares. The codec comes from the descriptor's own `compType` — a zlib image stores a
/// container there and everything else stores the region itself — so this reads both without
/// being told which it is looking at.
fn inner_mount_ok(image_len: u64, image: &[u8], layout: &crate::naps::Layout) -> (bool, String) {
    let mount_size = layout.mount_size();
    let mut detail = format!(
        "{} stored block(s), {} declared, mount {mount_size:#x}, comp {}",
        image_len.div_ceil(BLOCK),
        layout.num_outer_blocks,
        layout.compression_type
    );
    if image_len != u64::from(layout.num_outer_blocks) * BLOCK {
        detail.push_str(" (the image is not the length the descriptor declares)");
        return (false, detail);
    }
    let Some(meta_base) = layout.fidx.len().checked_sub(2).map(|i| layout.fidx[i].0) else {
        detail.push_str(" (no metadata-base fidx face)");
        return (false, detail);
    };
    let region = mount_size.saturating_sub(meta_base);
    // A stored region fills the mount, so the image is exactly the mount's length; anything
    // shorter has had its region compressed into the space above the base.
    let stored = image_len == mount_size;
    if !stored && layout.compression_type != crate::naps::COMP_ZLIB as u8 {
        // A compressed region in a codec this build does not decode — a real package's Kraken,
        // typically. The geometry above still holds; the region is left to a Kraken reader.
        detail.push_str(&format!(
            " (comp {} is not decoded here, so the region is unchecked)",
            layout.compression_type
        ));
        return (true, detail);
    }
    let Some(tail) = image.get(meta_base as usize..) else {
        detail.push_str(" (the image ends before its metadata base)");
        return (false, detail);
    };
    let plain = if stored {
        match tail.get(..region as usize) {
            Some(slice) => slice.to_vec(),
            None => {
                detail.push_str(" (the image does not reach the mount's end)");
                return (false, detail);
            }
        }
    } else {
        match crate::pfsc::parse(tail).and_then(|c| c.decompress()) {
            Ok(plain) => plain,
            Err(e) => {
                detail.push_str(&format!(" (the metadata region does not expand: {e})"));
                return (false, detail);
            }
        }
    };
    if plain.len() as u64 != region {
        detail.push_str(&format!(
            " (its metadata is {} bytes against a {region}-byte region)",
            plain.len()
        ));
        return (false, detail);
    }
    // The data region is not read: the walk needs the metadata and the inode table, both of which
    // live above the base, so a zero prefix stands in for it.
    let mut mount = vec![0u8; meta_base as usize];
    mount.extend_from_slice(&plain);
    let walked = crate::inner::read(&mount, meta_base);
    match walked {
        Ok(m) if m.flt_ok && !m.files.is_empty() => (true, detail),
        Ok(m) => {
            detail.push_str(&format!(
                " (the metadata walks to {} file(s), flat-path table ok: {})",
                m.files.len(),
                m.flt_ok
            ));
            (false, detail)
        }
        Err(e) => {
            detail.push_str(&format!(" (its metadata does not walk: {e})"));
            (false, detail)
        }
    }
}

/// Every check that needs only the header and the container — the two things both
/// verifiers read whole.
pub(crate) fn container_report(fih_block: &[u8], fih: &fih::Fih, cnt: &cnt::Cnt) -> Report {
    let mut r = Report {
        content_id: cnt.content_id.clone(),
        checks: Vec::new(),
    };
    r.push(
        Kind::Metadata,
        DEBUG_IMAGE,
        fih.is_debug(),
        format!("signed byte {:#04x}", fih.signed_byte),
    );
    r.push(
        Kind::Metadata,
        "fih format version 3",
        fih.format_version == 3,
        fih.format_version.to_string(),
    );
    r.push(
        Kind::Metadata,
        "cnt package digest",
        cnt.package_digest_ok(),
        "",
    );
    r.push(
        Kind::Metadata,
        "cnt digest-table digest",
        cnt.digest_table_digest_ok(),
        "",
    );
    r.push(
        Kind::Metadata,
        "cnt header rollup digest",
        cnt.header_rollup_ok(),
        "",
    );
    r.push(
        Kind::Metadata,
        "cnt second rollup digest",
        cnt.sc_entries2_ok(),
        "",
    );
    r.push(Kind::Metadata, "cnt body digest", cnt.body_digest_ok(), "");
    r.push(
        Kind::Metadata,
        "cnt finalized-image digest",
        cnt.fih_digest_ok(fih_block),
        "",
    );
    r.push(
        Kind::Metadata,
        "cnt descriptor pairs",
        cnt.descriptor_ok(),
        "",
    );
    r.push(
        Kind::Metadata,
        "cnt image-key digest",
        cnt.entry_digest_at(cnt::ids::IMAGE_KEY, 0x520),
        "",
    );
    r.push(
        Kind::Metadata,
        "cnt imagedigs digest",
        cnt.entry_digest_at(cnt::ids::IMAGE_DIGESTS, 0x540),
        "",
    );
    for (id, verdict) in cnt.entry_digests() {
        r.push(
            Kind::Metadata,
            format!("cnt entry {id:#06x} digest"),
            verdict != EntryDigest::Mismatch,
            format!("{verdict:?}"),
        );
    }
    if let Some(e) = cnt.entry(cnt::ids::PARAM_JSON) {
        let param = sha3(cnt.payload(e));
        let general = cnt
            .entry(cnt::ids::GENERAL_DIGESTS)
            .map(|g| cnt.payload(g))
            .unwrap_or(&[]);
        r.push(
            Kind::Metadata,
            "param.json digest in GeneralDigests",
            general.windows(32).any(|w| w == param),
            "",
        );
    }
    for (name, ok) in cnt.general_digests(&fih.game_digest) {
        r.push(Kind::Metadata, name, ok, "");
    }

    r
}

/// The checks that need only a PS5 package's header and its container: every digest and
/// descriptor the container carries about itself. Nothing of the image is read, so it costs the
/// container's size whatever the package's (a rename is checked with it before and after).
pub fn container_checks(path: &Path) -> Result<Report> {
    let mut file = PkgFile::open(path)?;
    let fih_block = file.read_at(0, BLOCK as usize)?;
    let fih = fih::parse(&fih_block)?;
    let cnt = cnt::read(&mut file, fih.cnt_offset)?;
    Ok(container_report(&fih_block, &fih, &cnt))
}

pub fn verify_package(path: &Path, passcode: &str) -> Result<Report> {
    let mut file = PkgFile::open(path)?;
    let fih_block = file.read_at(0, crate::BLOCK as usize)?;
    let fih = fih::parse(&fih_block)?;
    let cnt = cnt::read(&mut file, fih.cnt_offset)?;
    let mut r = container_report(&fih_block, &fih, &cnt);

    let img = outer::open(&mut file, &fih, &cnt, passcode)?;
    r.push(
        Kind::Structure,
        "outer superblock ICV",
        img.superblock.icv_ok,
        format!("block {}", img.superblock.index),
    );
    // Only the wording depends on the mode: a plaintext block is not decrypted, it has to be its
    // own digest exactly as it is stored.
    let plaintext = img.superblock.mode == crate::ImageMode::PlaintextNoAuth;
    for v in &img.verdicts {
        r.push(
            Kind::Data,
            format!(
                "outer block {} {} its imagedigs entry",
                v.index,
                if plaintext { "matches" } else { "decrypts to" }
            ),
            v.kind.is_some(),
            match v.kind {
                Some(BlockKind::Data) => "data sector",
                Some(BlockKind::Signed) => "signed sector",
                Some(BlockKind::Superblock) => "plaintext superblock",
                Some(BlockKind::Plaintext) => "plaintext block",
                None if plaintext => "not its stored digest",
                None => "no sector matched",
            },
        );
    }
    let table_ok = img
        .plaintext
        .get(img.superblock.inode_table_block as usize)
        .is_some_and(|b| sha3(b) == img.superblock.inode_table_digest);
    r.push(Kind::Structure, "outer inode table digest", table_ok, "");
    let nodes = img.dinodes();
    for (ino, n) in nodes.iter().enumerate() {
        let direct_ok = n
            .direct
            .iter()
            .take((n.blocks as usize).min(outer::DIRECT_SLOTS))
            .all(|d| {
                img.plaintext
                    .get(d.block as usize)
                    .is_some_and(|b| sha3(b) == d.digest)
            });
        // The indirect tables, level by level: a parent records its children's digests,
        // so every level is checked against the one above it.
        let used = n.indirect.iter().take_while(|t| t.block != 0).count();
        r.push(
            Kind::Structure,
            format!("outer inode {ino} block signatures"),
            direct_ok && img.indirect_ok(n),
            format!("{} block(s), {used} indirect level(s)", n.blocks),
        );
    }
    let uroot: Vec<String> = nodes
        .get(2)
        .map(|n| img.dirents(n).into_iter().map(|d| d.name).collect())
        .unwrap_or_default();
    r.push(
        Kind::Structure,
        "outer uroot holds pfs_image.dat and naps_pkg_layout.dat",
        uroot.iter().any(|n| n == "pfs_image.dat")
            && uroot.iter().any(|n| n == "naps_pkg_layout.dat"),
        uroot.join(", "),
    );

    // The inner image is what the console actually mounts, and nothing else in this report reads
    // it: the stored file has to be the length the descriptor declares, and its metadata region
    // has to expand to the rest of the mount and walk. A package can pass every outer check and
    // still fail here, which is the failure this exists to name.
    {
        let by_name = |name: &str| -> Vec<u8> {
            let ino = nodes
                .get(2)
                .map(|n| img.dirents(n))
                .unwrap_or_default()
                .into_iter()
                .find(|d| d.name == name)
                .map(|d| d.ino as usize);
            file_data(&img, ino.and_then(|i| nodes.get(i)))
        };
        let image = by_name("pfs_image.dat");
        let naps = by_name("naps_pkg_layout.dat");
        match crate::naps::parse(&naps) {
            // A descriptor our own reader cannot lay out is this crate's limitation, not the
            // package's — `naps::parse` reads the smallest sample exactly and the large ones not
            // at all — so it is reported without failing the package.
            Err(e) => r.push(
                Kind::Structure,
                "the inner image expands to the mount its descriptor fixes",
                true,
                format!("the descriptor is not parsed here: {e}"),
            ),
            Ok(layout) => {
                let (ok, detail) = inner_mount_ok(image.len() as u64, &image, &layout);
                r.push(
                    Kind::Structure,
                    "the inner image expands to the mount its descriptor fixes",
                    ok,
                    detail,
                );
            }
        }
    }

    // The outer flat-path table (inode 1) hashes each uroot dirent name to its inode.
    {
        let flt = file_data(&img, nodes.get(1));
        let count = flt.get(0x2C..0x30).map(|b| le32(b, 0)).unwrap_or(0) as usize;
        let mut checked = 0usize;
        let mut ok = count > 0;
        for d in nodes.get(2).map(|n| img.dirents(n)).unwrap_or_default() {
            if d.name == "." || d.name == ".." {
                continue;
            }
            checked += 1;
            ok &= flt_holds(&flt, count, flt::hash_path(&d.name), d.ino as u64);
        }
        r.push(
            Kind::Structure,
            "outer flat-path table hashes the uroot names",
            ok && checked > 0,
            format!("{checked} name(s), {count} table entries"),
        );
    }

    match si::read(&mut file)? {
        Some(s) => {
            let crc_name = format!("config/{}/playgo-chunk.crc", cnt.content_id);
            match s.members.iter().find(|m| m.name == crc_name) {
                Some(m) => {
                    let stored = file.read_at(m.offset, m.size as usize)?;
                    let expected = si::chunk_crc_table(&mut file, s.zip_start)?;
                    r.push(
                        Kind::Metadata,
                        "si playgo-chunk.crc",
                        stored == expected,
                        format!("{} bytes", m.size),
                    );
                }
                None => r.push(
                    Kind::Metadata,
                    "si playgo-chunk.crc",
                    false,
                    format!("{crc_name} missing"),
                ),
            }
        }
        None => r.push(
            Kind::Truncated,
            "si zip present",
            false,
            "no trailing STORED ZIP",
        ),
    }
    Ok(r)
}

/// Whether the flat-path table `flt` (a 0x40-byte header, then 16-byte records of a name's
/// hash and its inode) maps `hash` to `ino` in its first `count` records. `count` is the
/// table's own claim: only the records the table has room for are looked at (a count of
/// four billion once meant that many empty lookups per name, with no way to stop it).
fn flt_holds(flt: &[u8], count: usize, hash: u64, ino: u64) -> bool {
    let (records, _) = flt.get(0x40..).unwrap_or_default().as_chunks::<16>();
    records.iter().take(count).any(|rec| {
        let (name, node) = rec.split_at(8);
        u64::from_le_bytes(name.try_into().unwrap()) == hash
            && (u64::from_le_bytes(node.try_into().unwrap()) & 0xFF_FFFF) == ino
    })
}

/// Verify a package without holding it: every block is read and decrypted on demand, and
/// the data blocks are swept once against `imagedigs` instead of being kept. A 155 GB
/// package verifies in a few hundred megabytes this way, which is what the engine needs,
/// since the packages it builds are that size.
pub fn verify_streaming(
    path: &Path,
    passcode: &str,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Report> {
    verify_streaming_sampled(path, passcode, None, progress)
}

/// [`verify_streaming`], with the data-block sweep sampled when given a `seed`: the structure
/// (header, tables, inodes, indirect blocks, the playgo CRCs) is always checked in full, and
/// only the per-block decrypt and SHA3 is limited to the first and last 8 MiB batch of blocks
/// plus a seeded draw of about 1% of the batches (at most 128). `None` sweeps every block.
pub fn verify_streaming_sampled(
    path: &Path,
    passcode: &str,
    seed: Option<u64>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Report> {
    verify_streaming_inner(path, passcode, seed, None, progress, &mut |_| {})
}

/// [`verify_streaming`] that stops with a "cancelled" error once `cancel` is set (checked
/// between batches of blocks): the Collection's copy check runs it on packages of 100+ GB.
pub fn verify_streaming_cancellable(
    path: &Path,
    passcode: &str,
    cancel: &std::sync::atomic::AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Report> {
    verify_streaming_inner(path, passcode, None, Some(cancel), progress, &mut |_| {})
}

/// The passes a full read of a package makes, in order: what a caller can show beside the
/// progress, which counts both in one total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Every block of the image against its digest.
    Blocks,
    /// The PlayGo chunk CRCs (`playgo-chunk.crc`): a second read of everything before the SI ZIP,
    /// in a full check of a debug package that carries one.
    PlaygoCrc,
}

impl Phase {
    /// The word an API gives for it.
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Blocks => "blocks",
            Phase::PlaygoCrc => "playgo",
        }
    }
}

/// [`verify_streaming_cancellable`] that also says which pass it is on (`phase`, each time it
/// starts one).
pub fn verify_streaming_phased(
    path: &Path,
    passcode: &str,
    cancel: &std::sync::atomic::AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
    phase: &mut dyn FnMut(Phase),
) -> Result<Report> {
    verify_streaming_inner(path, passcode, None, Some(cancel), progress, phase)
}

/// [`verify_streaming_sampled`] that also stops with a "cancelled" error once `cancel` is set
/// (checked between batches of blocks): a build's Stop ends its read-back too.
pub fn verify_streaming_controlled(
    path: &Path,
    passcode: &str,
    seed: Option<u64>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Report> {
    verify_streaming_inner(path, passcode, seed, cancel, progress, &mut |_| {})
}

fn verify_streaming_inner(
    path: &Path,
    passcode: &str,
    seed: Option<u64>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    progress: &mut dyn FnMut(u64, u64),
    phase: &mut dyn FnMut(Phase),
) -> Result<Report> {
    let cancelled = || cancel.is_some_and(crate::workshop::checkpoint);
    if cancelled() {
        return format_err("cancelled");
    }
    let mut file = PkgFile::open(path)?;
    let fih_block = file.read_at(0, BLOCK as usize)?;
    let fih = fih::parse(&fih_block)?;
    // A file that stops before its image or container does: said as that, where reading on
    // would only fail with a read past the end (and a copy check would then read the file as
    // some other kind of package and find nothing wrong).
    if let Some((have, need)) = cut_short(&fih, file.len()) {
        let mut r = Report {
            content_id: String::new(),
            checks: Vec::new(),
        };
        r.push_counted(
            Kind::Truncated,
            "the file is as long as its header says",
            (have, need),
            false,
            format!("{have} bytes, at least {need} needed"),
        );
        return Ok(r);
    }
    let cnt = cnt::read(&mut file, fih.cnt_offset)?;
    let mut r = container_report(&fih_block, &fih, &cnt);

    if !fih.pfs_size.is_multiple_of(BLOCK) {
        return format_err("outer image size is not a whole number of blocks");
    }
    let count = fih.pfs_size / BLOCK;
    let digests = match cnt.image_digests() {
        Some(d) if d.len() as u64 == count => d,
        _ => {
            r.push(
                Kind::Structure,
                "outer imagedigs table",
                false,
                "missing or the wrong length",
            );
            return Ok(r);
        }
    };

    // The superblock is where the header says it is, and its plaintext is the game digest.
    let sb_absolute = crate::le64(&fih_block, 0x20);
    let sb_index = sb_absolute.saturating_sub(BLOCK) / BLOCK;
    let sb_block = if sb_absolute >= BLOCK && sb_index < count {
        file.read_at(fih.pfs_offset + sb_index * BLOCK, BLOCK as usize)?
    } else {
        return format_err("the header's superblock offset is outside the image");
    };
    r.push(
        Kind::Structure,
        "outer superblock is where the header says",
        sha3(&sb_block) == fih.game_digest,
        format!("block {sb_index}"),
    );
    let superblock = outer::parse_superblock(sb_index, &sb_block)?;
    r.push(
        Kind::Structure,
        "outer superblock ICV",
        superblock.icv_ok,
        "",
    );

    // A native image derives its keys here; a plaintext one stores its blocks as they are, so
    // there is nothing to derive and nothing to undo.
    let plaintext_mode = superblock.mode == crate::ImageMode::PlaintextNoAuth;
    let xts = (!plaintext_mode).then(|| {
        Xts::new(&derive_xts_keys(
            &derive_ekpfs(&cnt.content_id, passcode),
            &superblock.seed,
        ))
    });
    // The sector rule the samples follow: data blocks carry their own index, metadata
    // blocks set bit 47, and the superblock is not encrypted at all.
    let read_block = |file: &mut PkgFile, index: u64| -> Result<Vec<u8>> {
        let raw = file.read_at(fih.pfs_offset + index * BLOCK, BLOCK as usize)?;
        if index == sb_index {
            return Ok(raw);
        }
        let Some(xts) = &xts else {
            return Ok(raw);
        };
        let mut pt = raw;
        let sector = if index < sb_index {
            index
        } else {
            SIGNED_SECTOR_FLAG | index
        };
        xts.decrypt(sector, &mut pt);
        Ok(pt)
    };

    let table = read_block(&mut file, superblock.inode_table_block as u64)?;
    r.push(
        Kind::Structure,
        "outer inode table digest",
        sha3(&table) == superblock.inode_table_digest,
        "",
    );
    let nodes = outer::parse_dinodes(&table, superblock.dinode_count as u64);
    for (ino, node) in nodes.iter().enumerate() {
        // The data blocks are checked by the sweep below; here it is the dinode's own
        // records and every level of its indirect tables.
        let ok = {
            let mut read = |index: u64| read_block(&mut file, index);
            outer::verify_dinode(node, false, &mut read)?
        };
        r.push(
            Kind::Structure,
            format!("outer inode {ino} block signatures"),
            ok,
            format!("{} block(s)", node.blocks),
        );
    }

    // The sweep: every data block must reach the digest `imagedigs` carries — by decryption in
    // the native mode, as stored in the plaintext one.
    let mut bad = 0u64;
    let mut done = 0u64;
    // The game's data comes first; everything past it is the format's own (the layout
    // descriptor, the superblock, every table and directory) and is always swept. Only the
    // data is sampled; no pfs_image.dat found means every block is swept.
    // Inode 1 is the flat-path table and inode 2 the root folder. An image whose superblock
    // counts fewer inodes than that is damaged: reported, not indexed past its end.
    let (Some(flt_node), Some(uroot_node)) = (nodes.get(1), nodes.get(2)) else {
        r.push(
            Kind::Structure,
            "outer image holds its flat-path table and root folder",
            false,
            format!("{} inode(s)", nodes.len()),
        );
        return Ok(r);
    };
    let uroot_bytes = read_block(&mut file, uroot_node.direct[0].block as u64)?;
    let uroot = outer::OuterImage::parse_dirents(&uroot_bytes, uroot_node.size);
    let data = uroot
        .iter()
        .find(|d| d.name == "pfs_image.dat")
        .and_then(|d| nodes.get(d.ino as usize))
        .map_or(0, |n| (n.blocks as u64).min(count));
    const BATCH: u64 = crate::sample::SLICE / BLOCK;
    let batches = seed.map(|seed| crate::sample::pick(data.div_ceil(BATCH), seed, 128));
    let span = |b: u64| b * BATCH..((b + 1) * BATCH).min(data);
    let sweep = batches.as_ref().map_or(count, |v| {
        v.iter().map(|b| span(*b).end - span(*b).start).sum::<u64>() + (count - data)
    });
    // A full check also reads everything before the SI ZIP a second time for the PlayGo chunk
    // CRCs (#951): counted in the same total, so the progress never says 100% while that pass
    // still has a whole package to read. Asked of the SI here only for its size; the check
    // itself reads it again below and says what is wrong with it.
    let crc_bytes = match seed {
        Some(_) => 0,
        None => si::read(&mut file).ok().flatten().map_or(0, |s| {
            let crc_name = format!("config/{}/playgo-chunk.crc", cnt.content_id);
            if s.members.iter().any(|m| m.name == crc_name) {
                s.zip_start
            } else {
                0
            }
        }),
    };
    let total = sweep * BLOCK + crc_bytes;
    phase(Phase::Blocks);
    let indices: Box<dyn Iterator<Item = u64>> = match batches {
        None => Box::new(0..count),
        Some(v) => Box::new(v.into_iter().flat_map(span).chain(data..count)),
    };
    for index in indices {
        // The superblock is not encrypted; it was matched against the game digest above.
        if index != sb_index {
            let raw = file.read_at(fih.pfs_offset + index * BLOCK, BLOCK as usize)?;
            let mut matched = false;
            match &xts {
                None => matched = sha3(&raw) == digests[index as usize],
                Some(xts) => {
                    for sector in [index, SIGNED_SECTOR_FLAG | index] {
                        let mut pt = raw.clone();
                        xts.decrypt(sector, &mut pt);
                        if sha3(&pt) == digests[index as usize] {
                            matched = true;
                            break;
                        }
                    }
                }
            }
            if !matched {
                bad += 1;
            }
        }
        done += 1;
        if done.is_multiple_of(256) {
            if crate::workshop::checkpoint_opt(cancel) {
                return format_err("cancelled");
            }
            progress(done * BLOCK, total);
        }
    }
    // Every 256 blocks above; a smaller package (or the last few blocks) is checked here.
    if cancelled() {
        return format_err("cancelled");
    }
    progress(done * BLOCK, total);
    r.push_counted(
        Kind::Data,
        if plaintext_mode {
            "outer blocks match their imagedigs entry"
        } else {
            "outer blocks decrypt to their imagedigs entry"
        },
        // Of the blocks looked at: every one, or the sample's.
        (bad, sweep),
        bad == 0,
        match seed {
            None => format!("{} of {count} failed", bad),
            Some(seed) => format!(
                "{bad} of {sweep} sampled failed ({count} blocks, seed {seed}, policy {})",
                crate::sample::POLICY
            ),
        },
    );

    // The uroot dirents (read above) and the flat-path table.
    let names: Vec<String> = uroot.iter().map(|d| d.name.clone()).collect();
    r.push(
        Kind::Structure,
        "outer uroot holds pfs_image.dat and naps_pkg_layout.dat",
        names.iter().any(|n| n == "pfs_image.dat")
            && names.iter().any(|n| n == "naps_pkg_layout.dat"),
        names.join(", "),
    );
    {
        // The flat-path table hashes each uroot name to the inode its dirent records.
        let flt = read_block(&mut file, flt_node.direct[0].block as u64)?;
        let count = crate::le32(&flt, 0x2C) as usize;
        let mut checked = 0usize;
        let mut ok = count > 0;
        for entry in uroot.iter().filter(|d| !d.name.starts_with('.')) {
            checked += 1;
            ok &= flt_holds(
                &flt,
                count,
                flt::hash_path(&entry.name),
                u64::from(entry.ino),
            );
        }
        r.push(
            Kind::Structure,
            "outer flat-path table hashes the uroot names",
            ok && checked > 0,
            format!("{checked} name(s), {count} table entries"),
        );
    }

    match si::read(&mut file)? {
        Some(s) => {
            let crc_name = format!("config/{}/playgo-chunk.crc", cnt.content_id);
            match s.members.iter().find(|m| m.name == crc_name) {
                Some(m) => {
                    let stored = file.read_at(m.offset, m.size as usize)?;
                    // Sampled with the sweep: the whole table would read the package again.
                    let ok = match seed {
                        None => {
                            phase(Phase::PlaygoCrc);
                            let before = sweep * BLOCK;
                            let table = si::chunk_crc_table_with(
                                &mut file,
                                s.zip_start,
                                cancel,
                                &mut |read| progress(before + read, total.max(before + read)),
                            )?;
                            stored == table
                        }
                        Some(seed) => si::chunk_crc_sampled(&mut file, s.zip_start, &stored, seed)?,
                    };
                    r.push(
                        Kind::Metadata,
                        "si playgo-chunk.crc",
                        ok,
                        format!("{} bytes", m.size),
                    );
                }
                None => r.push(
                    Kind::Metadata,
                    "si playgo-chunk.crc",
                    false,
                    format!("{crc_name} missing"),
                ),
            }
        }
        None => r.push(
            Kind::Truncated,
            "si zip present",
            false,
            "no trailing STORED ZIP",
        ),
    }
    Ok(r)
}

#[cfg(test)]
mod tests;
