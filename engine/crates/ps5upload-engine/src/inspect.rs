//! What a game source is — a package, a split package, a mount image or a game
//! folder — as one description the package viewer renders.
//!
//! Only headers, PARAM, images and directories are read: never a whole package,
//! and never an encrypted entry (we have no console keys, and say so instead of
//! guessing).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use ps5upload_fpkg::remote_source;
use ps5upload_pkg::{
    parse_pkg_from, pkg_entries_from, read_pkg_entry_from, sfo_params, PkgAuthenticity, SfoValue,
};
use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct GameInspection {
    pub source: SourceInfo,
    pub identity: Identity,
    pub specs: Specs,
    /// The full PARAM (SFO or param.json), in source order.
    pub params: Vec<ParamField>,
    /// An update's `changeinfo.xml`, when readable.
    pub change_notes: Option<String>,
    pub images: Vec<ImageInfo>,
    /// `fake`, `retail`, `debug` or `unknown` for a package; `none` otherwise.
    pub authenticity: String,
    pub warnings: Vec<String>,
    /// Only header facts: PARAM was missing or encrypted.
    pub partial: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SourceInfo {
    /// `pkg`, `split-pkg`, `exfat`, `ffpkg`, `ffpfsc` or `folder`.
    pub format: String,
    /// `local`, `server` (a saved server) or `ps5` (the console).
    pub location: String,
    pub path: String,
    pub size: u64,
    pub parts: Vec<PartInfo>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PartInfo {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Identity {
    pub title: String,
    /// Title per language (param.json locale, or the SFO `TITLE_xx` code).
    pub titles: BTreeMap<String, String>,
    pub title_id: String,
    pub content_id: String,
    pub concept_id: Option<String>,
    /// `ps4`, `ps5` or empty.
    pub platform: String,
    pub category: String,
    /// `game`, `update`, `dlc`, `app` or `other`.
    pub content_type: String,
    pub region: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Specs {
    pub app_ver: String,
    pub master_ver: Option<String>,
    pub min_fw: Option<String>,
    pub sdk_ver: Option<String>,
    pub build_date: Option<String>,
    pub drm: Option<String>,
    pub age_rating: Option<String>,
    pub languages: Vec<String>,
    pub file_count: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ParamField {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ImageInfo {
    pub name: String,
    pub size: u64,
}

/// The images the viewer shows. Nothing else can be read through the image
/// route, so it can't be used to fetch arbitrary files.
const IMAGE_NAMES: [&str; 3] = ["icon0.png", "pic0.png", "pic1.png"];
const CHANGEINFO: &str = "changeinfo/changeinfo.xml";
const MAX_TEXT_BYTES: u32 = 1 << 20;
const MAX_IMAGE_BYTES: u32 = 8 << 20;

/// `0x0505_0000` → `5.05`: firmware is BCD in the top two bytes.
pub fn fw_from_sfo(v: u32) -> String {
    let major = (v >> 24) & 0xFF;
    let minor = (v >> 16) & 0xFF;
    let major = format!("{major:x}").parse::<u32>().unwrap_or(major);
    format!("{major}.{minor:02x}")
}

/// `0x0510000000000000` → `5.10` (param.json's 64-bit BCD word).
pub fn fw_from_hex_word(s: &str) -> Option<String> {
    let t = s.trim();
    let hex = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X"))?;
    let v = u64::from_str_radix(hex, 16).ok()?;
    Some(fw_from_sfo((v >> 32) as u32))
}

/// SDK version and build date from PS4 `PUBTOOLINFO`
/// (`c_date=20211126,sdk_ver=08008000,…` → `8.00`, `2021-11-26`).
pub fn sdk_from_pubtool(info: &str) -> (Option<String>, Option<String>) {
    let mut sdk = None;
    let mut date = None;
    for kv in info.split(',') {
        match kv.split_once('=') {
            Some(("sdk_ver", v)) if v.len() >= 4 && v.is_ascii() => {
                let major: u32 = v[..2].parse().unwrap_or(0);
                sdk = Some(format!("{major}.{}", &v[2..4]));
            }
            Some(("c_date", v)) if v.len() == 8 && v.is_ascii() => {
                date = Some(format!("{}-{}-{}", &v[..4], &v[4..6], &v[6..8]));
            }
            _ => {}
        }
    }
    (sdk, date)
}

/// Region from the content id's service prefix.
pub fn region_of(content_id: &str) -> Option<String> {
    Some(
        match content_id.get(..2)? {
            "UP" => "US",
            "EP" => "EU",
            "JP" => "JP",
            "HP" | "KP" => "Asia",
            "IP" => "Internal",
            _ => return None,
        }
        .to_string(),
    )
}

pub fn content_type_of(category: &str) -> String {
    match category {
        "gd" => "game",
        "gp" => "update",
        "ac" => "dlc",
        c if c.starts_with("gd") => "app",
        _ => "other",
    }
    .to_string()
}

fn authenticity_label(a: &PkgAuthenticity) -> String {
    match a {
        PkgAuthenticity::FakeDebug => "fake",
        PkgAuthenticity::Retail => "retail",
        _ => "unknown",
    }
    .to_string()
}

fn platform_from_title_id(title_id: &str) -> Option<&'static str> {
    if title_id.starts_with("PPSA") {
        Some("ps5")
    } else if title_id.starts_with("CUSA") {
        Some("ps4")
    } else {
        None
    }
}

/// Fill identity/specs/params from a decoded PARAM.SFO.
fn apply_sfo(g: &mut GameInspection, params: &[(String, SfoValue)]) {
    for (key, value) in params {
        let shown = match value {
            SfoValue::Text(t) => t.clone(),
            // Flags and BCD versions read best in hex; counts and levels don't.
            SfoValue::Int(i) if key.starts_with("ATTRIBUTE") || key.ends_with("_VER") => {
                format!("0x{i:08X}")
            }
            SfoValue::Int(i) => i.to_string(),
        };
        g.params.push(ParamField {
            key: key.clone(),
            value: shown,
        });
        match (key.as_str(), value) {
            ("TITLE", SfoValue::Text(t)) => g.identity.title = t.clone(),
            ("TITLE_ID", SfoValue::Text(t)) => g.identity.title_id = t.clone(),
            ("CONTENT_ID", SfoValue::Text(t)) if g.identity.content_id.is_empty() => {
                g.identity.content_id = t.clone()
            }
            ("CATEGORY", SfoValue::Text(t)) => g.identity.category = t.clone(),
            ("APP_VER", SfoValue::Text(t)) => g.specs.app_ver = t.clone(),
            ("VERSION", SfoValue::Text(t)) => g.specs.master_ver = Some(t.clone()),
            ("SYSTEM_VER", SfoValue::Int(v)) => g.specs.min_fw = Some(fw_from_sfo(*v)),
            ("PARENTAL_LEVEL", SfoValue::Int(v)) => g.specs.age_rating = Some(v.to_string()),
            ("PUBTOOLINFO", SfoValue::Text(t)) => {
                let (sdk, date) = sdk_from_pubtool(t);
                g.specs.sdk_ver = sdk.or(g.specs.sdk_ver.take());
                g.specs.build_date = date.or(g.specs.build_date.take());
            }
            (k, SfoValue::Text(t)) if k.starts_with("TITLE_") && k.len() == 8 && !t.is_empty() => {
                let code = k[6..].to_string();
                if code.chars().all(|c| c.is_ascii_digit()) {
                    g.specs.languages.push(code.clone());
                    g.identity.titles.insert(code, t.clone());
                }
            }
            _ => {}
        }
    }
}

/// Fill identity/specs/params from a PS5 `param.json`.
fn apply_param_json(g: &mut GameInspection, v: &serde_json::Value) {
    let Some(obj) = v.as_object() else { return };
    for (key, value) in obj {
        let shown = match value {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        g.params.push(ParamField {
            key: key.clone(),
            value: shown,
        });
    }
    let s = |k: &str| {
        obj.get(k)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    if let Some(t) = s("titleId") {
        g.identity.title_id = t;
    }
    if let Some(t) = s("contentId") {
        g.identity.content_id = t;
    }
    g.identity.concept_id = s("conceptId").or_else(|| obj.get("conceptId").map(|c| c.to_string()));
    if let Some(t) = s("contentVersion") {
        g.specs.app_ver = t;
    }
    g.specs.master_ver = s("masterVersion");
    g.specs.min_fw = s("requiredSystemSoftwareVersion").and_then(|w| fw_from_hex_word(&w));
    g.specs.sdk_ver = s("sdkVersion").and_then(|w| fw_from_hex_word(&w));
    g.specs.drm = s("applicationDrmType");
    g.specs.age_rating = obj
        .get("ageLevel")
        .and_then(|a| a.get("default"))
        .map(|d| d.to_string().trim_matches('"').to_string());
    if let Some(loc) = obj.get("localizedParameters").and_then(|l| l.as_object()) {
        let default = loc.get("defaultLanguage").and_then(|d| d.as_str());
        for (lang, params) in loc {
            if lang == "defaultLanguage" {
                continue;
            }
            if let Some(name) = params.get("titleName").and_then(|n| n.as_str()) {
                g.specs.languages.push(lang.clone());
                g.identity.titles.insert(lang.clone(), name.to_string());
                if Some(lang.as_str()) == default || g.identity.title.is_empty() {
                    g.identity.title = name.to_string();
                }
            }
        }
    }
    // A PS5 application declares `applicationCategoryType` 0 instead of a
    // category (a package's own header says game or patch, and wins).
    if g.identity.category.is_empty()
        && obj.get("applicationCategoryType").and_then(|c| c.as_u64()) == Some(0)
    {
        g.identity.category = "gd".to_string();
    }
    // PS5 keeps the build date in pubtools; the epoch means "not set".
    if let Some(date) = obj
        .get("pubtools")
        .and_then(|p| p.get("creationDate"))
        .and_then(|d| d.as_str())
        .and_then(|d| d.get(..10))
    {
        if date != "1970-01-01" {
            g.specs.build_date = Some(date.to_string());
        }
    }
    g.identity.platform = "ps5".to_string();
}

fn finish(g: &mut GameInspection) {
    if g.identity.title_id.is_empty() {
        // `get`, not slicing: a malformed id may split a multi-byte character.
        if let Some(t) = g.identity.content_id.get(7..16) {
            g.identity.title_id = t.to_string();
        }
    }
    if g.identity.platform.is_empty() {
        if let Some(p) = platform_from_title_id(&g.identity.title_id) {
            g.identity.platform = p.to_string();
        }
    }
    g.identity.region = region_of(&g.identity.content_id);
    g.identity.content_type = content_type_of(&g.identity.category);
}

/// Where a source lives, as `SourceInfo::location` says it.
fn location_of(path: &Path) -> &'static str {
    let p = path.to_string_lossy();
    if p.starts_with("remote://") {
        "server"
    } else if p.starts_with("ps5://") {
        "ps5"
    } else {
        "local"
    }
}

/// A file's bytes and size, here or through the remote opener.
fn open_bytes(path: &Path) -> Result<(Box<dyn ps5upload_fpkg::ReadSeek>, u64)> {
    if remote_source::is_remote(path) {
        return Ok(remote_source::open_file(path)?);
    }
    let f = std::fs::File::open(path)?;
    let size = f.metadata()?.len();
    Ok((Box::new(f), size))
}

fn format_of(path: &Path) -> Result<&'static str> {
    let remote = remote_source::is_remote(path);
    if !remote && path.is_dir() {
        return Ok("folder");
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    Ok(match ext.as_str() {
        "pkg" => "pkg",
        "exfat" => "exfat",
        "ffpkg" | "ufs2" => "ffpkg",
        "ffpfsc" => "ffpfsc",
        // A server path names no kind; anything else there is taken as a folder, and the
        // opener says so when it is not one.
        _ if remote => "folder",
        _ => bail!("not a package, image or game folder: {}", path.display()),
    })
}

/// Inspect a package, image or game folder on this computer.
pub fn inspect_local(path: &Path) -> Result<GameInspection> {
    match format_of(path)? {
        "pkg" => inspect_pkg(path),
        format => inspect_tree(path, format),
    }
}

fn inspect_pkg(path: &Path) -> Result<GameInspection> {
    // A split set is found by its sibling files, which only a local package has.
    let split = if remote_source::is_remote(path) {
        let (mut r, size) = open_bytes(path)?;
        let head = parse_pkg_from(&mut r, size, path)?;
        ps5upload_pkg::SplitPkgMetadata {
            parts: vec![path.to_path_buf()],
            part_sizes: vec![size],
            total_size: size,
            head,
        }
    } else {
        ps5upload_pkg::parse_split_pkg(path)?
    };
    let head = &split.head;
    let mut g = GameInspection {
        authenticity: authenticity_label(&head.authenticity),
        warnings: head.warnings.clone(),
        ..Default::default()
    };
    g.source = SourceInfo {
        format: if split.parts.len() > 1 {
            "split-pkg"
        } else {
            "pkg"
        }
        .to_string(),
        location: location_of(path).to_string(),
        path: path.display().to_string(),
        size: split.total_size,
        parts: split
            .parts
            .iter()
            .zip(&split.part_sizes)
            .map(|(p, s)| PartInfo {
                path: p.display().to_string(),
                size: *s,
            })
            .collect(),
    };
    g.identity.title = head.title.clone();
    g.identity.title_id = head.title_id.clone();
    g.identity.content_id = head.content_id.clone();
    g.identity.platform = head.platform.clone();
    g.identity.category = head.category.clone();
    g.specs.app_ver = head.app_ver.clone();

    let (mut r, size) = open_bytes(path)?;
    let entries = match pkg_entries_from(&mut r, size) {
        Ok(e) => e,
        Err(e) => {
            g.partial = true;
            g.warnings.push(format!("entry table unreadable: {e}"));
            finish(&mut g);
            return Ok(g);
        }
    };
    let by_name = |n: &str| entries.iter().find(|e| e.name.as_deref() == Some(n));
    let mut have_param = false;
    if let Some(e) = by_name("param.sfo") {
        if let Ok(bytes) = read_pkg_entry_from(&mut r, e, MAX_TEXT_BYTES) {
            if let Ok(params) = sfo_params(&bytes) {
                apply_sfo(&mut g, &params);
                have_param = true;
            }
        }
    }
    if let Some(e) = by_name("param.json") {
        if let Ok(bytes) = read_pkg_entry_from(&mut r, e, MAX_TEXT_BYTES) {
            let end = bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes[..end]) {
                apply_param_json(&mut g, &v);
                have_param = true;
            }
        }
    }
    if !have_param {
        g.partial = true;
        g.warnings
            .push("PARAM is encrypted or missing — showing header facts only".to_string());
    }
    for name in IMAGE_NAMES {
        if let Some(e) = by_name(name).filter(|e| !e.encrypted) {
            g.images.push(ImageInfo {
                name: name.to_string(),
                size: e.size as u64,
            });
        }
    }
    if let Some(e) = by_name(CHANGEINFO) {
        if let Ok(bytes) = read_pkg_entry_from(&mut r, e, MAX_TEXT_BYTES) {
            g.change_notes = Some(
                String::from_utf8_lossy(&bytes)
                    .trim_end_matches('\0')
                    .to_string(),
            );
        }
    }
    g.specs.file_count = Some(entries.len() as u64);
    // The header's platform is authoritative for a package (FIH = PS5).
    if !head.platform.is_empty() {
        g.identity.platform = head.platform.clone();
    }
    finish(&mut g);
    Ok(g)
}

fn inspect_tree(path: &Path, format: &str) -> Result<GameInspection> {
    let mut tree = ps5upload_fpkg::source::open(path)?;
    let files = tree.files().to_vec();
    let has = |p: &str| files.iter().any(|f| f.path == p);
    let mut too_large = Vec::new();
    // A text file this viewer reads whole, or None when it is absent, too
    // large (a damaged image can claim any size) or unreadable.
    let mut read_text = |tree: &mut Box<dyn ps5upload_fpkg::source::SourceTree>, p: &str| {
        let f = files.iter().find(|f| f.path == p)?;
        if f.size > u64::from(MAX_TEXT_BYTES) {
            too_large.push(p.to_string());
            return None;
        }
        tree.read(p).ok()
    };
    let mut g = GameInspection {
        authenticity: "none".to_string(),
        ..Default::default()
    };
    g.source = SourceInfo {
        format: format.to_string(),
        location: location_of(path).to_string(),
        path: path.display().to_string(),
        size: if format == "folder" {
            files.iter().map(|f| f.size).sum()
        } else if remote_source::is_remote(path) {
            open_bytes(path).map(|(_, n)| n).unwrap_or(0)
        } else {
            std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
        },
        parts: Vec::new(),
    };
    let mut have_param = false;
    if has("sce_sys/param.json") {
        if let Some(bytes) = read_text(&mut tree, "sce_sys/param.json") {
            let end = bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes[..end]) {
                apply_param_json(&mut g, &v);
                have_param = true;
            }
        }
    }
    if !have_param && has("sce_sys/param.sfo") {
        if let Some(bytes) = read_text(&mut tree, "sce_sys/param.sfo") {
            if let Ok(params) = sfo_params(&bytes) {
                apply_sfo(&mut g, &params);
                have_param = true;
            }
        }
    }
    if !have_param {
        g.partial = true;
        g.warnings
            .push("not a game folder: no sce_sys/param.json or param.sfo".to_string());
    }
    for name in IMAGE_NAMES {
        let p = format!("sce_sys/{name}");
        if let Some(f) = files.iter().find(|f| f.path == p) {
            g.images.push(ImageInfo {
                name: name.to_string(),
                size: f.size,
            });
        }
    }
    let change = format!("sce_sys/{CHANGEINFO}");
    if let Some(bytes) = read_text(&mut tree, &change) {
        g.change_notes = Some(String::from_utf8_lossy(&bytes).to_string());
    }
    for p in too_large {
        g.warnings.push(format!("{p} is too large to show"));
    }
    g.specs.file_count = Some(files.len() as u64);
    finish(&mut g);
    Ok(g)
}

/// One of [`IMAGE_NAMES`] from an inspected source.
pub fn read_image(path: &Path, format: &str, name: &str) -> Result<Vec<u8>> {
    if !IMAGE_NAMES.contains(&name) {
        bail!("unknown image {name}");
    }
    if format == "pkg" || format == "split-pkg" {
        let (mut r, size) = open_bytes(path)?;
        let entries = pkg_entries_from(&mut r, size)?;
        let Some(e) = entries.iter().find(|e| e.name.as_deref() == Some(name)) else {
            bail!("no {name} in this package");
        };
        return Ok(read_pkg_entry_from(&mut r, e, MAX_IMAGE_BYTES)?);
    }
    let mut tree = ps5upload_fpkg::source::open(path)?;
    let p = format!("sce_sys/{name}");
    match tree.files().iter().find(|f| f.path == p) {
        None => bail!("no {name} in this game"),
        Some(f) if f.size > u64::from(MAX_IMAGE_BYTES) => bail!("{name} is too large to show"),
        Some(_) => Ok(tree.read(&p)?),
    }
}

/// One row of the Files tab: a package entry or a file of a folder or image.
#[derive(Debug, Clone, Serialize)]
pub struct FileRow {
    pub path: String,
    pub size: u64,
    /// A package entry sealed with keys this app never has.
    pub encrypted: bool,
}

/// Rows past this are left out (and the reply says so): a list, not a copy of the game.
const MAX_FILE_ROWS: usize = 200_000;

/// What a source holds: a package's entries, or the tree's files. Sorted by path.
pub fn list_files(path: &Path, format: &str) -> Result<Vec<FileRow>> {
    let mut rows: Vec<FileRow> = if format == "pkg" || format == "split-pkg" {
        let (mut r, size) = open_bytes(path)?;
        pkg_entries_from(&mut r, size)?
            .into_iter()
            .map(|e| FileRow {
                path: e.name.unwrap_or_else(|| format!("#{:04X}", e.id)),
                size: u64::from(e.size),
                encrypted: e.encrypted,
            })
            .collect()
    } else {
        ps5upload_fpkg::source::open(path)?
            .files()
            .iter()
            .map(|f| FileRow {
                path: f.path.clone(),
                size: f.size,
                encrypted: false,
            })
            .collect()
    };
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(rows)
}

/// Inspections by token, so the image requests that follow reuse one parse.
/// Keyed by path plus the size and mtime of every file the inspection depends
/// on: a changed file, split part or folder PARAM is parsed again.
pub struct InspectCache {
    cap: usize,
    inner: std::sync::Mutex<Vec<CacheEntry>>,
}

struct CacheEntry {
    key: (PathBuf, Vec<(u64, std::time::SystemTime)>),
    token: String,
    at: std::time::Instant,
    inspection: GameInspection,
}

/// How long a token stays valid.
const CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(600);

impl InspectCache {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            inner: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The cached token and inspection for this exact file, or a fresh one.
    pub fn inspect(&self, path: &Path) -> Result<(String, GameInspection)> {
        let key = (path.to_path_buf(), fingerprint(path)?);
        {
            let mut entries = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            entries.retain(|e| e.at.elapsed() < CACHE_TTL);
            if let Some(e) = entries.iter_mut().find(|e| e.key == key) {
                // In use: keep its token alive for the image requests to come.
                e.at = std::time::Instant::now();
                return Ok((e.token.clone(), e.inspection.clone()));
            }
        }
        let inspection = inspect_local(path)?;
        let token = new_token()?;
        let mut entries = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        entries.push(CacheEntry {
            key,
            token: token.clone(),
            at: std::time::Instant::now(),
            inspection: inspection.clone(),
        });
        let excess = entries.len().saturating_sub(self.cap);
        entries.drain(..excess);
        Ok((token, inspection))
    }

    /// The path and format an image request's token points at.
    pub fn source_of(&self, token: &str) -> Option<(PathBuf, String)> {
        let entries = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .iter()
            .find(|e| e.token == token && e.at.elapsed() < CACHE_TTL)
            .map(|e| (e.key.0.clone(), e.inspection.source.format.clone()))
    }
}

/// Size and mtime of `path` and what else its inspection reads: a folder's
/// PARAM files (editing them leaves the folder's own mtime alone) and a
/// package's split parts.
fn fingerprint(path: &Path) -> Result<Vec<(u64, std::time::SystemTime)>> {
    // A server or console file is not stat'ed here: its token lives for the TTL.
    if remote_source::is_remote(path) {
        return Ok(Vec::new());
    }
    let stamp = |p: &Path| {
        std::fs::metadata(p)
            .ok()
            .map(|m| (m.len(), m.modified().unwrap_or(std::time::UNIX_EPOCH)))
    };
    let Some(own) = stamp(path) else {
        bail!("cannot read {}", path.display());
    };
    let mut out = vec![own];
    if path.is_dir() {
        for f in ["sce_sys/param.json", "sce_sys/param.sfo"] {
            out.extend(stamp(&path.join(f)));
        }
    } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        let dir = path.parent().unwrap_or(Path::new("."));
        // Same walk as the split-set parser: <name>.0, <name>.1, ...
        for i in 0..=1024u32 {
            match stamp(&dir.join(format!("{name}.{i}"))) {
                Some(s) => out.push(s),
                None => break,
            }
        }
    }
    Ok(out)
}

fn new_token() -> Result<String> {
    let mut b = [0u8; 16];
    // The OS generator; never a Unix-only path (the engine also runs on Windows).
    // A token is what lets a request read files, so never fall back to a
    // guessable one.
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("no secure random source: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

static CACHE: std::sync::LazyLock<InspectCache> =
    std::sync::LazyLock::new(|| InspectCache::new(32));

#[derive(serde::Deserialize)]
pub(crate) struct InspectReq {
    path: String,
}

#[derive(Serialize)]
struct InspectResp {
    token: String,
    inspection: GameInspection,
}

/// POST /api/game/inspect — what a package, image or game folder is.
pub(crate) async fn inspect_handler(
    axum::Json(req): axum::Json<InspectReq>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let path = crate::fpkg_api::resolve_engine_path(&req.path);
    match tokio::task::spawn_blocking(move || CACHE.inspect(&path)).await {
        Ok(Ok((token, inspection))) => {
            axum::Json(InspectResp { token, inspection }).into_response()
        }
        Ok(Err(e)) => {
            crate::json_err(axum::http::StatusCode::BAD_REQUEST, e.to_string()).into_response()
        }
        Err(join) => crate::json_err(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("the inspection task failed: {join}"),
        )
        .into_response(),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct ImageQuery {
    token: String,
    name: String,
}

#[derive(serde::Deserialize)]
pub(crate) struct FilesQuery {
    token: String,
}

#[derive(Serialize)]
struct FilesResp {
    files: Vec<FileRow>,
    /// More rows existed than a list shows.
    truncated: bool,
}

/// GET /api/game/inspect/files?token — the Files tab: a package's entries or the tree's files.
pub(crate) async fn inspect_files_handler(
    axum::extract::Query(q): axum::extract::Query<FilesQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some((path, format)) = CACHE.source_of(&q.token) else {
        return crate::json_err(
            axum::http::StatusCode::NOT_FOUND,
            "unknown or expired token",
        )
        .into_response();
    };
    match tokio::task::spawn_blocking(move || list_files(&path, &format)).await {
        Ok(Ok(mut files)) => {
            let truncated = files.len() > MAX_FILE_ROWS;
            files.truncate(MAX_FILE_ROWS);
            axum::Json(FilesResp { files, truncated }).into_response()
        }
        Ok(Err(e)) => {
            crate::json_err(axum::http::StatusCode::BAD_REQUEST, e.to_string()).into_response()
        }
        Err(join) => crate::json_err(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("the file list task failed: {join}"),
        )
        .into_response(),
    }
}

/// GET /api/game/inspect/image — one image of an inspected source.
pub(crate) async fn inspect_image_handler(
    axum::extract::Query(q): axum::extract::Query<ImageQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some((path, format)) = CACHE.source_of(&q.token) else {
        return crate::json_err(
            axum::http::StatusCode::NOT_FOUND,
            "unknown or expired token",
        )
        .into_response();
    };
    match tokio::task::spawn_blocking(move || read_image(&path, &format, &q.name)).await {
        Ok(Ok(bytes)) => ([(axum::http::header::CONTENT_TYPE, "image/png")], bytes).into_response(),
        Ok(Err(e)) => {
            crate::json_err(axum::http::StatusCode::NOT_FOUND, e.to_string()).into_response()
        }
        Err(join) => crate::json_err(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("the image task failed: {join}"),
        )
        .into_response(),
    }
}

#[cfg(test)]
mod test_fixtures {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    /// A fresh scratch directory per call.
    pub fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ps5upload-inspect-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sfo(pairs: &[(&str, &str)]) -> Vec<u8> {
        let mut keys = Vec::new();
        let mut data = Vec::new();
        let mut index = Vec::new();
        for (k, v) in pairs {
            let val = format!("{v}\0");
            index.extend_from_slice(&(keys.len() as u16).to_le_bytes());
            index.extend_from_slice(&0x0204u16.to_le_bytes());
            index.extend_from_slice(&(val.len() as u32).to_le_bytes());
            index.extend_from_slice(&(val.len() as u32).to_le_bytes());
            index.extend_from_slice(&(data.len() as u32).to_le_bytes());
            keys.extend_from_slice(k.as_bytes());
            keys.push(0);
            data.extend_from_slice(val.as_bytes());
        }
        let key_off = 0x14 + pairs.len() * 0x10;
        let mut out = b"\x00PSF".to_vec();
        out.extend_from_slice(&[1, 1, 0, 0]);
        out.extend_from_slice(&(key_off as u32).to_le_bytes());
        out.extend_from_slice(&((key_off + keys.len()) as u32).to_le_bytes());
        out.extend_from_slice(&(pairs.len() as u32).to_le_bytes());
        out.extend_from_slice(&index);
        out.extend_from_slice(&keys);
        out.extend_from_slice(&data);
        out
    }

    /// A `\x7FCNT` package with one PARAM.SFO entry.
    pub fn write_cnt_pkg(path: impl AsRef<Path>, category: &str) -> PathBuf {
        let s = sfo(&[
            ("CATEGORY", category),
            ("CONTENT_ID", "UP0000-CUSA00001_00-TEST000000000000"),
            ("TITLE", "Fixture"),
        ]);
        // The CNT header is 0xA0 bytes (content id at 0x40); the table follows it.
        let table = 0x100u32;
        let data_off = table + 0x20;
        let mut pkg = vec![0u8; data_off as usize];
        pkg[0..4].copy_from_slice(&ps5upload_pkg::PKG_MAGIC.to_be_bytes());
        pkg[0x10..0x14].copy_from_slice(&1u32.to_be_bytes());
        pkg[0x18..0x1C].copy_from_slice(&table.to_be_bytes());
        let cid = b"UP0000-CUSA00001_00-TEST000000000000";
        pkg[0x40..0x40 + cid.len()].copy_from_slice(cid);
        let e = table as usize;
        pkg[e..e + 4].copy_from_slice(&0x1000u32.to_be_bytes());
        pkg[e + 0x10..e + 0x14].copy_from_slice(&data_off.to_be_bytes());
        pkg[e + 0x14..e + 0x18].copy_from_slice(&(s.len() as u32).to_be_bytes());
        pkg.extend_from_slice(&s);
        pkg.resize(pkg.len().max(0x100), 0);
        std::fs::write(path.as_ref(), &pkg).unwrap();
        path.as_ref().to_path_buf()
    }

    /// A PS5 `\x7FFIH` package whose embedded CNT has no PARAM entry.
    pub fn write_fih_without_param() -> PathBuf {
        let mut b = vec![0u8; 0x2000];
        b[0..4].copy_from_slice(&ps5upload_pkg::PKG_MAGIC_FIH.to_be_bytes());
        b[0x05] = 0x00;
        b[0x06..0x08].copy_from_slice(&3u16.to_le_bytes());
        b[0x58..0x60].copy_from_slice(&0x1000u64.to_le_bytes());
        let c = 0x1000usize;
        b[c..c + 4].copy_from_slice(&ps5upload_pkg::PKG_MAGIC.to_be_bytes());
        let cid = b"EP0000-PPSA00001_00-TEST000000000000";
        b[c + 0x40..c + 0x40 + cid.len()].copy_from_slice(cid);
        b[c + 0x10..c + 0x14].copy_from_slice(&1u32.to_be_bytes());
        b[c + 0x18..c + 0x1C].copy_from_slice(&0x100u32.to_be_bytes());
        let e = c + 0x100;
        b[e..e + 4].copy_from_slice(&0x0400u32.to_be_bytes());
        b[e + 8..e + 12].copy_from_slice(&0x8000_0000u32.to_be_bytes());
        b[e + 0x10..e + 0x14].copy_from_slice(&0x200u32.to_be_bytes());
        b[e + 0x14..e + 0x18].copy_from_slice(&0x10u32.to_be_bytes());
        let p = scratch().join("fih.pkg");
        std::fs::write(&p, &b).unwrap();
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cache_reuses_a_token_until_the_file_changes() {
        // [RF 5]
        let dir = test_fixtures::scratch();
        let p = test_fixtures::write_cnt_pkg(dir.join("a.pkg"), "gd");
        let cache = InspectCache::new(32);
        let (t1, _) = cache.inspect(&p).unwrap();
        let (t2, _) = cache.inspect(&p).unwrap();
        assert_eq!(t1, t2);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&p, std::fs::read(&p).unwrap()).unwrap(); // new mtime
        let (t3, _) = cache.inspect(&p).unwrap();
        assert_ne!(t1, t3);
        assert!(cache.source_of(&t3).is_some());
        assert!(cache.source_of("nope").is_none());
    }

    #[test]
    fn firmware_formats() {
        assert_eq!(fw_from_sfo(0x0505_0000), "5.05");
        assert_eq!(fw_from_sfo(0x1100_0000), "11.00");
        assert_eq!(
            fw_from_hex_word("0x0510000000000000").as_deref(),
            Some("5.10")
        );
        assert_eq!(
            fw_from_hex_word("0x1200000000000000").as_deref(),
            Some("12.00")
        );
        assert_eq!(fw_from_hex_word("nonsense"), None);
    }

    #[test]
    fn sdk_and_date_from_pubtoolinfo() {
        let (sdk, date) =
            sdk_from_pubtool("c_date=20211126,sdk_ver=08008000,st_type=digital50,img0_l0_size=1");
        assert_eq!(sdk.as_deref(), Some("8.00"));
        assert_eq!(date.as_deref(), Some("2021-11-26"));
    }

    #[test]
    fn region_and_type() {
        assert_eq!(
            region_of("UP1082-CUSA03474_00-SLUS202680000001").as_deref(),
            Some("US")
        );
        assert_eq!(region_of("EP9000-PPSA01234_00-X").as_deref(), Some("EU"));
        assert_eq!(region_of("JP0001-X").as_deref(), Some("JP"));
        assert_eq!(region_of("HP0001-X").as_deref(), Some("Asia"));
        assert_eq!(region_of(""), None);
        assert_eq!(content_type_of("gd"), "game");
        assert_eq!(content_type_of("gp"), "update");
        assert_eq!(content_type_of("ac"), "dlc");
        assert_eq!(content_type_of("gde"), "app");
    }

    #[test]
    fn a_ps5_game_folder_is_inspected_from_param_json() {
        let dir = test_fixtures::scratch();
        let sys = dir.join("sce_sys");
        std::fs::create_dir_all(&sys).unwrap();
        std::fs::write(
            sys.join("param.json"),
            br#"{"titleId":"PPSA01234","contentId":"EP9000-PPSA01234_00-GAME000000000000",
          "contentVersion":"01.002.000","masterVersion":"01.00","conceptId":"10001234",
          "requiredSystemSoftwareVersion":"0x0510000000000000","sdkVersion":"0x0450000000000000",
          "applicationDrmType":"standard","ageLevel":{"default":12},
          "localizedParameters":{"defaultLanguage":"en-US","en-US":{"titleName":"Test Game"},"fr-FR":{"titleName":"Jeu"}}}"#,
        )
        .unwrap();
        std::fs::write(sys.join("icon0.png"), b"\x89PNG....").unwrap();
        std::fs::write(dir.join("eboot.bin"), vec![0u8; 100]).unwrap();

        let g = inspect_local(&dir).unwrap();
        assert_eq!(g.source.format, "folder");
        assert_eq!(g.identity.title, "Test Game");
        assert_eq!(
            g.identity.titles.get("fr-FR").map(String::as_str),
            Some("Jeu")
        );
        assert_eq!(g.identity.platform, "ps5");
        assert_eq!(g.identity.region.as_deref(), Some("EU"));
        assert_eq!(g.specs.min_fw.as_deref(), Some("5.10"));
        assert_eq!(g.specs.sdk_ver.as_deref(), Some("4.50"));
        assert_eq!(g.specs.drm.as_deref(), Some("standard"));
        assert_eq!(g.specs.age_rating.as_deref(), Some("12"));
        assert_eq!(
            g.specs.languages,
            vec!["en-US".to_string(), "fr-FR".to_string()]
        );
        assert!(g.images.iter().any(|i| i.name == "icon0.png"));
        assert!(g
            .params
            .iter()
            .any(|p| p.key == "conceptId" && p.value == "10001234"));
        assert!(!g.partial);
    }

    #[test]
    fn a_ps5_dump_is_a_game_with_its_build_date() {
        let dir = test_fixtures::scratch();
        let sys = dir.join("sce_sys");
        std::fs::create_dir_all(&sys).unwrap();
        std::fs::write(
            sys.join("param.json"),
            br#"{"titleId":"PPSA30528","contentId":"UP1004-PPSA30528_00-REDEMPTION000001",
              "applicationCategoryType":0,
              "pubtools":{"creationDate":"2026-01-28 10:47:23","toolVersion":"3.13"}}"#,
        )
        .unwrap();
        let g = inspect_local(&dir).unwrap();
        assert_eq!(g.identity.content_type, "game");
        assert_eq!(g.specs.build_date.as_deref(), Some("2026-01-28"));

        // An unset creation date (the epoch) is not a build date.
        std::fs::write(
            sys.join("param.json"),
            br#"{"titleId":"PPSA03016","applicationCategoryType":0,
              "pubtools":{"creationDate":"1970-01-01 00:00:00"}}"#,
        )
        .unwrap();
        assert_eq!(inspect_local(&dir).unwrap().specs.build_date, None);
    }

    #[test]
    fn a_folder_that_is_not_a_game_says_so() {
        // [RF 3]
        let dir = test_fixtures::scratch();
        std::fs::write(dir.join("readme.txt"), b"hi").unwrap();
        let g = inspect_local(&dir).unwrap();
        assert!(g.partial);
        assert!(g.warnings.iter().any(|w| w.contains("not a game folder")));
    }

    #[test]
    fn a_ps5_package_without_readable_param_is_partial_not_empty() {
        // [RF 1]
        let path = test_fixtures::write_fih_without_param();
        let g = inspect_local(&path).unwrap();
        assert_eq!(g.identity.platform, "ps5");
        assert!(!g.identity.content_id.is_empty());
        assert!(g.partial);
        assert!(g.warnings.iter().any(|w| w.contains("encrypted")));
    }

    #[test]
    fn a_split_set_sums_its_parts() {
        // [RF 2]
        let dir = test_fixtures::scratch();
        let lead = test_fixtures::write_cnt_pkg(dir.join("game.pkg"), "gd");
        std::fs::write(dir.join("game.pkg.0"), vec![0u8; 1000]).unwrap();
        let g = inspect_local(&lead).unwrap();
        assert_eq!(g.source.format, "split-pkg");
        assert_eq!(g.source.parts.len(), 2);
        assert_eq!(
            g.source.size,
            g.source.parts.iter().map(|p| p.size).sum::<u64>()
        );
        assert_eq!(g.identity.title, "Fixture");
        assert_eq!(g.identity.content_type, "game");
    }

    #[test]
    fn the_cache_sees_a_folder_param_edit_and_a_split_part_change() {
        let dir = test_fixtures::scratch();
        let sys = dir.join("g/sce_sys");
        std::fs::create_dir_all(&sys).unwrap();
        std::fs::write(sys.join("param.json"), br#"{"titleId":"PPSA00001"}"#).unwrap();
        let cache = InspectCache::new(32);
        let (t1, g1) = cache.inspect(&dir.join("g")).unwrap();
        assert_eq!(g1.identity.title_id, "PPSA00001");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        // Editing a file inside sce_sys leaves the folder's own mtime alone.
        std::fs::write(sys.join("param.json"), br#"{"titleId":"PPSA00002"}"#).unwrap();
        let (t2, g2) = cache.inspect(&dir.join("g")).unwrap();
        assert_ne!(t1, t2);
        assert_eq!(g2.identity.title_id, "PPSA00002");

        let lead = test_fixtures::write_cnt_pkg(dir.join("s.pkg"), "gd");
        std::fs::write(dir.join("s.pkg.0"), vec![0u8; 10]).unwrap();
        let (t3, g3) = cache.inspect(&lead).unwrap();
        std::fs::write(dir.join("s.pkg.0"), vec![0u8; 20]).unwrap();
        let (t4, g4) = cache.inspect(&lead).unwrap();
        assert_ne!(t3, t4);
        assert_eq!(g4.source.size, g3.source.size + 10);
    }

    #[test]
    fn a_non_ascii_content_id_does_not_panic() {
        let mut g = GameInspection::default();
        g.identity.content_id = "EP9000-PPSA0123é_00-X".to_string();
        finish(&mut g);
        assert!(g.identity.title_id.is_empty() || g.identity.title_id.is_char_boundary(0));
    }

    #[test]
    fn oversized_text_in_a_folder_is_not_read_whole() {
        let dir = test_fixtures::scratch();
        let sys = dir.join("sce_sys");
        std::fs::create_dir_all(sys.join("changeinfo")).unwrap();
        std::fs::write(sys.join("param.json"), br#"{"titleId":"PPSA00001"}"#).unwrap();
        std::fs::write(
            sys.join("changeinfo/changeinfo.xml"),
            vec![b'a'; MAX_TEXT_BYTES as usize + 1],
        )
        .unwrap();
        let g = inspect_local(&dir).unwrap();
        assert!(g.change_notes.is_none());
        assert!(g.warnings.iter().any(|w| w.contains("changeinfo")));
    }

    #[test]
    fn sfo_numbers_read_as_numbers_and_flags_as_hex() {
        let mut g = GameInspection::default();
        apply_sfo(
            &mut g,
            &[
                ("PARENTAL_LEVEL".to_string(), SfoValue::Int(5)),
                ("ATTRIBUTE".to_string(), SfoValue::Int(0x10)),
                ("SYSTEM_VER".to_string(), SfoValue::Int(0x0505_0000)),
            ],
        );
        let v = |k: &str| g.params.iter().find(|p| p.key == k).unwrap().value.clone();
        assert_eq!(v("PARENTAL_LEVEL"), "5");
        assert_eq!(v("ATTRIBUTE"), "0x00000010");
        assert_eq!(v("SYSTEM_VER"), "0x05050000");
    }

    #[test]
    fn an_ffpfsc_is_inspected_through_its_image() {
        use ps5upload_fpkg::ffpfsc::{wrap, Control, WrapOptions};
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../ps5upload-fpkg/tests/fixtures/mini.exfat");
        let dir = test_fixtures::scratch();
        let packed = dir.join("mini.ffpfsc");
        wrap(
            &fixture,
            &packed,
            &WrapOptions::default(),
            &mut Control::default(),
        )
        .unwrap();
        let plain = inspect_local(&fixture).unwrap();
        let g = inspect_local(&packed).unwrap();
        assert_eq!(g.source.format, "ffpfsc");
        assert!(!g.identity.title_id.is_empty());
        assert_eq!(g.identity.title_id, plain.identity.title_id);
        assert_eq!(
            read_image(&packed, "ffpfsc", "icon0.png").unwrap(),
            read_image(&fixture, "exfat", "icon0.png").unwrap()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_package_and_a_folder_on_a_server_are_inspected_in_place() {
        let dir = test_fixtures::scratch();
        let pkg = std::fs::read(test_fixtures::write_cnt_pkg(dir.join("a.pkg"), "gd")).unwrap();
        let r = crate::remote::pool::testing::install_global(&[
            ("/viewer/a.pkg", &pkg),
            (
                "/viewer/game/sce_sys/param.json",
                br#"{"titleId":"PPSA04321","contentId":"UP9000-PPSA04321_00-GAME000000000000"}"#,
            ),
            ("/viewer/game/eboot.bin", &[1u8; 64]),
        ]);
        let id = r
            .store
            .add(
                crate::remote::store::conn("NAS", crate::remote::store::Protocol::Smb),
                crate::remote::store::Secret::None,
            )
            .unwrap()
            .conn
            .id;
        crate::convert_source::register();
        let (p1, p2) = (
            format!("remote://{id}/viewer/a.pkg"),
            format!("remote://{id}/viewer/game"),
        );
        let (g1, g2, files) = tokio::task::spawn_blocking(move || {
            let g1 = inspect_local(Path::new(&p1)).unwrap();
            let files = list_files(Path::new(&p1), "pkg").unwrap();
            let img = g1.images.first().map(|i| i.name.clone());
            if let Some(name) = img {
                assert!(!read_image(Path::new(&p1), "pkg", &name).unwrap().is_empty());
            }
            (g1, inspect_local(Path::new(&p2)).unwrap(), files)
        })
        .await
        .unwrap();
        assert_eq!(g1.source.format, "pkg");
        assert_eq!(g1.source.location, "server");
        assert_eq!(g1.identity.title, "Fixture");
        assert_eq!(g1.source.size, pkg.len() as u64);
        assert!(!files.is_empty());
        assert_eq!(g2.source.format, "folder");
        assert_eq!(g2.source.location, "server");
        assert_eq!(g2.identity.title_id, "PPSA04321");
    }

    #[test]
    fn a_folder_lists_its_files_and_a_package_its_entries() {
        let dir = test_fixtures::scratch();
        std::fs::create_dir_all(dir.join("g/sce_sys")).unwrap();
        std::fs::write(dir.join("g/sce_sys/param.json"), b"{}").unwrap();
        std::fs::write(dir.join("g/eboot.bin"), [0u8; 10]).unwrap();
        let files = list_files(&dir.join("g"), "folder").unwrap();
        assert_eq!(
            files
                .iter()
                .map(|f| (f.path.as_str(), f.size))
                .collect::<Vec<_>>(),
            [("eboot.bin", 10), ("sce_sys/param.json", 2)]
        );
        let p = test_fixtures::write_cnt_pkg(dir.join("a.pkg"), "gd");
        let entries = list_files(&p, "pkg").unwrap();
        assert!(entries.iter().any(|e| e.path == "param.sfo"));
    }
}
