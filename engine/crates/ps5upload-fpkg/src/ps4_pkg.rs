//! A PS4 fake package as a source tree: the game's files read back out of a `.pkg`, so it converts
//! to an image or a folder as a PS5 one does.
//!
//! The layout and keys, as fthdgn-gh/Alembic (GPL-3.0-or-later) found them on real packages and
//! maxton/LibOrbisPkg (LGPL-3.0) writes them:
//!
//! - a bare container (`\x7FCNT` at 0, no finalized-image header), whose header at 0x400 says
//!   where the PFS image is;
//! - an outer PFS of 64 KiB blocks, signed and normally encrypted, per 4 KiB sector numbered by
//!   its offset; block 0 (the superblock) is plain, and its signature proves the passcode;
//! - its one file, `pfs_image.dat`: a zlib PFSC container, or a plain image, of an unsigned PFS,
//!   which [`crate::pfs_source`] reads as it reads a `.ffpfs`;
//! - the container's entries under `sce_sys`: `param.sfo`, icons, trophies, the licence.
//!
//! The keys come from the content ID and the passcode, SHA-256 where a PS5 package has SHA3. Only
//! packages with the usual all-zero passcode open. Another passcode would need the fake RSA key
//! to unwrap the image key, and is refused in words.
//!
//! Three kinds share this layout, told apart by the container header (`content_type` at 0x74,
//! `content_flags` at 0x78) and `param.sfo`'s CATEGORY, as LibOrbisPkg's `PkgBuilder` writes
//! them (`VolTypeToContentType`, `VolTypeToContentFlags`) and `ps5upload_pkg::kind::classify`
//! reads them:
//!
//! - a game: type GD (0x1A), CATEGORY `gd`;
//! - an update: type GD with a patch flag (`SUBSEQUENT_PATCH` 0x40000000, `FIRST_PATCH`
//!   0x00100000; `CUMULATIVE_PATCH` is 0x60000000), or type DP (0x1E), which LibOrbisPkg writes
//!   for every `pkg_ps4_patch`; CATEGORY `gp`. Its image holds whole files: those the update
//!   changed or added, not the rest of the game, which the console reads from the game under
//!   it. A delta update (`DELTA_PATCH` 0x41000000: the 0x01000000 bit) holds differences
//!   instead and is refused;
//! - an add-on: type AC (0x1B) with an image of its own files (LibOrbisPkg's `pkg_ps4_ac_data`;
//!   its `sce_sys` files are container entries, not in the image), or AL (0x1C,
//!   `pkg_ps4_ac_nodata`), a licence with no image at all; CATEGORY `ac`.
//!
//! An add-on reads as its image alone, what a game finds at the add-on's mount point; its
//! container entries (`param.sfo`, the icon, the licence) stay in [`Ps4Info`].

use std::io::{Read, Seek, SeekFrom};

use flate2::read::ZlibDecoder;
use ps5upload_pkg::kind::{classify, Kind};
use ps5upload_pkg::{sfo_params, SfoValue};
use sha2::{Digest, Sha256};

use crate::cnt::{self, Cnt};
use crate::crypto::{hmac_sha256, XtsKeys};
use crate::pfs_source::PfsSource;
use crate::source::{SourceFile, SourceTree};
use crate::xts::Xts;
use crate::{be32, be64, format_err, Error, PkgFile, ReadSeek, Result};

const BLOCK: u64 = 0x10000;
const SECTOR: usize = 0x1000;
const PFS_MAGIC: u64 = 20_130_315;
const MODE_64BIT_INODES: u16 = 0x2;
const MODE_ENCRYPTED: u16 = 0x4;
const NEW_CRYPT: u64 = 0x2000_0000_0000_0000;
const SEED_AT: usize = 0x370;
const ICV_AT: usize = 0x380;
const ICV_REGION: usize = 0x5A0;
const INODE: usize = 0x2C8;
const DIRECT_AT: usize = 0x64;
const INDIRECT_AT: usize = 0x214;
/// A 32-byte digest and a block number.
const SIG_STRIDE: usize = 36;
const PFSC_MAGIC: u32 = 0x4353_4650;
const NAMES_ID: u32 = 0x200;
/// 1 GiB of inodes, over a million files: more is a damaged package, not a bigger game.
const MAX_INODE_BLOCKS: u64 = 1 << 14;
/// The root folder holds a few names; this is already generous.
const MAX_ROOT_FOLDER: u64 = 64 << 20;
/// `content_flags`' bit that `DELTA_PATCH` (0x41000000) adds to `SUBSEQUENT_PATCH`.
const DELTA_FLAG: u32 = 0x0100_0000;
/// The container entry of `param.sfo`.
const PARAM_SFO_ID: u32 = 0x1000;

/// Why a licence-only add-on converts to nothing.
pub const LICENCE_ONLY: &str = "this PS4 add-on is a licence only: it unlocks something in the \
     game and holds no files, so there is nothing to convert";

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn le64(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(v)
}

fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// `SHA256(SHA256(index as BE32) ‖ SHA256(content id padded to 48) ‖ passcode)`.
pub(crate) fn package_key(content_id: &str, passcode: &str, index: u32) -> [u8; 32] {
    let mut cid = [0u8; 48];
    let id = content_id.as_bytes();
    let n = id.len().min(48);
    cid[..n].copy_from_slice(&id[..n]);
    let mut buf = Vec::with_capacity(96);
    buf.extend_from_slice(&sha256(&index.to_be_bytes()));
    buf.extend_from_slice(&sha256(&cid));
    buf.extend_from_slice(passcode.as_bytes());
    sha256(&buf)
}

/// The keys of an outer PFS and whether its superblock's signature holds: tweak and data keys are
/// `HMAC(base, LE32(1) ‖ seed)`, where base is the image key, or `HMAC(image key, seed)` in newer
/// packages; the signature is keyed by `HMAC(image key, LE32(2) ‖ seed)`.
fn outer_keys(ekpfs: &[u8; 32], new_crypt: bool, sb: &[u8]) -> (XtsKeys, bool) {
    let seed = &sb[SEED_AT..SEED_AT + 16];
    let base = if new_crypt {
        hmac_sha256(ekpfs, &[seed])
    } else {
        *ekpfs
    };
    let keys = hmac_sha256(&base, &[&1u32.to_le_bytes(), seed]);
    let sign = hmac_sha256(&base, &[&2u32.to_le_bytes(), seed]);
    let mut region = sb[..ICV_REGION].to_vec();
    region[ICV_AT..ICV_AT + 32].fill(0);
    let ok = hmac_sha256(&sign, &[&region]) == sb[ICV_AT..ICV_AT + 32];
    let mut tweak = [0u8; 16];
    let mut data = [0u8; 16];
    tweak.copy_from_slice(&keys[..16]);
    data.copy_from_slice(&keys[16..]);
    (XtsKeys { tweak, data }, ok)
}

struct Outer {
    file: PkgFile,
    at: u64,
    blocks: u32,
    xts: Option<Xts>,
}

impl Outer {
    fn block(&mut self, index: u32) -> Result<Vec<u8>> {
        if index >= self.blocks {
            return format_err("the PS4 package's PFS points outside its image");
        }
        let mut b = self
            .file
            .read_at(self.at + u64::from(index) * BLOCK, BLOCK as usize)?;
        if index != 0 {
            if let Some(xts) = &self.xts {
                for (s, sector) in b.as_chunks_mut::<SECTOR>().0.iter_mut().enumerate() {
                    xts.decrypt(
                        u64::from(index) * (BLOCK / SECTOR as u64) + s as u64,
                        sector,
                    );
                }
            }
        }
        Ok(b)
    }

    /// A file's data blocks: twelve direct pointers, then trees of depth 1 to 5.
    fn file_blocks(&mut self, inode: &[u8]) -> Result<Vec<u32>> {
        let wanted = le32(inode, 0x60);
        if wanted > self.blocks {
            return format_err(
                "a file in the PS4 package has more blocks than the image: the package is damaged",
            );
        }
        let wanted = wanted as usize;
        let mut out = Vec::with_capacity(wanted);
        for k in 0..12 {
            if out.len() < wanted {
                out.push(le32(inode, DIRECT_AT + k * SIG_STRIDE + 32));
            }
        }
        for level in 0..5 {
            if out.len() >= wanted {
                break;
            }
            let root = le32(inode, INDIRECT_AT + level * SIG_STRIDE + 32);
            if root as i32 > 0 {
                self.walk(root, level + 1, wanted, &mut out)?;
            }
        }
        if out.len() < wanted {
            return format_err("a file's block tree in the PS4 package is incomplete");
        }
        Ok(out)
    }

    fn walk(&mut self, root: u32, depth: usize, wanted: usize, out: &mut Vec<u32>) -> Result<()> {
        let node = self.block(root)?;
        for i in 0..BLOCK as usize / SIG_STRIDE {
            if out.len() >= wanted {
                break;
            }
            let child = le32(&node, i * SIG_STRIDE + 32);
            if depth == 1 {
                out.push(child);
            } else if child as i32 > 0 {
                self.walk(child, depth - 1, wanted, out)?;
            } else {
                return format_err("a block tree in the PS4 package has a missing child");
            }
        }
        Ok(())
    }
}

/// One file of the outer PFS as seekable bytes.
struct OuterStream {
    outer: Outer,
    blocks: Vec<u32>,
    len: u64,
    pos: u64,
    cached: Option<(usize, Vec<u8>)>,
}

impl Read for OuterStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let index = (self.pos / BLOCK) as usize;
        if !matches!(&self.cached, Some((i, _)) if *i == index) {
            let block = self
                .outer
                .block(self.blocks[index])
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
            self.cached = Some((index, block));
        }
        let Some((_, block)) = &self.cached else {
            return Ok(0);
        };
        let from = (self.pos % BLOCK) as usize;
        let n = buf
            .len()
            .min(block.len() - from)
            .min((self.len - self.pos) as usize);
        buf[..n].copy_from_slice(&block[from..from + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

fn seek_to(len: u64, pos: u64, to: SeekFrom) -> std::io::Result<u64> {
    let target = match to {
        SeekFrom::Start(p) => Some(p),
        SeekFrom::End(d) => len.checked_add_signed(d),
        SeekFrom::Current(d) => pos.checked_add_signed(d),
    };
    target.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "seek before the start of the file",
        )
    })
}

impl Seek for OuterStream {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.pos = seek_to(self.len, self.pos, to)?;
        Ok(self.pos)
    }
}

/// A PFSC container of 64 KiB blocks read at any offset. PS4 packages write version 2.
struct Pfsc<R> {
    inner: R,
    offsets: Vec<u64>,
    len: u64,
    pos: u64,
    cached: Option<(usize, Vec<u8>)>,
}

impl<R: Read + Seek> Pfsc<R> {
    fn open(mut inner: R) -> Result<Self> {
        let mut h = [0u8; 0x30];
        inner.seek(SeekFrom::Start(0))?;
        inner.read_exact(&mut h)?;
        if le32(&h, 0) != PFSC_MAGIC
            || !matches!(le32(&h, 8), 2 | 6)
            || le64(&h, 0x10) != BLOCK
            || !le64(&h, 0x28).is_multiple_of(BLOCK)
        {
            return format_err("the PS4 package's pfs_image.dat is not a PFSC this reader knows");
        }
        let (table, len) = (le64(&h, 0x18), le64(&h, 0x28));
        let count = len / BLOCK;
        // Bounded by what can be stored: an offset takes 8 bytes of a file of known size.
        let stored = inner.seek(SeekFrom::End(0))?;
        if (count + 1).saturating_mul(8) > stored {
            return format_err("the PS4 package's PFSC offset table is too large");
        }
        let mut raw = vec![0u8; ((count + 1) * 8) as usize];
        inner.seek(SeekFrom::Start(table))?;
        inner.read_exact(&mut raw)?;
        let offsets: Vec<u64> = raw
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| u64::from_le_bytes(*c))
            .collect();
        if offsets.windows(2).any(|w| w[1] < w[0]) {
            return format_err("the PS4 package's PFSC offsets go backwards");
        }
        Ok(Self {
            inner,
            offsets,
            len,
            pos: 0,
            cached: None,
        })
    }

    fn block(&mut self, index: usize) -> std::io::Result<Vec<u8>> {
        let (from, to) = (self.offsets[index], self.offsets[index + 1]);
        let size = to - from;
        if size > BLOCK {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "a PFSC block is larger than a block",
            ));
        }
        let mut stored = vec![0u8; size as usize];
        self.inner.seek(SeekFrom::Start(from))?;
        self.inner.read_exact(&mut stored)?;
        if size == BLOCK {
            return Ok(stored);
        }
        let mut plain = Vec::with_capacity(BLOCK as usize);
        ZlibDecoder::new(&stored[..])
            .take(BLOCK + 1)
            .read_to_end(&mut plain)?;
        if plain.len() as u64 != BLOCK {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "a PFSC block did not decode to 64 KiB",
            ));
        }
        Ok(plain)
    }
}

impl<R: Read + Seek> Read for Pfsc<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let index = (self.pos / BLOCK) as usize;
        if !matches!(&self.cached, Some((i, _)) if *i == index) {
            let plain = self.block(index)?;
            self.cached = Some((index, plain));
        }
        let Some((_, plain)) = &self.cached else {
            return Ok(0);
        };
        let from = (self.pos % BLOCK) as usize;
        let n = buf
            .len()
            .min(plain.len() - from)
            .min((self.len - self.pos) as usize);
        buf[..n].copy_from_slice(&plain[from..from + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> Seek for Pfsc<R> {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.pos = seek_to(self.len, self.pos, to)?;
        Ok(self.pos)
    }
}

/// CNT entries that are `sce_sys` files even when the names table leaves them out.
fn system_name(id: u32) -> Option<&'static str> {
    Some(match id {
        0x400 => "license.dat",
        0x401 => "license.info",
        0x402 => "nptitle.dat",
        0x403 => "npbind.dat",
        0x404 => "selfinfo.dat",
        0x406 => "imageinfo.dat",
        0x407 => "target-deltainfo.dat",
        0x408 => "origin-deltainfo.dat",
        0x409 => "psreserved.dat",
        _ => return None,
    })
}

/// An entry's clear bytes: a protected one (flags1 bit 31) is AES-128-CBC, its key and IV from
/// `SHA256(table row ‖ package key)`.
fn entry_bytes(c: &Cnt, e: &cnt::Entry, row: &[u8], passcode: &str) -> Vec<u8> {
    if e.flags1 & 0x8000_0000 == 0 {
        return c.payload(e).to_vec();
    }
    let key = package_key(&c.content_id, passcode, (e.flags2 >> 12) & 0xF);
    let mut seed_in = row.to_vec();
    seed_in.extend_from_slice(&key);
    let seed = sha256(&seed_in);
    let mut data = c.padded_payload(e).to_vec();
    data.truncate(data.len() - data.len() % 16);
    let mut k = [0u8; 16];
    k.copy_from_slice(&seed[16..]);
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&seed[..16]);
    crate::crypto::aes128_cbc_decrypt(&k, iv, &mut data);
    data.truncate(e.size as usize);
    data
}

/// The container's entries that are game files, by their path under `sce_sys`.
fn sce_sys_entries(c: &Cnt, passcode: &str) -> Vec<(String, Vec<u8>)> {
    let names = c.entry(NAMES_ID).map(|e| c.payload(e).to_vec());
    let table = be32(&c.bytes, 0x18) as usize;
    let mut out = Vec::new();
    for (i, e) in c.entries.iter().enumerate() {
        // Digests, keys, metadata and the names table are the container's own.
        if e.id < 0x400 || e.id == 0x40A {
            continue;
        }
        let named = names.as_ref().and_then(|blob| {
            let rest = blob.get(e.name_off as usize..)?;
            let end = rest.iter().position(|b| *b == 0)?;
            std::str::from_utf8(&rest[..end]).ok().map(str::to_string)
        });
        let Some(name) = named
            .filter(|n| !n.is_empty())
            .or_else(|| system_name(e.id).map(str::to_string))
        else {
            continue;
        };
        if name.contains("..") || name.starts_with('/') {
            continue;
        }
        let row = &c.bytes[table + i * 32..table + i * 32 + 32];
        out.push((format!("sce_sys/{name}"), entry_bytes(c, e, row, passcode)));
    }
    out
}

/// What a PS4 package is: see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Ps4Kind {
    Game,
    Patch,
    Addon,
}

impl Ps4Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Ps4Kind::Game => "game",
            Ps4Kind::Patch => "patch",
            Ps4Kind::Addon => "addon",
        }
    }

    /// From a `param.sfo` CATEGORY alone, for a folder or image: `None` when it names no kind.
    pub fn of_category(category: &str) -> Option<Self> {
        match ps5upload_pkg::kind::classify_category(category) {
            (Kind::Base, true, _) => Some(Ps4Kind::Game),
            (Kind::Patch, true, _) => Some(Ps4Kind::Patch),
            (Kind::Dlc, true, _) => Some(Ps4Kind::Addon),
            _ => None,
        }
    }
}

/// A PS4 package's container header and `param.sfo`, read without its image.
#[derive(Debug, Clone)]
pub struct Ps4Info {
    pub kind: Ps4Kind,
    pub content_id: String,
    pub content_type: u32,
    pub content_flags: u32,
    /// Whether the header places a PFS image in the file: an add-on that is a licence has none.
    pub has_image: bool,
    /// An update that holds differences from the previous version rather than files.
    pub delta: bool,
    pub params: Vec<(String, SfoValue)>,
}

impl Ps4Info {
    /// A text value of its `param.sfo`, trimmed; `None` when absent or blank.
    pub fn text(&self, key: &str) -> Option<String> {
        sfo_text(&self.params, key)
    }

    /// The title it belongs to: `TITLE_ID`, else the content ID's.
    pub fn title_id(&self) -> String {
        self.text("TITLE_ID")
            .unwrap_or_else(|| ps5upload_pkg::kind::title_id_of_content_id(&self.content_id))
    }
}

/// A text value of `param.sfo` values, trimmed; `None` when absent or blank.
pub fn sfo_text(params: &[(String, SfoValue)], key: &str) -> Option<String> {
    params.iter().find_map(|(k, v)| match v {
        SfoValue::Text(t) if k == key && !t.trim().is_empty() => Some(t.trim().to_string()),
        _ => None,
    })
}

/// Where the header at 0x400 puts the image: (PFS flags, offset, size, whether there is one).
fn image_place(file: &mut PkgFile, len: u64) -> Result<(u64, u64, u64, bool)> {
    let head = file.read_at(0x400, 0x20)?;
    let (flags, at, size) = (be64(&head, 8), be64(&head, 0x10), be64(&head, 0x18));
    let in_file = at.checked_add(size).is_some_and(|end| end <= len);
    Ok((
        flags,
        at,
        size,
        be32(&head, 0) >= 1 && size >= BLOCK && in_file,
    ))
}

fn info_of(c: &Cnt, has_image: bool) -> Ps4Info {
    let params = c
        .entry(PARAM_SFO_ID)
        .and_then(|e| sfo_params(c.payload(e)).ok())
        .unwrap_or_default();
    let (content_type, content_flags) = (be32(&c.bytes, 0x74), be32(&c.bytes, 0x78));
    let category = sfo_text(&params, "CATEGORY").unwrap_or_default();
    let kind = match classify(content_type, content_flags, &category).0 {
        Kind::Base => Ps4Kind::Game,
        Kind::Patch => Ps4Kind::Patch,
        Kind::Dlc => Ps4Kind::Addon,
    };
    Ps4Info {
        kind,
        content_id: c.content_id.clone(),
        content_type,
        content_flags,
        has_image,
        delta: kind == Ps4Kind::Patch && content_flags & DELTA_FLAG != 0,
        params,
    }
}

/// A PS4 package's kind and `param.sfo`, from its container alone (a few KiB, no keys needed).
pub fn peek(reader: Box<dyn ReadSeek>, len: u64) -> Result<Ps4Info> {
    let mut file = PkgFile::from_reader(reader, len);
    let c = cnt::read(&mut file, 0)?;
    let (_, _, _, has_image) = image_place(&mut file, len)?;
    Ok(info_of(&c, has_image))
}

/// [`peek`] at a `.pkg` on this computer or a saved server (`remote://`, `ps5://`); `None` for
/// anything that isn't a PS4 package.
pub fn peek_path(path: &std::path::Path) -> Result<Option<Ps4Info>> {
    let is_pkg = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pkg"));
    if !is_pkg {
        return Ok(None);
    }
    let (mut reader, len): (Box<dyn ReadSeek>, u64) = if crate::remote_source::is_remote(path) {
        crate::remote_source::open_file(path)?
    } else {
        let f = std::fs::File::open(path)?;
        let len = f.metadata()?.len();
        (Box::new(f), len)
    };
    let mut magic = [0u8; 4];
    if reader.read_exact(&mut magic).is_err() || !is_ps4_magic(&magic) {
        return Ok(None);
    }
    reader.seek(SeekFrom::Start(0))?;
    peek(reader, len).map(Some)
}

/// See the module docs.
pub struct Ps4Package {
    info: Ps4Info,
    inner: PfsSource,
    extras: Vec<(String, Vec<u8>)>,
    files: Vec<SourceFile>,
    /// The inner image's empty folders, less those the container's files went into.
    empty_dirs: Vec<String>,
}

/// Whether these are a package's first bytes that a PS5 one never starts with.
pub fn is_ps4_magic(magic: &[u8]) -> bool {
    magic == [0x7F, b'C', b'N', b'T']
}

impl Ps4Package {
    pub fn open(
        reader: Box<dyn ReadSeek>,
        len: u64,
        passcode: &str,
        label: String,
    ) -> Result<Self> {
        let mut file = PkgFile::from_reader(reader, len);
        let c = cnt::read(&mut file, 0)?;
        if c.content_id.len() != 36 {
            return format_err("the PS4 package has no usable content ID");
        }
        let (flags, at, size, has_image) = image_place(&mut file, len)?;
        let info = info_of(&c, has_image);
        if !has_image {
            return format_err(match info.kind {
                Ps4Kind::Addon => LICENCE_ONLY,
                _ => "this PS4 package holds no image, so it has no files to read",
            });
        }
        if info.delta {
            return format_err(
                "this PS4 update is a delta update: it holds differences from the previous \
                 version, not files, so it can't be read or merged here",
            );
        }
        let sb = file.read_at(at, BLOCK as usize)?;
        if le64(&sb, 8) != PFS_MAGIC {
            return format_err("the PS4 package's image has no PFS superblock");
        }
        let mode = le16(&sb, 0x1C);
        if mode & MODE_64BIT_INODES != 0 {
            return format_err("this PS4 package uses 64-bit PFS inodes, which can't be read here");
        }
        let ekpfs = package_key(&c.content_id, passcode, 1);
        let (keys, signature_ok) = outer_keys(&ekpfs, flags & NEW_CRYPT != 0, &sb);
        if !signature_ok {
            return format_err(
                "this PS4 package isn't made with the usual all-zero passcode, or it is damaged, \
                 so its files can't be read",
            );
        }
        let blocks = u32::try_from(size / BLOCK)
            .map_err(|_| Error::Format("the PS4 package's image is too large".into()))?;
        let xts = (mode & MODE_ENCRYPTED != 0).then(|| Xts::new(&keys));
        let mut outer = Outer {
            file,
            at,
            blocks,
            xts,
        };

        // The inode table, after the superblock and its indirect signature blocks.
        let indirect = (0..5)
            .filter(|i| le64(&sb, 0x50 + 0x244 + i * 40 + 8) as i64 > 0)
            .count() as u64;
        let (inode_count, inode_blocks) = (le64(&sb, 0x30), le64(&sb, 0x40));
        let first = 1 + indirect;
        // These sizes come from the file, and the keys that sign them are public.
        if inode_count < 3
            || inode_blocks == 0
            || inode_blocks > MAX_INODE_BLOCKS
            || first + inode_blocks > u64::from(blocks)
            || inode_count > inode_blocks * (BLOCK / INODE as u64)
        {
            return format_err("the PS4 package's inode table is out of range");
        }
        let mut table = Vec::with_capacity((inode_blocks * BLOCK) as usize);
        for b in 0..inode_blocks {
            table.extend(outer.block((first + b) as u32)?);
        }
        let inode = |i: usize| -> Option<&[u8]> {
            ((i as u64) < inode_count).then(|| &table[i * INODE..(i + 1) * INODE])
        };

        // uroot, and the one file in it that matters.
        let uroot = inode(2).ok_or_else(|| Error::Format("no uroot inode".into()))?;
        if le16(uroot, 0) & 0xF000 != 0x4000 {
            return format_err("the PS4 package's image has no root folder");
        }
        if le64(uroot, 8) > MAX_ROOT_FOLDER {
            return format_err("the PS4 package's root folder is too large");
        }
        let mut dir = Vec::new();
        for b in outer.file_blocks(uroot)? {
            if dir.len() as u64 >= le64(uroot, 8) {
                break;
            }
            dir.extend(outer.block(b)?);
        }
        let dir_len = (le64(uroot, 8) as usize).min(dir.len());
        let mut image = None;
        let mut pos = 0;
        while pos + 16 <= dir_len {
            let ino = le32(&dir, pos) as usize;
            let name_len = le32(&dir, pos + 8) as usize;
            let entry = le32(&dir, pos + 12) as usize;
            if entry < 16 || entry > dir_len - pos || name_len > entry - 16 {
                break;
            }
            if &dir[pos + 16..pos + 16 + name_len] == b"pfs_image.dat" {
                image = inode(ino);
            }
            pos += entry;
        }
        let image = image.ok_or_else(|| {
            Error::Format("the PS4 package's image has no pfs_image.dat".to_string())
        })?;
        let data = outer.file_blocks(image)?;
        let stored = (data.len() as u64) * BLOCK;
        let mut stream = OuterStream {
            outer,
            blocks: data,
            len: stored,
            pos: 0,
            cached: None,
        };
        let mut magic = [0u8; 4];
        stream.read_exact(&mut magic)?;
        stream.seek(SeekFrom::Start(0))?;
        let reader: Box<dyn ReadSeek> = if u32::from_le_bytes(magic) == PFSC_MAGIC {
            Box::new(Pfsc::open(stream)?)
        } else {
            Box::new(stream)
        };
        let inner = PfsSource::from_reader(reader, label)?;

        // The container's files go under sce_sys, unless the image already has them. An
        // add-on is its image alone: see the module docs.
        let mut files = inner.files().to_vec();
        let mut extras = Vec::new();
        let entries = if info.kind == Ps4Kind::Addon {
            Vec::new()
        } else {
            sce_sys_entries(&c, passcode)
        };
        for (path, bytes) in entries {
            if files.iter().any(|f| f.path.eq_ignore_ascii_case(&path)) {
                continue;
            }
            files.push(SourceFile {
                path: path.clone(),
                size: bytes.len() as u64,
            });
            extras.push((path, bytes));
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let empty_dirs = still_empty(inner.empty_dirs(), &files);
        Ok(Self {
            info,
            inner,
            extras,
            files,
            empty_dirs,
        })
    }

    /// Its kind and `param.sfo`.
    pub fn info(&self) -> &Ps4Info {
        &self.info
    }

    fn extra(&self, path: &str) -> Option<&[u8]> {
        self.extras
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, b)| b.as_slice())
    }
}

/// The folders of `empty` that no file in `files` is under: a folder empty in the image that
/// the container's files went into (sce_sys/changeinfo with changeinfo.xml from the CNT) has
/// files now, and an image built from this source would read back without it as empty.
pub(crate) fn still_empty(empty: &[String], files: &[SourceFile]) -> Vec<String> {
    empty
        .iter()
        .filter(|d| {
            let dir = d.trim_matches('/');
            !files.iter().any(|f| {
                f.path.len() > dir.len()
                    && f.path.as_bytes()[dir.len()] == b'/'
                    && f.path[..dir.len()].eq_ignore_ascii_case(dir)
            })
        })
        .cloned()
        .collect()
}

impl SourceTree for Ps4Package {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }
    fn empty_dirs(&self) -> &[String] {
        &self.empty_dirs
    }
    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        match self.extra(path) {
            Some(b) => Ok(b.to_vec()),
            None => self.inner.read(path),
        }
    }
    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        match self.extra(path) {
            Some(b) => {
                let start = usize::try_from(offset).unwrap_or(usize::MAX).min(b.len());
                let end = start.saturating_add(len).min(b.len());
                Ok(b[start..end].to_vec())
            }
            None => self.inner.read_range(path, offset, len),
        }
    }
    fn describe(&self) -> String {
        let what = match self.info.kind {
            Ps4Kind::Game => "PS4 package",
            Ps4Kind::Patch => "PS4 update package",
            Ps4Kind::Addon => "PS4 add-on package",
        };
        format!("{what}: {}", self.inner.describe())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn a_folder_the_container_filled_is_not_empty() {
        let f = |p: &str| SourceFile {
            path: p.into(),
            size: 1,
        };
        let empty = vec![
            "sce_sys/changeinfo".to_string(),
            "sce_sys/trophy".to_string(),
            "x".to_string(),
        ];
        let files = vec![f("sce_sys/changeinfo/changeinfo.xml"), f("xy.bin")];
        assert_eq!(still_empty(&empty, &files), ["sce_sys/trophy", "x"]);
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// Worked out independently with Python's hashlib, for a made-up content ID.
    #[test]
    fn the_package_key_is_the_sha256_formula() {
        let cid = "UP0000-CUSA00000_00-TEST000000000000";
        let got = package_key(cid, "00000000000000000000000000000000", 1);
        let mut padded = [0u8; 48];
        padded[..36].copy_from_slice(cid.as_bytes());
        let mut buf = sha256(&1u32.to_be_bytes()).to_vec();
        buf.extend_from_slice(&sha256(&padded));
        buf.extend_from_slice(b"00000000000000000000000000000000");
        assert_eq!(hex(&got), hex(&sha256(&buf)));
    }

    #[test]
    fn a_container_with_no_image_is_refused_in_words() {
        // A bare CNT header, nothing at 0x400.
        let mut bytes = vec![0u8; 0x10000];
        bytes[..4].copy_from_slice(&[0x7F, b'C', b'N', b'T']);
        bytes[0x40..0x64].copy_from_slice(b"UP0000-CUSA00000_00-TEST000000000000");
        let len = bytes.len() as u64;
        let e = Ps4Package::open(
            Box::new(std::io::Cursor::new(bytes)),
            len,
            crate::crypto::DEFAULT_PASSCODE,
            "t".into(),
        )
        .err()
        .expect("refused")
        .to_string();
        assert!(e.contains("holds no image"), "{e}");
    }

    /// A real package from a writer that isn't ours (tests/fixtures/ps4): the container's files
    /// are found, the image decrypts and decodes, and every file reads as it was written.
    #[test]
    fn a_package_from_another_writer_reads_back() {
        let gz: &[u8] = include_bytes!("../tests/fixtures/ps4/synthetic.pkg.gz");
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(gz)
            .read_to_end(&mut bytes)
            .unwrap();
        assert!(is_ps4_magic(&bytes[..4]));
        let len = bytes.len() as u64;
        let mut p = Ps4Package::open(
            Box::new(std::io::Cursor::new(bytes.clone())),
            len,
            crate::crypto::DEFAULT_PASSCODE,
            "t".into(),
        )
        .unwrap();
        let names: Vec<&str> = p.files().iter().map(|f| f.path.as_str()).collect();
        for want in [
            "eboot.bin",
            "data/sub/asset.bin",
            "data/empty.bin",
            "sce_sys/param.sfo",
        ] {
            assert!(names.contains(&want), "{want} in {names:?}");
        }
        let asset: Vec<u8> = (0..40000)
            .flat_map(|i| format!("asset {} ", i % 97).into_bytes())
            .collect();
        assert_eq!(p.read("data/sub/asset.bin").unwrap(), asset);
        assert_eq!(
            p.read_range("data/sub/asset.bin", 70_000, 9).unwrap(),
            asset[70_000..70_009]
        );
        assert_eq!(p.read("data/empty.bin").unwrap(), b"");
        assert_eq!(
            p.read("sce_sys/icon0.png").unwrap(),
            b"\x89PNG\r\n\x1a\nicon"
        );
        let sfo = p.read("sce_sys/param.sfo").unwrap();
        assert!(ps5upload_pkg::sfo_params(&sfo).is_ok());
        // A licence comes out of its protected entry whole, at its own size.
        let licence = p.read("sce_sys/license.dat").unwrap();
        assert_eq!(licence.len(), 1024);
        assert_eq!(
            &licence[..4],
            b"RIF\0",
            "a licence's header, not ciphertext"
        );

        // Any other passcode is refused in words.
        let e = Ps4Package::open(
            Box::new(std::io::Cursor::new(bytes)),
            len,
            "11111111111111111111111111111111",
            "t".into(),
        )
        .err()
        .expect("refused")
        .to_string();
        assert!(e.contains("all-zero passcode"), "{e}");
    }

    /// A fixture package of tests/fixtures/ps4 (see its README), unzipped.
    pub(crate) fn fixture(name: &str) -> Vec<u8> {
        let gz: &[u8] = match name {
            "synthetic" => include_bytes!("../tests/fixtures/ps4/synthetic.pkg.gz"),
            "update" => include_bytes!("../tests/fixtures/ps4/update.pkg.gz"),
            "addon" => include_bytes!("../tests/fixtures/ps4/addon.pkg.gz"),
            "licence" => include_bytes!("../tests/fixtures/ps4/licence.pkg.gz"),
            _ => panic!("no fixture {name}"),
        };
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(gz)
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }

    pub(crate) fn open_fixture(name: &str) -> Result<Ps4Package> {
        let bytes = fixture(name);
        let len = bytes.len() as u64;
        Ps4Package::open(
            Box::new(std::io::Cursor::new(bytes)),
            len,
            crate::crypto::DEFAULT_PASSCODE,
            name.into(),
        )
    }

    fn peek_fixture(name: &str) -> Ps4Info {
        let bytes = fixture(name);
        let len = bytes.len() as u64;
        peek(Box::new(std::io::Cursor::new(bytes)), len).unwrap()
    }

    /// `asset(seed, n)` of tests/fixtures/ps4/make_extras.py.
    pub(crate) fn asset(seed: &str, n: usize) -> Vec<u8> {
        (0..n)
            .flat_map(|i| format!("{seed} {} ", i % 97).into_bytes())
            .collect()
    }

    /// Each kind as LibOrbisPkg writes it (tests/fixtures/ps4/make_extras.py), told apart by
    /// its header and param.sfo.
    #[test]
    fn each_kind_is_told_from_its_header() {
        let game = peek_fixture("synthetic");
        assert_eq!(game.kind, Ps4Kind::Game);
        assert_eq!(
            (game.content_type, game.title_id()),
            (0x1A, "CUSA99990".into())
        );
        let update = peek_fixture("update");
        assert_eq!(update.kind, Ps4Kind::Patch);
        assert_eq!(update.content_type, 0x1E, "LibOrbisPkg writes DP");
        assert!(!update.delta && update.has_image);
        assert_eq!(update.text("APP_VER").as_deref(), Some("01.05"));
        assert_eq!(update.text("CATEGORY").as_deref(), Some("gp"));
        assert_eq!(update.content_id, game.content_id);
        let addon = peek_fixture("addon");
        assert_eq!((addon.kind, addon.content_type), (Ps4Kind::Addon, 0x1B));
        assert!(addon.has_image);
        assert_eq!(addon.title_id(), "CUSA99990");
        let licence = peek_fixture("licence");
        assert_eq!((licence.kind, licence.content_type), (Ps4Kind::Addon, 0x1C));
        assert!(!licence.has_image);
    }

    /// An update reads as its own files and the container's sce_sys; the files are what
    /// make_extras.py wrote (LibOrbisPkg's pkg_extract gives the same bytes: README).
    #[test]
    fn an_update_reads_back() {
        let mut p = open_fixture("update").unwrap();
        assert_eq!(p.info().kind, Ps4Kind::Patch);
        let names: Vec<&str> = p.files().iter().map(|f| f.path.as_str()).collect();
        for want in [
            "eboot.bin",
            "data/added.bin",
            "data/sub/asset.bin",
            "sce_sys/changeinfo/changeinfo.xml",
            "sce_sys/param.sfo",
            "sce_sys/icon0.png",
        ] {
            assert!(names.contains(&want), "{want} in {names:?}");
        }
        assert!(
            !names.contains(&"data/empty.bin"),
            "the game's files aren't in it"
        );
        assert_eq!(p.read("eboot.bin").unwrap(), b"\x7fELF patched eboot 01.05");
        assert_eq!(
            p.read("data/sub/asset.bin").unwrap(),
            asset("patched", 9000)
        );
        assert!(p.describe().starts_with("PS4 update package"));
    }

    /// An add-on is its image alone: no sce_sys, so no param.json is made for it and it is
    /// never taken for a game.
    #[test]
    fn an_addon_reads_as_its_data() {
        let mut p = open_fixture("addon").unwrap();
        assert_eq!(p.info().kind, Ps4Kind::Addon);
        let names: Vec<&str> = p.files().iter().map(|f| f.path.as_str()).collect();
        assert_eq!(names, ["dlc/map.bin", "dlc/readme.txt"]);
        assert_eq!(p.read("dlc/map.bin").unwrap(), asset("map", 8000));
        assert_eq!(p.read("dlc/readme.txt").unwrap(), b"an add-on's own file");
        let e = open_fixture("licence").err().expect("refused").to_string();
        assert!(e.contains(LICENCE_ONLY), "{e}");
    }

    #[test]
    fn a_pfsc_reads_at_any_offset() {
        use flate2::{write::ZlibEncoder, Compression};
        use std::io::Write;
        // Two blocks: one compressed, one stored as a whole block.
        let plain: Vec<u8> = (0..BLOCK * 2).map(|i| (i % 7) as u8).collect();
        let random: Vec<u8> = (0..BLOCK as u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect();
        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        z.write_all(&plain[..BLOCK as usize]).unwrap();
        let first = z.finish().unwrap();
        let data_at = BLOCK;
        let mut file = vec![0u8; data_at as usize];
        file[0..4].copy_from_slice(&PFSC_MAGIC.to_le_bytes());
        file[8..12].copy_from_slice(&2u32.to_le_bytes());
        file[0x10..0x18].copy_from_slice(&BLOCK.to_le_bytes());
        file[0x18..0x20].copy_from_slice(&0x400u64.to_le_bytes());
        file[0x20..0x28].copy_from_slice(&data_at.to_le_bytes());
        file[0x28..0x30].copy_from_slice(&(BLOCK * 2).to_le_bytes());
        let offs = [
            data_at,
            data_at + first.len() as u64,
            data_at + first.len() as u64 + BLOCK,
        ];
        for (i, o) in offs.iter().enumerate() {
            file[0x400 + i * 8..0x408 + i * 8].copy_from_slice(&o.to_le_bytes());
        }
        file.extend_from_slice(&first);
        file.extend_from_slice(&random);
        let mut p = Pfsc::open(std::io::Cursor::new(file)).unwrap();
        p.seek(SeekFrom::Start(BLOCK - 3)).unwrap();
        let mut got = [0u8; 8];
        p.read_exact(&mut got).unwrap();
        assert_eq!(&got[..3], &plain[BLOCK as usize - 3..BLOCK as usize]);
        assert_eq!(&got[3..], &random[..5]);
    }
}
