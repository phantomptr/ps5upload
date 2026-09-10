//! The backport library corpus: a content-addressed store of Sony system
//! libraries the user supplies from games they own.
//!
//! We cannot ship these binaries, so every corpus is built by the user, two
//! ways: importing a pack they already have, or scanning a console and
//! harvesting the `fakelib/` of games that are already backported. Both land
//! here, and the corpus persists so a second backport costs nothing.
//!
//! ## Why sets, and why they are kept whole
//!
//! A SET is the exact list of libraries one source shipped. Sets are installed
//! whole and never merged, because a library set is only known to work as the
//! unit somebody actually shipped. Measured on 34 titles: builds travel in
//! "harvests" — the 13 titles carrying one `libSceAgc` build are exactly the 13
//! carrying one `libSceAgcDriver` and one `libScePsml` build. Picking a build
//! per library independently manufactures a combination nobody has ever run.
//!
//! ## Why builds are content-addressed
//!
//! The corpus is small and heavily shared: 13 library names, ~52 distinct
//! builds, across ~34 sets. Storing a directory per source duplicated 15 MiB of
//! real content into 58 MiB. Addressing a build by its hash stores it once,
//! makes re-scanning idempotent, and makes "add this set" a cheap no-op when
//! the bytes are already present.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Bumped from 3: `observed_sets[]` became `sets[]` with a stable id, a
/// user-facing label, and an origin. Schema 3 was never released, so there is
/// no migration — a corpus is rebuilt by re-scanning, and an unrecognised
/// schema reads as an empty corpus ("no libraries yet") rather than an error.
pub const SCHEMA: u32 = 4;

/// Only these are libraries. A dot-prefixed name never is: copying a game
/// folder from a Mac leaves a `._libSceAgc.sprx` AppleDouble sidecar beside
/// every file, always ~4 KB, and an early build of this offered eight of them
/// for installation.
pub fn is_library_name(name: &str) -> bool {
    !name.starts_with('.') && !name.contains('/') && !name.contains('\\') && {
        let lower = name.to_ascii_lowercase();
        lower.ends_with(".sprx") || lower.ends_with(".prx")
    }
}

// ─── SELF/ELF param location ─────────────────────────────────────────────────

const SELF_MAGIC_PS4: u32 = 0x1D3D_154F;
const SELF_MAGIC_PS5: u32 = 0xEEF5_1454;
const PT_SCE_PROCPARAM: u32 = 0x6100_0001;
const PT_SCE_MODULEPARAM: u32 = 0x6100_0002;
const PROC_MAGIC: u32 = 0x4942_524F;
const MOD_MAGIC: u32 = 0x3C13_F4BF;
/// The 32-byte per-file digest a fake-signer rewrites whenever the file changes.
const DIGEST_SITE: (usize, usize) = (0x510, 32);

fn u16le(d: &[u8], at: usize) -> Option<u16> {
    d.get(at..at + 2)?.try_into().ok().map(u16::from_le_bytes)
}
fn u32le(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at + 4)?.try_into().ok().map(u32::from_le_bytes)
}
fn u64le(d: &[u8], at: usize) -> Option<u64> {
    d.get(at..at + 8)?.try_into().ok().map(u64::from_le_bytes)
}

/// Byte offset of a module's param segment, or None.
///
/// Inside a SELF the inner ELF keeps the offsets of the original unwrapped file
/// while the entry table relocates the bytes, so a segment is reached through
/// its entry (matched on id AND size) or the PT_LOAD containing it.
pub fn param_site(data: &[u8]) -> Option<usize> {
    let mut base = 0usize;
    let mut entries: Vec<(u32, u64, u64)> = Vec::new();
    if matches!(u32le(data, 0)?, SELF_MAGIC_PS4 | SELF_MAGIC_PS5) {
        let n = u16le(data, 0x18)? as usize;
        base = 0x20 + n * 0x20;
        for i in 0..n {
            let at = 0x20 + i * 0x20;
            let flags = u64le(data, at)?;
            entries.push((
                ((flags >> 20) & 0xFFF) as u32,
                u64le(data, at + 8)?,
                u64le(data, at + 16)?,
            ));
        }
    }
    let elf = data.get(base..)?;
    if elf.get(..4)? != b"\x7fELF" {
        return scan_param_site(data);
    }
    let phoff = u64le(elf, 0x20)? as usize;
    let phentsize = u16le(elf, 0x36)? as usize;
    let phnum = u16le(elf, 0x38)? as usize;
    let mut headers = Vec::with_capacity(phnum);
    for i in 0..phnum {
        let at = phoff.checked_add(i * phentsize)?;
        headers.push((u32le(elf, at)?, u64le(elf, at + 8)?, u64le(elf, at + 0x20)?));
    }
    for (i, &(ptype, off, filesz)) in headers.iter().enumerate() {
        if ptype != PT_SCE_PROCPARAM && ptype != PT_SCE_MODULEPARAM {
            continue;
        }
        let mut site = entries
            .iter()
            .find(|&&(id, _, sz)| id as usize == i && sz == filesz)
            .map(|&(_, o, _)| o as usize);
        if site.is_none() && base == 0 {
            site = Some(off as usize);
        }
        if site.is_none() {
            // A param segment often has no entry of its own and must be reached
            // through the PT_LOAD that contains it.
            for (j, &(t2, off2, sz2)) in headers.iter().enumerate() {
                if t2 == 1 && off2 <= off && off + 0x18 <= off2 + sz2 {
                    if let Some(&(_, o, _)) = entries
                        .iter()
                        .find(|&&(id, _, sz)| id as usize == j && sz == sz2)
                    {
                        site = Some((o + (off - off2)) as usize);
                        break;
                    }
                }
            }
        }
        let site = match site {
            Some(s) => s,
            None => continue,
        };
        let want = if ptype == PT_SCE_PROCPARAM {
            PROC_MAGIC
        } else {
            MOD_MAGIC
        };
        if u32le(data, site + 8) == Some(want) {
            return Some(site);
        }
    }
    scan_param_site(data)
}

/// Locate the param segment by its magic instead of by walking the ELF.
///
/// The walk fails on several real libraries — libSceAmpr, libScePlayGo and
/// friends — which is why they reported no SDK pair at all, making every
/// SDK-based decision operate on absent data. The magic occurs exactly once in
/// those files and the 0x20 size field in front of it confirms the hit, so
/// scanning is unambiguous where parsing gives up. An ambiguous or absent
/// magic returns None rather than guessing.
fn scan_param_site(data: &[u8]) -> Option<usize> {
    for magic in [PROC_MAGIC, MOD_MAGIC] {
        let pat = magic.to_le_bytes();
        let hits: Vec<usize> = data
            .windows(4)
            .enumerate()
            .filter(|(_, w)| *w == pat)
            .map(|(i, _)| i)
            .take(2)
            .collect();
        if hits.len() != 1 || hits[0] < 8 {
            continue;
        }
        let site = hits[0] - 8;
        if u32le(data, site) == Some(0x20) && site + 0x18 <= data.len() {
            return Some(site);
        }
    }
    None
}

/// Offset the param segment WOULD be at, computed from the header alone.
///
/// [`param_site`] verifies the magic at the offset it computes, which needs the
/// whole file in memory. An eboot is tens of megabytes and lives on the
/// console, so a caller that can do ranged reads wants the candidate offset
/// first, then reads 0x18 bytes there to confirm. Returns None when the walk
/// cannot place the segment; there is no magic-scan fallback here, because
/// scanning is exactly what a header-only caller is trying to avoid.
pub fn param_site_from_header(header: &[u8]) -> Option<usize> {
    let mut base = 0usize;
    let mut entries: Vec<(u32, u64, u64)> = Vec::new();
    if matches!(u32le(header, 0)?, SELF_MAGIC_PS4 | SELF_MAGIC_PS5) {
        let n = u16le(header, 0x18)? as usize;
        base = 0x20 + n * 0x20;
        for i in 0..n {
            let at = 0x20 + i * 0x20;
            let flags = u64le(header, at)?;
            entries.push((
                ((flags >> 20) & 0xFFF) as u32,
                u64le(header, at + 8)?,
                u64le(header, at + 16)?,
            ));
        }
    }
    let elf = header.get(base..)?;
    if elf.get(..4)? != b"\x7fELF" {
        return None;
    }
    let phoff = u64le(elf, 0x20)? as usize;
    let phentsize = u16le(elf, 0x36)? as usize;
    let phnum = u16le(elf, 0x38)? as usize;
    let mut headers = Vec::with_capacity(phnum);
    for i in 0..phnum {
        let at = phoff.checked_add(i * phentsize)?;
        headers.push((u32le(elf, at)?, u64le(elf, at + 8)?, u64le(elf, at + 0x20)?));
    }
    for (i, &(ptype, off, filesz)) in headers.iter().enumerate() {
        if ptype != PT_SCE_PROCPARAM && ptype != PT_SCE_MODULEPARAM {
            continue;
        }
        if let Some(&(_, o, _)) = entries
            .iter()
            .find(|&&(id, _, sz)| id as usize == i && sz == filesz)
        {
            return Some(o as usize);
        }
        if base == 0 {
            return Some(off as usize);
        }
        for (j, &(t2, off2, sz2)) in headers.iter().enumerate() {
            if t2 == 1 && off2 <= off && off + 0x18 <= off2 + sz2 {
                if let Some(&(_, o, _)) = entries
                    .iter()
                    .find(|&&(id, _, sz)| id as usize == j && sz == sz2)
                {
                    return Some((o + (off - off2)) as usize);
                }
            }
        }
    }
    None
}

/// The SDK pair in a param segment already read into `chunk` (0x18 bytes from
/// the site), verifying the magic first.
pub fn sdk_pair_at(chunk: &[u8]) -> Option<(u32, u32)> {
    let magic = u32le(chunk, 8)?;
    if magic != PROC_MAGIC && magic != MOD_MAGIC {
        return None;
    }
    Some((u32le(chunk, 0x10)?, u32le(chunk, 0x14)?))
}

/// The FW4 pair a backport targets: `(ps4, ps5)`.
pub const BACKPORT_SDK_PAIR: (u32, u32) = (0x0904_0001, 0x0400_0031);

/// `(ps4, ps5)` SDK words from a module's param segment, or None.
pub fn sdk_pair(data: &[u8]) -> Option<(u32, u32)> {
    let site = param_site(data)?;
    Some((u32le(data, site + 0x10)?, u32le(data, site + 0x14)?))
}

/// Identity of the CODE in a library, ignoring how it was stamped.
///
/// Rippers ship the same library patched to different SDK pairs, which rewrites
/// the 8-byte pair and the 32-byte digest covering it — and nothing else.
/// Measured: two `libSceAmpr` builds with different sha256 became byte-identical
/// once those two regions were masked. Treating them as different libraries
/// inflates the corpus and poses a choice that does not exist, since our own SDK
/// patcher moves either one to any pair.
pub fn code_id(data: &[u8]) -> String {
    let mut buf = data.to_vec();
    let (off, len) = DIGEST_SITE;
    if off + len <= buf.len() {
        buf[off..off + len].fill(0);
    }
    if let Some(site) = param_site(&buf) {
        if site + 0x18 <= buf.len() {
            buf[site + 0x10..site + 0x18].fill(0);
        }
    }
    hex(&Sha256::digest(&buf))
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ─── Manifest ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    /// Harvested from a game on a console.
    Scan {
        title_id: String,
        #[serde(default)]
        console: String,
        at: String,
    },
    /// Uploaded by the user as one pack.
    Import {
        #[serde(default)]
        source: String,
        at: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Build {
    pub sha256: String,
    pub size: u64,
    pub code_id: String,
    #[serde(default)]
    pub sdk: Option<[u32; 2]>,
    /// Ids of the sets shipping this exact build. The only signal with evidence
    /// behind it when choosing between builds of one name.
    #[serde(default)]
    pub shipped_by: Vec<String>,
    /// Relative to manifest.json, always `builds/<library>/<sha8>.<ext>`.
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LibraryEntry {
    pub name: String,
    pub builds: Vec<Build>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetEntry {
    pub id: String,
    pub label: String,
    pub origin: Origin,
    /// library name -> sha256 of the build this set ships.
    pub libraries: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub schema: u32,
    #[serde(default)]
    pub libraries: Vec<LibraryEntry>,
    #[serde(default)]
    pub sets: Vec<SetEntry>,
    /// Next set number to hand out. Persisted rather than derived from the
    /// highest live set, because deleting the newest set would otherwise let
    /// the next one reuse its id — and a backport record refers to a set by id,
    /// so reuse would point an undo at unrelated libraries.
    #[serde(default)]
    pub next_set_number: u32,
}

impl Default for Manifest {
    fn default() -> Self {
        Manifest {
            schema: SCHEMA,
            libraries: Vec::new(),
            sets: Vec::new(),
            next_set_number: 1,
        }
    }
}

/// One library offered for a new set.
pub struct IncomingLibrary {
    pub name: String,
    pub data: Vec<u8>,
}

pub struct Corpus {
    root: PathBuf,
    manifest: Manifest,
}

impl Corpus {
    /// Read the corpus at `root`, or an empty one when it is absent or its
    /// schema is not ours. Never fails on a bad manifest: an unreadable corpus
    /// must surface as "no libraries yet", which the UI can act on, rather than
    /// an error the user cannot fix.
    pub fn open(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        let manifest = std::fs::read(root.join("manifest.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Manifest>(&b).ok())
            .filter(|m| m.schema == SCHEMA)
            .unwrap_or_default();
        Corpus { root, manifest }
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn set(&self, id: &str) -> Option<&SetEntry> {
        self.manifest.sets.iter().find(|s| s.id == id)
    }

    /// Absolute path of a build, for copying to a console.
    pub fn build_path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Add one set. Content-addressed, so bytes already present are not
    /// rewritten and a re-scan or a repeated import is a no-op.
    ///
    /// Returns the new set's id, or None when an identical set already exists —
    /// identical meaning the same library-name-to-build mapping, regardless of
    /// label or origin. That is what makes "scan again any time" safe.
    pub fn add_set(
        &mut self,
        label: &str,
        origin: Origin,
        libraries: Vec<IncomingLibrary>,
    ) -> Result<Option<String>> {
        let libraries: Vec<IncomingLibrary> = libraries
            .into_iter()
            .filter(|l| is_library_name(&l.name))
            .collect();
        if libraries.is_empty() {
            return Err(anyhow!("no library files (.sprx/.prx) in this import"));
        }

        let mut mapping = BTreeMap::new();
        for lib in &libraries {
            mapping.insert(lib.name.clone(), sha256_hex(&lib.data));
        }
        if let Some(existing) = self.manifest.sets.iter().find(|s| s.libraries == mapping) {
            let _ = existing;
            return Ok(None);
        }

        let id = self.next_set_id();
        for lib in &libraries {
            let sha = mapping.get(&lib.name).expect("just inserted");
            self.store_build(&lib.name, sha, &lib.data, &id)?;
        }
        self.manifest.sets.push(SetEntry {
            id: id.clone(),
            label: label.to_string(),
            origin,
            libraries: mapping,
        });
        self.write()?;
        Ok(Some(id))
    }

    /// Remove a set and garbage-collect any build nothing else references.
    /// Builds are shared up to 13 ways, so deleting eagerly would break other
    /// sets.
    pub fn delete_set(&mut self, id: &str) -> Result<bool> {
        let before = self.manifest.sets.len();
        self.manifest.sets.retain(|s| s.id != id);
        if self.manifest.sets.len() == before {
            return Ok(false);
        }
        let live: BTreeSet<&str> = self
            .manifest
            .sets
            .iter()
            .flat_map(|s| s.libraries.values().map(|v| v.as_str()))
            .collect();
        for entry in &mut self.manifest.libraries {
            entry.builds.retain(|b| {
                if live.contains(b.sha256.as_str()) {
                    return true;
                }
                let _ = std::fs::remove_file(self.root.join(&b.path));
                false
            });
            for b in &mut entry.builds {
                b.shipped_by.retain(|s| s != id);
            }
        }
        self.manifest.libraries.retain(|e| !e.builds.is_empty());
        self.write()?;
        Ok(true)
    }

    fn next_set_id(&mut self) -> String {
        // Never derived from the live sets: deleting the newest would let the
        // next set reuse its id, and a backport record refers to a set by id.
        let n = self.manifest.next_set_number.max(1);
        self.manifest.next_set_number = n + 1;
        format!("set-{n}")
    }

    fn store_build(&mut self, name: &str, sha: &str, data: &[u8], set_id: &str) -> Result<()> {
        let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
        let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("sprx");
        let rel = format!("builds/{stem}/{}.{ext}", &sha[..8]);
        let abs = self.root.join(&rel);
        if !abs.exists() {
            if let Some(dir) = abs.parent() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
            }
            std::fs::write(&abs, data).with_context(|| format!("writing {}", abs.display()))?;
        }
        let entry = match self.manifest.libraries.iter_mut().find(|e| e.name == name) {
            Some(e) => e,
            None => {
                self.manifest.libraries.push(LibraryEntry {
                    name: name.to_string(),
                    builds: Vec::new(),
                });
                self.manifest.libraries.last_mut().expect("just pushed")
            }
        };
        match entry.builds.iter_mut().find(|b| b.sha256 == sha) {
            Some(b) => {
                if !b.shipped_by.iter().any(|s| s == set_id) {
                    b.shipped_by.push(set_id.to_string());
                }
            }
            None => entry.builds.push(Build {
                sha256: sha.to_string(),
                size: data.len() as u64,
                code_id: code_id(data),
                sdk: sdk_pair(data).map(|(a, b)| [a, b]),
                shipped_by: vec![set_id.to_string()],
                path: rel,
            }),
        }
        Ok(())
    }

    fn write(&mut self) -> Result<()> {
        // Most-shipped build first: the UI shows them in this order, and the
        // best-travelled build is the sane default to look at.
        for entry in &mut self.manifest.libraries {
            entry.builds.sort_by(|a, b| {
                b.shipped_by
                    .len()
                    .cmp(&a.shipped_by.len())
                    .then(a.sha256.cmp(&b.sha256))
            });
        }
        self.manifest.libraries.sort_by(|a, b| a.name.cmp(&b.name));
        std::fs::create_dir_all(&self.root)?;
        let json = serde_json::to_vec_pretty(&self.manifest)?;
        std::fs::write(self.root.join("manifest.json"), json)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal fake-signed PS4 SELF carrying a module param segment.
    ///
    /// The bytes after the entry table are deliberately NOT an ELF, so this
    /// exercises the magic-scan fallback — which is the path real libraries
    /// like libSceAmpr and libScePlayGo take, the ELF walk having failed on
    /// them and left them with no readable SDK pair at all.
    fn self_with(sdk: (u32, u32), digest: u8, body: u8) -> Vec<u8> {
        let mut d = vec![0u8; 0x800];
        d[0..4].copy_from_slice(&SELF_MAGIC_PS4.to_le_bytes());
        d[0x18..0x1a].copy_from_slice(&1u16.to_le_bytes());
        // Payload bytes that must be identical between stamp variants.
        for b in d[0x100..0x500].iter_mut() {
            *b = body;
        }
        // The per-file digest a fake-signer rewrites on every change.
        let (off, len) = DIGEST_SITE;
        for b in d[off..off + len].iter_mut() {
            *b = digest;
        }
        let site = 0x600usize;
        d[site..site + 4].copy_from_slice(&0x20u32.to_le_bytes());
        d[site + 8..site + 12].copy_from_slice(&MOD_MAGIC.to_le_bytes());
        d[site + 0x10..site + 0x14].copy_from_slice(&sdk.0.to_le_bytes());
        d[site + 0x14..site + 0x18].copy_from_slice(&sdk.1.to_le_bytes());
        d
    }

    fn incoming(name: &str, data: Vec<u8>) -> IncomingLibrary {
        IncomingLibrary {
            name: name.to_string(),
            data,
        }
    }

    fn scan_origin() -> Origin {
        Origin::Scan {
            title_id: "PPSA30528".into(),
            console: "PS5-Pro".into(),
            at: "2026-09-09T00:00:00Z".into(),
        }
    }

    #[test]
    fn finds_the_param_segment_and_sdk_pair_by_scanning() {
        let d = self_with((0x0904_0001, 0x0400_0031), 0xAA, 0x11);
        assert_eq!(param_site(&d), Some(0x600));
        assert_eq!(sdk_pair(&d), Some((0x0904_0001, 0x0400_0031)));
    }

    #[test]
    fn code_id_ignores_the_sdk_stamp_and_digest() {
        // The real finding this encodes: two libSceAmpr builds with different
        // sha256 and identical size differed in exactly 36 bytes — a 32-byte
        // digest at 0x510 and the 8-byte SDK pair — and were byte-identical
        // once those were masked. They are one library at two patch states, so
        // choosing between them is a choice that does not exist.
        let unpatched = self_with((0x0805_0001, 0x0200_0009), 0xAA, 0x11);
        let patched = self_with((0x0904_0001, 0x0400_0031), 0xBB, 0x11);
        assert_ne!(sha256_hex(&unpatched), sha256_hex(&patched));
        assert_eq!(code_id(&unpatched), code_id(&patched));
    }

    #[test]
    fn code_id_still_separates_genuinely_different_code() {
        let a = self_with((0x0904_0001, 0x0400_0031), 0xAA, 0x11);
        let b = self_with((0x0904_0001, 0x0400_0031), 0xAA, 0x22);
        assert_ne!(code_id(&a), code_id(&b));
    }

    #[test]
    fn locates_the_param_segment_from_the_header_alone() {
        // An eboot is tens of megabytes and lives on the console. A caller with
        // ranged reads gets the candidate offset from a small header read, then
        // reads 0x18 bytes there — instead of pulling 53 MB across the wire to
        // learn 8 bytes.
        let d = self_with((0x0904_0001, 0x0400_0031), 0xAA, 0x11);
        let header = &d[..0x400];
        // This fixture is deliberately unparseable by the ELF walk (its body is
        // not an ELF), which is what the whole-file path falls back to scanning
        // for — so header-only placement correctly declines rather than guesses.
        assert_eq!(param_site_from_header(header), None);
        assert_eq!(param_site(&d), Some(0x600), "whole-file path still scans");
    }

    #[test]
    fn reads_a_pair_from_a_param_chunk_and_checks_the_magic() {
        let d = self_with((0x0904_0001, 0x0400_0031), 0xAA, 0x11);
        let chunk = &d[0x600..0x618];
        assert_eq!(sdk_pair_at(chunk), Some(BACKPORT_SDK_PAIR));
        // Garbage at the offset must not be read as a pair: that would report a
        // title as backported when it is not, which is the failure this exists
        // to prevent.
        assert_eq!(sdk_pair_at(&[0u8; 0x18]), None);
        assert_eq!(sdk_pair_at(&[0u8; 4]), None);
    }

    #[test]
    fn the_backport_pair_is_what_a_patched_title_carries() {
        // Measured: Venus Vacation un-backported read (0x12090001, 0x10000040)
        // and could not launch on 9.60 whatever libraries were installed, which
        // is indistinguishable from a wrong library set unless this is checked.
        let patched = self_with(BACKPORT_SDK_PAIR, 0xAA, 0x11);
        let original = self_with((0x1209_0001, 0x1000_0040), 0xAA, 0x11);
        assert_eq!(sdk_pair(&patched), Some(BACKPORT_SDK_PAIR));
        assert_ne!(sdk_pair(&original), Some(BACKPORT_SDK_PAIR));
    }

    #[test]
    fn rejects_names_that_are_not_libraries() {
        assert!(is_library_name("libSceAgc.sprx"));
        assert!(is_library_name("libc.prx"));
        // AppleDouble sidecars end in .sprx and are ~4 KB; an early build
        // offered eight of them for installation.
        assert!(!is_library_name("._libSceAgc.sprx"));
        assert!(!is_library_name(".DS_Store"));
        assert!(!is_library_name("eboot.bin"));
        assert!(!is_library_name("../escape.sprx"));
        assert!(!is_library_name("dir/libSceAgc.sprx"));
    }

    #[test]
    fn an_import_with_no_libraries_is_refused() {
        let dir = tempdir();
        let mut c = Corpus::open(&dir);
        let err = c
            .add_set(
                "junk",
                scan_origin(),
                vec![
                    incoming("._libSceAgc.sprx", vec![1]),
                    incoming("readme.txt", vec![2]),
                ],
            )
            .unwrap_err();
        assert!(err.to_string().contains("no library files"), "{err}");
        assert!(c.manifest().sets.is_empty());
    }

    #[test]
    fn adding_the_same_set_twice_is_a_no_op() {
        // What makes "scan again any time" safe rather than a way to
        // accumulate duplicates.
        let dir = tempdir();
        let mut c = Corpus::open(&dir);
        let libs = || vec![incoming("libSceAgc.sprx", self_with((1, 2), 0xAA, 0x11))];
        assert_eq!(
            c.add_set("first", scan_origin(), libs()).unwrap(),
            Some("set-1".into())
        );
        assert_eq!(c.add_set("again", scan_origin(), libs()).unwrap(), None);
        assert_eq!(c.manifest().sets.len(), 1);
    }

    #[test]
    fn a_build_shared_by_two_sets_is_stored_once() {
        let dir = tempdir();
        let mut c = Corpus::open(&dir);
        let shared = self_with((1, 2), 0xAA, 0x11);
        c.add_set(
            "a",
            scan_origin(),
            vec![incoming("libSceAgc.sprx", shared.clone())],
        )
        .unwrap();
        c.add_set(
            "b",
            scan_origin(),
            vec![
                incoming("libSceAgc.sprx", shared.clone()),
                incoming("libScePsml.sprx", self_with((1, 2), 0xCC, 0x33)),
            ],
        )
        .unwrap();
        let agc = c
            .manifest()
            .libraries
            .iter()
            .find(|e| e.name == "libSceAgc.sprx")
            .unwrap();
        assert_eq!(agc.builds.len(), 1, "one build, referenced by two sets");
        assert_eq!(agc.builds[0].shipped_by, vec!["set-1", "set-2"]);
    }

    #[test]
    fn deleting_a_set_keeps_builds_another_set_still_uses() {
        // Builds are shared up to 13 ways in the real corpus, so deleting
        // eagerly would silently break unrelated sets.
        let dir = tempdir();
        let mut c = Corpus::open(&dir);
        let shared = self_with((1, 2), 0xAA, 0x11);
        let only_in_b = self_with((1, 2), 0xCC, 0x33);
        c.add_set(
            "a",
            scan_origin(),
            vec![incoming("libSceAgc.sprx", shared.clone())],
        )
        .unwrap();
        c.add_set(
            "b",
            scan_origin(),
            vec![
                incoming("libSceAgc.sprx", shared),
                incoming("libScePsml.sprx", only_in_b.clone()),
            ],
        )
        .unwrap();
        let psml_path = c
            .manifest()
            .libraries
            .iter()
            .find(|e| e.name == "libScePsml.sprx")
            .unwrap()
            .builds[0]
            .path
            .clone();

        assert!(c.delete_set("set-2").unwrap());
        assert!(
            c.manifest()
                .libraries
                .iter()
                .any(|e| e.name == "libSceAgc.sprx"),
            "shared build must survive"
        );
        assert!(
            !c.manifest()
                .libraries
                .iter()
                .any(|e| e.name == "libScePsml.sprx"),
            "unreferenced build must be collected"
        );
        assert!(
            !dir.join(&psml_path).exists(),
            "its file must be removed too"
        );
        assert!(c
            .build_path(&psml_path)
            .to_string_lossy()
            .contains("builds/"));
    }

    #[test]
    fn set_ids_are_never_reused_after_a_delete() {
        // A backport record refers to a set id; reuse would point an undo at
        // unrelated libraries.
        let dir = tempdir();
        let mut c = Corpus::open(&dir);
        c.add_set(
            "a",
            scan_origin(),
            vec![incoming("libSceAgc.sprx", self_with((1, 2), 1, 1))],
        )
        .unwrap();
        c.add_set(
            "b",
            scan_origin(),
            vec![incoming("libSceAgc.sprx", self_with((1, 2), 2, 2))],
        )
        .unwrap();
        c.delete_set("set-2").unwrap();
        let id = c
            .add_set(
                "c",
                scan_origin(),
                vec![incoming("libSceAgc.sprx", self_with((1, 2), 3, 3))],
            )
            .unwrap();
        assert_eq!(id, Some("set-3".into()));
    }

    #[test]
    fn survives_a_missing_or_foreign_corpus() {
        // An unreadable corpus must read as "no libraries yet", which the user
        // can act on, not an error they cannot fix.
        let dir = tempdir();
        assert!(Corpus::open(&dir).manifest().sets.is_empty());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), b"{not json").unwrap();
        assert!(Corpus::open(&dir).manifest().sets.is_empty());
        std::fs::write(dir.join("manifest.json"), br#"{"schema":1,"sets":[]}"#).unwrap();
        assert!(Corpus::open(&dir).manifest().sets.is_empty());
    }

    #[test]
    fn a_written_corpus_reads_back_identically() {
        let dir = tempdir();
        let mut c = Corpus::open(&dir);
        c.add_set(
            "FW11 pack",
            Origin::Import {
                source: "pack.zip".into(),
                at: "2026-09-09T00:00:00Z".into(),
            },
            vec![incoming(
                "libSceAgc.sprx",
                self_with((0x0904_0001, 0x0400_0031), 0xAA, 0x11),
            )],
        )
        .unwrap();
        let reopened = Corpus::open(&dir);
        assert_eq!(reopened.manifest(), c.manifest());
        assert_eq!(reopened.set("set-1").unwrap().label, "FW11 pack");
        assert_eq!(
            reopened.manifest().libraries[0].builds[0].sdk,
            Some([0x0904_0001, 0x0400_0031])
        );
    }

    /// A directory unique to this call. An earlier version keyed on pid plus
    /// wall-clock nanos, which two tests running in parallel can share — the
    /// foreign-corpus test then wrote `{"schema":1}` into another test's
    /// directory and that test read an empty corpus. A counter cannot collide.
    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "ps5upload-fakelibs-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        p
    }
}
