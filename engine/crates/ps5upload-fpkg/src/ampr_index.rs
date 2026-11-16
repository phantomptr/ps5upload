//! `ampr_emu.index`: the file table AMPR emulation reads from the image root.
//!
//! A title built against libSceAmpr resolves its files through AMPR; on a backported console
//! that is `ampr_emu`, which looks every `/app0` path up in this index (and otherwise has to
//! build one itself at startup). The format is `AMPRIDX3`, as `ampr_emu` 0.3.1 and later read it
//! (drakmor/ampr_emu, which validates every slot's hash against its path before trusting the
//! table):
//!
//! ```text
//! header   "AMPRIDX3", u32 3, u32 record size (24), u64 records, u64 path bytes,
//!          u64 slot table offset, u32 slot size (16), u32 slot count          (0x30 bytes)
//! records  {u32 path offset, u32 path length, u64 size, i64 mtime}, sorted by key
//! paths    "/app0/…" strings, each NUL-terminated
//! slots    {u64 hash, u32 record index + 1, u32 flags}, 16-aligned; open addressing from
//!          hash & (count − 1), count the least power of two ≥ twice the records
//! ```
//!
//! The key is the path with `\` as `/` and ASCII letters lowered; the hash is 64-bit FNV-1a of
//! the key with the offset basis `1469598103934665603` (0 is stored as 1). That basis is the
//! standard FNV-64 one with its last digit dropped, and it is what the format uses: `ampr_emu`
//! computes it at runtime and rejects a table hashed any other way. Every one of the 263 slots
//! in Spider-Man 2's released index reproduces with it. Flag 1 marks a slot whose hash another
//! path shares.

const MAGIC: &[u8; 8] = b"AMPRIDX3";
const RECORD: usize = 24;
const SLOT: usize = 16;
const HEADER: usize = 0x30;
const DUPLICATE: u32 = 1;

/// The lookup key: `\` as `/`, ASCII `A`–`Z` lowered, everything else as it is.
fn key(path: &str) -> Vec<u8> {
    path.bytes().map(fold).collect()
}

/// The format's FNV offset basis (see the module notes): not the standard `0xcbf29ce484222325`.
const BASIS: u64 = 1_469_598_103_934_665_603;

/// 64-bit FNV-1a of a path's key, from the format's basis.
pub fn hash(path: &str) -> u64 {
    hash_bytes(path.as_bytes())
}

/// [`hash`] of a path's bytes (the runtime hashes bytes; a path need not be UTF-8).
fn hash_bytes(path: &[u8]) -> u64 {
    let h = path.iter().fold(BASIS, |h, &b| {
        (h ^ u64::from(fold(b))).wrapping_mul(0x0100_0000_01b3)
    });
    h.max(1)
}

/// One byte of the key: `\` as `/`, ASCII `A`–`Z` lowered.
fn fold(b: u8) -> u8 {
    match b {
        b'\\' => b'/',
        b'A'..=b'Z' => b + 0x20,
        _ => b,
    }
}

// ─── The runtime's rules ────────────────────────────────────────────────────────────────────
//
// What ampr-emu-ng checks before it trusts an index (quer3q/ampr-emu-ng, GPL-3.0, like this
// project): `crates/ampr-format/src/index.rs` (`Layout::parse`, `Index::check`, `checked_path`,
// `kernel_path_limits`, `find`), `crates/ampr-format/src/lib.rs` (`logical_path`,
// `safe_components`) and `crates/ampr-format/src/view_join.rs` (`join_zstd`, `join_records`),
// as `docs/zstd-format.md` "What the runtime needs from ampr_emu.index" lists them. One record
// that fails makes the whole file invalid: a title without packs then gets an index the runtime
// builds from the folder, and a title with packs has none of its packed files served.

/// SCE_KERNEL_PATH_MAX: every stored path is shorter than this, `/app0/` included.
pub const PATH_MAX: usize = 1024;
/// SCE_KERNEL_NAME_MAX: the longest name part of a path, in bytes.
pub const NAME_MAX: usize = 255;
/// The runtime's limits on the header (`MAX_RECORDS`, `MAX_PATH_BYTES`).
const MAX_RECORDS: u64 = 2_000_000;
const MAX_PATH_BYTES: u64 = 256 << 20;
/// Hash-table steps [`check`] takes per record on average before it refuses the table.
const MAX_PROBES_PER_RECORD: usize = 64;

/// Why the runtime would refuse `path` (an index path, `/app0/...`), or `None`. Bytes, as the
/// runtime counts them: a name of 100 CJK characters is 300 bytes, over [`NAME_MAX`].
pub fn path_problem(path: &[u8]) -> Option<String> {
    if path.len() >= PATH_MAX {
        return Some(format!(
            "the path is {} bytes long; the AMPR runtime takes paths under {PATH_MAX} bytes \
             (with /app0/)",
            path.len()
        ));
    }
    if let Some(part) = path.split(|&b| b == b'/').find(|p| p.len() > NAME_MAX) {
        return Some(format!(
            "a name in the path is {} bytes long; the AMPR runtime takes names of at most \
             {NAME_MAX} bytes",
            part.len()
        ));
    }
    let app0 = path.len() > 6
        && path[0] == b'/'
        && path[5] == b'/'
        && path[1..5].iter().map(|&b| fold(b)).eq(*b"app0");
    if !app0 {
        return Some("the path does not start with /app0/".into());
    }
    if path.contains(&0) {
        return Some("the path holds a NUL byte".into());
    }
    if path.contains(&b'\\') {
        return Some("the path holds a backslash (\\)".into());
    }
    if path[6..]
        .split(|&b| b == b'/')
        .any(|p| p.is_empty() || p == b"." || p == b"..")
    {
        return Some("the path has an empty, \".\" or \"..\" part".into());
    }
    None
}

/// What [`check`] found in an index the runtime accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checked {
    /// Records in it.
    pub records: usize,
    /// Records in folded order: the runtime pages such an index without building a rank order.
    pub sorted: bool,
}

/// Whether two paths are equal with `A`-`Z` folded to `a`-`z` (and `\` to `/`).
fn folded_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(&x, &y)| fold(x) == fold(y))
}

/// The runtime's checks of an `ampr_emu.index` (ampr-emu-ng `Index::open`: `Layout::parse`, then
/// `Index::check`), or the first reason it would refuse the file, in plain words. Every record's
/// path keeps the rules of [`path_problem`] and is NUL-terminated in the path blob; every slot
/// carries its record's hash; every record is the lookup's answer for its own path, so no two
/// paths are equal after folding letter case.
pub fn check(d: &[u8]) -> Result<Checked, String> {
    fn bad<T>(why: &str) -> Result<T, String> {
        Err(format!("{NAME} is not one the AMPR runtime accepts: {why}"))
    }
    let u32_at = |at: usize| {
        d.get(at..at.checked_add(4)?)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    };
    let u64_at = |at: usize| {
        d.get(at..at.checked_add(8)?)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
    };
    if d.len() < HEADER || &d[..8] != MAGIC {
        return bad("it has no AMPRIDX3 header");
    }
    let field = |at: usize| u32_at(at).unwrap_or(0);
    if field(8) != 3 || field(12) as usize != RECORD || field(40) as usize != SLOT {
        return bad("its header names another version or record size");
    }
    let count = u64_at(0x10).unwrap_or(0);
    let path_bytes = u64_at(0x18).unwrap_or(0);
    let slots_at = u64_at(0x20).unwrap_or(0);
    let slot_count = field(0x2C);
    if count == 0 {
        return bad("it lists no file");
    }
    if count > MAX_RECORDS || !(1..=MAX_PATH_BYTES).contains(&path_bytes) {
        return bad("it lists more files or path bytes than the runtime takes");
    }
    if slot_count < 2 || !slot_count.is_power_of_two() || u64::from(slot_count) < count {
        return bad("its hash table is not a power of two at least as large as its records");
    }
    // Bounded above (count and path_bytes are capped), but every sum and product of the file's
    // own numbers is checked: a hostile index names a table at the end of the address space.
    let paths_at = HEADER as u64 + count * RECORD as u64;
    if paths_at
        .checked_add(path_bytes)
        .is_none_or(|end| slots_at < end)
        || slots_at % 8 != 0
    {
        return bad("its hash table overlaps its paths or is not 8-byte aligned");
    }
    let file_len = u64::from(slot_count)
        .checked_mul(SLOT as u64)
        .and_then(|t| t.checked_add(slots_at));
    if file_len.is_none_or(|end| (d.len() as u64) < end) {
        return bad("it is shorter than its header says");
    }
    // Within `d` from here (its length is a usize): the casts and the sums below fit.
    let (count, paths_at, path_bytes) = (count as usize, paths_at as usize, path_bytes as usize);
    let (slots_at, slot_count) = (slots_at as usize, slot_count as usize);
    // Record `id` (one-based): its path without the NUL, and its size.
    let record = |id: usize| -> Result<&[u8], String> {
        let r = HEADER + (id - 1) * RECORD;
        let (off, len) = (field(r) as usize, field(r + 4) as usize);
        match off.checked_add(len) {
            Some(end) if len < PATH_MAX && end < path_bytes => {
                Ok(&d[paths_at + off..paths_at + end])
            }
            _ => Err(format!("record {id}'s path is out of bounds or too long")),
        }
    };
    let shown = |p: &[u8]| String::from_utf8_lossy(p).into_owned();
    let mut sorted = true;
    let mut previous: Option<&[u8]> = None;
    for id in 1..=count {
        let path = record(id).or_else(|e| bad(&e))?;
        let r = HEADER + (id - 1) * RECORD;
        let end = paths_at + field(r) as usize + path.len();
        if d[end] != 0 {
            return bad(&format!("{}: its path has no NUL after it", shown(path)));
        }
        if let Some(why) = path_problem(path) {
            return bad(&format!("{}: {why}", shown(path)));
        }
        if previous.is_some_and(|p| p.iter().map(|&b| fold(b)).gt(path.iter().map(|&b| fold(b)))) {
            sorted = false;
        }
        previous = Some(path);
    }
    // `pos` < slot_count: inside `d` (checked above). Checked anyway; out of range reads empty.
    let slot = |pos: usize| {
        let Some(at) = pos.checked_mul(SLOT).and_then(|o| o.checked_add(slots_at)) else {
            return (0, 0, 0);
        };
        (
            u64_at(at).unwrap_or(0),
            at.checked_add(8).map_or(0, field) as usize,
            at.checked_add(12).map_or(0, field),
        )
    };
    let mut occupied = 0usize;
    for pos in 0..slot_count {
        let (hash, id, flags) = slot(pos);
        if id == 0 {
            if hash != 0 || flags != 0 {
                return bad("an empty hash slot is not zero");
            }
            continue;
        }
        if id > count || flags & !DUPLICATE != 0 {
            return bad("a hash slot names no record or carries unknown flags");
        }
        let path = record(id).or_else(|e| bad(&e))?;
        if hash != hash_bytes(path) {
            return bad(&format!(
                "{}: its hash slot holds another hash",
                shown(path)
            ));
        }
        occupied += 1;
    }
    if occupied != count {
        return bad("its hash table does not hold every record once");
    }
    let mask = slot_count - 1;
    // Every lookup walks its run of full slots: a table the runtime's builders write (at most
    // half full) takes a step or two each. One whose records all collide would take count² steps
    // (hours for two million): refused past a budget no real index comes near.
    let mut budget = count.saturating_mul(MAX_PROBES_PER_RECORD);
    for id in 1..=count {
        let path = record(id).or_else(|e| bad(&e))?;
        let h = hash_bytes(path);
        let mut pos = h as usize & mask;
        let mut found = None;
        for _ in 0..slot_count {
            if budget == 0 {
                return bad("its hash table is too crowded to look every file up");
            }
            budget -= 1;
            let (slot_hash, slot_id, _) = slot(pos);
            if slot_id == 0 {
                break;
            }
            if slot_hash == h && folded_eq(record(slot_id).or_else(|e| bad(&e))?, path) {
                found = Some(slot_id);
                break;
            }
            pos = (pos + 1) & mask;
        }
        match found {
            Some(f) if f == id => {}
            Some(f) => {
                return bad(&format!(
                    "{} and {} differ only in letter case",
                    shown(record(f).or_else(|e| bad(&e))?),
                    shown(path)
                ))
            }
            None => return bad(&format!("{} is not found through its hash", shown(path))),
        }
    }
    Ok(Checked {
        records: count,
        sorted,
    })
}

/// The runtime's join of `ampr_zstd.index` to the index (ampr-emu-ng `join_zstd`): each packed
/// file, `(path relative to /app0, logical size)` as `ampr_zstd.index` lists it, needs a record
/// whose path is `/app0/` plus its own (letter case aside) at the same size. A packed file with
/// no record is never served; one at another size makes the runtime serve no packed file at
/// all; of two that differ only in case, the second is never served. `index` must pass
/// [`check`].
pub fn check_zstd<'a>(
    index: &[u8],
    packed: impl IntoIterator<Item = (&'a str, u64)>,
) -> Result<(), String> {
    let entries = parse(index).ok_or_else(|| format!("{NAME} does not read"))?;
    let sizes: std::collections::HashMap<String, u64> = entries
        .into_iter()
        .map(|e| (e.path.to_ascii_lowercase(), e.size))
        .collect();
    let mut seen = std::collections::HashSet::new();
    for (path, size) in packed {
        let key = path.to_ascii_lowercase();
        match sizes.get(&key) {
            None => {
                return Err(format!(
                    "{path} is in the zstd packs but not in {NAME}, so the game could not open \
                     it"
                ))
            }
            Some(&listed) if listed != size => {
                return Err(format!(
                    "{path} is {size} bytes in ampr_zstd.index and {listed} in {NAME}; the AMPR \
                     runtime then serves no packed file"
                ))
            }
            Some(_) => {}
        }
        if !seen.insert(key) {
            return Err(format!(
                "two files in the zstd packs differ from {path} only in letter case; the second \
                 would never be served"
            ));
        }
    }
    Ok(())
}

/// The runtime's join of an LZ4 manifest (`ampr_assets.index`, AMPRPAK4) to the index
/// (ampr-emu-ng `join_records`): manifest record `i` is index record `i + 1`, and a packed one
/// has that record's size and the hash of its path. A packed record that does not match is never
/// served. `index` must pass [`check`].
pub fn check_lz4(
    index: &[u8],
    manifest: &[crate::ampr_pack::format::FileRecord],
) -> Result<(), String> {
    let entries = parse(index).ok_or_else(|| format!("{NAME} does not read"))?;
    for (i, f) in manifest.iter().enumerate().filter(|(_, f)| f.packed()) {
        let Some(e) = entries.get(i) else {
            return Err(format!(
                "the LZ4 manifest has a packed file at record {} and {NAME} only {} records",
                i + 1,
                entries.len()
            ));
        };
        let path = format!("/app0/{}", e.path);
        if f.logical_size != e.size || f.path_hash != crate::ampr_pack::format::path_hash(&path) {
            return Err(format!(
                "the LZ4 manifest's record {} is not {path} at {} bytes, as {NAME} lists it; the \
                 game could not open that file",
                i + 1,
                e.size
            ));
        }
    }
    Ok(())
}

/// The files `ampr_zstd.index` (AMPRZPK1) lists, `(path relative to /app0, logical size)`, read
/// from its header and file records alone; `None` when it does not read as one. For
/// [`check_zstd`] where the zstd reader is not at hand (this crate's checks of a game).
pub fn zstd_files(d: &[u8]) -> Option<Vec<(String, u64)>> {
    let u64_at = |at: usize| Some(u64::from_le_bytes(d.get(at..at + 8)?.try_into().ok()?));
    if d.get(..8)? != b"AMPRZPK1" {
        return None;
    }
    let count = usize::try_from(u64_at(40)?).ok()?;
    let files_at = usize::try_from(u64_at(56)?).ok()?;
    let strings_at = usize::try_from(u64_at(80)?).ok()?;
    let strings = d.get(strings_at..strings_at.checked_add(usize::try_from(u64_at(88)?).ok()?)?)?;
    let mut out = Vec::with_capacity(count.min(d.len() / 32));
    for i in 0..count {
        let r = d
            .get(files_at.checked_add(i.checked_mul(32)?)?..)?
            .get(..32)?;
        let size = u64::from_le_bytes(r[8..16].try_into().ok()?);
        let off = u32::from_le_bytes(r[20..24].try_into().ok()?) as usize;
        let len = u16::from_le_bytes(r[24..26].try_into().ok()?) as usize;
        let path = std::str::from_utf8(strings.get(off..off.checked_add(len)?)?).ok()?;
        out.push((path.to_string(), size));
    }
    Some(out)
}

/// Gives the record of `from` (a path relative to `/app0`) the spelling `to`, which differs from
/// it only in ASCII letter case: same length, same hash, same place in the folded order, so the
/// bytes are changed where they are and nothing else moves. Whether a record was changed.
pub fn respell(d: &mut [u8], from: &str, to: &str) -> bool {
    if from.len() != to.len() || !from.eq_ignore_ascii_case(to) || d.get(..8) != Some(MAGIC) {
        return false;
    }
    let u32_at = |d: &[u8], at: usize| {
        d.get(at..at + 4)
            .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap())) as usize
    };
    let u64_at = |d: &[u8], at: usize| {
        d.get(at..at + 8)
            .map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap())) as usize
    };
    let count = u64_at(d, 0x10);
    let Some(paths_at) = count
        .checked_mul(RECORD)
        .and_then(|n| n.checked_add(HEADER))
    else {
        return false;
    };
    let mut changed = false;
    for i in 0..count {
        let r = HEADER + i * RECORD;
        if r + RECORD > d.len() {
            break;
        }
        let (off, len) = (u32_at(d, r), u32_at(d, r + 4));
        let at = paths_at.saturating_add(off);
        let Some(path) = d.get_mut(at..at.saturating_add(len)) else {
            continue;
        };
        if len == from.len() + 6
            && folded_eq(&path[6..], from.as_bytes())
            && &path[6..] != to.as_bytes()
        {
            path[6..].copy_from_slice(to.as_bytes());
            changed = true;
        }
    }
    changed
}

/// Where record `i`'s size sits in an index (a plain u64, no checksum over it).
pub(crate) fn size_at(i: usize) -> usize {
    HEADER + i * RECORD + 8
}

/// The index's name at the image root.
pub const NAME: &str = "ampr_emu.index";

/// One record: a path inside the image (no `/app0/` prefix), its size and mtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub size: u64,
    pub mtime: i64,
}

/// The index for `files`, as `(path inside the image, size)`; every entry gets `mtime`.
/// Fails, naming both, when two paths differ only in case, which the index cannot tell apart.
pub fn build(files: &[(String, u64)], mtime: i64) -> Result<Vec<u8>, String> {
    let entries: Vec<Entry> = files
        .iter()
        .map(|(path, size)| Entry {
            path: path.clone(),
            size: *size,
            mtime,
        })
        .collect();
    build_entries(&entries)
}

/// The index for `entries`, each with its own mtime. Fails, in plain words, on anything the AMPR
/// runtime would refuse the whole index for ([`check`]): no file, a path it does not take
/// ([`path_problem`]), two paths that differ only in case.
pub fn build_entries(entries: &[Entry]) -> Result<Vec<u8>, String> {
    if entries.is_empty() {
        return Err(format!(
            "{NAME} would list no file, which the AMPR runtime refuses"
        ));
    }
    let mut rows: Vec<(Vec<u8>, String, u64, i64)> = entries
        .iter()
        .map(|e| {
            let path = format!("/app0/{}", e.path.trim_start_matches('/'));
            (key(&path), path, e.size, e.mtime)
        })
        .collect();
    if let Some((path, why)) = rows
        .iter()
        .find_map(|r| path_problem(r.1.as_bytes()).map(|why| (&r.1, why)))
    {
        return Err(format!(
            "{path}: {why}, and one such file makes it refuse the whole {NAME}; rename the file \
             or leave it out"
        ));
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    if let Some(w) = rows.windows(2).find(|w| w[0].0 == w[1].0) {
        return Err(format!(
            "{} and {} differ only in case, which {NAME} cannot tell apart; rename one",
            w[0].1, w[1].1
        ));
    }
    let mut records = Vec::with_capacity(rows.len() * RECORD);
    let mut paths = Vec::new();
    for (_, path, size, mtime) in &rows {
        records.extend_from_slice(&(paths.len() as u32).to_le_bytes());
        records.extend_from_slice(&(path.len() as u32).to_le_bytes());
        records.extend_from_slice(&size.to_le_bytes());
        records.extend_from_slice(&mtime.to_le_bytes());
        paths.extend_from_slice(path.as_bytes());
        paths.push(0);
    }
    let count = (rows.len() * 2).next_power_of_two().max(2);
    let mask = count - 1;
    let mut slots = vec![(0u64, 0u32, 0u32); count];
    for (i, (_, path, _, _)) in rows.iter().enumerate() {
        let h = hash(path);
        let mut at = h as usize & mask;
        let mut shared = false;
        while slots[at].1 != 0 {
            if slots[at].0 == h {
                slots[at].2 |= DUPLICATE;
                shared = true;
            }
            at = (at + 1) & mask;
        }
        slots[at] = (h, i as u32 + 1, if shared { DUPLICATE } else { 0 });
    }
    let slots_at = (HEADER + records.len() + paths.len()).next_multiple_of(SLOT);
    let mut out = Vec::with_capacity(slots_at + count * SLOT);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&(RECORD as u32).to_le_bytes());
    out.extend_from_slice(&(rows.len() as u64).to_le_bytes());
    out.extend_from_slice(&(paths.len() as u64).to_le_bytes());
    out.extend_from_slice(&(slots_at as u64).to_le_bytes());
    out.extend_from_slice(&(SLOT as u32).to_le_bytes());
    out.extend_from_slice(&(count as u32).to_le_bytes());
    out.extend_from_slice(&records);
    out.extend_from_slice(&paths);
    out.resize(slots_at, 0);
    for (h, index, flags) in slots {
        out.extend_from_slice(&h.to_le_bytes());
        out.extend_from_slice(&index.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
    }
    // Every index made here passes the runtime's own checks before anything writes it.
    check(&out)?;
    Ok(out)
}

/// The records of an index in file-id order (record `i` is file id `i + 1`), each path with
/// its `/app0/` taken off. `None` when the bytes are not a well-formed `AMPRIDX3`.
pub fn parse(d: &[u8]) -> Option<Vec<Entry>> {
    let u32_at = |at: usize| Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?));
    let u64_at = |at: usize| Some(u64::from_le_bytes(d.get(at..at + 8)?.try_into().ok()?));
    if d.get(..8)? != MAGIC || u32_at(8)? != 3 || u32_at(12)? as usize != RECORD {
        return None;
    }
    let n = usize::try_from(u64_at(0x10)?).ok()?;
    let path_bytes = usize::try_from(u64_at(0x18)?).ok()?;
    let paths_at = HEADER.checked_add(n.checked_mul(RECORD)?)?;
    let paths = d.get(paths_at..paths_at.checked_add(path_bytes)?)?;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let r = HEADER + i * RECORD;
        let (off, len) = (u32_at(r)? as usize, u32_at(r + 4)? as usize);
        if off.checked_add(len)? >= paths.len() || paths[off + len] != 0 {
            return None;
        }
        let path = std::str::from_utf8(&paths[off..off + len]).ok()?;
        let path = path.replace('\\', "/");
        let rel = path
            .get(6..)
            .filter(|_| path[..6].eq_ignore_ascii_case("/app0/"))?;
        out.push(Entry {
            path: rel.to_string(),
            size: u64_at(r + 8)?,
            mtime: u64_at(r + 16)? as i64,
        });
    }
    Some(out)
}

/// Where a rebuild keeps the index it replaced, once: a second rebuild leaves it alone, so it is
/// always the game's own (#621).
pub const BACKUP: &str = "ampr_emu.index.bak";

/// What an index lists for a game's `files`: every file at its size, but the index itself. A
/// game image's build (the engine's `AmprIndexed`) and [`rebuild`] both list this, so an index
/// rebuilt in place is the one the game's image would carry.
pub fn listing(files: &[crate::source::SourceFile]) -> Vec<(String, u64)> {
    files
        .iter()
        .filter(|f| !f.path.eq_ignore_ascii_case(NAME))
        .map(|f| (f.path.clone(), f.size))
        .collect()
}

/// Why an index is not rebuilt for a game in LZ4 packs: the runtime ties manifest record `i` to
/// index record `i + 1` ([`check_lz4`]), so an index made again could leave packed files unserved.
pub const LZ4_PACKED: &str = "this game's files are in LZ4 asset packs, whose manifest is tied \
                              to its ampr_emu.index record by record; convert the game again \
                              to get a fresh index";

/// Whether `path` is `ampr_zstd.index` or one of its volumes at the game's root.
fn zstd_pack_file(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    !p.contains('/')
        && (p == "ampr_zstd.index" || (p.starts_with("ampr_zstd-") && p.ends_with(".pak")))
}

/// What the index of a game in zstd packs lists, as the packer made it: every file on disk but
/// the index and the packs, and every packed file at the size `ampr_zstd.index` gives it (the
/// runtime refuses the packs when the two disagree).
pub fn zstd_listing(
    tree: &mut dyn crate::source::SourceTree,
) -> Result<Vec<(String, u64)>, String> {
    let packed = zstd_packed(tree)?;
    Ok(zstd_listing_of(tree.files(), packed))
}

/// The files the game's `ampr_zstd.index` lists ([`zstd_files`]).
pub fn zstd_packed(tree: &mut dyn crate::source::SourceTree) -> Result<Vec<(String, u64)>, String> {
    let at = tree
        .files()
        .iter()
        .find(|f| f.path.eq_ignore_ascii_case("ampr_zstd.index"))
        .map(|f| f.path.clone())
        .ok_or("the game has no ampr_zstd.index")?;
    let bytes = crate::source::read_small(tree, &at, crate::source::MAX_INDEX_BYTES)
        .map_err(|e| format!("{at}: {e}"))?;
    zstd_files(&bytes).ok_or_else(|| format!("{at} does not read"))
}

/// [`zstd_listing`] of a game's `files` and the `packed` files its `ampr_zstd.index` lists.
pub fn zstd_listing_of(
    files: &[crate::source::SourceFile],
    packed: Vec<(String, u64)>,
) -> Vec<(String, u64)> {
    // A file both on disk and in the packs is one record, the packed one (the runtime serves
    // that record from the packs).
    let in_packs: std::collections::HashSet<String> =
        packed.iter().map(|(p, _)| p.to_ascii_lowercase()).collect();
    let loose: Vec<crate::source::SourceFile> = files
        .iter()
        .filter(|f| !zstd_pack_file(&f.path) && !in_packs.contains(&f.path.to_ascii_lowercase()))
        .cloned()
        .collect();
    let mut listed = listing(&loose);
    listed.extend(packed);
    listed
}

/// Why an index is not rebuilt for a game that does not read through AMPR.
pub const NOT_AMPR: &str = "this game's eboot.bin does not import libSceAmpr, so ampr_emu \
                            never reads an ampr_emu.index for it";

/// An index regenerated for a game as it stands (Workshop's "Rebuild AMPR index", #621).
#[derive(Debug)]
pub struct Rebuilt {
    /// The new `ampr_emu.index`.
    pub index: Vec<u8>,
    /// How many files it lists.
    pub files: usize,
    /// The index the game had, and its path in the game (its own spelling), when it had one.
    pub old: Option<(String, Vec<u8>)>,
    /// Keep the old index as [`BACKUP`]: it had one and there is no backup yet. The new index
    /// lists the backup, which is in the folder once it is written.
    pub save_backup: bool,
    /// The new index lists other files or sizes than the old one did (mtimes and the backup
    /// aside: every rebuild stamps its own); true when there was none or it was unreadable.
    pub differs: bool,
}

/// Regenerates the index of the game in `tree`, every file stamped `mtime`, without writing
/// anything: the caller puts [`Rebuilt::index`] at the game's root (and the backup beside it).
/// Refused ([`NOT_AMPR`]) unless `eboot.bin` imports libSceAmpr, the same test a build uses.
pub fn rebuild(tree: &mut dyn crate::source::SourceTree, mtime: i64) -> Result<Rebuilt, String> {
    // A game in packs is read through the index whatever its eboot imports (ampr-emu-ng
    // serves zstd packs of a game without libSceAmpr too).
    let packs = crate::source::packed_kind(tree).map_err(|e| e.to_string())?;
    if packs.is_none() && !crate::source::imports_ampr(tree, "eboot.bin") {
        return Err(NOT_AMPR.to_string());
    }
    let old_path = tree
        .files()
        .iter()
        .find(|f| f.path.eq_ignore_ascii_case(NAME))
        .map(|f| f.path.clone());
    let old = match old_path {
        Some(p) => Some((
            p.clone(),
            crate::source::read_small(tree, &p, crate::source::MAX_INDEX_BYTES)
                .map_err(|e| format!("{p}: {e}"))?,
        )),
        None => None,
    };
    let has_backup = tree
        .files()
        .iter()
        .any(|f| f.path.eq_ignore_ascii_case(BACKUP));
    let save_backup = old.is_some() && !has_backup;
    let mut listed = match packs {
        None => listing(tree.files()),
        Some("zstd") => zstd_listing(tree)?,
        Some(_) => return Err(LZ4_PACKED.to_string()),
    };
    if save_backup {
        let len = old.as_ref().map_or(0, |(_, d)| d.len() as u64);
        listed.push((BACKUP.to_string(), len));
    }
    let index = build(&listed, mtime)?;
    // The game's files, the backup aside: keeping the old index is not a difference in it.
    let shape = |rows: &mut dyn Iterator<Item = (String, u64)>| {
        let mut v: Vec<(String, u64)> = rows
            .filter(|(p, _)| !p.eq_ignore_ascii_case(BACKUP))
            .map(|(p, s)| (p.to_ascii_lowercase(), s))
            .collect();
        v.sort();
        v
    };
    let differs = match old.as_ref().and_then(|(_, d)| parse(d)) {
        None => true,
        Some(before) => {
            shape(&mut before.into_iter().map(|e| (e.path, e.size)))
                != shape(&mut listed.iter().cloned())
        }
    };
    Ok(Rebuilt {
        files: listed.len(),
        index,
        old,
        save_backup,
        differs,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn u32_at(d: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(d[at..at + 4].try_into().unwrap())
    }
    fn u64_at(d: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(d[at..at + 8].try_into().unwrap())
    }

    /// Slots from Spider-Man 2's released `ampr_emu.index`, which `ampr_emu` accepts.
    #[test]
    fn the_hash_reproduces_a_released_index() {
        assert_eq!(hash("/app0/toc"), 0x3489_25d2_02b6_4400);
        assert_eq!(hash("/app0/d/materialgraph"), 0xedd2_eedc_1022_d002);
        assert_eq!(hash("/app0/commandline.txt"), 0x7fcf_babf_58b3_482b);
        assert_eq!(
            hash("/app0/sce_sys/trophy2/trophy00.ucp"),
            0xf487_434e_de06_e42f
        );
        assert_eq!(hash(""), BASIS);
        assert_eq!(hash("a"), 0x44bd8ad473cd9906);
        assert_eq!(hash("/APP0/Toc"), hash("/app0/toc"));
        assert_eq!(hash("\\app0\\toc"), hash("/app0/toc"));
    }

    /// Every record is found from its own hash by the runtime's probe, the table is at least
    /// twice the records, and the layout fields agree with the bytes.
    #[test]
    fn every_record_is_reachable_through_its_slot() {
        let files: Vec<(String, u64)> = (0..300)
            .map(|i| (format!("d/Part{i:03}.bin"), i as u64 * 7))
            .chain([
                ("eboot.bin".to_string(), 99),
                ("sce_sys/param.json".to_string(), 5),
            ])
            .collect();
        let d = build(&files, 1_777_000_000).unwrap();
        assert_eq!(&d[..8], MAGIC);
        let n = u64_at(&d, 0x10) as usize;
        let path_bytes = u64_at(&d, 0x18) as usize;
        let slots_at = u64_at(&d, 0x20) as usize;
        let count = u32_at(&d, 0x2C) as usize;
        assert_eq!(n, files.len());
        assert_eq!(count, 1024);
        assert_eq!(slots_at % 16, 0);
        assert_eq!(d.len(), slots_at + count * 16);
        let paths_at = HEADER + n * RECORD;
        assert!(paths_at + path_bytes <= slots_at);
        let path_of = |i: usize| {
            let r = HEADER + i * RECORD;
            let (off, len) = (u32_at(&d, r) as usize, u32_at(&d, r + 4) as usize);
            assert_eq!(d[paths_at + off + len], 0);
            std::str::from_utf8(&d[paths_at + off..paths_at + off + len]).unwrap()
        };
        let mut previous = Vec::new();
        for i in 0..n {
            let p = path_of(i);
            assert!(p.starts_with("/app0/"));
            assert!(key(p) > previous, "records sorted by key");
            previous = key(p);
            let h = hash(p);
            let mut at = h as usize & (count - 1);
            loop {
                let s = slots_at + at * 16;
                assert_ne!(u32_at(&d, s + 8), 0, "{p} not reachable");
                if u64_at(&d, s) == h && u32_at(&d, s + 8) as usize == i + 1 {
                    break;
                }
                at = (at + 1) & (count - 1);
            }
        }
        let size_of_eboot = (0..n)
            .find(|&i| path_of(i) == "/app0/eboot.bin")
            .map(|i| u64_at(&d, HEADER + i * RECORD + 8));
        assert_eq!(size_of_eboot, Some(99));

        // The reader gives the records back in file-id order, prefix off.
        let parsed = parse(&d).unwrap();
        assert_eq!(parsed.len(), n);
        for (i, e) in parsed.iter().enumerate() {
            assert_eq!(format!("/app0/{}", e.path), path_of(i));
            assert_eq!(e.mtime, 1_777_000_000);
        }
    }

    #[test]
    fn paths_differing_only_in_case_are_refused() {
        let files = vec![("A.bin".to_string(), 1), ("a.bin".to_string(), 2)];
        let e = build(&files, 0).unwrap_err();
        assert!(
            e.contains("/app0/A.bin") && e.contains("/app0/a.bin"),
            "{e}"
        );
    }

    #[test]
    fn a_damaged_index_does_not_parse() {
        let d = build(&[("a.bin".to_string(), 1)], 0).unwrap();
        assert!(parse(&d).is_some());
        assert!(parse(&d[..0x30]).is_none());
        let mut bad = d.clone();
        bad[7] = b'2';
        assert!(parse(&bad).is_none());
    }

    /// An AMPRIDX3 of `paths` as they are (sizes 10, 20, ...), with none of [`build`]'s checks:
    /// ampr-format's own test image (`index.rs` `tests::image`), to feed [`check`] what a
    /// careless packer could write.
    fn raw(paths: &[&[u8]]) -> Vec<u8> {
        let (mut records, mut blob) = (Vec::new(), Vec::new());
        for (i, p) in paths.iter().enumerate() {
            records.extend_from_slice(&(blob.len() as u32).to_le_bytes());
            records.extend_from_slice(&(p.len() as u32).to_le_bytes());
            records.extend_from_slice(&(10 * (i as u64 + 1)).to_le_bytes());
            records.extend_from_slice(&0i64.to_le_bytes());
            blob.extend_from_slice(p);
            blob.push(0);
        }
        let count = (paths.len() * 2).next_power_of_two().max(2);
        let mut slots = vec![(0u64, 0u32, 0u32); count];
        for (i, p) in paths.iter().enumerate() {
            let h = hash_bytes(p);
            let mut at = h as usize & (count - 1);
            let mut shared = false;
            while slots[at].1 != 0 {
                if slots[at].0 == h {
                    slots[at].2 |= DUPLICATE;
                    shared = true;
                }
                at = (at + 1) & (count - 1);
            }
            slots[at] = (h, i as u32 + 1, u32::from(shared));
        }
        let slots_at = (HEADER + records.len() + blob.len()).next_multiple_of(8);
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&(RECORD as u32).to_le_bytes());
        out.extend_from_slice(&(paths.len() as u64).to_le_bytes());
        out.extend_from_slice(&(blob.len() as u64).to_le_bytes());
        out.extend_from_slice(&(slots_at as u64).to_le_bytes());
        out.extend_from_slice(&(SLOT as u32).to_le_bytes());
        out.extend_from_slice(&(count as u32).to_le_bytes());
        out.extend_from_slice(&records);
        out.extend_from_slice(&blob);
        out.resize(slots_at, 0);
        for (h, id, flags) in slots {
            out.extend_from_slice(&h.to_le_bytes());
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&flags.to_le_bytes());
        }
        out
    }

    #[test]
    fn check_takes_what_the_runtime_takes() {
        let ok = raw(&[b"/app0/a/One.bin", b"/app0/a/two.bin", b"/app0/b.bin"]);
        assert_eq!(
            check(&ok),
            Ok(Checked {
                records: 3,
                sorted: true
            })
        );
        // Unsorted is valid: the runtime pages it with a rank order.
        let unsorted = raw(&[b"/app0/b.bin", b"/APP0/a.bin"]);
        assert_eq!(check(&unsorted).map(|c| c.sorted), Ok(false));
        // ampr-format's `open_refuses_a_path_of_path_buf_bytes`: 1023 bytes pass, 1024 do not.
        let mut long = vec![b'a'; PATH_MAX];
        long[..6].copy_from_slice(b"/app0/");
        for i in (206..PATH_MAX).step_by(200) {
            long[i] = b'/';
        }
        assert!(check(&raw(&[&long[..PATH_MAX - 1]])).is_ok());
        assert!(check(&raw(&[&long[..]])).is_err());
        // A UTF-8 name is counted in bytes: 85 three-byte characters pass, 86 do not.
        let name = |n: usize| format!("/app0/{}", "\u{3042}".repeat(n)).into_bytes();
        assert!(check(&raw(&[&name(85)])).is_ok());
        let e = check(&raw(&[&name(86)])).unwrap_err();
        assert!(e.contains("258 bytes"), "{e}");
        for bad in [
            &b"/app0/A.bin\0/app0/a.bin"[..], // split below
            b"/app0/x\\y.bin",
            b"/app0/x//y.bin",
            b"/app0/./y.bin",
            b"/app0/../y.bin",
            b"/app0/y.bin/",
            b"/data/y.bin",
            b"/app0/",
        ] {
            let paths: Vec<&[u8]> = bad.split(|&b| b == 0).collect();
            assert!(
                check(&raw(&paths)).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
        let e = check(&raw(&[b"/app0/A.bin", b"/app0/a.bin"])).unwrap_err();
        assert!(e.contains("differ only in letter case"), "{e}");
        // A path with no NUL after it, a slot with another hash, a short file, no record.
        let mut no_nul = ok.clone();
        let end = HEADER + 3 * RECORD + "/app0/a/One.bin".len();
        no_nul[end] = b'x';
        assert!(check(&no_nul).unwrap_err().contains("no NUL"));
        let mut wrong_slot = ok.clone();
        let slots_at = u64_at(&ok, 0x20) as usize;
        let used = (0..8)
            .find(|s| u32_at(&ok, slots_at + s * 16 + 8) != 0)
            .unwrap();
        wrong_slot[slots_at + used * 16] ^= 1;
        assert!(check(&wrong_slot).is_err());
        assert!(check(&ok[..ok.len() - 1]).is_err());
        assert!(check(&ok[..HEADER]).is_err());
        let mut none = raw(&[b"/app0/a"]);
        none[0x10] = 0;
        assert!(check(&none).unwrap_err().contains("lists no file"));
    }

    /// A header whose hash table sits at the end of the address space, or whose size wraps it:
    /// refused, never a wrapped length or an out-of-range read.
    #[test]
    fn check_refuses_offsets_that_overflow() {
        let ok = raw(&[b"/app0/a.bin", b"/app0/b.bin"]);
        for slots_at in [!7u64, (u64::MAX - (1 << 34)) & !7, 1 << 63] {
            let mut d = ok.clone();
            d[0x20..0x28].copy_from_slice(&slots_at.to_le_bytes());
            let e = check(&d).unwrap_err();
            assert!(e.contains("shorter than its header"), "{slots_at:#x}: {e}");
        }
        // The largest table the header can name, past the end of the file.
        let mut d = ok.clone();
        d[0x2C..0x30].copy_from_slice(&(1u32 << 31).to_le_bytes());
        assert!(check(&d).unwrap_err().contains("shorter than its header"));
        // A record whose offset and length add up past u32 (and past the paths).
        let mut d = ok.clone();
        d[HEADER..HEADER + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        d[HEADER + 4..HEADER + 8].copy_from_slice(&2u32.to_le_bytes());
        assert!(check(&d).unwrap_err().contains("out of bounds"));
    }

    /// A full table where every record sits one slot before where its lookup starts: each lookup
    /// walks nearly the whole table (count² steps). Refused past the budget, quickly; the same
    /// records in a table half full pass.
    #[test]
    fn check_refuses_a_table_every_lookup_crawls() {
        const N: usize = 1024;
        let names: Vec<Vec<u8>> = (0..N)
            .map(|i| format!("/app0/f{i:05}.bin").into_bytes())
            .collect();
        let paths: Vec<&[u8]> = names.iter().map(Vec::as_slice).collect();
        let good = raw(&paths);
        assert!(check(&good).is_ok());
        let slots_at = u64_at(&good, 0x20) as usize;
        let mut d = good[..slots_at].to_vec();
        d[0x2C..0x30].copy_from_slice(&(N as u32).to_le_bytes());
        let mut slots = vec![(0u64, 0u32); N];
        // Each record at its home slot minus one, in order of home; a taken slot pushes it on
        // (backwards), so the table stays full and every record is found only at the end.
        let mut order: Vec<usize> = (0..N).collect();
        order.sort_by_key(|&i| hash_bytes(paths[i]) as usize & (N - 1));
        for &i in &order {
            let h = hash_bytes(paths[i]);
            let mut at = (h as usize).wrapping_sub(1) & (N - 1);
            while slots[at].1 != 0 {
                at = at.wrapping_sub(1) & (N - 1);
            }
            slots[at] = (h, i as u32 + 1);
        }
        for (h, id) in slots {
            d.extend_from_slice(&h.to_le_bytes());
            d.extend_from_slice(&id.to_le_bytes());
            d.extend_from_slice(&0u32.to_le_bytes());
        }
        let t = std::time::Instant::now();
        let e = check(&d).unwrap_err();
        assert!(e.contains("too crowded"), "{e}");
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
    }

    /// What [`build`] refuses rather than write an index the runtime would throw away whole.
    #[test]
    fn build_refuses_what_the_runtime_refuses() {
        let one = |p: &str| build(&[(p.to_string(), 1)], 0);
        assert!(one("data/\u{3042}\u{3044}.bin").is_ok());
        let long_name = format!("data/{}", "\u{3042}".repeat(86));
        let e = one(&long_name).unwrap_err();
        assert!(e.contains("255 bytes") && e.contains("rename"), "{e}");
        let long_path = format!("{}/x.bin", vec!["d"; 510].join("/"));
        assert!(one(&long_path).unwrap_err().contains("under 1024 bytes"));
        for bad in [
            "a\\b.bin",
            "a//b.bin",
            "./a.bin",
            "a/../b.bin",
            "a\0b",
            "a/",
        ] {
            assert!(one(bad).is_err(), "{bad:?}");
        }
        assert!(build(&[], 0).unwrap_err().contains("no file"));
        // At the limit: a 1023-byte path (with /app0/) is taken.
        let dirs = vec!["d".repeat(200); 4].join("/");
        let at_limit = format!("{dirs}/{}", "e".repeat(1023 - 6 - dirs.len() - 1));
        assert_eq!(at_limit.len() + 6, PATH_MAX - 1);
        assert!(one(&at_limit).is_ok());
    }

    /// An AMPRZPK1 with just the fields [`zstd_files`] reads.
    pub(crate) fn zstd_index(files: &[(&str, u64)]) -> Vec<u8> {
        let mut out = vec![0u8; 128];
        out[..8].copy_from_slice(b"AMPRZPK1");
        let mut strings = Vec::new();
        for (path, size) in files {
            let mut r = [0u8; 32];
            r[8..16].copy_from_slice(&size.to_le_bytes());
            r[20..24].copy_from_slice(&(strings.len() as u32).to_le_bytes());
            r[24..26].copy_from_slice(&(path.len() as u16).to_le_bytes());
            out.extend_from_slice(&r);
            strings.extend_from_slice(path.as_bytes());
        }
        let strings_at = out.len() as u64;
        out.extend_from_slice(&strings);
        out[40..48].copy_from_slice(&(files.len() as u64).to_le_bytes());
        out[56..64].copy_from_slice(&128u64.to_le_bytes());
        out[80..88].copy_from_slice(&strings_at.to_le_bytes());
        out[88..96].copy_from_slice(&(strings.len() as u64).to_le_bytes());
        out
    }

    #[test]
    fn zstd_packs_must_join_the_index() {
        let files = [
            ("eboot.bin".to_string(), 9),
            ("Data/A.bin".to_string(), 300_000),
            ("data/b.bin".to_string(), 5),
        ];
        let index = build(&files, 0).unwrap();
        let joined = |packed: &[(&str, u64)]| check_zstd(&index, packed.iter().copied());
        assert!(joined(&[("Data/A.bin", 300_000), ("data/b.bin", 5)]).is_ok());
        // The runtime joins by folded path.
        assert!(joined(&[("data/a.bin", 300_000)]).is_ok());
        let e = joined(&[("data/c.bin", 1)]).unwrap_err();
        assert!(e.contains("not in ampr_emu.index"), "{e}");
        let e = joined(&[("Data/A.bin", 300_001)]).unwrap_err();
        assert!(e.contains("serves no packed file"), "{e}");
        let e = joined(&[("Data/A.bin", 300_000), ("data/a.bin", 300_000)]).unwrap_err();
        assert!(e.contains("only in letter case"), "{e}");
        assert_eq!(
            zstd_files(&zstd_index(&[("Data/A.bin", 300_000), ("data/b.bin", 5)])),
            Some(vec![
                ("Data/A.bin".to_string(), 300_000),
                ("data/b.bin".to_string(), 5)
            ])
        );
        assert!(zstd_files(&index).is_none());
    }

    #[test]
    fn lz4_records_must_match_the_index_by_place() {
        use crate::ampr_pack::format::{path_hash, FileRecord, FILE_PACKED};
        let index = build(&[("a.bin".into(), 4), ("b.bin".into(), 8)], 0).unwrap();
        let rec = |path: &str, size: u64, packed: bool| FileRecord {
            path_hash: path_hash(path),
            logical_size: size,
            flags: if packed { FILE_PACKED } else { 0 },
            ..Default::default()
        };
        let good = [rec("/app0/a.bin", 4, false), rec("/app0/b.bin", 8, true)];
        assert!(check_lz4(&index, &good).is_ok());
        // Out of place, at another size, past the index: not served.
        let swapped = [rec("/app0/b.bin", 8, true), rec("/app0/a.bin", 4, true)];
        assert!(check_lz4(&index, &swapped).is_err());
        let resized = [rec("/app0/a.bin", 5, true)];
        assert!(check_lz4(&index, &resized).is_err());
        let extra = [
            rec("/app0/a.bin", 4, false),
            rec("/app0/b.bin", 8, false),
            rec("/app0/c.bin", 1, true),
        ];
        assert!(check_lz4(&index, &extra).is_err());
    }

    /// A game tree in memory.
    struct Mem(Vec<crate::source::SourceFile>, Vec<Vec<u8>>);
    impl crate::source::SourceTree for Mem {
        fn files(&self) -> &[crate::source::SourceFile] {
            &self.0
        }
        fn read(&mut self, path: &str) -> crate::Result<Vec<u8>> {
            let i = self.0.iter().position(|f| f.path == path);
            i.map(|i| self.1[i].clone())
                .ok_or_else(|| crate::Error::Format(format!("no {path}")))
        }
        fn describe(&self) -> String {
            "memory".into()
        }
    }
    fn mem(items: Vec<(&str, Vec<u8>)>) -> Mem {
        Mem(
            items
                .iter()
                .map(|(p, b)| crate::source::SourceFile {
                    path: p.to_string(),
                    size: b.len() as u64,
                })
                .collect(),
            items.into_iter().map(|(_, b)| b).collect(),
        )
    }

    /// A rebuild of a zstd-packed game lists its packed files at their logical sizes (from
    /// ampr_zstd.index), not the packs; one of an LZ4-packed game is refused.
    #[test]
    fn a_packed_games_rebuild_keeps_its_packed_files() {
        let packed = [("data/big.bin", 1_000_000u64), ("Data/Other.bin", 7)];
        let zindex = zstd_index(&packed);
        let mut tree = mem(vec![
            ("eboot.bin", b"no import".to_vec()),
            ("sce_sys/param.json", b"{}".to_vec()),
            ("ampr_zstd.index", zindex),
            ("ampr_zstd-000.pak", vec![0; 64]),
            (NAME, b"stale".to_vec()),
        ]);
        let r = rebuild(&mut tree, 3).unwrap();
        let mut listed: Vec<(String, u64)> = parse(&r.index)
            .unwrap()
            .into_iter()
            .map(|e| (e.path, e.size))
            .collect();
        listed.sort();
        assert_eq!(
            listed,
            vec![
                ("Data/Other.bin".to_string(), 7),
                (BACKUP.to_string(), 5),
                ("data/big.bin".to_string(), 1_000_000),
                ("eboot.bin".to_string(), 9),
                ("sce_sys/param.json".to_string(), 2),
            ]
        );
        assert!(check_zstd(&r.index, packed.iter().copied()).is_ok());

        let mut lz4 = mem(vec![
            ("eboot.bin", b"libSceAmpr".to_vec()),
            ("ampr_assets.index", b"AMPRPAK4....".to_vec()),
        ]);
        assert_eq!(rebuild(&mut lz4, 3).unwrap_err(), LZ4_PACKED);
    }

    #[test]
    fn respell_changes_only_the_case_of_one_record() {
        let mut d = build(&[("Mods/il2cpp.prx".into(), 4), ("b.bin".into(), 8)], 0).unwrap();
        let before = d.clone();
        assert!(!respell(&mut d, "Mods/il2cpp.prx", "Mods/il2cpp.prx"));
        assert!(!respell(&mut d, "Mods/il2cpp.prx", "Mods/other.prx"));
        assert!(respell(&mut d, "Mods/il2cpp.prx", "Mods/Il2Cpp.prx"));
        assert_eq!(d.len(), before.len());
        check(&d).unwrap();
        let paths: Vec<String> = parse(&d).unwrap().into_iter().map(|e| e.path).collect();
        assert!(paths.contains(&"Mods/Il2Cpp.prx".to_string()));
    }
}
