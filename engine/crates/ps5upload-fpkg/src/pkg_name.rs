//! The name a fake package gives its game: read, and changed in the package itself (#937).
//!
//! **Where the name is.** A PS5 package (`\x7FFIH`) carries `param.json` twice: as entry
//! `0x2000` of its `\x7FCNT` container, and as `sce_sys/param.json` inside the game image. The
//! installer copies the container's entries to `/user/appmeta/<title>/` (see `cnt_write`),
//! which is where the system reads a game's name, icon and sound from, so the container's copy
//! is the one changed (`titleName` under `localizedParameters.<language>`, and a top-level
//! `titleName` when there is one). The image's copy is left alone: it sits in compressed image
//! blocks under the image digests, and the game is mounted from it, not named from it. That the
//! home screen then shows the new name has not been seen on a console yet. A PS4 package (a
//! bare `\x7FCNT`) carries `param.sfo` only as entry `0x1000`: `TITLE`, and
//! `TITLE_00`…`TITLE_29` for other languages.
//!
//! **What covers it**, all of it recomputed here from public values. Measured 2026-11-25: the
//! digests below hold in this crate's own packages, in the LibOrbisPkg-byte fixture, and in
//! real fake packages from other writers (PS5 games, an app and an add-on, one with a header
//! signature this crate's padding does not give; a PS4 game with another digest set). Copies
//! of three of the PS5 ones were renamed: the full verifier's report on two was line for line
//! the one from before, and the third passed every check.
//!
//! - PS5, SHA3-256: the entry's slot in the digest table (`0x0001`), the `param` slot of the
//!   general digests (`0x0080`) and that entry's own slot, the header rollups at `0x100` and
//!   `0x120`, the digest table's digest at `0x140`, the body digest at `0x160`, the package
//!   digest at `0xFE0`, and the header "signature" at `0x1000`, which is an RSA encryption of
//!   `SHA3(header)` under a public modulus. Outside the container: `playgo-chunk.crc` in the
//!   trailing ZIP (a CRC-32C per 64 KiB of everything before the ZIP), and `pfsimage.xml`
//!   there, which repeats the param, package and body digests and the name; each with the
//!   ZIP's own CRC-32 of the member.
//! - PS4, SHA-256: the same header slots, with `0x100` covering the keys, the image key, the
//!   general digests, the entry table and the digest table; the general digests' param slot at
//!   `0xC0`; and the header signature under `PkgPublicKeys[3]` as LibOrbisPkg writes it.
//!
//! Nothing here needs a private key, which is why only a fake (debug) package can be changed:
//! an official one is signed by its publisher, and is refused.
//!
//! **How.** Always at the same length, so no entry, offset or size moves and the file keeps
//! its length: a shorter `param.json` is padded with spaces before its closing brace, a
//! longer one gives up whitespace between its tokens; `param.sfo` strings have a fixed room
//! (`max_len`). A name that does not fit is refused. [`plan`] reads the package and returns
//! the bytes to write, having checked first that every digest it is about to recompute holds
//! in the package as it is (a package whose digests follow another rule is refused rather
//! than guessed at); the caller writes them (the engine does, through its undo journal) and
//! [`check`] reads the result back from disk.

use std::ops::Range;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::cnt::{self, ids, Cnt};
use crate::crypto::{crc32c, sha3};
use crate::{be32, be64, fih, format_err, keys, rsa, si, verify, Error, PkgFile, Result, BLOCK};

/// The longest name accepted, in UTF-8 bytes: what a `param.sfo` TITLE holds, and what Sony's
/// tools allow a `titleName`.
pub const MAX_NAME_BYTES: usize = 127;
/// The container entry of a PS4 package's `param.sfo`.
const PARAM_SFO: u32 = 0x1000;
/// The container entry of a PS4 package's PlayGo chunk digests.
const PS4_CHUNK_SHA: u32 = 0x1002;
/// The most of a `pfsimage.xml` read (the largest seen is under 1 MiB).
const MAX_MANIFEST: u64 = 64 << 20;
/// The most of a `playgo-chunk.crc` read: 4 bytes per 64 KiB, so 1 TiB of package.
const MAX_CRC_TABLE: u64 = 64 << 20;
const ENCRYPTED: u32 = 0x8000_0000;
const SIGNATURE_AT: usize = 0x1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Ps5,
    Ps4,
}

/// One name a package holds: `key` is the language (`en-US`; empty for a PS5 package's
/// top-level `titleName`) or the `param.sfo` key (`TITLE`, `TITLE_03`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NameEntry {
    pub key: String,
    pub name: String,
}

/// Why a package's name can't be changed. `code` is for the app to word: `retail` (signed by
/// its publisher), `patch` (a delta update, which holds no name of its own to change),
/// `no_name`, or `unsupported` (anything else; `reason` says what).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Refusal {
    pub code: &'static str,
    pub reason: String,
}

impl Refusal {
    fn new(code: &'static str, reason: impl Into<String>) -> Self {
        Self {
            code,
            reason: reason.into(),
        }
    }
}

/// How long a new name may be in this package, in UTF-8 bytes of plain text. A rename never
/// moves anything in the package, so the room is what the package itself has: the old names'
/// own bytes and the whitespace its `param.json` and manifest can give up (PS5), or the fixed
/// room of its `param.sfo` strings (PS4); never more than [`MAX_NAME_BYTES`]. A character that
/// has to be written escaped (`"` and `\` in `param.json`; `&`, `<` and `>` in the manifest)
/// takes more than its own bytes, so such a name can still be refused a few bytes earlier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Room {
    /// Changing only the entries that hold the current default name.
    pub default_name: usize,
    /// Giving every language the name.
    pub all_languages: usize,
}

/// A package's names as it holds them.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Names {
    pub platform: Platform,
    pub content_id: String,
    /// The name the console shows unless its language has another: the default language's
    /// (`TITLE` on PS4).
    pub name: String,
    pub entries: Vec<NameEntry>,
    /// How many entries hold a name other than `name`.
    pub other_names: usize,
    /// Why the name can't be changed; `None` when it can.
    pub refusal: Option<Refusal>,
    /// How long a new name may be; `None` when it can't be changed.
    pub room: Option<Room>,
}

/// Bytes to write at an offset of the package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub offset: u64,
    pub data: Vec<u8>,
}

/// A name change, planned: nothing written yet.
#[derive(Debug, Clone)]
pub struct Plan {
    pub platform: Platform,
    pub previous: String,
    pub name: String,
    /// How many of the package's name entries change.
    pub changed: usize,
    /// In offset order, never overlapping, all inside the file; empty when the package already
    /// has this name.
    pub patches: Vec<Patch>,
    /// The package's length, which the change keeps.
    pub len: u64,
    /// The entries as they read afterwards.
    expect: Vec<NameEntry>,
    /// Whether `ps5upload_pkg::parse_pkg` read the previous name, so it must read the new one.
    reader_reads_name: bool,
    /// The container checks that held before the change.
    held: Vec<String>,
    /// The 64 KiB blocks whose CRC in `playgo-chunk.crc` the change rewrites.
    crc_blocks: Vec<u64>,
    /// The members of the trailing ZIP whose own CRC-32 the change rewrites.
    zip_members: Vec<String>,
}

impl Plan {
    /// Bytes the change writes.
    pub fn write_bytes(&self) -> u64 {
        self.patches.iter().map(|p| p.data.len() as u64).sum()
    }
}

fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

fn refused<T>(reason: impl Into<String>) -> Result<T> {
    Err(Error::Format(reason.into()))
}

/// `name` as it is stored: trimmed, not empty, no control characters, at most
/// [`MAX_NAME_BYTES`].
pub fn clean_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return refused("the name is empty");
    }
    if name.chars().any(char::is_control) {
        return refused("the name holds a control character (a line break or a tab)");
    }
    if name.len() > MAX_NAME_BYTES {
        return refused(format!(
            "the name is {} bytes long; at most {MAX_NAME_BYTES} fit (about {MAX_NAME_BYTES} \
             letters, fewer in scripts that take more bytes each)",
            name.len()
        ));
    }
    Ok(name.to_string())
}

/// The first and last bytes that differ between `old` and `new` (same length), as one patch.
fn changed(base: u64, at: usize, old: &[u8], new: &[u8], out: &mut Vec<Patch>) {
    debug_assert_eq!(old.len(), new.len());
    let Some(first) = old.iter().zip(new).position(|(a, b)| a != b) else {
        return;
    };
    let last = old
        .iter()
        .zip(new)
        .rposition(|(a, b)| a != b)
        .unwrap_or(first);
    out.push(Patch {
        offset: base + (at + first) as u64,
        data: new[first..=last].to_vec(),
    });
}

/// `buf`, read from `at` of the package, with the patches that reach into it written over it.
fn overlay(buf: &mut [u8], at: u64, patches: &[Patch]) {
    let end = at + buf.len() as u64;
    for p in patches {
        let p_end = p.offset + p.data.len() as u64;
        if p.offset >= end || p_end <= at {
            continue;
        }
        let from = p.offset.max(at);
        let to = p_end.min(end);
        buf[(from - at) as usize..(to - at) as usize]
            .copy_from_slice(&p.data[(from - p.offset) as usize..(to - p.offset) as usize]);
    }
}

/// The names that change: every one with `all`, else those that hold the default name.
fn targets(entries: &[NameEntry], default: &str, all: bool) -> Vec<usize> {
    (0..entries.len())
        .filter(|i| all || entries[*i].name == default)
        .collect()
}

fn names_of(
    platform: Platform,
    content_id: &str,
    entries: Vec<NameEntry>,
    default: Option<usize>,
    refusal: Option<Refusal>,
) -> Names {
    let name = default
        .and_then(|i| entries.get(i))
        .map(|e| e.name.clone())
        .unwrap_or_default();
    let other_names = entries.iter().filter(|e| e.name != name).count();
    Names {
        platform,
        content_id: content_id.to_string(),
        name,
        other_names,
        entries,
        refusal,
        room: None,
    }
}

// ─── param.json ─────────────────────────────────────────────────────────────────────────

/// One `titleName` of a `param.json`: its language (empty for the top-level one), the string
/// literal's place (quotes included) and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
struct JsonName {
    key: String,
    span: Range<usize>,
    value: String,
}

/// A `param.json` as far as a rename needs it.
#[derive(Debug, Clone, Default)]
struct ParamDoc {
    names: Vec<JsonName>,
    default_language: Option<String>,
    /// Runs of whitespace between tokens: none of it means anything.
    whitespace: Vec<Range<usize>>,
    /// Where the outermost object's closing brace is.
    close: usize,
}

impl ParamDoc {
    /// The name shown by default: the default language's, else the top-level one, else the
    /// first.
    fn default_index(&self) -> Option<usize> {
        let by_key = |key: &str| self.names.iter().position(|n| n.key == key);
        self.default_language
            .as_deref()
            .and_then(by_key)
            .or_else(|| by_key(""))
            .or(if self.names.is_empty() { None } else { Some(0) })
    }

    fn entries(&self) -> Vec<NameEntry> {
        self.names
            .iter()
            .map(|n| NameEntry {
                key: n.key.clone(),
                name: n.value.clone(),
            })
            .collect()
    }
}

/// The longest plain name `edit_param` can put into the entries `which`: each takes the name
/// and its two quotes, out of the old literals' bytes and all the whitespace between tokens.
fn param_room(doc: &ParamDoc, which: &[usize]) -> usize {
    if which.is_empty() {
        return MAX_NAME_BYTES;
    }
    let old: usize = which.iter().map(|i| doc.names[*i].span.len()).sum();
    let spare: usize = doc.whitespace.iter().map(|r| r.len()).sum();
    ((old + spare) / which.len()).saturating_sub(2)
}

/// Where a value sits in a `param.json`, as far as names go.
#[derive(Clone)]
enum Place {
    Root,
    Localized,
    Language(String),
    Other,
}

struct Scanner<'a> {
    b: &'a [u8],
    i: usize,
    doc: ParamDoc,
}

const MAX_JSON_DEPTH: usize = 64;

impl Scanner<'_> {
    fn bad<T>(&self, what: &str) -> std::result::Result<T, String> {
        Err(format!("{what} at byte {}", self.i))
    }

    fn skip_whitespace(&mut self) {
        let start = self.i;
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
        if self.i > start {
            self.doc.whitespace.push(start..self.i);
        }
    }

    /// A string literal at the cursor: its place, quotes included, and its text.
    fn string(&mut self) -> std::result::Result<(Range<usize>, String), String> {
        let start = self.i;
        if self.b.get(self.i) != Some(&b'"') {
            return self.bad("a string was expected");
        }
        self.i += 1;
        loop {
            match self.b.get(self.i) {
                None => return self.bad("a string does not end"),
                Some(b'"') => break,
                Some(b'\\') => self.i += 2,
                Some(_) => self.i += 1,
            }
        }
        self.i += 1;
        if self.i > self.b.len() {
            return self.bad("a string does not end");
        }
        let text: String = serde_json::from_slice(&self.b[start..self.i])
            .map_err(|e| format!("a string at byte {start} does not read: {e}"))?;
        Ok((start..self.i, text))
    }

    fn value(&mut self, place: Place, depth: usize) -> std::result::Result<(), String> {
        if depth > MAX_JSON_DEPTH {
            return self.bad("nested too deep");
        }
        match self.b.get(self.i) {
            Some(b'{') => self.object(place, depth),
            Some(b'[') => {
                self.i += 1;
                self.skip_whitespace();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(());
                }
                loop {
                    self.skip_whitespace();
                    self.value(Place::Other, depth + 1)?;
                    self.skip_whitespace();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(());
                        }
                        _ => return self.bad("a list does not end"),
                    }
                }
            }
            Some(b'"') => self.string().map(drop),
            Some(_) => {
                // A number, true, false or null: up to the next structural byte.
                let start = self.i;
                while self.b.get(self.i).is_some_and(|c| {
                    !matches!(c, b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
                }) {
                    self.i += 1;
                }
                if self.i == start {
                    return self.bad("a value was expected");
                }
                Ok(())
            }
            None => self.bad("the text ends early"),
        }
    }

    fn object(&mut self, place: Place, depth: usize) -> std::result::Result<(), String> {
        self.i += 1;
        self.skip_whitespace();
        if self.b.get(self.i) == Some(&b'}') {
            if depth == 0 {
                self.doc.close = self.i;
            }
            self.i += 1;
            return Ok(());
        }
        loop {
            self.skip_whitespace();
            let (_, key) = self.string()?;
            self.skip_whitespace();
            if self.b.get(self.i) != Some(&b':') {
                return self.bad("a colon was expected");
            }
            self.i += 1;
            self.skip_whitespace();
            let is_string = self.b.get(self.i) == Some(&b'"');
            match (&place, key.as_str(), is_string) {
                (Place::Root, "titleName", true) => {
                    let (span, value) = self.string()?;
                    self.doc.names.push(JsonName {
                        key: String::new(),
                        span,
                        value,
                    });
                }
                (Place::Language(language), "titleName", true) => {
                    let (span, value) = self.string()?;
                    self.doc.names.push(JsonName {
                        key: language.clone(),
                        span,
                        value,
                    });
                }
                (Place::Localized, "defaultLanguage", true) => {
                    self.doc.default_language = Some(self.string()?.1);
                }
                _ => {
                    let inner = match (&place, key.as_str()) {
                        (Place::Root, "localizedParameters") => Place::Localized,
                        (Place::Localized, language) => Place::Language(language.to_string()),
                        _ => Place::Other,
                    };
                    self.value(inner, depth + 1)?;
                }
            }
            self.skip_whitespace();
            match self.b.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    if depth == 0 {
                        self.doc.close = self.i;
                    }
                    self.i += 1;
                    return Ok(());
                }
                _ => return self.bad("an object does not end"),
            }
        }
    }
}

/// Reads a `param.json` as stored in a container: one object, perhaps after a byte-order mark
/// and before zero padding.
fn scan_param(bytes: &[u8]) -> std::result::Result<ParamDoc, String> {
    let mut s = Scanner {
        b: bytes,
        i: 0,
        doc: ParamDoc::default(),
    };
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        s.i = 3;
    }
    s.skip_whitespace();
    if s.b.get(s.i) != Some(&b'{') {
        return s.bad("no object");
    }
    s.value(Place::Root, 0)?;
    s.skip_whitespace();
    if bytes[s.i..].iter().any(|b| *b != 0) {
        return s.bad("something follows the object");
    }
    Ok(s.doc)
}

/// The JSON a stored `param.json` holds (without a byte-order mark or zero padding).
fn param_value(bytes: &[u8]) -> Option<serde_json::Value> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let end = bytes.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
    serde_json::from_slice(&bytes[..end]).ok()
}

/// `bytes` (a stored `param.json`, read as `doc`) with the names at `which` set to `name`, at
/// the same length: spaces go in before the closing brace when it gets shorter, whitespace
/// between tokens comes out (from the end backwards) when it gets longer. Checked: the result
/// parses to the same JSON as before but for those names.
fn edit_param(bytes: &[u8], doc: &ParamDoc, which: &[usize], name: &str) -> Result<Vec<u8>> {
    let literal = serde_json::to_string(name)
        .map_err(|e| Error::Format(format!("the name can't be written as JSON: {e}")))?
        .into_bytes();
    let old: usize = which.iter().map(|i| doc.names[*i].span.len()).sum();
    let new = literal.len() * which.len();
    // (where, how many bytes of the original it replaces, what goes there)
    let mut steps: Vec<(usize, usize, Vec<u8>)> = which
        .iter()
        .map(|i| {
            let span = &doc.names[*i].span;
            (span.start, span.len(), literal.clone())
        })
        .collect();
    if new < old {
        steps.push((doc.close, 0, vec![b' '; old - new]));
    } else {
        let mut need = new - old;
        for run in doc.whitespace.iter().rev() {
            if need == 0 {
                break;
            }
            let take = need.min(run.len());
            steps.push((run.end - take, take, Vec::new()));
            need -= take;
        }
        if need > 0 {
            return refused(format!(
                "the new name is {need} byte(s) too long for this package: its param.json has \
                 no more room, and the package is changed without moving anything in it"
            ));
        }
    }
    steps.sort_by_key(|s| s.0);
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    for (start, len, put) in steps {
        if start < at {
            return refused("the param.json's names overlap");
        }
        out.extend_from_slice(&bytes[at..start]);
        out.extend_from_slice(&put);
        at = start + len;
    }
    out.extend_from_slice(&bytes[at..]);

    // The same JSON but for the names: anything else means the edit went wrong.
    let mut want = param_value(bytes)
        .ok_or_else(|| Error::Format("the package's param.json does not parse".into()))?;
    for i in which {
        let slot = match doc.names[*i].key.as_str() {
            "" => want.get_mut("titleName"),
            language => want
                .get_mut("localizedParameters")
                .and_then(|l| l.get_mut(language))
                .and_then(|l| l.get_mut("titleName")),
        };
        match slot {
            Some(v) => *v = serde_json::Value::String(name.to_string()),
            None => return refused("the param.json's names are not where they were read"),
        }
    }
    if out.len() != bytes.len() || param_value(&out) != Some(want) {
        return refused("the renamed param.json did not come out right; nothing was changed");
    }
    Ok(out)
}

// ─── pfsimage.xml ───────────────────────────────────────────────────────────────────────

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// The text between `<tag>` and `</tag>`, the first of them.
fn element(xml: &[u8], tag: &str) -> Option<Range<usize>> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = find(xml, open.as_bytes(), 0)? + open.len();
    let end = find(xml, close.as_bytes(), start)?;
    Some(start..end)
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Sets the digest `<tag>` holds (32 `0xNN` bytes) to `new`, in place. Left alone when the
/// manifest has no such element; an error when it holds anything but `old`.
fn set_manifest_digest(
    xml: &mut [u8],
    tag: &str,
    old: &[u8; 32],
    new: &[u8; 32],
) -> std::result::Result<(), String> {
    let Some(inner) = element(xml, tag) else {
        return Ok(());
    };
    let mut at = Vec::with_capacity(32);
    let mut i = inner.start;
    while i + 4 <= inner.end {
        if xml[i] == b'0'
            && xml[i + 1] == b'x'
            && xml[i + 2].is_ascii_hexdigit()
            && xml[i + 3].is_ascii_hexdigit()
        {
            at.push(i + 2);
            i += 4;
        } else {
            i += 1;
        }
    }
    let byte = |i: usize| {
        std::str::from_utf8(&xml[i..i + 2])
            .ok()
            .and_then(|h| u8::from_str_radix(h, 16).ok())
    };
    let held: Vec<u8> = at.iter().filter_map(|i| byte(*i)).collect();
    if held.as_slice() != old.as_slice() {
        return Err(format!(
            "the {tag} in the package's manifest (pfsimage.xml) is not the container's"
        ));
    }
    let upper = at
        .iter()
        .any(|i| xml[*i..*i + 2].iter().any(u8::is_ascii_uppercase));
    for (i, b) in at.iter().zip(new) {
        let hex = if upper {
            format!("{b:02X}")
        } else {
            format!("{b:02x}")
        };
        xml[*i..*i + 2].copy_from_slice(hex.as_bytes());
    }
    Ok(())
}

/// Runs of indentation that mean nothing: spaces or tabs at the start of a line that begins
/// with a tag, after a line that ended with one, and not between an element's own opening and
/// closing tags (where they would be its text).
fn manifest_indentation(xml: &[u8]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut line = 0;
    while let Some(nl) = find(xml, b"\n", line) {
        let start = nl + 1;
        let mut end = start;
        while matches!(xml.get(end), Some(b' ' | b'\t')) {
            end += 1;
        }
        line = start;
        if end == start || xml.get(end) != Some(&b'<') {
            continue;
        }
        let before = if nl > 0 && xml[nl - 1] == b'\r' {
            nl - 1
        } else {
            nl
        };
        if before == 0 || xml[before - 1] != b'>' {
            continue;
        }
        let closing_next = xml.get(end + 1) == Some(&b'/');
        let tag_end = before - 1;
        let self_closing = tag_end >= 1 && xml[tag_end - 1] == b'/';
        let previous = xml[..tag_end].iter().rposition(|b| *b == b'<');
        let opening_before = !self_closing
            && previous.is_some_and(|p| !matches!(xml.get(p + 1), Some(b'/' | b'?' | b'!')));
        if closing_next && opening_before {
            continue;
        }
        out.push(start..end);
    }
    out
}

/// The longest plain name `set_manifest_name` can put in place of `old`: the old name's bytes
/// and all the indentation the manifest can give up. `None` when the manifest's name is not
/// changed with the package's (it has none, or holds another).
fn manifest_room(xml: &[u8], old: &str) -> Option<usize> {
    let inner = element(xml, "titleName")?;
    if xml_unescape(&String::from_utf8_lossy(&xml[inner.clone()])) != old {
        return None;
    }
    let spare: usize = manifest_indentation(xml).iter().map(|r| r.len()).sum();
    Some(inner.len() + spare)
}

/// `xml` with `<titleName>` changed from `old` to `new`, at the same length: spaces after
/// the element when it gets shorter, indentation given up when it gets longer. `None` when
/// the manifest has no such element or it holds another name (left as it is).
fn set_manifest_name(xml: &[u8], old: &str, new: &str) -> Result<Option<Vec<u8>>> {
    const CLOSE: &[u8] = b"</titleName>";
    let Some(inner) = element(xml, "titleName") else {
        return Ok(None);
    };
    let held = String::from_utf8_lossy(&xml[inner.clone()]);
    if xml_unescape(&held) != old {
        return Ok(None);
    }
    let put = xml_escape(new).into_bytes();
    let after = inner.end + CLOSE.len();
    let mut out = Vec::with_capacity(xml.len());
    if put.len() <= inner.len() {
        out.extend_from_slice(&xml[..inner.start]);
        out.extend_from_slice(&put);
        out.extend_from_slice(CLOSE);
        out.resize(out.len() + (inner.len() - put.len()), b' ');
        out.extend_from_slice(&xml[after..]);
    } else {
        let mut need = put.len() - inner.len();
        let mut cuts: Vec<Range<usize>> = Vec::new();
        // The nearest runs after the name first, so as little of the manifest moves as can.
        let mut runs = manifest_indentation(xml);
        runs.sort_by_key(|r| {
            if r.start >= after {
                (0, r.start)
            } else {
                (1, usize::MAX - r.start)
            }
        });
        for run in runs {
            if need == 0 {
                break;
            }
            let take = need.min(run.len());
            cuts.push(run.end - take..run.end);
            need -= take;
        }
        if need > 0 {
            return refused(format!(
                "the new name is {need} byte(s) too long for this package: its manifest \
                 (pfsimage.xml) has no more room"
            ));
        }
        // (where, how many bytes of the original it replaces, what goes there)
        let mut steps: Vec<(usize, usize, &[u8])> =
            cuts.iter().map(|c| (c.start, c.len(), &[][..])).collect();
        steps.push((inner.start, inner.len(), &put));
        steps.sort_by_key(|s| s.0);
        let mut at = 0;
        for (start, len, bytes) in steps {
            if start < at {
                return refused("the manifest's name sits where its indentation is");
            }
            out.extend_from_slice(&xml[at..start]);
            out.extend_from_slice(bytes);
            at = start + len;
        }
        out.extend_from_slice(&xml[at..]);
    }
    if out.len() != xml.len() {
        return refused("the renamed manifest did not come out right; nothing was changed");
    }
    Ok(Some(out))
}

// ─── PS5 ────────────────────────────────────────────────────────────────────────────────

struct Ps5 {
    file: PkgFile,
    fih_block: Vec<u8>,
    fih: fih::Fih,
    cnt: Cnt,
    /// `param.json` in the container's bytes, when it has one that reads.
    param: Option<(Range<usize>, ParamDoc)>,
    refusal: Option<Refusal>,
}

fn slot(i: usize) -> Range<usize> {
    i * 32..(i + 1) * 32
}

/// An entry's payload, in the container's bytes.
fn place(cnt: &Cnt, id: u32) -> Option<Range<usize>> {
    cnt.entry(id)
        .map(|e| e.offset as usize..e.offset as usize + e.size as usize)
}

/// An entry's slot of the digest table, in the container's bytes.
fn table_slot(cnt: &Cnt, id: u32) -> Option<Range<usize>> {
    let table = place(cnt, ids::DIGESTS)?;
    let i = cnt.entries.iter().position(|e| e.id == id)?;
    let s = slot(i);
    (s.end <= table.len()).then(|| table.start + s.start..table.start + s.end)
}

/// The general digests' `param` slot (the sixth), in a PS5 container's bytes.
fn ps5_param_slot(cnt: &Cnt) -> Option<Range<usize>> {
    let general = place(cnt, ids::GENERAL_DIGESTS)?;
    let s = 0x20 + 5 * 32..0x20 + 6 * 32;
    (s.end <= general.len()).then(|| general.start + s.start..general.start + s.end)
}

/// `CNT+0x100`'s input: the region from the body's start that the header sizes at `0x1C`.
fn ps5_rollup(cnt: &Cnt) -> Option<[u8; 32]> {
    let off = be64(&cnt.bytes, 0x20) as usize;
    let size = be32(&cnt.bytes, 0x1C) as usize;
    cnt.bytes.get(off..off.checked_add(size)?).map(sha3)
}

/// `CNT+0x120`'s input: the keys, the image key, the general digests and the first rows of
/// the entry table (as many as the header counts at `0x14`).
fn second_rollup_input(cnt: &Cnt) -> Option<Vec<u8>> {
    let rows = u16::from_be_bytes([cnt.bytes[0x14], cnt.bytes[0x15]]) as usize * 32;
    let mut pre = Vec::new();
    for id in [ids::ENTRY_KEYS, ids::IMAGE_KEY, ids::GENERAL_DIGESTS] {
        pre.extend_from_slice(&cnt.bytes[place(cnt, id)?]);
    }
    let table = place(cnt, ids::METAS)?;
    pre.extend_from_slice(cnt.bytes.get(table.start..table.start.checked_add(rows)?)?);
    Some(pre)
}

fn body(cnt: &Cnt) -> Option<&[u8]> {
    let off = cnt.body_offset as usize;
    cnt.bytes.get(off..off.checked_add(cnt.body_size as usize)?)
}

/// Whether every digest a PS5 rename recomputes holds in the container as it is: the proof
/// that these formulas are this package's. The first that does not is named.
fn ps5_holds(cnt: &Cnt) -> std::result::Result<(), String> {
    let digest_of = |id: u32| place(cnt, id).map(|r| sha3(&cnt.bytes[r]));
    let at = |r: Option<Range<usize>>| r.map(|r| &cnt.bytes[r]);
    let param = digest_of(ids::PARAM_JSON);
    let general = digest_of(ids::GENERAL_DIGESTS);
    let same = |held: Option<&[u8]>, want: Option<[u8; 32]>| match (held, want) {
        (Some(h), Some(w)) => h == w,
        _ => false,
    };
    let checks = [
        (
            "param.json's entry digest",
            same(at(table_slot(cnt, ids::PARAM_JSON)), param),
        ),
        (
            "the general digests' entry digest",
            same(at(table_slot(cnt, ids::GENERAL_DIGESTS)), general),
        ),
        (
            "the general digests' param slot",
            same(at(ps5_param_slot(cnt)), param),
        ),
        ("the header rollup digest", cnt.header_rollup_ok()),
        ("the second rollup digest", cnt.sc_entries2_ok()),
        ("the digest table's digest", cnt.digest_table_digest_ok()),
        ("the body digest", cnt.body_digest_ok()),
        ("the package digest", cnt.package_digest_ok()),
    ];
    match checks.iter().find(|(_, ok)| !ok) {
        Some((name, _)) => Err(format!(
            "{name} does not hold in this package, so its digests follow a rule this app does \
             not know (or the package is damaged)"
        )),
        None => Ok(()),
    }
}

impl Ps5 {
    /// `None` for an official package, which is refused before its container is read (it need
    /// not be readable at all).
    fn load(mut file: PkgFile) -> Result<Option<Self>> {
        let fih_block = file.read_at(0, BLOCK as usize)?;
        let fih = fih::parse(&fih_block)?;
        if !fih.is_debug() {
            return Ok(None);
        }
        let cnt = cnt::read(&mut file, fih.cnt_offset)?;
        let mut refusal = None;
        let mut param = None;
        match cnt.entry(ids::PARAM_JSON) {
            None => refusal = Some(Refusal::new("no_name", "the package has no param.json")),
            Some(e) if e.flags1 & ENCRYPTED != 0 => {
                refusal = Some(Refusal::new(
                    "unsupported",
                    "the package's param.json is encrypted",
                ))
            }
            Some(e) => {
                let range = e.offset as usize..e.offset as usize + e.size as usize;
                match scan_param(&cnt.bytes[range.clone()]) {
                    Ok(doc) if doc.names.is_empty() => {
                        refusal = Some(Refusal::new(
                            "no_name",
                            "the package's param.json holds no name (titleName)",
                        ))
                    }
                    Ok(doc) => param = Some((range, doc)),
                    Err(e) => {
                        refusal = Some(Refusal::new(
                            "unsupported",
                            format!("the package's param.json does not read: {e}"),
                        ))
                    }
                }
            }
        }
        if refusal.is_none() {
            if cnt.bytes.len() < SIGNATURE_AT + keys::METADATA_MODULUS.len() {
                refusal = Some(Refusal::new(
                    "unsupported",
                    "the package's container is too short to hold its header",
                ));
            } else if let Err(why) = ps5_holds(&cnt) {
                refusal = Some(Refusal::new("unsupported", why));
            }
        }
        Ok(Some(Self {
            file,
            fih_block,
            fih,
            cnt,
            param,
            refusal,
        }))
    }

    fn names(&self) -> Names {
        let (entries, default) = match &self.param {
            Some((_, doc)) => (doc.entries(), doc.default_index()),
            None => (Vec::new(), None),
        };
        names_of(
            Platform::Ps5,
            &self.cnt.content_id,
            entries,
            default,
            self.refusal.clone(),
        )
    }

    /// [`Ps5::names`] with the room a new name has: `param.json`'s and, when the package's
    /// manifest repeats the name, the manifest's (the smaller of the two counts).
    fn names_with_room(&mut self) -> Names {
        let mut names = self.names();
        if names.refusal.is_some() {
            return names;
        }
        let Some((_, doc)) = &self.param else {
            return names;
        };
        // A manifest that can't be read here limits nothing here: the rename itself reads it
        // again and says what is wrong with it.
        let manifest = si::read(&mut self.file)
            .ok()
            .flatten()
            .and_then(|zip| {
                let m = zip
                    .members
                    .iter()
                    .find(|m| m.name.ends_with("pfsimage.xml"))?;
                if m.size > MAX_MANIFEST {
                    return None;
                }
                self.file.read_at(m.offset, m.size as usize).ok()
            })
            .and_then(|xml| manifest_room(&xml, &names.name))
            .unwrap_or(usize::MAX);
        let room = |all: bool| {
            param_room(doc, &targets(&names.entries, &names.name, all))
                .min(manifest)
                .min(MAX_NAME_BYTES)
        };
        names.room = Some(Room {
            default_name: room(false),
            all_languages: room(true),
        });
        names
    }

    /// The container checks that hold, by name.
    fn held(&self) -> Vec<String> {
        verify::container_report(&self.fih_block, &self.fih, &self.cnt)
            .checks
            .into_iter()
            .filter(|c| c.ok)
            .map(|c| c.name)
            .collect()
    }

    fn plan(mut self, name: &str, all: bool, reader_reads_name: bool) -> Result<Plan> {
        let names = self.names();
        if let Some(r) = &self.refusal {
            return refused(r.reason.clone());
        }
        let Some((range, doc)) = self.param.clone() else {
            return refused("the package has no param.json");
        };
        let which = targets(&names.entries, &names.name, all);
        let mut expect = names.entries.clone();
        for i in &which {
            expect[*i].name = name.to_string();
        }
        let mut plan = Plan {
            platform: Platform::Ps5,
            previous: names.name.clone(),
            name: name.to_string(),
            changed: which
                .iter()
                .filter(|i| names.entries[**i].name != name)
                .count(),
            patches: Vec::new(),
            len: self.file.len(),
            expect,
            reader_reads_name,
            held: self.held(),
            crc_blocks: Vec::new(),
            zip_members: Vec::new(),
        };
        if plan.changed == 0 {
            return Ok(plan);
        }
        let new_param = edit_param(&self.cnt.bytes[range.clone()], &doc, &which, name)?;

        // Everything the change touches in the container, as it was.
        let cnt = &mut self.cnt;
        let missing = || Error::Format("the container lost an entry while it was read".into());
        let table = place(cnt, ids::DIGESTS).ok_or_else(missing)?;
        let general = place(cnt, ids::GENERAL_DIGESTS).ok_or_else(missing)?;
        let regions = [
            range.clone(),
            table.clone(),
            general.clone(),
            0x100..0x180,
            0xFE0..SIGNATURE_AT + keys::METADATA_MODULUS.len(),
        ];
        let saved: Vec<Vec<u8>> = regions
            .iter()
            .map(|r| cnt.bytes[r.clone()].to_vec())
            .collect();
        let old_param = sha3(&cnt.bytes[range.clone()]);
        let old_package: [u8; 32] = cnt.bytes[0xFE0..0x1000].try_into().unwrap();
        let old_body: [u8; 32] = cnt.bytes[0x160..0x180].try_into().unwrap();

        // The entry, then each digest over it, innermost first.
        cnt.bytes[range.clone()].copy_from_slice(&new_param);
        let param_digest = sha3(&new_param);
        let param_in_table = table_slot(cnt, ids::PARAM_JSON).ok_or_else(missing)?;
        cnt.bytes[param_in_table].copy_from_slice(&param_digest);
        let param_in_general = ps5_param_slot(cnt).ok_or_else(missing)?;
        cnt.bytes[param_in_general].copy_from_slice(&param_digest);
        let general_digest = sha3(&cnt.bytes[general.clone()]);
        let general_in_table = table_slot(cnt, ids::GENERAL_DIGESTS).ok_or_else(missing)?;
        cnt.bytes[general_in_table].copy_from_slice(&general_digest);
        let rollup = ps5_rollup(cnt).ok_or_else(missing)?;
        cnt.bytes[0x100..0x120].copy_from_slice(&rollup);
        let second = sha3(&second_rollup_input(cnt).ok_or_else(missing)?);
        cnt.bytes[0x120..0x140].copy_from_slice(&second);
        let table_digest = sha3(&cnt.bytes[table.clone()]);
        cnt.bytes[0x140..0x160].copy_from_slice(&table_digest);
        let body_digest = sha3(body(cnt).ok_or_else(missing)?);
        cnt.bytes[0x160..0x180].copy_from_slice(&body_digest);
        let package_digest = sha3(&cnt.bytes[..0xFE0]);
        cnt.bytes[0xFE0..0x1000].copy_from_slice(&package_digest);
        let signature = rsa::pkcs1_encrypt(&keys::METADATA_MODULUS, &sha3(&cnt.bytes[..0x1000]));
        cnt.bytes[SIGNATURE_AT..SIGNATURE_AT + signature.len()].copy_from_slice(&signature);
        if let Err(why) = ps5_holds(cnt) {
            return refused(format!(
                "the renamed container did not come out right ({why}); nothing was changed"
            ));
        }
        let base = self.fih.cnt_offset;
        for (r, old) in regions.iter().zip(&saved) {
            changed(base, r.start, old, &cnt.bytes[r.clone()], &mut plan.patches);
        }

        // Outside the container: the block CRCs and the manifest, in the ZIP that ends the
        // package.
        let digests = ManifestDigests {
            param: (old_param, param_digest),
            package: (old_package, package_digest),
            body: (old_body, body_digest),
        };
        if let Some(zip) = si::read(&mut self.file)? {
            self.plan_zip(&zip, &digests, &mut plan)?;
        }
        plan.patches.sort_by_key(|p| p.offset);
        let mut end = 0;
        for p in &plan.patches {
            if p.offset < end || p.offset + p.data.len() as u64 > plan.len {
                return refused("the planned changes overlap; nothing was changed");
            }
            end = p.offset + p.data.len() as u64;
        }
        Ok(plan)
    }

    /// The trailing ZIP's part of a rename: `playgo-chunk.crc` for every block the container's
    /// changes touch, `pfsimage.xml`'s digests and name, and the ZIP's CRC-32 of each.
    fn plan_zip(&mut self, zip: &si::Si, digests: &ManifestDigests, plan: &mut Plan) -> Result<()> {
        let file = &mut self.file;
        let in_container = plan.patches.clone();
        if in_container
            .iter()
            .any(|p| p.offset + p.data.len() as u64 > zip.zip_start)
        {
            return refused("the package's container runs into the ZIP that ends it");
        }
        let mut blocks: Vec<u64> = in_container
            .iter()
            .flat_map(|p| p.offset / BLOCK..=(p.offset + p.data.len() as u64 - 1) / BLOCK)
            .collect();
        blocks.sort_unstable();
        blocks.dedup();

        if let Some(m) = zip
            .members
            .iter()
            .find(|m| m.name.ends_with("/playgo-chunk.crc"))
        {
            if m.size != zip.zip_start.div_ceil(BLOCK) * 4 || m.size > MAX_CRC_TABLE {
                return refused(
                    "the package's block checksums (playgo-chunk.crc) are not one per 64 KiB \
                     block, so they can't be brought up to date",
                );
            }
            let old = file.read_at(m.offset, m.size as usize)?;
            let mut new = old.clone();
            for b in &blocks {
                let start = b * BLOCK;
                let len = (zip.zip_start - start).min(BLOCK) as usize;
                let mut block = file.read_at(start, len)?;
                let at = *b as usize * 4;
                if old[at..at + 4] != crc32c(&block).to_le_bytes() {
                    return refused(
                        "the package's block checksums (playgo-chunk.crc) don't match its \
                         bytes: it is damaged or made another way, and is left as it is",
                    );
                }
                overlay(&mut block, start, &in_container);
                new[at..at + 4].copy_from_slice(&crc32c(&block).to_le_bytes());
                changed(
                    m.offset,
                    at,
                    &old[at..at + 4],
                    &new[at..at + 4],
                    &mut plan.patches,
                );
            }
            zip_crc(file, m, &old, &new, plan)?;
            plan.crc_blocks = blocks;
        }

        if let Some(m) = zip
            .members
            .iter()
            .find(|m| m.name.ends_with("pfsimage.xml"))
        {
            if m.size > MAX_MANIFEST {
                return refused("the package's manifest (pfsimage.xml) is too large to rewrite");
            }
            let old = file.read_at(m.offset, m.size as usize)?;
            let mut new = match set_manifest_name(&old, &plan.previous, &plan.name)? {
                Some(renamed) => renamed,
                None => old.clone(),
            };
            for (tag, (was, now)) in [
                ("param-digest", &digests.param),
                ("package-digest", &digests.package),
                ("body-digest", &digests.body),
            ] {
                set_manifest_digest(&mut new, tag, was, now).map_err(Error::Format)?;
            }
            changed(m.offset, 0, &old, &new, &mut plan.patches);
            zip_crc(file, m, &old, &new, plan)?;
        }
        Ok(())
    }
}

/// The digests a manifest repeats, each as it was and as it becomes.
struct ManifestDigests {
    param: ([u8; 32], [u8; 32]),
    package: ([u8; 32], [u8; 32]),
    body: ([u8; 32], [u8; 32]),
}

/// A ZIP member's CRC-32 in its local header and in the central directory, brought up to
/// date when both held the right one for the member as it was (a ZIP whose CRCs were already
/// off is not "repaired" on the way).
fn zip_crc(
    file: &mut PkgFile,
    m: &si::Member,
    old: &[u8],
    new: &[u8],
    plan: &mut Plan,
) -> Result<()> {
    if old == new {
        return Ok(());
    }
    let local = file.read_at(m.local + 14, 4)?;
    let was = crc32fast::hash(old).to_le_bytes();
    if m.flags & 8 != 0 || local != was || m.crc.to_le_bytes() != was {
        return Ok(());
    }
    let now = crc32fast::hash(new).to_le_bytes();
    changed(m.local + 14, 0, &was, &now, &mut plan.patches);
    changed(m.central + 16, 0, &was, &now, &mut plan.patches);
    plan.zip_members.push(m.name.clone());
    Ok(())
}

// ─── PS4 ────────────────────────────────────────────────────────────────────────────────

/// One name of a `param.sfo`, placed in the container's bytes.
#[derive(Debug, Clone)]
struct SfoName {
    key: String,
    value: String,
    /// Where its length (the text and its zero byte) is recorded.
    len_at: usize,
    /// Its room: `max_len` bytes.
    data: Range<usize>,
}

fn is_name_key(key: &str) -> bool {
    key == "TITLE"
        || key
            .strip_prefix("TITLE_")
            .is_some_and(|n| n.len() == 2 && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The names of the `param.sfo` at `at` of `bytes` (text values only).
fn sfo_names(bytes: &[u8], at: Range<usize>) -> std::result::Result<Vec<SfoName>, String> {
    let sfo = &bytes[at.clone()];
    let le32 = |i: usize| u32::from_le_bytes(sfo[i..i + 4].try_into().unwrap()) as usize;
    if sfo.len() < 0x14 || &sfo[..4] != b"\0PSF" {
        return Err("the package's param.sfo does not read".into());
    }
    let (keys_at, data_at, count) = (le32(8), le32(12), le32(16));
    if count > 256 || 0x14 + count * 16 > sfo.len() || keys_at > sfo.len() {
        return Err("the package's param.sfo is out of range".into());
    }
    let mut out = Vec::new();
    for i in 0..count {
        let e = 0x14 + i * 16;
        let key_at = keys_at + u16::from_le_bytes([sfo[e], sfo[e + 1]]) as usize;
        let Some(key_end) = sfo
            .get(key_at..)
            .and_then(|k| k.iter().position(|b| *b == 0))
        else {
            continue;
        };
        let key = String::from_utf8_lossy(&sfo[key_at..key_at + key_end]).into_owned();
        if !is_name_key(&key) {
            continue;
        }
        let format = u16::from_le_bytes([sfo[e + 2], sfo[e + 3]]);
        let (len, room) = (le32(e + 4), le32(e + 8));
        let start = data_at.checked_add(le32(e + 12));
        let end = start.and_then(|s| s.checked_add(room));
        let (Some(start), Some(end)) = (start, end) else {
            return Err(format!("{key} in the package's param.sfo is out of range"));
        };
        if format != 0x0204 || len > room || end > sfo.len() {
            return Err(format!(
                "{key} in the package's param.sfo is not plain text in its own room"
            ));
        }
        let text = &sfo[start..start + len];
        let text = &text[..text.iter().position(|b| *b == 0).unwrap_or(text.len())];
        out.push(SfoName {
            key,
            value: String::from_utf8_lossy(text).into_owned(),
            len_at: at.start + e + 4,
            data: at.start + start..at.start + end,
        });
    }
    Ok(out)
}

struct Ps4 {
    file: PkgFile,
    cnt: Cnt,
    names: Vec<SfoName>,
    /// Whether the general digests hold `param.sfo`'s digest (some writers leave it unset).
    general_holds_param: bool,
    refusal: Option<Refusal>,
}

/// The general digests' `param` slot, in a PS4 container's bytes.
fn ps4_param_slot(cnt: &Cnt) -> Option<Range<usize>> {
    let general = place(cnt, ids::GENERAL_DIGESTS)?;
    (general.len() >= 0xE0).then(|| general.start + 0xC0..general.start + 0xE0)
}

/// `CNT+0x100` of a PS4 package: the keys, the image key, the general digests, the entry
/// table and the digest table.
fn ps4_rollup(cnt: &Cnt) -> Option<[u8; 32]> {
    let mut pre = Vec::new();
    for id in [
        ids::ENTRY_KEYS,
        ids::IMAGE_KEY,
        ids::GENERAL_DIGESTS,
        ids::METAS,
        ids::DIGESTS,
    ] {
        pre.extend_from_slice(&cnt.bytes[place(cnt, id)?]);
    }
    Some(sha256(&pre))
}

/// Whether every digest a PS4 rename recomputes holds in the container as it is, and whether
/// the general digests hold `param.sfo`'s.
fn ps4_holds(cnt: &Cnt) -> std::result::Result<bool, String> {
    let digest_of = |id: u32| place(cnt, id).map(|r| sha256(&cnt.bytes[r]));
    let at = |r: Option<Range<usize>>| r.map(|r| &cnt.bytes[r]);
    let same = |held: Option<&[u8]>, want: Option<[u8; 32]>| match (held, want) {
        (Some(h), Some(w)) => h == w,
        _ => false,
    };
    let sfo = digest_of(PARAM_SFO);
    let in_general = at(ps4_param_slot(cnt));
    let general_holds_param = same(in_general, sfo);
    let unset = in_general.is_none_or(|s| s.iter().all(|b| *b == 0));
    let checks = [
        (
            "param.sfo's entry digest",
            same(at(table_slot(cnt, PARAM_SFO)), sfo),
        ),
        (
            "the general digests' entry digest",
            same(
                at(table_slot(cnt, ids::GENERAL_DIGESTS)),
                digest_of(ids::GENERAL_DIGESTS),
            ),
        ),
        (
            "the general digests' param slot",
            general_holds_param || unset,
        ),
        (
            "the first header digest",
            same(Some(&cnt.bytes[0x100..0x120]), ps4_rollup(cnt)),
        ),
        (
            "the second header digest",
            same(
                Some(&cnt.bytes[0x120..0x140]),
                second_rollup_input(cnt).map(|p| sha256(&p)),
            ),
        ),
        (
            "the digest table's digest",
            same(Some(&cnt.bytes[0x140..0x160]), digest_of(ids::DIGESTS)),
        ),
        (
            "the body digest",
            same(Some(&cnt.bytes[0x160..0x180]), body(cnt).map(sha256)),
        ),
        (
            "the header digest",
            cnt.bytes[0xFE0..0x1000] == sha256(&cnt.bytes[..0xFE0]),
        ),
    ];
    match checks.iter().find(|(_, ok)| !ok) {
        Some((name, _)) => Err(format!(
            "{name} does not hold in this package, so its digests follow a rule this app does \
             not know (or the package is damaged)"
        )),
        None => Ok(general_holds_param),
    }
}

/// The header signature LibOrbisPkg writes for a fake package.
fn ps4_signature(cnt: &Cnt) -> Vec<u8> {
    crate::ps4_pkg_write::mt_wrap(
        crate::ps4_pkg_write::public_key(3),
        &sha256(&cnt.bytes[..SIGNATURE_AT]),
    )
}

impl Ps4 {
    fn load(mut file: PkgFile) -> Result<Self> {
        let len = file.len();
        let cnt = cnt::read(&mut file, 0)?;
        let mut names = Vec::new();
        let mut refusal = None;
        let mut general_holds_param = false;
        match cnt.entry(PARAM_SFO) {
            None => refusal = Some(Refusal::new("no_name", "the package has no param.sfo")),
            Some(e) if e.flags1 & ENCRYPTED != 0 => {
                refusal = Some(Refusal::new(
                    "unsupported",
                    "the package's param.sfo is encrypted",
                ))
            }
            Some(e) => {
                let range = e.offset as usize..e.offset as usize + e.size as usize;
                match sfo_names(&cnt.bytes, range) {
                    Ok(n) if !n.iter().any(|n| n.key == "TITLE") => {
                        names = n;
                        refusal = Some(Refusal::new(
                            "no_name",
                            "the package's param.sfo holds no name (TITLE)",
                        ));
                    }
                    Ok(n) => names = n,
                    Err(why) => refusal = Some(Refusal::new("unsupported", why)),
                }
            }
        }
        if cnt.bytes.len() < SIGNATURE_AT + 0x100 {
            refusal = Some(Refusal::new(
                "unsupported",
                "the package's container is too short to hold its header",
            ));
        } else {
            // A fake package: its image opens with the keys the all-zero passcode gives, or
            // (with no image to ask) its header carries the signature a fake-package writer
            // makes. Anything else is its publisher's, or made in a way not known here.
            let fake = match crate::ps4_pkg::zero_passcode_image(&mut file, len, &cnt.content_id)? {
                Some(ours) => ours,
                None => cnt.bytes[SIGNATURE_AT..SIGNATURE_AT + 0x100] == ps4_signature(&cnt)[..],
            };
            if !fake {
                refusal = Some(Refusal::new(
                    "retail",
                    "this is not a fake package made with the usual all-zero passcode (an \
                     official package, or one made another way): its name is under a signature \
                     that can't be made again here, so it can't be changed",
                ));
            } else if refusal.is_none() {
                match ps4_holds(&cnt) {
                    Ok(holds) => general_holds_param = holds,
                    Err(why) => refusal = Some(Refusal::new("unsupported", why)),
                }
            }
        }
        Ok(Self {
            file,
            cnt,
            names,
            general_holds_param,
            refusal,
        })
    }

    fn names(&self) -> Names {
        let entries: Vec<NameEntry> = self
            .names
            .iter()
            .map(|n| NameEntry {
                key: n.key.clone(),
                name: n.value.clone(),
            })
            .collect();
        let default = entries.iter().position(|e| e.key == "TITLE");
        names_of(
            Platform::Ps4,
            &self.cnt.content_id,
            entries,
            default,
            self.refusal.clone(),
        )
    }

    /// [`Ps4::names`] with the room a new name has: the smallest `param.sfo` string it would
    /// go into, less its zero byte.
    fn names_with_room(&self) -> Names {
        let mut names = self.names();
        if names.refusal.is_some() {
            return names;
        }
        let room = |all: bool| {
            targets(&names.entries, &names.name, all)
                .iter()
                .map(|i| self.names[*i].data.len().saturating_sub(1))
                .min()
                .unwrap_or(MAX_NAME_BYTES)
                .min(MAX_NAME_BYTES)
        };
        names.room = Some(Room {
            default_name: room(false),
            all_languages: room(true),
        });
        names
    }

    fn plan(mut self, name: &str, all: bool, reader_reads_name: bool) -> Result<Plan> {
        let names = self.names();
        if let Some(r) = &self.refusal {
            return refused(r.reason.clone());
        }
        let which = targets(&names.entries, &names.name, all);
        let mut expect = names.entries.clone();
        for i in &which {
            expect[*i].name = name.to_string();
        }
        let mut plan = Plan {
            platform: Platform::Ps4,
            previous: names.name.clone(),
            name: name.to_string(),
            changed: which
                .iter()
                .filter(|i| names.entries[**i].name != name)
                .count(),
            patches: Vec::new(),
            len: self.file.len(),
            expect,
            reader_reads_name,
            held: Vec::new(),
            crc_blocks: Vec::new(),
            zip_members: Vec::new(),
        };
        if plan.changed == 0 {
            return Ok(plan);
        }
        for i in &which {
            let room = self.names[*i].data.len();
            if name.len() + 1 > room {
                return refused(format!(
                    "the new name is too long for this package: {} holds at most {} bytes and \
                     the name is {}",
                    self.names[*i].key,
                    room.saturating_sub(1),
                    name.len()
                ));
            }
        }
        let cnt = &mut self.cnt;
        let missing = || Error::Format("the container lost an entry while it was read".into());
        let sfo = place(cnt, PARAM_SFO).ok_or_else(missing)?;
        let table = place(cnt, ids::DIGESTS).ok_or_else(missing)?;
        let general = place(cnt, ids::GENERAL_DIGESTS).ok_or_else(missing)?;
        let regions = [
            sfo.clone(),
            table.clone(),
            general.clone(),
            0x100..0x180,
            0xFE0..SIGNATURE_AT + 0x100,
        ];
        // A package that keeps a digest of its own first blocks (the PlayGo chunk digests
        // cover the image; a writer may fill the header's blocks too) would need those redone
        // as well, and they sit in the body they would then change: left alone.
        if let Some(chunks) = place(cnt, PS4_CHUNK_SHA) {
            let covered = regions.iter().any(|r| {
                (r.start / BLOCK as usize..=(r.end - 1) / BLOCK as usize).any(|b| {
                    cnt.bytes[chunks.clone()]
                        .get(b * 4..b * 4 + 4)
                        .is_some_and(|d| d.iter().any(|x| *x != 0))
                })
            });
            if covered {
                return refused(
                    "this package keeps a digest of its own header blocks, which can't be \
                     brought up to date here",
                );
            }
        }
        let saved: Vec<Vec<u8>> = regions
            .iter()
            .map(|r| cnt.bytes[r.clone()].to_vec())
            .collect();

        for i in &which {
            let n = &self.names[*i];
            cnt.bytes[n.data.clone()].fill(0);
            cnt.bytes[n.data.start..n.data.start + name.len()].copy_from_slice(name.as_bytes());
            cnt.bytes[n.len_at..n.len_at + 4]
                .copy_from_slice(&(name.len() as u32 + 1).to_le_bytes());
        }
        let sfo_digest = sha256(&cnt.bytes[sfo.clone()]);
        let sfo_in_table = table_slot(cnt, PARAM_SFO).ok_or_else(missing)?;
        cnt.bytes[sfo_in_table].copy_from_slice(&sfo_digest);
        if self.general_holds_param {
            let slot = ps4_param_slot(cnt).ok_or_else(missing)?;
            cnt.bytes[slot].copy_from_slice(&sfo_digest);
        }
        let general_digest = sha256(&cnt.bytes[general.clone()]);
        let general_in_table = table_slot(cnt, ids::GENERAL_DIGESTS).ok_or_else(missing)?;
        cnt.bytes[general_in_table].copy_from_slice(&general_digest);
        let rollup = ps4_rollup(cnt).ok_or_else(missing)?;
        cnt.bytes[0x100..0x120].copy_from_slice(&rollup);
        let second = sha256(&second_rollup_input(cnt).ok_or_else(missing)?);
        cnt.bytes[0x120..0x140].copy_from_slice(&second);
        let table_digest = sha256(&cnt.bytes[table.clone()]);
        cnt.bytes[0x140..0x160].copy_from_slice(&table_digest);
        let body_digest = sha256(body(cnt).ok_or_else(missing)?);
        cnt.bytes[0x160..0x180].copy_from_slice(&body_digest);
        let header_digest = sha256(&cnt.bytes[..0xFE0]);
        cnt.bytes[0xFE0..0x1000].copy_from_slice(&header_digest);
        let signature = ps4_signature(cnt);
        cnt.bytes[SIGNATURE_AT..SIGNATURE_AT + signature.len()].copy_from_slice(&signature);
        if let Err(why) = ps4_holds(cnt) {
            return refused(format!(
                "the renamed container did not come out right ({why}); nothing was changed"
            ));
        }
        for (r, old) in regions.iter().zip(&saved) {
            changed(0, r.start, old, &cnt.bytes[r.clone()], &mut plan.patches);
        }
        plan.patches.sort_by_key(|p| p.offset);
        Ok(plan)
    }
}

// ─── The three calls ────────────────────────────────────────────────────────────────────

enum Loaded {
    Ps5(Box<Ps5>),
    Ps4(Box<Ps4>),
    /// Recognised, and never renamed.
    Refused(Names),
}

fn load(path: &Path) -> Result<Loaded> {
    let mut file = PkgFile::open(path)?;
    if file.len() < 4 {
        return format_err("this is not a PS4 or PS5 package");
    }
    let magic = file.read_at(0, 4)?;
    if crate::lih::is_lih(&magic) {
        return Ok(Loaded::Refused(names_of(
            Platform::Ps5,
            "",
            Vec::new(),
            None,
            Some(Refusal::new(
                "patch",
                "this is an update that holds only what changed (a delta): the name comes \
                 from the game it updates",
            )),
        )));
    }
    match magic.as_slice() {
        [0x7F, b'F', b'I', b'H'] => Ok(match Ps5::load(file)? {
            Some(p) => Loaded::Ps5(Box::new(p)),
            None => Loaded::Refused(names_of(
                Platform::Ps5,
                "",
                Vec::new(),
                None,
                Some(Refusal::new(
                    "retail",
                    "this is an official package: its name is under its publisher's signature, \
                     which only the publisher can make again, so it can't be changed",
                )),
            )),
        }),
        [0x7F, b'C', b'N', b'T'] => Ok(Loaded::Ps4(Box::new(Ps4::load(file)?))),
        _ => format_err("this is not a PS4 or PS5 package"),
    }
}

/// The names the package at `path` holds, and whether they can be changed.
pub fn read(path: &Path) -> Result<Names> {
    Ok(match load(path)? {
        Loaded::Ps5(mut p) => p.names_with_room(),
        Loaded::Ps4(p) => p.names_with_room(),
        Loaded::Refused(names) => names,
    })
}

/// Plans giving the package at `path` the name `name`: the entries that hold its current
/// default name change (every entry with `all_languages`). Nothing is written. An error says
/// why it can't be done; a plan with no patches means it already has that name.
pub fn plan(path: &Path, name: &str, all_languages: bool) -> Result<Plan> {
    let name = clean_name(name)?;
    let loaded = load(path)?;
    let previous = match &loaded {
        Loaded::Ps5(p) => p.names().name,
        Loaded::Ps4(p) => p.names().name,
        Loaded::Refused(names) => {
            return refused(
                names
                    .refusal
                    .as_ref()
                    .map_or("this package can't be renamed", |r| r.reason.as_str()),
            )
        }
    };
    // Whether the app's package reader shows this name: then it must show the new one.
    let reader_reads_name = ps5upload_pkg::parse_pkg(path).is_ok_and(|m| m.title == previous);
    match loaded {
        Loaded::Ps5(p) => p.plan(&name, all_languages, reader_reads_name),
        Loaded::Ps4(p) => p.plan(&name, all_languages, reader_reads_name),
        Loaded::Refused(_) => unreachable!("returned above"),
    }
}

/// Reads the package at `path` back from disk after `plan`'s patches were written, and says
/// what is wrong with it if anything is: its length, its names, every digest the change
/// recomputed (and every container check that held before), its block CRCs, and what the
/// app's own package reader shows.
pub fn check(path: &Path, plan: &Plan) -> Result<()> {
    let wrong = |what: String| -> Result<()> {
        refused(format!("the renamed package does not check out: {what}"))
    };
    let loaded = load(path)?;
    let names = match &loaded {
        Loaded::Ps5(p) => p.names(),
        Loaded::Ps4(p) => p.names(),
        Loaded::Refused(names) => names.clone(),
    };
    if let Some(r) = &names.refusal {
        return wrong(r.reason.clone());
    }
    if names.entries != plan.expect || names.name != plan.name {
        return wrong("its names are not the ones written".into());
    }
    match loaded {
        Loaded::Ps5(mut p) => {
            if p.file.len() != plan.len {
                return wrong("its length changed".into());
            }
            let held = p.held();
            if let Some(lost) = plan.held.iter().find(|name| !held.contains(name)) {
                return wrong(format!("{lost} no longer holds"));
            }
            if !plan.crc_blocks.is_empty() || !plan.zip_members.is_empty() {
                let Some(zip) = si::read(&mut p.file)? else {
                    return wrong("the ZIP that ends it no longer reads".into());
                };
                if let Some(m) = zip
                    .members
                    .iter()
                    .find(|m| m.name.ends_with("/playgo-chunk.crc"))
                {
                    for b in &plan.crc_blocks {
                        let start = b * BLOCK;
                        let len = (zip.zip_start - start).min(BLOCK) as usize;
                        let block = p.file.read_at(start, len)?;
                        if p.file.read_at(m.offset + b * 4, 4)? != crc32c(&block).to_le_bytes() {
                            return wrong(format!("block {b}'s checksum is wrong"));
                        }
                    }
                }
                for name in &plan.zip_members {
                    let Some(m) = zip.members.iter().find(|m| &m.name == name) else {
                        return wrong(format!("{name} is gone from its ZIP"));
                    };
                    let data = p.file.read_at(m.offset, m.size as usize)?;
                    let crc = crc32fast::hash(&data).to_le_bytes();
                    if m.crc.to_le_bytes() != crc || p.file.read_at(m.local + 14, 4)? != crc {
                        return wrong(format!("{name}'s ZIP checksum is wrong"));
                    }
                }
            }
        }
        Loaded::Ps4(p) => {
            if p.file.len() != plan.len {
                return wrong("its length changed".into());
            }
            if plan.changed > 0
                && p.cnt.bytes[SIGNATURE_AT..SIGNATURE_AT + 0x100] != ps4_signature(&p.cnt)[..]
            {
                return wrong("its header signature is not the one written".into());
            }
        }
        Loaded::Refused(_) => {}
    }
    if plan.reader_reads_name {
        match ps5upload_pkg::parse_pkg(path) {
            Ok(meta) if meta.title == plan.name => {}
            Ok(meta) => return wrong(format!("it reads as \"{}\"", meta.title)),
            Err(e) => return wrong(format!("it no longer reads ({e})")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
