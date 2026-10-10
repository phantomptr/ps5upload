//! The packer: AMPRIDX3 entries in, data volumes plus manifest, CRC sidecar and runtime
//! settings out. A port of the planning and placement rules in drakmor's `build_packs`.

use std::collections::HashMap;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

use super::config::{pack_output_glob, render_pattern, Action, Assignment, Config, Layout, Rule};
use super::format::*;
use super::glob;
use super::lz4;
use crate::ampr_index::Entry;
use crate::source::SourceTree;
use crate::{format_err, Error, Result};

/// What a build did, for logs.
#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub files_total: u64,
    pub files_packed: u64,
    pub files_loose: u64,
    pub files_auto_loose: u64,
    pub chunks: u64,
    pub chunks_lz4: u64,
    pub chunks_raw: u64,
    pub chunks_shared: u64,
    /// Decoded bytes of every packed file.
    pub logical_bytes: u64,
    /// Bytes the chunks occupy in the volumes, padding aside.
    pub stored_bytes: u64,
    pub padding_bytes: u64,
}

/// A finished pack set. The volumes are on disk in the output folder; the small files are here.
#[derive(Debug)]
pub struct Output {
    pub build_id: [u8; 16],
    pub index_name: String,
    pub manifest: Vec<u8>,
    /// `<index_name>.crc`: decoded-chunk CRCs, for offline verification only.
    pub crc: Vec<u8>,
    /// `<index_name>.runtime`, when the profile has `[runtime]`.
    pub runtime: Option<Vec<u8>>,
    /// `(name inside /app0, file on disk)` in pack-id order.
    pub volumes: Vec<(String, PathBuf)>,
    /// Paths (relative to `/app0`) whose bytes are in the packs: the image leaves them out.
    pub packed: Vec<String>,
    /// Paths that stay ordinary files.
    pub loose: Vec<String>,
    pub stats: Stats,
    pub warnings: Vec<String>,
}

impl Output {
    /// The manifest and its sidecars as `(name, bytes)`.
    pub fn small_files(&self) -> Vec<(String, &[u8])> {
        let mut out = vec![
            (self.index_name.clone(), self.manifest.as_slice()),
            (format!("{}.crc", self.index_name), self.crc.as_slice()),
        ];
        if let Some(rt) = &self.runtime {
            out.push((format!("{}.runtime", self.index_name), rt.as_slice()));
        }
        out
    }

    /// Write the manifest and its sidecars beside the volumes.
    pub fn write_small_files(&self, dir: &Path) -> Result<()> {
        for (name, bytes) in self.small_files() {
            let path = dir.join(&name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, bytes)?;
        }
        Ok(())
    }
}

/// Byte progress and cancellation.
#[derive(Default)]
pub struct Control<'a> {
    pub progress: Option<&'a mut dyn FnMut(u64, u64)>,
    pub cancel: Option<&'a AtomicBool>,
}

struct Selected {
    relative: String,
    rule: Rule,
    size: u64,
    lane: u32,
}

/// A block compressed (or not) by a worker.
struct Block {
    stored: Option<Vec<u8>>,
    codec: u8,
    raw_crc: u32,
    raw_len: u64,
    fingerprint: [u8; 28],
}

fn compress_block(raw: &[u8], rule: &Rule, io_page: u64) -> Block {
    let raw_crc = crc32(raw);
    let mut fingerprint = [0u8; 28];
    fingerprint[..4].copy_from_slice(&raw_crc.to_le_bytes());
    fingerprint[4..12].copy_from_slice(&(raw.len() as u64).to_le_bytes());
    fingerprint[12..].copy_from_slice(&Sha256::digest(raw)[..16]);
    let raw_block = Block {
        stored: None,
        codec: CODEC_RAW,
        raw_crc,
        raw_len: raw.len() as u64,
        fingerprint,
    };
    if rule.action == Action::Store {
        return raw_block;
    }
    let compressed = lz4::compress(raw, rule.lz4_mode());
    let saved = raw.len() as i64 - compressed.len() as i64;
    let ratio = if raw.is_empty() {
        0.0
    } else {
        saved as f64 / raw.len() as f64
    };
    let (mut need_bytes, mut need_ratio) = (rule.min_savings_bytes, rule.min_savings_ratio);
    // A cold random read costs whole I/O pages; demand more when LZ4 saves none of them.
    if rule.resolved_layout() != Layout::Streaming {
        let raw_pages = (raw.len() as u64).div_ceil(io_page);
        let stored_pages = (compressed.len() as u64).div_ceil(io_page);
        if stored_pages >= raw_pages {
            need_bytes = need_bytes.max(rule.io_neutral_min_savings_bytes);
            need_ratio = need_ratio.max(rule.io_neutral_min_savings_ratio);
        }
    }
    if saved < need_bytes as i64 || ratio < need_ratio {
        return raw_block;
    }
    Block {
        stored: Some(compressed),
        codec: CODEC_LZ4,
        ..raw_block
    }
}

/// One data volume being written.
struct Volume {
    pack_id: usize,
    name: String,
    path: PathBuf,
    out: BufWriter<std::fs::File>,
    payload_offset: u64,
    position: u64,
    chunk_alignment: u64,
    io_page: u64,
    max_size: u64,
    flags: u32,
    padding: u64,
}

impl Volume {
    /// Where a chunk of `size` goes and its placement flags. Small random/mixed chunks share a
    /// page but never straddle one; larger ones start page-aligned; streaming stays dense.
    fn placement(&self, size: u64, layout: Layout, extent_start: bool) -> (u64, u8) {
        let mut position = self.position;
        if extent_start {
            position = align_up(position, self.io_page);
        }
        let offset = if layout == Layout::Streaming {
            align_up(position, self.chunk_alignment)
        } else if size >= self.io_page {
            align_up(position, self.io_page)
        } else {
            let at = align_up(position, self.chunk_alignment);
            if at + size > align_up(at + 1, self.io_page) {
                align_up(at, self.io_page)
            } else {
                at
            }
        };
        let mut flags = 0;
        if size <= self.io_page && offset / self.io_page == (offset + size - 1) / self.io_page {
            flags |= CHUNK_PAGE_CONTAINED;
        }
        if offset % self.io_page == 0 {
            flags |= CHUNK_PAGE_ALIGNED;
        }
        (offset, flags)
    }

    fn fits(&self, size: u64, layout: Layout, extent_start: bool) -> bool {
        let (offset, _) = self.placement(size, layout, extent_start);
        self.max_size == 0 || align_up(offset + size, self.io_page) <= self.max_size
    }

    fn write(&mut self, bytes: &[u8], layout: Layout, extent_start: bool) -> Result<(u64, u8)> {
        let (offset, flags) = self.placement(bytes.len() as u64, layout, extent_start);
        self.pad_to(offset)?;
        self.out.write_all(bytes)?;
        self.position = offset + bytes.len() as u64;
        Ok((offset, flags))
    }

    fn pad_to(&mut self, offset: u64) -> Result<()> {
        let gap = offset - self.position;
        if gap > 0 {
            std::io::copy(&mut std::io::repeat(0).take(gap), &mut self.out)?;
            self.padding += gap;
            self.position = offset;
        }
        Ok(())
    }

    fn final_size(&self) -> u64 {
        align_up(self.position, self.io_page)
    }

    fn finish(&mut self, build_id: &[u8; 16]) -> Result<PackRecord> {
        let size = self.final_size();
        self.pad_to(size)?;
        let payload = self.position - self.payload_offset;
        self.out.flush()?;
        let file = self.out.get_mut();
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&data_header(
            self.pack_id as u32,
            build_id,
            self.payload_offset,
            payload,
            self.flags,
        ))?;
        file.sync_all()?;
        if file.metadata()?.len() != size {
            return format_err(format!("{} has an unexpected size", self.name));
        }
        Ok(PackRecord {
            payload_bytes: payload,
            file_size: size,
            name_offset: 0,
            name_length: 0,
            flags: self.flags,
            io_page_size: self.io_page as u32,
        })
    }
}

use std::io::Read as _;

struct Volumes<'a> {
    config: &'a Config,
    dir: &'a Path,
    all: Vec<Volume>,
    current: HashMap<(String, u32), usize>,
    numbers: HashMap<(String, u32), u64>,
    names: std::collections::HashSet<String>,
}

impl Volumes<'_> {
    fn open(&mut self, group: &str, lane: u32, flags: u32) -> Result<usize> {
        let pack_id = self.all.len();
        if pack_id > 0xFFFF {
            return format_err("the pack set needs more than 65536 volumes");
        }
        let key = (group.to_string(), lane);
        let number = *self.numbers.get(&key).unwrap_or(&0);
        self.numbers.insert(key.clone(), number + 1);
        let name = render_pattern(
            &self.config.pack_pattern,
            group,
            u64::from(lane),
            number,
            pack_id as u64,
        )?
        .replace('\\', "/");
        if name.len() > 1017 || !safe_relative(&name) || !self.names.insert(name.clone()) {
            return format_err(format!(
                "pack_pattern gave an unusable or repeated name: {name}"
            ));
        }
        let page = self.config.group_page(group);
        let payload_offset = align_up(
            DATA_HEADER_SIZE as u64,
            self.config.payload_alignment.max(page),
        );
        let path = self.dir.join(&name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| Error::Format(format!("{}: {e}", path.display())))?;
        let mut v = Volume {
            pack_id,
            name,
            path,
            out: BufWriter::with_capacity(4 << 20, file),
            payload_offset,
            position: 0,
            chunk_alignment: self.config.chunk_alignment,
            io_page: page,
            max_size: self.config.groups[group].max_pack_size,
            flags: flags | PACK_IO_PAGE_LAYOUT,
            padding: 0,
        };
        v.pad_to(payload_offset)?;
        v.padding = payload_offset - DATA_HEADER_SIZE as u64;
        self.all.push(v);
        self.current.insert(key, pack_id);
        Ok(pack_id)
    }

    fn writer_for(
        &mut self,
        group: &str,
        lane: u32,
        size: u64,
        striped: bool,
        layout: Layout,
        extent_start: bool,
    ) -> Result<usize> {
        let flags = PACK_IO_PAGE_LAYOUT | if striped { PACK_STRIPED } else { 0 };
        let mut id = match self.current.get(&(group.to_string(), lane)) {
            Some(&id) => id,
            None => self.open(group, lane, flags)?,
        };
        if !self.all[id].fits(size, layout, extent_start) {
            id = self.open(group, lane, flags)?;
            if !self.all[id].fits(size, layout, true) {
                return format_err(format!(
                    "one block ({size} bytes) exceeds max_pack_size of group {group}"
                ));
            }
        }
        if striped {
            self.all[id].flags |= PACK_STRIPED;
        }
        Ok(id)
    }
}

/// Evenly spread block indices to sample, always including the first and last.
fn sample_indices(total: u64, samples: u64) -> Vec<u64> {
    if total == 0 || samples == 0 {
        return Vec::new();
    }
    if total <= samples {
        return (0..total).collect();
    }
    if samples == 1 {
        return vec![total / 2];
    }
    let mut v: Vec<u64> = (0..samples)
        .map(|s| s * (total - 1) / (samples - 1))
        .collect();
    v.dedup();
    v
}

/// Should a large file the rule compresses stay loose because LZ4 gains too little on it?
/// Decided from a deterministic sample of its blocks, before lanes are assigned.
fn auto_loose(
    tree: &mut dyn SourceTree,
    item: &Selected,
    config: &Config,
) -> Result<Option<String>> {
    let rule = &item.rule;
    if !config.auto_loose_large_files
        || rule.action != Action::Compress
        || rule.force_pack
        || (rule.hot && !config.auto_loose_hot_files)
        || item.size < config.auto_loose_min_file_size
        || item.size == 0
    {
        return Ok(None);
    }
    let block = 1u64 << rule.block_shift;
    let total = item.size.div_ceil(block);
    let count = config
        .auto_loose_sample_blocks
        .min((config.auto_loose_sample_bytes / block).max(1));
    let page = config.group_page(&rule.group);
    let (mut sampled, mut stored, mut raw_blocks) = (0u64, 0u64, 0u64);
    let indices = sample_indices(total, count);
    for &i in &indices {
        let len = block.min(item.size - i * block) as usize;
        let raw = tree.read_range(&item.relative, i * block, len)?;
        if raw.len() != len {
            return format_err(format!("{} is shorter than its size", item.relative));
        }
        let b = compress_block(&raw, rule, page);
        sampled += len as u64;
        stored += b.stored.as_ref().map_or(len as u64, |s| s.len() as u64);
        raw_blocks += u64::from(b.codec == CODEC_RAW);
    }
    let savings = if sampled == 0 {
        0.0
    } else {
        1.0 - stored as f64 / sampled as f64
    };
    let raw_ratio = if indices.is_empty() {
        0.0
    } else {
        raw_blocks as f64 / indices.len() as f64
    };
    if savings < config.auto_loose_min_savings_ratio || raw_ratio >= config.auto_loose_max_raw_ratio
    {
        return Ok(Some(format!(
            "{} size={} sampled={sampled} saving={:.2}% rawBlocks={:.2}%",
            item.relative,
            item.size,
            savings * 100.0,
            raw_ratio * 100.0
        )));
    }
    Ok(None)
}

fn lanes(selected: &[Selected], config: &Config) -> Vec<u32> {
    let mut out = vec![0u32; selected.len()];
    let mut by_group: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, s) in selected.iter().enumerate() {
        if s.rule.action != Action::Loose {
            by_group.entry(s.rule.group.as_str()).or_default().push(i);
        }
    }
    for (name, mut members) in by_group {
        let g = &config.groups[name];
        let n = g.pack_count;
        match g.assignment {
            Assignment::Balanced => {
                // Largest first onto the lightest lane.
                members.sort_by(|&a, &b| {
                    selected[b]
                        .size
                        .cmp(&selected[a].size)
                        .then_with(|| {
                            selected[a]
                                .relative
                                .to_lowercase()
                                .cmp(&selected[b].relative.to_lowercase())
                        })
                        .then(a.cmp(&b))
                });
                let mut loads = vec![0u64; n as usize];
                for i in members {
                    let lane = (0..n as usize).min_by_key(|&l| (loads[l], l)).unwrap();
                    out[i] = lane as u32;
                    loads[lane] += selected[i].size;
                }
            }
            Assignment::Hash => {
                for i in members {
                    out[i] = (fnv1a64(&ascii_fold(&selected[i].relative)) % u64::from(n)) as u32;
                }
            }
            Assignment::RoundRobin => {
                for i in members {
                    out[i] = (i as u64 % u64::from(n)) as u32;
                }
            }
        }
    }
    out
}

/// One unit of the packing stream, in file-id order.
enum Item {
    /// A packed file begins: its chunks start here.
    Start(usize),
    Data {
        file: usize,
        local: u32,
        raw: Vec<u8>,
    },
}

/// Pack the files `entries` lists (file-id order: record `i` is id `i + 1`) from `tree` into
/// volumes under `out_dir`, by `config`. Loose entries are only recorded. On error every volume
/// written so far is removed.
pub fn build(
    entries: &[Entry],
    tree: &mut dyn SourceTree,
    config: &Config,
    out_dir: &Path,
    control: &mut Control,
) -> Result<Output> {
    let mut volumes = Volumes {
        config,
        dir: out_dir,
        all: Vec::new(),
        current: HashMap::new(),
        numbers: HashMap::new(),
        names: Default::default(),
    };
    let result = build_into(entries, tree, config, &mut volumes, control);
    if result.is_err() {
        for v in &volumes.all {
            std::fs::remove_file(&v.path).ok();
        }
    }
    result
}

fn build_into(
    entries: &[Entry],
    tree: &mut dyn SourceTree,
    config: &Config,
    volumes: &mut Volumes,
    control: &mut Control,
) -> Result<Output> {
    let cancelled = |c: &Control| c.cancel.is_some_and(|f| f.load(Ordering::Relaxed));
    let mut stats = Stats {
        files_total: entries.len() as u64,
        ..Stats::default()
    };
    let mut warnings = Vec::new();
    let sizes: HashMap<String, u64> = tree
        .files()
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    let index_name = config.index_name.replace('\\', "/");
    let index_name = index_name.trim_start_matches('/');
    let outputs = [
        index_name.to_string(),
        format!("{index_name}.crc"),
        format!("{index_name}.runtime"),
    ];
    let pack_glob = pack_output_glob(&config.pack_pattern)?;

    // Plan: a rule per file, the earlier build's outputs and auto-loose files loose.
    let mut selected = Vec::with_capacity(entries.len());
    for e in entries {
        let relative = e.path.trim_start_matches('/').to_string();
        let mut rule = config.select(&relative);
        if outputs.contains(&relative) || glob::matches_any(&relative, &[&pack_glob]) {
            rule = Rule::loose(rule.block_shift);
        }
        let mut item = Selected {
            relative,
            size: e.size,
            rule,
            lane: 0,
        };
        if item.rule.action != Action::Loose {
            let Some(&size) = sizes.get(&item.relative) else {
                return format_err(format!(
                    "{} is in the index but not in the folder",
                    item.relative
                ));
            };
            if config.validate_index_metadata && size != e.size {
                return format_err(format!(
                    "{} is {size} bytes but the index says {}",
                    item.relative, e.size
                ));
            }
            item.size = size;
            if let Some(why) = auto_loose(tree, &item, config)? {
                if config.self_contained {
                    warnings.push(format!(
                        "self-contained: auto-loose suppressed for {why}; incompressible blocks \
                         are stored raw"
                    ));
                } else {
                    stats.files_auto_loose += 1;
                    warnings.push(format!("auto-loose: {why}"));
                    item.rule.action = Action::Loose;
                }
            }
        }
        selected.push(item);
        if cancelled(control) {
            return Err(Error::Cancelled);
        }
    }
    let assigned = lanes(&selected, config);
    for (s, lane) in selected.iter_mut().zip(assigned) {
        s.lane = lane;
    }
    if !config.required_packed.is_empty() {
        let missing: Vec<&str> = selected
            .iter()
            .filter(|s| {
                s.rule.action == Action::Loose
                    && glob::matches_any(&s.relative, &config.required_packed)
            })
            .map(|s| s.relative.as_str())
            .collect();
        if !missing.is_empty() {
            return format_err(format!(
                "required_packed matched files that would stay loose: {}",
                missing.join(", ")
            ));
        }
    }

    // Records in file-id order; packed files fill in their chunk range as they are placed.
    let mut strings = StringTable::default();
    let group_names: Vec<&String> = config.groups.keys().collect();
    let mut files = Vec::with_capacity(entries.len());
    let (mut packed, mut loose) = (Vec::new(), Vec::new());
    for (e, s) in entries.iter().zip(&selected) {
        let path = canonical(&format!("/app0/{}", s.relative))?;
        let (path_offset, path_length) = strings.add(&path)?;
        let mut record = FileRecord {
            path_hash: path_hash(&path),
            logical_size: e.size,
            mtime: e.mtime,
            path_offset,
            path_length,
            ..FileRecord::default()
        };
        if s.rule.action == Action::Loose {
            loose.push(s.relative.clone());
            stats.files_loose += 1;
        } else {
            let mut flags = FILE_PACKED;
            if s.rule.action == Action::Store {
                flags |= FILE_STORE_ONLY;
            }
            match s.rule.resolved_layout() {
                Layout::Streaming => flags |= FILE_STREAMING,
                Layout::Random => flags |= FILE_RANDOM_ACCESS,
                _ => {}
            }
            if s.rule.hot {
                flags |= FILE_HOT;
            }
            record.flags = flags;
            record.logical_size = s.size;
            record.block_shift = s.rule.block_shift;
            record.packing_class = group_names
                .iter()
                .position(|g| **g == s.rule.group)
                .unwrap_or(0) as u8;
            packed.push(s.relative.clone());
            stats.files_packed += 1;
        }
        files.push(record);
    }

    let total: u64 = selected
        .iter()
        .filter(|s| s.rule.action != Action::Loose)
        .map(|s| s.size)
        .sum();
    let mut done = 0u64;
    let mut chunks: Vec<ChunkRecord> = Vec::new();
    let mut crcs: Vec<u32> = Vec::new();
    let mut dedupe: HashMap<(String, bool, u8, [u8; 28]), ChunkRecord> = HashMap::new();
    let workers = config.workers.max(1);
    const BATCH_BYTES: usize = 32 << 20;

    let mut place = |item: Item, block: Option<Block>, files: &mut Vec<FileRecord>| -> Result<()> {
        let (file, local) = match item {
            Item::Start(file) => {
                files[file].first_chunk = u32::try_from(chunks.len())
                    .map_err(|_| Error::Format("more than 2^32 chunks".into()))?;
                return Ok(());
            }
            Item::Data { file, local, .. } => (file, local),
        };
        let block = block.expect("a data item has a block");
        let s = &selected[file];
        let group = &config.groups[&s.rule.group];
        let layout = s.rule.resolved_layout();
        let streaming = layout == Layout::Streaming;
        let striped =
            group.stripe_large_files && group.pack_count > 1 && s.size >= group.stripe_threshold;
        let lane = if striped {
            ((u64::from(s.lane) + u64::from(local) / group.stripe_group_blocks)
                % u64::from(group.pack_count)) as u32
        } else {
            s.lane
        };
        let domain = if config.deduplicate_group_scope {
            s.rule.group.clone()
        } else {
            format!("{}:{lane}", s.rule.group)
        };
        let key = (domain, streaming, block.codec, block.fingerprint);
        let dedupe_ok = config.deduplicate && (!streaming || config.deduplicate_streaming);
        let record = match dedupe.get(&key).filter(|_| dedupe_ok) {
            Some(prior) => {
                stats.chunks_shared += 1;
                ChunkRecord {
                    flags: CHUNK_SHARED
                        | (prior.flags & (CHUNK_PAGE_CONTAINED | CHUNK_PAGE_ALIGNED))
                        | if streaming { CHUNK_STREAMING } else { 0 },
                    ..*prior
                }
            }
            None => {
                let bytes = match &block.stored {
                    Some(b) => b.as_slice(),
                    None => unreachable!("raw blocks keep their bytes in `stored`"),
                };
                let extent_start = streaming
                    && (local == 0
                        || (striped && u64::from(local) % group.stripe_group_blocks == 0));
                let id = volumes.writer_for(
                    &s.rule.group,
                    lane,
                    bytes.len() as u64,
                    striped,
                    layout,
                    extent_start,
                )?;
                let v = &mut volumes.all[id];
                let (offset, placement) = v.write(bytes, layout, extent_start)?;
                let safe = if bytes.len() as u64 <= v.io_page {
                    placement & CHUNK_PAGE_CONTAINED != 0
                } else {
                    placement & CHUNK_PAGE_ALIGNED != 0
                };
                if !streaming && !safe {
                    return format_err("a chunk was not placed page-safely");
                }
                let record = ChunkRecord {
                    offset,
                    stored_size: bytes.len() as u32,
                    pack_id: id as u16,
                    codec: block.codec,
                    flags: placement | if streaming { CHUNK_STREAMING } else { 0 },
                };
                if dedupe_ok {
                    dedupe.insert(key, record);
                }
                stats.stored_bytes += bytes.len() as u64;
                record
            }
        };
        stats.chunks += 1;
        if record.codec == CODEC_LZ4 {
            stats.chunks_lz4 += 1;
        } else {
            stats.chunks_raw += 1;
        }
        stats.logical_bytes += block.raw_len;
        chunks.push(record);
        crcs.push(block.raw_crc);
        files[file].chunk_count += 1;
        Ok(())
    };

    let mut batch: Vec<Item> = Vec::new();
    let mut batch_bytes = 0usize;
    let mut flush = |batch: &mut Vec<Item>,
                     files: &mut Vec<FileRecord>,
                     control: &mut Control|
     -> Result<()> {
        let items = std::mem::take(batch);
        let jobs: Vec<(usize, &[u8])> = items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| match it {
                Item::Data { raw, .. } => Some((i, raw.as_slice())),
                Item::Start(_) => None,
            })
            .collect();
        let mut blocks: Vec<Option<Block>> = (0..items.len()).map(|_| None).collect();
        let per = jobs.len().div_ceil(workers).max(1);
        let (items_ref, selected_ref): (&[Item], &[Selected]) = (&items, &selected);
        std::thread::scope(|scope| {
            let handles: Vec<_> = jobs
                .chunks(per)
                .map(|part| {
                    scope.spawn(move || {
                        part.iter()
                            .map(|&(i, raw)| {
                                let Item::Data { file, .. } = &items_ref[i] else {
                                    unreachable!()
                                };
                                let s = &selected_ref[*file];
                                let mut b =
                                    compress_block(raw, &s.rule, config.group_page(&s.rule.group));
                                if b.stored.is_none() {
                                    b.stored = Some(raw.to_vec());
                                }
                                (i, b)
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            for h in handles {
                for (i, b) in h.join().expect("a compression worker panicked") {
                    blocks[i] = Some(b);
                }
            }
        });
        for (item, block) in items.into_iter().zip(blocks) {
            if let Item::Data { raw, .. } = &item {
                done += raw.len() as u64;
            }
            place(item, block, files)?;
        }
        if let Some(p) = control.progress.as_deref_mut() {
            p(done, total);
        }
        Ok(())
    };

    for (i, s) in selected.iter().enumerate() {
        if s.rule.action == Action::Loose {
            continue;
        }
        batch.push(Item::Start(i));
        let block = 1u64 << s.rule.block_shift;
        let mut at = 0u64;
        let mut local = 0u32;
        while at < s.size {
            let len = block.min(s.size - at) as usize;
            let raw = tree.read_range(&s.relative, at, len)?;
            if raw.len() != len {
                return format_err(format!("{} was truncated while packing", s.relative));
            }
            batch_bytes += len;
            batch.push(Item::Data {
                file: i,
                local,
                raw,
            });
            at += len as u64;
            local += 1;
            if batch_bytes >= BATCH_BYTES {
                if cancelled(control) {
                    return Err(Error::Cancelled);
                }
                flush(&mut batch, &mut files, control)?;
                batch_bytes = 0;
            }
        }
    }
    flush(&mut batch, &mut files, control)?;
    drop(flush);
    drop(place);

    // Names go into the string table before the build id is taken; sizes are final already.
    let mut provisional = Vec::with_capacity(volumes.all.len());
    for v in &volumes.all {
        let (name_offset, name_length) = strings.add(&v.name)?;
        provisional.push(PackRecord {
            payload_bytes: v.final_size() - v.payload_offset,
            file_size: v.final_size(),
            name_offset,
            name_length,
            flags: v.flags,
            io_page_size: v.io_page as u32,
        });
    }
    let crc_payload: Vec<u8> = crcs.iter().flat_map(|c| c.to_le_bytes()).collect();
    let mut file_bytes = Vec::with_capacity(files.len() * FILE_RECORD_SIZE);
    files.iter().for_each(|f| f.encode(&mut file_bytes));
    let mut chunk_bytes = Vec::with_capacity(chunks.len() * CHUNK_RECORD_SIZE);
    for c in &chunks {
        c.encode(&mut chunk_bytes)?;
    }
    let mut pack_bytes = Vec::new();
    provisional.iter().for_each(|p| p.encode(&mut pack_bytes));
    let build_id = build_id(&[
        &config.canonical_json(),
        &file_bytes,
        &chunk_bytes,
        &crc_payload,
        &pack_bytes,
        strings.bytes(),
    ]);
    let mut packs = Vec::with_capacity(volumes.all.len());
    for (v, p) in volumes.all.iter_mut().zip(&provisional) {
        let mut actual = v.finish(&build_id)?;
        actual.name_offset = p.name_offset;
        actual.name_length = p.name_length;
        stats.padding_bytes += v.padding;
        packs.push(actual);
    }
    let manifest = manifest_bytes(&build_id, &files, &chunks, &packs, strings.bytes())?;
    let runtime = config.runtime.map(|r| r.encode(&build_id)).transpose()?;
    Ok(Output {
        build_id,
        index_name: index_name.to_string(),
        manifest,
        crc: crc_sidecar(&build_id, &crcs),
        runtime,
        volumes: volumes
            .all
            .iter()
            .map(|v| (v.name.clone(), v.path.clone()))
            .collect(),
        packed,
        loose,
        stats,
        warnings,
    })
}

/// The first 16 bytes of SHA-256 over a domain tag and each part, length-prefixed.
pub fn build_id(parts: &[&[u8]]) -> [u8; 16] {
    let mut h = Sha256::new();
    h.update(b"AMPRPACK4\0");
    for p in parts {
        h.update((p.len() as u64).to_le_bytes());
        h.update(p);
    }
    h.finalize()[..16].try_into().unwrap()
}
