//! A PS4 fake package written from a PS4 game: its folder, an image of it, a PS4 package read
//! back, or a game with its update merged in, so the console's package installer (and the
//! etaHEN / ShadowMount+ installers) can install it.
//!
//! The bytes are those maxton/LibOrbisPkg (LGPL-3.0, `6434772`) `PkgTool pkg_build` writes for
//! the project its `pkg_makegp4` would extract from the same game, as fthdgn-gh/Alembic
//! (GPL-3.0-or-later) ports them in `src/core/fpkg_ps4_build.cpp` and `fpkg_ps4_pfs.cpp`, which
//! this module follows (from `PKG/PkgBuilder.cs`, `PKG/PkgWriter.cs`, `PKG/Entry.cs`,
//! `PFS/PFSBuilder.cs`, `PFS/FlatPathTable.cs`, `PFS/PFSCWriter.cs`, `Rif/*.cs`,
//! `PlayGo/ChunkDat.cs`, `PlayGo/Manifest.cs`, `SFO/ParamSfo.cs`, `Util/Crypto.cs`):
//!
//! - a container header (`\x7FCNT`, signed under `PkgPublicKeys[3]`), its entries from 0x2000:
//!   the keys, the image key wrapped under the fake key, digests, names, the PlayGo files, a
//!   debug licence, `param.sfo` (its `PUBTOOLINFO` and `PUBTOOLVER` rewritten), and every file
//!   directly in `sce_sys` that has an entry name (`icon0.png`, `pic1.png`, `nptitle.dat`...);
//! - from the next 512 KiB on, an outer PFS of 64 KiB blocks, signed and encrypted (AES-XTS per
//!   4 KiB sector) with keys from the content ID and the all-zero passcode, its seed all zeros;
//! - its one file, `pfs_image.dat`: a version-6 PFSC whose blocks are stored, around an
//!   unsigned PFS of every other file of the game (a `keystone` made when it has none).
//!
//! Differences from LibOrbisPkg, each where its bytes would be wrong: a directory whose entries
//! spill into another block (LibOrbisPkg moves an entry that would end in a block's last 280
//! bytes to the next block, but sizes the folder by the entries alone) is sized to hold them;
//! only the top `sce_sys` gives entries (LibOrbisPkg also leaves out of the image entry-named
//! files of any folder named `sce_sys`); the licence a game already has is kept (Alembic's
//! choice). Where neither applies, the package is LibOrbisPkg's byte for byte (tests).
//!
//! Only a game (CATEGORY `gd`). An update (`gp`) is refused: LibOrbisPkg's own project check
//! refuses one, Alembic writes none, and real updates differ (another content type, PlayGo
//! files, delta info), so nothing here could be checked against a writer that is known to
//! work; a game merged with its update is a game, and packages as one.
//!
//! Streamed: the game's files are read once, block by block, signed and encrypted on every core
//! and written in place; what stays in memory grows with the number of files and blocks (36
//! bytes of signature per 64 KiB), never with the game's size. Each file's SHA-256 is taken on
//! the way, so [`read_back`] can compare the finished package with what went in.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::AtomicBool;

use num_bigint::BigUint;
use sha2::{Digest, Sha256};

use crate::crypto::{hmac_sha256, XtsKeys, DEFAULT_PASSCODE};
use crate::ps4_pkg::package_key;
use crate::source::SourceTree;
use crate::workshop;
use crate::xts::Xts;
use crate::{format_err, keys, license, Error, Result};

const BS: u64 = 0x10000;
const SECTOR: u64 = 0x1000;
const BODY_OFFSET: u64 = 0x2000;
const CONTENT_TYPE_GD: u32 = 0x1A;
const DRM_PS4: u32 = 0xF;
/// Blocks read, signed and encrypted together: 8 MiB.
const BATCH: usize = 128;

fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
fn put_be16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_be_bytes());
}
fn put_be32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_be_bytes());
}
fn put_be64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_be_bytes());
}

fn align(v: u64, to: u64) -> u64 {
    v.div_ceil(to) * to
}

// ── .NET's string order ──────────────────────────────────────────────────────────────────────

/// A byte's rank in .NET's default (culture-aware, ICU root) order for ASCII: whitespace, then
/// punctuation and symbols, then digits, then letters ignoring case (Alembic's
/// `unicode::dotnet_compare`, checked there against .NET 10). Other bytes after ASCII.
fn weight(c: u8) -> u32 {
    const ORDER: &[u8] = b" _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789";
    let c = c.to_ascii_lowercase();
    if c.is_ascii_lowercase() {
        return 100 + u32::from(c - b'a');
    }
    if c != 0 {
        if let Some(i) = ORDER.iter().position(|o| *o == c) {
            return i as u32;
        }
    }
    200 + u32::from(c)
}

/// `String.CompareTo` as LibOrbisPkg sorts names and paths with it: by weight, then the shorter
/// first, then the first lowercase letter first.
pub(crate) fn dotnet_cmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let n = a.len().min(b.len());
    for i in 0..n {
        let (wa, wb) = (weight(a[i]), weight(b[i]));
        if wa != wb {
            return wa.cmp(&wb);
        }
    }
    if a.len() != b.len() {
        return a.len().cmp(&b.len());
    }
    for i in 0..n {
        let (ua, ub) = (a[i].is_ascii_uppercase(), b[i].is_ascii_uppercase());
        if ua != ub {
            return if ua {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }
    }
    Ordering::Equal
}

// ── RSA key wrap ─────────────────────────────────────────────────────────────────────────────

/// MT19937 seeded by an array (`init_by_array`), as LibOrbisPkg's `Util/MersenneTwister.cs`.
struct Mt {
    mt: [u32; 624],
    index: usize,
}

impl Mt {
    const N: usize = 624;
    const M: usize = 397;

    fn new(seed: &[u32]) -> Self {
        let mut mt = [0u32; 624];
        mt[0] = 19_650_218;
        for i in 1..Self::N {
            mt[i] = 1_812_433_253u32
                .wrapping_mul(mt[i - 1] ^ (mt[i - 1] >> 30))
                .wrapping_add(i as u32);
        }
        let (mut i, mut j) = (1usize, 0usize);
        for _ in 0..Self::N.max(seed.len()) {
            mt[i] = (mt[i] ^ (mt[i - 1] ^ (mt[i - 1] >> 30)).wrapping_mul(1_664_525))
                .wrapping_add(seed[j])
                .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= Self::N {
                mt[0] = mt[Self::N - 1];
                i = 1;
            }
            if j >= seed.len() {
                j = 0;
            }
        }
        for _ in 0..Self::N - 1 {
            mt[i] = (mt[i] ^ (mt[i - 1] ^ (mt[i - 1] >> 30)).wrapping_mul(1_566_083_941))
                .wrapping_sub(i as u32);
            i += 1;
            if i >= Self::N {
                mt[0] = mt[Self::N - 1];
                i = 1;
            }
        }
        mt[0] = 0x8000_0000;
        Self { mt, index: Self::N }
    }

    fn next(&mut self) -> u32 {
        let (n, m) = (Self::N, Self::M);
        if self.index >= n {
            let mt = &mut self.mt;
            let twist = |a: u32, b: u32, c: u32| {
                let y = (a & 0x8000_0000) | (b & 0x7FFF_FFFF);
                c ^ (y >> 1) ^ if y & 1 != 0 { 0x9908_B0DF } else { 0 }
            };
            for k in 0..n - m {
                mt[k] = twist(mt[k], mt[k + 1], mt[k + m]);
            }
            for k in n - m..n - 1 {
                mt[k] = twist(mt[k], mt[k + 1], mt[k + m - n]);
            }
            mt[n - 1] = twist(mt[n - 1], mt[0], mt[m - 1]);
            self.index = 0;
        }
        let mut x = self.mt[self.index];
        self.index += 1;
        x ^= x >> 11;
        x ^= (x << 7) & 0x9D2C_5680;
        x ^= (x << 15) & 0xEFC6_0000;
        x ^ (x >> 18)
    }
}

/// `Crypto.RSA2048EncryptKey`: EME-PKCS#1 v1.5 encryption whose non-zero padding comes from
/// MT19937 seeded with `SHA256(SHA256(modulus ‖ data))`, so the same input always wraps the same.
fn mt_wrap(modulus: &[u8], data: &[u8]) -> Vec<u8> {
    let size = modulus.len();
    let mut block = vec![0u8; size];
    block[1] = 2;
    let end = size - data.len() - 1;
    let mut seed_in = modulus.to_vec();
    seed_in.extend_from_slice(data);
    let h = sha256(&sha256(&seed_in));
    let words: Vec<u32> = h
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_be_bytes(*c))
        .collect();
    let mut rng = Mt::new(&words);
    let mut pos = 2;
    while pos < end {
        let mut w = [0u8; 48];
        for j in 0..12 {
            w[j * 4..j * 4 + 4].copy_from_slice(&rng.next().to_be_bytes());
        }
        for b in sha256(&w) {
            if b != 0 && pos < end {
                block[pos] = b;
                pos += 1;
            }
        }
    }
    block[end + 1..].copy_from_slice(data);
    let n = BigUint::from_bytes_be(modulus);
    let c = BigUint::from_bytes_be(&block)
        .modpow(&BigUint::from(65_537u32), &n)
        .to_bytes_be();
    let mut out = vec![0u8; size];
    out[size - c.len()..].copy_from_slice(&c);
    out
}

fn public_key(i: usize) -> &'static [u8] {
    &keys::PS4_PKG_PUBLIC_MODULI[i * 256..(i + 1) * 256]
}

// ── param.sfo as LibOrbisPkg rewrites it ─────────────────────────────────────────────────────

const SFO_UTF8_SPECIAL: u16 = 0x0004;
const SFO_UTF8: u16 = 0x0204;
const SFO_INTEGER: u16 = 0x0404;

#[derive(Clone)]
struct SfoEntry {
    key: String,
    format: u16,
    text: Vec<u8>,
    number: u32,
    max_length: u32,
}

/// `SFO/ParamSfo.cs`: read, a value set, written back sorted by key in .NET's order.
struct Sfo(Vec<SfoEntry>);

impl Sfo {
    fn read(data: &[u8]) -> Result<Self> {
        let bad = |why: &str| Error::Format(format!("the game's sce_sys/param.sfo {why}"));
        if data.len() < 0x14 || &data[..4] != b"\0PSF" {
            return Err(bad("is not a param.sfo"));
        }
        let le16 = |at: usize| u16::from_le_bytes([data[at], data[at + 1]]);
        let le32 = |at: usize| u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
        let (keys, values, count) = (le32(8) as usize, le32(12) as usize, le32(16) as usize);
        if count > 4096 || 0x14 + count * 16 > data.len() || keys > data.len() {
            return Err(bad("is damaged"));
        }
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let row = 0x14 + i * 16;
            let key_at = keys + le16(row) as usize;
            let format = le16(row + 2);
            let length = le32(row + 4) as usize;
            let max_length = le32(row + 8);
            let value_at = values.saturating_add(le32(row + 12) as usize);
            let key = data
                .get(key_at..)
                .and_then(|k| k.split(|b| *b == 0).next())
                .filter(|_| data[key_at..].contains(&0))
                .ok_or_else(|| bad("is damaged"))?;
            let key = String::from_utf8_lossy(key).into_owned();
            let left = data.len().saturating_sub(value_at);
            let mut e = SfoEntry {
                key,
                format,
                text: Vec::new(),
                number: 0,
                max_length,
            };
            match format {
                SFO_INTEGER => {
                    if left < 4 {
                        return Err(bad("is damaged"));
                    }
                    e.number = le32(value_at);
                    e.max_length = 4;
                }
                SFO_UTF8 | SFO_UTF8_SPECIAL => {
                    let take = if format == SFO_UTF8 {
                        length.saturating_sub(1)
                    } else {
                        length
                    };
                    if take > left {
                        return Err(bad("is damaged"));
                    }
                    e.text = data[value_at..value_at + take].to_vec();
                }
                _ => return Err(bad("has a value of a kind LibOrbisPkg can't write")),
            }
            out.push(e);
        }
        Ok(Self(out))
    }

    fn find(&self, key: &str) -> Option<&SfoEntry> {
        self.0.iter().find(|e| e.key == key)
    }

    /// `Value.ToString()`: the text, or `0x%08x` for a number; "" when missing.
    fn display(&self, key: &str) -> String {
        match self.find(key) {
            None => String::new(),
            Some(e) if e.format == SFO_INTEGER => format!("0x{:08x}", e.number),
            Some(e) => String::from_utf8_lossy(&e.text).into_owned(),
        }
    }

    fn set(&mut self, e: SfoEntry) {
        self.0.retain(|v| v.key != e.key);
        self.0.push(e);
    }

    fn write(&self) -> Vec<u8> {
        let mut sorted = self.0.clone();
        sorted.sort_by(|a, b| dotnet_cmp(&a.key, &b.key));
        let key_table = 0x14 + sorted.len() * 16;
        let key_size: usize = sorted.iter().map(|v| v.key.len() + 1).sum();
        let data_size: usize = sorted.iter().map(|v| v.max_length as usize).sum();
        let data_table = align((key_table + key_size) as u64, 4) as usize;
        let mut out = vec![0u8; data_table + data_size];
        // A value longer than its slot grows the file and the next value overwrites, as a
        // MemoryStream does.
        let put = |out: &mut Vec<u8>, at: usize, p: &[u8]| {
            if at + p.len() > out.len() {
                out.resize(at + p.len(), 0);
            }
            out[at..at + p.len()].copy_from_slice(p);
        };
        out[..4].copy_from_slice(b"\0PSF");
        put32(&mut out, 4, 0x101);
        put32(&mut out, 8, key_table as u32);
        put32(&mut out, 12, data_table as u32);
        put32(&mut out, 16, sorted.len() as u32);
        let (mut key_off, mut data_off) = (0usize, 0usize);
        for (i, v) in sorted.iter().enumerate() {
            let bytes = if v.format == SFO_INTEGER {
                v.number.to_le_bytes().to_vec()
            } else {
                let mut b = v.text.clone();
                if v.format == SFO_UTF8 {
                    b.push(0);
                }
                b
            };
            let mut row = [0u8; 16];
            put16(&mut row, 0, key_off as u16);
            put16(&mut row, 2, v.format);
            put32(&mut row, 4, bytes.len() as u32);
            put32(&mut row, 8, v.max_length);
            put32(&mut row, 12, data_off as u32);
            put(&mut out, 0x14 + i * 16, &row);
            let mut key = v.key.as_bytes().to_vec();
            key.push(0);
            put(&mut out, key_table + key_off, &key);
            if !bytes.is_empty() {
                put(&mut out, data_table + data_off, &bytes);
            }
            key_off += v.key.len() + 1;
            data_off += v.max_length as usize;
        }
        out
    }
}

// ── container entry names (PKG/Enums.cs EntryNames) ─────────────────────────────────────────

/// The entry ID of a file directly in `sce_sys`.
fn entry_id_for_name(name: &str) -> Option<u32> {
    const NAMES: [(&str, u32); 27] = [
        ("license.dat", 0x400),
        ("license.info", 0x401),
        ("nptitle.dat", 0x402),
        ("npbind.dat", 0x403),
        ("selfinfo.dat", 0x404),
        ("imageinfo.dat", 0x406),
        ("target-deltainfo.dat", 0x407),
        ("origin-deltainfo.dat", 0x408),
        ("psreserved.dat", 0x409),
        ("param.sfo", 0x1000),
        ("playgo-chunk.dat", 0x1001),
        ("playgo-chunk.sha", 0x1002),
        ("playgo-manifest.xml", 0x1003),
        ("pronunciation.xml", 0x1004),
        ("pronunciation.sig", 0x1005),
        ("pic1.png", 0x1006),
        ("pubtoolinfo.dat", 0x1007),
        ("shareparam.json", 0x100B),
        ("shareoverlayimage.png", 0x100C),
        ("save_data.png", 0x100D),
        ("shareprivacyguardimage.png", 0x100E),
        ("icon0.png", 0x1200),
        ("pic0.png", 0x1220),
        ("snd0.at9", 0x1240),
        ("icon0.dds", 0x1280),
        ("pic0.dds", 0x12A0),
        ("pic1.dds", 0x12C0),
    ];
    if let Some((_, id)) = NAMES.iter().find(|(n, _)| *n == name) {
        return Some(*id);
    }
    // icon0_NN / pic1_NN, .png / .dds, NN 00..30.
    for (prefix, suffix, base) in [
        ("icon0_", ".png", 0x1201),
        ("icon0_", ".dds", 0x1281),
        ("pic1_", ".png", 0x1241),
        ("pic1_", ".dds", 0x12C1),
    ] {
        let Some(nn) = name
            .strip_prefix(prefix)
            .and_then(|r| r.strip_suffix(suffix))
        else {
            continue;
        };
        if nn.len() == 2 && nn.bytes().all(|b| b.is_ascii_digit()) {
            let n: u32 = nn.parse().unwrap_or(99);
            if n < 31 {
                return Some(base + n);
            }
        }
    }
    None
}

/// The entry ID of any path inside `sce_sys`, what `pkg_makegp4` orders a project by.
fn entry_id_for_path(rel: &str) -> Option<u32> {
    if !rel.contains('/') {
        return entry_id_for_name(rel);
    }
    let two = |s: &str| -> Option<u32> {
        (s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    match rel {
        "app/playgo-chunk.dat" => return Some(0x1008),
        "app/playgo-chunk.sha" => return Some(0x1009),
        "app/playgo-manifest.xml" => return Some(0x100A),
        "changeinfo/changeinfo.xml" => return Some(0x1260),
        _ => {}
    }
    if let Some(n) = rel
        .strip_prefix("changeinfo/changeinfo_")
        .and_then(|r| r.strip_suffix(".xml"))
        .and_then(two)
    {
        return (n < 31).then_some(0x1261 + n);
    }
    if let Some(n) = rel
        .strip_prefix("trophy/trophy")
        .and_then(|r| r.strip_suffix(".trp"))
        .and_then(two)
    {
        return Some(0x1400 + n);
    }
    if let Some(r) = rel.strip_prefix("keymap_rp/") {
        if r.len() == 7 && r.starts_with('0') && r.ends_with(".png") {
            let n = two(&r[1..3])?;
            return (1..=10).contains(&n).then_some(0x1600 + n - 1);
        }
        if r.len() == 10 && &r[2..4] == "/0" && r.ends_with(".png") {
            let (i, j) = (two(&r[..2])?, two(&r[4..6])?);
            return (i < 31 && (1..=10).contains(&j)).then_some(0x1610 + 16 * i + j - 1);
        }
    }
    None
}

/// `sce_sys` files the package makes again, never taken from the game; `param.json` is what
/// our images of a PS4 game carry ([`crate::ps4_param`]), never part of a package.
fn generated(name: &str) -> bool {
    matches!(
        name,
        "license.dat"
            | "license.info"
            | "psreserved.dat"
            | "playgo-chunk.dat"
            | "playgo-chunk.sha"
            | "playgo-manifest.xml"
            | "param.json"
    )
}

fn entry_of(sys_rel: Option<&str>) -> Option<u32> {
    let rel = sys_rel?;
    if rel.is_empty() || generated(rel) {
        return None;
    }
    entry_id_for_path(rel)
}

// ── PFS images as LibOrbisPkg builds them ────────────────────────────────────────────────────

#[derive(Clone)]
enum Payload {
    /// A file of the source tree, by its path there.
    Source(String),
    Memory(Vec<u8>),
    /// The outer image's one file, made on the way.
    Inner,
}

struct FsDir {
    name: String,
    parent: Option<usize>,
    dirs: Vec<usize>,
    files: Vec<usize>,
}

struct FsFile {
    name: String,
    parent: usize,
    size: u64,
    /// The inode's second size, and its compressed flag.
    csize: u64,
    compress: bool,
    payload: Payload,
}

/// LibOrbisPkg's `FSDir`/`FSFile` tree: each folder's sub-folders and files in the order added.
struct FsTree {
    dirs: Vec<FsDir>,
    files: Vec<FsFile>,
}

impl FsTree {
    fn new() -> Self {
        Self {
            dirs: vec![FsDir {
                name: String::new(),
                parent: None,
                dirs: Vec::new(),
                files: Vec::new(),
            }],
            files: Vec::new(),
        }
    }

    fn add_dir(&mut self, parent: usize, name: &str) -> usize {
        self.dirs.push(FsDir {
            name: name.to_string(),
            parent: Some(parent),
            dirs: Vec::new(),
            files: Vec::new(),
        });
        let i = self.dirs.len() - 1;
        self.dirs[parent].dirs.push(i);
        i
    }

    fn add_file(&mut self, mut file: FsFile) -> usize {
        let parent = file.parent;
        file.csize = if file.compress { file.csize } else { file.size };
        self.files.push(file);
        let i = self.files.len() - 1;
        self.dirs[parent].files.push(i);
        i
    }

    fn dir_path(&self, d: usize) -> String {
        match self.dirs[d].parent {
            None => String::new(),
            Some(p) => format!("{}/{}", self.dir_path(p), self.dirs[d].name),
        }
    }

    fn file_path(&self, f: usize) -> String {
        format!(
            "{}/{}",
            self.dir_path(self.files[f].parent),
            self.files[f].name
        )
    }
}

const MODE_SIGNED: u16 = 0x1;
const MODE_ENCRYPTED: u16 = 0x4;
const MODE_ALWAYS: u16 = 0x8;
const RX_ONLY: u16 = 0x16D;
const TYPE_DIR: u16 = 0x4000;
const TYPE_FILE: u16 = 0x8000;
const FLAG_COMPRESSED: u32 = 0x1;
const FLAG_UNK2: u32 = 0x4;
const FLAG_UNK3: u32 = 0x8;
const FLAG_READONLY: u32 = 0x10;
const FLAG_INTERNAL: u32 = 0x20000;
const DIRENT_FILE: u32 = 2;
const DIRENT_DIR: u32 = 3;
const DIRENT_DOT: u32 = 4;
const DIRENT_DOTDOT: u32 = 5;
const D32_SIZE: u64 = 0xA8;
const S32_SIZE: u64 = 0x2C8;
/// A folder's next entry starts a new block when this little is left of the block.
const DIRENT_MAX: u64 = 280;

#[derive(Clone)]
struct Dirent {
    ino: u32,
    kind: u32,
    name: String,
}

impl Dirent {
    fn entsize(&self) -> u64 {
        align(self.name.len() as u64 + 17, 8)
    }
    fn write(&self, out: &mut Vec<u8>) {
        let at = out.len();
        out.resize(at + self.entsize() as usize, 0);
        put32(out, at, self.ino);
        put32(out, at + 4, self.kind);
        put32(out, at + 8, self.name.len() as u32);
        put32(out, at + 12, self.entsize() as u32);
        out[at + 16..at + 16 + self.name.len()].copy_from_slice(self.name.as_bytes());
    }
}

#[derive(Clone, Copy, Default)]
struct Inode {
    mode: u16,
    nlink: u16,
    flags: u32,
    size: u64,
    csize: u64,
    blocks: u32,
    db: [i32; 12],
    number: u32,
}

/// Where a block's HMAC (and its number) goes.
#[derive(Clone, Copy)]
struct Sig {
    block: u64,
    offset: u64,
    size: u64,
}

#[derive(Clone, Copy)]
struct NodeRef {
    dir: bool,
    index: usize,
}

struct PfsProps {
    sign: bool,
    encrypt: bool,
    min_blocks: u64,
    time: i64,
    ekpfs: [u8; 32],
    seed: [u8; 16],
}

/// A planned image (`PfsBuilder` up to `CalculatePfsSize`).
struct Pfs {
    tree: FsTree,
    p: PfsProps,
    inodes: Vec<Inode>,
    cr: Option<usize>,
    dir_ino: Vec<usize>,
    file_ino: Vec<usize>,
    dirents: Vec<Vec<Dirent>>,
    super_root_dirents: Vec<Dirent>,
    all_files: Vec<usize>,
    all_nodes: Vec<NodeRef>,
    fpt_map: BTreeMap<u32, u32>,
    collisions: Vec<Vec<Dirent>>,
    fpt_size: u64,
    cr_size: u64,
    dinode_count: u64,
    dinode_block_count: u64,
    ndblock: u64,
    ibs_blocks: u32,
    ibs_flags: u32,
    ibs_size: u64,
    ibs_db: [i64; 12],
    empty_block: u64,
    final_sigs: Vec<Sig>,
    data_sigs: Vec<Sig>,
    file_start: Vec<u64>,
    file_first_sig: Vec<usize>,
}

const SUPER_ROOT: usize = 0;
const FPT: usize = 1;

fn path_hash(path: &str) -> u32 {
    path.bytes().fold(0u32, |h, c| {
        u32::from(c.to_ascii_uppercase()).wrapping_add(h.wrapping_mul(31))
    })
}

impl Pfs {
    fn plan(tree: FsTree, p: PfsProps) -> Result<Self> {
        let n_dirs = tree.dirs.len();
        let n_files = tree.files.len();
        let mut s = Self {
            tree,
            p,
            inodes: Vec::new(),
            cr: None,
            dir_ino: vec![usize::MAX; n_dirs],
            file_ino: vec![usize::MAX; n_files],
            dirents: vec![Vec::new(); n_dirs],
            super_root_dirents: Vec::new(),
            all_files: Vec::new(),
            all_nodes: Vec::new(),
            fpt_map: BTreeMap::new(),
            collisions: Vec::new(),
            fpt_size: 0,
            cr_size: 0,
            dinode_count: 0,
            dinode_block_count: 0,
            ndblock: 0,
            ibs_blocks: 0,
            ibs_flags: FLAG_READONLY,
            ibs_size: 0,
            ibs_db: [0; 12],
            empty_block: 4,
            final_sigs: vec![Sig {
                block: 0,
                offset: 0x380,
                size: 0x5A0,
            }],
            data_sigs: Vec::new(),
            file_start: vec![0; n_files],
            file_first_sig: vec![0; n_files],
        };
        s.setup()?;
        s.layout()?;
        Ok(s)
    }

    fn make_inode(
        &self,
        mode: u16,
        size: u64,
        csize: u64,
        nlink: u16,
        number: u32,
        flags: u32,
    ) -> Inode {
        Inode {
            mode,
            nlink,
            flags: if self.p.sign {
                flags | FLAG_UNK2 | FLAG_UNK3
            } else {
                flags
            },
            size,
            csize,
            blocks: 1,
            db: [0; 12],
            number,
        }
    }

    fn path(&self, n: NodeRef) -> String {
        if n.dir {
            self.tree.dir_path(n.index)
        } else {
            self.tree.file_path(n.index)
        }
    }

    fn ino_of(&self, n: NodeRef) -> usize {
        if n.dir {
            self.dir_ino[n.index]
        } else {
            self.file_ino[n.index]
        }
    }

    /// A folder's bytes: its entries, and the room left where one moved to the next block.
    fn node_size(&self, n: NodeRef) -> u64 {
        if !n.dir {
            return self.tree.files[n.index].size;
        }
        let mut at = 0;
        let mut end = 0;
        for d in &self.dirents[n.index] {
            end = at + d.entsize();
            at = end;
            if at % BS > BS - DIRENT_MAX {
                at = (at / BS + 1) * BS;
            }
        }
        end
    }

    fn ino_offset(&self, number: u32, db: u64) -> u64 {
        BS + S32_SIZE * u64::from(number) + 0x64 + 36 * db
    }

    /// `FSDir.GetAllChildrenDirs`: a folder's sub-folders, then each one's, in turn.
    fn children_dirs(&self, d: usize, out: &mut Vec<usize>) {
        out.extend(&self.tree.dirs[d].dirs);
        for &c in &self.tree.dirs[d].dirs {
            self.children_dirs(c, out);
        }
    }

    fn setup(&mut self) -> Result<()> {
        let mut all_dirs = Vec::new();
        self.children_dirs(0, &mut all_dirs);
        let mut in_order = vec![0];
        in_order.extend(&all_dirs);
        for &d in &in_order {
            self.all_files
                .extend(self.tree.dirs[d].files.iter().copied());
        }
        for &d in &all_dirs {
            if !self.tree.dirs[d].name.is_ascii() {
                return format_err(format!(
                    "a PS4 package can only hold names in plain ASCII, and this game has the \
                     folder {}",
                    self.tree.dir_path(d).trim_start_matches('/')
                ));
            }
        }
        for &f in &self.all_files {
            if !self.tree.files[f].name.is_ascii() {
                return format_err(format!(
                    "a PS4 package can only hold names in plain ASCII, and this game has the \
                     file {}",
                    self.tree.file_path(f).trim_start_matches('/')
                ));
            }
        }
        let mut sorted_dirs = all_dirs.clone();
        sorted_dirs.sort_by(|a, b| dotnet_cmp(&self.tree.dir_path(*a), &self.tree.dir_path(*b)));
        let mut sorted_files = self.all_files.clone();
        sorted_files.sort_by(|a, b| dotnet_cmp(&self.tree.file_path(*a), &self.tree.file_path(*b)));
        for &d in &sorted_dirs {
            self.all_nodes.push(NodeRef {
                dir: true,
                index: d,
            });
        }
        for &f in &self.all_files {
            self.all_nodes.push(NodeRef {
                dir: false,
                index: f,
            });
        }

        let mut seen = std::collections::HashSet::new();
        let collision = !self
            .all_nodes
            .iter()
            .all(|n| seen.insert(path_hash(&self.path(*n))));
        if collision && self.p.sign {
            return format_err("a signed PFS can't hold paths whose hashes collide");
        }

        // SetupRootStructure.
        let internal = FLAG_INTERNAL | FLAG_READONLY;
        let mut number = 0u32;
        let sr = self.make_inode(TYPE_DIR | RX_ONLY, 65536, 65536, 1, number, internal);
        number += 1;
        self.inodes.push(sr);
        let fpt = self.make_inode(TYPE_FILE | RX_ONLY, 0, 0, 1, number, internal);
        number += 1;
        self.inodes.push(fpt);
        if collision {
            self.cr = Some(self.inodes.len());
            let cr = self.make_inode(TYPE_FILE | RX_ONLY, 0, 0, 1, number, internal);
            number += 1;
            self.inodes.push(cr);
        }
        let mut uroot = self.make_inode(TYPE_DIR | RX_ONLY, 65536, 65536, 3, number, FLAG_READONLY);
        self.super_root_dirents.push(Dirent {
            ino: self.inodes[FPT].number,
            kind: DIRENT_FILE,
            name: "flat_path_table".into(),
        });
        if let Some(cr) = self.cr {
            self.super_root_dirents.push(Dirent {
                ino: self.inodes[cr].number,
                kind: DIRENT_FILE,
                name: "collision_resolver".into(),
            });
        }
        self.super_root_dirents.push(Dirent {
            ino: uroot.number,
            kind: DIRENT_DIR,
            name: "uroot".into(),
        });
        self.dirents[0] = vec![
            Dirent {
                ino: uroot.number,
                kind: DIRENT_DOT,
                name: ".".into(),
            },
            Dirent {
                ino: uroot.number,
                kind: DIRENT_DOTDOT,
                name: "..".into(),
            },
        ];
        if self.p.sign {
            self.inodes[SUPER_ROOT].flags &= !FLAG_READONLY;
            self.inodes[FPT].flags &= !FLAG_READONLY;
            uroot.flags &= !FLAG_READONLY;
        }

        // addDirInodes.
        self.dir_ino[0] = self.inodes.len();
        self.inodes.push(uroot);
        for &d in &sorted_dirs {
            let n = self.inodes.len() as u32;
            let ino = self.make_inode(TYPE_DIR | RX_ONLY, 65536, 0, 2, n, FLAG_READONLY);
            let parent = self.tree.dirs[d].parent.unwrap_or(0);
            let parent_ino = self.dir_ino[parent];
            self.dirents[d].push(Dirent {
                ino: n,
                kind: DIRENT_DOT,
                name: ".".into(),
            });
            self.dirents[d].push(Dirent {
                ino: self.inodes[parent_ino].number,
                kind: DIRENT_DOTDOT,
                name: "..".into(),
            });
            self.dirents[parent].push(Dirent {
                ino: n,
                kind: DIRENT_DIR,
                name: self.tree.dirs[d].name.clone(),
            });
            self.inodes[parent_ino].nlink += 1;
            self.dir_ino[d] = n as usize;
            self.inodes.push(ino);
        }
        // addFileInodes.
        for &f in &sorted_files {
            let file = &self.tree.files[f];
            let n = self.inodes.len() as u32;
            let flags = FLAG_READONLY | if file.compress { FLAG_COMPRESSED } else { 0 };
            let mut ino = self.make_inode(TYPE_FILE | RX_ONLY, file.size, file.csize, 1, n, flags);
            ino.blocks = file.size.div_ceil(BS) as u32;
            if self.p.sign {
                ino.flags &= !FLAG_READONLY;
            }
            self.file_ino[f] = n as usize;
            let parent = file.parent;
            let name = file.name.clone();
            self.dirents[parent].push(Dirent {
                ino: n,
                kind: DIRENT_FILE,
                name,
            });
            self.inodes.push(ino);
        }

        // FlatPathTable.Create over every node but the root.
        let mut by_hash: BTreeMap<u32, Vec<NodeRef>> = BTreeMap::new();
        for i in 0..self.all_nodes.len() {
            let n = self.all_nodes[i];
            let h = path_hash(&self.path(n));
            let value = self.ino_of(n) as u32 | if n.dir { 0x2000_0000 } else { 0 };
            self.fpt_map
                .entry(h)
                .and_modify(|v| *v = 0x8000_0000)
                .or_insert(value);
            by_hash.entry(h).or_default().push(n);
        }
        if self.cr.is_some() {
            let mut offset = 0u32;
            let keys: Vec<u32> = self.fpt_map.keys().copied().collect();
            for h in keys {
                if self.fpt_map[&h] != 0x8000_0000 {
                    continue;
                }
                self.fpt_map.insert(h, 0x8000_0000 | offset);
                let mut list = Vec::new();
                for &n in &by_hash[&h] {
                    let d = Dirent {
                        ino: self.ino_of(n) as u32,
                        kind: if n.dir { DIRENT_DIR } else { DIRENT_FILE },
                        name: self.path(n),
                    };
                    offset += d.entsize() as u32;
                    list.push(d);
                }
                offset += 0x18;
                self.collisions.push(list);
            }
            for list in &self.collisions {
                self.cr_size += list.iter().map(Dirent::entsize).sum::<u64>() + 0x18;
            }
        }
        self.fpt_size = self.fpt_map.len() as u64 * 8;
        self.all_nodes.insert(
            0,
            NodeRef {
                dir: true,
                index: 0,
            },
        );
        Ok(())
    }

    fn layout(&mut self) -> Result<()> {
        let bs = BS;
        let sr_number = self.inodes[SUPER_ROOT].number;
        let fpt_number = self.inodes[FPT].number;
        if self.p.sign {
            self.ndblock = 1;
            self.dinode_count = self.inodes.len() as u64;
            self.dinode_block_count = self.dinode_count.div_ceil(bs / S32_SIZE);
            self.ibs_blocks = self.dinode_block_count as u32;
            self.ibs_size = self.dinode_block_count * bs;
            self.ibs_flags = 0;
            for i in 0..self.dinode_block_count {
                if i < 12 {
                    self.ibs_db[i as usize] = 1 + i as i64;
                }
                self.final_sigs.push(Sig {
                    block: 1 + i,
                    offset: 0xB8 + 36 * i,
                    size: bs,
                });
            }
            self.ndblock += self.dinode_block_count;
            let sr_db0 = (self.dinode_block_count + 1) as i32;
            self.inodes[SUPER_ROOT].db[0] = sr_db0;
            self.final_sigs.push(Sig {
                block: sr_db0 as u64,
                offset: self.ino_offset(sr_number, 0),
                size: bs,
            });
            self.ndblock += u64::from(self.inodes[SUPER_ROOT].blocks);

            let fp_db0 = sr_db0 + 1;
            let fp_blocks = self.fpt_size.div_ceil(bs) as u32;
            {
                let fp = &mut self.inodes[FPT];
                fp.db[0] = fp_db0;
                fp.size = self.fpt_size;
                fp.csize = self.fpt_size;
                fp.blocks = fp_blocks;
            }
            self.final_sigs.push(Sig {
                block: fp_db0 as u64,
                offset: self.ino_offset(fpt_number, 0),
                size: bs,
            });
            for i in 1..fp_blocks.min(12) {
                self.inodes[FPT].db[i as usize] = self.ndblock as i32;
                self.ndblock += 1;
                self.final_sigs.push(Sig {
                    block: fp_db0 as u64,
                    offset: self.ino_offset(fpt_number, u64::from(i)),
                    size: bs,
                });
            }
            // An empty block follows the flat path table, and is not encrypted.
            self.ndblock += 1;
            self.empty_block = self.ndblock;
            self.ndblock += 1;

            let mut ib_start = self.ndblock;
            let spb = bs / 36;
            let indirect_blocks = |size: u64| {
                let mut blocks = size.div_ceil(bs);
                let mut ib = 0;
                if blocks > 12 {
                    blocks -= 12;
                    ib += 1;
                }
                if blocks > spb {
                    blocks -= spb;
                    ib += 1 + blocks.div_ceil(spb);
                }
                ib
            };
            for i in 0..self.all_nodes.len() {
                let n = self.all_nodes[i];
                let size = self.node_size(n);
                if size.div_ceil(bs) > 12 + spb + spb * spb {
                    return format_err(
                        "the game is too large for a PS4 package: its image would need more \
                         than about 200 GiB",
                    );
                }
                self.ndblock += indirect_blocks(size);
            }

            for i in 0..self.all_nodes.len() {
                let n = self.all_nodes[i];
                let size = self.node_size(n);
                let blocks = size.div_ceil(bs);
                let ino_i = self.ino_of(n);
                let number = self.inodes[ino_i].number;
                if !n.dir {
                    self.file_start[n.index] = self.ndblock;
                    self.file_first_sig[n.index] = self.data_sigs.len();
                }
                {
                    let ino = &mut self.inodes[ino_i];
                    ino.db[0] = self.ndblock as i32;
                    ino.blocks = blocks as u32;
                    ino.size = if n.dir { blocks * bs } else { size };
                    if ino.csize == 0 {
                        ino.csize = ino.size;
                    }
                }
                for i in 0..blocks.min(12) {
                    let offset = self.ino_offset(number, i);
                    self.data_sigs.push(Sig {
                        block: self.ndblock,
                        offset,
                        size: bs,
                    });
                    self.ndblock += 1;
                }
                if blocks > 12 {
                    self.final_sigs.push(Sig {
                        block: ib_start,
                        offset: self.ino_offset(number, 12),
                        size: bs,
                    });
                    let mut pointer = 0;
                    for _ in 12..blocks.min(12 + spb) {
                        self.data_sigs.push(Sig {
                            block: self.ndblock,
                            offset: ib_start * bs + pointer,
                            size: bs,
                        });
                        self.ndblock += 1;
                        pointer += 36;
                    }
                    ib_start += 1;
                }
                if blocks > 12 + spb {
                    let mut done = 12 + spb;
                    self.final_sigs.push(Sig {
                        block: ib_start,
                        offset: self.ino_offset(number, 13),
                        size: bs,
                    });
                    let ib1 = ib_start;
                    let mut i = 0;
                    while i < spb && done < blocks {
                        ib_start += 1;
                        self.final_sigs.push(Sig {
                            block: ib_start,
                            offset: ib1 * bs + i * 36,
                            size: bs,
                        });
                        let mut j = 0;
                        while j < spb && done < blocks {
                            self.data_sigs.push(Sig {
                                block: self.ndblock,
                                offset: ib_start * bs + j * 36,
                                size: bs,
                            });
                            self.ndblock += 1;
                            j += 1;
                            done += 1;
                        }
                        i += 1;
                    }
                }
            }
        } else {
            self.ndblock = 1;
            self.dinode_count = self.inodes.len() as u64;
            self.dinode_block_count = self.dinode_count.div_ceil(bs / D32_SIZE);
            self.ibs_blocks = self.dinode_block_count as u32;
            self.ibs_size = self.dinode_block_count * bs;
            self.ibs_db[0] = self.ndblock as i64;
            self.ndblock += 1;
            for i in 1..self.dinode_block_count {
                if i < 12 {
                    self.ibs_db[i as usize] = -1;
                }
                self.ndblock += 1;
            }
            self.inodes[SUPER_ROOT].db[0] = self.ndblock as i32;
            self.ndblock += u64::from(self.inodes[SUPER_ROOT].blocks);

            let fp_blocks = self.fpt_size.div_ceil(bs) as u32;
            {
                let fp = &mut self.inodes[FPT];
                fp.db[0] = self.ndblock as i32;
                fp.size = self.fpt_size;
                fp.csize = self.fpt_size;
                fp.blocks = fp_blocks;
            }
            self.ndblock += 1;
            for i in 1..fp_blocks.min(12) {
                self.inodes[FPT].db[i as usize] = self.ndblock as i32;
                self.ndblock += 1;
            }
            match self.cr {
                None => self.ndblock += 1,
                Some(cr) => {
                    let cr_blocks = self.cr_size.div_ceil(bs) as u32;
                    let c = &mut self.inodes[cr];
                    c.db[0] = self.ndblock as i32;
                    c.size = self.cr_size;
                    c.csize = self.cr_size;
                    c.blocks = cr_blocks;
                    self.ndblock += 1;
                    for i in 1..cr_blocks.min(12) {
                        self.inodes[cr].db[i as usize] = self.ndblock as i32;
                        self.ndblock += 1;
                    }
                }
            }
            for i in 0..self.all_nodes.len() {
                let n = self.all_nodes[i];
                let size = self.node_size(n);
                let blocks = size.div_ceil(bs);
                let ino_i = self.ino_of(n);
                if !n.dir {
                    self.file_start[n.index] = self.ndblock;
                }
                let ino = &mut self.inodes[ino_i];
                ino.db[0] = self.ndblock as i32;
                ino.blocks = blocks as u32;
                ino.size = if n.dir { blocks * bs } else { size };
                if ino.csize == 0 {
                    ino.csize = ino.size;
                }
                for k in 1..blocks.min(12) {
                    ino.db[k as usize] = -1;
                }
                self.ndblock += blocks;
            }
        }
        self.ndblock = self.ndblock.max(self.p.min_blocks);
        Ok(())
    }

    fn size(&self) -> u64 {
        self.ndblock * BS
    }

    /// The metadata blocks (header, inodes, folders, path tables) by block number.
    fn metadata(&self) -> BTreeMap<u64, Vec<u8>> {
        let bs = BS;
        let mut blocks: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
        fn put(blocks: &mut BTreeMap<u64, Vec<u8>>, mut offset: u64, mut data: &[u8]) {
            while !data.is_empty() {
                let (b, within) = (offset / BS, (offset % BS) as usize);
                let block = blocks.entry(b).or_insert_with(|| vec![0u8; BS as usize]);
                let take = data.len().min(BS as usize - within);
                block[within..within + take].copy_from_slice(&data[..take]);
                data = &data[take..];
                offset += take as u64;
            }
        }
        fn touch(blocks: &mut BTreeMap<u64, Vec<u8>>, b: u64) {
            blocks.entry(b).or_insert_with(|| vec![0u8; BS as usize]);
        }

        // The superblock.
        let mut h = vec![0u8; 0x5A0];
        put64(&mut h, 0, 1);
        put64(&mut h, 8, 20_130_315);
        h[0x1A] = 1;
        let mode = (if self.p.sign { MODE_SIGNED } else { 0 })
            | (if self.p.encrypt { MODE_ENCRYPTED } else { 0 })
            | MODE_ALWAYS;
        put16(&mut h, 0x1C, mode);
        put32(&mut h, 0x20, bs as u32);
        put64(&mut h, 0x28, 1);
        put64(&mut h, 0x30, self.dinode_count);
        put64(&mut h, 0x38, self.ndblock);
        put64(&mut h, 0x40, self.dinode_block_count);
        // The inode blocks' own (64-bit, signed) inode.
        put16(&mut h, 0x50, 0);
        put16(&mut h, 0x52, 1);
        put32(&mut h, 0x54, self.ibs_flags);
        put64(&mut h, 0x58, self.ibs_size);
        put64(&mut h, 0x60, self.ibs_size);
        for t in 0..4 {
            put64(&mut h, 0x68 + 8 * t, self.p.time as u64);
        }
        put32(&mut h, 0xB0, self.ibs_blocks);
        for i in 0..12 {
            put64(&mut h, 0xB8 + 40 * i + 32, self.ibs_db[i] as u64);
        }
        if self.p.sign || self.p.encrypt {
            put32(&mut h, 0x36C, 1);
            h[0x370..0x380].copy_from_slice(&self.p.seed);
        } else {
            put32(&mut h, 0x368, 1);
        }
        put(&mut blocks, 0, &h);

        // The inodes, from block 1; an inode never straddles two blocks.
        let size = if self.p.sign { S32_SIZE } else { D32_SIZE };
        let mut at = bs;
        for i in &self.inodes {
            let mut e = vec![0u8; size as usize];
            put16(&mut e, 0, i.mode);
            put16(&mut e, 2, i.nlink);
            put32(&mut e, 4, i.flags);
            put64(&mut e, 8, i.size);
            put64(&mut e, 0x10, i.csize);
            for t in 0..4 {
                put64(&mut e, 0x18 + 8 * t, self.p.time as u64);
            }
            put32(&mut e, 0x60, i.blocks);
            for k in 0..12 {
                let slot = if self.p.sign {
                    0x64 + 36 * k + 32
                } else {
                    0x64 + 4 * k
                };
                put32(&mut e, slot, i.db[k] as u32);
            }
            put(&mut blocks, at, &e);
            at += size;
            if at % bs > bs - size {
                at += bs - at % bs;
            }
        }
        for b in 0..self.dinode_block_count {
            touch(&mut blocks, 1 + b);
        }

        // The super root's entries.
        let mut d = Vec::new();
        for e in &self.super_root_dirents {
            e.write(&mut d);
        }
        put(&mut blocks, bs * (self.dinode_block_count + 1), &d);
        touch(&mut blocks, self.dinode_block_count + 1);

        // The flat path table, the collision resolver, then every folder.
        let fp = &self.inodes[FPT];
        let mut t = Vec::new();
        for (h, v) in &self.fpt_map {
            t.extend_from_slice(&h.to_le_bytes());
            t.extend_from_slice(&v.to_le_bytes());
        }
        put(&mut blocks, fp.db[0] as u64 * bs, &t);
        touch(&mut blocks, fp.db[0] as u64);
        if let Some(cr) = self.cr {
            let c = &self.inodes[cr];
            let mut r = Vec::new();
            for list in &self.collisions {
                for e in list {
                    e.write(&mut r);
                }
                r.resize(r.len() + 0x18, 0);
            }
            put(&mut blocks, c.db[0] as u64 * bs, &r);
        }
        for n in &self.all_nodes {
            if !n.dir {
                continue;
            }
            let ino = &self.inodes[self.ino_of(*n)];
            let mut block = ino.db[0] as u64;
            let mut at = block * bs;
            touch(&mut blocks, block);
            for e in &self.dirents[n.index] {
                let mut d = Vec::new();
                e.write(&mut d);
                put(&mut blocks, at, &d);
                at += d.len() as u64;
                if at % bs > bs - DIRENT_MAX {
                    block += 1;
                    at = block * bs;
                }
            }
        }
        blocks
    }

    /// Each file's first block and size, sorted, for finding the file a block belongs to.
    fn extents(&self) -> Vec<(u64, usize)> {
        let mut e: Vec<(u64, usize)> = (0..self.tree.files.len())
            .filter(|f| self.tree.files[*f].size > 0)
            .map(|f| (self.file_start[f], f))
            .collect();
        e.sort();
        e
    }
}

/// `PFSCWriter`: a version-6 header whose block table points at the payload stored as it is;
/// the header is a multiple of 64 KiB.
fn pfsc_header(payload: u64) -> Vec<u8> {
    let blocks = payload.div_ceil(BS);
    let table = 8 + blocks as i64 * 8;
    let extra = (table - 0xFC00 + 0xFFFF) / 0x10000; // C# division, toward zero
    let header = 0x10000 + if extra > 0 { extra as u64 * BS } else { 0 };
    let mut out = vec![0u8; header as usize];
    out[..4].copy_from_slice(b"PFSC");
    put32(&mut out, 8, 6);
    put32(&mut out, 0x0C, BS as u32);
    put64(&mut out, 0x10, BS);
    put64(&mut out, 0x18, 0x400);
    put64(&mut out, 0x20, header);
    put64(&mut out, 0x28, blocks * BS);
    for i in 0..=blocks {
        put64(&mut out, (0x400 + i * 8) as usize, header + i * BS);
    }
    out
}

// ── the game's files, read once ──────────────────────────────────────────────────────────────

/// One file the package holds and what it should read back as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    /// Its path as [`crate::ps4_pkg::Ps4Package`] lists it.
    pub path: String,
    pub size: u64,
    pub sha256: [u8; 32],
}

/// Hashes the inner image's files as their bytes go by, in order.
struct Hashes {
    done: HashMap<usize, [u8; 32]>,
    open: Option<(usize, u64, Sha256)>,
}

impl Hashes {
    fn feed(&mut self, f: usize, pos: u64, bytes: &[u8], size: u64) {
        if pos == 0 {
            self.open = Some((f, 0, Sha256::new()));
        }
        match &mut self.open {
            Some((open, next, h)) if *open == f && *next == pos => {
                h.update(bytes);
                *next += bytes.len() as u64;
                if *next >= size {
                    if let Some((_, _, h)) = self.open.take() {
                        self.done.insert(f, h.finalize().into());
                    }
                }
            }
            // Out of order: never happens, as the image is written in order; caught at the end.
            _ => self.open = None,
        }
    }
}

/// The inner image, unsigned and plain, as bytes: metadata from memory, files from the source.
struct InnerImage<'a> {
    pfs: &'a Pfs,
    meta: BTreeMap<u64, Vec<u8>>,
    extents: Vec<(u64, usize)>,
    tree: &'a mut dyn SourceTree,
    hashes: Hashes,
}

impl InnerImage<'_> {
    /// `out.len() / BS` blocks from `first`.
    fn fill(&mut self, first: u64, out: &mut [u8]) -> Result<()> {
        let count = out.len() as u64 / BS;
        let mut k = 0;
        while k < count {
            let b = first + k;
            let at = (k * BS) as usize;
            if let Some(m) = self.meta.get(&b) {
                out[at..at + BS as usize].copy_from_slice(m);
                k += 1;
                continue;
            }
            let found = self.extents.partition_point(|(start, _)| *start <= b);
            let Some(&(start, f)) = found.checked_sub(1).map(|i| &self.extents[i]) else {
                out[at..at + BS as usize].fill(0);
                k += 1;
                continue;
            };
            let file = &self.pfs.tree.files[f];
            let pos = (b - start) * BS;
            if pos >= file.size {
                out[at..at + BS as usize].fill(0);
                k += 1;
                continue;
            }
            let run = (count - k).min((file.size - pos).div_ceil(BS));
            let want = (run * BS).min(file.size - pos) as usize;
            let span = &mut out[at..at + (run * BS) as usize];
            match &file.payload {
                Payload::Source(path) => {
                    let mut got = 0;
                    while got < want {
                        let bytes = self.tree.read_range(path, pos + got as u64, want - got)?;
                        if bytes.is_empty() {
                            return format_err(format!(
                                "{path} ended before its {} bytes while it was being packaged",
                                file.size
                            ));
                        }
                        span[got..got + bytes.len()].copy_from_slice(&bytes);
                        got += bytes.len();
                    }
                }
                Payload::Memory(bytes) => {
                    span[..want].copy_from_slice(&bytes[pos as usize..pos as usize + want]);
                }
                Payload::Inner => unreachable!("an inner image holds no image"),
            }
            span[want..].fill(0);
            self.hashes.feed(f, pos, &span[..want], file.size);
            k += run;
        }
        Ok(())
    }
}

// ── writing ──────────────────────────────────────────────────────────────────────────────────

/// What [`write`] reports as it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The game's files into the image: bytes of `pfs_image.dat` done, of its total.
    Write,
    /// The finished image hashed for the container's digests.
    Hash,
}

/// How a package is made.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Every time stamp in the image, Unix seconds; and, when `param.sfo` has no creation date,
    /// that date. Default: `param.sfo`'s creation date and time, else now.
    pub time: Option<i64>,
}

/// What [`write`] made.
#[derive(Debug, Clone)]
pub struct Written {
    pub content_id: String,
    pub title: Option<String>,
    pub title_id: Option<String>,
    pub size: u64,
    /// Every file the package holds, as it should read back.
    pub files: Vec<Expected>,
    /// Folders with nothing in them.
    pub empty_dirs: Vec<String>,
    pub made_licence: bool,
    pub made_keystone: bool,
}

/// Why this tree can't become a PS4 package, in words; `None` when it can.
pub fn refusal(tree: &mut dyn SourceTree) -> Option<String> {
    match game_sfo(tree) {
        Ok(_) => None,
        Err(e) => Some(e.to_string()),
    }
}

/// The game's `param.sfo`: its listed path and bytes, after checking it is a PS4 game's.
fn game_sfo(tree: &mut dyn SourceTree) -> Result<(String, Sfo)> {
    let Some(path) = tree
        .files()
        .iter()
        .find(|f| f.path.eq_ignore_ascii_case("sce_sys/param.sfo"))
        .map(|f| f.path.clone())
    else {
        return format_err(
            "this isn't a PS4 game: it has no sce_sys/param.sfo, which every PS4 game has (a PS5 \
             game has sce_sys/param.json instead)",
        );
    };
    let bytes = crate::source::read_small(tree, &path, crate::source::MAX_PARAM_BYTES)?;
    let sfo = Sfo::read(&bytes)?;
    let category = sfo.display("CATEGORY");
    match category.as_str() {
        "gd" => {}
        "gp" => {
            return format_err(
                "this is a PS4 update (CATEGORY gp), and only a game can be made into a PS4 \
                 package here: choose the game too, so the update is merged into it, and package \
                 that",
            )
        }
        "ac" => {
            return format_err(
                "this is a PS4 add-on (CATEGORY ac), and only a game can be made into a PS4 \
                 package here",
            )
        }
        other => {
            return format_err(format!(
                "this isn't a PS4 game: its param.sfo says CATEGORY {other:?}, where a game says \
                 \"gd\""
            ))
        }
    }
    let content_id = sfo.display("CONTENT_ID");
    if content_id.len() != 36 || !content_id.is_ascii() {
        return format_err(
            "the game's param.sfo has no usable CONTENT_ID (36 characters, like \
             UP0000-CUSA00000_00-0000000000000000), which a package is named and keyed by",
        );
    }
    Ok((path, sfo))
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (i64::from(yoe) + era * 400 + i64::from(m <= 2), m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400) as u32;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + i64::from(doe) - 719_468
}

fn digits(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_digit())
}

/// One node of the game's folder tree.
#[derive(Default)]
struct Node {
    dirs: BTreeMap<String, Node>,
    /// Name -> path in the source.
    files: BTreeMap<String, (String, u64)>,
}

/// One container entry: its ID, name (none for most made ones) and bytes.
struct Entry {
    id: u32,
    name: Option<String>,
    data: Vec<u8>,
    flags1: u32,
    flags2: u32,
    name_offset: u32,
    offset: u32,
    size: u32,
}

impl Entry {
    fn new(id: u32, name: Option<&str>, data: Vec<u8>) -> Self {
        Self {
            id,
            name: name.map(str::to_string),
            data,
            flags1: 0,
            flags2: 0,
            name_offset: 0,
            offset: 0,
            size: 0,
        }
    }
    fn row(&self) -> [u8; 32] {
        let mut r = [0u8; 32];
        put_be32(&mut r, 0, self.id);
        put_be32(&mut r, 4, self.name_offset);
        put_be32(&mut r, 8, self.flags1);
        put_be32(&mut r, 12, self.flags2);
        put_be32(&mut r, 16, self.offset);
        put_be32(&mut r, 20, self.size);
        r
    }
}

/// The entry-keys entry (`KeysEntry`): the content ID's digest, each passcode key's digest
/// xored with itself, and each one's RSA wrap (key 0 wraps the passcode).
fn entry_keys(content_id: &str, passcode: &str) -> Vec<u8> {
    let mut cid = [0u8; 48];
    cid[..content_id.len()].copy_from_slice(content_id.as_bytes());
    let mut out = sha256(&cid).to_vec();
    let mut wraps = Vec::new();
    for i in 0..7u32 {
        let key = package_key(content_id, passcode, i);
        let mut d = sha256(&key);
        for (x, k) in d.iter_mut().zip(key) {
            *x ^= k;
        }
        out.extend_from_slice(&d);
        wraps.push(if i == 0 {
            mt_wrap(public_key(0), passcode.as_bytes())
        } else {
            mt_wrap(public_key(i as usize), &key)
        });
    }
    for w in wraps {
        out.extend(w);
    }
    out
}

/// `playgo-chunk.dat` for one chunk and one scenario (`ChunkDat.FromProject`).
fn chunk_dat(content_id: &str, package_size: u64, inner_size: u64) -> Vec<u8> {
    let mut d = vec![0u8; 416];
    put32(&mut d, 0, 0x6f67_6c70);
    for at in [8, 0xA, 0xC, 0xE] {
        put16(&mut d, at, 1);
    }
    put32(&mut d, 0x10, 416);
    put16(&mut d, 0x16, 1);
    d[0x20..0x40].fill(0xFF);
    d[0x40..0x40 + content_id.len()].copy_from_slice(content_id.as_bytes());
    let table: [u32; 16] = [
        256, 32, 288, 2, 304, 9, 320, 16, 352, 32, 384, 2, 400, 12, 336, 16,
    ];
    for (i, v) in table.iter().enumerate() {
        put32(&mut d, 0xC0 + 4 * i, *v);
    }
    d[0x100] = 0x80;
    d[0x102] = 3;
    put16(&mut d, 0x10E, 1);
    put64(&mut d, 0x110, u64::MAX);
    d[0x130..0x138].copy_from_slice(b"Chunk #0");
    put64(&mut d, 0x148, package_size);
    put64(&mut d, 0x158, inner_size);
    d[0x160] = 1;
    put16(&mut d, 0x174, 1);
    put16(&mut d, 0x176, 1);
    d[0x190..0x19B].copy_from_slice(b"Scenario #0");
    d
}

const MANIFEST: &str = "\u{FEFF}<?xml version=\"1.0\" encoding=\"utf-8\" standalone=\"yes\"?>\r\n\
<psproject fmt=\"playgo-manifest\" version=\"0990\">\r\n\
\x20 <volume>\r\n\
\x20   <chunk_info chunk_count=\"1\" scenario_count=\"1\">\r\n\
\x20     <scenarios default_id=\"0\">\r\n\
\x20       <scenario id=\"0\" type=\"sp\" initial_chunk_count=\"1\" label=\"Scenario #0\">0</scenario>\r\n\
\x20     </scenarios>\r\n\
\x20   </chunk_info>\r\n\
\x20 </volume>\r\n\
</psproject>\r\n";

/// Fields of the container header that `PkgWriter.WriteHeader` writes (big-endian).
#[derive(Default)]
struct Header {
    entry_count: u32,
    main_ent_data_size: u32,
    body_size: u64,
    content_id: String,
    promote_size: u32,
    sc_entries1: [u8; 32],
    sc_entries2: [u8; 32],
    digest_table: [u8; 32],
    body_digest: [u8; 32],
    pfs_offset: u64,
    pfs_size: u64,
    package_size: u64,
    pfs_image_digest: [u8; 32],
    pfs_signed_digest: [u8; 32],
}

impl Header {
    fn write(&self, h: &mut [u8]) {
        h[..4].copy_from_slice(b"\x7FCNT");
        put_be32(h, 0x04, 1);
        put_be32(h, 0x0C, 0xF);
        put_be32(h, 0x10, self.entry_count);
        put_be16(h, 0x14, 6);
        put_be16(h, 0x16, self.entry_count as u16);
        put_be32(h, 0x18, 0x2A80);
        put_be32(h, 0x1C, self.main_ent_data_size);
        put_be64(h, 0x20, BODY_OFFSET);
        put_be64(h, 0x28, self.body_size);
        h[0x40..0x40 + self.content_id.len()].copy_from_slice(self.content_id.as_bytes());
        put_be32(h, 0x70, DRM_PS4);
        put_be32(h, 0x74, CONTENT_TYPE_GD);
        put_be32(h, 0x78, 0x0A00_0000); // Unk_x8000000 | GD_AC
        put_be32(h, 0x7C, self.promote_size);
        put_be32(h, 0x80, 0x2016_1020);
        put_be32(h, 0x84, 0x0173_8551);
        put_be32(h, 0x9C, 1);
        h[0x100..0x120].copy_from_slice(&self.sc_entries1);
        h[0x120..0x140].copy_from_slice(&self.sc_entries2);
        h[0x140..0x160].copy_from_slice(&self.digest_table);
        h[0x160..0x180].copy_from_slice(&self.body_digest);
        put_be32(h, 0x400, 1);
        put_be32(h, 0x404, 1);
        put_be64(h, 0x408, 0x8000_0000_0000_03CC);
        put_be64(h, 0x410, self.pfs_offset);
        put_be64(h, 0x418, self.pfs_size);
        put_be64(h, 0x428, self.package_size);
        put_be64(h, 0x430, self.package_size);
        put_be32(h, 0x438, 0x10000);
        put_be32(h, 0x43C, 0xD0000);
        h[0x440..0x460].copy_from_slice(&self.pfs_image_digest);
        h[0x460..0x480].copy_from_slice(&self.pfs_signed_digest);
    }

    /// What LibOrbisPkg folds into its header digest: `[0, 64)` and `[0x400, 0x480)`.
    fn digest(&self) -> [u8; 32] {
        let mut h = vec![0u8; 0x1000];
        self.write(&mut h);
        let mut part = h[..64].to_vec();
        part.extend_from_slice(&h[0x400..0x480]);
        sha256(&part)
    }
}

fn write_at(out: &mut File, at: u64, data: &[u8]) -> Result<()> {
    out.seek(SeekFrom::Start(at))?;
    out.write_all(data)?;
    Ok(())
}

fn read_at(out: &mut File, at: u64, buf: &mut [u8]) -> Result<()> {
    out.seek(SeekFrom::Start(at))?;
    out.read_exact(buf)?;
    Ok(())
}

/// Runs `job` over each 64 KiB block of `buf` on every core.
fn each_block(buf: &mut [u8], job: &(dyn Fn(usize, &mut [u8]) + Sync)) {
    let blocks = buf.len() / BS as usize;
    let workers = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(blocks)
        .max(1);
    let per = blocks.div_ceil(workers);
    let lane = workshop::Lane::current();
    std::thread::scope(|s| {
        for (w, part) in buf.chunks_mut(per * BS as usize).enumerate() {
            s.spawn(move || {
                lane.enter();
                for (k, block) in part.chunks_mut(BS as usize).enumerate() {
                    job(w * per + k, block);
                }
            });
        }
    });
}

/// Writes the PS4 package of the game in `tree` to `out` (empty, readable and writable); see
/// the module docs. `progress` hears each step's bytes done and total.
pub fn write(
    tree: &mut dyn SourceTree,
    out: &mut File,
    options: &Options,
    progress: &mut dyn FnMut(Step, u64, u64),
    cancel: &AtomicBool,
) -> Result<Written> {
    let passcode = DEFAULT_PASSCODE;
    let (sfo_path, mut sfo) = game_sfo(tree)?;
    let content_id = sfo.display("CONTENT_ID");
    let text = |s: &Sfo, k: &str| Some(s.display(k).trim().to_string()).filter(|t| !t.is_empty());
    let title = text(&sfo, "TITLE");
    let title_id = text(&sfo, "TITLE_ID");

    // GP4/Gp4Creator.cs keeps c_date and c_time from PUBTOOLINFO.
    let info = sfo.display("PUBTOOLINFO");
    let item = |key: &str| {
        info.split(',')
            .find_map(|i| i.strip_prefix(key))
            .unwrap_or_default()
            .to_string()
    };
    let (mut c_date, mut c_time) = (item("c_date="), item("c_time="));
    if !digits(&c_date, 8) {
        c_date.clear();
        c_time.clear();
    }
    if !c_time.is_empty() && !digits(&c_time, 6) {
        c_time.clear();
    }
    let sfo_dated = !c_date.is_empty();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    if !sfo_dated {
        let (y, m, d) = civil_from_days(options.time.unwrap_or(now).div_euclid(86_400));
        c_date = format!("{y:04}{m:02}{d:02}");
    }
    let num = |s: &str, at: usize, n: usize| -> i64 { s[at..at + n].parse().unwrap_or(0) };
    let dated = sfo_dated.then(|| {
        let mut t = days_from_civil(
            num(&c_date, 0, 4),
            num(&c_date, 4, 2) as u32,
            num(&c_date, 6, 2) as u32,
        ) * 86_400;
        if !c_time.is_empty() {
            t += num(&c_time, 0, 2) * 3600 + num(&c_time, 2, 2) * 60 + num(&c_time, 4, 2);
        }
        t
    });
    let time = options.time.or(dated).unwrap_or(now);

    // The game's folders, the top sce_sys spelt as the console spells it.
    let mut root = Node::default();
    let listed: Vec<(String, u64)> = tree
        .files()
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    let canonical = |path: &str| -> Vec<String> {
        let mut parts: Vec<String> = path
            .split('/')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect();
        if parts.len() > 1 && parts[0].eq_ignore_ascii_case("sce_sys") {
            parts[0] = "sce_sys".into();
        }
        parts
    };
    for (path, size) in &listed {
        let parts = canonical(path);
        let Some((name, dirs)) = parts.split_last() else {
            continue;
        };
        let mut node = &mut root;
        for d in dirs {
            node = node.dirs.entry(d.clone()).or_default();
        }
        if path == &sfo_path {
            node.files.insert("param.sfo".into(), (path.clone(), *size));
        } else {
            node.files.insert(name.clone(), (path.clone(), *size));
        }
    }
    let empty: Vec<String> = tree.empty_dirs().to_vec();
    for d in &empty {
        let mut node = &mut root;
        for p in canonical(d) {
            node = node.dirs.entry(p).or_default();
        }
    }

    // The project's order (pkg_makegp4's): sce_sys first at the top; in each folder,
    // sub-folders holding entry files first (by their lowest entry ID), then the others by
    // name; entry files first (by ID), then the others by name.
    fn lowest(node: &Node, rel: &str) -> u64 {
        let mut best = u64::MAX;
        for (name, child) in &node.dirs {
            best = best.min(lowest(child, &format!("{rel}{name}/")));
        }
        for name in node.files.keys() {
            if let Some(id) = entry_of(Some(&format!("{rel}{name}"))) {
                best = best.min(u64::from(id));
            }
        }
        best
    }
    struct Walk {
        fs: FsTree,
        sce_sys: Option<usize>,
        licence: Option<(String, String)>,
        licence_info: Option<(String, String)>,
        /// sce_sys files that become entries: (ID, name, source path).
        entries: Vec<(u32, String, String)>,
        empty_dirs: Vec<String>,
    }
    fn add(w: &mut Walk, node: &Node, dir: usize, rel: Option<&str>, path: &str) {
        let top = dir == 0;
        let mut dirs: Vec<(&String, &Node)> = node.dirs.iter().collect();
        dirs.sort_by(|a, b| {
            let (sa, sb) = (top && a.0 == "sce_sys", top && b.0 == "sce_sys");
            sb.cmp(&sa)
                .then_with(|| {
                    let l = |n: &(&String, &Node)| match rel {
                        Some(r) => lowest(n.1, &format!("{r}{}/", n.0)),
                        None => u64::MAX,
                    };
                    l(a).cmp(&l(b))
                })
                .then_with(|| a.0.as_bytes().cmp(b.0.as_bytes()))
        });
        let mut files: Vec<(&String, &(String, u64))> = node.files.iter().collect();
        let id_of = |name: &str| -> u64 {
            rel.and_then(|r| entry_of(Some(&format!("{r}{name}"))))
                .map_or(u64::MAX, u64::from)
        };
        files.sort_by(|a, b| {
            id_of(a.0)
                .cmp(&id_of(b.0))
                .then_with(|| a.0.as_bytes().cmp(b.0.as_bytes()))
        });
        for (name, child) in dirs {
            let index = w.fs.add_dir(dir, name);
            let is_sce_sys = top && name == "sce_sys";
            if is_sce_sys {
                w.sce_sys = Some(index);
            }
            let child_rel = if is_sce_sys {
                Some(String::new())
            } else {
                rel.map(|r| format!("{r}{name}/"))
            };
            let child_path = format!("{path}{name}/");
            if child.dirs.is_empty() && child.files.is_empty() {
                w.empty_dirs
                    .push(child_path.trim_end_matches('/').to_string());
            }
            add(w, child, index, child_rel.as_deref(), &child_path);
        }
        for (name, (src, size)) in files {
            if rel == Some("") {
                if name == "license.dat" {
                    w.licence = Some((src.clone(), format!("{path}{name}")));
                    continue;
                }
                if name == "license.info" {
                    w.licence_info = Some((src.clone(), format!("{path}{name}")));
                    continue;
                }
                if generated(name) {
                    continue;
                }
                if let Some(id) = entry_id_for_name(name) {
                    w.entries.push((id, name.clone(), src.clone()));
                    continue;
                }
            }
            w.fs.add_file(FsFile {
                name: name.clone(),
                parent: dir,
                size: *size,
                csize: *size,
                compress: false,
                payload: Payload::Source(src.clone()),
            });
        }
    }
    let mut w = Walk {
        fs: FsTree::new(),
        sce_sys: None,
        licence: None,
        licence_info: None,
        entries: Vec::new(),
        empty_dirs: Vec::new(),
    };
    add(&mut w, &root, 0, None, "");
    let Some(sce_sys) = w.sce_sys else {
        return format_err("the game has no sce_sys folder");
    };
    let mut files: Vec<Expected> = Vec::new();
    let mut fs = w.fs;
    // A game needs a keystone; the one for the passcode when it has none.
    let made_keystone = !fs.dirs[sce_sys]
        .files
        .iter()
        .any(|f| fs.files[*f].name == "keystone");
    if made_keystone {
        let keystone = keys::PS4_KEYSTONE.to_vec();
        fs.add_file(FsFile {
            name: "keystone".into(),
            parent: sce_sys,
            size: keystone.len() as u64,
            csize: keystone.len() as u64,
            compress: false,
            payload: Payload::Memory(keystone),
        });
    }
    // The game's licence when it has a whole one, else a debug one.
    let mut read_small = |src: &str, max: u64| -> Result<Vec<u8>> {
        let size = listed.iter().find(|(p, _)| p == src).map_or(0, |(_, s)| *s);
        if size > max {
            return format_err(format!(
                "{src} is too large for a package's sce_sys ({size} bytes)"
            ));
        }
        let bytes = tree.read(src)?;
        if bytes.len() as u64 != size {
            return format_err(format!("{src} changed while it was being packaged"));
        }
        Ok(bytes)
    };
    let kept = match (&w.licence, &w.licence_info) {
        (Some((dat, _)), Some((info, _))) => {
            let (d, i) = (read_small(dat, 0x10000)?, read_small(info, 0x10000)?);
            (d.len() == 0x400 && i.len() == 0x200).then_some((d, i))
        }
        _ => None,
    };
    let made_licence = kept.is_none();
    let (licence, licence_info) = kept.unwrap_or_else(|| {
        (
            license::license_dat_for(&content_id, DRM_PS4 as u16, CONTENT_TYPE_GD as u16).to_vec(),
            license::license_info_for(&content_id, CONTENT_TYPE_GD as u16).to_vec(),
        )
    });
    let mut file_entries = Vec::new();
    for (id, name, src) in &w.entries {
        if name == "param.sfo" {
            continue;
        }
        file_entries.push((*id, name.clone(), read_small(src, 256 << 20)?));
    }

    // The inner PFS, and the signed, encrypted outer one holding it.
    let ekpfs = package_key(&content_id, passcode, 1);
    let inner = Pfs::plan(
        fs,
        PfsProps {
            sign: false,
            encrypt: false,
            min_blocks: 0x55,
            time,
            ekpfs: [0; 32],
            seed: [0; 16],
        },
    )?;
    let inner_size = inner.size();
    let pfsc = pfsc_header(inner_size);
    let pfsc_size = pfsc.len() as u64 + inner_size;
    let mut outer_tree = FsTree::new();
    outer_tree.add_file(FsFile {
        name: "pfs_image.dat".into(),
        parent: 0,
        size: pfsc_size,
        csize: inner_size,
        compress: true,
        payload: Payload::Inner,
    });
    let outer = Pfs::plan(
        outer_tree,
        PfsProps {
            sign: true,
            encrypt: true,
            min_blocks: 0,
            time,
            ekpfs,
            seed: [0; 16],
        },
    )?;
    let pfs_size = outer.size();

    // param.sfo as the package stores it.
    let first_package_size = 0x80000 + pfs_size; // what BuildPkg has at this point
    let tool_info = format!(
        "c_date={c_date}{},img0_l0_size={},img0_l1_size=0,img0_sc_ksize=512,img0_pc_ksize=832",
        if c_time.is_empty() {
            String::new()
        } else {
            format!(",c_time={c_time}")
        },
        first_package_size.div_ceil(1 << 20)
    );
    sfo.set(SfoEntry {
        key: "PUBTOOLINFO".into(),
        format: SFO_UTF8,
        text: tool_info.into_bytes(),
        number: 0,
        max_length: 0x200,
    });
    sfo.set(SfoEntry {
        key: "PUBTOOLVER".into(),
        format: SFO_INTEGER,
        text: Vec::new(),
        number: 0x0289_0000,
        max_length: 4,
    });
    let sfo_out = sfo.write();

    // The entries, in body order.
    const K_KEYS: usize = 0;
    const K_IMAGE_KEY: usize = 1;
    const K_GENERAL: usize = 2;
    const K_METAS: usize = 3;
    const K_DIGESTS: usize = 4;
    const K_NAMES: usize = 5;
    const K_CHUNK_DAT: usize = 6;
    const K_CHUNK_SHA: usize = 7;
    let mut entries = vec![
        Entry::new(0x10, None, entry_keys(&content_id, passcode)),
        Entry::new(0x20, None, mt_wrap(&keys::PS4_FAKE_MODULUS, &ekpfs)),
        Entry::new(0x80, None, vec![0u8; 0x180]),
        Entry::new(0x100, None, Vec::new()),
        Entry::new(0x1, None, Vec::new()),
        Entry::new(0x200, None, Vec::new()),
        Entry::new(0x1001, Some("playgo-chunk.dat"), vec![0u8; 416]),
        Entry::new(0x1002, Some("playgo-chunk.sha"), Vec::new()),
        Entry::new(
            0x1003,
            Some("playgo-manifest.xml"),
            MANIFEST.as_bytes().to_vec(),
        ),
        Entry::new(0x400, None, licence),
        Entry::new(0x401, None, licence_info),
        Entry::new(0x1000, Some("param.sfo"), sfo_out.clone()),
        Entry::new(0x409, None, vec![0u8; 0x2000]),
    ];
    for (id, name, data) in file_entries {
        entries.push(Entry::new(id, Some(&name), data));
    }
    let count = entries.len();
    entries[K_DIGESTS].data = vec![0u8; count * 32];

    // Names: "" first, then each named entry in .NET's order.
    let mut names = vec![0u8];
    {
        let mut order: Vec<usize> = (0..count).collect();
        order.sort_by(|a, b| match (&entries[*a].name, &entries[*b].name) {
            (Some(x), Some(y)) => dotnet_cmp(x, y),
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        });
        let mut table: Vec<(String, u32)> = Vec::new();
        for i in order {
            let Some(name) = entries[i].name.clone().filter(|n| !n.is_empty()) else {
                continue;
            };
            if let Some((_, at)) = table.iter().find(|(n, _)| *n == name) {
                entries[i].name_offset = *at;
                continue;
            }
            entries[i].name_offset = names.len() as u32;
            table.push((name.clone(), names.len() as u32));
            names.extend_from_slice(name.as_bytes());
            names.push(0);
        }
    }
    entries[K_NAMES].data = names;

    // Offsets and sizes (BuildPkg's second pass). LibOrbisPkg reserves the PlayGo chunk
    // digests' room from an estimate made before the entries are laid out and gives up when the
    // package outgrows it; there, and only there, the room needed is reserved (Alembic's fix).
    let mut header = Header {
        content_id: content_id.clone(),
        ..Header::default()
    };
    let mut chunk_room = 0u64;
    loop {
        let mut offset = BODY_OFFSET;
        for i in 0..count {
            let (f1, f2) = match entries[i].id {
                0x1 => (0x4000_0000, 0),
                0x10 => (0x6000_0000, 0),
                0x20 => (0xE000_0000, 3 << 12),
                0x80 => (0x6000_0000, 0),
                0x100 => (0x6000_0000, 0),
                0x200 => (0x4000_0000, 0),
                0x400 => (0x8000_0000, 3 << 12),
                0x401 => (0x8000_0000, 2 << 12),
                _ => (0, 0),
            };
            entries[i].flags1 = f1;
            entries[i].flags2 = f2;
            entries[i].offset = offset as u32;
            let size = if i == K_METAS {
                (count * 32) as u64
            } else if i == K_CHUNK_SHA {
                // Estimated from the entries so far (metas: one per entry up to this one).
                let sum: u64 = (0..count)
                    .map(|j| {
                        let length = if j == K_METAS {
                            (i as u64 + 1) * 32
                        } else if j == K_CHUNK_SHA {
                            0
                        } else {
                            entries[j].data.len() as u64
                        };
                        align(length, 16)
                    })
                    .sum();
                let mut estimate = align(BODY_OFFSET + sum, 0x80000) + pfs_size;
                estimate += (estimate + 16) / BS * 4;
                if chunk_room != 0 {
                    chunk_room
                } else {
                    estimate / BS * 4
                }
            } else {
                entries[i].data.len() as u64
            };
            entries[i].size = size as u32;
            offset = align(offset + size, 16);
        }
        let body = offset - BODY_OFFSET;
        header.entry_count = count as u32;
        header.body_size = align(BODY_OFFSET + body, 0x80000) - BODY_OFFSET;
        header.main_ent_data_size = (2048 + 256 + 0x180 + count * 32 + count * 32) as u32;
        header.pfs_offset = BODY_OFFSET + header.body_size;
        header.pfs_size = pfs_size;
        header.package_size = header.body_size + BODY_OFFSET + pfs_size;
        let chunk_sha = 4 * (header.package_size / BS);
        if chunk_sha > u64::from(entries[K_CHUNK_SHA].size) {
            if chunk_room >= chunk_sha {
                return format_err("the PlayGo chunk digests do not fit");
            }
            chunk_room = chunk_sha;
            continue;
        }
        entries[K_CHUNK_SHA].size = chunk_sha as u32;
        entries[K_CHUNK_SHA].data = vec![0u8; chunk_sha as usize];
        entries[K_CHUNK_DAT].data = chunk_dat(&content_id, header.package_size, inner_size);
        header.promote_size = (header.body_size + BODY_OFFSET) as u32;
        break;
    }

    // The outer PFS, straight into the package.
    out.set_len(header.package_size)?;
    let mut image = InnerImage {
        pfs: &inner,
        meta: inner.metadata(),
        extents: inner.extents(),
        tree,
        hashes: Hashes {
            done: HashMap::new(),
            open: None,
        },
    };
    write_outer(
        &outer,
        out,
        header.pfs_offset,
        &pfsc,
        &mut image,
        &mut |done| progress(Step::Write, done, pfsc_size),
        cancel,
    )?;
    let hashes = image.hashes.done;

    // Read back: the image's digests and the PlayGo chunk digests.
    {
        let mut whole = Sha256::new();
        let chunks = &mut entries[K_CHUNK_SHA].data;
        let mut buf = vec![0u8; BATCH * BS as usize];
        let mut done = 0u64;
        progress(Step::Hash, 0, pfs_size);
        while done < pfs_size {
            if workshop::checkpoint(cancel) {
                return Err(Error::Cancelled);
            }
            let n = (buf.len() as u64).min(pfs_size - done) as usize;
            read_at(out, header.pfs_offset + done, &mut buf[..n])?;
            if done == 0 {
                header.pfs_signed_digest = sha256(&buf[..BS as usize]);
            }
            let first = (header.pfs_offset + done) / BS;
            let blocks = n / BS as usize;
            let mut digests = vec![[0u8; 4]; blocks];
            {
                let slots: Vec<std::sync::Mutex<[u8; 4]>> =
                    (0..blocks).map(|_| std::sync::Mutex::new([0; 4])).collect();
                each_block(&mut buf[..n], &|k, block| {
                    let d = sha256(block);
                    if let Ok(mut s) = slots[k].lock() {
                        s.copy_from_slice(&d[..4]);
                    }
                });
                for (k, s) in slots.into_iter().enumerate() {
                    digests[k] = s.into_inner().unwrap_or_default();
                }
            }
            for (k, d) in digests.iter().enumerate() {
                let at = ((first + k as u64) * 4) as usize;
                chunks[at..at + 4].copy_from_slice(d);
            }
            whole.update(&buf[..n]);
            done += n as u64;
            progress(Step::Hash, done, pfs_size);
        }
        header.pfs_image_digest = whole.finalize().into();
    }

    // FinishPkg: general digests, body, body digests, header, its digest and signature.
    {
        let mut major = format!("ATTRIBUTE{}", sfo.display("ATTRIBUTE"));
        if sfo.find("ATTRIBUTE2").is_some() {
            major += &format!("ATTRIBUTE2{}", sfo.display("ATTRIBUTE2"));
        }
        major += &format!(
            "CATEGORY{}FORMAT{}PUBTOOLVER{}",
            sfo.display("CATEGORY"),
            sfo.display("FORMAT"),
            sfo.display("PUBTOOLVER")
        );
        let major_digest = sha256(major.as_bytes());
        let mut content = vec![0u8; 56];
        content[..36].copy_from_slice(content_id.as_bytes());
        put_be32(&mut content, 48, DRM_PS4);
        put_be32(&mut content, 52, CONTENT_TYPE_GD);
        content.extend_from_slice(&header.pfs_image_digest);
        content.extend_from_slice(&major_digest);
        let content_digest = sha256(&content);
        let param_digest = sha256(&sfo_out);
        let head = header.digest();
        let g = &mut entries[K_GENERAL].data;
        put_be16(g, 0, 0xD256);
        put_be16(g, 2, 0x100);
        put_be32(g, 0x1C, 0x2 | 0x4 | 0x8 | 0x20 | 0x40);
        g[0x20..0x40].copy_from_slice(&content_digest);
        g[0x40..0x60].copy_from_slice(&header.pfs_image_digest);
        g[0x60..0x80].copy_from_slice(&head);
        g[0xA0..0xC0].copy_from_slice(&major_digest);
        g[0xC0..0xE0].copy_from_slice(&param_digest);
    }
    let mut body = vec![0u8; header.pfs_offset as usize];
    let mut sorted: Vec<usize> = (0..count).collect();
    sorted.sort_by_key(|i| entries[*i].id);
    let mut metas = vec![0u8; count * 32];
    for (i, e) in sorted.iter().enumerate() {
        metas[i * 32..i * 32 + 32].copy_from_slice(&entries[*e].row());
    }
    entries[K_METAS].data = metas;
    for e in &entries {
        let mut data = e.data.clone();
        data.resize(e.size as usize, 0);
        if e.flags1 & 0x8000_0000 != 0 {
            if !data.len().is_multiple_of(16) {
                return format_err("an encrypted entry is not a multiple of 16 bytes");
            }
            let key = package_key(&content_id, passcode, (e.flags2 >> 12) & 0xF);
            let mut seed_in = e.row().to_vec();
            seed_in.extend_from_slice(&key);
            let seed = sha256(&seed_in);
            let mut k = [0u8; 16];
            k.copy_from_slice(&seed[16..]);
            let mut iv = [0u8; 16];
            iv.copy_from_slice(&seed[..16]);
            crate::crypto::aes128_cbc_encrypt(&k, iv, &mut data);
        }
        let at = e.offset as usize;
        if at + data.len() > body.len() {
            return format_err("a container entry runs into the image");
        }
        body[at..at + data.len()].copy_from_slice(&data);
    }
    {
        let digests_at = entries[K_DIGESTS].offset as usize;
        for (i, e) in sorted.iter().enumerate().skip(1) {
            let e = &entries[*e];
            let d = sha256(&body[e.offset as usize..(e.offset + e.size) as usize]);
            entries[K_DIGESTS].data[i * 32..i * 32 + 32].copy_from_slice(&d);
            body[digests_at + i * 32..digests_at + i * 32 + 32].copy_from_slice(&d);
        }
        header.body_digest =
            sha256(&body[BODY_OFFSET as usize..(BODY_OFFSET + header.body_size) as usize]);
        header.digest_table = sha256(&entries[K_DIGESTS].data);
        let slice = |e: &Entry, size: u32| &body[e.offset as usize..(e.offset + size) as usize];
        let mut sc = Vec::new();
        for k in [K_KEYS, K_IMAGE_KEY, K_GENERAL, K_METAS, K_DIGESTS] {
            sc.extend_from_slice(slice(&entries[k], entries[k].size));
        }
        header.sc_entries1 = sha256(&sc);
        sc.clear();
        for k in [K_KEYS, K_IMAGE_KEY, K_GENERAL, K_METAS] {
            let size = if k == K_METAS {
                6 * 0x20
            } else {
                entries[k].size
            };
            sc.extend_from_slice(slice(&entries[k], size));
        }
        header.sc_entries2 = sha256(&sc);
    }
    header.write(&mut body);
    let head = sha256(&body[..0xFE0]);
    body[0xFE0..0x1000].copy_from_slice(&head);
    let whole_head = sha256(&body[..0x1000]);
    let signature = mt_wrap(public_key(3), &whole_head);
    body[0x1000..0x1100].copy_from_slice(&signature);
    write_at(out, 0, &body)?;
    out.flush()?;

    // What the package reads back as: the image's files, then the container's.
    for (f, file) in inner.tree.files.iter().enumerate() {
        let path = inner.tree.file_path(f).trim_start_matches('/').to_string();
        let sha = match hashes.get(&f) {
            Some(h) => *h,
            None if file.size == 0 => sha256(&[]),
            None => {
                return format_err(format!(
                    "{path} was not read in order while it was packaged"
                ))
            }
        };
        files.push(Expected {
            path,
            size: file.size,
            sha256: sha,
        });
    }
    for e in &entries {
        let name = match (e.id, &e.name) {
            (0x400, _) => "license.dat".to_string(),
            (0x401, _) => "license.info".to_string(),
            (0x409, _) => "psreserved.dat".to_string(),
            (_, Some(n)) if e.id >= 0x400 => n.clone(),
            _ => continue,
        };
        let mut data = e.data.clone();
        data.resize(e.size as usize, 0);
        files.push(Expected {
            path: format!("sce_sys/{name}"),
            size: data.len() as u64,
            sha256: sha256(&data),
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut empty_dirs = w.empty_dirs;
    empty_dirs.sort();
    Ok(Written {
        content_id,
        title,
        title_id,
        size: header.package_size,
        files,
        empty_dirs,
        made_licence,
        made_keystone,
    })
}

/// The outer image (signed and encrypted) at `offset` of `out`: its one file is `pfsc` then
/// the inner image's blocks.
fn write_outer(
    m: &Pfs,
    out: &mut File,
    offset: u64,
    pfsc: &[u8],
    inner: &mut InnerImage,
    progress: &mut dyn FnMut(u64),
    cancel: &AtomicBool,
) -> Result<()> {
    let bs = BS;
    let mut meta = m.metadata();
    // Indirect blocks are metadata too (they get the data blocks' signatures).
    for s in &m.final_sigs {
        meta.entry(s.block)
            .or_insert_with(|| vec![0u8; bs as usize]);
    }
    let mut message = [0u8; 20];
    message[4..].copy_from_slice(&m.p.seed);
    message[0] = 2;
    let sign_key = hmac_sha256(&m.p.ekpfs, &[&message]);
    message[0] = 1;
    let enc = hmac_sha256(&m.p.ekpfs, &[&message]);
    let mut keys = XtsKeys {
        tweak: [0; 16],
        data: [0; 16],
    };
    keys.tweak.copy_from_slice(&enc[..16]);
    keys.data.copy_from_slice(&enc[16..]);
    let xts = Xts::new(&keys);
    let encrypt = |block: u64, data: &mut [u8]| {
        if !m.p.encrypt || block == 0 || block == m.empty_block {
            return;
        }
        for (s, sector) in data.chunks_mut(SECTOR as usize).enumerate() {
            xts.encrypt(block * (bs / SECTOR) + s as u64, sector);
        }
    };
    fn put_sig(meta: &mut BTreeMap<u64, Vec<u8>>, s: &Sig, d: &[u8; 32]) {
        let (b, within) = (s.offset / BS, (s.offset % BS) as usize);
        if let Some(block) = meta.get_mut(&b) {
            block[within..within + 32].copy_from_slice(d);
            block[within + 32..within + 36].copy_from_slice(&(s.block as u32).to_le_bytes());
        }
    }

    // Data blocks: folders from memory; the file read, signed and encrypted on every core.
    for s in &m.data_sigs {
        if let Some(block) = meta.get(&s.block) {
            let d = hmac_sha256(&sign_key, &[block]);
            put_sig(&mut meta, s, &d);
        }
    }
    let header_blocks = pfsc.len() as u64 / bs;
    let mut buf = vec![0u8; BATCH * bs as usize];
    for (f, file) in m.tree.files.iter().enumerate() {
        let blocks = file.size.div_ceil(bs);
        let mut first = 0;
        while first < blocks {
            if workshop::checkpoint(cancel) {
                return Err(Error::Cancelled);
            }
            let count = (BATCH as u64).min(blocks - first);
            let span = &mut buf[..(count * bs) as usize];
            // pfs_image.dat: the PFSC header's blocks, then the inner image's.
            let mut k = 0;
            while k < count {
                let b = first + k;
                let at = (k * bs) as usize;
                if b < header_blocks {
                    span[at..at + bs as usize]
                        .copy_from_slice(&pfsc[(b * bs) as usize..((b + 1) * bs) as usize]);
                    k += 1;
                } else {
                    let n = (count - k) as usize;
                    inner.fill(b - header_blocks, &mut span[at..at + n * bs as usize])?;
                    k = count;
                }
            }
            let start = m.file_start[f] + first;
            let digests: Vec<std::sync::Mutex<[u8; 32]>> = (0..count)
                .map(|_| std::sync::Mutex::new([0u8; 32]))
                .collect();
            each_block(span, &|k, block| {
                let d = hmac_sha256(&sign_key, &[block]);
                encrypt(start + k as u64, block);
                if let Ok(mut slot) = digests[k].lock() {
                    *slot = d;
                }
            });
            for (k, d) in digests.into_iter().enumerate() {
                let sig = m.data_sigs[m.file_first_sig[f] + first as usize + k];
                if sig.block != start + k as u64 {
                    return format_err("a PFS data block has the wrong signature slot");
                }
                put_sig(&mut meta, &sig, &d.into_inner().unwrap_or_default());
            }
            write_at(out, offset + start * bs, span)?;
            first += count;
            progress((first * bs).min(file.size));
        }
    }

    // The rest of the signatures, last pushed first (indirect blocks, then the folders' and
    // inodes' blocks, the superblock last).
    for s in m.final_sigs.iter().rev() {
        let d = {
            let block = &meta[&s.block];
            hmac_sha256(&sign_key, &[&block[..s.size as usize]])
        };
        put_sig(&mut meta, s, &d);
    }

    // Metadata blocks, and zero blocks nothing covers (encrypted like the rest).
    let mut written = vec![false; m.ndblock as usize];
    for (f, file) in m.tree.files.iter().enumerate() {
        for b in 0..file.size.div_ceil(bs) {
            written[(m.file_start[f] + b) as usize] = true;
        }
    }
    for (block, data) in meta.iter_mut() {
        if *block >= m.ndblock {
            return format_err("PFS metadata lies past the image");
        }
        encrypt(*block, data);
        write_at(out, offset + block * bs, data)?;
        written[*block as usize] = true;
    }
    if m.p.encrypt {
        let mut zero = vec![0u8; bs as usize];
        for b in 1..m.ndblock {
            if written[b as usize] || b == m.empty_block {
                continue;
            }
            zero.fill(0);
            encrypt(b, &mut zero);
            write_at(out, offset + b * bs, &zero)?;
        }
    }
    Ok(())
}

// ── reading the package back ─────────────────────────────────────────────────────────────────

/// Opens the package at `path` with our PS4 reader and compares every file with what was
/// written: the same files at the same sizes, the same empty folders, and the same SHA-256 of
/// every byte. `progress` hears bytes compared and the total.
pub fn read_back(
    path: &std::path::Path,
    written: &Written,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<()> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut p = crate::ps4_pkg::Ps4Package::open(
        Box::new(file),
        len,
        DEFAULT_PASSCODE,
        path.display().to_string(),
    )
    .map_err(|e| Error::Format(format!("the package could not be read back: {e}")))?;
    let info = p.info();
    if info.content_id != written.content_id || info.kind != crate::ps4_pkg::Ps4Kind::Game {
        return format_err("the package read back as something other than the game written");
    }
    let got: Vec<(String, u64)> = p.files().iter().map(|f| (f.path.clone(), f.size)).collect();
    let want: Vec<(String, u64)> = written
        .files
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    if got != want {
        let missing = want.iter().find(|w| !got.contains(w));
        let extra = got.iter().find(|g| !want.contains(g));
        return format_err(match (missing, extra) {
            (Some((p, s)), _) => format!("the package read back without {p} ({s} bytes)"),
            (_, Some((p, s))) => format!("the package read back with {p} ({s} bytes), not written"),
            _ => "the package read back with other files than were written".to_string(),
        });
    }
    let mut empty: Vec<String> = SourceTree::empty_dirs(&p)
        .iter()
        .map(|d| d.trim_matches('/').to_string())
        .collect();
    empty.sort();
    if empty != written.empty_dirs {
        return format_err(format!(
            "the package read back with empty folders {empty:?} where the game has {:?}",
            written.empty_dirs
        ));
    }
    let total: u64 = written.files.iter().map(|f| f.size).sum();
    let mut done = 0u64;
    progress(0, total);
    const CHUNK: u64 = 8 << 20;
    for f in &written.files {
        let mut h = Sha256::new();
        let mut at = 0;
        while at < f.size {
            if workshop::checkpoint(cancel) {
                return Err(Error::Cancelled);
            }
            let n = CHUNK.min(f.size - at) as usize;
            let bytes = p.read_range(&f.path, at, n)?;
            if bytes.is_empty() {
                return format_err(format!("{} ended early when read back", f.path));
            }
            h.update(&bytes);
            at += bytes.len() as u64;
            done += bytes.len() as u64;
            progress(done, total);
        }
        if <[u8; 32]>::from(h.finalize()) != f.sha256 {
            return format_err(format!(
                "{} differs between the package and the game",
                f.path
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ps4_pkg::tests::fixture;
    use crate::ps4_pkg::Ps4Package;

    fn open(bytes: Vec<u8>) -> Ps4Package {
        let len = bytes.len() as u64;
        Ps4Package::open(
            Box::new(std::io::Cursor::new(bytes)),
            len,
            DEFAULT_PASSCODE,
            "t".into(),
        )
        .unwrap()
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ps4w-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn build(
        tree: &mut dyn SourceTree,
        time: Option<i64>,
        path: &std::path::Path,
    ) -> Result<Written> {
        let mut out = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        write(
            tree,
            &mut out,
            &Options { time },
            &mut |_, _, _| {},
            &AtomicBool::new(false),
        )
    }

    /// The synthetic fixture (Alembic's package, LibOrbisPkg's bytes), read by our reader and
    /// written again at its own time, is the same package byte for byte wherever the trimmed
    /// fixture kept its bytes: every 4 KiB a reader reads (the header, the entries, the outer
    /// image's metadata and the game's data).
    #[test]
    fn rewriting_the_fixture_gives_its_bytes() {
        let original = fixture("synthetic");
        let pfs_at = crate::be64(&original, 0x410) as usize;
        let time = crate::le64(&original, pfs_at + 0x68) as i64;
        let mut src = open(original.clone());
        let dir = scratch("same");
        let path = dir.join("out.pkg");
        let w = build(&mut src, Some(time), &path).unwrap();
        assert!(!w.made_licence, "the fixture's licence is kept");
        assert!(!w.made_keystone, "the fixture's keystone is kept");
        let ours = std::fs::read(&path).unwrap();
        assert_eq!(ours.len(), original.len());
        let mut kept = 0;
        for (i, (a, b)) in original.chunks(0x1000).zip(ours.chunks(0x1000)).enumerate() {
            if a.iter().any(|x| *x != 0) {
                kept += 1;
                assert!(a == b, "4 KiB sector {i} (at {:#x}) differs", i * 0x1000);
            }
        }
        assert!(kept > 20, "the fixture kept {kept} sectors");
        read_back(&path, &w, &mut |_, _| {}, &AtomicBool::new(false)).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A tree's files written out as a folder.
    fn to_folder(tree: &mut dyn SourceTree, dir: &std::path::Path) {
        let files: Vec<String> = tree.files().iter().map(|f| f.path.clone()).collect();
        for f in files {
            let p = dir.join(&f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, tree.read(&f).unwrap()).unwrap();
        }
        for d in tree.empty_dirs().to_vec() {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
    }

    /// The fixture's game as a folder, with the files a package makes again taken out, as a
    /// game unpacked by Convert has them.
    fn game_folder(dir: &std::path::Path) {
        let mut src = open(fixture("synthetic"));
        to_folder(&mut src, dir);
        for made in [
            "license.dat",
            "license.info",
            "psreserved.dat",
            "playgo-chunk.dat",
            "playgo-chunk.sha",
            "playgo-manifest.xml",
            "keystone",
        ] {
            std::fs::remove_file(dir.join("sce_sys").join(made)).unwrap();
        }
        std::fs::create_dir_all(dir.join("data/empty_dir")).unwrap();
        std::fs::write(dir.join("sce_sys/param.json"), b"{}").unwrap();
    }

    /// From a folder: the licence and the keystone are made, param.json stays out, an empty
    /// folder stays, every file reads back, and the same time gives the same bytes.
    #[test]
    fn a_folder_packages_reads_back_and_is_deterministic() {
        let dir = scratch("folder");
        let game = dir.join("game");
        game_folder(&game);
        let mut tree = crate::source::FolderSource::open(&game).unwrap();
        let a = build(&mut tree, Some(1_600_000_000), &dir.join("a.pkg")).unwrap();
        assert!(a.made_licence && a.made_keystone);
        assert_eq!(a.content_id, "UP0000-CUSA99990_00-TESTGAME00000000");
        assert_eq!(a.title.as_deref(), Some("Test PS4 Game"));
        assert_eq!(a.empty_dirs, ["data/empty_dir"]);
        let paths: Vec<&str> = a.files.iter().map(|f| f.path.as_str()).collect();
        assert!(!paths.contains(&"sce_sys/param.json"), "{paths:?}");
        for want in ["eboot.bin", "sce_sys/keystone", "sce_sys/license.dat"] {
            assert!(paths.contains(&want), "{want} in {paths:?}");
        }
        read_back(
            &dir.join("a.pkg"),
            &a,
            &mut |_, _| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        let mut tree = crate::source::FolderSource::open(&game).unwrap();
        build(&mut tree, Some(1_600_000_000), &dir.join("b.pkg")).unwrap();
        let (x, y) = (
            std::fs::read(dir.join("a.pkg")).unwrap(),
            std::fs::read(dir.join("b.pkg")).unwrap(),
        );
        assert!(x == y, "the same game and time give the same package");

        // The package reads as a PS4 game with the made keystone and licence.
        let mut p = open(x);
        assert_eq!(p.read("sce_sys/keystone").unwrap(), keys::PS4_KEYSTONE);
        assert_eq!(
            p.read("sce_sys/license.dat").unwrap(),
            license::license_dat_for(&a.content_id, 0xF, 0x1A)
        );
        let sfo = Sfo::read(&p.read("sce_sys/param.sfo").unwrap()).unwrap();
        assert_eq!(sfo.display("PUBTOOLVER"), "0x02890000");
        assert!(sfo.display("PUBTOOLINFO").starts_with("c_date="));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The keystone made for the all-zero passcode is the one LibOrbisPkg made for the fixture.
    #[test]
    fn the_keystone_is_liborbispkgs() {
        let mut p = open(fixture("synthetic"));
        assert_eq!(p.read("sce_sys/keystone").unwrap(), keys::PS4_KEYSTONE);
    }

    /// A game merged with its update packages as one game at the update's version.
    #[test]
    fn a_merged_game_packages() {
        let game = Box::new(open(fixture("synthetic")));
        let update = Box::new(open(fixture("update")));
        let mut merged = crate::ps4_merge::Ps4Merged::new(game, update).unwrap();
        let dir = scratch("merge");
        let path = dir.join("m.pkg");
        let w = build(&mut merged, None, &path).unwrap();
        read_back(&path, &w, &mut |_, _| {}, &AtomicBool::new(false)).unwrap();
        let mut p = open(std::fs::read(&path).unwrap());
        assert_eq!(p.info().kind, crate::ps4_pkg::Ps4Kind::Game);
        assert_eq!(p.info().text("APP_VER").as_deref(), Some("01.05"));
        assert_eq!(p.read("eboot.bin").unwrap(), b"\x7fELF patched eboot 01.05");
        assert_eq!(p.read("data/added.bin").unwrap(), b"added by the update");
        assert_eq!(p.read("data/empty.bin").unwrap(), b"");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// What isn't a PS4 game is refused, in a sentence that says why.
    #[test]
    fn what_isnt_a_ps4_game_is_refused() {
        let why = |tree: &mut dyn SourceTree| refusal(tree).expect("refused");
        assert!(
            why(&mut open(fixture("update"))).contains("PS4 update (CATEGORY gp)"),
            "an update"
        );
        assert!(why(&mut open(fixture("addon"))).contains("no sce_sys/param.sfo"));
        assert!(refusal(&mut open(fixture("synthetic"))).is_none());

        let dir = scratch("refuse");
        let ps5 = dir.join("ps5");
        std::fs::create_dir_all(ps5.join("sce_sys")).unwrap();
        std::fs::write(ps5.join("sce_sys/param.json"), b"{}").unwrap();
        std::fs::write(ps5.join("eboot.bin"), b"x").unwrap();
        let mut tree = crate::source::FolderSource::open(&ps5).unwrap();
        assert!(why(&mut tree).contains("a PS5 game has sce_sys/param.json"));

        let addon = dir.join("addon");
        std::fs::create_dir_all(addon.join("sce_sys")).unwrap();
        let sfo = with_value(&fixture_sfo(), "CATEGORY", "ac");
        std::fs::write(addon.join("sce_sys/param.sfo"), sfo).unwrap();
        let mut tree = crate::source::FolderSource::open(&addon).unwrap();
        assert!(why(&mut tree).contains("PS4 add-on (CATEGORY ac)"));

        let bad_id = dir.join("bad_id");
        std::fs::create_dir_all(bad_id.join("sce_sys")).unwrap();
        let sfo = with_value(&fixture_sfo(), "CONTENT_ID", "UP0000-SHORT");
        std::fs::write(bad_id.join("sce_sys/param.sfo"), sfo).unwrap();
        let mut tree = crate::source::FolderSource::open(&bad_id).unwrap();
        assert!(why(&mut tree).contains("no usable CONTENT_ID"));

        // A name the PFS can't hold, refused before anything is written.
        let game = dir.join("game");
        game_folder(&game);
        std::fs::write(game.join("data/caf\u{e9}.bin"), b"x").unwrap();
        let mut tree = crate::source::FolderSource::open(&game).unwrap();
        let path = dir.join("x.pkg");
        let e = build(&mut tree, None, &path)
            .expect_err("refused")
            .to_string();
        assert!(e.contains("plain ASCII") && e.contains("data/caf"), "{e}");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn fixture_sfo() -> Vec<u8> {
        open(fixture("synthetic"))
            .read("sce_sys/param.sfo")
            .unwrap()
    }

    fn with_value(sfo: &[u8], key: &str, value: &str) -> Vec<u8> {
        let mut s = Sfo::read(sfo).unwrap();
        let max = s.find(key).map_or(48, |e| e.max_length);
        s.set(SfoEntry {
            key: key.into(),
            format: SFO_UTF8,
            text: value.as_bytes().to_vec(),
            number: 0,
            max_length: max,
        });
        s.write()
    }

    /// A folder whose entries fill a block but for LibOrbisPkg's last 280 bytes, which it would
    /// size as one block and spill into the next file's: sized to hold them, and read back whole.
    #[test]
    fn a_folder_that_spills_into_a_second_block_reads_back() {
        let dir = scratch("spill");
        let game = dir.join("game");
        game_folder(&game);
        std::fs::create_dir_all(game.join("many")).unwrap();
        // ".", ".." and 2044 entries of 32 bytes: 65,456 bytes, one block by their sizes.
        for i in 0..2044 {
            std::fs::write(game.join(format!("many/file_{i:04}.bin")), b"x").unwrap();
        }
        let mut tree = crate::source::FolderSource::open(&game).unwrap();
        let path = dir.join("s.pkg");
        let w = build(&mut tree, Some(0), &path).unwrap();
        read_back(&path, &w, &mut |_, _| {}, &AtomicBool::new(false)).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_cancel_stops_the_build() {
        let dir = scratch("cancel");
        let mut src = open(fixture("synthetic"));
        let mut out = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(dir.join("c.pkg"))
            .unwrap();
        let e = write(
            &mut src,
            &mut out,
            &Options::default(),
            &mut |_, _, _| {},
            &AtomicBool::new(true),
        )
        .expect_err("cancelled");
        assert!(matches!(e, Error::Cancelled), "{e}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// .NET's order as LibOrbisPkg sorts with it (Alembic's checks against .NET 10).
    #[test]
    fn names_sort_in_dotnets_order() {
        let mut names = vec![
            "b", "B", "a_b", "a-b", "a.b", "ab", "A", "a1", "_", "10", "9",
        ];
        names.sort_by(|a, b| dotnet_cmp(a, b));
        assert_eq!(
            names,
            ["_", "10", "9", "A", "a_b", "a-b", "a.b", "a1", "ab", "b", "B"]
        );
        assert_eq!(entry_id_for_path("trophy/trophy07.trp"), Some(0x1407));
        assert_eq!(entry_id_for_path("keymap_rp/003.png"), Some(0x1602));
        assert_eq!(entry_id_for_path("keymap_rp/02/001.png"), Some(0x1630));
        assert_eq!(entry_id_for_name("icon0_30.dds"), Some(0x1281 + 30));
        assert_eq!(entry_id_for_name("icon0_31.dds"), None);
    }
}
