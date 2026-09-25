//! The source of the game: a folder, or a mount image the same tree is read out of.

use std::path::{Path, PathBuf};

use crate::{format_err, Error, Result};

/// One file of the source tree: path relative to the user root, `/`-separated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: String,
    pub size: u64,
}

/// The game files to convert, whatever they live in. Sizes are known up front, so the
/// plan fixes every offset before the first byte is read. Images seek, so reading takes
/// `&mut self`.
pub trait SourceTree {
    fn files(&self) -> &[SourceFile];

    /// The whole file at `path`.
    fn read(&mut self, path: &str) -> Result<Vec<u8>>;

    /// Up to `len` bytes at `offset`. Sources override this so a four-byte module magic
    /// check does not read a 100 MB `eboot.bin` into memory.
    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let all = self.read(path)?;
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(all.len());
        let end = start.saturating_add(len).min(all.len());
        Ok(all[start..end].to_vec())
    }

    /// Directories with nothing in them, which a file list cannot express. The image keeps
    /// them: a game may look for one (Minecraft's `data/shaders`) and PSVIETHOA keeps them too.
    fn empty_dirs(&self) -> &[String] {
        &[]
    }

    /// One line for logs: what the source is and where it came from.
    fn describe(&self) -> String;
}

/// A game folder on the filesystem.
pub struct FolderSource {
    root: PathBuf,
    files: Vec<SourceFile>,
    empty_dirs: Vec<String>,
}

impl FolderSource {
    pub fn open(root: &Path) -> Result<Self> {
        let (files, empty_dirs) = scan_tree(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            files,
            empty_dirs,
        })
    }
}

impl SourceTree for FolderSource {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }

    fn empty_dirs(&self) -> &[String] {
        &self.empty_dirs
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        std::fs::read(self.root.join(path))
            .map_err(|e| Error::Io(std::io::Error::new(e.kind(), format!("{path}: {e}"))))
    }

    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = std::fs::File::open(self.root.join(path))
            .map_err(|e| Error::Io(std::io::Error::new(e.kind(), format!("{path}: {e}"))))?;
        file.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; len];
        let read = file.read(&mut buf)?;
        buf.truncate(read);
        Ok(buf)
    }

    fn describe(&self) -> String {
        format!("folder {}", self.root.display())
    }
}

/// Open whatever `path` names as a source tree.
pub fn open(path: &Path) -> Result<Box<dyn SourceTree>> {
    if path.is_dir() {
        return Ok(Box::new(FolderSource::open(path)?));
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "exfat" => Ok(Box::new(crate::exfat::ExFatSource::open(path)?)),
        "ffpkg" | "ufs2" => Ok(Box::new(crate::ufs2_source::Ufs2Source::open(path)?)),
        _ => format_err(format!(
            "{} is neither a folder nor a supported image (.exfat, .ffpkg)",
            path.display()
        )),
    }
}

/// Junk no package wants, skipped by name at any depth.
pub(crate) fn is_junk(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == ".ds_store"
        || lower == "thumbs.db"
        || lower == "desktop.ini"
        || lower == "system volume information"
        || lower == ".fseventsd"
        || lower == ".spotlight-v100"
        || name.starts_with("._")
}

/// Walk `root` (a game folder) into its file list, sizes from the filesystem only.
pub fn scan(root: &Path) -> Result<Vec<SourceFile>> {
    Ok(scan_tree(root)?.0)
}

/// [`scan`], plus the directories left with nothing in them once junk is skipped.
pub fn scan_tree(root: &Path) -> Result<(Vec<SourceFile>, Vec<String>)> {
    let mut out = Vec::new();
    let mut empty = Vec::new();
    walk(root, root, &mut out, &mut empty)?;
    out.sort_by(|a, b| a.path.cmp(&b.path));
    empty.sort();
    Ok((out, empty))
}

/// Walks one directory; returns whether anything under it was kept.
fn walk(
    root: &Path,
    dir: &Path,
    out: &mut Vec<SourceFile>,
    empty: &mut Vec<String>,
) -> Result<bool> {
    let rel = |path: &Path| -> Result<String> {
        Ok(path
            .strip_prefix(root)
            .map_err(|_| Error::Format(format!("{} escaped the source root", path.display())))?
            .to_string_lossy()
            .replace('\\', "/"))
    };
    let mut kept = false;
    let entries = std::fs::read_dir(dir).map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("{}: {e}", dir.display()),
        ))
    })?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_junk(&name) {
            continue;
        }
        let path = entry.path();
        // A symbolic link counts as what it points at, so a build can be staged as a folder of
        // links (to add or leave out a file) without copying or touching the game's own folder.
        let meta = if entry.file_type()?.is_symlink() {
            std::fs::metadata(&path)?
        } else {
            entry.metadata()?
        };
        if meta.is_dir() {
            if !walk(root, &path, out, empty)? {
                empty.push(rel(&path)?);
            }
            kept = true;
        } else if meta.is_file() {
            out.push(SourceFile {
                path: rel(&path)?,
                size: meta.len(),
            });
            kept = true;
        }
    }
    Ok(kept)
}

/// One readiness finding: what was checked, whether it holds, and what was seen.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Default)]
pub struct Readiness {
    pub checks: Vec<Check>,
}

impl Readiness {
    /// True when every check holds. Warnings are checks with `ok = false`.
    pub fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &Check> {
        self.checks.iter().filter(|c| !c.ok)
    }

    fn push(&mut self, name: &str, ok: bool, detail: impl Into<String>) {
        self.checks.push(Check {
            name: name.to_string(),
            ok,
            detail: detail.into(),
        });
    }
}

/// The module magics a launchable title may carry. As the payload's own note says
/// (`payload/include/elf_param.h`), a SELF magic means the module is *wrapped*, not that it
/// is encrypted — which console family the wrapper is for is what the magic distinguishes.
/// Measured: 17 of the 27 real game mounts carry the PS5 magic, 10 the PS4 one.
pub mod magic {
    pub const RAW_ELF: [u8; 4] = [0x7F, b'E', b'L', b'F'];
    pub const SELF_PS5: [u8; 4] = [0x54, 0x14, 0xF5, 0xEE];
    pub const SELF_PS4: [u8; 4] = [0x4F, 0x15, 0x3D, 0x1D];
    /// A genuine (Sony-signed) SELF.
    pub const SIGNED_SELF: [u8; 4] = [0x53, 0x43, 0x45, 0x00];
}

/// A `param.json`'s bytes, parsed (it may carry a BOM).
pub(crate) fn parse_param_json(bytes: &[u8]) -> Option<serde_json::Value> {
    let text = String::from_utf8_lossy(bytes);
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

/// The content id a `param.json` declares, if any.
pub fn content_id(param_json: &[u8]) -> Option<String> {
    let json = parse_param_json(param_json)?;
    let id = json.get("contentId")?.as_str()?;
    Some(id.to_string())
}

/// The DRM value a debug package must carry. A `"free"` (or `"upgradable"`) source makes
/// the console show a lock and refuse to start the title, so the value is rewritten in the
/// package — the user's own file is never touched.
pub const STANDARD_DRM: &str = "standard";

/// The byte span of the *inside* of the string value of the first `key` a `param.json`
/// carries, so a rewrite can replace one value and leave the file's formatting alone.
fn string_value_span(text: &str, key: &str) -> Option<(usize, usize)> {
    let at = text.find(key)?;
    let rest = &text[at + key.len()..];
    let colon = rest.find(':')?;
    let after = &rest[colon + 1..];
    let open = after.find('\"')?;
    let start = at + key.len() + colon + 1 + open + 1;
    let close = text[start..].find('\"')?;
    Some((start, start + close))
}

/// `param.json` with the first `key`'s string value replaced by `value`.
fn set_string_value(param_json: &[u8], key: &str, value: &str) -> Option<Vec<u8>> {
    let text = String::from_utf8_lossy(param_json);
    let (start, end) = string_value_span(&text, key)?;
    let mut out = Vec::with_capacity(param_json.len() + value.len());
    out.extend_from_slice(&param_json[..start]);
    out.extend_from_slice(value.as_bytes());
    out.extend_from_slice(&param_json[end..]);
    Some(out)
}

/// `param.json` with a `"name": "value"` field added after its opening brace, carrying the
/// file's own newline and indentation so an already-formatted file stays readable.
fn insert_field(param_json: &[u8], name: &str, value: &str) -> Option<Vec<u8>> {
    insert_raw_field(param_json, name, &format!("\"{value}\""))
}

/// [`insert_field`] for a value already written as JSON (a number, say).
pub(crate) fn insert_raw_field(param_json: &[u8], name: &str, raw: &str) -> Option<Vec<u8>> {
    let text = String::from_utf8_lossy(param_json);
    let open = text.find('{')?;
    let at = open + 1;
    // The file's own line ending: Sony's tools write CRLF, most others LF.
    let nl = if text[at..].starts_with("\r\n") {
        "\r\n"
    } else if text[at..].starts_with('\n') {
        "\n"
    } else {
        ""
    };
    // The indentation the file already uses for its first key, so the inserted field lines up.
    let pad: String = text[at..]
        .chars()
        .skip_while(|c| *c != '\n')
        .skip(1)
        .take_while(|c| c.is_whitespace())
        .collect();
    let sep = if nl.is_empty() { ":" } else { ": " };
    let mut out = Vec::with_capacity(param_json.len() + name.len() + raw.len() + 8);
    out.extend_from_slice(text[..at].as_bytes());
    out.extend_from_slice(format!("{nl}{pad}\"{name}\"{sep}{raw},").as_bytes());
    out.extend_from_slice(text[at..].as_bytes());
    Some(out)
}

/// `param.json` with `requiredSystemSoftwareVersion` replaced by `version` (a BCD hex word
/// such as `0x0510000000000000` for 5.10), or `None` when the source does not declare one.
///
/// The console refuses a package whose declared minimum is above its own firmware —
/// measured on the Phat at 5.10 with a title declaring 12.60: `state=9 error=0x80a3000d`.
/// The value goes into the install metadata, so a title whose minimum is above the console
/// can be made installable; whether it then *runs* is a separate question that the
/// executable's own SDK version decides.
pub fn firmware_rewrite(param_json: &[u8], version: &str) -> Option<Vec<u8>> {
    set_string_value(param_json, "\"requiredSystemSoftwareVersion\"", version)
}

/// `param.json` with `applicationDrmType` set to `standard`, or `None` when it already is
/// (or does not say). Only the value's bytes change, so the file's formatting survives.
pub fn drm_rewrite(param_json: &[u8]) -> Option<Vec<u8>> {
    let text = String::from_utf8_lossy(param_json);
    let (start, end) = string_value_span(&text, "\"applicationDrmType\"")?;
    if text[start..end].eq_ignore_ascii_case(STANDARD_DRM) {
        return None;
    }
    set_string_value(param_json, "\"applicationDrmType\"", STANDARD_DRM)
}

/// `param.json` made safe to launch from a debug package, as PSVIETHOA's builder does in a
/// package that runs where ours showed a black screen (same Minecraft folder, FW 5.10):
///
/// - `versionFileUri` cleared: the game otherwise checks Sony's update server at start;
/// - `originContentVersion` / `targetContentVersion` removed: they mark a patch applied over a
///   base, which a standalone package is not;
/// - each `addcont.serviceIdForSharing` id blanked to spaces of the same length.
///
/// Only those bytes change. `None` when there is nothing to do.
pub fn launch_rewrite(param_json: &[u8]) -> Option<Vec<u8>> {
    let mut out = param_json.to_vec();
    let mut changed = false;
    let text = String::from_utf8_lossy(&out).into_owned();
    if let Some((start, end)) = string_value_span(&text, "\"versionFileUri\"") {
        if start < end {
            out = set_string_value(&out, "\"versionFileUri\"", "")?;
            changed = true;
        }
    }
    for key in ["\"originContentVersion\"", "\"targetContentVersion\""] {
        if let Some(next) = remove_string_field(&out, key) {
            out = next;
            changed = true;
        }
    }
    if let Some(next) = blank_string_array(&out, "\"serviceIdForSharing\"") {
        out = next;
        changed = true;
    }
    changed.then_some(out)
}

/// `param.json` without the first `key` and its string value, plus the comma that separated
/// it from its neighbour; `None` when the key is absent.
fn remove_string_field(param_json: &[u8], key: &str) -> Option<Vec<u8>> {
    let text = String::from_utf8_lossy(param_json);
    let key_at = text.find(key)?;
    let (_, value_end) = string_value_span(&text, key)?;
    let mut end = value_end + 1; // past the closing quote
                                 // Take the following comma, or if this was the last field, the preceding one.
    let after = &text[end..];
    let start;
    if let Some(comma) = after.find(',').filter(|&c| after[..c].trim().is_empty()) {
        end += comma + 1;
        // Also the line break and indentation up to the next key, so no blank line is left.
        let rest = &text[end..];
        let ws = rest.len() - rest.trim_start().len();
        end += ws;
        let before = &text[..key_at];
        let lead = before.len() - before.trim_end_matches([' ', '\t']).len();
        start = key_at - lead;
        // Keep the indentation of the removed line for the next key.
        let indent = &text[start..key_at];
        let mut out = param_json[..start].to_vec();
        out.extend_from_slice(indent.as_bytes());
        out.extend_from_slice(&param_json[end..]);
        return Some(out);
    }
    let before = &text[..key_at];
    let comma = before.rfind(',')?;
    if !before[comma + 1..].trim().is_empty() {
        return None;
    }
    start = comma;
    let mut out = param_json[..start].to_vec();
    out.extend_from_slice(&param_json[end..]);
    Some(out)
}

/// `param.json` with every string inside the first `key`'s array replaced by spaces of the same
/// length; `None` when the key is absent or already blank.
fn blank_string_array(param_json: &[u8], key: &str) -> Option<Vec<u8>> {
    let text = String::from_utf8_lossy(param_json);
    let at = text.find(key)? + key.len();
    let open = at + text[at..].find('[')?;
    let close = open + text[open..].find(']')?;
    let mut out = param_json.to_vec();
    let mut changed = false;
    let mut in_str = false;
    for i in open + 1..close {
        match out[i] {
            b'"' => in_str = !in_str,
            b' ' => {}
            _ if in_str => {
                out[i] = b' ';
                changed = true;
            }
            _ => {}
        }
    }
    changed.then_some(out)
}

/// The title id `content_id` belongs to: the part between its first `-` and its first `_`
/// (`UP4433-PPSA17221_00-…` → `PPSA17221`).
pub fn title_id_from_content_id(content_id: &str) -> Option<&str> {
    let rest = content_id.split_once('-')?.1;
    let id = rest.split_once('_')?.0;
    (!id.is_empty()).then_some(id)
}

/// `param.json` as the package must carry it: `contentId` set to the id the package is being
/// built under, and `titleId` set to match it.
///
/// Both fields have to agree with the transfer's own id. A package whose copy disagrees is
/// refused before a byte of it is committed — `content_id disagree pkg:… param.sfo:…`,
/// `CheckContentIdAgreement() ret = 80a3000f` — and a copy with no `titleId` at all fails
/// earlier still (`Invalid TitleId : [] strLength = 0`). A source named differently from the
/// package it is built into is the ordinary case, so the copy is rewritten rather than read.
/// The user's own file is untouched: the rewritten bytes are what the package carries.
pub fn content_id_rewrite(param_json: &[u8], content_id: &str) -> Option<Vec<u8>> {
    let json = parse_param_json(param_json)?;
    let title_id = title_id_from_content_id(content_id)?;
    let mut out = param_json.to_vec();
    for (name, value) in [("contentId", content_id), ("titleId", title_id)] {
        let key = format!("\"{name}\"");
        out = match json.get(name).and_then(|v| v.as_str()) {
            Some(current) if current == value => continue,
            Some(_) => set_string_value(&out, &key, value)?,
            None => insert_field(&out, name, value)?,
        };
    }
    Some(out)
}

/// The content version (`MM.mmm.ppp`) a `param.json` declares, packed as the 2-3-3 BCD
/// word the finalized-image header echoes at `0x9C`.
pub fn content_version_word(param_json: &[u8]) -> Option<u32> {
    let json = parse_param_json(param_json)?;
    let version = json.get("contentVersion")?.as_str()?;
    let digits: Vec<u8> = version
        .chars()
        .filter(|c| c.is_ascii_digit())
        .map(|c| c as u8 - b'0')
        .collect();
    if digits.len() != 8 || digits.iter().any(|d| *d > 9) {
        return None;
    }
    let byte = |a: u8, b: u8| (a << 4) | b;
    Some(u32::from_be_bytes([
        byte(digits[0], digits[1]),
        byte(digits[2], digits[3]),
        byte(digits[4], digits[5]),
        byte(digits[6], digits[7]),
    ]))
}

fn module_magic(tree: &mut dyn SourceTree, rel: &str) -> Option<[u8; 4]> {
    let head = tree.read_range(rel, 0, 4).ok()?;
    head.try_into().ok()
}

/// Bytes read per window while looking for an import name. Bounded so a 100 MB `eboot.bin`
/// costs a megabyte of buffer, not its whole size.
const AMPR_SCAN_WINDOW: usize = 1024 * 1024;
/// How far into a module the scan looks. Import names live in the dynamic section near the
/// front; reading further would cost time on every build for no added detection.
const AMPR_SCAN_LIMIT: u64 = 16 * 1024 * 1024;
/// The import whose presence means the title needs `ampr_emu` on the console.
const AMPR_LIB: &[u8] = b"libSceAmpr";

/// Does `rel` import `libSceAmpr`?
///
/// A title built against AMPR installs normally and then fails to start unless `ampr_emu` is
/// loaded, which presents as a black screen with nothing in the package to explain it. The
/// name is looked for as a literal in the module's own bytes: fake-SELF and raw-ELF modules —
/// the two kinds that can launch here — keep their import names in clear text.
///
/// Windows overlap by the name's length so a match lying across a boundary is still found;
/// without that, detection would depend on where in the file the string happened to land.
fn imports_ampr(tree: &mut dyn SourceTree, rel: &str) -> bool {
    let mut offset = 0u64;
    let mut carry: Vec<u8> = Vec::new();
    while offset < AMPR_SCAN_LIMIT {
        let Ok(chunk) = tree.read_range(rel, offset, AMPR_SCAN_WINDOW) else {
            return false;
        };
        if chunk.is_empty() {
            return false;
        }
        let read = chunk.len() as u64;
        // Prepend the tail of the previous window so a straddling name is contiguous here.
        let mut window = carry;
        window.extend_from_slice(&chunk);
        if window.windows(AMPR_LIB.len()).any(|w| w == AMPR_LIB) {
            return true;
        }
        let keep = window.len().saturating_sub(AMPR_LIB.len() - 1);
        carry = window.split_off(keep);
        offset += read;
        if read < AMPR_SCAN_WINDOW as u64 {
            return false; // short read: end of file
        }
    }
    false
}

/// Report readiness for a source tree. Never blocks: the caller decides which findings
/// matter for the build it is about to run.
pub fn readiness(tree: &mut dyn SourceTree) -> Readiness {
    let mut r = Readiness::default();
    let files = tree.files().to_vec();
    let has = |path: &str| files.iter().any(|f| f.path == path);

    let bytes: u64 = files.iter().map(|f| f.size).sum();
    r.push(
        "source",
        true,
        format!(
            "{}: {} files, {:.1} GiB",
            tree.describe(),
            files.len(),
            bytes as f64 / (1u64 << 30) as f64
        ),
    );

    r.push(
        "eboot.bin present",
        has("eboot.bin"),
        "the title module is the package root's eboot.bin",
    );
    r.push(
        "param.json present",
        has("sce_sys/param.json"),
        "a PS5 title uses param.json, not param.sfo",
    );
    r.push(
        "no param.sfo",
        !has("sce_sys/param.sfo"),
        "a param.sfo makes the launch path treat the title as PS4",
    );
    let param = tree.read("sce_sys/param.json").unwrap_or_default();
    match content_id(&param) {
        Some(id) => r.push(
            "content id",
            id.len() == 36 && id.is_ascii(),
            format!("{id} ({} chars)", id.len()),
        ),
        None => r.push("content id", false, "no contentId in sce_sys/param.json"),
    }
    if drm_rewrite(&param).is_some() {
        r.push(
            "drm",
            true,
            "applicationDrmType is rewritten to \"standard\" in the package; a free or \
             upgradable value makes the console lock the title",
        );
    }
    r.push(
        "icon0.png and icon0.dds present",
        has("sce_sys/icon0.png") && has("sce_sys/icon0.dds"),
        "both are carried as container entries",
    );
    r.push(
        "sce_sys/about/right.sprx present",
        has("sce_sys/about/right.sprx"),
        "the rights module a debug package ships",
    );
    // AMPR titles install fine and then will not start without ampr_emu loaded, which looks
    // like a broken package rather than a missing dependency. Say so before the build.
    if imports_ampr(tree, "eboot.bin") {
        r.push(
            "libSceAmpr import",
            false,
            "eboot.bin imports libSceAmpr: the title installs but will not start unless \
             ampr_emu is loaded on the console",
        );
    }

    if let Some(m) = module_magic(tree, "eboot.bin") {
        let (kind, launchable) = match m {
            magic::RAW_ELF => ("raw ELF", true),
            magic::SELF_PS5 => ("PS5 SELF wrapper", true),
            magic::SELF_PS4 => ("PS4 SELF wrapper", true),
            magic::SIGNED_SELF => ("genuine SELF", false),
            _ => ("unknown", false),
        };
        r.push(
            "eboot.bin module magic",
            launchable,
            format!("{kind} ({m:02x?})"),
        );
    }
    r
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn symlinks_count_as_their_targets() {
        let base = std::env::temp_dir().join(format!("fpkg-symlink-{}", std::process::id()));
        let (real, staged) = (base.join("real"), base.join("staged"));
        std::fs::remove_dir_all(&base).ok();
        std::fs::create_dir_all(real.join("sub")).unwrap();
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(real.join("sub/a.bin"), b"abc").unwrap();
        std::fs::write(real.join("eboot.bin"), b"12345").unwrap();
        std::os::unix::fs::symlink(real.join("sub"), staged.join("sub")).unwrap();
        std::os::unix::fs::symlink(real.join("eboot.bin"), staged.join("eboot.bin")).unwrap();
        let (files, empty) = super::scan_tree(&staged).unwrap();
        std::fs::remove_dir_all(&base).ok();
        let got: Vec<(String, u64)> = files.into_iter().map(|f| (f.path, f.size)).collect();
        assert_eq!(got, vec![("eboot.bin".into(), 5), ("sub/a.bin".into(), 3)]);
        assert!(empty.is_empty());
    }

    /// The launch rewrite leaves valid JSON with only the three launch fields changed.
    #[test]
    fn launch_rewrite_clears_update_and_patch_fields() {
        let src = b"{\r\n  \"addcont\": {\r\n    \"serviceIdForSharing\": [\"UP4433-CUSA00744_00\", \"UP4433-PPSA19634_00\"]\r\n  },\r\n  \"contentId\": \"UP4433-PPSA17221_00-MINECRAFTPS50000\",\r\n  \"originContentVersion\": \"01.000.000\",\r\n  \"targetContentVersion\": \"01.043.000\",\r\n  \"versionFileUri\": \"https://example/version.xml\"\r\n}\r\n";
        let out = launch_rewrite(src).unwrap();
        let v = parse_param_json(&out).expect("still valid JSON");
        assert_eq!(v["versionFileUri"], "");
        assert!(v.get("originContentVersion").is_none());
        assert!(v.get("targetContentVersion").is_none());
        assert_eq!(v["contentId"], "UP4433-PPSA17221_00-MINECRAFTPS50000");
        let ids = v["addcont"]["serviceIdForSharing"].as_array().unwrap();
        assert!(ids.iter().all(|s| s.as_str().unwrap().trim().is_empty()));
        assert_eq!(ids[0].as_str().unwrap().len(), 19);
        assert!(
            launch_rewrite(&out).is_none(),
            "a second pass has nothing to do"
        );
    }

    use super::*;

    /// A tree held in memory, so a readiness check can be exercised without touching disk.
    struct MemTree(Vec<(String, Vec<u8>)>);

    impl SourceTree for MemTree {
        fn files(&self) -> &[SourceFile] {
            // Readiness only asks `files()` for names and sizes; the checks under test read
            // bytes instead, so an empty slice keeps this helper to the point.
            &[]
        }
        fn read(&mut self, path: &str) -> Result<Vec<u8>> {
            self.0
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, b)| b.clone())
                .ok_or_else(|| crate::Error::Format(format!("no {path}")))
        }
        fn describe(&self) -> String {
            "mem".to_string()
        }
    }

    /// A title that imports libSceAmpr will install and then fail to start unless ampr_emu is
    /// loaded on the console. Detecting it at build time is the difference between a known
    /// requirement and an unexplained black screen.
    #[test]
    fn an_ampr_import_is_detected_in_the_module() {
        let mut eboot = vec![0u8; 4096];
        eboot[0..4].copy_from_slice(&magic::RAW_ELF);
        eboot[2048..2048 + b"libSceAmpr.prx".len()].copy_from_slice(b"libSceAmpr.prx");
        let mut tree = MemTree(vec![("eboot.bin".to_string(), eboot)]);
        assert!(imports_ampr(&mut tree, "eboot.bin"));
    }

    #[test]
    fn a_module_without_the_import_is_not_flagged() {
        let mut eboot = vec![0u8; 4096];
        eboot[0..4].copy_from_slice(&magic::RAW_ELF);
        eboot[2048..2048 + b"libSceGnmDriver".len()].copy_from_slice(b"libSceGnmDriver");
        let mut tree = MemTree(vec![("eboot.bin".to_string(), eboot)]);
        assert!(!imports_ampr(&mut tree, "eboot.bin"));
        // A module that is not there at all is not a reason to claim an AMPR dependency.
        assert!(!imports_ampr(&mut tree, "nope.bin"));
    }

    /// The scan reads in windows, so a name lying across a window boundary must still be
    /// found — otherwise detection would depend on where in the file the string happens to sit.
    #[test]
    fn an_import_spanning_a_scan_window_is_still_found() {
        let name = b"libSceAmpr";
        let mut eboot = vec![0u8; AMPR_SCAN_WINDOW * 2];
        eboot[0..4].copy_from_slice(&magic::RAW_ELF);
        let at = AMPR_SCAN_WINDOW - (name.len() / 2);
        eboot[at..at + name.len()].copy_from_slice(name);
        let mut tree = MemTree(vec![("eboot.bin".to_string(), eboot)]);
        assert!(imports_ampr(&mut tree, "eboot.bin"));
    }

    #[test]
    fn the_packaged_param_json_names_the_id_it_is_built_under() {
        // The console's GetRawContentInfo needs the packaged copy to carry `titleId`: a
        // package without one fails the install with `Invalid TitleId : [] strLength = 0`.
        let bare = br#"{"contentId":"UP4433-PPSA17221_00-MINECRAFTPS50000","contentVersion":"01.044.000"}"#;
        let out = content_id_rewrite(bare, "UP4433-PPSA17221_00-MINECRAFTPS50000")
            .expect("a title id is injected");
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains(r#""titleId":"PPSA17221""#), "{text}");
        assert!(text.contains(r#""contentId":"UP4433-PPSA17221_00-MINECRAFTPS50000""#));

        // A rename rewrites both fields: leaving `contentId` behind makes the console refuse
        // the transfer (`content_id disagree pkg:… param.sfo:…` → 0x80a3000f).
        let renamed = content_id_rewrite(bare, "UP0000-PPSA99012_00-MINIFI8TURE00013").unwrap();
        let text = String::from_utf8(renamed).unwrap();
        assert!(
            text.contains(r#""contentId":"UP0000-PPSA99012_00-MINIFI8TURE00013""#),
            "{text}"
        );
        assert!(text.contains(r#""titleId":"PPSA99012""#), "{text}");

        // An indented file keeps its shape rather than being collapsed onto one line.
        let pretty = b"{\n  \"contentId\": \"UP0000-PPSA99003_00-MINIFI8TURE00003\"\n}";
        let out = String::from_utf8(
            content_id_rewrite(pretty, "UP0000-PPSA99011_00-MINIFI8TURE00011").unwrap(),
        )
        .unwrap();
        assert!(out.contains("\n  \"titleId\": \"PPSA99011\","), "{out}");
        assert!(out.contains("\n}"), "{out}");

        // A source that names neither field gets both: the console compares the packaged copy
        // against the transfer's own id, so an unnamed copy is refused like a mismatched one.
        let out = content_id_rewrite(br#"{"titleName":"x"}"#, "UP0000-PPSA99011_00-X").unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains(r#""contentId":"UP0000-PPSA99011_00-X""#),
            "{text}"
        );
        assert!(text.contains(r#""titleId":"PPSA99011""#), "{text}");

        // Nothing to change, or nothing we can read: the source bytes are handed back.
        let same = br#"{"contentId":"UP0000-PPSA99011_00-X","titleId":"PPSA99011"}"#;
        assert_eq!(
            content_id_rewrite(same, "UP0000-PPSA99011_00-X").unwrap(),
            same
        );
        assert!(content_id_rewrite(b"not json", "UP0000-PPSA99011_00-X").is_none());
    }

    #[test]
    fn the_firmware_requirement_can_be_lowered_for_an_older_console() {
        // The console compares this value against its own firmware and refuses the package
        // when it is newer: measured on a 5.10 console against a title declaring 12.60,
        // `state=9 error=0x80a3000d`. Only the value's bytes change.
        let json = br#"{"contentId":"UP1004-PPSA30528_00-REDEMPTION000001","requiredSystemSoftwareVersion":"0x1260000000000000"}"#;
        let out = firmware_rewrite(json, "0x0510000000000000").unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains(r#""requiredSystemSoftwareVersion":"0x0510000000000000""#),
            "{text}"
        );
        assert!(text.contains(r#""contentId":"UP1004-PPSA30528_00-REDEMPTION000001""#));

        // A source that declares none is left alone rather than gaining a field: the console
        // treats an absent minimum as no minimum.
        assert!(firmware_rewrite(br#"{"titleName":"x"}"#, "0x0510000000000000").is_none());
    }

    #[test]
    fn junk_is_skipped_and_sizes_are_recorded() {
        let dir = std::env::temp_dir().join(format!("fpkg-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sce_sys/about")).unwrap();
        std::fs::write(dir.join("eboot.bin"), [0u8; 10]).unwrap();
        std::fs::write(dir.join("sce_sys/about/right.sprx"), [0u8; 5]).unwrap();
        std::fs::write(dir.join(".DS_Store"), [0u8; 3]).unwrap();
        std::fs::write(dir.join("._eboot.bin"), [0u8; 3]).unwrap();
        std::fs::create_dir_all(dir.join(".Spotlight-V100")).unwrap();
        std::fs::write(dir.join(".Spotlight-V100/x"), [0u8; 3]).unwrap();
        let files = scan(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(
            files,
            vec![
                SourceFile {
                    path: "eboot.bin".into(),
                    size: 10
                },
                SourceFile {
                    path: "sce_sys/about/right.sprx".into(),
                    size: 5
                },
            ]
        );
    }

    #[test]
    fn content_version_packs_as_bcd() {
        let dir = std::env::temp_dir().join(format!("fpkg-ver-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sce_sys")).unwrap();
        std::fs::write(
            dir.join("sce_sys/param.json"),
            br#"{"contentId":"UP0000-PPSA01234_00-TESTGAME00000000","contentVersion":"01.001.000"}"#,
        )
        .unwrap();
        let param = std::fs::read(dir.join("sce_sys/param.json")).unwrap();
        let word = content_version_word(&param);
        let id = content_id(&param);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(word, Some(0x0100_1000));
        assert_eq!(id.as_deref(), Some("UP0000-PPSA01234_00-TESTGAME00000000"));
    }

    #[test]
    fn a_folder_source_reads_ranges_and_reports_itself() {
        let dir = std::env::temp_dir().join(format!("fpkg-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("eboot.bin"), b"0123456789").unwrap();
        let mut tree = open(&dir).unwrap();
        assert_eq!(tree.read_range("eboot.bin", 2, 3).unwrap(), b"234");
        assert_eq!(tree.read_range("eboot.bin", 8, 99).unwrap(), b"89");
        assert_eq!(tree.read_range("eboot.bin", 99, 4).unwrap(), b"");
        assert_eq!(tree.read("eboot.bin").unwrap(), b"0123456789");
        assert!(tree.describe().starts_with("folder "));
        assert_eq!(tree.files().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_non_standard_drm_is_rewritten_in_place() {
        let free =
            br#"{"contentId":"X","applicationDrmType":"free","contentVersion":"01.000.000"}"#;
        let fixed = drm_rewrite(free).expect("free is rewritten");
        assert!(String::from_utf8_lossy(&fixed).contains("\"applicationDrmType\":\"standard\""));
        assert!(!String::from_utf8_lossy(&fixed).contains("free"));
        // Everything else survives, and the length grows only by the value's difference.
        assert_eq!(fixed.len(), free.len() + "standard".len() - "free".len());
        assert!(String::from_utf8_lossy(&fixed).starts_with(r#"{"contentId":"X","#));

        // Already standard (any case), or absent: nothing to do.
        assert!(drm_rewrite(br#"{"applicationDrmType":"Standard"}"#).is_none());
        assert!(drm_rewrite(br#"{"contentId":"X"}"#).is_none());
        assert!(drm_rewrite(b"not json at all").is_none());
    }

    /// The finding has to reach the report a user actually sees — a detector that works but
    /// is never called would be invisible, which is the failure this guards.
    #[test]
    fn readiness_reports_an_ampr_dependency() {
        let dir = std::env::temp_dir().join(format!(
            "fpkg-ready-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sce_sys")).unwrap();
        let mut eboot = vec![0u8; 8192];
        eboot[0..4].copy_from_slice(&magic::RAW_ELF);
        eboot[4096..4096 + b"libSceAmpr.prx".len()].copy_from_slice(b"libSceAmpr.prx");
        std::fs::write(dir.join("eboot.bin"), &eboot).unwrap();
        std::fs::write(
            dir.join("sce_sys/param.json"),
            br#"{"contentId":"UP0000-PPSA99011_00-X","requiredSystemSoftwareVersion":"0x1160000000000000"}"#,
        )
        .unwrap();

        let mut tree = open(&dir).unwrap();
        let readiness = readiness(tree.as_mut());

        // The AMPR finding is a WARNING, so it surfaces through `warnings()` — the same
        // iterator the build turns into the user-visible list.
        let ampr = readiness
            .warnings()
            .find(|c| c.name == "libSceAmpr import")
            .expect("an ampr warning");
        assert!(ampr.detail.contains("ampr_emu"), "{}", ampr.detail);
        assert!(
            !readiness.ok(),
            "an AMPR dependency must not read as all-clear"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn readiness_names_the_source_and_its_size() {
        let dir = std::env::temp_dir().join(format!("fpkg-named-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sce_sys")).unwrap();
        std::fs::write(dir.join("eboot.bin"), [0u8; 1024]).unwrap();
        std::fs::write(
            dir.join("sce_sys/param.json"),
            br#"{"applicationDrmType":"free"}"#,
        )
        .unwrap();
        let mut tree = open(&dir).unwrap();
        let readiness = readiness(tree.as_mut());
        let line = readiness
            .checks
            .iter()
            .find(|c| c.name == "source")
            .expect("a source line");
        assert!(line.ok);
        assert!(line.detail.contains("folder"), "{}", line.detail);
        assert!(line.detail.contains("2 files"), "{}", line.detail);
        // A non-standard DRM is reported, because the package will differ from the source.
        assert!(
            readiness.checks.iter().any(|c| c.name == "drm" && c.ok),
            "{:?}",
            readiness.checks
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unknown_source_kind_is_an_error() {
        let dir = std::env::temp_dir().join(format!("fpkg-kind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("game.iso");
        std::fs::write(&img, [0u8; 8]).unwrap();
        let Err(err) = open(&img) else {
            panic!("a .iso must not open as a source");
        };
        let err = err.to_string();
        assert!(err.contains(".exfat"), "{err}");
        assert!(err.contains(".ffpkg"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
