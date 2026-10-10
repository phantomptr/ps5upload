//! What gets packed and how: drakmor's TOML profile (`[pack]`, `[groups.*]`, `[[rule]]`,
//! `[runtime]`), the same keys, defaults and checks as `ampr_pack.py`'s `load_config`, and the
//! built-in profile used when the user gives none.

use std::collections::BTreeMap;

use super::format::{RuntimeSettings, CHUNK_ALIGNMENT, DATA_HEADER_SIZE, MAX_IO_PAGE, MIN_IO_PAGE};
use super::glob;
use crate::{format_err, Error, Result};

/// Reads an `include_from` / `exclude_from` list by its name in the profile.
pub type ListReader<'a> = dyn FnMut(&str) -> Result<String> + 'a;

pub const DEFAULT_INDEX_NAME: &str = "ampr_assets.index";
pub const DEFAULT_PACK_PATTERN: &str = "ampr_assets-{id:03d}.pak";
/// The packer version the build id commits to; drakmor's tool is "4.0" for this format.
const TOOL_VERSION: &str = "4.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Compress,
    Store,
    Loose,
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Action::Compress => "compress",
            Action::Store => "store",
            Action::Loose => "loose",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Auto,
    Random,
    Mixed,
    Streaming,
}

impl Layout {
    fn name(self) -> &'static str {
        match self {
            Layout::Auto => "auto",
            Layout::Random => "random",
            Layout::Mixed => "mixed",
            Layout::Streaming => "streaming",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assignment {
    Balanced,
    Hash,
    RoundRobin,
}

impl Assignment {
    fn name(self) -> &'static str {
        match self {
            Assignment::Balanced => "balanced",
            Assignment::Hash => "hash",
            Assignment::RoundRobin => "round_robin",
        }
    }
}

/// LZ4 encoder choice, as `compression_mode` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionMode {
    Fast,
    Hc,
}

impl CompressionMode {
    fn name(self) -> &'static str {
        match self {
            CompressionMode::Fast => "fast",
            CompressionMode::Hc => "hc",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub name: String,
    pub pack_count: u32,
    pub assignment: Assignment,
    /// 0 is unlimited.
    pub max_pack_size: u64,
    pub stripe_large_files: bool,
    pub stripe_threshold: u64,
    pub stripe_group_blocks: u64,
    /// 0 takes `[pack].io_page_size`.
    pub io_page_size: u64,
}

impl Group {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            pack_count: 1,
            assignment: Assignment::Balanced,
            max_pack_size: 0,
            stripe_large_files: false,
            stripe_threshold: 256 << 20,
            stripe_group_blocks: 8,
            io_page_size: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub action: Action,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub block_shift: u8,
    pub group: String,
    pub mode: CompressionMode,
    pub level: u8,
    pub acceleration: u32,
    pub min_savings_bytes: u64,
    pub min_savings_ratio: f64,
    pub io_neutral_min_savings_bytes: u64,
    pub io_neutral_min_savings_ratio: f64,
    pub layout: Layout,
    pub streaming: bool,
    pub hot: bool,
    pub force_pack: bool,
}

impl Rule {
    pub fn matches(&self, relative: &str) -> bool {
        glob::matches_any(relative, &self.include) && !glob::matches_any(relative, &self.exclude)
    }

    /// `auto` resolved: streaming, then hot (random), then mixed.
    pub fn resolved_layout(&self) -> Layout {
        match self.layout {
            Layout::Auto if self.streaming => Layout::Streaming,
            Layout::Auto if self.hot => Layout::Random,
            Layout::Auto => Layout::Mixed,
            other => other,
        }
    }

    /// A forced-loose rule that keeps `block_shift`, as the packer substitutes one.
    pub fn loose(block_shift: u8) -> Self {
        Self {
            action: Action::Loose,
            include: vec!["**".into()],
            exclude: Vec::new(),
            block_shift,
            group: "default".into(),
            mode: CompressionMode::Hc,
            level: 12,
            acceleration: 1,
            min_savings_bytes: 64,
            min_savings_ratio: 0.01,
            io_neutral_min_savings_bytes: 8192,
            io_neutral_min_savings_ratio: 0.125,
            layout: Layout::Auto,
            streaming: false,
            hot: false,
            force_pack: false,
        }
    }

    pub fn lz4_mode(&self) -> super::lz4::Mode {
        match self.mode {
            CompressionMode::Fast => super::lz4::Mode::Fast,
            CompressionMode::Hc => super::lz4::Mode::High(self.level),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub runtime: Option<RuntimeSettings>,
    pub index_name: String,
    pub pack_pattern: String,
    pub default_action: Action,
    pub default_block_shift: u8,
    pub io_page_size: u64,
    pub payload_alignment: u64,
    pub chunk_alignment: u64,
    pub workers: usize,
    pub compression_mode: CompressionMode,
    pub compression_level: u8,
    pub acceleration: u32,
    pub min_savings_bytes: u64,
    pub min_savings_ratio: f64,
    pub io_neutral_min_savings_bytes: u64,
    pub io_neutral_min_savings_ratio: f64,
    pub deduplicate: bool,
    pub deduplicate_group_scope: bool,
    pub deduplicate_streaming: bool,
    pub validate_index_metadata: bool,
    pub self_contained: bool,
    pub required_packed: Vec<String>,
    pub auto_loose_large_files: bool,
    pub auto_loose_hot_files: bool,
    pub auto_loose_min_file_size: u64,
    pub auto_loose_sample_blocks: u64,
    pub auto_loose_sample_bytes: u64,
    pub auto_loose_min_savings_ratio: f64,
    pub auto_loose_max_raw_ratio: f64,
    /// By name; `default` always exists.
    pub groups: BTreeMap<String, Group>,
    pub rules: Vec<Rule>,
}

impl Default for Config {
    fn default() -> Self {
        let mut groups = BTreeMap::new();
        groups.insert("default".to_string(), Group::new("default"));
        Self {
            runtime: None,
            index_name: DEFAULT_INDEX_NAME.into(),
            pack_pattern: DEFAULT_PACK_PATTERN.into(),
            default_action: Action::Loose,
            default_block_shift: 16,
            io_page_size: 64 << 10,
            payload_alignment: 64 << 10,
            chunk_alignment: 64,
            workers: std::thread::available_parallelism().map_or(1, |n| n.get().min(8)),
            compression_mode: CompressionMode::Hc,
            compression_level: 12,
            acceleration: 1,
            min_savings_bytes: 64,
            min_savings_ratio: 0.01,
            io_neutral_min_savings_bytes: 8 << 10,
            io_neutral_min_savings_ratio: 0.125,
            deduplicate: true,
            deduplicate_group_scope: false,
            deduplicate_streaming: false,
            validate_index_metadata: true,
            self_contained: false,
            required_packed: Vec::new(),
            auto_loose_large_files: true,
            auto_loose_hot_files: false,
            auto_loose_min_file_size: 64 << 20,
            auto_loose_sample_blocks: 32,
            auto_loose_sample_bytes: 16 << 20,
            auto_loose_min_savings_ratio: 0.05,
            auto_loose_max_raw_ratio: 0.90,
            groups,
            rules: Vec::new(),
        }
    }
}

/// `"64KiB"`, `"8GiB"`, `"1.5MB"` or a plain integer, as drakmor's `parse_size` reads them.
pub fn parse_size(value: &toml::Value) -> Result<u64> {
    match value {
        toml::Value::Integer(i) if *i >= 0 => Ok(*i as u64),
        toml::Value::Integer(_) => format_err("a size must not be negative"),
        toml::Value::String(s) => parse_size_str(s),
        other => format_err(format!("not a size: {other}")),
    }
}

pub fn parse_size_str(s: &str) -> Result<u64> {
    let text: String = s.trim().chars().filter(|&c| c != '_').collect();
    let split = text
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphabetic())
        .last()
        .map_or(text.len(), |(i, _)| i);
    let (number, suffix) = text.split_at(split);
    let unit: u64 = match suffix.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1000,
        "kib" => 1 << 10,
        "m" | "mb" => 1_000_000,
        "mib" => 1 << 20,
        "g" | "gb" => 1_000_000_000,
        "gib" => 1 << 30,
        "t" | "tb" => 1_000_000_000_000,
        "tib" => 1 << 40,
        other => return format_err(format!("unknown size suffix {other:?} in {s:?}")),
    };
    let n: f64 = number
        .trim()
        .parse()
        .map_err(|_| Error::Format(format!("invalid size: {s:?}")))?;
    if !n.is_finite() || n < 0.0 {
        return format_err(format!("invalid size: {s:?}"));
    }
    Ok((n * unit as f64) as u64)
}

/// The block shift for a power-of-two size from 16 KiB to 1 MiB.
pub fn block_shift(size: u64) -> Result<u8> {
    if !size.is_power_of_two() || !(1 << 14..=1 << 20).contains(&size) {
        return format_err(format!(
            "a block size must be a power of two from 16 KiB to 1 MiB, not {size}"
        ));
    }
    Ok(size.trailing_zeros() as u8)
}

fn get<'a>(t: &'a toml::Table, key: &str) -> Option<&'a toml::Value> {
    t.get(key)
}

fn as_bool(t: &toml::Table, key: &str, default: bool) -> Result<bool> {
    match get(t, key) {
        None => Ok(default),
        Some(toml::Value::Boolean(b)) => Ok(*b),
        Some(v) => format_err(format!("{key} must be true or false, not {v}")),
    }
}

fn as_int(t: &toml::Table, key: &str, default: i64) -> Result<i64> {
    match get(t, key) {
        None => Ok(default),
        Some(toml::Value::Integer(i)) => Ok(*i),
        Some(v) => format_err(format!("{key} must be an integer, not {v}")),
    }
}

fn as_float(t: &toml::Table, key: &str, default: f64) -> Result<f64> {
    match get(t, key) {
        None => Ok(default),
        Some(toml::Value::Float(f)) => Ok(*f),
        Some(toml::Value::Integer(i)) => Ok(*i as f64),
        Some(v) => format_err(format!("{key} must be a number, not {v}")),
    }
}

fn as_str(t: &toml::Table, key: &str, default: &str) -> Result<String> {
    match get(t, key) {
        None => Ok(default.to_string()),
        Some(toml::Value::String(s)) => Ok(s.clone()),
        Some(v) => format_err(format!("{key} must be a string, not {v}")),
    }
}

fn as_size(t: &toml::Table, key: &str, default: u64) -> Result<u64> {
    get(t, key).map_or(Ok(default), |v| {
        parse_size(v).map_err(|e| Error::Format(format!("{key}: {e}")))
    })
}

fn as_strings(t: &toml::Table, key: &str) -> Result<Option<Vec<String>>> {
    match get(t, key) {
        None => Ok(None),
        Some(toml::Value::String(s)) => Ok(Some(vec![s.clone()])),
        Some(toml::Value::Array(a)) => a
            .iter()
            .map(|v| match v {
                toml::Value::String(s) => Ok(s.clone()),
                _ => format_err(format!("{key} must be a string or a list of strings")),
            })
            .collect::<Result<Vec<_>>>()
            .map(Some),
        Some(_) => format_err(format!("{key} must be a string or a list of strings")),
    }
}

fn action(s: &str) -> Result<Action> {
    match s.to_ascii_lowercase().as_str() {
        "compress" => Ok(Action::Compress),
        "store" => Ok(Action::Store),
        "loose" => Ok(Action::Loose),
        other => format_err(format!(
            "action must be compress, store or loose, not {other:?}"
        )),
    }
}

fn layout(s: &str) -> Result<Layout> {
    match s.to_ascii_lowercase().as_str() {
        "auto" => Ok(Layout::Auto),
        "random" => Ok(Layout::Random),
        "mixed" => Ok(Layout::Mixed),
        "streaming" => Ok(Layout::Streaming),
        other => format_err(format!(
            "layout must be auto, random, mixed or streaming, not {other:?}"
        )),
    }
}

fn mode(s: &str) -> Result<CompressionMode> {
    match s.to_ascii_lowercase().as_str() {
        "fast" => Ok(CompressionMode::Fast),
        "hc" => Ok(CompressionMode::Hc),
        other => format_err(format!(
            "compression_mode must be fast or hc, not {other:?}"
        )),
    }
}

fn ratio(name: &str, v: f64, inclusive_one: bool) -> Result<f64> {
    let ok = v >= 0.0 && if inclusive_one { v <= 1.0 } else { v < 1.0 };
    if ok {
        Ok(v)
    } else {
        format_err(format!(
            "{name} must be in [0, 1{}",
            if inclusive_one { "]" } else { ")" }
        ))
    }
}

/// The glob that matches every name `pack_pattern` can render, so an earlier build's packs are
/// never packed again. Only `group`, `lane`, `volume` and `id` fields are allowed.
pub fn pack_output_glob(pattern: &str) -> Result<String> {
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(open) = rest.find(['{', '}']) {
        out.push_str(&rest[..open]);
        let tail = &rest[open..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push_str(&tail[..1]);
            rest = &tail[2..];
            continue;
        }
        if tail.starts_with('}') {
            return format_err(format!("invalid pack_pattern: {pattern:?}"));
        }
        let close = tail
            .find('}')
            .ok_or_else(|| Error::Format(format!("invalid pack_pattern: {pattern:?}")))?;
        let field = tail[1..close].split([':', '!']).next().unwrap_or("");
        if !matches!(field, "group" | "lane" | "volume" | "id") {
            return format_err(format!(
                "pack_pattern uses unsupported field {field:?}; allowed are group, lane, \
                 volume and id"
            ));
        }
        out.push('*');
        rest = &tail[close + 1..];
    }
    out.push_str(rest);
    let out = out.replace('\\', "/").trim_start_matches('/').to_string();
    if out.is_empty() {
        return format_err("pack_pattern cannot be empty");
    }
    if !super::format::safe_relative(&render_pattern(pattern, "g", 0, 0, 0)?.replace('\\', "/")) {
        return format_err(format!("unsafe pack_pattern: {pattern:?}"));
    }
    Ok(out)
}

/// `pack_pattern` with its fields filled, Python `str.format` style: `{id:03d}`, `{lane:02d}`,
/// `{group}`; the integer specs support zero/space padding to a width.
pub fn render_pattern(
    pattern: &str,
    group: &str,
    lane: u64,
    volume: u64,
    id: u64,
) -> Result<String> {
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(open) = rest.find(['{', '}']) {
        out.push_str(&rest[..open]);
        let tail = &rest[open..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push_str(&tail[..1]);
            rest = &tail[2..];
            continue;
        }
        let close = tail
            .find('}')
            .filter(|_| tail.starts_with('{'))
            .ok_or_else(|| Error::Format(format!("invalid pack_pattern: {pattern:?}")))?;
        let inner = &tail[1..close];
        let (field, spec) = inner.split_once(':').unwrap_or((inner, ""));
        let value = match field {
            "group" => {
                if !spec.is_empty() && spec != "s" {
                    return format_err(format!("unsupported format for group: {spec:?}"));
                }
                group.to_string()
            }
            "lane" | "volume" | "id" => {
                let n = match field {
                    "lane" => lane,
                    "volume" => volume,
                    _ => id,
                };
                let spec = spec.strip_suffix('d').unwrap_or(spec);
                let (zero, width) = match spec.strip_prefix('0') {
                    Some(w) => (true, w),
                    None => (false, spec),
                };
                let width: usize = if width.is_empty() {
                    0
                } else {
                    width.parse().map_err(|_| {
                        Error::Format(format!("unsupported format spec {spec:?} in pack_pattern"))
                    })?
                };
                if zero {
                    format!("{n:0width$}")
                } else {
                    format!("{n:>width$}")
                }
            }
            other => return format_err(format!("pack_pattern uses unsupported field {other:?}")),
        };
        out.push_str(&value);
        rest = &tail[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

impl Config {
    /// A profile from TOML text. `include_from` / `exclude_from` lists resolve through
    /// `read_list` (a name relative to the TOML's folder); without one they are refused.
    pub fn from_toml(text: &str, read_list: Option<&mut ListReader>) -> Result<Self> {
        let raw: toml::Table = text.parse().map_err(|e: toml::de::Error| {
            Error::Format(format!("the profile is not valid TOML: {e}"))
        })?;
        let empty = toml::Table::new();
        let pack = match raw.get("pack") {
            None => &empty,
            Some(toml::Value::Table(t)) => t,
            Some(_) => return format_err("[pack] must be a table"),
        };
        let mut c = Config::default();

        if let Some(rt) = raw.get("runtime") {
            let toml::Value::Table(rt) = rt else {
                return format_err("[runtime] must be a table");
            };
            let keys = [
                "decoded_cache_bytes",
                "physical_cache_bytes",
                "workers",
                "latency_reserve_workers",
            ];
            if rt.len() != keys.len() || !keys.iter().all(|k| rt.contains_key(*k)) {
                return format_err(
                    "[runtime] needs exactly decoded_cache_bytes, physical_cache_bytes, workers \
                     and latency_reserve_workers",
                );
            }
            let int = |k: &str| -> Result<u32> {
                match rt.get(k) {
                    Some(toml::Value::Integer(i)) if (0..=u32::MAX as i64).contains(i) => {
                        Ok(*i as u32)
                    }
                    _ => format_err(format!("[runtime] {k} must be an integer")),
                }
            };
            let s = RuntimeSettings {
                decoded_cache_bytes: parse_size(&rt["decoded_cache_bytes"])?,
                physical_cache_bytes: parse_size(&rt["physical_cache_bytes"])?,
                workers: int("workers")?,
                latency_reserve_workers: int("latency_reserve_workers")?,
            };
            s.validate()?;
            c.runtime = Some(s);
        }

        c.default_block_shift = block_shift(as_size(pack, "default_block_size", 64 << 10)?)?;
        c.io_page_size = as_size(pack, "io_page_size", 64 << 10)?;
        c.index_name = as_str(pack, "index_name", DEFAULT_INDEX_NAME)?;
        c.pack_pattern = as_str(pack, "pack_pattern", DEFAULT_PACK_PATTERN)?;
        c.default_action = action(&as_str(pack, "default_action", "loose")?)?;
        c.payload_alignment = as_size(pack, "payload_alignment", c.io_page_size)?;
        c.chunk_alignment = as_size(pack, "chunk_alignment", 64)?;
        let workers = as_int(pack, "workers", c.workers as i64)?;
        if !(1..=256).contains(&workers) {
            return format_err("workers must be between 1 and 256");
        }
        c.workers = workers as usize;
        c.compression_mode = mode(&as_str(pack, "compression_mode", "hc")?)?;
        let level = as_int(pack, "compression_level", 12)?;
        if !(1..=12).contains(&level) {
            return format_err("compression_level must be between 1 and 12");
        }
        c.compression_level = level as u8;
        let acceleration = as_int(pack, "acceleration", 1)?;
        if acceleration < 1 {
            return format_err("acceleration must be at least 1");
        }
        c.acceleration = acceleration as u32;
        c.min_savings_bytes = as_size(pack, "min_savings_bytes", 64)?;
        c.min_savings_ratio = ratio(
            "min_savings_ratio",
            as_float(pack, "min_savings_ratio", 0.01)?,
            false,
        )?;
        c.io_neutral_min_savings_bytes = as_size(pack, "io_neutral_min_savings_bytes", 8 << 10)?;
        c.io_neutral_min_savings_ratio = ratio(
            "io_neutral_min_savings_ratio",
            as_float(pack, "io_neutral_min_savings_ratio", 0.125)?,
            false,
        )?;
        c.deduplicate = as_bool(pack, "deduplicate", true)?;
        c.deduplicate_group_scope = match as_str(pack, "deduplicate_scope", "lane")?
            .to_ascii_lowercase()
            .as_str()
        {
            "lane" => false,
            "group" => true,
            _ => return format_err("deduplicate_scope must be lane or group"),
        };
        c.deduplicate_streaming = as_bool(pack, "deduplicate_streaming", false)?;
        // preserve_mtime only chooses between the index's mtime and the disk's, which are the
        // same here: the index is generated from the files packed.
        as_bool(pack, "preserve_mtime", true)?;
        c.validate_index_metadata = as_bool(pack, "validate_index_metadata", true)?;
        c.self_contained = as_bool(pack, "self_contained", false)?;
        c.required_packed = as_strings(pack, "required_packed")?.unwrap_or_default();
        c.auto_loose_large_files = as_bool(pack, "auto_loose_large_files", true)?;
        c.auto_loose_hot_files = as_bool(pack, "auto_loose_hot_files", false)?;
        c.auto_loose_min_file_size = as_size(pack, "auto_loose_min_file_size", 64 << 20)?;
        let samples = as_int(pack, "auto_loose_sample_blocks", 32)?;
        if !(1..=4096).contains(&samples) {
            return format_err("auto_loose_sample_blocks must be between 1 and 4096");
        }
        c.auto_loose_sample_blocks = samples as u64;
        c.auto_loose_sample_bytes = as_size(pack, "auto_loose_sample_bytes", 16 << 20)?;
        if c.auto_loose_sample_bytes < 1 {
            return format_err("auto_loose_sample_bytes must be at least 1 byte");
        }
        c.auto_loose_min_savings_ratio = ratio(
            "auto_loose_min_savings_ratio",
            as_float(pack, "auto_loose_min_savings_ratio", 0.05)?,
            false,
        )?;
        c.auto_loose_max_raw_ratio = ratio(
            "auto_loose_max_raw_ratio",
            as_float(pack, "auto_loose_max_raw_ratio", 0.90)?,
            true,
        )?;
        c.check_geometry()?;

        c.groups.clear();
        match raw.get("groups") {
            None => {}
            Some(toml::Value::Table(groups)) => {
                for (name, value) in groups {
                    let toml::Value::Table(g) = value else {
                        return format_err(format!("[groups.{name}] must be a table"));
                    };
                    let pack_count = as_int(g, "pack_count", 1)?;
                    if !(1..=0xFFFF).contains(&pack_count) {
                        return format_err(format!(
                            "groups.{name}.pack_count must be between 1 and 65535"
                        ));
                    }
                    let stripe_group_blocks = as_int(g, "stripe_group_blocks", 8)?;
                    if stripe_group_blocks < 1 {
                        return format_err(format!(
                            "groups.{name}.stripe_group_blocks must be at least 1"
                        ));
                    }
                    let group = Group {
                        name: name.clone(),
                        pack_count: pack_count as u32,
                        assignment: match as_str(g, "assignment", "balanced")?
                            .to_ascii_lowercase()
                            .as_str()
                        {
                            "balanced" => Assignment::Balanced,
                            "hash" => Assignment::Hash,
                            "round_robin" => Assignment::RoundRobin,
                            other => {
                                return format_err(format!(
                                    "assignment must be balanced, hash or round_robin, not \
                                     {other:?}"
                                ))
                            }
                        },
                        max_pack_size: as_size(g, "max_pack_size", 0)?,
                        stripe_large_files: as_bool(g, "stripe_large_files", false)?,
                        stripe_threshold: as_size(g, "stripe_threshold", 256 << 20)?,
                        stripe_group_blocks: stripe_group_blocks as u64,
                        io_page_size: as_size(g, "io_page_size", 0)?,
                    };
                    c.check_group(&group)?;
                    c.groups.insert(name.clone(), group);
                }
            }
            Some(_) => return format_err("[groups] must be a table"),
        }
        c.groups
            .entry("default".to_string())
            .or_insert_with(|| Group::new("default"));
        if c.groups.len() > 256 {
            return format_err("a profile can have at most 256 groups");
        }

        let mut read_list = read_list;
        let rules = match raw.get("rule") {
            None => Vec::new(),
            Some(toml::Value::Array(a)) => a.clone(),
            Some(_) => return format_err("[[rule]] entries must form an array"),
        };
        for (index, value) in rules.iter().enumerate() {
            let toml::Value::Table(r) = value else {
                return format_err(format!("rule {index} must be a table"));
            };
            let group = as_str(r, "group", "default")?;
            if !c.groups.contains_key(&group) {
                return format_err(format!("rule {index} names an unknown group {group:?}"));
            }
            let shift = match r.get("block_size") {
                None => c.default_block_shift,
                Some(v) => block_shift(parse_size(v)?)?,
            };
            let mut lists = |key: &str| -> Result<Vec<String>> {
                let names = as_strings(r, key)?.unwrap_or_default();
                let mut out = Vec::new();
                for name in names {
                    let reader = read_list.as_deref_mut().ok_or_else(|| {
                        Error::Format(format!(
                            "rule {index} uses {key}, which needs the list file beside the profile"
                        ))
                    })?;
                    if name.starts_with('/') || name.split(['/', '\\']).any(|c| c == "..") {
                        return format_err(format!(
                            "{key} entry escapes the profile folder: {name}"
                        ));
                    }
                    out.extend(
                        reader(&name)?
                            .lines()
                            .map(str::trim)
                            .filter(|l| !l.is_empty() && !l.starts_with('#'))
                            .map(String::from),
                    );
                }
                Ok(out)
            };
            let include_from = lists("include_from")?;
            let exclude_from = lists("exclude_from")?;
            let mut include = as_strings(r, "include")?.unwrap_or_else(|| {
                if include_from.is_empty() {
                    vec!["**".to_string()]
                } else {
                    Vec::new()
                }
            });
            include.extend(include_from);
            let mut exclude = as_strings(r, "exclude")?.unwrap_or_default();
            exclude.extend(exclude_from);
            if include.is_empty() {
                return format_err(format!("rule {index}'s include list is empty"));
            }
            let level = as_int(r, "compression_level", i64::from(c.compression_level))?;
            if !(1..=12).contains(&level) {
                return format_err(format!("rule {index} compression_level must be 1 to 12"));
            }
            let acceleration = as_int(r, "acceleration", i64::from(c.acceleration))?;
            if acceleration < 1 {
                return format_err(format!("rule {index} acceleration must be at least 1"));
            }
            c.rules.push(Rule {
                action: action(&as_str(r, "action", "compress")?)?,
                include,
                exclude,
                block_shift: shift,
                group,
                mode: mode(&as_str(r, "compression_mode", c.compression_mode.name())?)?,
                level: level as u8,
                acceleration: acceleration as u32,
                min_savings_bytes: as_size(r, "min_savings_bytes", c.min_savings_bytes)?,
                min_savings_ratio: ratio(
                    "min_savings_ratio",
                    as_float(r, "min_savings_ratio", c.min_savings_ratio)?,
                    false,
                )?,
                io_neutral_min_savings_bytes: as_size(
                    r,
                    "io_neutral_min_savings_bytes",
                    c.io_neutral_min_savings_bytes,
                )?,
                io_neutral_min_savings_ratio: ratio(
                    "io_neutral_min_savings_ratio",
                    as_float(
                        r,
                        "io_neutral_min_savings_ratio",
                        c.io_neutral_min_savings_ratio,
                    )?,
                    false,
                )?,
                layout: layout(&as_str(r, "layout", "auto")?)?,
                streaming: as_bool(r, "streaming", false)?,
                hot: as_bool(r, "hot", false)?,
                force_pack: as_bool(r, "force_pack", false)?,
            });
        }
        Ok(c)
    }

    fn check_geometry(&self) -> Result<()> {
        if !self.io_page_size.is_power_of_two()
            || !(MIN_IO_PAGE..=MAX_IO_PAGE).contains(&self.io_page_size)
        {
            return format_err("io_page_size must be a power of two between 4 KiB and 1 MiB");
        }
        if !self.payload_alignment.is_power_of_two()
            || self.payload_alignment < DATA_HEADER_SIZE as u64
            || self.payload_alignment > 1 << 20
        {
            return format_err("payload_alignment must be a power of two between 64 B and 1 MiB");
        }
        if !self.chunk_alignment.is_power_of_two()
            || self.chunk_alignment < CHUNK_ALIGNMENT
            || self.chunk_alignment > self.io_page_size
        {
            return format_err(
                "chunk_alignment must be a power of two between 64 B and io_page_size",
            );
        }
        if !super::format::safe_relative(&self.index_name.replace('\\', "/")) {
            return format_err(format!("unsafe index_name: {:?}", self.index_name));
        }
        pack_output_glob(&self.pack_pattern)?;
        Ok(())
    }

    fn check_group(&self, g: &Group) -> Result<()> {
        let page = if g.io_page_size == 0 {
            self.io_page_size
        } else {
            g.io_page_size
        };
        if !page.is_power_of_two()
            || !(MIN_IO_PAGE..=MAX_IO_PAGE).contains(&page)
            || page % self.chunk_alignment != 0
        {
            return format_err(format!(
                "groups.{}.io_page_size must be a power of two between 4 KiB and 1 MiB",
                g.name
            ));
        }
        let minimum =
            super::format::align_up(DATA_HEADER_SIZE as u64, self.payload_alignment.max(page))
                + page;
        if g.max_pack_size != 0 && g.max_pack_size < minimum {
            return format_err(format!(
                "groups.{}.max_pack_size must be at least {minimum} bytes",
                g.name
            ));
        }
        Ok(())
    }

    pub fn group_page(&self, group: &str) -> u64 {
        match self.groups.get(group) {
            Some(g) if g.io_page_size != 0 => g.io_page_size,
            _ => self.io_page_size,
        }
    }

    /// The rule a path gets when no `[[rule]]` matches.
    pub fn default_rule(&self) -> Rule {
        Rule {
            action: self.default_action,
            include: vec!["**".into()],
            exclude: Vec::new(),
            block_shift: self.default_block_shift,
            group: "default".into(),
            mode: self.compression_mode,
            level: self.compression_level,
            acceleration: self.acceleration,
            min_savings_bytes: self.min_savings_bytes,
            min_savings_ratio: self.min_savings_ratio,
            io_neutral_min_savings_bytes: self.io_neutral_min_savings_bytes,
            io_neutral_min_savings_ratio: self.io_neutral_min_savings_ratio,
            layout: Layout::Auto,
            streaming: false,
            hot: false,
            force_pack: false,
        }
    }

    /// The last matching rule wins.
    pub fn select(&self, relative: &str) -> Rule {
        self.rules
            .iter()
            .rev()
            .find(|r| r.matches(relative))
            .cloned()
            .unwrap_or_else(|| self.default_rule())
    }

    /// The configuration the build id commits to: drakmor's `_canonical_config_bytes`, a
    /// sorted-key, compact, ASCII-escaped JSON document, so the same profile over the same files
    /// gets the same id from either tool.
    pub fn canonical_json(&self) -> Vec<u8> {
        use Json::*;
        let groups = self
            .groups
            .values()
            .map(|g| {
                Obj(vec![
                    ("assignment", Str(g.assignment.name().into())),
                    ("io_page_size", Int(g.io_page_size as i128)),
                    ("max_pack_size", Int(g.max_pack_size as i128)),
                    ("name", Str(g.name.clone())),
                    ("pack_count", Int(g.pack_count.into())),
                    ("stripe_group_blocks", Int(g.stripe_group_blocks.into())),
                    ("stripe_large_files", Bool(g.stripe_large_files)),
                    ("stripe_threshold", Int(g.stripe_threshold.into())),
                ])
            })
            .collect();
        let rules = self
            .rules
            .iter()
            .map(|r| {
                Obj(vec![
                    ("acceleration", Int(r.acceleration.into())),
                    ("action", Str(r.action.name().into())),
                    ("block_shift", Int(r.block_shift.into())),
                    ("exclude", Arr(r.exclude.iter().cloned().map(Str).collect())),
                    ("force_pack", Bool(r.force_pack)),
                    ("group", Str(r.group.clone())),
                    ("hot", Bool(r.hot)),
                    ("include", Arr(r.include.iter().cloned().map(Str).collect())),
                    (
                        "io_neutral_min_savings_bytes",
                        Int(r.io_neutral_min_savings_bytes.into()),
                    ),
                    (
                        "io_neutral_min_savings_ratio",
                        Float(r.io_neutral_min_savings_ratio),
                    ),
                    ("layout", Str(r.layout.name().into())),
                    ("level", Int(r.level.into())),
                    ("min_savings_bytes", Int(r.min_savings_bytes.into())),
                    ("min_savings_ratio", Float(r.min_savings_ratio)),
                    ("mode", Str(r.mode.name().into())),
                    ("streaming", Bool(r.streaming)),
                ])
            })
            .collect();
        let doc = Obj(vec![
            ("acceleration", Int(self.acceleration.into())),
            ("auto_loose_hot_files", Bool(self.auto_loose_hot_files)),
            ("auto_loose_large_files", Bool(self.auto_loose_large_files)),
            (
                "auto_loose_max_raw_ratio",
                Float(self.auto_loose_max_raw_ratio),
            ),
            (
                "auto_loose_min_file_size",
                Int(self.auto_loose_min_file_size.into()),
            ),
            (
                "auto_loose_min_savings_ratio",
                Float(self.auto_loose_min_savings_ratio),
            ),
            (
                "auto_loose_sample_blocks",
                Int(self.auto_loose_sample_blocks.into()),
            ),
            (
                "auto_loose_sample_bytes",
                Int(self.auto_loose_sample_bytes.into()),
            ),
            ("chunk_alignment", Int(self.chunk_alignment.into())),
            ("compression_level", Int(self.compression_level.into())),
            ("compression_mode", Str(self.compression_mode.name().into())),
            ("deduplicate", Bool(self.deduplicate)),
            (
                "deduplicate_scope",
                Str(if self.deduplicate_group_scope {
                    "group"
                } else {
                    "lane"
                }
                .into()),
            ),
            ("deduplicate_streaming", Bool(self.deduplicate_streaming)),
            ("default_action", Str(self.default_action.name().into())),
            ("default_block_shift", Int(self.default_block_shift.into())),
            ("groups", Arr(groups)),
            ("index_name", Str(self.index_name.clone())),
            (
                "io_neutral_min_savings_bytes",
                Int(self.io_neutral_min_savings_bytes.into()),
            ),
            (
                "io_neutral_min_savings_ratio",
                Float(self.io_neutral_min_savings_ratio),
            ),
            ("io_page_size", Int(self.io_page_size.into())),
            ("min_savings_bytes", Int(self.min_savings_bytes.into())),
            ("min_savings_ratio", Float(self.min_savings_ratio)),
            ("pack_pattern", Str(self.pack_pattern.clone())),
            ("payload_alignment", Int(self.payload_alignment.into())),
            ("rules", Arr(rules)),
            ("tool_version", Str(TOOL_VERSION.into())),
        ]);
        let mut out = String::new();
        doc.write(&mut out);
        out.into_bytes()
    }
}

/// Just enough JSON to reproduce Python's `json.dumps(sort_keys=True, separators=(",", ":"))`:
/// keys are given in sorted order, floats print as `repr` does, non-ASCII is `\u` escaped.
enum Json {
    Obj(Vec<(&'static str, Json)>),
    Arr(Vec<Json>),
    Str(String),
    Int(i128),
    Float(f64),
    Bool(bool),
}

impl Json {
    fn write(&self, out: &mut String) {
        match self {
            Json::Obj(fields) => {
                debug_assert!(fields.windows(2).all(|w| w[0].0 < w[1].0), "keys sorted");
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_str(out, k);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
            Json::Arr(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Json::Str(s) => write_str(out, s),
            Json::Int(i) => out.push_str(&i.to_string()),
            Json::Float(f) => out.push_str(&python_float(*f)),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        }
    }
}

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7E => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A float as Python's `repr` prints it: shortest round-trip digits, `.0` on integral values,
/// exponent form below 1e-4 and from 1e16.
pub(crate) fn python_float(f: f64) -> String {
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    let a = f.abs();
    if !(1e-4..1e16).contains(&a) {
        let s = format!("{f:e}");
        let (mantissa, exp) = s.split_once('e').unwrap();
        let exp: i32 = exp.parse().unwrap();
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{mantissa}e{sign}{:02}", exp.abs());
    }
    let s = format!("{f}");
    if s.contains('.') {
        s
    } else {
        format!("{s}.0")
    }
}

/// The profile used when the user gives none.
///
/// Built from drakmor's guidance (tools/ampr_pack.example.toml and docs/ASSET_PACKS.md):
/// default loose, everything inside a folder compressed in independent 64 KiB blocks with
/// the page-safe `mixed` layout, and a final `loose` rule — the last match wins — for what must
/// stay a real file or gains nothing. That list was informed by the safety exclusions in
/// Nazky's Lazy_AMPR, a front end for the same packer: executables and modules (loaded with
/// mmap, which the runtime does not serve), the system and backport folders, AMPR's own
/// files, text configuration a title may read outside AMPR, already-compressed media, and the
/// streaming containers Insomniac titles open at boot.
pub fn default_profile(level: u8, block_shift: u8) -> Config {
    let mut c = Config {
        compression_mode: if level <= 2 {
            CompressionMode::Fast
        } else {
            CompressionMode::Hc
        },
        compression_level: level.clamp(1, 12),
        default_block_shift: block_shift,
        ..Config::default()
    };
    let base = c.default_rule();
    c.rules.push(Rule {
        action: Action::Compress,
        include: vec!["*/**".into()],
        layout: Layout::Mixed,
        ..base.clone()
    });
    let loose: Vec<String> = DEFAULT_LOOSE.iter().map(|s| s.to_string()).collect();
    c.rules.push(Rule {
        action: Action::Loose,
        include: loose,
        ..base
    });
    c
}

/// Paths the built-in profile leaves loose. `*` crosses folders (see [`glob`]), so `*.sprx`
/// is any `.sprx` anywhere.
const DEFAULT_LOOSE: &[&str] = &[
    // Modules and executables: loaded through mmap.
    "eboot.bin",
    "*/eboot.bin",
    "*.elf",
    "*.self",
    "*.prx",
    "*.sprx",
    // System, backport and save folders.
    "sce_sys/*",
    "sce_module/*",
    "*/sce_module/*",
    "fakelib/*",
    "fakelib2/*",
    "trophy2/*",
    "uds/*",
    "system/*",
    "save/*",
    "mods/*",
    // AMPR's own files and logs.
    "ampr_emu.index",
    "ampr_assets.index*",
    "ampr_assets-*.pak",
    "ampr_commands.bin",
    "apr_emu.log",
    // Text configuration, which a title may open with ordinary file calls.
    "*.json",
    "*.ini",
    "*.cfg",
    "*.xml",
    "*.txt",
    // Already compressed: LZ4 gains nothing and the reads would go through the pack runtime.
    "*.mp4",
    "*.bk2",
    "*.usm",
    "*.ivf",
    "*.webm",
    "*.mkv",
    "*.avi",
    "*.mov",
    "*.m4v",
    "*.wem",
    "*.bnk",
    "*.at9",
    "*.ogg",
    "*.opus",
    "*.mp3",
    "*.flac",
    "*.aac",
    "*.m4a",
    "*.png",
    "*.jpg",
    "*.jpeg",
    "*.webp",
    "*.zip",
    "*.7z",
    "*.rar",
    "*.gz",
    "*.xz",
    "*.bz2",
    "*.zst",
    "*.lz4",
    "*.pfs",
    "*.img",
    // Streaming containers (Insomniac's movie and sound banks) a title opens at boot.
    "*/movie",
    "*/movie_*",
    "*/movies",
    "*/movies_*",
    "*/soundbank",
    "*/soundbank_*",
    "*/screenreaderwem",
    "*/screenreaderwem.*",
    "*/wem",
    "*/wem_*",
    "*/wem.*",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse_as_drakmor_reads_them() {
        assert_eq!(parse_size_str("64KiB").unwrap(), 65536);
        assert_eq!(parse_size_str("8GiB").unwrap(), 8 << 30);
        assert_eq!(parse_size_str("1.5 MB").unwrap(), 1_500_000);
        assert_eq!(parse_size_str("1_024").unwrap(), 1024);
        assert_eq!(parse_size_str("64B").unwrap(), 64);
        assert!(parse_size_str("12 parsecs").is_err());
        assert!(block_shift(48 << 10).is_err());
        assert!(block_shift(8 << 10).is_err());
        assert_eq!(block_shift(1 << 20).unwrap(), 20);
    }

    #[test]
    fn floats_print_as_python_repr() {
        for (f, want) in [
            (0.01, "0.01"),
            (0.125, "0.125"),
            (0.9, "0.9"),
            (0.0, "0.0"),
            (1.0, "1.0"),
            (0.0025, "0.0025"),
            (1e-5, "1e-05"),
            (1.5e16, "1.5e+16"),
            (0.1 + 0.2, "0.30000000000000004"),
        ] {
            assert_eq!(python_float(f), want);
        }
    }

    #[test]
    fn pack_patterns_render_like_str_format() {
        assert_eq!(
            render_pattern(DEFAULT_PACK_PATTERN, "default", 0, 0, 7).unwrap(),
            "ampr_assets-007.pak"
        );
        assert_eq!(
            render_pattern(
                "ampr_assets-{group}-lane{lane:02d}-vol{volume:02d}-{id:03d}.pak",
                "assets",
                3,
                1,
                12
            )
            .unwrap(),
            "ampr_assets-assets-lane03-vol01-012.pak"
        );
        assert_eq!(
            pack_output_glob(DEFAULT_PACK_PATTERN).unwrap(),
            "ampr_assets-*.pak"
        );
        assert!(pack_output_glob("x-{name}.pak").is_err());
        assert!(pack_output_glob("../x-{id}.pak").is_err());
    }

    /// drakmor's example profile loads, its rules in order, the last match winning.
    #[test]
    fn a_profile_loads_and_the_last_rule_wins() {
        let text = r#"
            [pack]
            default_action = "loose"
            default_block_size = "64KiB"
            compression_mode = "hc"
            compression_level = 9
            deduplicate_scope = "group"

            [runtime]
            decoded_cache_bytes = "128MiB"
            physical_cache_bytes = "32MiB"
            workers = 4
            latency_reserve_workers = 1

            [groups.assets]
            pack_count = 4
            max_pack_size = "8GiB"

            [[rule]]
            action = "compress"
            include = ["assets/**", "data/**"]
            exclude = ["**/*.sprx"]
            group = "assets"
            layout = "mixed"

            [[rule]]
            action = "store"
            include = "movies/**"
            block_size = "256KiB"
            layout = "streaming"

            [[rule]]
            action = "loose"
            include = ["assets/debug/**"]
        "#;
        let c = Config::from_toml(text, None).unwrap();
        assert_eq!(c.compression_level, 9);
        assert!(c.deduplicate_group_scope);
        assert_eq!(c.runtime.unwrap().workers, 4);
        assert_eq!(c.groups["assets"].pack_count, 4);
        assert!(c.groups.contains_key("default"));
        assert_eq!(c.select("assets/a.bin").action, Action::Compress);
        assert_eq!(c.select("assets/a.bin").group, "assets");
        assert_eq!(c.select("assets/x/m.sprx").action, Action::Loose);
        assert_eq!(c.select("assets/debug/x").action, Action::Loose);
        assert_eq!(c.select("movies/a.bk2").block_shift, 18);
        assert_eq!(
            c.select("movies/a.bk2").resolved_layout(),
            Layout::Streaming
        );
        assert_eq!(c.select("eboot.bin").action, Action::Loose);

        assert!(Config::from_toml("[pack]\ncompression_level = 13", None).is_err());
        assert!(Config::from_toml("[[rule]]\ngroup = \"nope\"", None).is_err());
        assert!(Config::from_toml("[runtime]\nworkers = 4", None).is_err());
        assert!(Config::from_toml("[[rule]]\ninclude_from = [\"x.txt\"]", None).is_err());
        let mut lists = |name: &str| -> Result<String> {
            assert_eq!(name, "x.txt");
            Ok("# comment\n\nd/a.bin\n d/b.bin \n".into())
        };
        let c =
            Config::from_toml("[[rule]]\ninclude_from = [\"x.txt\"]", Some(&mut lists)).unwrap();
        assert_eq!(c.rules[0].include, ["d/a.bin", "d/b.bin"]);
    }

    #[test]
    fn the_default_profile_packs_folders_and_keeps_the_unsafe_loose() {
        let c = default_profile(9, 16);
        assert_eq!(c.select("d/actor/x").action, Action::Compress);
        assert_eq!(c.select("d/actor/x").resolved_layout(), Layout::Mixed);
        for loose in [
            "eboot.bin",
            "toc",
            "fakelib/libSceAmpr.sprx",
            "sce_sys/param.json",
            "sce_module/libc.prx",
            "data/settings.json",
            "d/soundbank",
            "d/soundbank_fr",
            "d/movie_intro",
            "media/intro.mp4",
            "ampr_assets-000.pak",
        ] {
            assert_eq!(c.select(loose).action, Action::Loose, "{loose}");
        }
        assert_eq!(c.select("d/soundbank.fr").action, Action::Compress);
    }

    /// The canonical document drakmor's packer hashes for the build id, for the default config
    /// with one rule (checked against `_canonical_config_bytes`).
    #[test]
    fn the_canonical_config_matches_drakmors() {
        let c = Config::from_toml(
            "[pack]\nworkers = 2\n[[rule]]\naction = \"store\"\ninclude = [\"d/**\", \"é\"]\n",
            None,
        )
        .unwrap();
        let got = String::from_utf8(c.canonical_json()).unwrap();
        let want = concat!(
            r#"{"acceleration":1,"auto_loose_hot_files":false,"auto_loose_large_files":true,"#,
            r#""auto_loose_max_raw_ratio":0.9,"auto_loose_min_file_size":67108864,"#,
            r#""auto_loose_min_savings_ratio":0.05,"auto_loose_sample_blocks":32,"#,
            r#""auto_loose_sample_bytes":16777216,"chunk_alignment":64,"compression_level":12,"#,
            r#""compression_mode":"hc","deduplicate":true,"deduplicate_scope":"lane","#,
            r#""deduplicate_streaming":false,"default_action":"loose","default_block_shift":16,"#,
            r#""groups":[{"assignment":"balanced","io_page_size":0,"max_pack_size":0,"#,
            r#""name":"default","pack_count":1,"stripe_group_blocks":8,"#,
            r#""stripe_large_files":false,"stripe_threshold":268435456}],"#,
            r#""index_name":"ampr_assets.index","io_neutral_min_savings_bytes":8192,"#,
            r#""io_neutral_min_savings_ratio":0.125,"io_page_size":65536,"#,
            r#""min_savings_bytes":64,"min_savings_ratio":0.01,"#,
            r#""pack_pattern":"ampr_assets-{id:03d}.pak","payload_alignment":65536,"#,
            r#""rules":[{"acceleration":1,"action":"store","block_shift":16,"exclude":[],"#,
            r#""force_pack":false,"group":"default","hot":false,"include":["d/**",""#,
            "\\",
            r#"u00e9"],"#,
            r#""io_neutral_min_savings_bytes":8192,"io_neutral_min_savings_ratio":0.125,"#,
            r#""layout":"auto","level":12,"min_savings_bytes":64,"min_savings_ratio":0.01,"#,
            r#""mode":"hc","streaming":false}],"tool_version":"4.0"}"#
        );
        assert_eq!(got, want);
    }
}
