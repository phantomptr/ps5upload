//! A game's unsigned PS5 executables (plain ELF, `e_type` `0xFE10`/`0xFE18`) served as
//! fake-signed SELFs: the console's loader refuses a plain ELF, so such a game never starts.
//! Ported from PS5 Dump Forge (quer3q, GPL-3.0-or-later, with its UnRAR exception). Byte for byte what ampr-emu-ng's
//! `tools/make_fself.py --ptype fake` writes (its defaults: paid `0x3100000000000002`, ptype 1,
//! versions 0, no auth info): a header, an entry pair per copied segment (zero digests, then the
//! segment's bytes), the ELF and program headers, ex-info with the ELF's SHA-256, a zero NPDRM
//! block, zero meta blocks, footer and signature, then the segments, then any `PT_SCE_VERSION`
//! bytes.
//!
//! The source is never touched; only what a Convert build reads from it changes. A folder sent to
//! the console as it is keeps its files as they are.

use std::collections::HashMap;

use sha2::{Digest, Sha256};

use crate::ampr_index;
use crate::source::{magic, SourceFile, SourceTree};
use crate::{Error, Result};

const ET_SCE_EXEC_ASLR: u16 = 0xFE10;
const ET_SCE_DYNAMIC: u16 = 0xFE18;
const PT_LOAD: u32 = 1;
const PT_SCE_DYNLIBDATA: u32 = 0x6100_0000;
const PT_SCE_RELRO: u32 = 0x6100_0010;
const PT_SCE_COMMENT: u32 = 0x6FFF_FF00;
const PT_SCE_VERSION: u32 = 0x6FFF_FF01;
/// The segments a SELF carries; every other program header lies inside one of them.
const COPIED: [u32; 4] = [PT_LOAD, PT_SCE_RELRO, PT_SCE_DYNLIBDATA, PT_SCE_COMMENT];
const PHDR: usize = 0x38;
/// A data entry's block, as the script signs it (one zero digest per block).
const BLOCK: u64 = 0x4000;
/// The ELF is hashed in reads of this size.
const CHUNK: usize = 8 << 20;
/// The first read of an executable: its header and, as built, its program headers.
const HEAD: u64 = 0x4000;
/// make_fself's defaults: program authority id and program type (fake).
const PAID: u64 = 0x3100_0000_0000_0002;
const PTYPE_FAKE: u64 = 1;
/// Executables by extension, as backport tools pick them.
const EXTENSIONS: [&str; 5] = ["bin", "elf", "self", "prx", "sprx"];
/// At most this many executables are signed; a game has a few dozen.
const MAX_EXECUTABLES: usize = 4096;
/// A path index this big is not read.
const MAX_INDEX: u64 = 64 << 20;

/// An executable by extension, a `.bin` only when named `eboot*`: games hold thousands of data
/// `.bin` files, and a solid archive would be decoded to the last one just to look.
fn executable(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some((_, ext)) = name.rsplit_once('.') else {
        return false;
    };
    if ext.eq_ignore_ascii_case("bin") {
        return name
            .get(..5)
            .is_some_and(|s| s.eq_ignore_ascii_case("eboot"));
    }
    EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e))
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn is_unsigned(head: &[u8]) -> bool {
    head.get(..4) == Some(&magic::RAW_ELF[..])
        && matches!(u16_at(head, 0x10), Some(ET_SCE_EXEC_ASLR | ET_SCE_DYNAMIC))
}

/// The executables (by extension) that are unsigned PS5 ELFs, in `files()` order; `fakelib/`
/// and `fakelib2/` included, the loader loads them too.
pub fn unsigned(tree: &mut dyn SourceTree) -> Vec<String> {
    let paths: Vec<String> = tree
        .files()
        .iter()
        .filter(|f| f.size >= 0x40 && executable(&f.path))
        .map(|f| f.path.clone())
        .collect();
    let mut found = Vec::new();
    for path in paths {
        if found.len() >= MAX_EXECUTABLES {
            break;
        }
        if tree
            .read_range(&path, 0, 0x12)
            .is_ok_and(|h| is_unsigned(&h))
        {
            found.push(path);
        }
    }
    found
}

fn align16(x: u64) -> Option<u64> {
    Some(x.checked_add(15)? & !15)
}

/// One executable's SELF: `head` holds bytes `0..head.len()` (headers, meta, signature),
/// `copies` the ELF ranges placed after it, zeros everywhere else.
struct Plan {
    elf_size: u64,
    size: u64,
    head: Vec<u8>,
    /// The ex-info digest's place in `head`, filled on the first read that touches it.
    digest_at: usize,
    digest_done: bool,
    /// (SELF offset, ELF offset, length).
    copies: Vec<(u64, u64, u64)>,
}

/// The SELF of the ELF at `path` (`elf_size` bytes), from its headers only.
fn plan(tree: &mut dyn SourceTree, path: &str, elf_size: u64) -> std::result::Result<Plan, String> {
    let err = |e: Error| e.to_string();
    // The header and (where it fits, as usual) the program headers in one read: a second read
    // right after it would make a remote source fetch a whole read-ahead window.
    let first = tree
        .read_range(path, 0, elf_size.min(HEAD) as usize)
        .map_err(err)?;
    let h = &first[..first.len().min(0x40)];
    if h.len() < 0x40 || !is_unsigned(h) {
        return Err("not a whole PS5 ELF header".into());
    }
    // make_fself's own refusals.
    if h[4] != 2 || h[5] != 1 {
        return Err("not a 64-bit little-endian ELF".into());
    }
    if u16_at(h, 0x12) != Some(0x3E) || u32_at(h, 0x14) != Some(1) {
        return Err("not an x86-64 ELF version 1".into());
    }
    let shentsize = u16_at(h, 0x3A).unwrap_or(1);
    if u16_at(h, 0x36) != Some(PHDR as u16) || !(shentsize == 0 || shentsize == 0x40) {
        return Err("unsupported header entry size".into());
    }
    let too_big = || "headers too large for a SELF".to_string();
    let phoff = u64_at(h, 0x20).unwrap_or(0);
    let ehsize = u64::from(u16_at(h, 0x34).unwrap_or(0));
    let phnum = usize::from(u16_at(h, 0x38).unwrap_or(0));
    let end = phoff.saturating_add((phnum * PHDR) as u64);
    let table = if phnum == 0 {
        Vec::new()
    } else if end <= first.len() as u64 {
        first[phoff as usize..end as usize].to_vec()
    } else {
        tree.read_range(path, phoff, phnum * PHDR).map_err(err)?
    };
    if table.len() != phnum * PHDR {
        return Err("program headers past the end of the file".into());
    }
    // (index, type, offset, file size) per program header.
    let phdrs: Vec<(usize, u32, u64, u64)> = (0..phnum)
        .map(|i| {
            let at = i * PHDR;
            let kind = u32_at(&table, at).unwrap_or(0);
            let off = u64_at(&table, at + 8).unwrap_or(0);
            (i, kind, off, u64_at(&table, at + 0x20).unwrap_or(0))
        })
        .collect();
    let copied: Vec<_> = phdrs.iter().filter(|p| COPIED.contains(&p.1)).collect();
    let version = phdrs.iter().rfind(|p| p.1 == PT_SCE_VERSION);
    for p in copied.iter().copied().chain(version) {
        if p.3 > 0 && p.2.checked_add(p.3).is_none_or(|end| end > elf_size) {
            return Err(format!("segment {} runs past the end of the file", p.0));
        }
    }
    let n = copied.len() * 2;
    let span = (phoff
        .checked_add((phnum * PHDR) as u64)
        .ok_or_else(too_big)?)
    .max(ehsize);
    let elf_at = 0x20 + 0x20 * n;
    let elf_hs = align16(span).ok_or_else(too_big)?;
    let header_size = elf_hs
        .checked_add(elf_at as u64 + 0x40 + 0x30)
        .filter(|&s| s <= 0xFFFF)
        .ok_or_else(too_big)? as usize;
    let meta_size = 0x50 * n + 0x50 + 0x100;
    if n > 0xFFFF || meta_size > 0xFFFF {
        return Err(too_big());
    }
    let elf_hs = elf_hs as usize;
    let mut head = vec![0u8; header_size + meta_size];
    let mut put = |at: usize, bytes: &[u8]| head[at..at + bytes.len()].copy_from_slice(bytes);

    // Entries: a meta entry of zero digests, then the segment's bytes, each 16-aligned.
    let mut offset = (header_size + meta_size) as u64;
    let mut copies = Vec::new();
    for (k, p) in copied.iter().enumerate().map(|(j, p)| (2 * j, p)) {
        let digests = p.3.div_ceil(BLOCK) * 0x20;
        let meta = (1 << 2) | (1 << 16) | (((k + 1) as u64) << 20);
        let data = (1 << 2) | (1 << 11) | (2 << 12) | ((p.0 as u64) << 20);
        let entry =
            |props: u64, at: u64, len: u64| [props, at, len, len].map(u64::to_le_bytes).concat();
        put(0x20 + 0x20 * k, &entry(meta, offset, digests));
        offset = offset
            .checked_add(digests)
            .and_then(align16)
            .ok_or_else(too_big)?;
        put(0x20 + 0x20 * (k + 1), &entry(data, offset, p.3));
        copies.push((offset, p.2, p.3));
        offset = offset
            .checked_add(p.3)
            .and_then(align16)
            .ok_or_else(too_big)?;
    }
    let file_size = offset;
    // Common and extended header.
    put(0, &[0x4F, 0x15, 0x3D, 0x1D, 0x00, 0x01, 0x01, 0x12]);
    put(8, &0x101u32.to_le_bytes());
    put(0xC, &(header_size as u16).to_le_bytes());
    put(0xE, &(meta_size as u16).to_le_bytes());
    put(0x10, &file_size.to_le_bytes());
    put(0x18, &(n as u16).to_le_bytes());
    put(0x1A, &0x22u16.to_le_bytes());
    // The ELF header as the script saves it: e_ident[6] is e_version, [9..15] zero,
    // e_shnum 0 (section headers ignored); then each program header at its offset.
    let mut ehdr = h[..0x40].to_vec();
    ehdr[6] = 1;
    ehdr[9..15].fill(0);
    ehdr[0x3C..0x3E].fill(0);
    put(elf_at, &ehdr);
    for i in 0..phnum {
        put(
            elf_at + phoff as usize + i * PHDR,
            &table[i * PHDR..(i + 1) * PHDR],
        );
    }
    // Ex-info (digest later), NPDRM block type 3; meta blocks, footer, signature.
    let info = elf_at + elf_hs;
    put(
        info,
        &[PAID, PTYPE_FAKE, 0, 0].map(u64::to_le_bytes).concat(),
    );
    put(info + 0x40, &3u16.to_le_bytes());
    let footer = info + 0x70 + 0x50 * n;
    put(footer + 48, &0x10000u32.to_le_bytes());

    let mut size = file_size;
    if let Some(v) = version {
        copies.push((file_size, v.2, v.3));
        size = file_size.checked_add(v.3).ok_or_else(too_big)?;
    }
    Ok(Plan {
        elf_size,
        size,
        head,
        digest_at: info + 0x20,
        digest_done: false,
        copies,
    })
}

/// What [`wrap`] did to a source, for a log.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Signing {
    /// The executables it signs, in `files()` order.
    pub signed: Vec<String>,
    /// `(path, why)` for each unsigned executable it serves unchanged.
    pub refused: Vec<(String, String)>,
    /// How many `ampr_emu.index` records it set to the signed sizes.
    pub resized: usize,
}

impl Signing {
    /// One line for a log; `None` when there was nothing to sign.
    pub fn describe(&self) -> Option<String> {
        if self.signed.is_empty() && self.refused.is_empty() {
            return None;
        }
        let mut line = format!("fake-signing {} unsigned executable(s)", self.signed.len());
        if !self.signed.is_empty() {
            line.push_str(&format!(
                ": {}",
                self.signed[..self.signed.len().min(10)].join(", ")
            ));
            if self.signed.len() > 10 {
                line.push_str(&format!(" and {} more", self.signed.len() - 10));
            }
        }
        if self.resized > 0 {
            line.push_str(&format!(
                "; {} {} record(s) set to the signed sizes",
                ampr_index::NAME,
                self.resized
            ));
        }
        for (path, why) in &self.refused {
            line.push_str(&format!("; can't sign {path}: {why}, left as it is"));
        }
        Some(line)
    }
}

/// `tree` with its unsigned PS5 executables read as fake-signed SELFs at their SELF sizes; one
/// that can't be planned, and every other file, read as they are. The tree unchanged when it
/// has none.
pub fn wrap(mut inner: Box<dyn SourceTree>) -> (Box<dyn SourceTree>, Signing) {
    let sizes: HashMap<String, u64> = inner
        .files()
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    let mut plans = HashMap::new();
    let mut signing = Signing::default();
    for path in unsigned(inner.as_mut()) {
        match plan(inner.as_mut(), &path, sizes[&path]) {
            Ok(p) => {
                plans.insert(path, p);
            }
            Err(why) => signing.refused.push((path, why)),
        }
    }
    if plans.is_empty() {
        return (inner, signing);
    }
    let files: Vec<SourceFile> = inner
        .files()
        .iter()
        .map(|f| SourceFile {
            path: f.path.clone(),
            size: plans.get(&f.path).map_or(f.size, |p| p.size),
        })
        .collect();
    signing.signed = files
        .iter()
        .filter(|f| plans.contains_key(&f.path))
        .map(|f| f.path.clone())
        .collect();
    let index = resized_index(inner.as_mut(), &plans);
    if let Some((_, n)) = &index {
        signing.resized = *n;
    }
    let tree = Signed {
        inner,
        files,
        plans,
        index: index.map(|(bytes, _)| bytes),
    };
    (Box::new(tree), signing)
}

/// `ampr_emu.index` with each record of a signed file that lists its ELF size set to its SELF
/// size, and the count; `None` when it doesn't read or lists none of them. The runtime answers
/// `/app0` lookups from it, and a packed game keeps it as it is.
fn resized_index(
    inner: &mut dyn SourceTree,
    plans: &HashMap<String, Plan>,
) -> Option<(Vec<u8>, usize)> {
    let size = inner
        .files()
        .iter()
        .find(|f| f.path == ampr_index::NAME)?
        .size;
    if size > MAX_INDEX {
        return None;
    }
    let mut bytes = exact(inner, ampr_index::NAME, 0, size as usize).ok()?;
    let records = ampr_index::parse(&bytes)?;
    let mut resized = 0;
    for (i, entry) in records.iter().enumerate() {
        // The runtime folds ASCII case.
        let plan = plans
            .iter()
            .find(|(p, _)| p.eq_ignore_ascii_case(&entry.path))
            .map(|(_, plan)| plan);
        if let Some(plan) = plan.filter(|p| p.elf_size == entry.size) {
            let at = ampr_index::size_at(i);
            bytes[at..at + 8].copy_from_slice(&plan.size.to_le_bytes());
            resized += 1;
        }
    }
    (resized > 0).then_some((bytes, resized))
}

/// Exactly `len` bytes of `path` at `at`: a shorter read means the file changed.
fn exact(tree: &mut dyn SourceTree, path: &str, at: u64, len: usize) -> Result<Vec<u8>> {
    let bytes = tree.read_range(path, at, len)?;
    if bytes.len() != len {
        return Err(Error::Format(format!("{path} changed while it was signed")));
    }
    Ok(bytes)
}

/// SHA-256 of the whole ELF, for the ex-info. The ELF is read twice, once for this digest that
/// sits in front of its segments; byte identity with make_fself is kept.
fn digest(tree: &mut dyn SourceTree, path: &str, size: u64) -> Result<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut at = 0;
    while at < size {
        let len = (size - at).min(CHUNK as u64) as usize;
        hash.update(exact(tree, path, at, len)?);
        at += len as u64;
    }
    Ok(hash.finalize().into())
}

struct Signed {
    inner: Box<dyn SourceTree>,
    files: Vec<SourceFile>,
    plans: HashMap<String, Plan>,
    /// `ampr_emu.index` with the signed files' records at their SELF sizes, when it lists any.
    index: Option<Vec<u8>>,
}

impl SourceTree for Signed {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        if let Some(bytes) = self.index.as_ref().filter(|_| path == ampr_index::NAME) {
            return Ok(bytes.clone());
        }
        match self.plans.get(path) {
            Some(p) => {
                let size = usize::try_from(p.size).unwrap_or(usize::MAX);
                self.read_range(path, 0, size)
            }
            None => self.inner.read(path),
        }
    }

    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let Self {
            inner,
            plans,
            index,
            ..
        } = self;
        if let Some(bytes) = index.as_ref().filter(|_| path == ampr_index::NAME) {
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(bytes.len());
            return Ok(bytes[start..start.saturating_add(len).min(bytes.len())].to_vec());
        }
        let Some(p) = plans.get_mut(path) else {
            return inner.read_range(path, offset, len);
        };
        let start = offset.min(p.size);
        let end = start.saturating_add(len as u64).min(p.size);
        let mut out = vec![0u8; (end - start) as usize];
        let head = p.head.len() as u64;
        if start < head {
            let digest_at = p.digest_at as u64;
            if !p.digest_done && start < digest_at + 32 && digest_at < end {
                let d = digest(inner.as_mut(), path, p.elf_size)?;
                p.head[p.digest_at..p.digest_at + 32].copy_from_slice(&d);
                p.digest_done = true;
            }
            let to = end.min(head);
            out[..(to - start) as usize].copy_from_slice(&p.head[start as usize..to as usize]);
        }
        for &(at, from, n) in &p.copies {
            let (s, e) = (start.max(at), end.min(at + n));
            if s < e {
                let bytes = exact(inner.as_mut(), path, from + (s - at), (e - s) as usize)?;
                out[(s - start) as usize..(e - start) as usize].copy_from_slice(&bytes);
            }
        }
        Ok(out)
    }

    /// The source's own reads for everything but the signed files, so a folder still opens
    /// several at once (a 156 GiB game's check depends on it).
    fn read_heads(
        &mut self,
        paths: &[String],
        len: usize,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<Vec<Vec<u8>>> {
        let plain: Vec<String> = paths
            .iter()
            .filter(|p| !self.plans.contains_key(*p) && **p != ampr_index::NAME)
            .cloned()
            .collect();
        let mut heads = self.inner.read_heads(&plain, len, progress)?.into_iter();
        paths
            .iter()
            .map(|p| {
                if self.plans.contains_key(p) || p == ampr_index::NAME {
                    self.read_range(p, 0, len)
                } else {
                    heads
                        .next()
                        .ok_or_else(|| Error::Format("a source returned too few headers".into()))
                }
            })
            .collect()
    }

    fn empty_dirs(&self) -> &[String] {
        self.inner.empty_dirs()
    }

    fn describe(&self) -> String {
        self.inner.describe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SDK: u32 = 0x0450_0031;
    const PROCESS_PARAM_MAGIC: u32 = 0x4942_524F;
    const PT_SCE_PROCPARAM: u32 = 0x6100_0001;

    struct Files(Vec<SourceFile>, HashMap<String, Vec<u8>>);

    impl Files {
        fn new(list: Vec<(&str, Vec<u8>)>) -> Box<Self> {
            let files = list
                .iter()
                .map(|(p, b)| SourceFile {
                    path: p.to_string(),
                    size: b.len() as u64,
                })
                .collect();
            let bytes = list.into_iter().map(|(p, b)| (p.to_string(), b)).collect();
            Box::new(Self(files, bytes))
        }
    }

    impl SourceTree for Files {
        fn files(&self) -> &[SourceFile] {
            &self.0
        }
        fn read(&mut self, path: &str) -> Result<Vec<u8>> {
            self.1
                .get(path)
                .cloned()
                .ok_or_else(|| Error::Format(format!("no {path}")))
        }
        fn describe(&self) -> String {
            "test files".into()
        }
    }

    /// An eboot-like ELF: a PT_LOAD of 0x4321 bytes holding the process param, a RELRO,
    /// a PT_DYNAMIC (no entry), a PT_SCE_VERSION, section headers left at the end.
    fn elf(kind: u16) -> Vec<u8> {
        let mut f: Vec<u8> = (0..0x58C0u32).map(|i| (i * 7 + 3) as u8).collect();
        let mut put = |at: usize, b: &[u8]| f[at..at + b.len()].copy_from_slice(b);
        put(0, &magic::RAW_ELF);
        put(
            4,
            &[2, 1, 1, 9, 0, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0x55],
        );
        put(0x10, &kind.to_le_bytes());
        put(0x12, &0x3Eu16.to_le_bytes());
        put(0x14, &1u32.to_le_bytes());
        put(0x18, &0x1234u64.to_le_bytes()); // e_entry
        put(0x20, &0x40u64.to_le_bytes()); // e_phoff
        put(0x28, &0x5800u64.to_le_bytes()); // e_shoff
        put(0x30, &0u32.to_le_bytes());
        for (at, v) in [
            (0x34, 0x40u16),
            (0x36, 0x38),
            (0x38, 5),
            (0x3A, 0x40),
            (0x3C, 3),
            (0x3E, 2),
        ] {
            put(at, &v.to_le_bytes());
        }
        let phdrs: [(u32, u64, u64); 5] = [
            (PT_LOAD, 0x1000, 0x4321),
            (PT_SCE_PROCPARAM, 0x1200, 0x40),
            (PT_SCE_RELRO, 0x5400, 0x123),
            (0x2, 0x5600, 0x80), // PT_DYNAMIC
            (PT_SCE_VERSION, 0x5700, 0x25),
        ];
        for (i, (t, off, size)) in phdrs.into_iter().enumerate() {
            let at = 0x40 + i * PHDR;
            put(at, &t.to_le_bytes());
            put(at + 4, &4u32.to_le_bytes()); // p_flags
            put(at + 8, &off.to_le_bytes());
            put(at + 0x10, &(0x10_0000 + off).to_le_bytes()); // p_vaddr
            put(at + 0x18, &(0x10_0000 + off).to_le_bytes()); // p_paddr
            put(at + 0x20, &size.to_le_bytes());
            put(at + 0x28, &(size + 0x100).to_le_bytes()); // p_memsz
            put(at + 0x30, &0x4000u64.to_le_bytes()); // p_align
        }
        put(0x1200, &PROCESS_PARAM_MAGIC.to_le_bytes());
        put(0x120C, &SDK.to_le_bytes());
        f
    }

    fn signed() -> Box<dyn SourceTree> {
        let (tree, signing) = wrap(Files::new(vec![("eboot.bin", elf(ET_SCE_EXEC_ASLR))]));
        assert_eq!(signing.signed, ["eboot.bin"]);
        assert!(signing.refused.is_empty());
        tree
    }

    #[test]
    fn signs_as_make_fself_does() {
        // `make_fself.py --ptype fake` (ampr-emu-ng 0.3.0) on `elf(ET_SCE_EXEC_ASLR)`, the
        // length and hash ps5-dump-forge pins.
        const LEN: usize = 18917;
        const BLAKE3: &str = "d3539ca69b63349fc50f624471f88bf7e6ad91dbaa7deb57b02202931f60eb1b";
        let mut tree = signed();
        assert_eq!(tree.files()[0].size, LEN as u64);
        let whole = tree.read("eboot.bin").unwrap();
        assert_eq!(
            (whole.len(), blake3::hash(&whole).to_hex().to_string()),
            (LEN, BLAKE3.into())
        );
        // Windows across the digest, head/zeros/segment, segment/pad/meta/RELRO and the end.
        for (at, len) in [
            (0x230, 7),
            (0x4F0, 0x60),
            (0x4860, 0x40),
            (0x49B0, 0x100),
            (LEN + 5, 9),
        ] {
            let got = signed().read_range("eboot.bin", at as u64, len).unwrap();
            let want = &whole[at.min(LEN)..(at + len).min(LEN)];
            assert_eq!(got, want, "window {at:#x}+{len:#x}");
        }
    }

    /// The readiness check and the builder find the module's magic and its imports as they do
    /// in any other fake-signed module.
    #[test]
    fn a_signed_eboot_reads_as_a_self() {
        let mut tree = signed();
        assert_eq!(tree.read_range("eboot.bin", 0, 4).unwrap(), magic::SELF_PS4);
    }

    #[test]
    fn finds_only_unsigned_executables() {
        let self_bytes = signed().read("eboot.bin").unwrap();
        let mut tree = Files::new(vec![
            ("eboot.bin", elf(ET_SCE_EXEC_ASLR)),
            ("sce_module/libfoo.prx", self_bytes),
            ("fakelib/libSceFoo.sprx", elf(ET_SCE_DYNAMIC)),
            ("data/blob.bin", vec![7; 0x100]),
            ("data/elf.dat", elf(ET_SCE_DYNAMIC)),
            // A data `.bin` isn't opened, whatever it holds.
            ("data/level.bin", elf(ET_SCE_DYNAMIC)),
            // An ordinary ELF isn't a PS5 executable.
            ("tools/helper.elf", elf(2)),
        ]);
        assert_eq!(
            unsigned(tree.as_mut()),
            ["eboot.bin", "fakelib/libSceFoo.sprx"]
        );
    }

    /// One that can't be signed is left as it is and named; the others still are.
    #[test]
    fn an_unusable_elf_is_left_and_named() {
        let mut wrong = elf(ET_SCE_DYNAMIC);
        wrong[0x12] = 0xB7; // aarch64
        let (mut tree, signing) = wrap(Files::new(vec![
            ("eboot.bin", elf(ET_SCE_EXEC_ASLR)),
            ("sce_module/odd.prx", wrong.clone()),
        ]));
        assert_eq!(signing.signed, ["eboot.bin"]);
        assert_eq!(signing.refused[0].0, "sce_module/odd.prx");
        assert_eq!(tree.read("sce_module/odd.prx").unwrap(), wrong);
        let line = signing.describe().unwrap();
        assert!(line.contains("can't sign sce_module/odd.prx"), "{line}");
    }

    /// A source with nothing to sign comes back as it was.
    #[test]
    fn nothing_to_sign_changes_nothing() {
        let (mut tree, signing) = wrap(Files::new(vec![
            ("eboot.bin", vec![0x54; 0x100]),
            ("data/a.dat", vec![1; 5]),
        ]));
        assert_eq!(signing, Signing::default());
        assert_eq!(signing.describe(), None);
        assert_eq!(tree.files()[0].size, 0x100);
        assert_eq!(tree.read("data/a.dat").unwrap(), vec![1; 5]);
    }

    /// `read_heads` gives what `read_range` gives, in the order asked.
    #[test]
    fn heads_match_the_signed_bytes() {
        let (mut tree, _) = wrap(Files::new(vec![
            ("data/a.dat", vec![1; 0x40]),
            ("eboot.bin", elf(ET_SCE_EXEC_ASLR)),
            ("data/b.dat", vec![2; 0x40]),
        ]));
        let paths: Vec<String> = ["data/b.dat", "eboot.bin", "data/a.dat"]
            .map(String::from)
            .into();
        let heads = tree.read_heads(&paths, 0x20, &mut |_| true).unwrap();
        for (path, head) in paths.iter().zip(&heads) {
            assert_eq!(*head, tree.read_range(path, 0, 0x20).unwrap(), "{path}");
        }
        assert_eq!(heads[1][..4], magic::SELF_PS4);
    }

    #[test]
    fn the_path_index_lists_the_signed_sizes() {
        let elf = elf(ET_SCE_EXEC_ASLR);
        let self_size = signed().files()[0].size;
        let records = |eboot: u64| {
            vec![
                ("EBOOT.BIN".to_string(), eboot),
                ("data/a.bin".to_string(), 5),
            ]
        };
        let index = ampr_index::build(&records(elf.len() as u64), 7).unwrap();
        let (mut tree, signing) = wrap(Files::new(vec![
            ("eboot.bin", elf),
            ("data/a.bin", vec![1; 5]),
            (ampr_index::NAME, index.clone()),
        ]));
        assert_eq!(signing.resized, 1);
        let served = tree.read(ampr_index::NAME).unwrap();
        assert_eq!(served.len(), index.len());
        let mut got: Vec<_> = ampr_index::parse(&served)
            .unwrap()
            .into_iter()
            .map(|e| (e.path, e.size))
            .collect();
        got.sort();
        let mut want = records(self_size);
        want.sort();
        assert_eq!(got, want);
        assert_eq!(
            tree.read_range(ampr_index::NAME, 3, 9).unwrap(),
            served[3..12]
        );
    }
}
